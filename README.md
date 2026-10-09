# buntauri

Bun + Tauri's native stack ([tao](https://github.com/tauri-apps/tao) + [wry](https://github.com/tauri-apps/wry)) in a single binary.
Goal: `bun build --compile` produces one desktop `.exe`, like Wails, with the frontend embedded and a JS backend.

> **Unofficial.** buntauri is an independent project. It is not affiliated with, endorsed by or sponsored by Oven (Bun) or The Tauri Programme within The Commons Conservancy. "Bun" and "Tauri" are used only to describe what this project builds on.

> Early stage. The window layer works standalone; Bun integration is next.

## Try the window layer

```sh
cargo run --example demo                      # opens a window, click around
BUNTAURI_SELFTEST=1 cargo run --example demo  # automated round-trip test
```

The demo's main thread stands in for Bun's JS thread; tao + wry run on a separate UI thread.
Pages are served from `app://` (no port) and talk to the host with the snippet below. Only pages from `app://` (or inline HTML set by the host) may call `invoke`; remote pages are blocked unless `allow_remote_ipc` is set.


```js
const { invoke, listen } = window.__BUNTAURI__;
await invoke("greet", { name: "world" });
listen("tick", (n) => console.log(n));
```

## Design

Everything lives outside the Bun tree so new Bun releases can be picked up with a small patch set.
See [BUNTAURI.md](BUNTAURI.md) (Dutch) for the layout, rules and roadmap.

## License

MIT. Depends on tao (Apache-2.0) and wry (MIT/Apache-2.0).
