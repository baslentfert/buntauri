//! buntauri window layer.
//!
//! Runs tao + wry on a dedicated UI thread. The host thread (Bun's JS
//! thread) talks to it through [`UiThread`] (commands in) and an event
//! callback (events out). Nothing here blocks the host thread.
//!
//! Inside Bun the event callback pushes onto the concurrent task queue and
//! wakes the event loop; in the standalone demo it is just an mpsc channel.
//!
//! macOS: AppKit only runs on the process main thread, which belongs to the
//! host. There the UI runs in a child process instead (same executable, see
//! [`run_ui_host_if_requested`]); [`UiThread`] has the same API either way.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::run_return::EventLoopExtRunReturn;
use serde::{Deserialize, Serialize};
use tao::window::Window;
use wry::http::{header::CONTENT_TYPE, Request, Response};
use wry::{DragDropEvent, WebContext, WebView, WebViewBuilder};

mod control;
mod extras;
#[cfg(unix)]
mod host;
mod native;
mod options;
pub use control::WindowOp;
pub use extras::{clipboard_read_text, clipboard_write_text, DialogSpec, NotificationSpec};
pub use native::{decode_base64, decode_png, MenuItemSpec, Owner, TraySpec};
pub use options::{BackgroundThrottling, Color, ExternalLinks, IpcPolicy, PreventOverflow, ScrollBarStyle, Theme, WindowOptions};

/// Id of a tray icon. Shares the id space with windows.
pub type TrayId = u32;

/// Straight RGBA8 pixels: (pixels, width, height).
type Rgba = (Vec<u8>, u32, u32);

/// Serde for [`Rgba`] as `[base64, width, height]`, so icons cross the UI
/// host pipe as compact JSON.
mod rgba_serde {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &super::Rgba, s: S) -> Result<S::Ok, S::Error> {
        (base64::engine::general_purpose::STANDARD.encode(&v.0), v.1, v.2).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<super::Rgba, D::Error> {
        let (b64, w, h) = <(String, u32, u32)>::deserialize(d)?;
        let px = base64::engine::general_purpose::STANDARD.decode(b64).map_err(serde::de::Error::custom)?;
        Ok((px, w, h))
    }

    pub mod opt {
        use super::*;

        pub fn serialize<S: Serializer>(v: &Option<super::super::Rgba>, s: S) -> Result<S::Ok, S::Error> {
            match v {
                Some(v) => s.serialize_some(&Wrap(v)),
                None => s.serialize_none(),
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<super::super::Rgba>, D::Error> {
            #[derive(Deserialize)]
            struct Owned(#[serde(with = "super")] super::super::Rgba);
            Ok(Option::<Owned>::deserialize(d)?.map(|o| o.0))
        }

        struct Wrap<'a>(&'a super::super::Rgba);

        impl Serialize for Wrap<'_> {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                super::serialize(self.0, s)
            }
        }
    }
}

pub type WindowId = u32;

/// Scheme used for embedded assets: `app://localhost/<path>`.
/// On Windows wry maps this to `http://app.localhost/<path>`.
pub const ASSET_SCHEME: &str = "app";
pub const ASSET_ROOT: &str = "app://localhost/";

/// Where inline HTML is served on macOS (see `create`).
const INLINE_PATH: &str = "/__buntauri/inline.html";

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
    /// Window lifecycle: `kind` is `resized` / `moved` (logical px in `data`),
    /// `focus`, `blur`, `scalechanged`, or `closerequested` (only with
    /// preventClose on; the window stays open).
    Window { window: WindowId, kind: &'static str, data: serde_json::Value },
    /// The window could not be created (bad options, duplicate label, ...).
    CreateFailed { window: WindowId, message: String },
    Error { window: Option<WindowId>, message: String },
    /// Something was ignored, e.g. an unknown or unsupported window option.
    Warning { window: Option<WindowId>, message: String },
    /// A menu item was clicked: from a window's menu bar / context menu
    /// (`window`) or from a tray menu (`tray`). `id` is the item's own id.
    Menu { window: Option<WindowId>, tray: Option<TrayId>, id: String },
    /// A tray icon was clicked. `kind`: `click` or `doubleclick`;
    /// `button`: `left`, `right` or `middle`; position in physical pixels.
    Tray { tray: TrayId, kind: &'static str, button: &'static str, x: f64, y: f64 },
    /// The tray icon could not be created.
    TrayFailed { tray: TrayId, message: String },
    /// Answer to a request (dialog, notification, shortcut registration):
    /// `value` on success, an error message (as `value`) otherwise.
    Reply { req: u64, ok: bool, value: serde_json::Value },
    /// A registered global shortcut fired. `state`: `pressed` or `released`.
    Shortcut { accelerator: String, state: &'static str },
    /// About the app as a whole. `kind`: `reopen` (macOS: Dock icon clicked;
    /// `data.hasVisibleWindows`) or `quitrequested` (macOS app menu Quit /
    /// Cmd+Q; the host decides whether to quit).
    App { kind: &'static str, data: serde_json::Value },
    /// The UI thread's event loop has stopped.
    Exited,
}

impl HostEvent {
    /// Parse what [`HostEvent::to_json`] wrote (the UI host sends events this way).
    pub fn from_json(json: &str) -> Result<HostEvent, String> {
        let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("bad event: {e}"))?;
        let num = |k: &str| v[k].as_u64().map(|n| n as u32);
        let window = || num("window").ok_or_else(|| format!("event without window: {json}"));
        let tray = || num("tray").ok_or_else(|| format!("event without tray: {json}"));
        let text = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        // Kinds are a small fixed set; map them back to their static names.
        let kind = |k: &str, known: &[&'static str]| -> &'static str {
            let s = v[k].as_str().unwrap_or_default();
            known.iter().copied().find(|&n| n == s).unwrap_or("unknown")
        };
        Ok(match v["type"].as_str().unwrap_or_default() {
            "created" => HostEvent::Created { window: window()? },
            "invoke" => HostEvent::Invoke {
                window: window()?,
                call: v["call"].as_u64().unwrap_or_default(),
                cmd: text("cmd"),
                args: v["args"].to_string(),
                origin: text("origin"),
                remote: v["remote"].as_bool().unwrap_or_default(),
            },
            "dragdrop" => HostEvent::DragDrop {
                window: window()?,
                kind: kind("kind", &["enter", "over", "drop", "leave"]),
                paths: v["paths"].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(String::from)).collect(),
                x: v["x"].as_i64().unwrap_or_default() as i32,
                y: v["y"].as_i64().unwrap_or_default() as i32,
            },
            "closed" => HostEvent::Closed { window: window()? },
            "window" => HostEvent::Window {
                window: window()?,
                kind: kind("kind", &["resized", "moved", "focus", "blur", "scalechanged", "closerequested", "externallink"]),
                data: v["data"].clone(),
            },
            "createfailed" => HostEvent::CreateFailed { window: window()?, message: text("message") },
            "error" => HostEvent::Error { window: num("window"), message: text("message") },
            "warning" => HostEvent::Warning { window: num("window"), message: text("message") },
            "menu" => HostEvent::Menu { window: num("window"), tray: num("tray"), id: text("id") },
            "tray" => HostEvent::Tray {
                tray: tray()?,
                kind: kind("kind", &["click", "doubleclick"]),
                button: kind("button", &["left", "right", "middle"]),
                x: v["x"].as_f64().unwrap_or_default(),
                y: v["y"].as_f64().unwrap_or_default(),
            },
            "trayfailed" => HostEvent::TrayFailed { tray: tray()?, message: text("message") },
            "app" => HostEvent::App { kind: kind("kind", &["reopen", "quitrequested"]), data: v["data"].clone() },
            "reply" => HostEvent::Reply {
                req: v["req"].as_u64().ok_or_else(|| format!("reply without req: {json}"))?,
                ok: v["ok"].as_bool().unwrap_or_default(),
                value: v["value"].clone(),
            },
            "shortcut" => HostEvent::Shortcut {
                accelerator: text("accelerator"),
                state: kind("state", &["pressed", "released"]),
            },
            "exited" => HostEvent::Exited,
            other => return Err(format!("unknown event type {other:?}")),
        })
    }

    /// The window this event is about, if any.
    pub fn window(&self) -> Option<WindowId> {
        match self {
            HostEvent::Created { window }
            | HostEvent::Invoke { window, .. }
            | HostEvent::DragDrop { window, .. }
            | HostEvent::Closed { window }
            | HostEvent::Window { window, .. }
            | HostEvent::CreateFailed { window, .. } => Some(*window),
            HostEvent::Error { window, .. } | HostEvent::Warning { window, .. } | HostEvent::Menu { window, .. } => *window,
            HostEvent::Tray { .. }
            | HostEvent::TrayFailed { .. }
            | HostEvent::Reply { .. }
            | HostEvent::Shortcut { .. }
            | HostEvent::App { .. }
            | HostEvent::Exited => None,
        }
    }

    /// The window or tray this event is about. Windows and trays share one
    /// id space, so a host can keep a single "alive" set for both.
    pub fn owner(&self) -> Option<u32> {
        match self {
            HostEvent::Tray { tray, .. } | HostEvent::TrayFailed { tray, .. } => Some(*tray),
            HostEvent::Menu { window, tray, .. } => window.or(*tray),
            _ => self.window(),
        }
    }

    /// A pending request is answered by this event.
    pub fn ends_request(&self) -> bool {
        matches!(self, HostEvent::Reply { .. })
    }

    /// The window or tray is gone after this event (closed or never created).
    pub fn ends_owner(&self) -> bool {
        matches!(self, HostEvent::Closed { .. } | HostEvent::CreateFailed { .. } | HostEvent::TrayFailed { .. })
    }

    /// JSON for a JS host: `{"type": "invoke", "window": 1, ...}`.
    /// `args` of an invoke is embedded as JSON, not as a string.
    pub fn to_json(&self) -> String {
        use serde_json::json;
        let v = match self {
            HostEvent::Created { window } => json!({ "type": "created", "window": window }),
            HostEvent::Invoke { window, call, cmd, args, origin, remote } => json!({
                "type": "invoke", "window": window, "call": call, "cmd": cmd,
                "args": serde_json::from_str::<serde_json::Value>(args).unwrap_or(serde_json::Value::Null),
                "origin": origin, "remote": remote,
            }),
            HostEvent::DragDrop { window, kind, paths, x, y } => {
                json!({ "type": "dragdrop", "window": window, "kind": kind, "paths": paths, "x": x, "y": y })
            }
            HostEvent::Closed { window } => json!({ "type": "closed", "window": window }),
            HostEvent::Window { window, kind, data } => {
                json!({ "type": "window", "window": window, "kind": kind, "data": data })
            }
            HostEvent::CreateFailed { window, message } => json!({ "type": "createfailed", "window": window, "message": message }),
            HostEvent::Error { window, message } => json!({ "type": "error", "window": window, "message": message }),
            HostEvent::Warning { window, message } => json!({ "type": "warning", "window": window, "message": message }),
            HostEvent::Menu { window, tray, id } => json!({ "type": "menu", "window": window, "tray": tray, "id": id }),
            HostEvent::Tray { tray, kind, button, x, y } => {
                json!({ "type": "tray", "tray": tray, "kind": kind, "button": button, "x": x, "y": y })
            }
            HostEvent::TrayFailed { tray, message } => json!({ "type": "trayfailed", "tray": tray, "message": message }),
            HostEvent::Reply { req, ok, value } => json!({ "type": "reply", "req": req, "ok": ok, "value": value }),
            HostEvent::App { kind, data } => json!({ "type": "app", "kind": kind, "data": data }),
            HostEvent::Shortcut { accelerator, state } => {
                json!({ "type": "shortcut", "accelerator": accelerator, "state": state })
            }
            HostEvent::Exited => json!({ "type": "exited" }),
        };
        v.to_string()
    }
}

/// Call this first thing in `main` (before anything else touches the
/// process): when this process was started as the macOS UI host, it runs the
/// UI and never returns. `make_assets` gets the host's
/// [`AssetProvider::host_spec`] and rebuilds the provider. Elsewhere, and in a
/// normal start, it returns right away.
pub fn run_ui_host_if_requested<F>(make_assets: F)
where
    F: FnOnce(Option<String>) -> Arc<dyn AssetProvider>,
{
    #[cfg(unix)]
    if std::env::var_os(host::ENV).is_some() {
        let spec = std::env::var(host::ENV_ASSETS).ok();
        host::run(make_assets(spec));
    }
    #[cfg(not(unix))]
    let _ = make_assets;
}

/// Serves files from a directory on disk. Paths that try to leave the
/// directory (`..`, absolute, drive letters) are refused.
pub struct DirAssets(pub std::path::PathBuf);

impl AssetProvider for DirAssets {
    fn host_spec(&self) -> Option<String> {
        // Absolute, so the UI host finds it whatever its working directory.
        Some(std::path::absolute(&self.0).unwrap_or_else(|_| self.0.clone()).to_string_lossy().into_owned())
    }

    fn get(&self, path: &str) -> Option<Asset> {
        let rel = std::path::Path::new(path);
        if rel.components().any(|c| !matches!(c, std::path::Component::Normal(_))) {
            return None;
        }
        let bytes = std::fs::read(self.0.join(rel)).ok()?;
        Some(Asset { bytes: Cow::Owned(bytes), mime: Cow::Borrowed(mime_for(path)) })
    }
}

/// Serves nothing; `app://` answers 404.
pub struct NoAssets;

impl AssetProvider for NoAssets {
    fn get(&self, _: &str) -> Option<Asset> {
        None
    }
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

    /// macOS: a string from which the UI host process rebuilds this provider
    /// (see [`run_ui_host_if_requested`]); `None` serves nothing there.
    fn host_spec(&self) -> Option<String> {
        None
    }
}

/// What the host asks of the UI. Serializable: on macOS it travels to the
/// UI host process as one JSON line.
#[derive(Serialize, Deserialize)]
enum Command {
    Create(WindowId, WindowOptions),
    Eval(WindowId, String),
    SetTitle(WindowId, String),
    Close(WindowId),
    SetIcon(WindowId, #[serde(with = "rgba_serde")] Rgba),
    Op(WindowId, WindowOp),
    SetMenu(WindowId, Option<Vec<MenuItemSpec>>),
    Popup(WindowId, Vec<MenuItemSpec>, Option<(f64, f64)>),
    TrayCreate(TrayId, TraySpec, #[serde(with = "rgba_serde::opt")] Option<Rgba>),
    TrayUpdate(TrayId, TraySpec, #[serde(with = "rgba_serde::opt")] Option<Rgba>),
    TrayRemove(TrayId),
    /// Evaluate an expression in the page; its JSON value comes back as a Reply.
    EvalResult(WindowId, u64, String),
    /// List the monitors; answered by a Reply.
    Monitors(u64),
    Dialog(u64, Option<WindowId>, DialogSpec),
    Notify(u64, NotificationSpec),
    Shortcut(u64, bool, String),
    /// Internal (UI side only): the page started (false) or finished (true) loading.
    PageLoad(WindowId, bool),
    Shutdown,
}

/// Handle to the UI thread. Cheap to use from any thread.
pub struct UiThread {
    backend: Backend,
    states: States,
    next_id: AtomicU32,
    next_req: AtomicU64,
}

enum Backend {
    /// tao + wry on a thread of this process (Windows).
    #[cfg_attr(unix, allow(dead_code))]
    Thread { proxy: EventLoopProxy<Command>, join: Option<JoinHandle<()>> },
    /// tao + wry on the main thread of a child process (macOS, Linux/BSD).
    /// macOS: AppKit needs the main thread. Linux: GTK/WebKitGTK (and the
    /// JavaScriptCore of WebKitGTK) stay out of the host process.
    #[cfg(unix)]
    Host(host::Client),
}

impl UiThread {
    /// Start the UI. On macOS and Linux this starts the UI host process,
    /// which serves `app://` from [`AssetProvider::host_spec`] of `assets`;
    /// it fails if that process cannot show windows (e.g. no WebKitGTK).
    pub fn spawn<F>(on_event: F, assets: Arc<dyn AssetProvider>) -> std::io::Result<Self>
    where
        F: Fn(HostEvent) + Send + Sync + 'static,
    {
        let states: States = Default::default();
        #[cfg(unix)]
        let backend = Backend::Host(host::Client::spawn(Arc::new(on_event), assets.host_spec(), states.clone())?);
        #[cfg(not(unix))]
        let backend = {
            let (tx, rx) = mpsc::channel();
            let ui_states = states.clone();
            let on_state: StateSink = Arc::new(move |id, state| {
                let mut map = ui_states.lock().unwrap();
                match state {
                    Some(v) => map.insert(id, v),
                    None => map.remove(&id),
                };
            });
            let join = std::thread::Builder::new()
                .name("buntauri-ui".into())
                .spawn(move || ui_main(tx, Arc::new(on_event), assets, on_state))?;
            let proxy = rx
                .recv()
                .map_err(|_| std::io::Error::other("UI thread failed to start"))?;
            Backend::Thread { proxy, join: Some(join) }
        };
        Ok(Self { backend, states, next_id: AtomicU32::new(1), next_req: AtomicU64::new(1) })
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

    /// Run a window operation (show, hide, setSize, ...) given as JSON.
    pub fn window_op_json(&self, window: WindowId, json: &str) -> Result<(), String> {
        self.send(Command::Op(window, WindowOp::from_json(json)?));
        Ok(())
    }

    /// The window's last known state as JSON (`null` before it exists or
    /// after it closed). Updated by the UI thread after every change.
    pub fn window_state_json(&self, window: WindowId) -> String {
        self.states.lock().unwrap().get(&window).map_or_else(|| "null".into(), |v| v.to_string())
    }

    /// Evaluate a JavaScript expression in the page. Answered by a Reply with
    /// the expression's value (JSON). Promises are not awaited.
    pub fn eval_result(&self, window: WindowId, js: &str) -> u64 {
        let req = self.next_req.fetch_add(1, Ordering::Relaxed);
        self.send(Command::EvalResult(window, req, js.to_string()));
        req
    }

    /// List the monitors (name, position, size, scale factor, primary). Answered by a Reply.
    pub fn monitors(&self) -> u64 {
        let req = self.next_req.fetch_add(1, Ordering::Relaxed);
        self.send(Command::Monitors(req));
        req
    }

    /// Show a dialog ([`DialogSpec`] as JSON), modal to `window` if given.
    /// Returns the request id; the answer comes as [`HostEvent::Reply`].
    pub fn dialog_json(&self, window: Option<WindowId>, json: &str) -> Result<u64, String> {
        let spec = DialogSpec::from_json(json)?;
        let req = self.next_req.fetch_add(1, Ordering::Relaxed);
        self.send(Command::Dialog(req, window, spec));
        Ok(req)
    }

    /// Show a desktop notification ([`NotificationSpec`] as JSON). Answered by a Reply.
    pub fn notify_json(&self, json: &str) -> Result<u64, String> {
        let spec = NotificationSpec::from_json(json)?;
        let req = self.next_req.fetch_add(1, Ordering::Relaxed);
        self.send(Command::Notify(req, spec));
        Ok(req)
    }

    /// Register (or unregister) a global shortcut like "CmdOrCtrl+Shift+K".
    /// Answered by a Reply (fails if another app holds the shortcut).
    pub fn shortcut(&self, accelerator: &str, register: bool) -> Result<u64, String> {
        extras::parse_shortcut(accelerator)?;
        let req = self.next_req.fetch_add(1, Ordering::Relaxed);
        self.send(Command::Shortcut(req, register, accelerator.to_string()));
        Ok(req)
    }

    /// Window icon (title bar + taskbar) from a base64 PNG.
    pub fn set_icon_base64(&self, window: WindowId, png_base64: &str) -> Result<(), String> {
        let rgba = decode_png(&decode_base64(png_base64)?)?;
        self.send(Command::SetIcon(window, rgba));
        Ok(())
    }

    /// Menu bar from a JSON array of [`MenuItemSpec`]; `null` removes it.
    pub fn set_menu_json(&self, window: WindowId, json: &str) -> Result<(), String> {
        let specs: Option<Vec<MenuItemSpec>> = serde_json::from_str(json).map_err(|e| format!("invalid menu: {e}"))?;
        self.send(Command::SetMenu(window, specs));
        Ok(())
    }

    /// Show a context menu at a logical position in the window (or at the cursor).
    pub fn popup_menu_json(&self, window: WindowId, json: &str, at: Option<(f64, f64)>) -> Result<(), String> {
        let specs: Vec<MenuItemSpec> = serde_json::from_str(json).map_err(|e| format!("invalid menu: {e}"))?;
        self.send(Command::Popup(window, specs, at));
        Ok(())
    }

    /// Create a tray icon from a JSON [`TraySpec`]. The icon PNG is decoded
    /// here, so a bad icon is an error right away.
    pub fn create_tray_json(&self, json: &str) -> Result<TrayId, String> {
        let (spec, icon) = parse_tray(json)?;
        if icon.is_none() {
            return Err("a tray needs an icon (base64 PNG)".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.send(Command::TrayCreate(id, spec, icon));
        Ok(id)
    }

    /// Change a tray: only the fields present in the JSON are applied.
    pub fn update_tray_json(&self, tray: TrayId, json: &str) -> Result<(), String> {
        let (spec, icon) = parse_tray(json)?;
        self.send(Command::TrayUpdate(tray, spec, icon));
        Ok(())
    }

    pub fn remove_tray(&self, tray: TrayId) {
        self.send(Command::TrayRemove(tray));
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
        match &mut self.backend {
            Backend::Thread { join, .. } => {
                if let Some(j) = join.take() {
                    let _ = j.join();
                }
            }
            #[cfg(unix)]
            Backend::Host(client) => client.wait(),
        }
    }

    fn send(&self, cmd: Command) {
        match &self.backend {
            // Only fails once the loop has exited; nothing left to do then.
            Backend::Thread { proxy, .. } => {
                let _ = proxy.send_event(cmd);
            }
            #[cfg(unix)]
            Backend::Host(client) => client.send(&cmd),
        }
    }
}

/// Linux/BSD, in the UI host process: GTK and WebKitGTK may be linked
/// through lazy-loading stubs (no NEEDED entries, so the binary starts
/// without them). Load them up front: a missing library is then a clear
/// error, not an abort on the first GTK call.
#[cfg(any(target_os = "linux", target_os = "dragonfly", target_os = "freebsd", target_os = "netbsd", target_os = "openbsd"))]
pub(crate) fn load_gtk() -> Result<(), String> {
    use std::ffi::{c_char, c_int, c_void, CStr};
    unsafe extern "C" {
        fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void;
        fn dlerror() -> *mut c_char;
    }
    const RTLD_LAZY: c_int = 0x1;
    const RTLD_GLOBAL: c_int = 0x100;
    // WebKitGTK pulls in GTK 3, GLib, libsoup and its JavaScriptCore.
    for lib in [c"libwebkit2gtk-4.1.so.0", c"libgtk-3.so.0"] {
        // SAFETY: plain dlopen of a library name; the handle is kept (never closed).
        if unsafe { dlopen(lib.as_ptr(), RTLD_LAZY | RTLD_GLOBAL) }.is_null() {
            // SAFETY: dlerror returns a valid C string right after a failed dlopen.
            let why = unsafe { CStr::from_ptr(dlerror()) }.to_string_lossy();
            return Err(format!(
                "cannot open windows: {} is not available ({why}). Install WebKitGTK 4.1, e.g. `apt install libwebkit2gtk-4.1-0`.",
                lib.to_string_lossy()
            ));
        }
    }
    Ok(())
}

type Emit = Arc<dyn Fn(HostEvent) + Send + Sync>;
type States = Arc<Mutex<HashMap<WindowId, serde_json::Value>>>;
/// Where the UI reports a window's state snapshot (`None`: the window is gone).
type StateSink = Arc<dyn Fn(WindowId, Option<serde_json::Value>) + Send + Sync>;

fn parse_tray(json: &str) -> Result<(TraySpec, Option<Rgba>), String> {
    let mut spec: TraySpec = serde_json::from_str(json).map_err(|e| format!("invalid tray: {e}"))?;
    // Decoded here once; the UI only needs the pixels.
    let icon = match spec.icon.take() {
        Some(b64) => Some(decode_png(&decode_base64(&b64)?)?),
        None => None,
    };
    Ok((spec, icon))
}

struct Entry {
    label: String,
    /// Menu bar and the last context menu; kept alive while the window lives.
    menu: Option<muda::Menu>,
    popup: Option<muda::Menu>,
    /// Close button emits `closerequested` instead of closing.
    prevent_close: bool,
    /// The page is (re)loading: scripts sent now would be lost (WKWebView
    /// drops them), so they wait in `queued` until it has finished.
    loading: bool,
    queued: Vec<Command>,
    // Field order matters: the webview drops before its window, and both
    // before the web context that holds the data directory.
    webview: WebView,
    window: Window,
    _context: Option<Box<WebContext>>,
}

fn ui_main(ready: mpsc::Sender<EventLoopProxy<Command>>, emit: Emit, assets: Arc<dyn AssetProvider>, on_state: StateSink) {
    let snap = |id: WindowId, e: &Entry| on_state(id, Some(control::snapshot(&e.window, &e.webview, e.prevent_close)));
    let forget = |id: WindowId| on_state(id, None);
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
    // Windows: menu accelerators (Ctrl+S, ...) only fire when the message
    // loop translates them, as Tauri does. Menus register their table here.
    #[cfg(target_os = "windows")]
    let accels: std::rc::Rc<std::cell::RefCell<HashMap<WindowId, isize>>> = Default::default();
    #[cfg(target_os = "windows")]
    {
        use tao::platform::windows::EventLoopBuilderExtWindows;
        #[link(name = "user32")]
        unsafe extern "system" {
            fn TranslateAcceleratorW(hwnd: isize, haccel: isize, msg: *const core::ffi::c_void) -> i32;
        }
        let accels = accels.clone();
        builder.with_msg_hook(move |msg| {
            // MSG starts with its HWND.
            let hwnd = unsafe { *(msg as *const isize) };
            accels.borrow().values().any(|&h| unsafe { TranslateAcceleratorW(hwnd, h, msg) } == 1)
        });
    }
    // macOS: AppKit requires the process main thread; there this runs in the
    // UI host process (host.rs), on its main thread.
    let mut event_loop = builder.build();
    // macOS: the menu bar while no window sets its own (app menu, Edit, Window).
    // tao sets none, and without an Edit menu copy/paste shortcuts do nothing.
    #[cfg(target_os = "macos")]
    let app_menu = native::default_menu(&native::app_name())
        .map_err(|message| emit(HostEvent::Error { window: None, message }))
        .ok();
    #[cfg(target_os = "macos")]
    let restore_app_menu = || {
        if let Some(m) = &app_menu {
            m.init_for_nsapp();
        }
    };
    let proxy = event_loop.create_proxy();
    if ready.send(event_loop.create_proxy()).is_err() {
        return;
    }

    let mut windows: HashMap<WindowId, Entry> = HashMap::new();
    let mut by_native: HashMap<tao::window::WindowId, WindowId> = HashMap::new();
    let mut trays: HashMap<TrayId, tray_icon::TrayIcon> = HashMap::new();
    // Global shortcuts: the manager lives on this thread (it needs its message
    // loop on Windows); the handler maps hotkey ids back to accelerators.
    let mut hotkeys: Option<global_hotkey::GlobalHotKeyManager> = None;
    let shortcut_names: Arc<Mutex<HashMap<u32, String>>> = Default::default();
    {
        let emit = emit.clone();
        let names = shortcut_names.clone();
        global_hotkey::GlobalHotKeyEvent::set_event_handler(Some(move |e: global_hotkey::GlobalHotKeyEvent| {
            let Some(accelerator) = names.lock().unwrap().get(&e.id).cloned() else { return };
            let state = match e.state {
                global_hotkey::HotKeyState::Pressed => "pressed",
                global_hotkey::HotKeyState::Released => "released",
            };
            emit(HostEvent::Shortcut { accelerator, state });
        }));
    }

    // One global handler each (muda/tray-icon allow only one): route by the
    // owner prefix in the menu id, and by tray id.
    {
        let emit = emit.clone();
        muda::MenuEvent::set_event_handler(Some(move |e: muda::MenuEvent| {
            if let Some((owner, id)) = Owner::parse(&e.id.0) {
                let (window, tray) = match owner {
                    Owner::Window(w) => (Some(w), None),
                    Owner::Tray(t) => (None, Some(t)),
                    Owner::App => {
                        if id == "quit" {
                            emit(HostEvent::App { kind: "quitrequested", data: serde_json::Value::Null });
                        }
                        return;
                    }
                };
                emit(HostEvent::Menu { window, tray, id });
            }
        }));
    }
    {
        let emit = emit.clone();
        tray_icon::TrayIconEvent::set_event_handler(Some(move |e: tray_icon::TrayIconEvent| {
            use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent as T};
            let (id, kind, button, pos) = match e {
                T::Click { id, button, button_state: MouseButtonState::Up, position, .. } => (id, "click", button, position),
                T::DoubleClick { id, button, position, .. } => (id, "doubleclick", button, position),
                _ => return,
            };
            let Ok(tray) = id.0.parse::<TrayId>() else { return };
            let button = match button {
                MouseButton::Left => "left",
                MouseButton::Right => "right",
                MouseButton::Middle => "middle",
            };
            emit(HostEvent::Tray { tray, kind, button, x: pos.x, y: pos.y });
        }));
    }

    // Windows to snapshot again a little later: some operations take effect
    // asynchronously (on macOS: resizing, always-on-top, devtools).
    let mut resnap: Vec<(std::time::Instant, WindowId)> = Vec::new();

    event_loop.run_return(|event, target, control_flow| {
        let now = std::time::Instant::now();
        resnap.retain(|&(at, id)| {
            if at > now {
                return true;
            }
            if let Some(e) = windows.get(&id) {
                snap(id, e);
            }
            false
        });
        *control_flow = match resnap.iter().map(|&(at, _)| at).min() {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        };
        // The app has finished launching: now the menu bar can be set.
        #[cfg(target_os = "macos")]
        if matches!(event, Event::NewEvents(tao::event::StartCause::Init)) {
            restore_app_menu();
        }
        match event {
            Event::Reopen { has_visible_windows, .. } => {
                emit(HostEvent::App { kind: "reopen", data: serde_json::json!({ "hasVisibleWindows": has_visible_windows }) });
            }
            Event::UserEvent(cmd) => match cmd {
                Command::Create(id, opts) => match create(target, &proxy, id, opts, &windows, &emit, &assets) {
                    Ok(entry) => {
                        by_native.insert(entry.window.id(), id);
                        snap(id, &entry);
                        windows.insert(id, entry);
                        emit(HostEvent::Created { window: id });
                    }
                    Err(e) => emit(HostEvent::CreateFailed { window: id, message: e }),
                },
                cmd @ (Command::Eval(..) | Command::EvalResult(..)) => match windows.get_mut(&script_window(&cmd)) {
                    Some(e) if e.loading => e.queued.push(cmd),
                    Some(e) => run_script(e, cmd, &emit),
                    None => run_script_closed(cmd, &emit),
                },
                Command::PageLoad(id, finished) => {
                    if let Some(e) = windows.get_mut(&id) {
                        e.loading = !finished;
                        if finished {
                            for cmd in std::mem::take(&mut e.queued) {
                                run_script(e, cmd, &emit);
                            }
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
                        #[cfg(target_os = "macos")]
                        if e.menu.is_some() {
                            restore_app_menu();
                        }
                        by_native.remove(&e.window.id());
                        forget(id);
                        #[cfg(target_os = "windows")]
                        accels.borrow_mut().remove(&id);
                        emit(HostEvent::Closed { window: id });
                    }
                }
                Command::Op(id, op) => {
                    if let Some(e) = windows.get_mut(&id) {
                        if let Err(message) = control::apply(&e.window, &e.webview, &op, &mut e.prevent_close) {
                            emit(HostEvent::Error { window: Some(id), message });
                        }
                        snap(id, e);
                        for ms in [100, 600] {
                            resnap.push((now + std::time::Duration::from_millis(ms), id));
                        }
                        let first = resnap.iter().map(|&(at, _)| at).min().unwrap();
                        *control_flow = ControlFlow::WaitUntil(first);
                    }
                }
                Command::SetIcon(id, (rgba, w, h)) => {
                    if let Some(e) = windows.get(&id) {
                        match tao::window::Icon::from_rgba(rgba, w, h) {
                            Ok(icon) => e.window.set_window_icon(Some(icon)),
                            Err(err) => emit(HostEvent::Error { window: Some(id), message: format!("bad icon: {err}") }),
                        }
                    }
                }
                Command::SetMenu(id, specs) => {
                    if let Some(e) = windows.get_mut(&id) {
                        let result = set_menu_bar(e, id, specs);
                        #[cfg(target_os = "macos")]
                        if e.menu.is_none() {
                            restore_app_menu();
                        }
                        #[cfg(target_os = "windows")]
                        match &e.menu {
                            Some(m) => {
                                accels.borrow_mut().insert(id, m.haccel());
                            }
                            None => {
                                accels.borrow_mut().remove(&id);
                            }
                        }
                        if let Err(message) = result {
                            emit(HostEvent::Error { window: Some(id), message });
                        }
                    }
                }
                Command::Popup(id, specs, at) => {
                    if let Some(e) = windows.get_mut(&id) {
                        if let Err(message) = popup_menu(e, id, &specs, at) {
                            emit(HostEvent::Error { window: Some(id), message });
                        }
                    }
                }
                Command::TrayCreate(id, spec, icon) => match create_tray(id, &spec, icon) {
                    Ok(t) => {
                        trays.insert(id, t);
                    }
                    Err(message) => emit(HostEvent::TrayFailed { tray: id, message }),
                },
                Command::TrayUpdate(id, spec, icon) => {
                    if let Some(t) = trays.get(&id) {
                        if let Err(message) = update_tray(t, id, &spec, icon) {
                            emit(HostEvent::Error { window: None, message });
                        }
                    }
                }
                Command::TrayRemove(id) => {
                    trays.remove(&id);
                }
                Command::Dialog(req, window, spec) => {
                    #[allow(unused_mut)]
                    let mut parent = None;
                    #[cfg(target_os = "windows")]
                    if let Some(e) = window.and_then(|w| windows.get(&w)) {
                        use tao::platform::windows::WindowExtWindows;
                        parent = Some(extras::Parent(e.window.hwnd()));
                    }
                    #[cfg(not(target_os = "windows"))]
                    let _ = window;
                    // Dialogs block until answered: run them off this thread.
                    let reply = emit.clone();
                    let spawned = std::thread::Builder::new().name("buntauri-dialog".into()).spawn(move || {
                        let value = extras::run_dialog(&spec, parent);
                        reply(HostEvent::Reply { req, ok: true, value });
                    });
                    if let Err(e) = spawned {
                        emit(HostEvent::Reply { req, ok: false, value: e.to_string().into() });
                    }
                }
                Command::Notify(req, spec) => {
                    let reply = emit.clone();
                    let spawned = std::thread::Builder::new().name("buntauri-notify".into()).spawn(move || {
                        match extras::notify(&spec) {
                            Ok(()) => reply(HostEvent::Reply { req, ok: true, value: serde_json::Value::Null }),
                            Err(e) => reply(HostEvent::Reply { req, ok: false, value: e.into() }),
                        }
                    });
                    if let Err(e) = spawned {
                        emit(HostEvent::Reply { req, ok: false, value: e.to_string().into() });
                    }
                }
                Command::Monitors(req) => {
                    let primary = target.primary_monitor();
                    let list: Vec<serde_json::Value> = target
                        .available_monitors()
                        .map(|m| {
                            let scale = m.scale_factor();
                            let pos = m.position().to_logical::<f64>(scale);
                            let size = m.size().to_logical::<f64>(scale);
                            serde_json::json!({
                                "name": m.name(),
                                "x": pos.x, "y": pos.y,
                                "width": size.width, "height": size.height,
                                "scaleFactor": scale,
                                "primary": primary.as_ref().is_some_and(|p| p.name() == m.name() && p.position() == m.position()),
                            })
                        })
                        .collect();
                    emit(HostEvent::Reply { req, ok: true, value: list.into() });
                }
                Command::Shortcut(req, register, accelerator) => {
                    let result = (|| -> Result<(), String> {
                        let hotkey = extras::parse_shortcut(&accelerator)?;
                        if hotkeys.is_none() {
                            hotkeys = Some(global_hotkey::GlobalHotKeyManager::new().map_err(|e| e.to_string())?);
                        }
                        let manager = hotkeys.as_ref().unwrap();
                        if register {
                            manager.register(hotkey).map_err(|e| e.to_string())?;
                            shortcut_names.lock().unwrap().insert(hotkey.id(), accelerator.clone());
                        } else {
                            manager.unregister(hotkey).map_err(|e| e.to_string())?;
                            shortcut_names.lock().unwrap().remove(&hotkey.id());
                        }
                        Ok(())
                    })();
                    emit(match result {
                        Ok(()) => HostEvent::Reply { req, ok: true, value: serde_json::Value::Null },
                        Err(e) => HostEvent::Reply { req, ok: false, value: e.into() },
                    });
                }
                Command::Shutdown => {
                    trays.clear();
                    windows.clear();
                    by_native.clear();
                    *control_flow = ControlFlow::Exit;
                }
            },
            Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. } => {
                if let Some(&id) = by_native.get(&window_id) {
                    if windows.get(&id).is_some_and(|e| e.prevent_close) {
                        emit(HostEvent::Window { window: id, kind: "closerequested", data: serde_json::Value::Null });
                        return;
                    }
                }
                if let Some(id) = by_native.remove(&window_id) {
                    #[cfg(target_os = "macos")]
                    if windows.remove(&id).is_some_and(|e| e.menu.is_some()) {
                        restore_app_menu();
                    }
                    #[cfg(not(target_os = "macos"))]
                    windows.remove(&id);
                    forget(id);
                    #[cfg(target_os = "windows")]
                    accels.borrow_mut().remove(&id);
                    emit(HostEvent::Closed { window: id });
                }
            }
            Event::WindowEvent { window_id, event, .. } => {
                let Some(&id) = by_native.get(&window_id) else { return };
                let Some(e) = windows.get(&id) else { return };
                let scale = e.window.scale_factor();
                let (kind, data) = match event {
                    WindowEvent::Resized(s) => {
                        let s = s.to_logical::<f64>(scale);
                        ("resized", serde_json::json!({ "width": s.width, "height": s.height }))
                    }
                    WindowEvent::Moved(p) => {
                        let p = p.to_logical::<f64>(scale);
                        ("moved", serde_json::json!({ "x": p.x, "y": p.y }))
                    }
                    WindowEvent::Focused(true) => ("focus", serde_json::Value::Null),
                    WindowEvent::Focused(false) => ("blur", serde_json::Value::Null),
                    WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                        ("scalechanged", serde_json::json!({ "scaleFactor": scale_factor }))
                    }
                    _ => return,
                };
                // The event's own size/position: on macOS the window can still
                // report the old values while the event is delivered.
                let mut state = control::snapshot(&e.window, &e.webview, e.prevent_close);
                if let (Some(state), Some(fresh)) = (state.as_object_mut(), data.as_object()) {
                    state.extend(fresh.clone());
                }
                on_state(id, Some(state));
                emit(HostEvent::Window { window: id, kind, data });
            }
            _ => {}
        }
    });

    emit(HostEvent::Exited);
}

/// The window an `Eval`/`EvalResult` is for.
fn script_window(cmd: &Command) -> WindowId {
    match cmd {
        Command::Eval(id, _) | Command::EvalResult(id, ..) => *id,
        _ => unreachable!("not a script command"),
    }
}

/// Run an `Eval`/`EvalResult` in a loaded page.
fn run_script(e: &Entry, cmd: Command, emit: &Emit) {
    match cmd {
        Command::Eval(id, js) => {
            if let Err(err) = e.webview.evaluate_script(&js) {
                emit(HostEvent::Error { window: Some(id), message: err.to_string() });
            }
        }
        Command::EvalResult(_, req, js) => {
            let reply = emit.clone();
            // Wrapped so a thrown error comes back as a rejected reply.
            let script = format!(
                "(() => {{ try {{ return {{ ok: true, value: ({js}) }}; }} catch (e) {{ return {{ ok: false, value: String(e && e.message || e) }}; }} }})()"
            );
            let result = e.webview.evaluate_script_with_callback(&script, move |json| {
                let v: serde_json::Value = serde_json::from_str(&json).unwrap_or(serde_json::Value::Null);
                let ok = v["ok"].as_bool().unwrap_or(false);
                reply(HostEvent::Reply { req, ok, value: v["value"].clone() });
            });
            if let Err(err) = result {
                emit(HostEvent::Reply { req, ok: false, value: err.to_string().into() });
            }
        }
        _ => {}
    }
}

/// An `EvalResult` for a window that is gone still gets its answer.
fn run_script_closed(cmd: Command, emit: &Emit) {
    if let Command::EvalResult(_, req, _) = cmd {
        emit(HostEvent::Reply { req, ok: false, value: "window is closed".into() });
    }
}

fn create(
    target: &EventLoopWindowTarget<Command>,
    proxy: &EventLoopProxy<Command>,
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

    let load_proxy = proxy.clone();
    builder = builder.with_on_page_load_handler(move |ev, _url| {
        let _ = load_proxy.send_event(Command::PageLoad(id, matches!(ev, wry::PageLoadEvent::Finished)));
    });
    let ipc_emit = emit.clone();
    // Inline HTML comes from the host itself, so it is trusted like app://.
    let trust_inline = opts.url.is_none() && opts.html.is_some();
    let policy = opts.ipc.clone();
    let assets = assets.clone();
    // macOS: wry drops IPC from pages whose URL has no host (about:blank,
    // data:), which is what inline HTML gets. Serve it from app:// instead.
    #[cfg(target_os = "macos")]
    let inline: Option<Arc<str>> = if trust_inline { opts.html.as_deref().map(Arc::from) } else { None };
    #[cfg(not(target_os = "macos"))]
    let inline: Option<Arc<str>> = None;
    let inline_page = inline.clone();
    builder = builder
        .with_initialization_script(BRIDGE_JS)
        .with_custom_protocol(ASSET_SCHEME.into(), move |_, req| match &inline_page {
            Some(html) if req.uri().path() == INLINE_PATH => Response::builder()
                .header(CONTENT_TYPE, mime_for("index.html"))
                .body(Cow::Owned(html.as_bytes().to_vec()))
                .unwrap(),
            _ => serve_asset(&*assets, &req),
        })
        .with_ipc_handler(move |req: Request<String>| {
            let origin = req.uri().to_string();
            match ipc_access(req.uri(), trust_inline, &policy) {
                Access::Local => on_ipc(id, req.body(), &origin, false, None, &ipc_emit),
                Access::Remote => on_ipc(id, req.body(), &origin, true, policy.remote_commands.as_deref(), &ipc_emit),
                Access::Denied => ipc_emit(HostEvent::Error { window: Some(id), message: format!("ipc blocked from {origin}") }),
            }
        });

    // Links to other sites: open them in the browser, block them, or allow
    // them (ExternalLinks). Our own pages, data:/about:/blob: and the
    // window's start origin always navigate normally.
    {
        let start_origin = opts.url.as_deref().map(resolve_url).and_then(|u| origin_of(&u));
        let nav_policy = opts.external_links;
        let nav_emit = emit.clone();
        let nav_start = start_origin.clone();
        builder = builder.with_navigation_handler(move |url: String| {
            if nav_policy == ExternalLinks::Allow || is_own_url(&url, nav_start.as_deref()) {
                return true;
            }
            external_link(&url, nav_policy, id, &nav_emit);
            false
        });
        let new_emit = emit.clone();
        builder = builder.with_new_window_req_handler(move |url: String, _features| {
            if nav_policy == ExternalLinks::Allow {
                return wry::NewWindowResponse::Allow;
            }
            external_link(&url, nav_policy, id, &new_emit);
            wry::NewWindowResponse::Deny
        });
    }

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
        (None, Some(_)) if inline.is_some() => builder.with_url(format!("{}{}", ASSET_ROOT, INLINE_PATH.trim_start_matches('/'))),
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
    let mut entry = Entry {
        label: opts.label.clone(),
        menu: None,
        popup: None,
        prevent_close: opts.prevent_close,
        loading: true,
        queued: Vec::new(),
        webview,
        window,
        _context: context,
    };
    if let Some(b64) = &opts.icon {
        let (rgba, w, h) = decode_png(&decode_base64(b64)?)?;
        let icon = tao::window::Icon::from_rgba(rgba, w, h).map_err(|e| format!("bad icon: {e}"))?;
        entry.window.set_window_icon(Some(icon));
    }
    if opts.menu.is_some() {
        set_menu_bar(&mut entry, id, opts.menu.clone())?;
    }
    Ok(entry)
}

/// Attach (or with `None`, remove) the window's menu bar.
fn set_menu_bar(e: &mut Entry, id: WindowId, specs: Option<Vec<MenuItemSpec>>) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use tao::platform::windows::WindowExtWindows;
        let hwnd = e.window.hwnd();
        if let Some(old) = e.menu.take() {
            // SAFETY: hwnd is this live window's handle.
            let _ = unsafe { old.remove_for_hwnd(hwnd) };
        }
        let Some(specs) = specs else { return Ok(()) };
        let menu = native::build_menu(Owner::Window(id), &specs)?;
        // SAFETY: as above.
        unsafe { menu.init_for_hwnd_with_theme(hwnd, muda::MenuTheme::Auto) }.map_err(|e| e.to_string())?;
        e.menu = Some(menu);
        Ok(())
    }
    // macOS has one menu bar for the whole app: the window that set its
    // menu last owns it.
    #[cfg(target_os = "macos")]
    {
        if let Some(old) = e.menu.take() {
            old.remove_for_nsapp();
        }
        let Some(specs) = specs else { return Ok(()) };
        let menu = native::build_menu(Owner::Window(id), &specs)?;
        // macOS shows the first submenu as the app menu (titled with the app's
        // name): put ours first, so the window's own menus keep their titles.
        menu.prepend(&native::app_submenu(&native::app_name())?).map_err(|e| e.to_string())?;
        menu.init_for_nsapp();
        e.menu = Some(menu);
        Ok(())
    }
    // Linux/BSD: a GTK menu bar packed into tao's default vbox, above the webview.
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use tao::platform::unix::WindowExtUnix;
        let gtk_window = e.window.gtk_window();
        if let Some(old) = e.menu.take() {
            let _ = old.remove_for_gtk_window(gtk_window);
        }
        let Some(specs) = specs else { return Ok(()) };
        let menu = native::build_menu(Owner::Window(id), &specs)?;
        menu.init_for_gtk_window(gtk_window, e.window.default_vbox()).map_err(|e| e.to_string())?;
        e.menu = Some(menu);
        Ok(())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        let _ = (e, id, specs);
        Err("menu bars are not supported on this platform".into())
    }
}

fn popup_menu(e: &mut Entry, id: WindowId, specs: &[MenuItemSpec], at: Option<(f64, f64)>) -> Result<(), String> {
    let menu = native::build_menu(Owner::Window(id), specs)?;
    #[cfg(target_os = "windows")]
    {
        use muda::ContextMenu as _;
        use tao::platform::windows::WindowExtWindows;
        let pos = at.map(|(x, y)| muda::dpi::Position::Logical(muda::dpi::LogicalPosition::new(x, y)));
        // SAFETY: hwnd is this live window's handle.
        unsafe { menu.show_context_menu_for_hwnd(e.window.hwnd(), pos) };
        // Keep it alive until the next popup, so its click event is delivered.
        e.popup = Some(menu);
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        use muda::ContextMenu as _;
        use tao::platform::macos::WindowExtMacOS;
        let pos = at.map(|(x, y)| muda::dpi::Position::Logical(muda::dpi::LogicalPosition::new(x, y)));
        // SAFETY: ns_view is this live window's content view.
        unsafe { menu.show_context_menu_for_nsview(e.window.ns_view() as _, pos) };
        e.popup = Some(menu);
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use gtk::prelude::Cast as _;
        use muda::ContextMenu as _;
        use tao::platform::unix::WindowExtUnix;
        let pos = at.map(|(x, y)| muda::dpi::Position::Logical(muda::dpi::LogicalPosition::new(x, y)));
        menu.show_context_menu_for_gtk_window(e.window.gtk_window().upcast_ref(), pos);
        e.popup = Some(menu);
        Ok(())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
    {
        let _ = (e, at, menu);
        Err("context menus are not supported on this platform".into())
    }
}

fn tray_icon_from(rgba: Rgba) -> Result<tray_icon::Icon, String> {
    tray_icon::Icon::from_rgba(rgba.0, rgba.1, rgba.2).map_err(|e| format!("bad tray icon: {e}"))
}

fn create_tray(id: TrayId, spec: &TraySpec, icon: Option<Rgba>) -> Result<tray_icon::TrayIcon, String> {
    let mut b = tray_icon::TrayIconBuilder::new()
        .with_id(id.to_string())
        .with_menu_on_left_click(spec.menu_on_left_click);
    if let Some(icon) = icon {
        b = b.with_icon(tray_icon_from(icon)?);
    }
    if let Some(t) = &spec.tooltip {
        b = b.with_tooltip(t);
    }
    if let Some(t) = &spec.title {
        b = b.with_title(t);
    }
    if let Some(items) = &spec.menu {
        b = b.with_menu(Box::new(native::build_menu(Owner::Tray(id), items)?));
    }
    let tray = b.build().map_err(|e| e.to_string())?;
    if spec.visible == Some(false) {
        tray.set_visible(false).map_err(|e| e.to_string())?;
    }
    Ok(tray)
}

fn update_tray(t: &tray_icon::TrayIcon, id: TrayId, spec: &TraySpec, icon: Option<Rgba>) -> Result<(), String> {
    if let Some(icon) = icon {
        t.set_icon(Some(tray_icon_from(icon)?)).map_err(|e| e.to_string())?;
    }
    if let Some(tip) = &spec.tooltip {
        t.set_tooltip(Some(tip)).map_err(|e| e.to_string())?;
    }
    if let Some(title) = &spec.title {
        t.set_title(Some(title));
    }
    if let Some(items) = &spec.menu {
        t.set_menu(Some(Box::new(native::build_menu(Owner::Tray(id), items)?)));
    }
    if let Some(v) = spec.visible {
        t.set_visible(v).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `scheme://host[:port]` of a URL, lowercased.
fn origin_of(url: &str) -> Option<String> {
    let uri: wry::http::Uri = url.parse().ok()?;
    let mut o = format!("{}://{}", uri.scheme_str()?, uri.host()?);
    if let Some(port) = uri.port_u16() {
        o.push_str(&format!(":{port}"));
    }
    Some(o.to_ascii_lowercase())
}

/// Navigation that stays inside the app: our asset protocol (also as
/// `http(s)://app.localhost`), data:/about:/blob:, or the window's start origin.
fn is_own_url(url: &str, start_origin: Option<&str>) -> bool {
    let lower = url.to_ascii_lowercase();
    if ["data:", "about:", "blob:", "javascript:"].iter().any(|p| lower.starts_with(p)) {
        return true;
    }
    let Some(origin) = origin_of(url) else { return false };
    origin == format!("{ASSET_SCHEME}://localhost")
        || origin == format!("http://{ASSET_SCHEME}.localhost")
        || origin == format!("https://{ASSET_SCHEME}.localhost")
        || start_origin == Some(origin.as_str())
}

/// A link to another site was followed: open it in the browser (only http/https/mailto)
/// or drop it, and tell the host.
fn external_link(url: &str, policy: ExternalLinks, window: WindowId, emit: &Emit) {
    let lower = url.to_ascii_lowercase();
    let openable = ["http://", "https://", "mailto:"].iter().any(|p| lower.starts_with(p));
    let action = if policy == ExternalLinks::Browser && openable {
        match open::that_detached(url) {
            Ok(()) => "opened",
            Err(_) => "failed",
        }
    } else {
        "blocked"
    };
    emit(HostEvent::Window { window, kind: "externallink", data: serde_json::json!({ "url": url, "action": action }) });
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

    #[test]
    fn event_json() {
        let ev = HostEvent::Invoke { window: 2, call: 7, cmd: "greet".into(), args: r#"{"name":"x"}"#.into(), origin: "http://app.localhost/".into(), remote: false };
        let v: serde_json::Value = serde_json::from_str(&ev.to_json()).unwrap();
        assert_eq!(v["type"], "invoke");
        assert_eq!(v["args"]["name"], "x");
        assert_eq!(ev.window(), Some(2));
    }

    #[test]
    fn own_urls() {
        assert!(is_own_url("app://localhost/index.html", None));
        assert!(is_own_url("http://app.localhost/x?y", None));
        assert!(is_own_url("about:blank", None));
        assert!(is_own_url("data:text/html,hi", None));
        assert!(!is_own_url("https://example.com/", None));
        assert!(is_own_url("https://example.com/page", Some("https://example.com")));
        assert!(!is_own_url("https://evil.com/", Some("https://example.com")));
        assert!(!is_own_url("http://app.localhost.evil.com/", None));
        assert_eq!(origin_of("HTTPS://Example.com:8443/a").as_deref(), Some("https://example.com:8443"));
    }

    #[test]
    fn events_round_trip() {
        let events = [
            HostEvent::Created { window: 1 },
            HostEvent::Invoke { window: 2, call: 7, cmd: "greet".into(), args: r#"{"name":"x"}"#.into(), origin: "app://localhost/".into(), remote: true },
            HostEvent::DragDrop { window: 3, kind: "drop", paths: vec!["/tmp/a b".into()], x: -4, y: 5 },
            HostEvent::Closed { window: 4 },
            HostEvent::Window { window: 5, kind: "resized", data: serde_json::json!({ "width": 800.0, "height": 600.0 }) },
            HostEvent::CreateFailed { window: 6, message: "nope".into() },
            HostEvent::Error { window: None, message: "e".into() },
            HostEvent::Warning { window: Some(7), message: "w".into() },
            HostEvent::Menu { window: None, tray: Some(8), id: "quit".into() },
            HostEvent::Tray { tray: 9, kind: "doubleclick", button: "right", x: 1.5, y: 2.5 },
            HostEvent::TrayFailed { tray: 10, message: "t".into() },
            HostEvent::App { kind: "reopen", data: serde_json::json!({ "hasVisibleWindows": false }) },
            HostEvent::App { kind: "quitrequested", data: serde_json::Value::Null },
            HostEvent::Reply { req: 11, ok: true, value: serde_json::json!(["C:/a.txt", "C:/b.txt"]) },
            HostEvent::Reply { req: 12, ok: false, value: serde_json::json!("cancelled") },
            HostEvent::Shortcut { accelerator: "CmdOrCtrl+Shift+B".into(), state: "pressed" },
            HostEvent::Exited,
        ];
        for ev in events {
            let json = ev.to_json();
            assert_eq!(HostEvent::from_json(&json).unwrap().to_json(), json);
        }
        assert!(HostEvent::from_json(r#"{"type":"bogus"}"#).is_err());
    }

    #[test]
    fn commands_round_trip() {
        let opts = WindowOptions::from_json(r##"{"label":"x","width":300,"backgroundColor":"#123","menu":[{"id":"a","text":"A"}],"someFutureOption":1}"##).unwrap();
        let cmds = [
            Command::Create(1, opts),
            Command::SetIcon(1, (vec![1, 2, 3, 4], 1, 1)),
            Command::Op(1, WindowOp::from_json(r#"{"op":"setSize","width":10,"height":20}"#).unwrap()),
            Command::TrayCreate(2, TraySpec::default(), Some((vec![9; 8], 2, 1))),
            Command::TrayUpdate(2, TraySpec::default(), None),
            Command::Dialog(3, Some(1), DialogSpec::from_json(r#"{"kind":"open","multiple":true,"filters":[{"name":"T","extensions":["txt"]}]}"#).unwrap()),
            Command::Notify(4, NotificationSpec::from_json(r#"{"title":"t","body":"b"}"#).unwrap()),
            Command::Shortcut(5, true, "CmdOrCtrl+Shift+B".into()),
            Command::EvalResult(1, 6, "document.title".into()),
            Command::Monitors(7),
            Command::Shutdown,
        ];
        for cmd in cmds {
            let json = serde_json::to_string(&cmd).unwrap();
            let back: Command = serde_json::from_str(&json).unwrap();
            assert_eq!(serde_json::to_string(&back).unwrap(), json);
        }
        let json = serde_json::to_string(&Command::Create(1, WindowOptions::from_json(r##"{"backgroundColor":"#123"}"##).unwrap())).unwrap();
        let Command::Create(_, o) = serde_json::from_str(&json).unwrap() else { panic!() };
        assert_eq!(o.background_color, Some(Color(0x11, 0x22, 0x33, 255)));
    }

    #[test]
    fn dir_assets_stay_inside() {
        let dir = std::env::temp_dir().join("buntauri-assets-test");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("a.txt"), b"hi").unwrap();
        let a = DirAssets(dir);
        assert_eq!(&*a.get("sub/a.txt").unwrap().bytes, b"hi");
        assert!(a.get("../secret.txt").is_none());
        assert!(a.get("sub/../../x").is_none());
        assert!(a.get("C:/Windows/win.ini").is_none());
    }
}
