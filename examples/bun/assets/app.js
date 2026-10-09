const { invoke, listen } = window.__BUNTAURI__;
const out = document.getElementById("out");
const log = (s) => (out.textContent += s + "\n");
const show = (p) => p.then((v) => log(JSON.stringify(v)), (e) => log(`error: ${e.message}`));

listen("hello", (p) => log(`event hello: ${JSON.stringify(p)}`));

for (const cmd of ["versions", "slow", "fail"]) document.getElementById(cmd).onclick = () => show(invoke(cmd));
document.getElementById("greet").onclick = () => show(invoke("greet", { name: "world" }));

if (new URLSearchParams(location.search).has("selftest")) {
  (async () => {
    const r = { href: location.href };
    r.greet = await invoke("greet", { name: "selftest" });
    r.versions = await invoke("versions");
    r.slow = await invoke("slow");
    r.fail = await invoke("fail").then(() => "no error", (e) => e.message);
    r.unknown = await invoke("nope").then(() => "no error", (e) => e.message);
    await invoke("done", r);
  })();
}
