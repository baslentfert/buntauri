// bun:buntauri - native windows (tao + wry) for Bun.
// Copied from the buntauri repo (bun-glue/); edit it there.
//
//   import { Window } from "bun:buntauri";
//   const win = new Window({ title: "Hello", url: "index.html" });
//   win.handle("greet", ({ name }) => `Hello ${name}`);
//
// Options use the names of Tauri's WindowConfig (tauri.conf.json).
// Pages call `await window.__BUNTAURI__.invoke(cmd, args)`.

const nativeInit = $newRustFunction("buntauri/window.rs", "init", 2);
const nativeCreate = $newRustFunction("buntauri/window.rs", "createWindow", 1);
const nativeEval = $newRustFunction("buntauri/window.rs", "evalScript", 2);
const nativeSetTitle = $newRustFunction("buntauri/window.rs", "setTitle", 2);
const nativeClose = $newRustFunction("buntauri/window.rs", "close", 1);
const nativeResolve = $newRustFunction("buntauri/window.rs", "resolve", 3);
const nativeReject = $newRustFunction("buntauri/window.rs", "reject", 3);
const nativeEmit = $newRustFunction("buntauri/window.rs", "emit", 3);

const windows = new Map<number, Window>();
let started = false;
let assetsDir: string | undefined;

/** Serve `app://` from this directory. Call before creating the first window. */
function setAssetsDir(dir: string) {
  if (started) throw new Error("setAssetsDir() must be called before the first Window is created");
  assetsDir = dir;
}

function start() {
  if (started) return;
  started = true;
  nativeInit(onNativeEvent, assetsDir);
}

function onNativeEvent(json: string) {
  const ev = JSON.parse(json);
  const win = ev.window != null ? windows.get(ev.window) : undefined;
  if (!win) {
    if (ev.type === "error" || ev.type === "warning") console.warn(`[buntauri] ${ev.message}`);
    return;
  }
  win._dispatch(ev);
}

type Handler = (args: any, info: { window: Window; origin: string; remote: boolean }) => any;
type Listener = (payload: any) => void;

class Window {
  #id: number;
  #label: string;
  #handlers = new Map<string, Handler>();
  #listeners = new Map<string, Set<Listener>>();
  #closed = false;
  #ready: Promise<void>;
  #resolveReady!: () => void;
  #rejectReady!: (e: Error) => void;

  constructor(options: Record<string, any> = {}) {
    start();
    this.#ready = new Promise((resolve, reject) => {
      this.#resolveReady = resolve;
      this.#rejectReady = reject;
    });
    // Created windows report errors through "error"; don't make an unhandled rejection of it.
    this.#ready.catch(() => {});
    this.#label = options.label ?? "main";
    this.#id = nativeCreate(JSON.stringify(options));
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

  /** Listen to window events: "created", "closed", "dragdrop", "error", "warning". */
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

  close() {
    if (!this.#closed) nativeClose(this.#id);
  }

  #fire(event: string, payload: any) {
    const set = this.#listeners.get(event);
    if (!set) return false;
    for (const fn of set) fn(payload);
    return set.size > 0;
  }

  /** @internal */
  _dispatch(ev: any) {
    switch (ev.type) {
      case "created":
        this.#resolveReady();
        this.#fire("created", undefined);
        break;
      case "invoke":
        this.#invoke(ev);
        break;
      case "dragdrop":
        this.#fire("dragdrop", { kind: ev.kind, paths: ev.paths, x: ev.x, y: ev.y });
        break;
      case "closed":
      case "createfailed":
        this.#closed = true;
        windows.delete(this.#id);
        if (ev.type === "createfailed") {
          const err = new Error(ev.message);
          this.#rejectReady(err);
          if (!this.#fire("error", err)) console.error(`[buntauri] window ${JSON.stringify(this.#label)}: ${ev.message}`);
        }
        this.#fire("closed", undefined);
        break;
      case "error":
        if (!this.#fire("error", new Error(ev.message))) console.error(`[buntauri] ${ev.message}`);
        break;
      case "warning":
        if (!this.#fire("warning", ev.message)) console.warn(`[buntauri] ${ev.message}`);
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

export default {
  Window,
  setAssetsDir,
};
