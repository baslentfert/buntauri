//! buntauri window layer.
//!
//! Runs tao + wry on a dedicated UI thread. The host thread (later: Bun's JS
//! thread) talks to it through [`UiThread`] (commands in) and an event
//! callback (events out). Nothing here blocks the host thread.
//!
//! Inside Bun the event callback will push onto the concurrent task queue and
//! wake the event loop; in the standalone demo it is just an mpsc channel.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::{Window, WindowBuilder};
use wry::http::{header::CONTENT_TYPE, Request, Response};
use wry::{WebView, WebViewBuilder};

pub type WindowId = u32;

/// Scheme used for embedded assets: `app://localhost/<path>`.
/// On Windows wry maps this to `http://app.localhost/<path>`.
pub const ASSET_SCHEME: &str = "app";
pub const ASSET_ROOT: &str = "app://localhost/";

/// JS injected into every page before any page script runs.
const BRIDGE_JS: &str = include_str!("bridge.js");

#[derive(Debug, Clone)]
pub struct WindowOptions {
    pub title: String,
    pub width: f64,
    pub height: f64,
    /// URL to load. Relative paths are resolved against [`ASSET_ROOT`].
    pub url: Option<String>,
    /// Inline HTML, used when `url` is `None`.
    pub html: Option<String>,
    pub devtools: bool,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            title: "buntauri".into(),
            width: 800.0,
            height: 600.0,
            url: None,
            html: None,
            devtools: cfg!(debug_assertions),
        }
    }
}

/// Events sent from the UI thread to the host.
#[derive(Debug)]
pub enum HostEvent {
    /// A window and its webview were created.
    Created { window: WindowId },
    /// The page called `window.__BUNTAURI__.invoke(cmd, args)`.
    /// Answer with [`UiThread::resolve`] / [`UiThread::reject`].
    Invoke { window: WindowId, call: u64, cmd: String, args: String },
    /// The user asked to close the window. It is already destroyed.
    Closed { window: WindowId },
    Error { window: Option<WindowId>, message: String },
    /// The UI thread's event loop has stopped.
    Exited,
}

/// A file served under [`ASSET_ROOT`].
pub struct Asset {
    pub bytes: Cow<'static, [u8]>,
    pub mime: Cow<'static, str>,
}

/// Source of embedded assets. Inside Bun this reads straight from the
/// standalone module graph (`'static` section bytes, no copy).
pub trait AssetProvider: Send + Sync + 'static {
    fn get(&self, path: &str) -> Option<Asset>;
}

enum Command {
    Create(WindowId, WindowOptions),
    Eval(WindowId, String),
    SetTitle(WindowId, String),
    Close(WindowId),
    Shutdown,
}

/// Handle to the UI thread. Cheap to use from any thread.
pub struct UiThread {
    proxy: EventLoopProxy<Command>,
    next_id: AtomicU32,
    join: Option<JoinHandle<()>>,
}

impl UiThread {
    pub fn spawn<F>(on_event: F, assets: Arc<dyn AssetProvider>) -> std::io::Result<Self>
    where
        F: Fn(HostEvent) + Send + Sync + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let join = std::thread::Builder::new()
            .name("buntauri-ui".into())
            .spawn(move || ui_main(tx, Arc::new(on_event), assets))?;
        let proxy = rx
            .recv()
            .map_err(|_| std::io::Error::other("UI thread failed to start"))?;
        Ok(Self { proxy, next_id: AtomicU32::new(1), join: Some(join) })
    }

    pub fn create_window(&self, opts: WindowOptions) -> WindowId {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.send(Command::Create(id, opts));
        id
    }

    pub fn eval(&self, window: WindowId, js: impl Into<String>) {
        self.send(Command::Eval(window, js.into()));
    }

    pub fn set_title(&self, window: WindowId, title: impl Into<String>) {
        self.send(Command::SetTitle(window, title.into()));
    }

    pub fn close(&self, window: WindowId) {
        self.send(Command::Close(window));
    }

    /// Fulfil an `invoke` call. `json` must be valid JSON.
    pub fn resolve(&self, window: WindowId, call: u64, json: &str) {
        self.eval(window, format!("window.__BUNTAURI__.__settle({call},true,{json})"));
    }

    /// Reject an `invoke` call with an error message.
    pub fn reject(&self, window: WindowId, call: u64, message: &str) {
        let msg = serde_json::to_string(message).unwrap();
        self.eval(window, format!("window.__BUNTAURI__.__settle({call},false,new Error({msg}))"));
    }

    /// Fire an event in the page (`window.__BUNTAURI__.listen(name, fn)`). `json` must be valid JSON.
    pub fn emit(&self, window: WindowId, name: &str, json: &str) {
        let name = serde_json::to_string(name).unwrap();
        self.eval(window, format!("window.__BUNTAURI__.__emit({name},{json})"));
    }

    /// Stop the event loop and wait for the UI thread to finish.
    pub fn shutdown(mut self) {
        self.send(Command::Shutdown);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }

    fn send(&self, cmd: Command) {
        // Only fails once the loop has exited; nothing left to do then.
        let _ = self.proxy.send_event(cmd);
    }
}

type Emit = Arc<dyn Fn(HostEvent) + Send + Sync>;

struct Entry {
    // Field order matters: the webview must drop before its window.
    webview: WebView,
    window: Window,
}

fn ui_main(ready: mpsc::Sender<EventLoopProxy<Command>>, emit: Emit, assets: Arc<dyn AssetProvider>) {
    let mut builder = EventLoopBuilder::<Command>::with_user_event();
    #[cfg(target_os = "windows")]
    {
        use tao::platform::windows::EventLoopBuilderExtWindows;
        builder.with_any_thread(true);
    }
    #[cfg(any(target_os = "linux", target_os = "dragonfly", target_os = "freebsd", target_os = "netbsd", target_os = "openbsd"))]
    {
        use tao::platform::unix::EventLoopBuilderExtUnix;
        builder.with_any_thread(true);
    }
    // macOS: AppKit requires the process main thread, so this panics there.
    // buntauri will use a host subprocess on macOS (see BUNTAURI.md).
    let mut event_loop = builder.build();
    if ready.send(event_loop.create_proxy()).is_err() {
        return;
    }

    let mut windows: HashMap<WindowId, Entry> = HashMap::new();
    let mut by_native: HashMap<tao::window::WindowId, WindowId> = HashMap::new();

    event_loop.run_return(|event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(cmd) => match cmd {
                Command::Create(id, opts) => match create(target, id, opts, &emit, &assets) {
                    Ok(entry) => {
                        by_native.insert(entry.window.id(), id);
                        windows.insert(id, entry);
                        emit(HostEvent::Created { window: id });
                    }
                    Err(e) => emit(HostEvent::Error { window: Some(id), message: e }),
                },
                Command::Eval(id, js) => {
                    if let Some(e) = windows.get(&id) {
                        if let Err(err) = e.webview.evaluate_script(&js) {
                            emit(HostEvent::Error { window: Some(id), message: err.to_string() });
                        }
                    }
                }
                Command::SetTitle(id, title) => {
                    if let Some(e) = windows.get(&id) {
                        e.window.set_title(&title);
                    }
                }
                Command::Close(id) => {
                    if let Some(e) = windows.remove(&id) {
                        by_native.remove(&e.window.id());
                        emit(HostEvent::Closed { window: id });
                    }
                }
                Command::Shutdown => {
                    windows.clear();
                    by_native.clear();
                    *control_flow = ControlFlow::Exit;
                }
            },
            Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. } => {
                if let Some(id) = by_native.remove(&window_id) {
                    windows.remove(&id);
                    emit(HostEvent::Closed { window: id });
                }
            }
            _ => {}
        }
    });

    emit(HostEvent::Exited);
}

fn create(
    target: &EventLoopWindowTarget<Command>,
    id: WindowId,
    opts: WindowOptions,
    emit: &Emit,
    assets: &Arc<dyn AssetProvider>,
) -> Result<Entry, String> {
    let window = WindowBuilder::new()
        .with_title(&opts.title)
        .with_inner_size(LogicalSize::new(opts.width, opts.height))
        .build(target)
        .map_err(|e| e.to_string())?;

    let ipc_emit = emit.clone();
    let assets = assets.clone();
    let mut builder = WebViewBuilder::new()
        .with_devtools(opts.devtools)
        .with_initialization_script(BRIDGE_JS)
        .with_custom_protocol(ASSET_SCHEME.into(), move |_, req| serve_asset(&*assets, &req))
        .with_ipc_handler(move |req: Request<String>| on_ipc(id, req.body(), &ipc_emit));

    builder = match (opts.url, opts.html) {
        (Some(url), _) => builder.with_url(resolve_url(&url)),
        (None, Some(html)) => builder.with_html(html),
        (None, None) => builder.with_url(ASSET_ROOT),
    };

    #[cfg(not(any(target_os = "linux", target_os = "dragonfly", target_os = "freebsd", target_os = "netbsd", target_os = "openbsd")))]
    let webview = builder.build(&window);
    #[cfg(any(target_os = "linux", target_os = "dragonfly", target_os = "freebsd", target_os = "netbsd", target_os = "openbsd"))]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder.build_gtk(window.default_vbox().unwrap())
    };

    Ok(Entry { webview: webview.map_err(|e| e.to_string())?, window })
}

fn resolve_url(url: &str) -> String {
    if url.contains("://") || url.starts_with("data:") || url.starts_with("about:") {
        url.to_string()
    } else {
        format!("{ASSET_ROOT}{}", url.trim_start_matches('/'))
    }
}

fn serve_asset(assets: &dyn AssetProvider, req: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let path = req.uri().path().trim_start_matches('/');
    let path = if path.is_empty() || path.ends_with('/') {
        format!("{path}index.html")
    } else {
        path.to_string()
    };
    match assets.get(&path) {
        Some(a) => Response::builder()
            .header(CONTENT_TYPE, a.mime.as_ref())
            .body(a.bytes)
            .unwrap(),
        None => Response::builder()
            .status(404)
            .header(CONTENT_TYPE, "text/plain")
            .body(Cow::Borrowed(&b"not found"[..]))
            .unwrap(),
    }
}

fn on_ipc(window: WindowId, body: &str, emit: &Emit) {
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(body);
    let msg = match parsed {
        Ok(serde_json::Value::Object(m)) => m,
        _ => {
            emit(HostEvent::Error { window: Some(window), message: format!("bad ipc message: {body}") });
            return;
        }
    };
    let call = msg.get("id").and_then(|v| v.as_u64());
    let cmd = msg.get("cmd").and_then(|v| v.as_str());
    match (call, cmd) {
        (Some(call), Some(cmd)) => emit(HostEvent::Invoke {
            window,
            call,
            cmd: cmd.to_string(),
            args: msg.get("args").map(|a| a.to_string()).unwrap_or_else(|| "null".into()),
        }),
        _ => emit(HostEvent::Error { window: Some(window), message: format!("bad ipc message: {body}") }),
    }
}

/// Guess a MIME type from a file extension.
pub fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
