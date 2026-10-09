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

use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::Window;
use wry::http::{header::CONTENT_TYPE, Request, Response};
use wry::{DragDropEvent, WebContext, WebView, WebViewBuilder};

mod options;
pub use options::{BackgroundThrottling, Color, IpcPolicy, PreventOverflow, ScrollBarStyle, Theme, WindowOptions};

pub type WindowId = u32;

/// Scheme used for embedded assets: `app://localhost/<path>`.
/// On Windows wry maps this to `http://app.localhost/<path>`.
pub const ASSET_SCHEME: &str = "app";
pub const ASSET_ROOT: &str = "app://localhost/";

/// JS injected into every page before any page script runs.
const BRIDGE_JS: &str = include_str!("bridge.js");

/// Events sent from the UI thread to the host.
#[derive(Debug)]
pub enum HostEvent {
    /// A window and its webview were created.
    Created { window: WindowId },
    /// The page called `window.__BUNTAURI__.invoke(cmd, args)`.
    /// Answer with [`UiThread::resolve`] / [`UiThread::reject`].
    /// `origin` is the calling page's URL; `remote` is true when it was let in
    /// by [`IpcPolicy::remote`] rather than being an `app://` page.
    Invoke { window: WindowId, call: u64, cmd: String, args: String, origin: String, remote: bool },
    /// Native file drag & drop (`dragDropEnabled`). `kind` is `enter`, `over`,
    /// `drop` or `leave`; position is in physical pixels relative to the webview.
    DragDrop { window: WindowId, kind: &'static str, paths: Vec<String>, x: i32, y: i32 },
    /// The user asked to close the window. It is already destroyed.
    Closed { window: WindowId },
    Error { window: Option<WindowId>, message: String },
    /// Something was ignored, e.g. an unknown or unsupported window option.
    Warning { window: Option<WindowId>, message: String },
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

    /// Create a window from JSON options (Tauri `WindowConfig` names), as sent by Bun JS.
    pub fn create_window_json(&self, json: &str) -> Result<WindowId, String> {
        Ok(self.create_window(WindowOptions::from_json(json)?))
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
    label: String,
    // Field order matters: the webview drops before its window, and both
    // before the web context that holds the data directory.
    webview: WebView,
    window: Window,
    _context: Option<Box<WebContext>>,
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
                Command::Create(id, opts) => match create(target, id, opts, &windows, &emit, &assets) {
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
    windows: &HashMap<WindowId, Entry>,
    emit: &Emit,
    assets: &Arc<dyn AssetProvider>,
) -> Result<Entry, String> {
    if windows.values().any(|e| e.label == opts.label) {
        return Err(format!("a window with label {:?} already exists", opts.label));
    }
    for message in opts.warnings() {
        emit(HostEvent::Warning { window: Some(id), message });
    }

    #[allow(unused_mut)]
    let mut wb = opts.window_builder();
    if let Some(parent) = &opts.parent {
        let owner = windows
            .values()
            .find(|e| &e.label == parent)
            .ok_or_else(|| format!("parent window {parent:?} not found"))?;
        #[cfg(target_os = "windows")]
        {
            use tao::platform::windows::{WindowBuilderExtWindows, WindowExtWindows};
            wb = wb.with_owner_window(owner.window.hwnd());
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = owner;
            emit(HostEvent::Warning { window: Some(id), message: "window option \"parent\" is not supported on this platform yet".into() });
        }
    }
    let window = wb.build(target).map_err(|e| e.to_string())?;

    let mut context = opts.data_directory.clone().map(|dir| Box::new(WebContext::new(Some(dir))));
    let mut builder = match context.as_deref_mut() {
        Some(ctx) => WebViewBuilder::new_with_web_context(ctx),
        None => WebViewBuilder::new(),
    };
    builder = opts.apply_webview(builder)?;

    let ipc_emit = emit.clone();
    // Inline HTML comes from the host itself, so it is trusted like app://.
    let trust_inline = opts.url.is_none() && opts.html.is_some();
    let policy = opts.ipc.clone();
    let assets = assets.clone();
    builder = builder
        .with_initialization_script(BRIDGE_JS)
        .with_custom_protocol(ASSET_SCHEME.into(), move |_, req| serve_asset(&*assets, &req))
        .with_ipc_handler(move |req: Request<String>| {
            let origin = req.uri().to_string();
            match ipc_access(req.uri(), trust_inline, &policy) {
                Access::Local => on_ipc(id, req.body(), &origin, false, None, &ipc_emit),
                Access::Remote => on_ipc(id, req.body(), &origin, true, policy.remote_commands.as_deref(), &ipc_emit),
                Access::Denied => ipc_emit(HostEvent::Error { window: Some(id), message: format!("ipc blocked from {origin}") }),
            }
        });

    if opts.drag_drop_enabled {
        let dd_emit = emit.clone();
        builder = builder.with_drag_drop_handler(move |ev| {
            let paths = |p: Vec<std::path::PathBuf>| p.into_iter().map(|p| p.to_string_lossy().into_owned()).collect();
            let (kind, paths, (x, y)) = match ev {
                DragDropEvent::Enter { paths: p, position } => ("enter", paths(p), position),
                DragDropEvent::Over { position } => ("over", Vec::new(), position),
                DragDropEvent::Drop { paths: p, position } => ("drop", paths(p), position),
                DragDropEvent::Leave => ("leave", Vec::new(), (0, 0)),
                _ => return false,
            };
            dd_emit(HostEvent::DragDrop { window: id, kind, paths, x, y });
            // Block the OS default (the webview opening the file).
            true
        });
    }

    builder = match (&opts.url, &opts.html) {
        (Some(url), _) => builder.with_url(resolve_url(url)),
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
    let webview = webview.map_err(|e| e.to_string())?;

    opts.place(&window);
    Ok(Entry { label: opts.label, webview, window, _context: context })
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

enum Access {
    Local,
    Remote,
    Denied,
}

/// Our own pages (`app://localhost`, or `http(s)://app.localhost` on
/// Windows/Android) are local. Anything else needs a matching remote pattern.
fn ipc_access(uri: &wry::http::Uri, trust_inline: bool, policy: &IpcPolicy) -> Access {
    let host = uri.host().unwrap_or("");
    let local = match uri.scheme_str() {
        Some(s) if s == ASSET_SCHEME => host == "localhost",
        Some("http") | Some("https") => host == format!("{ASSET_SCHEME}.localhost"),
        // about:blank / data: for inline HTML set by the host.
        _ => trust_inline,
    };
    if local {
        Access::Local
    } else if policy.remote.iter().any(|p| url_matches(p, uri)) {
        Access::Remote
    } else {
        Access::Denied
    }
}

/// Match a URL against a pattern like `https://*.example.com/app/*`.
/// Scheme and port must match exactly. A host has no `/`, so a `*` in the
/// host pattern can't reach into the path (`https://*.a.nl` won't match
/// `https://evil.com/.a.nl`).
fn url_matches(pattern: &str, uri: &wry::http::Uri) -> bool {
    let Some((scheme, rest)) = pattern.split_once("://") else { return false };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/*"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(p) => (h, Some(p)),
            Err(_) => return false,
        },
        None => (authority, None),
    };
    uri.scheme_str() == Some(scheme)
        && uri.port_u16() == port
        && glob(&host.to_ascii_lowercase(), &uri.host().unwrap_or("").to_ascii_lowercase())
        && glob(path, uri.path())
}

/// `*` matches any run of characters, including none.
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

fn on_ipc(window: WindowId, body: &str, origin: &str, remote: bool, allowed: Option<&[String]>, emit: &Emit) {
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
        (Some(call), Some(cmd)) => {
            if allowed.is_some_and(|list| !list.iter().any(|c| c == cmd)) {
                emit(HostEvent::Error { window: Some(window), message: format!("ipc command {cmd:?} not allowed from {origin}") });
                return;
            }
            emit(HostEvent::Invoke {
                window,
                call,
                cmd: cmd.to_string(),
                args: msg.get("args").map(|a| a.to_string()).unwrap_or_else(|| "null".into()),
                origin: origin.to_string(),
                remote,
            })
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pattern: &str, url: &str) -> bool {
        url_matches(pattern, &url.parse().unwrap())
    }

    #[test]
    fn remote_patterns() {
        assert!(m("https://example.com", "https://example.com/"));
        assert!(m("https://example.com", "https://example.com/deep/page?q=1"));
        assert!(m("https://*.example.com", "https://app.example.com/x"));
        assert!(!m("https://*.example.com", "https://example.com/"));
        assert!(!m("https://*.example.com", "https://evil.com/.example.com"));
        assert!(!m("https://*.example.com", "https://example.com.evil.com/"));
        assert!(!m("https://example.com", "http://example.com/"));
        assert!(!m("https://example.com", "https://example.com:8443/"));
        assert!(m("https://example.com:8443", "https://example.com:8443/"));
        assert!(m("https://example.com/app/*", "https://example.com/app/x"));
        assert!(!m("https://example.com/app/*", "https://example.com/other"));
        assert!(m("http://localhost:5173", "http://localhost:5173/"));
        assert!(m("https://EXAMPLE.com", "https://example.COM/"));
        assert!(!m("https://example.com:abc", "https://example.com/"));
    }

    #[test]
    fn local_remote_denied() {
        let u = |s: &str| s.parse::<wry::http::Uri>().unwrap();
        let none = IpcPolicy::default();
        assert!(matches!(ipc_access(&u("http://app.localhost/"), false, &none), Access::Local));
        assert!(matches!(ipc_access(&u("app://localhost/x"), false, &none), Access::Local));
        assert!(matches!(ipc_access(&u("https://example.com/"), false, &none), Access::Denied));
        assert!(matches!(ipc_access(&u("http://app.localhost.evil.com/"), false, &none), Access::Denied));
        let p = IpcPolicy { remote: vec!["https://example.com".into()], remote_commands: None };
        assert!(matches!(ipc_access(&u("https://example.com/"), false, &p), Access::Remote));
    }
}
