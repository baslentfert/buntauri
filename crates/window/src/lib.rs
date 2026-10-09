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
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::Window;
use wry::http::{header::CONTENT_TYPE, Request, Response};
use wry::{DragDropEvent, WebContext, WebView, WebViewBuilder};

mod control;
mod extras;
mod native;
mod options;
pub use control::WindowOp;
pub use extras::{clipboard_read_text, clipboard_write_text, DialogSpec, NotificationSpec};
pub use native::{decode_base64, decode_png, MenuItemSpec, Owner, TraySpec};
pub use options::{BackgroundThrottling, Color, IpcPolicy, PreventOverflow, ScrollBarStyle, Theme, WindowOptions};

/// Id of a tray icon. Shares the id space with windows.
pub type TrayId = u32;

/// Straight RGBA8 pixels: (pixels, width, height).
type Rgba = (Vec<u8>, u32, u32);

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
    /// The UI thread's event loop has stopped.
    Exited,
}

impl HostEvent {
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
            HostEvent::Shortcut { accelerator, state } => {
                json!({ "type": "shortcut", "accelerator": accelerator, "state": state })
            }
            HostEvent::Exited => json!({ "type": "exited" }),
        };
        v.to_string()
    }
}

/// Serves files from a directory on disk. Paths that try to leave the
/// directory (`..`, absolute, drive letters) are refused.
pub struct DirAssets(pub std::path::PathBuf);

impl AssetProvider for DirAssets {
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
}

enum Command {
    Create(WindowId, WindowOptions),
    Eval(WindowId, String),
    SetTitle(WindowId, String),
    Close(WindowId),
    SetIcon(WindowId, Rgba),
    Op(WindowId, WindowOp),
    SetMenu(WindowId, Option<Vec<MenuItemSpec>>),
    Popup(WindowId, Vec<MenuItemSpec>, Option<(f64, f64)>),
    TrayCreate(TrayId, TraySpec, Option<Rgba>),
    TrayUpdate(TrayId, TraySpec, Option<Rgba>),
    TrayRemove(TrayId),
    Dialog(u64, Option<WindowId>, DialogSpec),
    Notify(u64, NotificationSpec),
    Shortcut(u64, bool, String),
    Shutdown,
}

/// Handle to the UI thread. Cheap to use from any thread.
pub struct UiThread {
    proxy: EventLoopProxy<Command>,
    states: States,
    next_id: AtomicU32,
    next_req: AtomicU64,
    join: Option<JoinHandle<()>>,
}

impl UiThread {
    pub fn spawn<F>(on_event: F, assets: Arc<dyn AssetProvider>) -> std::io::Result<Self>
    where
        F: Fn(HostEvent) + Send + Sync + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let states: States = Default::default();
        let ui_states = states.clone();
        let join = std::thread::Builder::new()
            .name("buntauri-ui".into())
            .spawn(move || ui_main(tx, Arc::new(on_event), assets, ui_states))?;
        let proxy = rx
            .recv()
            .map_err(|_| std::io::Error::other("UI thread failed to start"))?;
        Ok(Self { proxy, states, next_id: AtomicU32::new(1), next_req: AtomicU64::new(1), join: Some(join) })
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
type States = Arc<Mutex<HashMap<WindowId, serde_json::Value>>>;

fn parse_tray(json: &str) -> Result<(TraySpec, Option<Rgba>), String> {
    let spec: TraySpec = serde_json::from_str(json).map_err(|e| format!("invalid tray: {e}"))?;
    let icon = match &spec.icon {
        Some(b64) => Some(decode_png(&decode_base64(b64)?)?),
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
    // Field order matters: the webview drops before its window, and both
    // before the web context that holds the data directory.
    webview: WebView,
    window: Window,
    _context: Option<Box<WebContext>>,
}

fn ui_main(ready: mpsc::Sender<EventLoopProxy<Command>>, emit: Emit, assets: Arc<dyn AssetProvider>, states: States) {
    let snap = |id: WindowId, e: &Entry| {
        states.lock().unwrap().insert(id, control::snapshot(&e.window, e.prevent_close));
    };
    let forget = |id: WindowId| {
        states.lock().unwrap().remove(&id);
    };
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
    // macOS: AppKit requires the process main thread, so this panics there.
    // buntauri will use a host subprocess on macOS (see BUNTAURI.md).
    let mut event_loop = builder.build();
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

    event_loop.run_return(|event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(cmd) => match cmd {
                Command::Create(id, opts) => match create(target, id, opts, &windows, &emit, &assets) {
                    Ok(entry) => {
                        by_native.insert(entry.window.id(), id);
                        snap(id, &entry);
                        windows.insert(id, entry);
                        emit(HostEvent::Created { window: id });
                    }
                    Err(e) => emit(HostEvent::CreateFailed { window: id, message: e }),
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
                        forget(id);
                        #[cfg(target_os = "windows")]
                        accels.borrow_mut().remove(&id);
                        emit(HostEvent::Closed { window: id });
                    }
                }
                Command::Op(id, op) => {
                    if let Some(e) = windows.get_mut(&id) {
                        if let Err(message) = control::apply(&e.window, &op, &mut e.prevent_close) {
                            emit(HostEvent::Error { window: Some(id), message });
                        }
                        snap(id, e);
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
                snap(id, e);
                emit(HostEvent::Window { window: id, kind, data });
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
    let mut entry = Entry {
        label: opts.label.clone(),
        menu: None,
        popup: None,
        prevent_close: opts.prevent_close,
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
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (e, id, specs);
        Err("menu bars are only supported on Windows so far".into())
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
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (e, at, menu);
        Err("context menus are only supported on Windows so far".into())
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
