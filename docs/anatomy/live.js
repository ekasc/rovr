const D = __DATA__;
const P = D.P;
const at = (p) => iso(p, P);
const S = (paths, style) => solidSvg(paths, style);
const L = (d, style) => lineSvg(d, style);
const Dots = (pts, style) => dotsSvg(pts, style);
const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));

function zcoil(cx, cy, z0, z1, r, turns, n = 12) {
  const count = Math.max(2, Math.round(turns * n));
  const back = [];
  const front = [];
  const backFlat = [];
  const frontFlat = [];
  let run = [];
  let near = null;
  for (let i = 0; i <= count; i++) {
    const t = i / count;
    const a = t * turns * Math.PI * 2;
    const point = [cx + r * Math.cos(a), cy + r * Math.sin(a), z0 + (z1 - z0) * t];
    const vis = depthOf([Math.cos(a), Math.sin(a), 0], P) > 0;
    const flat = at(point);
    if (near === null) near = vis;
    if (vis !== near) {
      run.push(flat);
      (near ? front : back).push(pathOf(run));
      (near ? frontFlat : backFlat).push(run);
      run = [flat];
      near = vis;
      continue;
    }
    run.push(flat);
  }
  (near ? front : back).push(pathOf(run));
  (near ? frontFlat : backFlat).push(run);
  return { back: back.join(""), front: front.join(""), backFlat, frontFlat };
}

const stage = document.querySelector(".rc-stage");
const svg = stage.querySelector("svg");
const readout = document.querySelector("[data-readout]");
const status = stage.querySelector("[data-status]");
const still = matchMedia("(prefers-reduced-motion: reduce)");
const ref = (name) => svg.querySelector(`[data-part="${name}"]`);

const el = {
  cam: D.camXs.map((_, i) => ref(`cam.${i}`)),
  roller: D.camXs.map((_, i) => ref(`roller.${i}`)),
  bar: ref("bar"),
  springsBack: ref("bar.springs.back"),
  springsFront: ref("bar.springs.front"),
  beam: ref("beam"),
  plungerL: ref("plunger.l"),
  plungerR: ref("plunger.r"),
  pointer: ref("pointer"),
  tile: Array.from({ length: D.nt }, (_, i) => ref(`tile.${i}`)),
  fence: ["fence.plate", "fence.web", "fence.cap", "fence.ramp"].map(ref),
  nut: ref("nut"),
  pen: ref("pen"),
  grid: ref("chart.grid"),
  pawl: ref("gen.pawl"),
};

// ---------------------------------------------------------------------------
// the model: two bars, one difference
// ---------------------------------------------------------------------------

const camTop = (phase) => D.camZ + D.camR + D.camE * Math.cos(phase);
const avgCos = (phases) => phases.reduce((s, p) => s + Math.cos(p), 0) / D.nt;
const barUnder = (phases) => D.camZ + D.camR + D.camE * avgCos(phases) + 2 * D.rollerR + 4.5;
const desiredTop = (phases) => barUnder(phases) + D.fbarH;
const fenceX = (e) => D.stopR - e - D.fenceT;
const observedTop = (e) => D.rampZ0 + D.slope * (D.stylusX - (fenceX(e) - D.rampBack));
const targetE = (phases) => (desiredTop(phases) - D.rampZ0 - D.slope * (D.stylusX + D.fenceT + D.rampBack - D.stopR)) / D.slope;
const tileLeft = (e, i) => D.stopR - e + i * (D.tileW + (e - D.packed) / (D.nt - 1));
const tileCenter = (e, i) => tileLeft(e, i) + D.tileW / 2;

// ---------------------------------------------------------------------------
// geometry, rebuilt from the same rules the build used
// ---------------------------------------------------------------------------

const camFrame = (i, phase) => G.frameOf([D.camXs[i], D.camE * Math.sin(phase), D.camZ + D.camE * Math.cos(phase)], [1, 0, 0], [0, 0, 1], [0, 1, 0]);
function camSvg(i, phase) {
  const F = camFrame(i, phase);
  const ribs = G.ribsOf(F, -D.camT / 2 + 0.6, D.camT / 2 - 0.6, D.camR, 18, P, { least: 0.05, fade: [0.05, 0.4] });
  return S(G.disc(-D.camT / 2, D.camT / 2, D.camR, F, P, { steps: 36, bevel: 0.7 }), { tone: "hi", crease: "lo" }) +
    (typeof ribs === "string" ? L(ribs, { tone: "faint" }) : fadedSvg(ribs, { tone: "faint" }));
}
function rollerSvg(i, phase) {
  return S(G.disc(-3, 3, D.rollerR, G.frameAlong([D.camXs[i], 0, camTop(phase) + D.rollerR], [1, 0, 0]), P, { steps: 24 }), { tone: "hi", crease: "lo" });
}
function springsSvg(under, side) {
  const out = [];
  D.camXs.forEach((cx, i) => {
    const c = zcoil(cx, 0, camTop(state.phases[i]) + 2 * D.rollerR - 1, under - 0.5, 3, 5);
    const runs = side === "back" ? c.backFlat : c.frontFlat;
    out.push(runs.map((pts) => L(pathOf(pts), { tone: "lo", free: true })).join(""));
  });
  return out.join("");
}
function barSvg(under) {
  const plan = { x: D.fbarX0, y: -D.fbarD / 2, w: D.fbarX1 - D.fbarX0, d: D.fbarD, r: 1.5 };
  return S(slabOf(plan, under, D.fbarH, P, 6, 0.6), { tone: "hi", crease: "lo" });
}
const BEAM_POLY = [[-5, -4], [5, -4], [5, 4], [-5, 4]];
function beamSvg(theta) {
  const F = G.frameOf(D.pivot, [Math.cos(theta), 0, Math.sin(theta)], [0, 1, 0], [-Math.sin(theta), 0, Math.cos(theta)]);
  const dots = [];
  for (let s = -D.arm + 8; s <= D.arm - 8; s += 12) {
    const p = G.pointOf(F, s, 0, Math.PI / 2);
    dots.push(at([p[0], p[1], p[2] + 0.2]));
  }
  return S(G.prismOf(F, BEAM_POLY, -D.arm, D.arm, P), { tone: "hi", crease: "lo" }) + Dots(dots, { size: 0.4, tone: "faint" });
}
function plungerSvg(x, z0, z1) {
  const c = zcoil(x, 0, z0 + 5, Math.min(z1 - 3, z0 + 19), 3, 4);
  return L(c.back, { tone: "lo" }) +
    S(G.disc(0, Math.max(1, z1 - z0), 1.8, G.frameAlong([x, 0, z0], [0, 0, 1]), P, { steps: 20 }), { tone: "hi", lit: true, crease: "none" }) +
    L(c.front, { tone: "lo" });
}
function tileSvg(e, i) {
  const left = tileLeft(e, i);
  const plan = { x: left, y: -D.tileD / 2, w: D.tileW, d: D.tileD, r: 1.4 };
  const out = [S(slabOf(plan, D.boardTop, D.tileH, P, 5, 0.5), { tone: "hi", crease: "lo" }),
    L(planOutline(insetPlan(plan, 2.2), D.tileTop, P, 5), { tone: "faint" }),
    L(lineOnTop([left + 3, -3], [left + D.tileW - 3, -3], D.tileTop, P), { tone: "lo" })];
  for (const dy of [1.5, 5]) out.push(L(onTop([[left + 3, dy], [left + D.tileW - 3, dy]], D.tileTop, P), { tone: "faint", free: true }));
  out.push(Dots([at([left + 3, D.tileD / 2 - 3, D.tileTop]), at([left + D.tileW - 3, D.tileD / 2 - 3, D.tileTop])], { size: 0.4 }));
  return out.join("");
}
function paperSvg(phase) {
  const f = G.frameAlong([D.rec.cx, D.rec.cy, D.rec.cz], [0, 0, 1]);
  const out = [S(G.disc(16, 16.8, D.rec.r - 1, f, P, { steps: 48 }), { tone: "hi", crease: "none" })];
  for (const r of [7.5, 14, 20]) out.push(L(ring(D.rec.cx, D.rec.cy, r, D.rec.cz + 17.1, P, 48), { tone: "faint" }));
  for (let i = 0; i < 12; i++) {
    const a = (i / 12) * Math.PI * 2 + phase;
    out.push(L(lineOnTop([D.rec.cx + Math.cos(a) * 7.5, D.rec.cy + Math.sin(a) * 7.5],
      [D.rec.cx + Math.cos(a) * 20.5, D.rec.cy + Math.sin(a) * 20.5], D.rec.cz + 17.1, P), { tone: i % 3 === 0 ? "lo" : "faint" }));
  }
  return out.join("");
}
function fenceSvgs(e) {
  const f = fenceX(e);
  const plate = { x: f, y: -9, w: D.fenceT, d: 18, r: 1.2 };
  const web = { x: f - 4, y: -5, w: 9, d: 10, r: 1.2 };
  const cap = { x: f - D.rampBack / 2, y: -7, w: D.rampBack + 10, d: 14, r: 1.4 };
  const poly = [[0, 0], [D.rampLen, 0], [D.rampLen, D.rampLen * D.slope]];
  const RF = G.frameOf([f - D.rampBack, 0, D.rampZ0], [0, 1, 0], [1, 0, 0], [0, 0, 1]);
  return [
    S(slabOf(plate, 22, 12, P, 3, 0.5), { tone: "mid" }),
    S(slabOf(web, 34, 18, P, 3, 0.4), { tone: "mid" }),
    S(slabOf(cap, 52, 6, P, 4, 0.5), { tone: "mid" }),
    S(G.prismOf(RF, poly, -8, 8, P), { tone: "hi", crease: "lo" }),
  ];
}
function nutSvg(e) {
  const f = fenceX(e);
  const F = G.frameAlong([f - 3, D.screwY, D.screwZ], [1, 0, 0]);
  const arm = { x: f - 6, y: 0, w: 6, d: D.screwY, r: 1.4 };
  return S(G.ringBand(0, 7, D.screwR + 0.2, 7, F, P, { steps: 24 }), { tone: "hi", crease: "lo" }) +
    S(slabOf({ x: f - 3, y: D.screwY - 5, w: 6, d: 6, r: 1.2 }, 16, 10, P, 3, 0.4), { tone: "mid" }) +
    S(slabOf(arm, 22, 6, P, 4, 0.4), { tone: "mid" });
}
function penSvg(e) {
  const reach = clamp(9 + (e - D.eHome) * 0.55, 3, 19);
  const arm = { x: D.rec.cx - D.rec.r + 3, y: D.rec.cy - 2.5, w: D.rec.r - 3, d: 5, r: 1.2 };
  const head = { x: D.rec.cx - reach - 3, y: D.rec.cy - 4, w: 6, d: 8, r: 1.4 };
  return S(slabOf(arm, D.rec.cz + 17.2, 3, P, 3, 0.3), { tone: "mid" }) +
    S(slabOf(head, D.rec.cz + 20.2, 6, P, 3, 0.4), { tone: "hi" }) +
    S(G.disc(0, 3.4, 0.8, G.frameAlong([D.rec.cx - reach, D.rec.cy, D.rec.cz + 20.2], [0, 0, -1]), P, { steps: 16 }), { tone: "hi", lit: true });
}
function pointerD(a) {
  const wy = D.colC[1];
  return lineOnTop([D.pivot[0] - Math.sin(a) * 3, wy + Math.cos(a) * 3],
    [D.pivot[0] + Math.sin(a) * (D.dialR - 5), wy - Math.cos(a) * (D.dialR - 5)], D.dialZ + 10.2, P);
}

// ---------------------------------------------------------------------------
// state and the loop
// ---------------------------------------------------------------------------

const state = {
  phases: D.homePhases.slice(),
  e: D.eHome,
  gen: 7,
  genAt: -10,
  clock: 0,
  selected: 2,
  touring: true,
  tourClock: 0,
  idle: 0,
  dragging: null,
  dragY: 0,
  dragX: 0,
  lift: 0,
};

const liftEnvelope = (age) => (age < 0 ? 0 : age < 0.12 ? age / 0.12 : Math.exp(-(age - 0.12) / 0.34));

function compute() {
  const lift = liftEnvelope(state.clock - state.genAt);
  state.lift = lift;
  const zd = desiredTop(state.phases);
  const zr = observedTop(state.e);
  const zrLifted = zr + 11 * lift;
  const diff = zd - zrLifted;
  const theta = -Math.asin(clamp(diff / (2 * D.arm), -0.32, 0.32));
  return { zd, zr, zrLifted, diff, theta, zEndL: D.pivot[2] + diff / 2, zEndR: D.pivot[2] - diff / 2, lift, target: targetE(state.phases), error: targetE(state.phases) - state.e };
}

function draw(m) {
  const under = m.zd - D.fbarH;
  D.camXs.forEach((_, i) => { if (el.cam[i]) el.cam[i].innerHTML = camSvg(i, state.phases[i]); });
  D.camXs.forEach((_, i) => { if (el.roller[i]) el.roller[i].innerHTML = rollerSvg(i, state.phases[i]); });
  if (el.bar) el.bar.innerHTML = barSvg(under);
  if (el.springsBack) el.springsBack.innerHTML = springsSvg(under, "back");
  if (el.springsFront) el.springsFront.innerHTML = springsSvg(under, "front");
  if (el.beam) el.beam.innerHTML = beamSvg(m.theta);
  if (el.plungerL) el.plungerL.innerHTML = plungerSvg(D.feelerL, m.zd, m.zEndL - 4);
  if (el.plungerR) el.plungerR.innerHTML = plungerSvg(D.stylusX, m.zrLifted, m.zEndR - 4);
  if (el.pointer) el.pointer.setAttribute("d", pointerD(-8 * m.theta));
  for (let i = 0; i < D.nt; i++) if (el.tile[i]) el.tile[i].innerHTML = tileSvg(state.e, i);
  const fences = fenceSvgs(state.e);
  el.fence.forEach((node, i) => { if (node) node.innerHTML = fences[i]; });
  if (el.nut) el.nut.innerHTML = nutSvg(state.e);
  if (el.pen) el.pen.innerHTML = penSvg(state.e);
  if (el.grid) el.grid.innerHTML = paperSvg(state.clock * 0.35);
  if (el.pawl) el.pawl.setAttribute("transform", translateAlong(P, [0, 0, 6 * m.lift]));
}

let announced = "";
function say(m) {
  let text;
  if (m.lift > 0.06) text = `generation ${state.gen} · re-observing`;
  else if (Math.abs(m.error) < 0.35) text = `reconciled · row ${state.e.toFixed(1)} · 5 windows`;
  else text = `${m.error > 0 ? "packing" : "releasing"} · row ${state.e.toFixed(1)} · target ${m.target.toFixed(1)} · error ${m.error.toFixed(1)}`;
  if (readout.textContent !== text) readout.textContent = text;
  stage.setAttribute("aria-valuenow", String(Math.round(state.e)));
  stage.setAttribute("aria-valuetext", `Wanted ${m.zd.toFixed(1)}, actual ${m.zr.toFixed(1)}. ${text}`);
  const phase = text.split("·")[0].trim();
  if (phase !== announced) {
    announced = phase;
    if (status) status.textContent = text;
  }
}

let frame = 0;
let last = 0;
let visible = false;
function tick(now) {
  const dt = last ? Math.min((now - last) / 1000, 1 / 30) : 1 / 60;
  last = now;
  state.clock += dt;
  const calm = still.matches;
  if (state.touring && state.clock - state.idle > 1.6) {
    state.tourClock += dt;
    if (state.tourClock > 3.4) {
      state.tourClock = 0;
      if (Math.random() < 0.6) nudge(3 + Math.random() * 3.5);
      else { turnCam(state.selected, (Math.random() < 0.5 ? 1 : -1) * 0.75); state.selected = (state.selected + 1) % D.nt; }
    }
  }
  const m = compute();
  if (!calm && m.lift < 0.06) state.e = clamp(state.e + clamp(m.error * 3.6, -16, 16) * dt, D.eMin, D.eMax);
  draw(m);
  say(m);
  const settled = !state.touring && Math.abs(m.error) < 0.05 && m.lift < 0.02;
  frame = visible && !settled ? requestAnimationFrame(tick) : 0;
}

const run = () => { if (!frame && visible) { last = 0; frame = requestAnimationFrame(tick); } };
let idleTimer = 0;
const touched = () => {
  state.touring = false;
  state.idle = state.clock;
  clearTimeout(idleTimer);
  idleTimer = setTimeout(() => { if (!still.matches) { state.touring = true; state.tourClock = 0; } run(); }, 4200);
  run();
};
const nudge = (amount) => { state.e = clamp(state.e + amount, D.eMin, D.eMax); touched(); };
const turnCam = (i, delta) => { state.phases[i] += delta; touched(); };

new IntersectionObserver((entries) => {
  visible = entries[entries.length - 1].isIntersecting;
  if (visible) run();
}, { rootMargin: "140px 0px" }).observe(stage);

stage.addEventListener("pointerdown", (event) => {
  const node = event.target.closest("[data-name]");
  if (!node) return;
  const name = node.getAttribute("data-name");
  if (name.startsWith("cam.")) {
    state.selected = Number(name.slice(4));
    state.dragging = "cam";
    state.dragY = event.clientY;
  } else if (name.startsWith("tile.") || name === "board" || name.startsWith("fence") || name === "nut") {
    state.dragging = "row";
    state.dragX = event.clientX;
  } else return;
  try { stage.setPointerCapture(event.pointerId); } catch { /* fine */ }
  event.preventDefault();
  touched();
});
stage.addEventListener("pointermove", (event) => {
  if (!state.dragging) return;
  if (state.dragging === "cam") {
    turnCam(state.selected, -(event.clientY - state.dragY) * 0.022);
    state.dragY = event.clientY;
  } else {
    state.e = clamp(state.e + (event.clientX - state.dragX) * 0.14, D.eMin, D.eMax);
    state.dragX = event.clientX;
    touched();
  }
});
const release = (event) => {
  if (!state.dragging) return;
  state.dragging = null;
  try { stage.releasePointerCapture(event.pointerId); } catch { /* fine */ }
};
stage.addEventListener("pointerup", release);
stage.addEventListener("pointercancel", release);

const bump = () => { state.gen = state.gen > 99 ? 0 : state.gen + 1; state.genAt = state.clock; touched(); run(); };

stage.addEventListener("keydown", (event) => {
  const key = event.key;
  if (key === "ArrowUp" || key === "ArrowRight") { turnCam(state.selected, 0.12); event.preventDefault(); }
  else if (key === "ArrowDown" || key === "ArrowLeft") { turnCam(state.selected, -0.12); event.preventDefault(); }
  else if (key === "[") { state.selected = (state.selected + D.nt - 1) % D.nt; touched(); event.preventDefault(); }
  else if (key === "]") { state.selected = (state.selected + 1) % D.nt; touched(); event.preventDefault(); }
  else if (key === " " || key === "Spacebar") { nudge(4); event.preventDefault(); }
  else if (key === "g" || key === "G") { bump(); event.preventDefault(); }
  else if (key === "Home") { nudge(20); event.preventDefault(); }
  else if (key === "Escape") stage.blur();
});

if (still.matches) state.touring = false;
{
  const m = compute();
  draw(m);
  say(m);
}
