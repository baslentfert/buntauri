// Package a buntauri app as a signed (and optionally notarized) macOS .app.
//
//   bun scripts/macos-app.ts --entry app.ts --asset assets/index.html \
//     --name "My App" --id com.example.myapp --version 1.0.0 --icon icon.png \
//     [--runtime path/to/buntauri-bun] [--sign "Developer ID Application: …"] \
//     [--notarize <notarytool keychain profile>] [--dmg] [--out dist]
//
// - The runtime is the buntauri build of Bun that `bun build --compile` copies
//   into the app (default: the bun running this script). For distribution use
//   one built for macOS 13 (CI builds are; a local build targets its SDK).
// - --sign: a codesigning identity (`security find-identity -v -p codesigning`);
//   "-" signs ad hoc. Hardened runtime with entitlements.plist next to this file.
// - --notarize: a profile stored once with `xcrun notarytool store-credentials`;
//   no Apple ID, password or key ever passes through this script.
// - --dmg: also make Name-version.dmg (the app + a link to /Applications),
//   signed, and notarized/stapled too with --notarize. The app inside is
//   stapled first, so it also passes Gatekeeper offline once copied out.
import { spawnSync } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { parseArgs } from "node:util";

const { values: opt } = parseArgs({
  options: {
    entry: { type: "string" },
    asset: { type: "string", multiple: true, default: [] },
    name: { type: "string" },
    id: { type: "string" },
    version: { type: "string", default: "0.1.0" },
    icon: { type: "string" },
    runtime: { type: "string", default: process.execPath },
    sign: { type: "string" },
    notarize: { type: "string" },
    dmg: { type: "boolean", default: false },
    out: { type: "string", default: "dist" },
    "min-macos": { type: "string" },
  },
});
if (!opt.entry || !opt.name || !opt.id) {
  console.error("usage: bun scripts/macos-app.ts --entry app.ts --name Name --id com.example.app [--asset …] [--icon icon.png] [--sign …] [--notarize profile]");
  process.exit(2);
}

const run = (cmd: string[], what: string) => {
  const r = spawnSync(cmd[0], cmd.slice(1), { encoding: "utf8" });
  if (r.status !== 0) {
    console.error(`${what} failed: ${cmd.join(" ")}\n${r.stderr || r.stdout}`);
    process.exit(1);
  }
  return r.stdout;
};
const xml = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

// The runtime's own minimum macOS (LC_BUILD_VERSION minos): LaunchServices
// refuses an app whose binary needs a newer macOS than Info.plist promises.
const minos = /minos (\S+)/.exec(run(["otool", "-l", opt.runtime], "otool"))?.[1];
const minMacos = opt["min-macos"] ?? minos ?? "13.0";
if (minos && Number.parseFloat(minos) > Number.parseFloat(minMacos)) {
  console.error(`the runtime needs macOS ${minos}, more than --min-macos ${minMacos}`);
  process.exit(2);
}

const exeName = opt.name.replace(/[^A-Za-z0-9._-]/g, "");
const app = resolve(opt.out, `${opt.name}.app`);
const contents = join(app, "Contents");
rmSync(app, { recursive: true, force: true });
mkdirSync(join(contents, "MacOS"), { recursive: true });
mkdirSync(join(contents, "Resources"), { recursive: true });

// 1. The executable: the app compiled into the buntauri runtime.
const exe = join(contents, "MacOS", exeName);
run([opt.runtime, "build", "--compile", opt.entry, ...opt.asset, "--outfile", exe], "bun build --compile");
console.log(`✓ compiled ${opt.entry} -> ${exe}`);

// 2. Icon: PNG -> .icns (sips + iconutil ship with macOS).
let iconKey = "";
if (opt.icon) {
  const tmp = mkdtempSync(join(tmpdir(), "buntauri-icon-"));
  const set = join(tmp, "AppIcon.iconset");
  mkdirSync(set);
  for (const size of [16, 32, 128, 256, 512]) {
    run(["sips", "-z", `${size}`, `${size}`, opt.icon, "--out", join(set, `icon_${size}x${size}.png`)], "sips");
    run(["sips", "-z", `${size * 2}`, `${size * 2}`, opt.icon, "--out", join(set, `icon_${size}x${size}@2x.png`)], "sips");
  }
  run(["iconutil", "-c", "icns", set, "-o", join(contents, "Resources", "AppIcon.icns")], "iconutil");
  rmSync(tmp, { recursive: true, force: true });
  iconKey = "  <key>CFBundleIconFile</key><string>AppIcon</string>\n";
  console.log("✓ icon");
}

// 3. Info.plist.
writeFileSync(
  join(contents, "Info.plist"),
  `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleName</key><string>${xml(opt.name)}</string>
  <key>CFBundleDisplayName</key><string>${xml(opt.name)}</string>
  <key>CFBundleIdentifier</key><string>${xml(opt.id)}</string>
  <key>CFBundleExecutable</key><string>${xml(exeName)}</string>
  <key>CFBundleShortVersionString</key><string>${xml(opt.version)}</string>
  <key>CFBundleVersion</key><string>${xml(opt.version)}</string>
${iconKey}  <key>LSMinimumSystemVersion</key><string>${xml(minMacos)}</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
`,
);
console.log("✓ Info.plist");

// 4. Sign: the executable, then the bundle (hardened runtime, timestamp).
if (opt.sign) {
  const entitlements = join(import.meta.dir, "macos-entitlements.plist");
  const flags = ["--force", "--options", "runtime", "--entitlements", entitlements, "--sign", opt.sign];
  if (opt.sign !== "-") flags.push("--timestamp");
  run(["codesign", ...flags, exe], "codesign (executable)");
  run(["codesign", ...flags, app], "codesign (app)");
  run(["codesign", "--verify", "--deep", "--strict", "--verbose=2", app], "codesign --verify");
  console.log(`✓ signed (${opt.sign})`);
}

// 5. Notarize and staple: submit, wait, and require "Accepted" (notarytool
// exits 0 for a rejected submission too); on rejection show Apple's log.
const notarize = (path: string, upload: string) => {
  console.log(`… notarizing ${upload.split("/").pop()} (this waits for Apple)`);
  const r = spawnSync("xcrun", ["notarytool", "submit", upload, "--keychain-profile", opt.notarize!, "--wait"], { encoding: "utf8" });
  const out = `${r.stdout}${r.stderr}`;
  const id = /id: ([0-9a-f-]{36})/.exec(out)?.[1];
  if (r.status !== 0 || !/status: Accepted/.test(out)) {
    console.error(out.trim());
    if (id) console.error(run(["xcrun", "notarytool", "log", id, "--keychain-profile", opt.notarize!], "notarytool log"));
    process.exit(1);
  }
  run(["xcrun", "stapler", "staple", path], "stapler");
  console.log(`✓ notarized and stapled (${id})`);
};

if (opt.notarize) {
  if (!opt.sign || opt.sign === "-") {
    console.error("--notarize needs --sign with a Developer ID Application identity");
    process.exit(2);
  }
  const zip = join(resolve(opt.out), `${exeName}-notarize.zip`);
  run(["ditto", "-c", "-k", "--keepParent", app, zip], "ditto");
  notarize(app, zip);
  rmSync(zip, { force: true });
  const gk = spawnSync("spctl", ["--assess", "--type", "execute", "--verbose=2", app], { encoding: "utf8" });
  console.log(`  Gatekeeper: ${(gk.stderr || gk.stdout).trim().split("\n").slice(1).join(", ")}`);
}

// 6. Disk image: the app and a link to /Applications to drag it onto.
if (opt.dmg) {
  const dmg = resolve(opt.out, `${exeName}-${opt.version}.dmg`);
  const stage = mkdtempSync(join(tmpdir(), "buntauri-dmg-"));
  // ditto-like copy that keeps the signature, the stapled ticket and symlinks.
  cpSync(app, join(stage, `${opt.name}.app`), { recursive: true, verbatimSymlinks: true });
  symlinkSync("/Applications", join(stage, "Applications"));
  rmSync(dmg, { force: true });
  run(["hdiutil", "create", "-volname", opt.name, "-srcfolder", stage, "-format", "UDZO", "-ov", dmg], "hdiutil create");
  rmSync(stage, { recursive: true, force: true });
  if (opt.sign) {
    const flags = ["--force", "--sign", opt.sign];
    if (opt.sign !== "-") flags.push("--timestamp");
    run(["codesign", ...flags, dmg], "codesign (dmg)");
  }
  console.log(`✓ ${dmg}`);
  if (opt.notarize) {
    notarize(dmg, dmg);
    const gk = spawnSync("spctl", ["--assess", "--type", "open", "--context", "context:primary-signature", "--verbose=2", dmg], { encoding: "utf8" });
    console.log(`  Gatekeeper: ${(gk.stderr || gk.stdout).trim().split("\n").slice(1).join(", ")}`);
  }
}

console.log(`done: ${app}`);
