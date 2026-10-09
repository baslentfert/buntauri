// System Pulse: a React dashboard whose data all comes from Bun.
//
//   bun-debug app.ts                                   (dev: bundles assets/ first)
//   bun-debug build --compile --windows-hide-console app.ts assets/index.html --outfile SystemPulse.exe
import { Window, setAssetsDir } from "bun:buntauri";
import { Database } from "bun:sqlite";
import { existsSync, statfsSync } from "node:fs";
import os from "node:os";

// In a compiled exe the bundled frontend is inside the executable. In dev,
// bundle assets/ (React + TSX) into .dist/ and serve that.
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

// ── Live CPU + memory, sampled every second and kept in SQLite ──────────────
const db = new Database(":memory:");
db.run("CREATE TABLE samples (t INTEGER PRIMARY KEY, cpu REAL, mem REAL)");
const insert = db.prepare("INSERT INTO samples VALUES (?, ?, ?)");

let previous = os.cpus().map(c => c.times);
function sample() {
  const now = os.cpus().map(c => c.times);
  const cores = now.map((t, i) => {
    const p = previous[i];
    const idle = t.idle - p.idle;
    const total = t.user + t.nice + t.sys + t.idle + t.irq - (p.user + p.nice + p.sys + p.idle + p.irq);
    return total > 0 ? Math.round((1 - idle / total) * 1000) / 10 : 0;
  });
  previous = now;
  const cpu = Math.round((cores.reduce((a, b) => a + b, 0) / cores.length) * 10) / 10;
  const memUsed = os.totalmem() - os.freemem();
  const mem = Math.round((memUsed / os.totalmem()) * 1000) / 10;
  insert.run(Date.now(), cpu, mem);
  db.run("DELETE FROM samples WHERE t < ?", [Date.now() - 5 * 60_000]);
  return { cpu, cores, mem, memUsed, memTotal: os.totalmem(), uptime: os.uptime(), bunRss: process.memoryUsage().rss };
}

// ── One-off lookups ─────────────────────────────────────────────────────────
let gpuCache: Promise<string[]> | undefined;
function gpus() {
  gpuCache ??= Bun.$`powershell -NoProfile -Command "(Get-CimInstance Win32_VideoController).Name"`
    .quiet()
    .text()
    .then(t => t.split(/\r?\n/).map(s => s.trim()).filter(Boolean))
    .catch(() => []);
  return gpuCache;
}

async function systemInfo() {
  const cpu = os.cpus()[0];
  const nets = Object.entries(os.networkInterfaces()).flatMap(([name, addrs]) =>
    (addrs ?? []).filter(a => !a.internal).map(a => ({ name, address: a.address, family: a.family, mac: a.mac })),
  );
  return {
    hostname: os.hostname(),
    user: os.userInfo().username,
    os: `${os.version()} (${os.release()})`,
    arch: os.arch(),
    cpu: cpu?.model.trim(),
    cores: os.cpus().length,
    speedMHz: cpu?.speed,
    memTotal: os.totalmem(),
    gpus: await gpus(),
    network: nets,
    bun: Bun.version,
    revision: Bun.revision.slice(0, 9),
    pid: process.pid,
    compiled,
    exe: process.execPath,
  };
}

function disks() {
  const out = [];
  for (const letter of "CDEFGHIJKLMNOPQRSTUVWXYZ") {
    const root = `${letter}:\\`;
    if (!existsSync(root)) continue;
    try {
      const s = statfsSync(root);
      out.push({ name: `${letter}:`, total: s.blocks * s.bsize, free: s.bavail * s.bsize });
    } catch {}
  }
  return out;
}

async function processes() {
  const csv = await Bun.$`tasklist /fo csv /nh`.quiet().text();
  const rows = csv
    .trim()
    .split(/\r?\n/)
    .map(line => line.slice(1, -1).split('","'))
    .map(([name, pid, , , mem]) => ({ name, pid: Number(pid), memKB: Number(mem?.replace(/\D/g, "") ?? 0) }));
  // Group by name: browsers and friends run as many processes.
  const byName = new Map<string, { name: string; count: number; memKB: number }>();
  for (const r of rows) {
    const g = byName.get(r.name) ?? { name: r.name, count: 0, memKB: 0 };
    g.count++;
    g.memKB += r.memKB;
    byName.set(r.name, g);
  }
  return { total: rows.length, top: [...byName.values()].sort((a, b) => b.memKB - a.memKB).slice(0, 10) };
}

function history() {
  // SQL over the last minute, straight from bun:sqlite.
  const since = Date.now() - 60_000;
  return {
    points: db.query("SELECT cpu, mem FROM samples WHERE t >= ? ORDER BY t").all(since),
    stats: db.query("SELECT ROUND(AVG(cpu), 1) AS avgCpu, MAX(cpu) AS maxCpu, ROUND(AVG(mem), 1) AS avgMem, COUNT(*) AS n FROM samples WHERE t >= ?").get(since),
  };
}

function benchmark() {
  const size = 64 * 1024 * 1024;
  const data = new Uint8Array(size);
  crypto.getRandomValues(data.subarray(0, 65536));
  for (let o = 65536; o < size; o *= 2) data.copyWithin(o, 0, o);
  const rounds = 4;
  const run = (bytes: Uint8Array, fn: (b: Uint8Array) => unknown) => {
    const t = performance.now();
    for (let i = 0; i < rounds; i++) fn(bytes);
    const ms = performance.now() - t;
    return { ms: Math.round(ms), mbps: Math.round((bytes.length * rounds) / 1048576 / (ms / 1000)) };
  };
  return {
    sizeMB: (size * rounds) / 1048576,
    sha256: run(data, b => new Bun.CryptoHasher("sha256").update(b).digest()),
    wyhash: run(data, b => Bun.hash(b)),
    gzip: run(data.subarray(0, 16 * 1024 * 1024), b => Bun.gzipSync(b, { level: 1 })),
  };
}

// ── Window ──────────────────────────────────────────────────────────────────
const win = new Window({
  title: "System Pulse",
  url: "index.html",
  width: 1280,
  height: 860,
  minWidth: 860,
  minHeight: 600,
  center: true,
  theme: "dark",
  backgroundColor: "#07080d",
});

win.handle("system", systemInfo);
win.handle("disks", disks);
win.handle("processes", processes);
win.handle("history", history);
win.handle("benchmark", benchmark);

const timer = setInterval(() => win.emit("stats", sample()), 1000);
win.on("closed", () => clearInterval(timer));

// DASHBOARD_SELFTEST=1: after a few seconds the page reports what it shows, then the window closes.
if (process.env.DASHBOARD_SELFTEST) {
  win.handle("report", report => {
    console.log("selftest:", JSON.stringify(report));
    win.close();
  });
  setTimeout(() => {
    win.eval(`(async () => {
      const q = s => [...document.querySelectorAll(s)];
      await __BUNTAURI__.invoke("report", {
        cards: q(".card h2").map(e => e.textContent),
        cpu: q(".ring-text strong")[0]?.textContent,
        cores: q(".core").length,
        machine: q(".facts dd").slice(0, 3).length,
        disks: q(".disk").length,
        nets: q(".net .row").length,
        processes: q("tbody tr").length,
        historyPoints: q(".spark polyline").length,
        bench: await __BUNTAURI__.invoke("benchmark"),
      });
    })()`);
  }, 5000);
}
