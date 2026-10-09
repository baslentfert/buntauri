// buntauri: system libraries the window crate (tao, wry, muda, tray-icon)
// needs when Bun links its executable.
// Copied from the buntauri repo (bun-glue/); edit it there.
//
// Bun links the Rust rlibs itself instead of letting rustc do it, so dynamic
// libraries and frameworks that crates name (#[link] attributes, build
// scripts) never reach the link. Static ones are bundled into the rlibs, which
// is why Windows needs nothing here. This list fills the gap per OS.
import { spawnSync } from "node:child_process";
import type { Config } from "./config.ts";

/** Linker arguments to append to Bun's system libs. */
export function buntauriLinkLibs(cfg: Config): string[] {
  if (cfg.darwin) {
    // AppKit/Foundation: windows and menus; CoreFoundation/CoreGraphics: tao;
    // Carbon: keyboard layouts (TIS*, UCKeyTranslate); WebKit: WKWebView
    // (looked up by name at run time, so it must be linked even though no
    // symbol references it); objc: the Objective-C runtime.
    const frameworks = ["AppKit", "Foundation", "CoreFoundation", "CoreGraphics", "Carbon", "WebKit"];
    return [...frameworks.map(f => `-Wl,-framework,${f}`), "-lobjc"];
  }
  if ((cfg.linux && cfg.abi !== "android") || cfg.freebsd) {
    // GTK 3 for tao/muda, WebKitGTK for wry. Their Requires pull in glib, gio,
    // gdk, cairo, pango, libsoup and javascriptcoregtk. The tray's
    // libappindicator is loaded at run time (dlopen), so it is not listed.
    const pkgs = ["gtk+-3.0", "webkit2gtk-4.1"];
    const r = spawnSync("pkg-config", ["--libs", ...pkgs], { encoding: "utf8" });
    if (r.status !== 0) {
      throw new Error(
        `buntauri: \`pkg-config --libs ${pkgs.join(" ")}\` failed.\n` +
          "Install the dev packages, e.g. on Debian/Ubuntu: libgtk-3-dev libwebkit2gtk-4.1-dev\n" +
          (r.stderr ?? ""),
      );
    }
    return r.stdout.trim().split(/\s+/).filter(Boolean);
  }
  return [];
}
