//! `bun:buntauri` native side. Thin glue between the builtin JS module
//! (`src/js/bun/buntauri.ts`) and the `buntauri_window` crate, which owns the
//! UI thread (tao + wry). Everything here runs on the JS thread except
//! `post`, which the UI thread calls to hand an event over.
//!
//! Lives in the buntauri repo (bun-glue/) and is copied into the Bun tree.

use std::collections::HashSet;
use std::sync::Arc;

use bun_event_loop::ConcurrentTask::ConcurrentTask;
use bun_event_loop::{ContextId, Task, TaskTag, Taskable, task_tag};
use bun_jsc::bun_string_jsc::create_utf8_for_js;
use bun_jsc::{CallFrame, JSGlobalObject, JSValue, JsResult, Strong};
use buntauri_window::{AssetProvider, DirAssets, HostEvent, NoAssets, UiThread, WindowId};

struct State {
    ui: UiThread,
    callback: Strong,
    /// Windows that keep the event loop alive (created, not yet closed).
    alive: HashSet<WindowId>,
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

/// An event from the UI thread, queued onto the JS thread.
pub(crate) struct WindowEvent {
    window: Option<WindowId>,
    /// The window is gone (closed, or never created): release its keep-alive.
    ends_window: bool,
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
        if self.ends_window {
            if let Some(id) = self.window {
                if s.alive.remove(&id) {
                    global.bun_vm().event_loop_mut().unref_keep_alive();
                }
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
    let event = WindowEvent {
        window: ev.window(),
        ends_window: matches!(ev, HostEvent::Closed { .. } | HostEvent::CreateFailed { .. }),
        json: ev.to_json(),
    };
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
    let provider: Arc<dyn AssetProvider> = if assets.is_string() {
        Arc::new(DirAssets(string_arg(global, assets, "assetsDir")?.into()))
    } else {
        Arc::new(NoAssets)
    };
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
        if s.alive.insert(id) {
            global.bun_vm().event_loop_mut().ref_keep_alive();
        }
    }
    Ok(JSValue::js_number(id as f64))
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
