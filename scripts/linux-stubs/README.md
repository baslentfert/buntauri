# Linux: GTK/WebKitGTK through lazy-loading stubs (prototype)

Linked normally, GTK and WebKitGTK become NEEDED entries, so the binary does not
start at all where they are missing (`error while loading shared libraries`),
not even for a script without a window. ELF has no delay-load (Windows) or weak
framework (macOS), so this emulates it with [Implib.so](https://github.com/yugr/Implib.so):
link against small stub archives that `dlopen` the real library on first call.

- `gen-stubs.sh <implib-dir> <out-dir>`: generates `lib<name>-stub.a` for each GTK/WebKitGTK library
  (needs `implib-gen.py` at `/implib` and the -dev packages, see `Dockerfile`).
- `cc-wrap.sh`: linker wrapper that replaces `-l<name>` by the stub archive.
  Bun's link would do the same in `scripts/build/buntauri-libs.ts`.
- `buntauri_window` loads WebKitGTK up front (`load_gtk` in lib.rs), so a missing
  library is an error from `UiThread::spawn` (a JS exception in Bun), not an abort.

Tried with the crate demo (aarch64, Ubuntu 24.04, Colima):
`RUSTFLAGS="-C linker=/scripts/cc-wrap.sh" cargo build -p buntauri_window --example demo`

| | normal link | stubs |
|---|---|---|
| NEEDED on GTK/WebKitGTK/GLib | 13 | 0 |
| without GTK | does not start (exit 127) | starts; `UiThread::spawn` returns an error |
| with GTK, xvfb | windows, IPC | same behaviour as normal link |

Not intercepted: exported data symbols (e.g. `g_utf8_skip`, `glib_major_version`);
the crate and its dependencies do not reference them (the link would fail).
Still to test: Bun itself (JavaScriptCore of Bun and of WebKitGTK in one process).
