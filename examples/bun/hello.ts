// Run with a buntauri build of Bun:
//   bun-debug examples/bun/hello.ts
//   BUNTAURI_SELFTEST=1 bun-debug examples/bun/hello.ts   (automated round trip, exits by itself)
import { Tray, Window, setAssetsDir } from "bun:buntauri";
// A file import, so `bun build --compile` embeds the icon (under a hashed name).
import iconPath from "./assets/icon.png" with { type: "file" };

setAssetsDir(import.meta.dir + "/assets");
const selftest = !!process.env.BUNTAURI_SELFTEST;

const win = new Window({
  title: "buntauri + Bun",
  url: selftest ? "index.html?selftest" : "index.html",
  width: 640,
  height: 480,
  center: true,
  theme: "dark",
  icon: iconPath,
  // The close button hides to the tray instead of quitting (see "closerequested").
  preventClose: !selftest,
  menu: [
    {
      text: "File",
      items: [
        { text: "Say hello", accelerator: "CmdOrCtrl+H", action: () => win.emit("hello", { from: "the menu" }) },
        { type: "separator" },
        { text: "Quit", accelerator: "CmdOrCtrl+Q", action: () => quit() },
      ],
    },
    { text: "Edit", items: [{ predefined: "copy" }, { predefined: "paste" }, { predefined: "selectAll" }] },
  ],
});

// A tray icon keeps the app alive on its own; quit() removes it.
const tray = new Tray({
  icon: iconPath,
  tooltip: "buntauri + Bun",
  menu: [
    { text: "Show window", action: () => win.focus() },
    { text: "Say hello", action: () => win.emit("hello", { from: "the tray" }) },
    { type: "separator" },
    { text: "Quit", action: () => quit() },
  ],
});
tray.on("click", ({ button }) => {
  console.log(`tray clicked (${button})`);
  if (button === "left") win.focus();
});

win.on("closerequested", () => {
  win.hide();
  console.log("window hidden to the tray; click the tray icon to bring it back");
});
win.on("resized", ({ width, height }) => console.log(`resized to ${width}x${height}`));

function quit() {
  tray.remove();
  win.close();
}

win.handle("greet", ({ name }) => `Hello ${name}, greetings from Bun ${Bun.version}!`);
win.handle("versions", () => ({ bun: Bun.version, platform: process.platform, pid: process.pid }));
win.handle("slow", async () => {
  await Bun.sleep(200);
  return "after 200 ms";
});
win.handle("fail", () => {
  throw new Error("failed on purpose");
});
win.handle("done", async result => {
  console.log("selftest:", JSON.stringify(result));
  // With --windows-hide-console there is no console to read: write it to a file instead.
  if (process.env.BUNTAURI_SELFTEST_OUT) await Bun.write(process.env.BUNTAURI_SELFTEST_OUT, JSON.stringify(result));
  quit();
});

win.on("created", () => console.log("window open"));
win.on("closed", () => console.log("window closed, Bun exits by itself"));
win.on("warning", msg => console.warn("warning:", msg));

await win.ready;
win.emit("hello", { from: "bun" });
