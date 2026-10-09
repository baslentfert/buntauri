//! `bun:buntauri` native side. Thin glue between the builtin JS module
//! (`src/js/bun/buntauri.ts`) and the `buntauri_window` crate, which owns the
//! UI thread (tao + wry). Everything here runs on the JS thread except
//! `post`, which the UI thread calls to hand an event over.
//!
//! Lives in the buntauri repo (bun-glue/) and is copied into the Bun tree.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Arc;

use bun_event_loop::ConcurrentTask::ConcurrentTask;
use bun_event_loop::{ContextId, Task, TaskTag, Taskable, task_tag};
use bun_jsc::bun_string_jsc::create_utf8_for_js;
use bun_jsc::{CallFrame, JSGlobalObject, JSValue, JsResult, Strong};
use bun_standalone_graph::{BASE_PUBLIC_PATH, Graph, is_bun_standalone_file_path};
use buntauri_window::{Asset, AssetProvider, DirAssets, HostEvent, NoAssets, UiThread, WindowId, mime_for};

struct State {
    ui: UiThread,
    callback: Strong,
    /// Windows and trays that keep the event loop alive (they share one id space).
    alive: HashSet<u32>,
}

// JS-thread only (host functions and `deliver`).
static STATE: bun_core::RacyCell<Option<State>> = bun_core::RacyCell::new(None);

fn state<'a>() -> Option<&'a mut State> {
    // SAFETY: only touched on the JS thread; no borrow outlives one host call.
    unsafe { (*STATE.get()).as_mut() }
}

fn ui<'a>(global: &JSGlobalObject) -> JsResult<&'a UiThread> {
    match state() {
        Some(s) => Ok(&s.ui),
        None => Err(global.throw(format_args!("bun:buntauri is not initialized"))),
    }
}

/// Serves `app://` straight from the executable (`bun build --compile`):
/// `base` is a bunfs directory such as `B:/~BUN/root/assets`. Zero-copy: the
/// graph is read-only and its bytes live in the executable's section.
struct GraphAssets {
    base: String,
}

impl AssetProvider for GraphAssets {
    fn get(&self, path: &str) -> Option<Asset> {
        if path.split(['/', '\\']).any(|seg| seg == ".." || seg.contains(':')) {
            return None;
        }
        let graph = Graph::get_ref()?;
        // The assets dir first, then the graph root: `--compile` puts the
        // bundled chunks of an HTML entry (index-<hash>.js) at the root.
        let file = graph
            .find_ref(format!("{}/{}", self.base.trim_end_matches(['/', '\\']), path).as_bytes())
            .or_else(|| graph.find_ref(format!("{BASE_PUBLIC_PATH}root/{path}").as_bytes()))?;
        let mime = mime_for(path);
        Some(Asset { bytes: rewrite_bunfs_urls(file.utf8_contents(), mime), mime: Cow::Borrowed(mime) })
    }
}

/// `--compile` points bundled HTML at its chunks with the bunfs path
/// (`src="B:/~BUN/root/index-x.js"`), which means nothing to a browser. In
/// HTML/CSS/JS, turn that prefix into `/`, the root of `app://`. Untouched
/// files stay zero-copy.
fn rewrite_bunfs_urls(bytes: &'static [u8], mime: &str) -> Cow<'static, [u8]> {
    const PREFIXES: [&str; 2] = ["B:/~BUN/root/", "/$bunfs/root/"];
    let text = mime.starts_with("text/html") || mime.starts_with("text/css") || mime.starts_with("text/javascript");
    let Ok(s) = std::str::from_utf8(bytes) else { return Cow::Borrowed(bytes) };
    if !text || !PREFIXES.iter().any(|p| s.contains(p)) {
        return Cow::Borrowed(bytes);
    }
    let mut out = s.to_string();
    for p in PREFIXES {
        out = out.replace(p, "/");
    }
    Cow::Owned(out.into_bytes())
}

/// `app://` source for `assetsDir`: inside the executable when it is a bunfs
/// path of a compiled app, otherwise the directory on disk.
fn asset_provider(dir: Option<String>) -> Arc<dyn AssetProvider> {
    match dir {
        Some(dir) if is_bun_standalone_file_path(dir.as_bytes()) && Graph::get_ref().is_some() => {
            Arc::new(GraphAssets { base: dir.replace('\\', "/") })
        }
        Some(dir) => Arc::new(DirAssets(dir.into())),
        None => Arc::new(NoAssets),
    }
}

/// An event from the UI thread, queued onto the JS thread.
pub(crate) struct WindowEvent {
    owner: Option<u32>,
    /// The window/tray is gone (closed, or never created): release its keep-alive.
    ends_owner: bool,
    json: String,
}

impl Taskable for WindowEvent {
    const TAG: TaskTag = task_tag::BuntauriWindowEvent;
    unsafe fn release_unrun(this: *mut Self) {
        // SAFETY: boxed in `post`.
        drop(unsafe { bun_core::heap::take(this) });
    }
    unsafe fn context(_: *const Self) -> ContextId {
        ContextId::NONE
    }
}

impl WindowEvent {
    #[allow(clippy::boxed_local, reason = "reclaim point for the boxed task")]
    pub(crate) fn deliver(self: Box<Self>, global: &JSGlobalObject) -> JsResult<()> {
        let Some(s) = state() else { return Ok(()) };
        if self.ends_owner {
            if let Some(id) = self.owner {
                release(global, s, id);
            }
        }
        let arg = create_utf8_for_js(global, self.json.as_bytes())?;
        global
            .bun_vm()
            .event_loop_mut()
            .run_callback(ContextId::NONE, s.callback.get(), global, JSValue::UNDEFINED, &[arg]);
        Ok(())
    }
}

/// UI thread: hand an event to the JS thread.
fn post(vm: &bun_jsc::VmHandle, ev: HostEvent) {
    let event = WindowEvent { owner: ev.owner(), ends_owner: ev.ends_owner(), json: ev.to_json() };
    let payload = bun_core::heap::into_raw(Box::new(event));
    let task = ConcurrentTask::create(Task::init(payload));
    if let bun_jsc::vm_handle::Posted::Refused(task) = vm.post(bun_jsc::LoopKind::Regular, task) {
        // VM is gone: nobody will run it, so free the node and the event.
        // SAFETY: refused ⇒ never queued; both were boxed just above.
        unsafe {
            drop(bun_core::heap::take(task.as_ptr()));
            drop(bun_core::heap::take(payload));
        }
    }
}

fn hold(global: &JSGlobalObject, s: &mut State, id: u32) {
    if s.alive.insert(id) {
        global.bun_vm().event_loop_mut().ref_keep_alive();
    }
}

fn release(global: &JSGlobalObject, s: &mut State, id: u32) {
    if s.alive.remove(&id) {
        global.bun_vm().event_loop_mut().unref_keep_alive();
    }
}

fn string_arg(global: &JSGlobalObject, v: JSValue, name: &str) -> JsResult<String> {
    if !v.is_string() {
        return Err(global.throw_type_error(format_args!("{name} must be a string")));
    }
    let utf8 = v.to_utf8(global)?;
    Ok(String::from_utf8_lossy(&utf8).into_owned())
}

fn id_arg(global: &JSGlobalObject, v: JSValue) -> JsResult<WindowId> {
    if !v.is_number() {
        return Err(global.throw_type_error(format_args!("window id must be a number")));
    }
    Ok(v.to_u32())
}

/// `init(callback, assetsDir?)`: start the UI thread. Every event arrives as
/// one JSON string argument to `callback`.
#[bun_jsc::host_fn]
pub(crate) fn init(global: &JSGlobalObject, frame: &CallFrame) -> JsResult<JSValue> {
    let [callback, assets] = frame.arguments_as_array::<2>();
    if !callback.is_callable() {
        return Err(global.throw_type_error(format_args!("callback must be a function")));
    }
    if let Some(s) = state() {
        s.callback = Strong::create(callback, global);
        return Ok(JSValue::UNDEFINED);
    }
    let dir = if assets.is_string() { Some(string_arg(global, assets, "assetsDir")?) } else { None };
    let provider = asset_provider(dir);
    let vm = global.bun_vm().handle();
    let ui = UiThread::spawn(move |ev| post(&vm, ev), provider)
        .map_err(|e| global.throw(format_args!("failed to start UI thread: {e}")))?;
    // SAFETY: JS thread; see `state`.
    unsafe {
        *STATE.get() = Some(State { ui, callback: Strong::create(callback, global), alive: HashSet::new() });
    }
    Ok(JSValue::UNDEFINED)
}

/// `createWindow(optionsJson) -> id`. Options use Tauri's `WindowConfig` names.
#[bun_jsc::host_fn]
pub(crate) fn create_window(global: &JSGlobalObject, frame: &CallFrame) -> JsResult<JSValue> {
    let json = string_arg(global, frame.argument(0), "options")?;
    let ui = ui(global)?;
    let id = ui
        .create_window_json(&json)
        .map_err(|e| global.throw_type_error(format_args!("{e}")))?;
    if let Some(s) = state() {
        hold(global, s, id);
    }
    Ok(JSValue::js_number(id as f64))
}

/// `setIcon(id, pngBase64)`
#[bun_jsc::host_fn]
pub(crate) fn set_icon(global: &JSGlobalObject, frame: &CallFrame) -> JsResult<JSValue> {
    let [id, png] = frame.arguments_as_array::<2>();
    let png = string_arg(global, png, "icon")?;
    ui(global)?
        .set_icon_base64(id_arg(global, id)?, &png)
        .map_err(|e| global.throw_type_error(format_args!("{e}")))?;
    Ok(JSValue::UNDEFINED)
}

/// `setMenu(id, menuJson)`; `"null"` removes the menu bar.
#[bun_jsc::host_fn]
pub(crate) fn set_menu(global: &JSGlobalObject, frame: &CallFrame) -> JsResult<JSValue> {
    let [id, json] = frame.arguments_as_array::<2>();
    let json = string_arg(global, json, "menu")?;
    ui(global)?
        .set_menu_json(id_arg(global, id)?, &json)
        .map_err(|e| global.throw_type_error(format_args!("{e}")))?;
    Ok(JSValue::UNDEFINED)
}

/// `popupMenu(id, menuJson, x?, y?)`: at a logical position, or at the cursor.
#[bun_jsc::host_fn]
pub(crate) fn popup_menu(global: &JSGlobalObject, frame: &CallFrame) -> JsResult<JSValue> {
    let [id, json, x, y] = frame.arguments_as_array::<4>();
    let json = string_arg(global, json, "menu")?;
    let at = if x.is_number() && y.is_number() { Some((x.as_number(), y.as_number())) } else { None };
    ui(global)?
        .popup_menu_json(id_arg(global, id)?, &json, at)
        .map_err(|e| global.throw_type_error(format_args!("{e}")))?;
    Ok(JSValue::UNDEFINED)
}

/// `trayCreate(trayJson) -> id`. A tray keeps the process alive until removed.
#[bun_jsc::host_fn]
pub(crate) fn tray_create(global: &JSGlobalObject, frame: &CallFrame) -> JsResult<JSValue> {
    let json = string_arg(global, frame.argument(0), "tray")?;
    let id = ui(global)?
        .create_tray_json(&json)
        .map_err(|e| global.throw_type_error(format_args!("{e}")))?;
    if let Some(s) = state() {
        hold(global, s, id);
    }
    Ok(JSValue::js_number(id as f64))
}

/// `trayUpdate(id, trayJson)`: only the given fields change.
#[bun_jsc::host_fn]
pub(crate) fn tray_update(global: &JSGlobalObject, frame: &CallFrame) -> JsResult<JSValue> {
    let [id, json] = frame.arguments_as_array::<2>();
    let json = string_arg(global, json, "tray")?;
    ui(global)?
        .update_tray_json(id_arg(global, id)?, &json)
        .map_err(|e| global.throw_type_error(format_args!("{e}")))?;
    Ok(JSValue::UNDEFINED)
}

/// `trayRemove(id)`
#[bun_jsc::host_fn]
pub(crate) fn tray_remove(global: &JSGlobalObject, frame: &CallFrame) -> JsResult<JSValue> {
    let id = id_arg(global, frame.argument(0))?;
    ui(global)?.remove_tray(id);
    if let Some(s) = state() {
        release(global, s, id);
    }
    Ok(JSValue::UNDEFINED)
}

/// `evalScript(id, js)`
#[bun_jsc::host_fn]
pub(crate) fn eval_script(global: &JSGlobalObject, frame: &CallFrame) -> JsResult<JSValue> {
    let [id, js] = frame.arguments_as_array::<2>();
    ui(global)?.eval(id_arg(global, id)?, string_arg(global, js, "script")?);
    Ok(JSValue::UNDEFINED)
}

/// `setTitle(id, title)`
#[bun_jsc::host_fn]
pub(crate) fn set_title(global: &JSGlobalObject, frame: &CallFrame) -> JsResult<JSValue> {
    let [id, title] = frame.arguments_as_array::<2>();
    ui(global)?.set_title(id_arg(global, id)?, string_arg(global, title, "title")?);
    Ok(JSValue::UNDEFINED)
}

/// `close(id)`
#[bun_jsc::host_fn]
pub(crate) fn close(global: &JSGlobalObject, frame: &CallFrame) -> JsResult<JSValue> {
    ui(global)?.close(id_arg(global, frame.argument(0))?);
    Ok(JSValue::UNDEFINED)
}

/// `resolve(id, call, json)`: answer an `invoke` from the page.
#[bun_jsc::host_fn]
pub(crate) fn resolve(global: &JSGlobalObject, frame: &CallFrame) -> JsResult<JSValue> {
    let [id, call, json] = frame.arguments_as_array::<3>();
    let json = string_arg(global, json, "result")?;
    ui(global)?.resolve(id_arg(global, id)?, call.as_number() as u64, &json);
    Ok(JSValue::UNDEFINED)
}

/// `reject(id, call, message)`
#[bun_jsc::host_fn]
pub(crate) fn reject(global: &JSGlobalObject, frame: &CallFrame) -> JsResult<JSValue> {
    let [id, call, message] = frame.arguments_as_array::<3>();
    let message = string_arg(global, message, "message")?;
    ui(global)?.reject(id_arg(global, id)?, call.as_number() as u64, &message);
    Ok(JSValue::UNDEFINED)
}

/// `emit(id, name, json)`: fire an event in the page.
#[bun_jsc::host_fn]
pub(crate) fn emit(global: &JSGlobalObject, frame: &CallFrame) -> JsResult<JSValue> {
    let [id, name, json] = frame.arguments_as_array::<3>();
    let name = string_arg(global, name, "event name")?;
    let json = string_arg(global, json, "payload")?;
    ui(global)?.emit(id_arg(global, id)?, &name, &json);
    Ok(JSValue::UNDEFINED)
}
