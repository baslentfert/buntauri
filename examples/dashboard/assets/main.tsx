import { StrictMode, useEffect, useState, type ReactNode } from "react";
import { createRoot } from "react-dom/client";

// ── Bridge to Bun ─────────────────────────────────────────────────────────────
declare global {
  interface Window {
    __BUNTAURI__: {
      invoke<T = unknown>(cmd: string, args?: unknown): Promise<T>;
      listen(name: string, fn: (payload: any) => void): () => void;
    };
  }
}
const { invoke, listen } = window.__BUNTAURI__;

type Stats = { cpu: number; cores: number[]; mem: number; memUsed: number; memTotal: number; uptime: number; bunRss: number };
type System = {
  hostname: string; user: string; os: string; arch: string; cpu: string; cores: number; speedMHz: number;
  memTotal: number; gpus: string[]; network: { name: string; address: string; family: string; mac: string }[];
  bun: string; revision: string; pid: number; compiled: boolean; exe: string;
};
type Disk = { name: string; total: number; free: number };
type Procs = { total: number; top: { name: string; count: number; memKB: number }[] };
type History = { points: { cpu: number; mem: number }[]; stats: { avgCpu: number; maxCpu: number; avgMem: number; n: number } };
type Bench = { sizeMB: number; sha256: Run; wyhash: Run; gzip: Run };
type Run = { ms: number; mbps: number };

// ── Formatting ────────────────────────────────────────────────────────────────
const gb = (b: number) => `${(b / 1024 ** 3).toFixed(1)} GB`;
const mb = (b: number) => `${Math.round(b / 1024 ** 2)} MB`;
const duration = (s: number) => {
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  return d ? `${d}d ${h}h ${m}m` : `${h}h ${m}m`;
};
const heat = (pct: number) => (pct > 80 ? "var(--hot)" : pct > 50 ? "var(--warm)" : "var(--cool)");

// ── Pieces ────────────────────────────────────────────────────────────────────
function Card({ title, icon, wide, tall, action, children }: { title: string; icon: string; wide?: boolean; tall?: boolean; action?: ReactNode; children: ReactNode }) {
  return (
    <section className={`card${wide ? " wide" : ""}${tall ? " tall" : ""}`}>
      <header>
        <h2><span className="icon">{icon}</span>{title}</h2>
        {action}
      </header>
      {children}
    </section>
  );
}

function Ring({ value, label, sub }: { value: number; label: string; sub?: string }) {
  const r = 52, c = 2 * Math.PI * r;
  return (
    <div className="ring">
      <svg viewBox="0 0 128 128">
        <circle cx="64" cy="64" r={r} className="ring-track" />
        <circle cx="64" cy="64" r={r} className="ring-value" stroke={heat(value)}
          strokeDasharray={c} strokeDashoffset={c * (1 - value / 100)} />
      </svg>
      <div className="ring-text"><strong>{value.toFixed(0)}%</strong><span>{label}</span>{sub && <small>{sub}</small>}</div>
    </div>
  );
}

function Sparkline({ values, color }: { values: number[]; color: string }) {
  if (values.length < 2) return <div className="spark empty">collecting…</div>;
  const w = 600, h = 120;
  const pts = values.map((v, i) => [(i / (values.length - 1)) * w, h - (v / 100) * h]);
  const line = pts.map(([x, y]) => `${x.toFixed(1)},${y.toFixed(1)}`).join(" ");
  return (
    <svg className="spark" viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none">
      <defs>
        <linearGradient id={`g-${color}`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={color} stopOpacity="0.45" />
          <stop offset="100%" stopColor={color} stopOpacity="0" />
        </linearGradient>
      </defs>
      <polygon points={`0,${h} ${line} ${w},${h}`} fill={`url(#g-${color})`} />
      <polyline points={line} fill="none" stroke={color} strokeWidth="2.5" vectorEffect="non-scaling-stroke" />
    </svg>
  );
}

function Bar({ value, max = 100, color }: { value: number; max?: number; color?: string }) {
  const pct = Math.min(100, (value / max) * 100);
  return <div className="bar"><div style={{ width: `${pct}%`, background: color ?? heat(pct) }} /></div>;
}

function useCall<T>(cmd: string, deps: unknown[] = []) {
  const [data, setData] = useState<T>();
  const [busy, setBusy] = useState(false);
  const run = async (args?: unknown) => {
    setBusy(true);
    try { setData(await invoke<T>(cmd, args)); } finally { setBusy(false); }
  };
  useEffect(() => { run(); }, deps);
  return { data, busy, run };
}

// ── App ───────────────────────────────────────────────────────────────────────
function App() {
  const [stats, setStats] = useState<Stats>();
  const system = useCall<System>("system");
  const disks = useCall<Disk[]>("disks");
  const procs = useCall<Procs>("processes");
  const history = useCall<History>("history");
  const [bench, setBench] = useState<Bench>();
  const [benching, setBenching] = useState(false);

  useEffect(() => listen("stats", setStats), []);
  // Re-read the SQL history every 2 s.
  useEffect(() => {
    const t = setInterval(() => history.run(), 2000);
    return () => clearInterval(t);
  }, []);

  const s = system.data;
  const runBench = async () => {
    setBenching(true);
    try { setBench(await invoke<Bench>("benchmark")); } finally { setBenching(false); }
  };

  return (
    <main>
      <div className="glow a" /><div className="glow b" />
      <header className="top">
        <div>
          <h1>System <span>Pulse</span></h1>
          <p>Every number on this page comes from Bun{s ? ` ${s.bun}` : ""}, running natively behind this window.</p>
        </div>
        <div className="live"><i />live · {s?.hostname ?? "…"}</div>
      </header>

      <div className="grid">
        <Card title="CPU" icon="⚡">
          <div className="split">
            <Ring value={stats?.cpu ?? 0} label="load" sub={s ? `${s.cores} threads` : undefined} />
            <div className="cores">
              {(stats?.cores ?? []).map((c, i) => (
                <div key={i} className="core"><span>#{i}</span><Bar value={c} /><b>{c.toFixed(0)}%</b></div>
              ))}
            </div>
          </div>
        </Card>

        <Card title="Memory" icon="🧠">
          <div className="split">
            <Ring value={stats?.mem ?? 0} label="used" sub={stats ? `${gb(stats.memUsed)} / ${gb(stats.memTotal)}` : undefined} />
            <dl className="facts">
              <dt>Free</dt><dd>{stats ? gb(stats.memTotal - stats.memUsed) : "…"}</dd>
              <dt>Bun process</dt><dd>{stats ? mb(stats.bunRss) : "…"}</dd>
              <dt>Uptime</dt><dd>{stats ? duration(stats.uptime) : "…"}</dd>
            </dl>
          </div>
        </Card>

        <Card title="Last minute · bun:sqlite" icon="📈" wide>
          <div className="chart">
            <Sparkline values={(history.data?.points ?? []).map(p => p.cpu)} color="#7c5cff" />
            <Sparkline values={(history.data?.points ?? []).map(p => p.mem)} color="#22d3ee" />
          </div>
          <div className="legend">
            <span><i style={{ background: "#7c5cff" }} />CPU avg {history.data?.stats.avgCpu ?? "–"}% · max {history.data?.stats.maxCpu ?? "–"}%</span>
            <span><i style={{ background: "#22d3ee" }} />Memory avg {history.data?.stats.avgMem ?? "–"}%</span>
            <span className="muted">{history.data?.stats.n ?? 0} samples, queried with SQL</span>
          </div>
        </Card>

        <Card title="Machine" icon="🖥️">
          {s ? (
            <dl className="facts">
              <dt>Host</dt><dd>{s.hostname}</dd>
              <dt>User</dt><dd>{s.user}</dd>
              <dt>OS</dt><dd>{s.os}</dd>
              <dt>CPU</dt><dd>{s.cpu}</dd>
              <dt>GPU</dt><dd>{s.gpus.join(", ") || "–"}</dd>
              <dt>Arch</dt><dd>{s.arch}</dd>
            </dl>
          ) : <div className="skeleton" />}
        </Card>

        <Card title="Disks" icon="💽" action={<button onClick={() => disks.run()} disabled={disks.busy}>↻</button>}>
          <div className="list">
            {(disks.data ?? []).map(d => {
              const used = d.total - d.free;
              return (
                <div key={d.name} className="disk">
                  <div className="row"><b>{d.name}</b><span>{gb(d.free)} free of {gb(d.total)}</span></div>
                  <Bar value={used} max={d.total} />
                </div>
              );
            })}
          </div>
        </Card>

        <Card title="Network" icon="🌐">
          <div className="list net">
            {(s?.network ?? []).map((n, i) => (
              <div key={i} className="row"><b>{n.name}</b><code>{n.address}</code><span className="tag">{n.family}</span></div>
            ))}
          </div>
        </Card>

        <Card title="Top processes" icon="📋" wide action={<button onClick={() => procs.run()} disabled={procs.busy}>{procs.busy ? "…" : "↻"}</button>}>
          <table>
            <thead><tr><th>Process</th><th>Instances</th><th>Memory</th><th /></tr></thead>
            <tbody>
              {(procs.data?.top ?? []).map(p => (
                <tr key={p.name}>
                  <td>{p.name}</td><td>{p.count}</td><td>{mb(p.memKB * 1024)}</td>
                  <td className="w"><Bar value={p.memKB} max={procs.data!.top[0].memKB} color="linear-gradient(90deg,#7c5cff,#22d3ee)" /></td>
                </tr>
              ))}
            </tbody>
          </table>
          <p className="muted">{procs.data ? `${procs.data.total} processes, read with Bun.$\`tasklist\`` : "reading…"}</p>
        </Card>

        <Card title="Bun speed test" icon="🚀" action={<button className="primary" onClick={runBench} disabled={benching}>{benching ? "running…" : "Run"}</button>}>
          {bench ? (
            <div className="bench">
              {([["SHA-256", bench.sha256], ["wyhash", bench.wyhash], ["gzip", bench.gzip]] as const).map(([name, r]) => (
                <div key={name} className="row"><b>{name}</b><span>{r.mbps.toLocaleString()} MB/s</span><small>{r.ms} ms</small></div>
              ))}
              <p className="muted">{bench.sizeMB} MB hashed (gzip: 64 MB), natively in Bun</p>
            </div>
          ) : <p className="muted">Hash 256 MB with Bun.CryptoHasher and Bun.hash, compress with Bun.gzipSync.</p>}
        </Card>

        <Card title="Runtime" icon="🥟">
          {s ? (
            <dl className="facts">
              <dt>Bun</dt><dd>{s.bun} <code>{s.revision}</code></dd>
              <dt>PID</dt><dd>{s.pid}</dd>
              <dt>Mode</dt><dd>{s.compiled ? "single exe" : "bun run (dev)"}</dd>
              <dt>Exe</dt><dd className="path">{s.exe}</dd>
            </dl>
          ) : <div className="skeleton" />}
        </Card>
      </div>
    </main>
  );
}

createRoot(document.getElementById("root")!).render(<StrictMode><App /></StrictMode>);
