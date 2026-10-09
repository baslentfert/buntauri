// Stress test: Bun's JavaScriptCore under load while windows talk to the UI.
// Checks that the webview side (on macOS/Linux in the UI host process) and
// Bun's own JS engine stay out of each other's way: many evaluate() and
// invoke round trips, forced GC and a busy worker at the same time.
//
//   bun examples/bun/stress.ts            (exits 0 when everything held up)
//   STRESS_ROUNDS=2000 bun examples/bun/stress.ts
import { Window } from "bun:buntauri";

const rounds = Number(process.env.STRESS_ROUNDS ?? 500);
const started = performance.now();
const fail = (msg: string) => {
  console.error("stress: FAIL:", msg);
  process.exit(1);
};
setTimeout(() => fail("timeout after 120 s"), 120_000).unref();

// A worker that keeps allocating, so Bun's GC and threads stay busy.
const worker = new Worker(
  URL.createObjectURL(
    new Blob([
      `let keep = [];
       setInterval(() => { keep.push(new Array(10000).fill(Math.random())); if (keep.length > 50) keep = []; }, 1);
       onmessage = e => postMessage(e.data * 2);`,
    ]),
  ),
);
const workerAnswer = (n: number) =>
  new Promise<number>(resolve => {
    worker.onmessage = e => resolve(e.data);
    worker.postMessage(n);
  });

const page = `<script>
  window.__BUNTAURI__.listen("ping", n => window.__BUNTAURI__.invoke("pong", { n }));
</script><p>stress</p>`;
const win = new Window({ title: "buntauri stress", html: page, width: 400, height: 300 });
let pongs = 0;
win.handle("pong", ({ n }: { n: number }) => {
  pongs++;
  return n + 1;
});
await win.ready;

for (let i = 0; i < rounds; i++) {
  // Page JS engine: an expression with an object result.
  const r = await win.evaluate<{ i: number; s: string }>(`({ i: ${i}, s: "x".repeat(${i % 100}) })`);
  if (r.i !== i || r.s.length !== i % 100) fail(`evaluate round ${i} returned ${JSON.stringify(r)}`);
  // Page -> Bun invoke, started by an event from Bun.
  win.emit("ping", i);
  // Bun's own engine: garbage, a forced GC now and then, and the worker.
  const junk = Array.from({ length: 1000 }, (_, k) => ({ k, s: String(k) }));
  if (junk.length !== 1000) fail("junk");
  if (i % 50 === 0) {
    Bun.gc(true);
    if ((await workerAnswer(i)) !== i * 2) fail(`worker round ${i}`);
  }
}

// Let the last invokes arrive.
const deadline = performance.now() + 10_000;
while (pongs < rounds && performance.now() < deadline) await Bun.sleep(20);
if (pongs !== rounds) fail(`${pongs} of ${rounds} invokes arrived`);

const ms = Math.round(performance.now() - started);
console.log(`stress: ok, ${rounds} evaluate + ${rounds} invoke round trips, ${Math.ceil(rounds / 50)} forced GCs with a busy worker, ${ms} ms`);
worker.terminate();
win.close();
