# buntauri

Bun + Tauri's native stack ([tao](https://github.com/tauri-apps/tao) + [wry](https://github.com/tauri-apps/wry)) in a single binary.
Goal: `bun build --compile` produces one desktop `.exe`, like Wails, with the frontend embedded and a JS backend.

> **Unofficial.** buntauri is an independent project. It is not affiliated with, endorsed by or sponsored by Oven (Bun) or The Tauri Programme within The Commons Conservancy. "Bun" and "Tauri" are used only to describe what this project builds on.

> Early stage. Works on Windows in a self-built Bun: `import { Window } from "bun:buntauri"`.

## Use it from Bun

```ts
import { Window, setAssetsDir } from "bun:buntauri";

setAssetsDir(import.meta.dir + "/assets");          // served as app://
const win = new Window({ title: "Hello", url: "index.html", width: 800, height: 600, center: true });
win.handle("greet", ({ name }) => `Hello ${name}`); // page: await __BUNTAURI__.invoke("greet", { name })
win.on("closed", () => console.log("bye"));          // Bun exits when the last window closes
```

One exe (frontend + Bun backend, nothing next to it):

```sh
bun build --compile --windows-hide-console app.ts assets/index.html --outfile MyApp.exe
```

`--windows-hide-console` makes it a GUI app: no console window next to yours.

`setAssetsDir(import.meta.dir + "/assets")` works both with `bun run` (disk) and inside the exe (served zero-copy from the executable).

Building Bun with buntauri (Windows): `bun scripts/sync.ts ../bun` then `pwsh scripts/build-bun.ps1`.
See [examples/bun](examples/bun) and [BUNTAURI.md](BUNTAURI.md).

## Try the window layer

```sh
cargo run --example demo                      # opens a window, click around
BUNTAURI_SELFTEST=1 cargo run --example demo  # automated round-trip test
```

The demo's main thread stands in for Bun's JS thread; tao + wry run on a separate UI thread.
Pages are served from `app://` (no port) and talk to the host with the snippet below. Only pages from `app://` (or inline HTML set by the host) may call `invoke`; remote pages are blocked unless the host allows them, like Tauri's capability `remote.urls`:

```rust
WindowOptions {
    url: Some("https://example.com".into()),
    ipc: IpcPolicy {
        remote: vec!["https://example.com".into(), "https://*.example.com".into()],
        remote_commands: Some(vec!["greet".into()]), // None = all commands
    },
    ..Default::default()
}
```

The policy is set by the host, never by the page. Each `Invoke` event carries the page `origin` and a `remote` flag, so the host can also decide per command.


```js
const { invoke, listen } = window.__BUNTAURI__;
await invoke("greet", { name: "world" });
listen("tick", (n) => console.log(n));
```

## Window options

Same names and defaults as Tauri's `WindowConfig` (`tauri.conf.json`), so a Tauri window entry can be passed as-is. See [docs/window-options.md](docs/window-options.md).

## Design

Everything lives outside the Bun tree so new Bun releases can be picked up with a small patch set.
See [BUNTAURI.md](BUNTAURI.md) for the layout, rules and roadmap.

## License

MIT. Depends on tao (Apache-2.0) and wry (MIT/Apache-2.0).
