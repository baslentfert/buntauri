// Bun Catcher: a PixiJS game in a buntauri window. Bun keeps the high score.
//
//   bun-debug app.ts                                   (dev: bundles assets/ first)
//   bun-debug build --compile --windows-hide-console app.ts assets/index.html --outfile BunCatcher.exe
import { Window, setAssetsDir } from "bun:buntauri";
import { mkdirSync } from "node:fs";
import os from "node:os";
import { join } from "node:path";

const compiled = import.meta.dir.includes("~BUN") || import.meta.dir.startsWith("/$bunfs");
let assets = import.meta.dir + "/assets";
if (!compiled) {
  const out = import.meta.dir + "/.dist";
  const result = await Bun.build({ entrypoints: [assets + "/index.html"], outdir: out });
  if (!result.success) {
    console.error(result.logs.join("\n"));
    process.exit(1);
  }
  assets = out;
}
setAssetsDir(assets);

// High score lives in the user's app data, not next to the exe.
const dataDir = join(process.env.APPDATA ?? os.homedir(), "buntauri-bun-catcher");
const scoreFile = join(dataDir, "highscore.json");

async function readBest(): Promise<number> {
  try {
    return (await Bun.file(scoreFile).json()).best ?? 0;
  } catch {
    return 0;
  }
}

const win = new Window({
  title: "Bun Catcher",
  url: "index.html",
  width: 900,
  height: 700,
  minWidth: 480,
  minHeight: 480,
  center: true,
  theme: "dark",
  backgroundColor: "#0b0820",
});

win.handle("best", readBest);
win.handle("submit", async ({ score }: { score: number }) => {
  const best = await readBest();
  const isNew = score > best;
  if (isNew) {
    mkdirSync(dataDir, { recursive: true });
    await Bun.write(scoreFile, JSON.stringify({ best: score, at: new Date().toISOString() }));
    win.setTitle(`Bun Catcher · best ${score}`);
  }
  return { best: Math.max(best, score), isNew };
});

// GAME_SELFTEST=1: start a round from the page, let it run, report, close.
if (process.env.GAME_SELFTEST) {
  win.handle("report", r => {
    console.log("selftest:", JSON.stringify(r));
    win.close();
  });
  setTimeout(() => {
    win.eval(`(async () => {
      const g = window.__game;
      g.start();
      const f0 = g.frames;
      await new Promise(r => setTimeout(r, 3000));
      await __BUNTAURI__.invoke("report", {
        canvas: !!document.querySelector("canvas"),
        renderer: g.renderer, state: g.state, framesIn3s: g.frames - f0,
        items: g.items, best: await __BUNTAURI__.invoke("best"),
      });
    })()`);
  }, 3000);
}
