# buntauri

Bun + Tauri's native stack (tao + wry) in one binary. `bun build --compile` on a
buntauri build of Bun produces a single desktop exe (bun2exe comes built in).

## Main rule: touch Bun as little as possible

All logic lives **outside** the Bun tree. Bun only gets glue, so picking up a new
Bun release comes down to: check out the release, run sync, build, run the selftest.

```
buntauri/
  UPSTREAM                  Bun revision this is built and tested against
  crates/window/            ALL window logic: tao + wry on a UI thread, app:// assets,
                            IPC bridge, Tauri-compatible options. Knows nothing about Bun.
  bun-glue/                 NEW files, copied 1:1 into the Bun tree
    src/js/bun/buntauri.ts      the bun:buntauri module (Window class)
    src/runtime/buntauri/       Rust side: host functions -> crates/window
  scripts/sync.ts           copies bun-glue into a Bun checkout and applies the edits
  scripts/build-bun.ps1     builds Bun on Windows (picks the right VS + LLVM)
  examples/bun/             example app for bun:buntauri
```

## Rules for the glue

1. **New files over edits.** Whatever can live in `bun-glue/` goes there.
2. **Edits to existing Bun files are registration only**: one line per spot, no logic,
   applied by `scripts/sync.ts` against text anchors (idempotent, marked `buntauri`):
   | File | What |
   |---|---|
   | `Cargo.toml`, `src/runtime/Cargo.toml` | path dependency on `crates/window` |
   | `src/runtime/lib.rs` | `mod buntauri;` |
   | `src/codegen/generate-js2native.ts` | `buntauri/window.rs` for `$newRustFunction` |
   | `src/event_loop/ConcurrentTask.rs`, `src/runtime/dispatch.rs` | task tag that carries UI-thread events to the JS thread |
   | `src/resolve_builtins/HardcodedModule.rs`, `isBuiltinModule.cpp`, `NodeModuleModule.cpp` | the `bun:buntauri` module name |
   | `scripts/build/binary-expectations.ts`, `scripts/build/flags.ts` | allow + delay-load the Windows UI DLLs |
3. **Type fixes** (`typeFixes` in `sync.ts`): linking wry brings `http`/`serde_json`, whose
   `impl PartialEq<…> for u16/i32` make Bun's `x == CONST as _` ambiguous (E0283). Spelling
   the type out is harmless for upstream Bun; these are candidates for a PR to Bun, after
   which `sync.ts` skips them.
4. **No changes to the event loop.** The UI thread posts concurrent tasks through Bun's
   existing `VmHandle`; each open window holds a keep-alive ref.
5. **No changes to `--compile`.** In a compiled app, `app://` is served zero-copy from the
   existing standalone module graph.

## Picking up a new Bun release

1. Check out the new Bun revision and update `UPSTREAM`.
2. `bun scripts/sync.ts [path/to/bun]`. If an anchor is not found, fix only that anchor.
   It also handles Bun's `Cargo.lock` (Bun builds with `--locked`): for the UPSTREAM revision the
   lock is kept in `bun-glue/Cargo.lock`; after updating UPSTREAM, build Bun once, run sync, commit it.
3. `pwsh scripts/build-bun.ps1` (add `-Target build:release` for a release build).
4. Run the selftest. A new E0283 error (`as _` ambiguous)? Add a line to `typeFixes` in `sync.ts`.

## Threading

- **Windows/Linux:** tao + wry on their own UI thread (`with_any_thread`, STA for WebView2).
  Bun's JS thread stays free; commands go through `EventLoopProxy`, events come back as
  concurrent tasks.
- **macOS:** AppKit requires the main thread, so a host subprocess (same exe, like
  `Bun.WebView` does). Later.

## Status

- [x] `crates/window`: UI thread, windows, `app://` assets, invoke/resolve/reject, events,
      IPC only from `app://` or allowed URL patterns, Tauri-compatible window options.
      Test: `cargo run --example demo` (`BUNTAURI_SELFTEST=1` for the automated test).
- [x] Phase 0: build Bun on Windows: `pwsh scripts/build-bun.ps1`
      (VS 2022 17.14 / MSVC 14.44 with ATL, LLVM 23.1.1 via scoop, Perl, NASM).
- [x] Phase 1: `bun:buntauri` in Bun via `bun scripts/sync.ts` (bun-glue + anchored edits + type fixes).
      Test: `BUNTAURI_SELFTEST=1 bun-debug examples/bun/hello.ts`.
- [x] Phase 2a: assets from the `--compile` exe (zero-copy from the graph; bunfs URLs in
      HTML/CSS/JS rewritten to `/`). One exe:
      `bun build --compile --windows-hide-console examples/bun/hello.ts examples/bun/assets/index.html`.
- [x] Release build: Windows UI DLLs allowed + delay-loaded. `bun.exe` and a compiled app are ~97 MB.
- [x] Examples: `examples/dashboard` (React 19 system dashboard), `examples/game` (PixiJS 8 game).
- [ ] Submit the type fixes to Bun (PR), so they drop out of `sync.ts`.

## TODO: native features (what Tauri has and Bun does not)

Bun already covers most Tauri plugins: fs (`node:fs`, `Bun.file`), http (`fetch`), shell/process
(`Bun.spawn`, `Bun.$`), os (`node:os`), sql/store (`bun:sqlite`, `Bun.SQL`), websocket, crypto,
opening URLs/files (`Bun.spawn(["cmd", "/c", "start", "", url])`; note `Bun.$` has no `start`),
autostart (registry via `reg.exe`), clipboard *images* (`Bun.Image.fromClipboard`).
What is left is the native shell around the window:

- [x] Window icon (tao): `icon` option and `win.setIcon()`
- [x] Tray icon with menu and click events (`tray-icon` 0.26): `new Tray({...})`
- [x] Menus: window menu bar and context menus (`muda` 0.21), accelerators on Windows
- [x] Window control at runtime: minimize, maximize, show/hide, size, position, focus,
      always-on-top, fullscreen, plus resize/move/focus events and `preventClose` (tao)
- [x] Native dialogs: open/save file, folder picker, message box (`rfd`): `dialog.*`
- [x] Notifications (Windows toasts, `notify-rust`): `notify()`
- [x] Global shortcuts (`global-hotkey`): `globalShortcut.register()`
- [x] Clipboard text (`arboard`): `clipboard.readText()/writeText()`
- [x] Single instance: `requestSingleInstance()`, a named pipe / socket in TS, no native code
- [ ] Deep links / custom URL scheme (registry)
- [ ] `@tauri-apps/api` compatibility layer (`invoke`, events)
- [ ] CSP headers on `app://`
- [ ] `buntauri build`: one command for compile + icon + manifest (DPI awareness) + signing
- [ ] Code signing (Azure Artifact Signing, configurable per app)
- [ ] Updater (maybe; single exe makes it simpler)
- [ ] macOS: Bun + buntauri **builds, links and starts** in CI (macOS 14, Apple Silicon, release;
      `bun:buntauri` loads). Windows cannot open yet: AppKit needs the process main thread, so the
      UI host subprocess comes next (mac-buntauri, branch `macos`). Every bun start on macOS loads
      AppKit/WebKit (no delay-load on macOS); revisit with the UI host design.
- [ ] Linux: builds in CI up to the final link; link still fails (being diagnosed).
- [ ] CI (GitHub Actions): `build-macos.yml` (manual, macOS arm64) is the first step; build buntauri per platform, publish base executables; sign + notarize
      macOS (Developer ID Application, App Store Connect API key as secrets)
- [ ] App manifest (comctl32 v6 + DPI awareness); then re-enable muda `common-controls-v6`
