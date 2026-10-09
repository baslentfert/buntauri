//! Menus (muda), tray icons (tray-icon) and icons, built on the UI thread
//! from plain data the host sends (JSON-friendly specs).
//!
//! muda/tray-icon objects are not `Send`, so the host only ever sends specs;
//! the UI thread builds and owns the native objects.

use std::io::Cursor;

use muda::accelerator::Accelerator;
use muda::{CheckMenuItem, IsMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use serde::Deserialize;

/// One menu entry. Shapes (Tauri-like):
/// - `{ "id": "save", "text": "Save", "accelerator": "CmdOrCtrl+S" }`
/// - `{ "id": "dark", "text": "Dark mode", "type": "check", "checked": true }`
/// - `{ "text": "File", "items": [ ... ] }` (submenu)
/// - `{ "type": "separator" }`
/// - `{ "predefined": "copy" }` (copy, cut, paste, selectAll, undo, redo,
///   minimize, maximize, fullscreen, hide, closeWindow, quit, about)
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MenuItemSpec {
    pub id: Option<String>,
    pub text: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub enabled: Option<bool>,
    pub checked: Option<bool>,
    pub accelerator: Option<String>,
    pub items: Option<Vec<MenuItemSpec>>,
    pub predefined: Option<String>,
}

/// Tray icon. `icon` is a PNG, base64-encoded.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TraySpec {
    pub icon: Option<String>,
    pub tooltip: Option<String>,
    pub title: Option<String>,
    pub menu: Option<Vec<MenuItemSpec>>,
    /// Show the menu on left click too (default: right click only).
    pub menu_on_left_click: bool,
    pub visible: Option<bool>,
}

/// Who owns a menu: menu item ids are namespaced per owner so one global
/// muda event handler can route clicks (`w3:save`, `t7:quit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Window(u32),
    Tray(u32),
}

impl Owner {
    fn prefix(self) -> String {
        match self {
            Owner::Window(id) => format!("w{id}:"),
            Owner::Tray(id) => format!("t{id}:"),
        }
    }

    /// Split a namespaced muda id back into (owner, user id).
    pub fn parse(id: &str) -> Option<(Owner, String)> {
        let (head, rest) = id.split_once(':')?;
        let n: u32 = head.get(1..)?.parse().ok()?;
        let owner = match head.as_bytes().first()? {
            b'w' => Owner::Window(n),
            b't' => Owner::Tray(n),
            _ => return None,
        };
        Some((owner, rest.to_string()))
    }
}

pub fn build_menu(owner: Owner, specs: &[MenuItemSpec]) -> Result<Menu, String> {
    let menu = Menu::new();
    for item in build_items(owner, specs)? {
        menu.append(item.as_ref()).map_err(|e| e.to_string())?;
    }
    Ok(menu)
}

fn build_items(owner: Owner, specs: &[MenuItemSpec]) -> Result<Vec<Box<dyn IsMenuItem>>, String> {
    let mut out: Vec<Box<dyn IsMenuItem>> = Vec::with_capacity(specs.len());
    let mut anon = 0;
    for s in specs {
        let enabled = s.enabled.unwrap_or(true);
        let text = s.text.clone().unwrap_or_default();
        let id = MenuId::new(format!(
            "{}{}",
            owner.prefix(),
            s.id.clone().unwrap_or_else(|| {
                anon += 1;
                format!("_{anon}")
            })
        ));
        let accel = match &s.accelerator {
            Some(a) => Some(a.parse::<Accelerator>().map_err(|e| format!("bad accelerator {a:?}: {e}"))?),
            None => None,
        };

        if let Some(p) = &s.predefined {
            let t = s.text.as_deref();
            out.push(Box::new(match p.as_str() {
                "separator" => PredefinedMenuItem::separator(),
                "copy" => PredefinedMenuItem::copy(t),
                "cut" => PredefinedMenuItem::cut(t),
                "paste" => PredefinedMenuItem::paste(t),
                "selectAll" => PredefinedMenuItem::select_all(t),
                "undo" => PredefinedMenuItem::undo(t),
                "redo" => PredefinedMenuItem::redo(t),
                "minimize" => PredefinedMenuItem::minimize(t),
                "maximize" => PredefinedMenuItem::maximize(t),
                "fullscreen" => PredefinedMenuItem::fullscreen(t),
                "hide" => PredefinedMenuItem::hide(t),
                "closeWindow" => PredefinedMenuItem::close_window(t),
                "quit" => PredefinedMenuItem::quit(t),
                "about" => PredefinedMenuItem::about(t, None),
                other => return Err(format!("unknown predefined menu item {other:?}")),
            }));
            continue;
        }

        match (s.kind.as_deref(), &s.items) {
            (Some("separator"), _) => out.push(Box::new(PredefinedMenuItem::separator())),
            (_, Some(children)) => {
                let sub = Submenu::with_id(id, text, enabled);
                for child in build_items(owner, children)? {
                    sub.append(child.as_ref()).map_err(|e| e.to_string())?;
                }
                out.push(Box::new(sub));
            }
            (Some("check"), None) => out.push(Box::new(CheckMenuItem::with_id(id, text, enabled, s.checked.unwrap_or(false), accel))),
            (None | Some("normal"), None) => out.push(Box::new(MenuItem::with_id(id, text, enabled, accel))),
            (Some(other), None) => return Err(format!("unknown menu item type {other:?}")),
        }
    }
    Ok(out)
}

/// Decode a PNG into straight RGBA8.
pub fn decode_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let mut dec = png::Decoder::new(Cursor::new(bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().map_err(|e| format!("bad PNG: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("PNG too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("bad PNG: {e}"))?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("indexed PNG not expanded".into()),
    };
    Ok((rgba, info.width, info.height))
}

/// Base64 (standard alphabet, padding optional) to bytes.
pub fn decode_base64(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let s = s.trim();
    let s = s.split_once("base64,").map_or(s, |(_, rest)| rest); // accept data: URLs too
    base64::engine::general_purpose::STANDARD
        .decode(s.trim_end_matches('='))
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(s.trim_end_matches('=')))
        .map_err(|e| format!("bad base64: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_ids_round_trip() {
        assert_eq!(Owner::parse("w3:save"), Some((Owner::Window(3), "save".into())));
        assert_eq!(Owner::parse("t12:a:b"), Some((Owner::Tray(12), "a:b".into())));
        assert_eq!(Owner::parse("x1:y"), None);
        assert_eq!(Owner::parse("nocolon"), None);
    }

    #[test]
    fn menu_specs_parse() {
        let specs: Vec<MenuItemSpec> = serde_json::from_str(
            r#"[{"text":"File","items":[{"id":"open","text":"Open","accelerator":"CmdOrCtrl+O"},{"type":"separator"},{"predefined":"quit"}]},
                {"id":"dark","text":"Dark","type":"check","checked":true}]"#,
        )
        .unwrap();
        assert_eq!(specs[0].items.as_ref().unwrap().len(), 3);
        assert_eq!(specs[1].checked, Some(true));
        assert!("CmdOrCtrl+O".parse::<Accelerator>().is_ok());
        assert!("Nope+Q+".parse::<Accelerator>().is_err());
    }

    #[test]
    fn png_and_base64() {
        // 1x1 red RGBA PNG.
        let b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==";
        let bytes = decode_base64(b64).unwrap();
        let (rgba, w, h) = decode_png(&bytes).unwrap();
        assert_eq!((w, h, rgba.len()), (1, 1, 4));
        assert!(decode_png(b"not a png").is_err());
        assert_eq!(decode_base64("data:image/png;base64,aGk=").unwrap(), b"hi");
    }
}
