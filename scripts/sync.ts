// Put buntauri into a Bun checkout.
//
//   bun scripts/sync.ts [path/to/bun]        (default: ../bun next to this repo)
//
// 1. Copies bun-glue/** into the Bun tree (new files only, owned by us).
// 2. Applies small anchored edits to existing Bun files. Each edit is
//    idempotent: lines we add carry a "buntauri" marker and are skipped when
//    present. If an anchor is missing (Bun changed), the script stops and says
//    which file and anchor to fix, nothing else is guessed.
//
// Run it again after updating Bun to a new release (see UPSTREAM).

import { cpSync, existsSync, readFileSync, writeFileSync } from "node:fs";
import { join, relative, resolve } from "node:path";

const repo = resolve(import.meta.dir, "..");
const bun = resolve(process.argv[2] ?? join(repo, "..", "bun"));
if (!existsSync(join(bun, "src", "runtime", "lib.rs"))) {
  console.error(`Not a Bun checkout: ${bun}`);
  process.exit(1);
}

const MARK = "buntauri";
// Path from Bun's root Cargo.toml to our crate, with forward slashes.
const crateDir = relative(bun, join(repo, "crates", "window")).replaceAll("\\", "/");

// System DLLs that tao/wry/WebView2 add to bun.exe's imports (all present on Windows 10+).
const WINDOWS_UI_DLLS = [
  "combase.dll",
  "rpcrt4.dll",
  "comctl32.dll",
  "gdi32.dll",
  "dwmapi.dll",
  "api-ms-win-core-winrt-error-l1-1-0.dll",
  "imm32.dll",
  "propsys.dll",
  "shlwapi.dll",
];

type Edit = {
  file: string;
  /** Text that must exist; the insertion goes right after (or before) it. */
  anchor: string | RegExp;
  insert?: string;
  before?: boolean;
  /** Instead of inserting: rewrite the anchor match. */
  replace?: (match: string) => string;
  /** Skip when this text is already present (default: the insert itself). */
  present?: string;
};

const edits: Edit[] = [
  // js2native: map the $newRustFunction("buntauri/window.rs", ...) key to the file.
  {
    file: "src/codegen/generate-js2native.ts",
    anchor: `  "bun.rs": "bun.rs",\n`,
    insert: `  "buntauri/window.rs": "runtime/buntauri/window.rs", // ${MARK}\n`,
  },
  // Rust module.
  {
    file: "src/runtime/lib.rs",
    anchor: "pub(crate) mod webview;\n",
    insert: `pub(crate) mod buntauri; // ${MARK}\n`,
  },
  // Crate dependency.
  {
    file: "Cargo.toml",
    anchor: "[workspace.dependencies]\n",
    insert: `buntauri_window = { path = "${crateDir}" } # ${MARK}\n`,
    present: "buntauri_window = {",
  },
  {
    file: "src/runtime/Cargo.toml",
    anchor: "[dependencies]\n",
    insert: `buntauri_window.workspace = true # ${MARK}\n`,
  },
  // Event-loop task carrying UI-thread events to the JS thread.
  {
    file: "src/event_loop/ConcurrentTask.rs",
    anchor: /^ *AsyncModule,\n/m,
    insert: `        BuntauriWindowEvent, // ${MARK}\n`,
  },
  {
    file: "src/runtime/dispatch.rs",
    anchor: /^ *task_tag::CppTask => \{\n *cast!\(CppTask\)\.run\(global\)\?;/m,
    before: true,
    insert:
      `        task_tag::BuntauriWindowEvent => { // ${MARK}\n` +
      `            // SAFETY: boxed in buntauri::window::post; the arm consumes it.\n` +
      `            unsafe { bun_core::heap::take(cast_ptr!(crate::buntauri::window::WindowEvent)) }\n` +
      `                .deliver(global)?;\n` +
      `        }\n`,
    present: "task_tag::BuntauriWindowEvent => { //",
  },
  {
    file: "src/runtime/dispatch.rs",
    anchor: /^ *task_tag::ChromePipeEvent => \{\n *#\[cfg\(windows\)\]\n *release!/m,
    before: true,
    insert: `        task_tag::BuntauriWindowEvent => release!(crate::buntauri::window::WindowEvent), // ${MARK}\n`,
  },
  {
    file: "src/runtime/dispatch.rs",
    anchor: /task_tag::COUNT == (\d+),/,
    replace: m => m.replace(/\d+/, n => String(Number(n) + 1)) + ` // ${MARK}: +1`,
    present: `// ${MARK}: +1`,
  },
  // `bun:buntauri` module name (served from src/js/bun/buntauri.ts).
  {
    file: "src/resolve_builtins/HardcodedModule.rs",
    anchor: `    #[strum(serialize = "bun:ffi")]\n    BunFfi,\n`,
    insert: `    #[strum(serialize = "bun:buntauri")] // ${MARK}\n    BunBuntauri,\n`,
  },
  {
    file: "src/resolve_builtins/HardcodedModule.rs",
    anchor: `        b"bun:ffi" => HardcodedModule::BunFfi,\n`,
    insert: `        b"bun:buntauri" => HardcodedModule::BunBuntauri, // ${MARK}\n`,
  },
  {
    file: "src/resolve_builtins/HardcodedModule.rs",
    anchor: `    entry!("bun:ffi"),\n`,
    insert: `    entry!("bun:buntauri"), // ${MARK}\n`,
  },
  {
    file: "src/jsc/bindings/isBuiltinModule.cpp",
    anchor: `    "bun:ffi"_s,\n`,
    insert: `    "bun:buntauri"_s, // ${MARK}\n`,
  },
  {
    file: "src/jsc/modules/NodeModuleModule.cpp",
    anchor: `    "bun:ffi"_s,\n`,
    insert: `    "bun:buntauri"_s, // ${MARK}\n`,
  },
  // macOS/Linux: frameworks and shared libraries tao/wry need at link time
  // (bun-glue/scripts/build/buntauri-libs.ts); Bun only links its own list.
  {
    file: "scripts/build/bun.ts",
    anchor: `import { streamPath } from "./stream.ts";\n`,
    insert: `import { buntauriLinkLibs } from "./buntauri-libs.ts"; // ${MARK}\n`,
  },
  {
    file: "scripts/build/bun.ts",
    anchor: /^ {2}return libs;\n\}/m,
    before: true,
    insert: `  libs.push(...buntauriLinkLibs(cfg)); // ${MARK}\n`,
  },
  // Windows: the system DLLs tao/wry import. Allowed in the binary check and
  // delay-loaded, so they are only mapped once a window is opened.
  {
    file: "scripts/build/binary-expectations.ts",
    anchor: /"ole32\.dll",\n\s*\],\n\s*exact: true,[\s\S]*?allowed: \[\.\.\.sanitizerLibs,/,
    replace: m => m + ` ...${JSON.stringify(WINDOWS_UI_DLLS)} /* ${MARK} */,`,
    present: `/* ${MARK} */`,
  },
  {
    file: "scripts/build/flags.ts",
    anchor: `      "/delayload:USERENV.dll",\n`,
    insert: WINDOWS_UI_DLLS.map(d => `      "/delayload:${d}", // ${MARK}\n`).join(""),
    present: `"/delayload:${WINDOWS_UI_DLLS[0]}", // ${MARK}`,
  },
];

// Type-inference fixes. Linking tao/wry/serde_json brings crates (http,
// serde_json) whose `impl PartialEq<Their> for u16/i32` make Bun's
// `x == CONST as _` ambiguous (E0283). Spelling the type out is the fix; it
// changes nothing for upstream Bun, so these are candidates for a PR to Bun.
// A pattern that no longer occurs (fixed upstream / code moved) is skipped.
// Each is [file, from, to], replacing every occurrence.
const typeFixes: [string, string, string][] = [
  ["src/runtime/dns_jsc/options_jsc.rs", "== super::netc::AF_INET as _", "== super::netc::AF_INET as i32"],
  ["src/runtime/dns_jsc/options_jsc.rs", "== super::netc::AF_INET6 as _", "== super::netc::AF_INET6 as i32"],
  ["src/runtime/node/node_fs.rs", "errno == E::EEXIST as _", "errno == E::EEXIST as u16"],
  ["src/runtime/node/win_watcher.rs", "errno == sys::E::NOENT as _", "errno == sys::E::NOENT as u16"],
];

// 1. New files.
cpSync(join(repo, "bun-glue"), bun, { recursive: true, filter: src => !src.endsWith("Cargo.lock") });
console.log(`copied bun-glue/ -> ${bun}`);

// 2. Anchored edits.
let changed = 0;
let failed = 0;
for (const e of edits) {
  const path = join(bun, e.file);
  const src = readFileSync(path, "utf8");
  const present = e.present ?? e.insert!;
  if (src.includes(present)) continue;

  const m = typeof e.anchor === "string" ? (src.includes(e.anchor) ? { index: src.indexOf(e.anchor), 0: e.anchor } : null) : e.anchor.exec(src);
  if (!m) {
    console.error(`✗ ${e.file}: anchor not found: ${String(e.anchor).slice(0, 80)}`);
    failed++;
    continue;
  }
  const start = m.index!;
  const end = start + m[0].length;
  const out = e.replace
    ? src.slice(0, start) + e.replace(m[0]) + src.slice(end)
    : e.before
      ? src.slice(0, start) + e.insert + src.slice(start)
      : src.slice(0, end) + e.insert + src.slice(end);
  writeFileSync(path, out);
  console.log(`✓ ${e.file}`);
  changed++;
}

for (const [file, from, to] of typeFixes) {
  const path = join(bun, file);
  const src = readFileSync(path, "utf8");
  if (!src.includes(from)) continue;
  const n = src.split(from).length - 1;
  writeFileSync(path, src.replaceAll(from, to));
  console.log(`✓ ${file} (type fix ×${n})`);
  changed++;
}

if (failed) {
  console.error(`${failed} edit(s) failed. Fix the anchors in scripts/sync.ts for this Bun version.`);
  process.exit(1);
}
console.log(changed ? `${changed} edit(s) applied.` : "Already in sync.");

// Cargo.lock. Bun builds with `cargo --locked`, so its lockfile must list our
// crates. The lock for the UPSTREAM revision is kept in this repo
// (bun-glue/Cargo.lock): CI just uses it, no resolving needed (resolving needs
// Bun's vendored path deps, which only Bun's build fetches).
const upstream = readFileSync(join(repo, "UPSTREAM"), "utf8").split("\n")[0].trim();
const head = Bun.spawnSync(["git", "rev-parse", "HEAD"], { cwd: bun }).stdout.toString().trim();
const savedLock = join(repo, "bun-glue", "Cargo.lock");
const bunLock = join(bun, "Cargo.lock");
if (head === upstream && existsSync(savedLock)) {
  writeFileSync(bunLock, readFileSync(savedLock));
}

// Where Bun's vendored deps are present (a checkout that has been built),
// resolve for real: adds whatever crates/window needs, never changes versions
// Bun already pins. Then save it back so it can be committed.
const cargoToml = readFileSync(join(bun, "Cargo.toml"), "utf8");
const vendored = [...cargoToml.matchAll(/path = "vendor\/([^"]+)"/g)].map(m => m[1]);
const canResolve = vendored.every(dir => existsSync(join(bun, "vendor", dir, "Cargo.toml")));
if (canResolve) {
  const meta = Bun.spawnSync(["cargo", "metadata", "--format-version", "1"], { cwd: bun, stdout: "ignore", stderr: "pipe" });
  if (meta.exitCode !== 0) {
    console.error(`cargo metadata failed:\n${meta.stderr.toString()}`);
    process.exit(1);
  }
  if (head === upstream) {
    const lock = readFileSync(bunLock);
    if (!existsSync(savedLock) || !lock.equals(readFileSync(savedLock))) {
      writeFileSync(savedLock, lock);
      console.log("Cargo.lock updated; commit bun-glue/Cargo.lock.");
    } else console.log("Cargo.lock up to date.");
  } else {
    console.log(`Cargo.lock resolved for ${head.slice(0, 9)} (not UPSTREAM ${upstream.slice(0, 9)}; bun-glue/Cargo.lock left alone).`);
  }
} else if (head === upstream && existsSync(savedLock)) {
  console.log("Cargo.lock taken from bun-glue/ (UPSTREAM revision).");
} else {
  console.error(
    `Cannot update Cargo.lock: Bun is not at UPSTREAM (${upstream.slice(0, 9)}) and its vendored deps (${vendored.join(", ")}) are not fetched yet.\n` +
      "Build Bun once without buntauri (or check out UPSTREAM), then run sync again.",
  );
  process.exit(1);
}
