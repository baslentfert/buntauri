//! Native extras: file/message dialogs (rfd), notifications (notify-rust),
//! clipboard text (arboard) and global shortcuts (global-hotkey). The same
//! crates Tauri's dialog, notification, clipboard and global-shortcut plugins use.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Filter {
    pub name: String,
    pub extensions: Vec<String>,
}

/// A dialog, as JSON: `{"kind": "open", "multiple": true, "filters": [...]}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DialogSpec {
    /// Pick file(s), or folder(s) with `directory: true`. Result: a path, an
    /// array of paths (`multiple`), or null when cancelled.
    Open {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        default_path: Option<String>,
        #[serde(default)]
        filters: Vec<Filter>,
        #[serde(default)]
        multiple: bool,
        #[serde(default)]
        directory: bool,
    },
    /// Choose where to save. Result: a path or null.
    Save {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        default_path: Option<String>,
        #[serde(default)]
        file_name: Option<String>,
        #[serde(default)]
        filters: Vec<Filter>,
    },
    /// Message box. `level`: info | warning | error; `buttons`: ok |
    /// okCancel | yesNo | yesNoCancel. Result: "ok", "cancel", "yes" or "no".
    Message {
        message: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        level: Option<String>,
        #[serde(default)]
        buttons: Option<String>,
    },
}

impl DialogSpec {
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("invalid dialog: {e}"))
    }
}

/// Owner window of a dialog, by native handle (an HWND on Windows), so the
/// dialog is modal to it. Plain data: can be moved to the dialog's thread.
#[derive(Clone, Copy)]
pub(crate) struct Parent(#[allow(dead_code)] pub isize);

#[cfg(target_os = "windows")]
mod parent_handle {
    use super::Parent;
    use raw_window_handle::{
        DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
    };
    use std::num::NonZeroIsize;

    impl HasWindowHandle for Parent {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            let hwnd = NonZeroIsize::new(self.0).ok_or(HandleError::Unavailable)?;
            // SAFETY: the HWND belongs to a live buntauri window for the dialog's lifetime;
            // a closed owner only makes the dialog non-modal.
            Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(Win32WindowHandle::new(hwnd))) })
        }
    }
    impl HasDisplayHandle for Parent {
        fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
            Ok(DisplayHandle::windows())
        }
    }
}

/// Show the dialog (blocking; run it off the UI thread) and return its result.
pub(crate) fn run_dialog(spec: &DialogSpec, parent: Option<Parent>) -> Value {
    macro_rules! with_parent {
        ($d:expr) => {{
            #[allow(unused_mut)]
            let mut d = $d;
            #[cfg(target_os = "windows")]
            if let Some(p) = &parent {
                d = d.set_parent(p);
            }
            #[cfg(not(target_os = "windows"))]
            let _ = &parent;
            d
        }};
    }
    let path = |p: std::path::PathBuf| Value::String(p.to_string_lossy().into_owned());
    match spec {
        DialogSpec::Open { title, default_path, filters, multiple, directory } => {
            let mut d = rfd::FileDialog::new();
            if let Some(t) = title {
                d = d.set_title(t);
            }
            if let Some(p) = default_path {
                d = d.set_directory(p);
            }
            for f in filters {
                d = d.add_filter(&f.name, &f.extensions);
            }
            let d = with_parent!(d);
            match (*directory, *multiple) {
                (false, false) => d.pick_file().map_or(Value::Null, path),
                (false, true) => d.pick_files().map_or(Value::Null, |v| v.into_iter().map(path).collect()),
                (true, false) => d.pick_folder().map_or(Value::Null, path),
                (true, true) => d.pick_folders().map_or(Value::Null, |v| v.into_iter().map(path).collect()),
            }
        }
        DialogSpec::Save { title, default_path, file_name, filters } => {
            let mut d = rfd::FileDialog::new();
            if let Some(t) = title {
                d = d.set_title(t);
            }
            if let Some(p) = default_path {
                d = d.set_directory(p);
            }
            if let Some(n) = file_name {
                d = d.set_file_name(n);
            }
            for f in filters {
                d = d.add_filter(&f.name, &f.extensions);
            }
            with_parent!(d).save_file().map_or(Value::Null, path)
        }
        DialogSpec::Message { message, title, level, buttons } => {
            let mut d = rfd::MessageDialog::new().set_description(message).set_level(match level.as_deref() {
                Some("warning") => rfd::MessageLevel::Warning,
                Some("error") => rfd::MessageLevel::Error,
                _ => rfd::MessageLevel::Info,
            });
            if let Some(t) = title {
                d = d.set_title(t);
            }
            d = d.set_buttons(match buttons.as_deref() {
                Some("okCancel") => rfd::MessageButtons::OkCancel,
                Some("yesNo") => rfd::MessageButtons::YesNo,
                Some("yesNoCancel") => rfd::MessageButtons::YesNoCancel,
                _ => rfd::MessageButtons::Ok,
            });
            json!(match with_parent!(d).show() {
                rfd::MessageDialogResult::Ok => "ok",
                rfd::MessageDialogResult::Yes => "yes",
                rfd::MessageDialogResult::No => "no",
                rfd::MessageDialogResult::Cancel => "cancel",
                rfd::MessageDialogResult::Custom(_) => "ok",
            })
        }
    }
}

/// A desktop notification. On Windows a toast; `appId` is the AppUserModelID
/// shown as the sender (default: Windows PowerShell's, as notify-rust does).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NotificationSpec {
    pub title: String,
    pub body: Option<String>,
    pub app_id: Option<String>,
    pub icon: Option<String>,
}

impl NotificationSpec {
    pub fn from_json(json: &str) -> Result<Self, String> {
        let spec: Self = serde_json::from_str(json).map_err(|e| format!("invalid notification: {e}"))?;
        if spec.title.is_empty() {
            return Err("a notification needs a title".into());
        }
        Ok(spec)
    }
}

pub(crate) fn notify(spec: &NotificationSpec) -> Result<(), String> {
    let mut n = notify_rust::Notification::new();
    n.summary(&spec.title);
    if let Some(b) = &spec.body {
        n.body(b);
    }
    if let Some(i) = &spec.icon {
        n.icon(i);
    }
    #[cfg(target_os = "windows")]
    if let Some(id) = &spec.app_id {
        n.app_id(id);
    }
    n.show().map(|_| ()).map_err(|e| e.to_string())
}

/// Clipboard text. Synchronous; fine from any thread.
pub fn clipboard_read_text() -> Result<Option<String>, String> {
    let mut c = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    match c.get_text() {
        Ok(t) => Ok(Some(t)),
        Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

pub fn clipboard_write_text(text: &str) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut c| c.set_text(text))
        .map_err(|e| e.to_string())
}

/// Parse a global shortcut like "CmdOrCtrl+Shift+K".
pub fn parse_shortcut(accelerator: &str) -> Result<global_hotkey::hotkey::HotKey, String> {
    accelerator
        .parse::<global_hotkey::hotkey::HotKey>()
        .map_err(|e| format!("bad shortcut {accelerator:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_parse() {
        assert!(matches!(
            DialogSpec::from_json(r#"{"kind":"open","multiple":true,"filters":[{"name":"Images","extensions":["png","jpg"]}]}"#),
            Ok(DialogSpec::Open { multiple: true, ref filters, .. }) if filters[0].extensions.len() == 2
        ));
        assert!(matches!(DialogSpec::from_json(r#"{"kind":"save","fileName":"a.txt"}"#), Ok(DialogSpec::Save { file_name: Some(_), .. })));
        assert!(matches!(
            DialogSpec::from_json(r#"{"kind":"message","message":"Hi","buttons":"yesNo"}"#),
            Ok(DialogSpec::Message { .. })
        ));
        assert!(DialogSpec::from_json(r#"{"kind":"message"}"#).is_err());
        assert!(DialogSpec::from_json(r#"{"kind":"popup"}"#).is_err());
        assert!(NotificationSpec::from_json(r#"{"title":"Done","body":"All good"}"#).is_ok());
        assert!(NotificationSpec::from_json(r#"{"body":"no title"}"#).is_err());
    }

    #[test]
    fn shortcuts_parse() {
        assert!(parse_shortcut("CmdOrCtrl+Shift+K").is_ok());
        assert!(parse_shortcut("Alt+F4").is_ok());
        assert!(parse_shortcut("Ctrl+Nope").is_err());
        assert_eq!(parse_shortcut("CmdOrCtrl+Shift+K").unwrap().id(), parse_shortcut("CmdOrCtrl+Shift+K").unwrap().id());
    }
}
