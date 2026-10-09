//! Window control at run time: operations the host sends as JSON
//! (`{"op": "hide"}`, `{"op": "setSize", "width": 800, "height": 600}`), and
//! a snapshot of the window's state the host can read without a round trip.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::window::{Fullscreen, Window};

/// A window operation. Sizes and positions are logical (CSS) pixels.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum WindowOp {
    Show,
    Hide,
    Minimize,
    Unminimize,
    Maximize,
    Unmaximize,
    ToggleMaximize,
    /// Bring to front and focus; restores a minimized window.
    Focus,
    Center,
    SetSize { width: f64, height: f64 },
    SetPosition { x: f64, y: f64 },
    SetMinSize { width: Option<f64>, height: Option<f64> },
    SetMaxSize { width: Option<f64>, height: Option<f64> },
    SetAlwaysOnTop { value: bool },
    SetFullscreen { value: bool },
    SetResizable { value: bool },
    SetDecorations { value: bool },
    /// When on, the close button emits `closerequested` instead of closing.
    SetPreventClose { value: bool },
    /// Start moving the window with the mouse (for a custom title bar; call on mouse down).
    StartDragging,
    /// Flash the taskbar button / bounce the dock icon.
    RequestAttention,
    /// Load a URL; relative paths load from `app://`.
    Navigate { url: String },
    Reload,
    OpenDevtools,
    CloseDevtools,
    /// Page zoom, 1.0 = 100%.
    SetZoom { factor: f64 },
    Print,
    /// Taskbar/dock progress: `progress` 0..1 (null = keep), `state`: none |
    /// normal | indeterminate | paused | error.
    SetProgress { progress: Option<f64>, state: Option<String> },
}

impl WindowOp {
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("invalid window operation: {e}"))
    }
}

/// Apply `op`. `prevent_close` is the window's close policy, owned by the caller.
pub(crate) fn apply(window: &Window, webview: &wry::WebView, op: &WindowOp, prevent_close: &mut bool) -> Result<(), String> {
    match *op {
        WindowOp::Show => window.set_visible(true),
        WindowOp::Hide => window.set_visible(false),
        WindowOp::Minimize => window.set_minimized(true),
        WindowOp::Unminimize => window.set_minimized(false),
        WindowOp::Maximize => window.set_maximized(true),
        WindowOp::Unmaximize => window.set_maximized(false),
        WindowOp::ToggleMaximize => window.set_maximized(!window.is_maximized()),
        WindowOp::Focus => {
            if window.is_minimized() {
                window.set_minimized(false);
            }
            window.set_visible(true);
            window.set_focus();
        }
        WindowOp::Center => {
            if let Some(m) = window.current_monitor() {
                let (pos, size, outer) = (m.position(), m.size(), window.outer_size());
                window.set_outer_position(tao::dpi::PhysicalPosition::new(
                    pos.x + (size.width as i32 - outer.width as i32) / 2,
                    pos.y + (size.height as i32 - outer.height as i32) / 2,
                ));
            }
        }
        WindowOp::SetSize { width, height } => window.set_inner_size(LogicalSize::new(width, height)),
        WindowOp::SetPosition { x, y } => window.set_outer_position(LogicalPosition::new(x, y)),
        WindowOp::SetMinSize { width, height } => {
            window.set_min_inner_size(width.zip(height).map(|(w, h)| LogicalSize::new(w, h)))
        }
        WindowOp::SetMaxSize { width, height } => {
            window.set_max_inner_size(width.zip(height).map(|(w, h)| LogicalSize::new(w, h)))
        }
        WindowOp::SetAlwaysOnTop { value } => window.set_always_on_top(value),
        WindowOp::SetFullscreen { value } => window.set_fullscreen(value.then_some(Fullscreen::Borderless(None))),
        WindowOp::SetResizable { value } => window.set_resizable(value),
        WindowOp::SetDecorations { value } => window.set_decorations(value),
        WindowOp::SetPreventClose { value } => *prevent_close = value,
        WindowOp::StartDragging => window.drag_window().map_err(|e| e.to_string())?,
        WindowOp::RequestAttention => window.request_user_attention(Some(tao::window::UserAttentionType::Informational)),
        WindowOp::Navigate { ref url } => webview.load_url(&crate::resolve_url(url)).map_err(|e| e.to_string())?,
        WindowOp::Reload => webview.reload().map_err(|e| e.to_string())?,
        WindowOp::OpenDevtools => webview.open_devtools(),
        WindowOp::CloseDevtools => webview.close_devtools(),
        WindowOp::SetZoom { factor } => webview.zoom(factor).map_err(|e| e.to_string())?,
        WindowOp::Print => webview.print().map_err(|e| e.to_string())?,
        WindowOp::SetProgress { progress, ref state } => {
            use tao::window::{ProgressBarState, ProgressState};
            let state = match state.as_deref() {
                None => None,
                Some("none") => Some(ProgressState::None),
                Some("normal") => Some(ProgressState::Normal),
                Some("indeterminate") => Some(ProgressState::Indeterminate),
                Some("paused") => Some(ProgressState::Paused),
                Some("error") => Some(ProgressState::Error),
                Some(other) => return Err(format!("unknown progress state {other:?}")),
            };
            let progress = progress.map(|p| (p.clamp(0.0, 1.0) * 100.0).round() as u64);
            // A value alone means "normal" progress.
            let state = state.or(progress.map(|_| ProgressState::Normal));
            window.set_progress_bar(ProgressBarState { state, progress, desktop_filename: None });
        }
    }
    Ok(())
}

/// What the host can read at any time (`win.state` in JS).
pub(crate) fn snapshot(window: &Window, webview: &wry::WebView, prevent_close: bool) -> Value {
    let scale = window.scale_factor();
    // macOS: tao's inner_size reads its own view, which keeps its first size
    // once wry has put the webview in; the webview itself shrinks when the
    // devtools are docked. The window's content rect is the real inner size.
    #[cfg(target_os = "macos")]
    let size = content_size(window).unwrap_or_else(|| window.inner_size().to_logical::<f64>(scale));
    #[cfg(not(target_os = "macos"))]
    let size = window.inner_size().to_logical::<f64>(scale);
    let pos = window.outer_position().map(|p| p.to_logical::<f64>(scale)).ok();
    json!({
        "visible": window.is_visible(),
        "minimized": window.is_minimized(),
        "maximized": window.is_maximized(),
        "focused": window.is_focused(),
        "fullscreen": window.fullscreen().is_some(),
        "alwaysOnTop": window.is_always_on_top(),
        "resizable": window.is_resizable(),
        "decorated": window.is_decorated(),
        "preventClose": prevent_close,
        "width": size.width,
        "height": size.height,
        "x": pos.map(|p| p.x),
        "y": pos.map(|p| p.y),
        "scaleFactor": scale,
        "url": webview.url().ok(),
        "devtoolsOpen": webview.is_devtools_open(),
    })
}

/// The window's content area in points (= logical pixels).
#[cfg(target_os = "macos")]
fn content_size(window: &Window) -> Option<LogicalSize<f64>> {
    use tao::platform::macos::WindowExtMacOS;
    // SAFETY: ns_window is this live window's NSWindow, used on the main thread.
    let ns = unsafe { (window.ns_window() as *const objc2_app_kit::NSWindow).as_ref() }?;
    let rect = ns.contentRectForFrameRect(ns.frame());
    Some(LogicalSize::new(rect.size.width, rect.size.height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_parse() {
        assert!(matches!(WindowOp::from_json(r#"{"op":"hide"}"#), Ok(WindowOp::Hide)));
        assert!(matches!(WindowOp::from_json(r#"{"op":"toggleMaximize"}"#), Ok(WindowOp::ToggleMaximize)));
        assert!(matches!(
            WindowOp::from_json(r#"{"op":"setSize","width":800,"height":600}"#),
            Ok(WindowOp::SetSize { width, height }) if width == 800.0 && height == 600.0
        ));
        assert!(matches!(WindowOp::from_json(r#"{"op":"setPreventClose","value":true}"#), Ok(WindowOp::SetPreventClose { value: true })));
        assert!(matches!(WindowOp::from_json(r#"{"op":"setMinSize","width":null,"height":null}"#), Ok(WindowOp::SetMinSize { width: None, height: None })));
        assert!(matches!(WindowOp::from_json(r#"{"op":"navigate","url":"about.html"}"#), Ok(WindowOp::Navigate { .. })));
        assert!(matches!(
            WindowOp::from_json(r#"{"op":"setProgress","progress":0.5,"state":null}"#),
            Ok(WindowOp::SetProgress { progress: Some(p), state: None }) if p == 0.5
        ));
        assert!(matches!(WindowOp::from_json(r#"{"op":"setZoom","factor":1.25}"#), Ok(WindowOp::SetZoom { .. })));
        assert!(WindowOp::from_json(r#"{"op":"explode"}"#).is_err());
        assert!(WindowOp::from_json(r#"{"op":"setSize","width":"big"}"#).is_err());
    }
}
