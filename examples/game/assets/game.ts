import { Application, Container, Graphics, Text } from "pixi.js";

const { invoke } = (window as any).__BUNTAURI__ as {
  invoke<T = unknown>(cmd: string, args?: unknown): Promise<T>;
};

type State = "title" | "play" | "over";
type Item = { g: Graphics; kind: "bun" | "gold" | "bomb"; vy: number; spin: number; r: number };
type Particle = { g: Graphics; vx: number; vy: number; life: number };

const PLATE_W = 120;

async function main() {
  const app = new Application();
  await app.init({ background: "#0b0820", resizeTo: window, antialias: true, preference: "webgl" });
  document.body.appendChild(app.canvas);

  const world = new Container();
  const stars = new Container();
  const itemLayer = new Container();
  const fx = new Container();
  const hud = new Container();
  world.addChild(stars, itemLayer, fx);
  app.stage.addChild(world, hud);

  // ── Background: twinkling, drifting stars ─────────────────────────────────
  const starData = Array.from({ length: 140 }, () => {
    const g = new Graphics().circle(0, 0, Math.random() * 1.6 + 0.4).fill(0xffffff);
    g.x = Math.random() * app.screen.width;
    g.y = Math.random() * app.screen.height;
    stars.addChild(g);
    return { g, speed: Math.random() * 0.4 + 0.1, phase: Math.random() * Math.PI * 2 };
  });

  // ── Sprites drawn with vector graphics ────────────────────────────────────
  const makeBurger = () => {
    const g = new Graphics();
    g.ellipse(0, -8, 26, 14).fill(0xf2b55a); // top bun
    for (const [x, y] of [[-12, -14], [-2, -18], [9, -15], [16, -9], [-18, -8], [3, -10]]) {
      g.ellipse(x, y, 2.4, 1.3).fill(0xfff1d0); // sesame
    }
    g.roundRect(-28, -4, 56, 6, 3).fill(0x4cc35a); // lettuce
    g.roundRect(-26, 0, 52, 8, 4).fill(0x6b3a22); // patty
    g.roundRect(-24, 7, 48, 9, 4).fill(0xe0a050); // bottom bun
    return g;
  };
  const makeBomb = () => {
    const g = new Graphics();
    g.circle(0, 0, 17).fill(0x262640);
    g.circle(-6, -6, 5).fill({ color: 0xffffff, alpha: 0.18 });
    g.roundRect(-5, -21, 10, 6, 2).fill(0x44445e);
    g.moveTo(0, -21).quadraticCurveTo(8, -30, 14, -26).stroke({ width: 3, color: 0xb08a5a });
    g.star(15, -27, 6, 7, 3).fill(0xffd34d);
    return g;
  };
  const plate = new Graphics();
  plate.roundRect(-PLATE_W / 2, -10, PLATE_W, 18, 9).fill(0x8b5cf6);
  plate.roundRect(-PLATE_W / 2 + 6, -10, PLATE_W - 12, 6, 3).fill({ color: 0xffffff, alpha: 0.28 });
  plate.ellipse(0, 16, PLATE_W / 2, 5).fill({ color: 0x000000, alpha: 0.35 });
  world.addChild(plate);

  // ── HUD ────────────────────────────────────────────────────────────────────
  const font = { fontFamily: "Segoe UI, system-ui, sans-serif", fontWeight: "800" as const, fill: 0xffffff };
  const scoreText = new Text({ text: "0", style: { ...font, fontSize: 40 } });
  const livesText = new Text({ text: "", style: { ...font, fontSize: 26, fill: 0xff5c7a } });
  const bestText = new Text({ text: "best 0", style: { ...font, fontSize: 18, fill: 0xa79bd6 } });
  const center = new Text({ text: "", style: { ...font, fontSize: 52, align: "center", dropShadow: { color: 0x7c5cff, blur: 18, distance: 0, alpha: 0.9 } } });
  const sub = new Text({ text: "", style: { ...font, fontSize: 20, fontWeight: "600", fill: 0xc9c2ea, align: "center" } });
  center.anchor.set(0.5);
  sub.anchor.set(0.5);
  hud.addChild(scoreText, livesText, bestText, center, sub);

  // ── State ──────────────────────────────────────────────────────────────────
  let state: State = "title";
  let score = 0, lives = 3, best = await invoke<number>("best").catch(() => 0);
  let spawnIn = 0, shake = 0, frames = 0, time = 0;
  let targetX = app.screen.width / 2;
  const keys = new Set<string>();
  const items: Item[] = [];
  const particles: Particle[] = [];

  const setOverlay = (title: string, line: string) => {
    center.text = title;
    sub.text = line;
  };
  const updateHud = () => {
    scoreText.text = String(score);
    livesText.text = "♥".repeat(Math.max(0, lives));
    bestText.text = `best ${best}`;
  };

  const start = () => {
    for (const it of items.splice(0)) it.g.destroy();
    score = 0;
    lives = 3;
    spawnIn = 30;
    state = "play";
    setOverlay("", "");
    updateHud();
  };

  const gameOver = async () => {
    state = "over";
    shake = 22;
    setOverlay("GAME OVER", `score ${score}\n\nclick or press space`);
    const res = await invoke<{ best: number; isNew: boolean }>("submit", { score }).catch(() => null);
    if (res) {
      best = res.best;
      if (res.isNew) setOverlay("NEW BEST!", `score ${score}\n\nclick or press space`);
      updateHud();
    }
  };

  const burst = (x: number, y: number, color: number, n = 18) => {
    for (let i = 0; i < n; i++) {
      const g = new Graphics().circle(0, 0, Math.random() * 3 + 1.5).fill(color);
      g.x = x;
      g.y = y;
      fx.addChild(g);
      const a = Math.random() * Math.PI * 2, s = Math.random() * 5 + 2;
      particles.push({ g, vx: Math.cos(a) * s, vy: Math.sin(a) * s - 2, life: 1 });
    }
  };

  const spawn = () => {
    const roll = Math.random();
    const bombChance = Math.min(0.42, 0.2 + score * 0.004);
    const kind: Item["kind"] = roll < bombChance ? "bomb" : roll > 0.95 ? "gold" : "bun";
    const g = kind === "bomb" ? makeBomb() : makeBurger();
    if (kind === "gold") g.tint = 0xffe14d;
    g.x = 40 + Math.random() * (app.screen.width - 80);
    g.y = -40;
    itemLayer.addChild(g);
    items.push({ g, kind, vy: 2.6 + score * 0.035 + Math.random() * 1.2, spin: (Math.random() - 0.5) * 0.08, r: kind === "bomb" ? 17 : 24 });
  };

  // ── Input ──────────────────────────────────────────────────────────────────
  app.stage.eventMode = "static";
  app.stage.hitArea = app.screen;
  app.stage.on("pointermove", e => (targetX = e.global.x));
  app.stage.on("pointerdown", () => state !== "play" && start());
  window.addEventListener("keydown", e => {
    keys.add(e.key);
    if ((e.key === " " || e.key === "Enter") && state !== "play") start();
  });
  window.addEventListener("keyup", e => keys.delete(e.key));

  setOverlay("BUN CATCHER", "catch the burgers · dodge the bombs\ngolden burger = +5\n\nclick or press space to start");
  updateHud();

  // ── Loop ───────────────────────────────────────────────────────────────────
  app.ticker.add(t => {
    const dt = t.deltaTime;
    const { width: W, height: H } = app.screen;
    frames++;
    time += dt;

    for (const s of starData) {
      s.g.y += s.speed * dt * (state === "play" ? 1 + score * 0.02 : 1);
      if (s.g.y > H) (s.g.y = -2), (s.g.x = Math.random() * W);
      s.g.alpha = 0.35 + 0.65 * Math.abs(Math.sin(time * 0.03 + s.phase));
    }

    if (keys.has("ArrowLeft") || keys.has("a")) targetX -= 12 * dt;
    if (keys.has("ArrowRight") || keys.has("d")) targetX += 12 * dt;
    targetX = Math.max(PLATE_W / 2, Math.min(W - PLATE_W / 2, targetX));
    plate.x += (targetX - plate.x) * Math.min(1, 0.25 * dt);
    plate.y = H - 50;

    if (state === "play") {
      spawnIn -= dt;
      if (spawnIn <= 0) {
        spawn();
        spawnIn = Math.max(16, 52 - score * 0.5);
      }
    }

    for (let i = items.length - 1; i >= 0; i--) {
      const it = items[i];
      it.g.y += it.vy * dt;
      it.g.rotation += it.spin * dt;
      const caught = state === "play" && Math.abs(it.g.x - plate.x) < PLATE_W / 2 + it.r * 0.5 && it.g.y > plate.y - 28 && it.g.y < plate.y + 8;
      if (caught) {
        if (it.kind === "bomb") {
          burst(it.g.x, it.g.y, 0xff5c3a, 32);
          shake = 14;
          lives--;
          if (lives <= 0) void gameOver();
        } else {
          const pts = it.kind === "gold" ? 5 : 1;
          score += pts;
          burst(it.g.x, it.g.y, it.kind === "gold" ? 0xffe14d : 0xf2b55a, pts * 10 + 8);
        }
        updateHud();
      }
      if (caught || it.g.y > H + 50) {
        if (!caught && it.kind !== "bomb" && state === "play") {
          // A missed burger costs nothing, but it flashes.
          burst(it.g.x, H - 6, 0x6b5ca8, 6);
        }
        it.g.destroy();
        items.splice(i, 1);
      }
    }

    for (let i = particles.length - 1; i >= 0; i--) {
      const p = particles[i];
      p.g.x += p.vx * dt;
      p.g.y += p.vy * dt;
      p.vy += 0.18 * dt;
      p.life -= 0.022 * dt;
      p.g.alpha = Math.max(0, p.life);
      if (p.life <= 0) {
        p.g.destroy();
        particles.splice(i, 1);
      }
    }

    shake = Math.max(0, shake - dt);
    world.x = (Math.random() - 0.5) * shake;
    world.y = (Math.random() - 0.5) * shake;

    scoreText.position.set(24, 16);
    livesText.position.set(26, 66);
    bestText.position.set(W - bestText.width - 24, 24);
    center.position.set(W / 2, H / 2 - 60 + Math.sin(time * 0.05) * 4);
    sub.position.set(W / 2, H / 2 + 40);
  });

  // For the selftest.
  (window as any).__game = {
    start,
    get state() { return state; },
    get frames() { return frames; },
    get items() { return items.length; },
    renderer: app.renderer.name,
  };
}

main();
