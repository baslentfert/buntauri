# Window options

buntauri accepts the same window options as Tauri's `WindowConfig`
(`tauri.conf.json` > `app.windows[]`), with the same camelCase names and defaults.
A window entry from a Tauri config can be passed from Bun JS as-is.

Unknown keys and options that don't apply on the current platform are not
errors: they come back as `Warning` events, so typos don't go unnoticed.
Wrong types (`"width": "wide"`, `"theme": "blue"`) are errors.

✅ applied · 🪟 Windows only · ⏳ accepted, not applied yet · — not applicable

| Option | Status | Notes |
|---|---|---|
| `label` | ✅ | Unique; used by `parent`. Default `"main"` |
| `url` | ✅ | `index.html` / `/path` → embedded assets (`app://`), or a full URL |
| `html` | ✅ | buntauri extra: inline HTML when `url` is unset |
| `title`, `width`, `height` | ✅ | Default `"buntauri"`, 800×600 |
| `x`, `y`, `center` | ✅ | `center` uses the window's real outer size |
| `minWidth`, `minHeight`, `maxWidth`, `maxHeight` | ✅ | |
| `preventOverflow` | ✅ | `true` or `{ width, height }` margin. Uses the monitor size (taskbar not subtracted) |
| `resizable`, `maximizable`, `minimizable`, `closable` | ✅ | |
| `fullscreen`, `maximized`, `visible`, `focus`, `focusable` | ✅ | |
| `decorations`, `transparent`, `shadow` | ✅ | `shadow`: Windows undecorated windows |
| `alwaysOnTop`, `alwaysOnBottom`, `visibleOnAllWorkspaces` | ✅ | |
| `contentProtected` | ✅ | |
| `theme` | ✅ | `"light"` / `"dark"`, window and webview |
| `backgroundColor` | ✅ | `"#rgb[a]"`, `"#rrggbb[aa]"`, `[r,g,b(,a)]`, `{red,green,blue,alpha}` |
| `parent` | 🪟 | Label of the owner window |
| `skipTaskbar` | ✅ | Windows, Linux |
| `windowClassname`, `noRedirectionBitmap` | 🪟 | |
| `userAgent`, `incognito`, `devtools` | ✅ | `devtools` unset: on in debug builds |
| `dragDropEnabled` | ✅ | Native file drops → `DragDrop` events. Turn off for HTML5 drag & drop on Windows |
| `zoomHotkeysEnabled`, `javascriptDisabled`, `generalAutofillEnabled` | ✅ | |
| `backgroundThrottling` | ✅ | `"disabled"` / `"suspend"` / `"throttle"` |
| `proxyUrl` | ✅ | `http://host:port` or `socks5://host:port` |
| `dataDirectory` | ✅ | Cookies/storage directory for this webview |
| `additionalBrowserArgs`, `browserExtensionsEnabled`, `useHttpsScheme`, `scrollBarStyle` | 🪟 | WebView2 |
| `titleBarStyle`, `trafficLightPosition`, `hiddenTitle`, `acceptFirstMouse`, `tabbingIdentifier`, `allowLinkPreview`, `dataStoreIdentifier` | ⏳ | macOS, phase 4 |
| `windowEffects` | ⏳ | Mica/Acrylic/vibrancy; needs the `window-vibrancy` crate |
| `disableInputAccessoryView`, `limitNavigationsToAppBoundDomains`, `activityName`, `createdByActivityName`, `requestedBySceneIdentifier` | — | Mobile |
| `create` | — | The host creates windows itself |
| `ipc` | ✅ | buntauri extra: `{ remote: [urlPatterns], remoteCommands: [names] }` |

## From Bun

```ts
import { Window } from "bun:buntauri";

const win = new Window({
  title: "My app",
  width: 1024, height: 768, minWidth: 400,
  center: true, theme: "dark", backgroundColor: "#111",
});
```

The JS binding sends `JSON.stringify(options)` to `UiThread::create_window_json`.
