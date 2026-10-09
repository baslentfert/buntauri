//! Window options, using the same names as Tauri's `WindowConfig`
//! (`tauri.conf.json` > `app.windows[]`), so a window entry from a Tauri
//! config can be passed from Bun JS as-is: `JSON.stringify(opts)` ->
//! [`WindowOptions::from_json`].
//!
//! Applied today: everything cross-platform plus the Windows-only options.
//! macOS/Linux/mobile-only options are accepted but not applied yet; they
//! are reported as warnings so nothing is silently ignored.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tao::dpi::{LogicalPosition, LogicalSize, LogicalUnit, PixelUnit};
use tao::window::{Theme as TaoTheme, WindowBuilder, WindowSizeConstraints};
use wry::{BackgroundThrottlingPolicy, ProxyConfig, ProxyEndpoint, WebViewBuilder};

/// Window + webview options. Field names (camelCase in JSON) and defaults
/// follow Tauri's `WindowConfig`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WindowOptions {
    /// Unique name, used by `parent` and reported back in events.
    pub label: String,
    /// `index.html` / `/path` -> embedded assets (`app://`), or a full URL.
    pub url: Option<String>,
    /// buntauri extra: inline HTML, used when `url` is not set.
    pub html: Option<String>,
    pub user_agent: Option<String>,
    /// Native file drag & drop events (`HostEvent::DragDrop`). Turn off to
    /// use HTML5 drag & drop in the page on Windows.
    pub drag_drop_enabled: bool,
    pub center: bool,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub width: f64,
    pub height: f64,
    pub min_width: Option<f64>,
    pub min_height: Option<f64>,
    pub max_width: Option<f64>,
    pub max_height: Option<f64>,
    /// `true`, or `{ "width": .., "height": .. }` margin: keep the window inside the monitor.
    pub prevent_overflow: Option<PreventOverflow>,
    pub resizable: bool,
    pub maximizable: bool,
    pub minimizable: bool,
    pub closable: bool,
    pub title: String,
    pub fullscreen: bool,
    pub focus: bool,
    pub focusable: bool,
    pub transparent: bool,
    pub maximized: bool,
    pub visible: bool,
    pub decorations: bool,
    pub always_on_bottom: bool,
    pub always_on_top: bool,
    pub visible_on_all_workspaces: bool,
    pub content_protected: bool,
    /// Windows, Linux.
    pub skip_taskbar: bool,
    /// Windows.
    pub window_classname: Option<String>,
    /// Windows.
    pub no_redirection_bitmap: bool,
    /// `"light"` or `"dark"`; unset follows the system.
    pub theme: Option<Theme>,
    /// Windows: extra WebView2 browser arguments.
    pub additional_browser_args: Option<String>,
    /// Windows (undecorated windows), macOS.
    pub shadow: bool,
    pub incognito: bool,
    /// Label of the owner window.
    pub parent: Option<String>,
    /// `http://host:port` or `socks5://host:port`.
    pub proxy_url: Option<String>,
    pub zoom_hotkeys_enabled: bool,
    /// Windows.
    pub browser_extensions_enabled: bool,
    /// Windows: serve `app://` as `https://app.localhost` instead of `http://`.
    pub use_https_scheme: bool,
    /// Unset: on in debug builds, off in release.
    pub devtools: Option<bool>,
    /// `"#rrggbb"`, `"#rrggbbaa"`, `[r, g, b, a]` or `{ red, green, blue, alpha }`.
    pub background_color: Option<Color>,
    /// `"disabled"`, `"suspend"` or `"throttle"`.
    pub background_throttling: Option<BackgroundThrottling>,
    pub javascript_disabled: bool,
    /// Webview data (cookies, storage) directory.
    pub data_directory: Option<PathBuf>,
    /// Windows: `"default"` or `"fluentOverlay"`.
    pub scroll_bar_style: ScrollBarStyle,
    pub general_autofill_enabled: bool,

    /// buntauri extra: who may call `invoke` besides `app://` pages.
    pub ipc: IpcPolicy,
    /// buntauri extra: window icon, a base64 PNG (or data: URL).
    pub icon: Option<String>,
    /// buntauri extra: menu bar (see `MenuItemSpec`).
    pub menu: Option<Vec<crate::MenuItemSpec>>,
    /// buntauri extra: the close button emits `closerequested` instead of
    /// closing (e.g. to hide to the tray). Can be changed at run time.
    pub prevent_close: bool,

    /// Everything else (unknown keys and options not applied on this platform).
    #[serde(flatten)]
    pub other: serde_json::Map<String, Value>,
}

impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            label: "main".into(),
            url: None,
            html: None,
            user_agent: None,
            drag_drop_enabled: true,
            center: false,
            x: None,
            y: None,
            width: 800.0,
            height: 600.0,
            min_width: None,
            min_height: None,
            max_width: None,
            max_height: None,
            prevent_overflow: None,
            resizable: true,
            maximizable: true,
            minimizable: true,
            closable: true,
            title: "buntauri".into(),
            fullscreen: false,
            focus: true,
            focusable: true,
            transparent: false,
            maximized: false,
            visible: true,
            decorations: true,
            always_on_bottom: false,
            always_on_top: false,
            visible_on_all_workspaces: false,
            content_protected: false,
            skip_taskbar: false,
            window_classname: None,
            no_redirection_bitmap: false,
            theme: None,
            additional_browser_args: None,
            shadow: true,
            incognito: false,
            parent: None,
            proxy_url: None,
            zoom_hotkeys_enabled: false,
            browser_extensions_enabled: false,
            use_https_scheme: false,
            devtools: None,
            background_color: None,
            background_throttling: None,
            javascript_disabled: false,
            data_directory: None,
            scroll_bar_style: ScrollBarStyle::Default,
            general_autofill_enabled: true,
            ipc: IpcPolicy::default(),
            icon: None,
            menu: None,
            prevent_close: false,
            other: serde_json::Map::new(),
        }
    }
}

/// IPC access for pages outside `app://`, like Tauri's capability `remote.urls`.
///
/// Set by the host (Bun backend), never by the page: a page can't grant itself
/// access. Pages served from `app://` (and inline HTML set by the host) may
/// always invoke every command.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct IpcPolicy {
    /// URL patterns of remote pages that may invoke, e.g. `https://*.example.com`
    /// or `https://example.com/app/*`. `*` in the host matches subdomains
    /// (not the bare domain); a pattern without a path allows every path.
    pub remote: Vec<String>,
    /// Commands remote pages may call. `None` = all commands.
    pub remote_commands: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(untagged)]
pub enum PreventOverflow {
    Enable(bool),
    Margin { width: f64, height: f64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BackgroundThrottling {
    Disabled,
    Suspend,
    Throttle,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ScrollBarStyle {
    #[default]
    Default,
    FluentOverlay,
}

/// RGBA color in any of the forms Tauri accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u8, pub u8, pub u8, pub u8);

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Hex(String),
            Rgb([u8; 3]),
            Rgba([u8; 4]),
            Obj { red: u8, green: u8, blue: u8, #[serde(default = "opaque")] alpha: u8 },
        }
        fn opaque() -> u8 {
            255
        }
        match Repr::deserialize(d)? {
            Repr::Hex(s) => parse_hex(&s).ok_or_else(|| serde::de::Error::custom(format!("invalid color {s:?}"))),
            Repr::Rgb([r, g, b]) => Ok(Color(r, g, b, 255)),
            Repr::Rgba([r, g, b, a]) => Ok(Color(r, g, b, a)),
            Repr::Obj { red, green, blue, alpha } => Ok(Color(red, green, blue, alpha)),
        }
    }
}

impl Serialize for Color {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("#{:02x}{:02x}{:02x}{:02x}", self.0, self.1, self.2, self.3))
    }
}

fn parse_hex(s: &str) -> Option<Color> {
    let h = s.strip_prefix('#')?;
    let nib = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|v| v * 17);
    let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    if !h.is_ascii() {
        return None;
    }
    match h.len() {
        3 => Some(Color(nib(0)?, nib(1)?, nib(2)?, 255)),
        4 => Some(Color(nib(0)?, nib(1)?, nib(2)?, nib(3)?)),
        6 => Some(Color(byte(0)?, byte(2)?, byte(4)?, 255)),
        8 => Some(Color(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
        _ => None,
    }
}

/// Tauri options that are valid but not applied on this platform (yet).
const NOT_APPLIED: &[&str] = &[
    // macOS
    "titleBarStyle",
    "trafficLightPosition",
    "hiddenTitle",
    "acceptFirstMouse",
    "tabbingIdentifier",
    "allowLinkPreview",
    "dataStoreIdentifier",
    // window-vibrancy (Mica/Acrylic/vibrancy), not part of tao/wry
    "windowEffects",
    // mobile
    "disableInputAccessoryView",
    "limitNavigationsToAppBoundDomains",
    "activityName",
    "createdByActivityName",
    "requestedBySceneIdentifier",
];

/// Tauri options that are meaningless here: the host creates windows itself.
const IGNORED: &[&str] = &["create"];

impl WindowOptions {
    /// Parse options from JSON (what Bun JS sends with `JSON.stringify`).
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|e| format!("invalid window options: {e}"))
    }

    /// Keys that were given but have no effect, with the reason.
    pub fn warnings(&self) -> Vec<String> {
        self.other
            .keys()
            .filter(|k| !IGNORED.contains(&k.as_str()))
            .map(|k| {
                if NOT_APPLIED.contains(&k.as_str()) {
                    format!("window option {k:?} is not supported on this platform yet")
                } else {
                    format!("unknown window option {k:?}")
                }
            })
            .collect()
    }

    pub(crate) fn proxy(&self) -> Result<Option<ProxyConfig>, String> {
        let Some(url) = &self.proxy_url else { return Ok(None) };
        let uri: wry::http::Uri = url.parse().map_err(|_| format!("invalid proxyUrl {url:?}"))?;
        let endpoint = ProxyEndpoint {
            host: uri.host().ok_or_else(|| format!("proxyUrl {url:?} has no host"))?.to_string(),
            port: uri.port_u16().ok_or_else(|| format!("proxyUrl {url:?} has no port"))?.to_string(),
        };
        match uri.scheme_str() {
            Some("http") => Ok(Some(ProxyConfig::Http(endpoint))),
            Some("socks5") => Ok(Some(ProxyConfig::Socks5(endpoint))),
            _ => Err(format!("proxyUrl {url:?} must start with http:// or socks5://")),
        }
    }

    /// Window settings. `center` / `preventOverflow` are applied after the
    /// window exists (see `place`), because they need its real outer size.
    pub(crate) fn window_builder(&self) -> WindowBuilder {
        let logical = |v: Option<f64>| v.map(|v| PixelUnit::Logical(LogicalUnit(v)));
        let mut b = WindowBuilder::new()
            .with_title(&self.title)
            .with_inner_size(LogicalSize::new(self.width, self.height))
            .with_inner_size_constraints(WindowSizeConstraints::new(
                logical(self.min_width),
                logical(self.min_height),
                logical(self.max_width),
                logical(self.max_height),
            ))
            .with_resizable(self.resizable)
            .with_maximizable(self.maximizable)
            .with_minimizable(self.minimizable)
            .with_closable(self.closable)
            .with_maximized(self.maximized)
            .with_visible(self.visible && !self.needs_placement())
            .with_focused(self.focus)
            .with_focusable(self.focusable)
            .with_transparent(self.transparent)
            .with_decorations(self.decorations)
            .with_always_on_bottom(self.always_on_bottom)
            .with_always_on_top(self.always_on_top)
            .with_visible_on_all_workspaces(self.visible_on_all_workspaces)
            .with_content_protection(self.content_protected)
            .with_theme(self.theme.map(|t| match t {
                Theme::Light => TaoTheme::Light,
                Theme::Dark => TaoTheme::Dark,
            }));
        if let (Some(x), Some(y)) = (self.x, self.y) {
            b = b.with_position(LogicalPosition::new(x, y));
        }
        if self.fullscreen {
            b = b.with_fullscreen(Some(tao::window::Fullscreen::Borderless(None)));
        }
        if let Some(Color(r, g, bl, a)) = self.background_color {
            b = b.with_background_color((r, g, bl, a));
        }
        #[cfg(target_os = "windows")]
        {
            use tao::platform::windows::WindowBuilderExtWindows;
            b = b
                .with_skip_taskbar(self.skip_taskbar)
                .with_no_redirection_bitmap(self.no_redirection_bitmap)
                .with_drag_and_drop(self.drag_drop_enabled)
                .with_undecorated_shadow(self.shadow);
            if let Some(class) = &self.window_classname {
                b = b.with_window_classname(class);
            }
        }
        #[cfg(any(target_os = "linux", target_os = "dragonfly", target_os = "freebsd", target_os = "netbsd", target_os = "openbsd"))]
        {
            use tao::platform::unix::WindowBuilderExtUnix;
            b = b.with_skip_taskbar(self.skip_taskbar);
        }
        b
    }

    pub(crate) fn needs_placement(&self) -> bool {
        self.center || matches!(self.prevent_overflow, Some(PreventOverflow::Enable(true)) | Some(PreventOverflow::Margin { .. }))
    }

    /// Apply `preventOverflow` and `center` using the window's real size,
    /// then show it if it was meant to be visible.
    pub(crate) fn place(&self, window: &tao::window::Window) {
        if !self.needs_placement() {
            return;
        }
        if let Some(monitor) = window.current_monitor() {
            let scale = monitor.scale_factor();
            let (mpos, msize) = (monitor.position(), monitor.size());
            if let Some(po) = self.prevent_overflow {
                let (mw, mh) = match po {
                    PreventOverflow::Margin { width, height } => ((width * scale) as u32, (height * scale) as u32),
                    _ => (0, 0),
                };
                let limit = (msize.width.saturating_sub(mw), msize.height.saturating_sub(mh));
                let (outer, inner) = (window.outer_size(), window.inner_size());
                if outer.width > limit.0 || outer.height > limit.1 {
                    let w = inner.width.saturating_sub(outer.width.saturating_sub(limit.0));
                    let h = inner.height.saturating_sub(outer.height.saturating_sub(limit.1));
                    window.set_inner_size(tao::dpi::PhysicalSize::new(w, h));
                }
            }
            if self.center {
                let outer = window.outer_size();
                let x = mpos.x + (msize.width as i32 - outer.width as i32) / 2;
                let y = mpos.y + (msize.height as i32 - outer.height as i32) / 2;
                window.set_outer_position(tao::dpi::PhysicalPosition::new(x, y));
            }
        }
        if self.visible {
            window.set_visible(true);
            if self.focus {
                window.set_focus();
            }
        }
    }

    /// Webview settings that don't need host callbacks.
    pub(crate) fn apply_webview<'a>(&self, mut b: WebViewBuilder<'a>) -> Result<WebViewBuilder<'a>, String> {
        b = b
            .with_devtools(self.devtools.unwrap_or(cfg!(debug_assertions)))
            .with_transparent(self.transparent)
            .with_focused(self.focus)
            .with_visible(true)
            .with_incognito(self.incognito)
            .with_hotkeys_zoom(self.zoom_hotkeys_enabled)
            .with_general_autofill_enabled(self.general_autofill_enabled);
        if let Some(ua) = &self.user_agent {
            b = b.with_user_agent(ua);
        }
        if let Some(Color(r, g, bl, a)) = self.background_color {
            b = b.with_background_color((r, g, bl, a));
        }
        if let Some(t) = self.background_throttling {
            b = b.with_background_throttling(match t {
                BackgroundThrottling::Disabled => BackgroundThrottlingPolicy::Disabled,
                BackgroundThrottling::Suspend => BackgroundThrottlingPolicy::Suspend,
                BackgroundThrottling::Throttle => BackgroundThrottlingPolicy::Throttle,
            });
        }
        if self.javascript_disabled {
            b = b.with_javascript_disabled();
        }
        if let Some(p) = self.proxy()? {
            b = b.with_proxy_config(p);
        }
        #[cfg(target_os = "windows")]
        {
            use wry::WebViewBuilderExtWindows;
            b = b
                .with_https_scheme(self.use_https_scheme)
                .with_browser_extensions_enabled(self.browser_extensions_enabled)
                .with_scroll_bar_style(match self.scroll_bar_style {
                    ScrollBarStyle::Default => wry::ScrollBarStyle::Default,
                    ScrollBarStyle::FluentOverlay => wry::ScrollBarStyle::FluentOverlay,
                });
            if let Some(t) = self.theme {
                b = b.with_theme(match t {
                    Theme::Light => wry::Theme::Light,
                    Theme::Dark => wry::Theme::Dark,
                });
            }
            if let Some(args) = &self.additional_browser_args {
                b = b.with_additional_browser_args(args);
            }
        }
        Ok(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tauri_conf_window_parses() {
        // A window entry as it appears in tauri.conf.json.
        let o = WindowOptions::from_json(
            r##"{
                "label": "main", "title": "My app", "width": 1024, "height": 768,
                "minWidth": 400, "center": true, "resizable": false, "alwaysOnTop": true,
                "theme": "dark", "backgroundColor": "#11223380", "decorations": false,
                "preventOverflow": { "width": 10, "height": 20 }, "scrollBarStyle": "fluentOverlay",
                "backgroundThrottling": "disabled", "proxyUrl": "socks5://127.0.0.1:9050",
                "titleBarStyle": "Overlay", "create": false, "tittle": "typo",
                "ipc": { "remote": ["https://*.example.com"], "remoteCommands": ["greet"] }
            }"##,
        )
        .unwrap();
        assert_eq!(o.title, "My app");
        assert_eq!((o.width, o.height), (1024.0, 768.0));
        assert_eq!(o.min_width, Some(400.0));
        assert!(o.center && !o.resizable && o.always_on_top && !o.decorations);
        assert_eq!(o.theme, Some(Theme::Dark));
        assert_eq!(o.background_color, Some(Color(0x11, 0x22, 0x33, 0x80)));
        assert!(matches!(o.prevent_overflow, Some(PreventOverflow::Margin { width, height }) if width == 10.0 && height == 20.0));
        assert_eq!(o.scroll_bar_style, ScrollBarStyle::FluentOverlay);
        assert!(matches!(o.proxy(), Ok(Some(ProxyConfig::Socks5(_)))));
        assert_eq!(o.ipc.remote, vec!["https://*.example.com"]);
        // Defaults follow Tauri.
        assert!(o.visible && o.focus && o.closable && o.drag_drop_enabled && o.shadow);
        let w = o.warnings();
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w.iter().any(|m| m.contains("titleBarStyle") && m.contains("not supported")));
        assert!(w.iter().any(|m| m.contains("tittle") && m.contains("unknown")));
    }

    #[test]
    fn colors() {
        let c = |j: &str| serde_json::from_str::<Color>(j).unwrap();
        assert_eq!(c(r##""#fff""##), Color(255, 255, 255, 255));
        assert_eq!(c(r##""#0f08""##), Color(0, 255, 0, 136));
        assert_eq!(c(r##""#102030""##), Color(16, 32, 48, 255));
        assert_eq!(c("[1,2,3]"), Color(1, 2, 3, 255));
        assert_eq!(c("[1,2,3,4]"), Color(1, 2, 3, 4));
        assert_eq!(c(r#"{"red":1,"green":2,"blue":3}"#), Color(1, 2, 3, 255));
        assert!(serde_json::from_str::<Color>(r#""red""#).is_err());
        assert!(serde_json::from_str::<Color>(r##""#12345""##).is_err());
    }

    #[test]
    fn bad_options_are_errors() {
        assert!(WindowOptions::from_json(r#"{"width": "wide"}"#).is_err());
        assert!(WindowOptions::from_json(r#"{"theme": "blue"}"#).is_err());
        let o = WindowOptions::from_json(r#"{"proxyUrl": "ftp://x:1"}"#).unwrap();
        assert!(o.proxy().is_err());
    }
}
