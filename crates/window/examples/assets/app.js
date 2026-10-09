const { invoke, listen } = window.__BUNTAURI__;
const out = document.getElementById("out");
const log = (s) => (out.textContent += s + "\n");
const $ = (id) => document.getElementById(id);

listen("tick", (n) => log(`event tick ${n}`));

$("greet").onclick = async () => log(await invoke("greet", { name: $("name").value }));
$("uptime").onclick = async () => log(`uptime: ${(await invoke("uptime")).toFixed(2)}s`);
$("title").onclick = async () => { await invoke("title", { title: `buntauri - ${new Date().toLocaleTimeString()}` }); log("titel gewijzigd"); };
$("tick").onclick = () => invoke("tick");
$("bad").onclick = () => invoke("nope").catch((e) => log(`fout: ${e.message}`));

log(`geladen vanaf ${location.href}`);

if (new URLSearchParams(location.search).has("selftest")) {
  (async () => {
    const r = { href: location.href };
    const ticks = [];
    listen("tick", (n) => ticks.push(n));
    r.greet = await invoke("greet", { name: "selftest" });
    r.uptime = typeof (await invoke("uptime"));
    await invoke("tick");
    r.ticks = ticks;
    r.reject = await invoke("nope").then(() => "no error", (e) => e.message);
    await invoke("done", r);
  })();
}
