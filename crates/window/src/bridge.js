// Injected into every buntauri page before page scripts run.
(() => {
  if (window.__BUNTAURI__) return;
  const pending = new Map();
  const listeners = new Map();
  let seq = 0;

  const api = {
    invoke(cmd, args) {
      const id = ++seq;
      return new Promise((resolve, reject) => {
        pending.set(id, { resolve, reject });
        window.ipc.postMessage(JSON.stringify({ id, cmd, args: args ?? null }));
      });
    },
    listen(name, fn) {
      if (!listeners.has(name)) listeners.set(name, new Set());
      listeners.get(name).add(fn);
      return () => listeners.get(name)?.delete(fn);
    },
    __settle(id, ok, value) {
      const p = pending.get(id);
      if (!p) return;
      pending.delete(id);
      ok ? p.resolve(value) : p.reject(value);
    },
    __emit(name, payload) {
      for (const fn of listeners.get(name) ?? []) {
        try { fn(payload); } catch (e) { console.error(e); }
      }
    },
  };

  Object.defineProperty(window, "__BUNTAURI__", { value: Object.freeze(api) });
})();
