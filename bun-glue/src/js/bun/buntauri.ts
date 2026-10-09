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
const nativeTrayCreate = $newRustFunction("buntauri/window.rs", "trayCreate", 1);
const nativeTrayUpdate = $newRustFunction("buntauri/window.rs", "trayUpdate", 2);
const nativeTrayRemove = $newRustFunction("buntauri/window.rs", "trayRemove", 1);

const { readFileSync } = require("node:fs");
const { Buffer } = require("node:buffer");

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

function onNativeEvent(json: string) {
  const ev = JSON.parse(json);
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

/**
 * A native window with a webview.
 * Events: "created", "closed", "menu" (item id), "dragdrop", "error", "warning".
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

export default {
  Window,
  Tray,
  setAssetsDir,
};
