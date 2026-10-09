# buntauri

Bun + Tauri's native stack (tao + wry) in één binary. `buntauri build --compile app.ts`
levert één desktop-exe op (bun2exe zit er dus standaard in).

## Hoofdregel: Bun zo min mogelijk aanraken

Alle logica leeft **buiten** de Bun-tree. In Bun komt alleen lijm, zodat een nieuwe
Bun-release overnemen neerkomt op: tag ophogen, sync draaien, bouwen, selftest.

```
buntauri/
  UPSTREAM                  Bun-release waarop we bouwen (bv. bun-v1.4.2)
  crates/window/            ALLE window-logica: tao + wry op een UI-thread,
                            app:// assets, IPC-bridge. Kent Bun niet.
  bun-glue/                 NIEUWE bestanden die 1-op-1 in de Bun-tree gekopieerd worden
    src/runtime/buntauri/   Window.classes.ts, Window.rs (JS-binding -> crates/window)
  patches/                  kleine edits op BESTAANDE Bun-bestanden (alleen registratie)
  scripts/sync.ts           kopieer bun-glue naar een Bun-checkout en pas de edits toe
  scripts/build-bun.ps1     Bun bouwen op Windows (kiest de juiste VS + LLVM)
  examples/bun/             voorbeeld-app voor bun:buntauri
```

## Regels voor de lijm

1. **Nieuwe bestanden boven edits.** Alles wat kan, komt in `bun-glue/`, nooit in `patches/`.
2. **Edits zijn registratieregels:** één regel per plek, geen logica. Verwachte plekken:
   | Bestand | Wat |
   |---|---|
   | `Cargo.toml` (root) | workspace-member + path-dep naar `crates/window` |
   | `src/runtime/lib.rs` | `mod buntauri;` |
   | `src/jsc/bindings/BunObject+exports.h` | `macro(Window)` |
   | `src/runtime/api/BunObject.rs` | lazy property `Window` |
   | `scripts/build/binary-expectations.ts` | extra DLL-imports toestaan |
   | `scripts/build/flags.ts` | `/delayload` voor die DLL's |
3. **Achter een cargo-feature `buntauri`.** Zonder feature is de build identiek aan upstream.
4. **Geen edits in de event loop.** De UI-thread gebruikt Bun's bestaande
   concurrent task queue + `EventLoop::wakeup()` om terug te praten.
5. **Types apart:** `buntauri.d.ts` naast `bun-types`, niet erin.
6. **`--compile` niet aanpassen:** de buntauri-binary wordt de basis via
   `--compile-executable-path`; assets komen uit de bestaande standalone module graph.

## Nieuwe Bun-release overnemen

1. `UPSTREAM` aanpassen naar de nieuwe tag.
2. `bun scripts/sync.ts [pad/naar/bun]` – faalt een anker, dan alleen dat anker herstellen.
3. `pwsh scripts/build-bun.ps1`.
4. Selftest draaien. Nieuwe E0283-fout (`as _` dubbelzinnig)? Regel toevoegen aan `typeFixes` in `sync.ts`.

## Threading

- **Windows/Linux:** tao + wry op een eigen UI-thread (`with_any_thread`, STA voor WebView2).
  Bun's JS-thread blijft vrij; commando's gaan via `EventLoopProxy`, events terug via callback.
- **macOS:** AppKit eist de main thread -> host-subproces (zelfde exe, zoals `Bun.WebView` doet). Later.

## Status

- [x] `crates/window`: UI-thread, vensters, `app://` assets, invoke/resolve/reject, events,
      IPC alleen vanaf `app://` of toegestane URL-patronen, Tauri-compatibele vensteropties.
      Test: `cargo run --example demo` (`BUNTAURI_SELFTEST=1` voor de automatische test).
- [x] Fase 0: Bun bouwen op Windows: `pwsh scripts/build-bun.ps1`
      (VS 2022 17.14 / MSVC 14.44 met ATL, LLVM 23.1.1 via scoop, Perl, NASM).
- [x] Fase 1: `bun:buntauri` in Bun via `bun scripts/sync.ts` (bun-glue + ankerpunt-edits + typefixes).
      Test: `BUNTAURI_SELFTEST=1 bun-debug examples/bun/hello.ts`.
- [ ] Release-build: 9 extra Windows-DLL's toestaan + delay-loaden (`binary-expectations.ts`, `flags.ts`).
- [ ] Typefixes upstream bij Bun indienen (PR), dan verdwijnen ze uit `sync.ts`.
- [ ] Fase 2: assets uit de `--compile`-graph, `@tauri-apps/api`-compat (`invoke`).
- [ ] Fase 3: `buntauri build`, venster-icoon, tray (tray-icon), menu's (muda), manifest, signing
      (Azure Artifact Signing, instelbaar per app).
- [ ] Fase 4: macOS (host-subproces), Linux.
