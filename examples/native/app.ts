// Native extras from Bun: dialogs, notifications, clipboard, global shortcuts,
// single instance.
//
//   bun-debug app.ts
//   NATIVE_SELFTEST=1 bun-debug app.ts   (clipboard, shortcut, single instance; no dialogs)
import { Window, clipboard, dialog, globalShortcut, notify, requestSingleInstance, setAssetsDir } from "bun:buntauri";
// A file import, so `bun build --compile` embeds the icon (under a hashed name).
import iconPath from "./assets/icon.png" with { type: "file" };

const selftest = !!process.env.NATIVE_SELFTEST;

// Only one copy at a time: a second start hands its arguments to this one and exits.
const instance = await requestSingleInstance("buntauri.example.native");
if (!instance) {
  console.log("already running: arguments handed to the first instance");
  process.exit(0);
}

setAssetsDir(import.meta.dir + "/assets");
const win = new Window({
  title: "buntauri native extras",
  url: "index.html",
  width: 720,
  height: 560,
  center: true,
  theme: "dark",
  icon: iconPath,
});
const log = (line: string) => win.emit("log", line);

instance.on("second-instance", ({ argv }) => {
  win.focus();
  log(`second instance started with ${JSON.stringify(argv)}; focused this window instead`);
});

win.handle("open", () => dialog.open({ title: "Pick files", multiple: true, window: win }));
win.handle("openFolder", () => dialog.open({ title: "Pick a folder", directory: true, window: win }));
win.handle("save", () =>
  dialog.save({ title: "Save as", fileName: "notes.txt", filters: [{ name: "Text", extensions: ["txt"] }], window: win }),
);
win.handle("message", () => dialog.message("Hello from Bun!", { title: "buntauri", level: "info", window: win }));
win.handle("confirm", () => dialog.confirm("Do you like this?", { title: "Question", window: win }));
win.handle("notify", () => notify({ title: "buntauri", body: `A toast from Bun ${Bun.version}` }));
win.handle("copy", ({ text }) => clipboard.writeText(text));
win.handle("paste", () => clipboard.readText());
win.handle("shortcut", async () => {
  const accel = "CmdOrCtrl+Shift+B";
  if (globalShortcut.isRegistered(accel)) {
    await globalShortcut.unregister(accel);
    return `${accel} unregistered`;
  }
  await globalShortcut.register(accel, state => {
    if (state !== "pressed") return;
    win.focus();
    log(`${accel} pressed (works even when another app has focus)`);
  });
  return `${accel} registered: press it anywhere`;
});

win.on("closed", () => {
  instance.release();
  process.exit(0);
});

if (selftest) {
  await win.ready;
  const r: Record<string, unknown> = {};
  // Clipboard round trip, restoring the user's clipboard.
  const saved = clipboard.readText();
  clipboard.writeText("buntauri clipboard test");
  r.clipboard = clipboard.readText() === "buntauri clipboard test";
  if (saved !== null) clipboard.writeText(saved);
  // Global shortcut.
  await globalShortcut.register("CmdOrCtrl+Alt+Shift+F11", () => {});
  r.shortcutRegistered = globalShortcut.isRegistered("CmdOrCtrl+Alt+Shift+F11");
  await globalShortcut.unregister("CmdOrCtrl+Alt+Shift+F11");
  // Bad input is a rejected promise / thrown error, not a crash.
  r.badShortcut = await globalShortcut.register("Ctrl+Nope", () => {}).then(() => "accepted", e => e.message);
  // Single instance: a second copy must hand over and exit.
  const second = new Promise<unknown>(resolve => instance.on("second-instance", resolve));
  const child = Bun.spawn([process.execPath, import.meta.path, "--from-selftest"], { stdout: "pipe" });
  r.secondExit = await child.exited;
  r.secondOutput = (await new Response(child.stdout).text()).trim();
  r.secondArgs = ((await second) as { argv: string[] }).argv;
  console.log("selftest:", JSON.stringify(r));
  win.close();
}
