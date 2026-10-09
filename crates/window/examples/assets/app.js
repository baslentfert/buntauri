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
