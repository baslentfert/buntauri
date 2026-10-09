// Run with a buntauri build of Bun:
//   bun-debug examples/bun/hello.ts
//   BUNTAURI_SELFTEST=1 bun-debug examples/bun/hello.ts   (automated round trip, exits by itself)
import { Window, setAssetsDir } from "bun:buntauri";

setAssetsDir(import.meta.dir + "/assets");
const selftest = !!process.env.BUNTAURI_SELFTEST;

const win = new Window({
  title: "buntauri + Bun",
  url: selftest ? "index.html?selftest" : "index.html",
  width: 640,
  height: 480,
  center: true,
  theme: "dark",
});

win.handle("greet", ({ name }) => `Hallo ${name}, groeten van Bun ${Bun.version}!`);
win.handle("versions", () => ({ bun: Bun.version, platform: process.platform, pid: process.pid }));
win.handle("slow", async () => {
  await Bun.sleep(200);
  return "na 200 ms";
});
win.handle("fail", () => {
  throw new Error("expres fout");
});
win.handle("done", async result => {
  console.log("selftest:", JSON.stringify(result));
  // With --windows-hide-console there is no console to read: write it to a file instead.
  if (process.env.BUNTAURI_SELFTEST_OUT) await Bun.write(process.env.BUNTAURI_SELFTEST_OUT, JSON.stringify(result));
  win.close();
});

win.on("created", () => console.log("venster open"));
win.on("closed", () => console.log("venster dicht, Bun stopt vanzelf"));
win.on("warning", msg => console.warn("waarschuwing:", msg));

await win.ready;
win.emit("hello", { from: "bun" });
