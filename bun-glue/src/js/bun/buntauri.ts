// bun:buntauri - native windows (tao + wry) for Bun.
// Copied from the buntauri repo (bun-glue/); edit it there.
//
//   import { Window, Tray } from "bun:buntauri";
//   const win = new Window({ title: "Hello", url: "index.html", icon: "icon.png" });
//   win.handle("greet", ({ name }) => `Hello ${name}`);
//   win.setMenu([{ text: "File", items: [{ text: "Quit", accelerator: "CmdOrCtrl+Q", action: () => win.close() }] }]);
//   new Tray({ icon: "icon.png", tooltip: "My app", menu: [{ text: "Hello", action: () => win.emit("hello") }] });
//
// Window options use the names of Tauri's WindowConfig (tauri.conf.json).
// Pages call `await window.__BUNTAURI__.invoke(cmd, args)`.

const nativeInit = $newRustFunction("buntauri/window.rs", "init", 2);
const nativeCreate = $newRustFunction("buntauri/window.rs", "createWindow", 1);
const nativeEval = $newRustFunction("buntauri/window.rs", "evalScript", 2);
const nativeSetTitle = $newRustFunction("buntauri/window.rs", "setTitle", 2);
const nativeClose = $newRustFunction("buntauri/window.rs", "close", 1);
const nativeResolve = $newRustFunction("buntauri/window.rs", "resolve", 3);
const nativeReject = $newRustFunction("buntauri/window.rs", "reject", 3);
const nativeEmit = $newRustFunction("buntauri/window.rs", "emit", 3);
const nativeSetIcon = $newRustFunction("buntauri/window.rs", "setIcon", 2);
const nativeSetMenu = $newRustFunction("buntauri/window.rs", "setMenu", 2);
const nativePopupMenu = $newRustFunction("buntauri/window.rs", "popupMenu", 4);
const nativeWindowOp = $newRustFunction("buntauri/window.rs", "windowOp", 2);
const nativeWindowState = $newRustFunction("buntauri/window.rs", "windowState", 1);
const nativeDialog = $newRustFunction("buntauri/window.rs", "dialog", 2);
const nativeNotify = $newRustFunction("buntauri/window.rs", "notify", 1);
const nativeShortcut = $newRustFunction("buntauri/window.rs", "shortcut", 2);
const nativeEvaluate = $newRustFunction("buntauri/window.rs", "evaluate", 2);
const nativeMonitors = $newRustFunction("buntauri/window.rs", "monitors", 0);
const nativeClipboardRead = $newRustFunction("buntauri/window.rs", "clipboardRead", 0);
const nativeClipboardWrite = $newRustFunction("buntauri/window.rs", "clipboardWrite", 1);
const nativeTrayCreate = $newRustFunction("buntauri/window.rs", "trayCreate", 1);
const nativeTrayUpdate = $newRustFunction("buntauri/window.rs", "trayUpdate", 2);
const nativeTrayRemove = $newRustFunction("buntauri/window.rs", "trayRemove", 1);

const { readFileSync, unlinkSync } = require("node:fs");
const { Buffer } = require("node:buffer");
const net = require("node:net");
const { tmpdir } = require("node:os");

const windows = new Map<number, Window>();
const trays = new Map<number, Tray>();
let started = false;
let assetsDir: string | undefined;

/** Serve `app://` from this directory. Call before creating the first window. */
function setAssetsDir(dir: string) {
  if (started) throw new Error("setAssetsDir() must be called before the first Window or Tray is created");
  assetsDir = dir;
}

function start() {
  if (started) return;
  started = true;
  nativeInit(onNativeEvent, assetsDir);
}

// Requests answered by a "reply" event (dialogs, notifications, shortcut changes).
const pending = new Map<number, { resolve: (v: any) => void; reject: (e: Error) => void }>();
function request<T>(req: number): Promise<T> {
  return new Promise((resolve, reject) => pending.set(req, { resolve, reject }));
}

const shortcutHandlers = new Map<string, (state: "pressed" | "released") => void>();

function onNativeEvent(json: string) {
  const ev = JSON.parse(json);
  if (ev.type === "reply") {
    const p = pending.get(ev.req);
    pending.delete(ev.req);
    if (p) ev.ok ? p.resolve(ev.value) : p.reject(new Error(String(ev.value)));
    return;
  }
  if (ev.type === "shortcut") {
    shortcutHandlers.get(ev.accelerator)?.(ev.state);
    return;
  }
  if (ev.type === "app") {
    app._dispatch(ev);
    return;
  }
  const target = ev.tray != null ? trays.get(ev.tray) : ev.window != null ? windows.get(ev.window) : undefined;
  if (!target) {
    if (ev.type === "error" || ev.type === "warning") console.warn(`[buntauri] ${ev.message}`);
    return;
  }
  target._dispatch(ev);
}

type IconInput = string | Blob | ArrayBuffer | ArrayBufferView;

/** A PNG as base64: from a file path (also inside a compiled exe), Bun.file(), or bytes. */
function iconBase64(icon: IconInput): string {
  if (typeof icon === "string") return icon.startsWith("data:") ? icon : readFileSync(icon).toString("base64");
  if (icon instanceof Blob) {
    const name = (icon as any).name;
    if (typeof name === "string") return readFileSync(name).toString("base64");
    throw new TypeError("icon: a Blob must be a file (Bun.file); pass its bytes instead");
  }
  if (icon instanceof ArrayBuffer) return Buffer.from(icon).toString("base64");
  if (ArrayBuffer.isView(icon)) return Buffer.from(icon.buffer, icon.byteOffset, icon.byteLength).toString("base64");
  throw new TypeError("icon must be a PNG file path, Bun.file(), Uint8Array or ArrayBuffer");
}

type MenuItem = {
  id?: string;
  text?: string;
  type?: "normal" | "check" | "separator";
  enabled?: boolean;
  checked?: boolean;
  accelerator?: string;
  predefined?: string;
  items?: MenuItem[];
  /** Called when the item is clicked (buntauri extra; no id needed). */
  action?: (id: string) => void;
};

/** Strip `action` functions (kept in `actions` under the item id) so the menu can go to native as JSON. */
function prepareMenu(items: MenuItem[], actions: Map<string, (id: string) => void>): unknown[] {
  return items.map(item => {
    const { action, items: children, ...rest } = item;
    const out: Record<string, unknown> = { ...rest };
    if (action) {
      out.id ??= `_action${actions.size + 1}`;
      actions.set(out.id as string, action);
    }
    if (children) out.items = prepareMenu(children, actions);
    return out;
  });
}

type Listener = (payload: any) => void;

class Emitter {
  #listeners = new Map<string, Set<Listener>>();

  /** Listen to an event (see the class docs for names). */
  on(event: string, fn: Listener) {
    let set = this.#listeners.get(event);
    if (!set) this.#listeners.set(event, (set = new Set()));
    set.add(fn);
    return this;
  }

  off(event: string, fn: Listener) {
    this.#listeners.get(event)?.delete(fn);
    return this;
  }

  /** @internal Returns whether anyone listened. */
  _fire(event: string, payload?: any) {
    const set = this.#listeners.get(event);
    if (!set || set.size === 0) return false;
    for (const fn of set) fn(payload);
    return true;
  }
}

type Handler = (args: any, info: { window: Window; origin: string; remote: boolean }) => any;

type WindowState = {
  visible: boolean;
  minimized: boolean;
  maximized: boolean;
  focused: boolean;
  fullscreen: boolean;
  alwaysOnTop: boolean;
  resizable: boolean;
  decorated: boolean;
  preventClose: boolean;
  /** Inner size and outer position, in CSS pixels. */
  width: number;
  height: number;
  x: number | null;
  y: number | null;
  scaleFactor: number;
  /** The page's current URL. */
  url: string | null;
  devtoolsOpen: boolean;
};

/**
 * A native window with a webview.
 * Events: "created", "closed", "menu" (item id), "dragdrop", "error", "warning",
 * "resized" ({ width, height }), "moved" ({ x, y }), "focus", "blur",
 * "scalechanged" ({ scaleFactor }), "closerequested" (only with preventClose),
 * "externallink" ({ url, action: "opened" | "blocked" | "failed" }; see the externalLinks option).
 */
class Window extends Emitter {
  #id: number;
  #label: string;
  #handlers = new Map<string, Handler>();
  #actions = new Map<string, (id: string) => void>();
  #popupActions = new Map<string, (id: string) => void>();
  #closed = false;
  #ready: Promise<void>;
  #resolveReady!: () => void;
  #rejectReady!: (e: Error) => void;

  constructor(options: Record<string, any> = {}) {
    super();
    start();
    this.#ready = new Promise((resolve, reject) => {
      this.#resolveReady = resolve;
      this.#rejectReady = reject;
    });
    // Created windows report errors through "error"; don't make an unhandled rejection of it.
    this.#ready.catch(() => {});
    this.#label = options.label ?? "main";
    const opts = { ...options };
    if (opts.icon != null) opts.icon = iconBase64(opts.icon);
    if (opts.menu != null) opts.menu = prepareMenu(opts.menu, this.#actions);
    this.#id = nativeCreate(JSON.stringify(opts));
    windows.set(this.#id, this);
  }

  get id() {
    return this.#id;
  }
  get label() {
    return this.#label;
  }
  get closed() {
    return this.#closed;
  }
  /** Resolves when the native window exists; rejects if it could not be created. */
  get ready() {
    return this.#ready;
  }

  /** Answer `invoke(cmd, args)` calls from the page. The return value (or awaited promise) is sent back as JSON. */
  handle(cmd: string, fn: Handler) {
    this.#handlers.set(cmd, fn);
    return this;
  }

  /** Fire an event in the page: `window.__BUNTAURI__.listen(name, fn)`. */
  emit(name: string, payload?: any) {
    if (!this.#closed) nativeEmit(this.#id, String(name), JSON.stringify(payload ?? null));
  }

  /** Run JavaScript in the page (fire and forget). */
  eval(script: string) {
    if (!this.#closed) nativeEval(this.#id, String(script));
  }

  setTitle(title: string) {
    if (!this.#closed) nativeSetTitle(this.#id, String(title));
  }

  /** Window icon (title bar + taskbar): PNG path, Bun.file() or bytes. */
  setIcon(icon: IconInput) {
    if (!this.#closed) nativeSetIcon(this.#id, iconBase64(icon));
  }

  /** Menu bar; `null` removes it. Items may carry an `action` callback. */
  setMenu(items: MenuItem[] | null) {
    if (this.#closed) return;
    this.#actions.clear();
    nativeSetMenu(this.#id, JSON.stringify(items ? prepareMenu(items, this.#actions) : null));
  }

  /** Show a context menu at a position in the window (CSS pixels), or at the cursor. */
  popupMenu(items: MenuItem[], x?: number, y?: number) {
    if (this.#closed) return;
    this.#popupActions.clear();
    nativePopupMenu(this.#id, JSON.stringify(prepareMenu(items, this.#popupActions)), x, y);
  }

  close() {
    if (!this.#closed) nativeClose(this.#id);
  }

  /** Last known state (updated by the UI thread); null before creation or after close. */
  get state(): WindowState | null {
    return this.#closed ? null : JSON.parse(nativeWindowState(this.#id));
  }

  #op(op: string, args?: Record<string, unknown>) {
    if (!this.#closed) nativeWindowOp(this.#id, JSON.stringify({ op, ...args }));
    return this;
  }
  show() { return this.#op("show"); }
  hide() { return this.#op("hide"); }
  minimize() { return this.#op("minimize"); }
  unminimize() { return this.#op("unminimize"); }
  maximize() { return this.#op("maximize"); }
  unmaximize() { return this.#op("unmaximize"); }
  toggleMaximize() { return this.#op("toggleMaximize"); }
  /** Bring to front and focus; also shows and restores the window. */
  focus() { return this.#op("focus"); }
  center() { return this.#op("center"); }
  setSize(width: number, height: number) { return this.#op("setSize", { width, height }); }
  setPosition(x: number, y: number) { return this.#op("setPosition", { x, y }); }
  setMinSize(width: number | null, height: number | null) { return this.#op("setMinSize", { width, height }); }
  setMaxSize(width: number | null, height: number | null) { return this.#op("setMaxSize", { width, height }); }
  setAlwaysOnTop(value: boolean) { return this.#op("setAlwaysOnTop", { value }); }
  setFullscreen(value: boolean) { return this.#op("setFullscreen", { value }); }
  setResizable(value: boolean) { return this.#op("setResizable", { value }); }
  setDecorations(value: boolean) { return this.#op("setDecorations", { value }); }
  /** With true, the close button emits "closerequested" instead of closing (e.g. hide to tray). */
  setPreventClose(value: boolean) { return this.#op("setPreventClose", { value }); }
  /** Move the window with the mouse; call from a mousedown on a custom title bar. */
  startDragging() { return this.#op("startDragging"); }
  /** Flash the taskbar button. */
  requestAttention() { return this.#op("requestAttention"); }
  /** Load a URL; relative paths load from app://. */
  navigate(url: string) { return this.#op("navigate", { url: String(url) }); }
  reload() { return this.#op("reload"); }
  /** Needs the window option `devtools: true` in release builds. */
  openDevtools() { return this.#op("openDevtools"); }
  closeDevtools() { return this.#op("closeDevtools"); }
  /** Page zoom; 1 = 100%. */
  setZoom(factor: number) { return this.#op("setZoom", { factor }); }
  print() { return this.#op("print"); }
  /**
   * Progress on the taskbar button: `progress` 0..1, or null to only change the state.
   * `state`: "normal" | "indeterminate" | "paused" | "error" | "none" (removes it).
   */
  setProgress(progress: number | null, state?: "none" | "normal" | "indeterminate" | "paused" | "error") {
    return this.#op("setProgress", { progress, state: state ?? null });
  }

  /** Evaluate a JavaScript expression in the page and get its (JSON) value. Promises are not awaited. */
  evaluate<T = unknown>(expression: string): Promise<T> {
    if (this.#closed) return Promise.reject(new Error("window is closed"));
    return request<T>(nativeEvaluate(this.#id, String(expression)));
  }

  /** @internal */
  _dispatch(ev: any) {
    switch (ev.type) {
      case "created":
        this.#resolveReady();
        this._fire("created");
        break;
      case "invoke":
        this.#invoke(ev);
        break;
      case "menu": {
        const action = this.#actions.get(ev.id) ?? this.#popupActions.get(ev.id);
        action?.(ev.id);
        this._fire("menu", ev.id);
        break;
      }
      case "window":
        this._fire(ev.kind, ev.data ?? undefined);
        break;
      case "dragdrop":
        this._fire("dragdrop", { kind: ev.kind, paths: ev.paths, x: ev.x, y: ev.y });
        break;
      case "closed":
      case "createfailed":
        this.#closed = true;
        windows.delete(this.#id);
        if (ev.type === "createfailed") {
          const err = new Error(ev.message);
          this.#rejectReady(err);
          if (!this._fire("error", err)) console.error(`[buntauri] window ${JSON.stringify(this.#label)}: ${ev.message}`);
        }
        this._fire("closed");
        break;
      case "error":
        if (!this._fire("error", new Error(ev.message))) console.error(`[buntauri] ${ev.message}`);
        break;
      case "warning":
        if (!this._fire("warning", ev.message)) console.warn(`[buntauri] ${ev.message}`);
        break;
    }
  }

  async #invoke({ call, cmd, args, origin, remote }) {
    const fn = this.#handlers.get(cmd);
    if (!fn) {
      nativeReject(this.#id, call, `unknown command: ${cmd}`);
      return;
    }
    try {
      const value = await fn(args, { window: this, origin, remote });
      if (!this.#closed) nativeResolve(this.#id, call, JSON.stringify(value ?? null));
    } catch (e) {
      if (!this.#closed) nativeReject(this.#id, call, String((e as Error)?.message ?? e));
    }
  }
}

type TrayOptions = {
  icon: IconInput;
  tooltip?: string;
  title?: string;
  menu?: MenuItem[];
  /** Show the menu on left click too (default: right click only). */
  menuOnLeftClick?: boolean;
  visible?: boolean;
};

/**
 * A system tray icon. Keeps the app running until `remove()`, even with no windows.
 * Events: "click" and "doubleclick" ({ button, x, y }), "menu" (item id), "error".
 */
class Tray extends Emitter {
  #id: number;
  #actions = new Map<string, (id: string) => void>();
  #removed = false;

  constructor(options: TrayOptions) {
    super();
    if (options?.icon == null) throw new TypeError("Tray needs an icon (PNG path, Bun.file() or bytes)");
    start();
    this.#id = nativeTrayCreate(JSON.stringify(this.#spec(options)));
    trays.set(this.#id, this);
  }

  get id() {
    return this.#id;
  }
  get removed() {
    return this.#removed;
  }

  #spec(options: Partial<TrayOptions>) {
    const spec: Record<string, unknown> = { ...options };
    if (options.icon != null) spec.icon = iconBase64(options.icon);
    if (options.menu) {
      this.#actions.clear();
      spec.menu = prepareMenu(options.menu, this.#actions);
    }
    return spec;
  }

  /** Change some of the tray's options. */
  update(options: Partial<TrayOptions>) {
    if (!this.#removed) nativeTrayUpdate(this.#id, JSON.stringify(this.#spec(options)));
    return this;
  }
  setIcon(icon: IconInput) {
    return this.update({ icon });
  }
  setTooltip(tooltip: string) {
    return this.update({ tooltip });
  }
  setMenu(menu: MenuItem[]) {
    return this.update({ menu });
  }
  setVisible(visible: boolean) {
    return this.update({ visible });
  }

  remove() {
    if (this.#removed) return;
    this.#removed = true;
    trays.delete(this.#id);
    nativeTrayRemove(this.#id);
  }

  /** @internal */
  _dispatch(ev: any) {
    switch (ev.type) {
      case "tray":
        this._fire(ev.kind, { button: ev.button, x: ev.x, y: ev.y });
        break;
      case "menu":
        this.#actions.get(ev.id)?.(ev.id);
        this._fire("menu", ev.id);
        break;
      case "trayfailed":
        this.#removed = true;
        trays.delete(this.#id);
        if (!this._fire("error", new Error(ev.message))) console.error(`[buntauri] tray: ${ev.message}`);
        break;
    }
  }
}

// ── Dialogs ─────────────────────────────────────────────────────────────────

type FileFilter = { name: string; extensions: string[] };
type DialogBase = {
  title?: string;
  /** Folder to start in. */
  defaultPath?: string;
  filters?: FileFilter[];
  /** Make the dialog modal to this window. */
  window?: Window;
};

function showDialog<T>(spec: Record<string, unknown>, window?: Window): Promise<T> {
  start();
  return request<T>(nativeDialog(window && !window.closed ? window.id : undefined, JSON.stringify(spec)));
}

const dialog = {
  /** Pick a file; `multiple` for several, `directory` for folders. Resolves to a path, paths, or null. */
  open(options: DialogBase & { multiple?: boolean; directory?: boolean } = {}): Promise<string | string[] | null> {
    const { window, ...spec } = options;
    return showDialog({ kind: "open", ...spec }, window);
  },
  /** Choose where to save. Resolves to a path or null. */
  save(options: DialogBase & { fileName?: string } = {}): Promise<string | null> {
    const { window, ...spec } = options;
    return showDialog({ kind: "save", ...spec }, window);
  },
  /** Message box. Resolves to "ok", "cancel", "yes" or "no". */
  message(
    message: string,
    options: { title?: string; level?: "info" | "warning" | "error"; buttons?: "ok" | "okCancel" | "yesNo" | "yesNoCancel"; window?: Window } = {},
  ): Promise<"ok" | "cancel" | "yes" | "no"> {
    const { window, ...spec } = options;
    return showDialog({ kind: "message", message: String(message), ...spec }, window);
  },
  /** OK/Cancel question. Resolves to true for OK. */
  async confirm(message: string, options: { title?: string; level?: "info" | "warning" | "error"; window?: Window } = {}) {
    return (await dialog.message(message, { ...options, buttons: "okCancel" })) === "ok";
  },
  /** Yes/No question. Resolves to true for Yes. */
  async ask(message: string, options: { title?: string; level?: "info" | "warning" | "error"; window?: Window } = {}) {
    return (await dialog.message(message, { ...options, buttons: "yesNo" })) === "yes";
  },
};

// ── Notifications ───────────────────────────────────────────────────────────

/** Desktop notification (a toast on Windows). `appId`: the AppUserModelID shown as sender. */
function notify(options: { title: string; body?: string; appId?: string; icon?: string }): Promise<void> {
  start();
  return request<void>(nativeNotify(JSON.stringify(options)));
}

// ── Monitors ────────────────────────────────────────────────────────────────

type Monitor = { name: string | null; x: number; y: number; width: number; height: number; scaleFactor: number; primary: boolean };

/** The connected monitors, in CSS pixels. */
function monitors(): Promise<Monitor[]> {
  start();
  return request<Monitor[]>(nativeMonitors());
}

// ── Clipboard ───────────────────────────────────────────────────────────────

const clipboard = {
  /** The clipboard's text, or null when it holds no text. */
  readText(): string | null {
    return nativeClipboardRead();
  },
  writeText(text: string): void {
    nativeClipboardWrite(String(text));
  },
};

// ── Global shortcuts ────────────────────────────────────────────────────────

const globalShortcut = {
  /** Fire `handler` for "CmdOrCtrl+Shift+K"-style shortcuts, even when the app has no focus. */
  async register(accelerator: string, handler: (state: "pressed" | "released") => void): Promise<void> {
    start();
    await request<void>(nativeShortcut(accelerator, true));
    shortcutHandlers.set(accelerator, handler);
  },
  async unregister(accelerator: string): Promise<void> {
    start();
    shortcutHandlers.delete(accelerator);
    await request<void>(nativeShortcut(accelerator, false));
  },
  isRegistered(accelerator: string): boolean {
    return shortcutHandlers.has(accelerator);
  },
};

// ── Single instance ─────────────────────────────────────────────────────────

/**
 * Make sure only one copy of the app runs. Resolves to null in a second copy
 * (its arguments have been passed to the first one: quit). In the first copy
 * it resolves to an emitter that fires "second-instance" ({ argv, cwd }).
 * Uses a named pipe (Windows) or a socket in the temp dir; no native code.
 */
async function requestSingleInstance(appId: string) {
  const safe = String(appId).replace(/[^A-Za-z0-9._-]/g, "_");
  const path =
    process.platform === "win32"
      ? `\\\\.\\pipe\\buntauri-${safe}`
      : `${tmpdir()}/buntauri-${safe}.sock`;

  const handOver = () =>
    new Promise<boolean>(resolve => {
      const c = net.createConnection(path, () => {
        c.end(JSON.stringify({ argv: process.argv.slice(2), cwd: process.cwd() }), () => resolve(true));
      });
      c.on("error", () => resolve(false));
    });

  for (let attempt = 0; attempt < 2; attempt++) {
    if (await handOver()) return null;
    if (process.platform !== "win32") {
      try {
        unlinkSync(path); // stale socket of a crashed instance
      } catch {}
    }
    const instance = new Emitter();
    const server = net.createServer(socket => {
      let data = "";
      socket.setEncoding("utf8");
      socket.on("data", chunk => (data += chunk));
      socket.on("end", () => {
        try {
          instance._fire("second-instance", JSON.parse(data));
        } catch {}
      });
    });
    const listening = await new Promise<boolean>(resolve => {
      server.once("error", () => resolve(false));
      server.listen(path, () => resolve(true));
    });
    if (listening) {
      server.unref(); // the app's windows/trays decide how long it lives
      return Object.assign(instance, { release: () => server.close() });
    }
    // Lost a race with another first instance: hand over to it instead.
  }
  return null;
}

// ── The app as a whole ──────────────────────────────────────────────────────

/**
 * App-wide events and quitting.
 * - "reopen" ({ hasVisibleWindows }): macOS, the Dock icon was clicked. Without
 *   a listener, the most recent window is shown when none is visible.
 * - "before-quit" ({ preventDefault() }): Quit from the macOS app menu (Cmd+Q).
 *   Without preventDefault() the app quits.
 */
class App extends Emitter {
  /** Close every window and tray, then exit. */
  quit(code = 0) {
    for (const w of [...windows.values()]) w.close();
    for (const t of [...trays.values()]) t.remove();
    // The UI exits with us (its socket closes); nothing else should keep a desktop app alive.
    process.exit(code);
  }

  /** @internal */
  _dispatch(ev: any) {
    if (ev.kind === "reopen") {
      const data = { hasVisibleWindows: !!ev.data?.hasVisibleWindows };
      if (!this._fire("reopen", data) && !data.hasVisibleWindows) [...windows.values()].at(-1)?.focus();
    } else if (ev.kind === "quitrequested") {
      let prevented = false;
      this._fire("before-quit", { preventDefault: () => (prevented = true) });
      if (!prevented) this.quit();
    }
  }
}

const app = new App();

export default {
  app,
  Window,
  Tray,
  setAssetsDir,
  dialog,
  notify,
  monitors,
  clipboard,
  globalShortcut,
  requestSingleInstance,
};
