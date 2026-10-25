import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { homedir } from "node:os";

const HERE = dirname(fileURLToPath(import.meta.url));
const SKILL = process.env.ANATOMY_DIR || join(homedir(), ".agents/skills/anatomy");
const k = await import(pathToFileURL(join(SKILL, "kit/iso-kit.mjs")).href);
const G = await import(pathToFileURL(join(SKILL, "kit/lathe.mjs")).href);
const A = await import(pathToFileURL(join(SKILL, "kit/audit.mjs")).href);
const { kitScript } = await import(pathToFileURL(join(SKILL, "scripts/inline-kit.mjs")).href);

const THEME = process.argv.includes("--light") ? "light" : "dark";
const W = 720;
const H = 440;
const AZIMUTH = 38;
const ELEVATION = 33;

// ---------------------------------------------------------------------------
// the bench
// ---------------------------------------------------------------------------

const PLATE = { x: -170, y: -72, w: 340, d: 144, r: 10 };
const PLATE_T = 14;
const FOOT_H = 6;

// wanted: five eccentrics on one shaft, a sprung follower bar above them
const CAM_XS = [-140, -122, -104, -86, -68];
const CAM_Z = 33;
const CAM_R = 12;
const CAM_E = 4.5;
const CAM_T = 8;
const ROLLER_R = 4;
const SHAFT_R = 3.5;
const SHAFT_X0 = -156;
const SHAFT_X1 = -52;
const FBAR_H = 6;
const FBAR_D = 12;
const FBAR_X0 = -162;
const FBAR_X1 = -46;
const HOME_PHASES = [0, 72, 144, 216, 288].map((d) => (d * Math.PI) / 180);

// the comparator
const PIVOT = [-45, 0, 104];
const ARM = 50;
const COL_C = [-45, -26];
const COL_TOP = 96;
const HEAD_TOP = 112;
const DIAL_Z = 112;
const DIAL_R = 28;
const FEELER_L_X = -95;

// reported: five window tiles packed by a fence, a feeler on the fence's ramp
const BOARD = { x: -28, y: -30, w: 190, d: 60, r: 4 };
const BOARD_TOP = 18;
const TILE_W = 22;
const TILE_D = 14;
const TILE_H = 6;
const TILE_TOP = BOARD_TOP + TILE_H;
const NT = 5;
const STOP_R = 149;
const PACKED = NT * TILE_W; // 110
const E_MIN = 110;
const E_MAX = 146;
const E_HOME = 128;
const RAMP_SLOPE = 0.25;
const RAMP_BACK = 44;
const RAMP_LEN = 50;
const RAMP_Z0 = 55.25;
const STYLUS_X = 5;
const STYLUS_Y = 0;
const FENCE_T = 5;

// executor
const SCREW_Y = 57;
const SCREW_Z = 16;
const SCREW_R = 3.5;
const SCREW_X0 = -25;
const SCREW_X1 = 60;
const SERVO = { x: -60, y: 42, w: 26, d: 26 };

// flight recorder: a circular chart, its face toward the viewer
const REC = { cx: 126, cy: -48, cz: 74, r: 22 };

// generations
const GEN = { cx: 5, cy: -46, cz: 68 };

const P = k.fitProjection(
  [
    ...k.boxCorners(PLATE, -PLATE_T - FOOT_H, 0),
    [COL_C[0], -18, DIAL_Z + DIAL_R + 8],
    [REC.cx, REC.cy, REC.cz + 34],
    [GEN.cx, GEN.cy, GEN.cz + 34],
    [GEN.cx + 40, GEN.cy, 100],
    [FEELER_L_X, 0, 96],
    [STYLUS_X, 0, PIVOT[2] + 10],
    [FBAR_X0, 0, 80],
  ],
  W,
  H,
  { pad: 22, azimuth: AZIMUTH, elevation: ELEVATION },
);

const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
const at = (p) => k.iso(p, P);
const S = (paths, style) => k.solidSvg(paths, style);
const L = (d, style) => k.lineSvg(d, style);
const D = (pts, style) => k.dotsSvg(pts, style);
const d3 = (pts, closed = false) => k.pathOf(pts.map(at), closed);
const byDepth = (pts) => [...pts].sort((a, b) => k.depthOf([a[0], a[1], 0], P) - k.depthOf([b[0], b[1], 0], P));

const R = A.recorder(P);
const rec = R.solid;
function put(name, ref, svg, shapes = [], extra = {}) {
  return R.put({ name, at: ref, svg, shapes, ...extra });
}
function solid(name, ref, paths, shape, style = {}, extra = {}) {
  put(name, ref, S(paths, style), [rec(shape)], extra);
}
function lines(name, ref, d, style = {}, extra = {}) {
  put(name, ref, L(d, style), [], extra);
}

function zcoil(cx, cy, z0, z1, r, turns, n = 12) {
  const count = Math.max(2, Math.round(turns * n));
  const back = [];
  const front = [];
  const backPoints = [];
  const frontPoints = [];
  const backFlat = [];
  const frontFlat = [];
  let run = [];
  let runPoints = [];
  let near = null;
  const flush = () => {
    (near ? front : back).push(k.pathOf(run));
    (near ? frontPoints : backPoints).push(runPoints);
    (near ? frontFlat : backFlat).push(run);
  };
  for (let i = 0; i <= count; i++) {
    const t = i / count;
    const a = t * turns * Math.PI * 2;
    const point = [cx + r * Math.cos(a), cy + r * Math.sin(a), z0 + (z1 - z0) * t];
    const vis = k.depthOf([Math.cos(a), Math.sin(a), 0], P) > 0;
    const flat = at(point);
    if (near === null) near = vis;
    if (vis !== near) {
      run.push(flat);
      runPoints.push(point);
      flush();
      run = [flat];
      runPoints = [point];
      near = vis;
      continue;
    }
    run.push(flat);
    runPoints.push(point);
  }
  flush();
  return { back: back.join(""), front: front.join(""), backPoints, frontPoints, backFlat, frontFlat };
}

const rodX = (x0, x1, y, z, r, steps = 40) => G.disc(0, x1 - x0, r, G.frameAlong([x0, y, z], [1, 0, 0]), P, { steps });
const rodY = (y0, y1, x, z, r, steps = 40) => G.disc(0, y1 - y0, r, G.frameAlong([x, y0, z], [0, 1, 0]), P, { steps });
const discShape = (x0, x1, y, z, r, name, owner) => rec(A.disc(G.frameAlong([x0, y, z], [1, 0, 0]), 0, x1 - x0, r, { name, owner }));

// ---------------------------------------------------------------------------
// base
// ---------------------------------------------------------------------------

function base() {

  byDepth(k.corners(PLATE, 26)).forEach(([x, y], i) => {
    put(`stand.foot.${i}.flange`, [x, y, -PLATE_T - FOOT_H], S(k.cylinder(x, y, 8, -PLATE_T - FOOT_H, 2, P, 26), { tone: "lo" }),
      [rec(A.disc(G.frameAlong([x, y, -PLATE_T - FOOT_H], [0, 0, 1]), 0, 2, 8, { name: "stand.foot", owner: "stand" }))]);
    put(`stand.foot.${i}`, [x, y, -PLATE_T - FOOT_H + 2], S(k.cylinder(x, y, 6, -PLATE_T - FOOT_H + 2, FOOT_H - 2, P, 26), { tone: "mid" }),
      [rec(A.disc(G.frameAlong([x, y, -PLATE_T - FOOT_H + 2], [0, 0, 1]), 0, FOOT_H - 2, 6, { name: "stand.foot", owner: "stand" }))]);
  });
  put("stand.plate", [0, 0, 0], `<path class="iso-halo" d="${k.haloOf(PLATE, -PLATE_T - FOOT_H, 16, P)}"/>` +
    S(k.slabOf(PLATE, -PLATE_T, PLATE_T, P, 10, 1.6), { tone: "mid" }) +
    L(k.planOutline(k.insetPlan(PLATE, 6), 0, P, 6), { tone: "lo" }) +
    k.corners(PLATE, 12).map(([x, y]) => L(k.ring(x, y, 2.4, 0, P, 16), { tone: "lo" })).join("") +
    D(k.corners(PLATE, 12).map(([x, y]) => at([x, y, 0])), { size: 0.5 }),
    [rec(A.slab(PLATE, -PLATE_T, PLATE_T, { name: "stand.plate", owner: "stand" }, 10))]);

  const LAB = { x: -180, y: 38, w: 66, d: 18, r: 2 };
  put("stand.label", [LAB.x + 33, LAB.y + 9, 0], S(k.slabOf(LAB, 0, 0.9, P, 4), { tone: "lo", crease: "none" }) +
    L(k.planOutline(k.insetPlan(LAB, 1.8), 0.9, P, 4), { tone: "faint" }) +
    k.faceTextSvg(k.topMatrix([LAB.x + 6, LAB.y + 3.2], 1.0, P, "x"), "ROVR", { size: 3.2, tone: "mid" }) +
    k.faceTextSvg(k.topMatrix([LAB.x + 6, LAB.y + 8.4], 1.0, P, "x"), "reconciler", { size: 2.0, tone: "faint" }) +
    k.faceTextSvg(k.topMatrix([LAB.x + 6, LAB.y + 13.6], 1.0, P, "x"), "model R-1", { size: 2.0, tone: "faint" }) +
    D([[LAB.x + 59, LAB.y + 4], [LAB.x + 59, LAB.y + 14]].map(([x, y]) => at([x, y, 0.9])), { size: 0.45 }),
    [rec(A.slab(LAB, 0, 0.9, { name: "stand.label", owner: "stand" }, 4))]);

  const STRIP = { x: -180, y: 58, w: 110, d: 8, r: 1 };
  const ticks = k.topTicks(STRIP.x + 4, STRIP.x + STRIP.w - 4, 4, 5, STRIP.y + 0.9, 0.6, [2.2, 3.8], P);
  put("stand.ruler", [STRIP.x + 55, STRIP.y + 4, 0], S(k.slabOf(STRIP, 0, 0.6, P, 3), { tone: "lo", crease: "none" }) +
    L(ticks.minor, { tone: "lo" }) + L(ticks.major, { tone: "mid" }) +
    D([[STRIP.x + 2, STRIP.y + 4], [STRIP.x + STRIP.w - 2, STRIP.y + 4]].map(([x, y]) => at([x, y, 0.6])), { size: 0.45 }),
    [rec(A.slab(STRIP, 0, 0.6, { name: "stand.ruler", owner: "stand" }, 3))]);
}

// ---------------------------------------------------------------------------
// wanted: the camshaft
// ---------------------------------------------------------------------------

const camTop = (phase) => CAM_Z + CAM_R + CAM_E * Math.cos(phase);
const avgCos = (phases) => phases.reduce((s, p) => s + Math.cos(p), 0) / NT;
const barUnderOf = (phases) => CAM_Z + CAM_R + CAM_E * avgCos(phases) + 2 * ROLLER_R + 4.5;
const desiredTopOf = (phases) => barUnderOf(phases) + FBAR_H;

function camFrames(phase, i) {
  const oy = CAM_E * Math.sin(phase);
  const oz = CAM_E * Math.cos(phase);
  return G.frameOf([CAM_XS[i], oy, CAM_Z + oz], [1, 0, 0], [0, 0, 1], [0, 1, 0]);
}
function camSvg(i, phase) {
  const F = camFrames(phase, i);
  const ribs = G.ribsOf(F, -CAM_T / 2 + 0.6, CAM_T / 2 - 0.6, CAM_R, 18, P, { least: 0.05, fade: [0.05, 0.4] });
  return S(G.disc(-CAM_T / 2, CAM_T / 2, CAM_R, F, P, { steps: 36, bevel: 0.7 }), { tone: "hi", crease: "lo" }) +
    (typeof ribs === "string" ? L(ribs, { tone: "faint" }) : k.fadedSvg(ribs, { tone: "faint" }));
}

function wanted() {
  const gaps = [SHAFT_X0];
  CAM_XS.forEach((cx) => { gaps.push(cx - CAM_T / 2); gaps.push(cx + CAM_T / 2); });
  gaps.push(SHAFT_X1);
  for (let i = 0; i < gaps.length; i += 2) {
    const [x0, x1] = [gaps[i], gaps[i + 1]];
    if (x1 - x0 < 0.5) continue;
    solid(`camshaft.shaft.${i / 2}`, [(x0 + x1) / 2, 0, CAM_Z], rodX(x0, x1, 0, CAM_Z, SHAFT_R), discShape(x0, x1, 0, CAM_Z, SHAFT_R, "camshaft.shaft", "camshaft"), { tone: "mid", crease: "none" });
  }
  [SHAFT_X0 + 4, SHAFT_X1 - 4].forEach((x, i) => {
    const b = { x: x - 5, y: -6, w: 10, d: 12, r: 2 };
    solid(`camshaft.bearing.${i}`, [x, 0, 15], k.slabOf(b, 0, CAM_Z - SHAFT_R, P, 4, 0.5), A.slab(b, 0, CAM_Z - SHAFT_R, { name: "camshaft.bearing", owner: "camshaft" }, 4), { tone: "lo" });
  });
  CAM_XS.forEach((cx, i) => {
    const phase = HOME_PHASES[i];
    put(`cam.${i}`, [cx, 0, CAM_Z], camSvg(i, phase), [rec(A.disc(camFrames(phase, i), -CAM_T / 2, CAM_T / 2, CAM_R, { name: `cam.${i}`, owner: "camshaft" }))], { attrs: ` data-part="cam.${i}"` });
    const top = camTop(phase);
    const F = G.frameAlong([cx, 0, top + ROLLER_R], [1, 0, 0]);
    put(`roller.${i}`, [cx, 0, top + ROLLER_R], S(G.disc(-3, 3, ROLLER_R, F, P, { steps: 24 }), { tone: "hi", crease: "lo" }),
      [rec(A.disc(F, -3, 3, ROLLER_R, { name: `roller.${i}`, owner: "camshaft" }))], { attrs: ` data-part="roller.${i}"` });
  });
  const under = barUnderOf(HOME_PHASES);
  const bar = { x: FBAR_X0, y: -FBAR_D / 2, w: FBAR_X1 - FBAR_X0, d: FBAR_D, r: 1.5 };
  put("bar", [0, 0, under + FBAR_H / 2], S(k.slabOf(bar, under, FBAR_H, P, 6, 0.6), { tone: "hi", crease: "lo" }),
    [rec(A.slab(bar, under, FBAR_H, { name: "bar", owner: "desired" }, 6))], { attrs: ` data-part="bar"` });
  [FBAR_X0, FBAR_X1].forEach((x, i) => {
    solid(`bar.rod.${i}.lo`, [x, 0, under / 2], k.cylinder(x, 0, 2.6, 0, under, P, 24), A.disc(G.frameAlong([x, 0, 0], [0, 0, 1]), 0, under, 2.6, { name: "bar.rod", owner: "desired" }), { tone: "mid" });
    solid(`bar.rod.${i}.hi`, [x, 0, under + FBAR_H + 5], k.cylinder(x, 0, 2.6, under + FBAR_H, 10, P, 24), A.disc(G.frameAlong([x, 0, under + FBAR_H], [0, 0, 1]), 0, 10, 2.6, { name: "bar.rod", owner: "desired" }), { tone: "mid" });
  });
  CAM_XS.forEach((cx, i) => {
    const top = camTop(HOME_PHASES[i]);
    solid(`roller.pin.${i}`, [cx, 0, (top + 2 * ROLLER_R + under) / 2], k.cylinder(cx, 0, 1, top + 2 * ROLLER_R, under - top - 2 * ROLLER_R, P, 16), A.disc(G.frameAlong([cx, 0, top + 2 * ROLLER_R], [0, 0, 1]), 0, under - top - 2 * ROLLER_R, 1, { name: "roller.pin", owner: "desired" }), { tone: "mid", crease: "none" });
  });
  const springBack = [];
  const springFront = [];
  const springBackShapes = [];
  const springFrontShapes = [];
  CAM_XS.forEach((cx, i) => {
    const top = camTop(HOME_PHASES[i]);
    const c = zcoil(cx, 0, top + 2 * ROLLER_R - 1, under - 0.5, 3, 5);
    springBack.push(c.backFlat.map((pts) => L(k.pathOf(pts), { tone: "lo", free: true })).join(""));
    springFront.push(c.frontFlat.map((pts) => L(k.pathOf(pts), { tone: "lo", free: true })).join(""));
    c.backPoints.forEach((pts) => springBackShapes.push(rec(A.tube(pts, 0.5, { name: "bar.spring", owner: "desired" }))));
    c.frontPoints.forEach((pts) => springFrontShapes.push(rec(A.tube(pts, 0.5, { name: "bar.spring", owner: "desired" }))));
  });
  put("bar.springs.back", [0, -20, under - 10], springBack.join(""), springBackShapes, { attrs: ` data-part="bar.springs.back"` });
  put("bar.springs.front", [0, 20, under - 6], springFront.join(""), springFrontShapes, { attrs: ` data-part="bar.springs.front"` });
}

// ---------------------------------------------------------------------------
// comparator
// ---------------------------------------------------------------------------

const BEAM_POLY = [[-5, -4], [5, -4], [5, 4], [-5, 4]];
const beamFrame = (theta) => G.frameOf(PIVOT, [Math.cos(theta), 0, Math.sin(theta)], [0, 1, 0], [-Math.sin(theta), 0, Math.cos(theta)]);
function beamSvg(theta) {
  const F = beamFrame(theta);
  const dots = [];
  for (let s = -ARM + 8; s <= ARM - 8; s += 12) {
    const p = G.pointOf(F, s, 0, Math.PI / 2);
    dots.push(at([p[0], p[1], p[2] + 0.2]));
  }
  return S(G.prismOf(F, BEAM_POLY, -ARM, ARM, P), { tone: "hi", crease: "lo" }) + D(dots, { size: 0.4, tone: "faint" });
}
function plungerSvg(x, y, z0, z1) {
  const c = zcoil(x, y, z0 + 5, Math.min(z1 - 3, z0 + 19), 3, 4);
  return L(c.back, { tone: "lo" }) +
    S(G.disc(0, Math.max(1, z1 - z0), 1.8, G.frameAlong([x, y, z0], [0, 0, 1]), P, { steps: 20 }), { tone: "hi", lit: true, crease: "none" }) +
    L(c.front, { tone: "lo" });
}

function comparator() {
  const c0 = { x: COL_C[0] - 16, y: COL_C[1] - 16, w: 32, d: 32, r: 3 };
  const c1 = { x: COL_C[0] - 12, y: COL_C[1] - 12, w: 24, d: 24, r: 3 };
  solid("column.base", [COL_C[0], COL_C[1], 5], k.slabOf(c0, 0, 10, P, 5, 0.6), A.slab(c0, 0, 10, { name: "column.base", owner: "comparator" }, 5), { tone: "mid" });
  put("column", [COL_C[0], COL_C[1], COL_TOP / 2], S(k.slabOf(c1, 10, COL_TOP - 10, P, 5, 0.6), { tone: "mid" }) +
    L(k.sideSeam(c1, 40, P, 5), { tone: "faint" }),
    [rec(A.slab(c1, 10, COL_TOP - 10, { name: "column", owner: "comparator" }, 5))]);
  const head = { x: COL_C[0] - 18, y: COL_C[1] - 18, w: 36, d: 36, r: 3 };
  put("head", [COL_C[0], COL_C[1], HEAD_TOP], S(k.slabOf(head, COL_TOP, HEAD_TOP - COL_TOP, P, 5, 0.8), { tone: "mid" }) +
    L(k.planOutline(k.insetPlan(head, 3), HEAD_TOP, P, 5), { tone: "faint" }) +
    D(k.corners(head, 6).map(([x, y]) => at([x, y, HEAD_TOP])), { size: 0.45 }),
    [rec(A.slab(head, COL_TOP, HEAD_TOP - COL_TOP, { name: "head", owner: "comparator" }, 5))]);

  const pivotBlock = { x: PIVOT[0] - 10, y: -8, w: 20, d: 13, r: 2 };
  put("pivot.block", [PIVOT[0], -1.5, 98], S(k.slabOf(pivotBlock, 96, 4, P, 4, 0.6), { tone: "mid" }),
    [rec(A.slab(pivotBlock, 96, 4, { name: "pivot.block", owner: "comparator" }, 4))]);

  const dialSvg = [S(k.cylinder(PIVOT[0], COL_C[1], DIAL_R, DIAL_Z, 10, P, 64, 0.8), { tone: "hi", crease: "lo" }),
    L(k.knurl(PIVOT[0], COL_C[1], DIAL_R, DIAL_Z + 5.6, DIAL_Z + 10, 84, P, 0.3), { tone: "lo" }),
    L(k.ring(PIVOT[0], COL_C[1], DIAL_R - 2.4, DIAL_Z + 10, P, 64), { tone: "lo" })];
  const span = (80 * Math.PI) / 180;
  const ticks = k.radialTicks(PIVOT[0], COL_C[1], DIAL_R - 3.4, 33, 5, DIAL_Z + 10, [1.4, 3], P, -Math.PI / 2 - span / 2, span);
  dialSvg.push(L(ticks.minor, { tone: "lo" }), L(ticks.major, { tone: "mid" }));
  dialSvg.push(L(k.ring(PIVOT[0], COL_C[1], 1.8, DIAL_Z + 10, P, 16), { tone: "mid" }));
  dialSvg.push(k.faceTextSvg(k.topMatrix([PIVOT[0], COL_C[1] + DIAL_R - 8], DIAL_Z + 10, P, "x"), "0", { size: 2.3, tone: "mid", anchor: "middle" }));
  put("dial", [PIVOT[0], COL_C[1], DIAL_Z], dialSvg.join(""),
    [rec(A.disc(G.frameAlong([PIVOT[0], COL_C[1], DIAL_Z], [0, 0, 1]), 0, 10, DIAL_R, { name: "dial", owner: "comparator" }))]);

  [FEELER_L_X, STYLUS_X].forEach((gx, i) => {
    const post = { x: gx - 4, y: 38, w: 8, d: 12, r: 1.6 };
    const arm = { x: gx - 4.5, y: 11, w: 9, d: 27, r: 1.6 };
    const capFront = { x: gx - 4.5, y: 7, w: 9, d: 31, r: 1.2 };
    put(`guide.bracket.${i}`, [gx, 20, 50],
      S(k.slabOf(post, 0, 88, P, 4, 0.5), { tone: "lo" }) +
      S(k.slabOf(arm, 86, 9, P, 4, 0.5), { tone: "mid" }) +
      S(k.slabOf(capFront, 95, 4, P, 3, 0.4), { tone: "mid" }),
      [rec(A.slab(post, 0, 88, { name: "guide.post", owner: "comparator" }, 4)),
        rec(A.slab(arm, 86, 9, { name: "guide.arm", owner: "comparator" }, 4)),
        rec(A.slab(capFront, 95, 4, { name: "guide.cap", owner: "comparator" }, 3))]);
    [[7, 3.5]].forEach(([y0, d0], j) => {
      const cheek = { x: gx - 4.5, y: y0, w: 9, d: d0, r: 0.8 };
      solid(`guide.cheek.${i}.${j}`, [gx, y0 + d0 / 2, 91.5], k.slabOf(cheek, 88, 7, P, 3, 0.4), A.slab(cheek, 88, 7, { name: "guide.cheek", owner: "comparator" }, 3), { tone: "mid" });
    });
  });
}

// ---------------------------------------------------------------------------
// reported: board, tiles, fence, feeler
// ---------------------------------------------------------------------------

const rowLeft = (e) => STOP_R - e;
const fenceX = (e) => STOP_R - e - FENCE_T;
const tileLeft = (e, i) => rowLeft(e) + i * (TILE_W + (e - PACKED) / (NT - 1));
const tileCenter = (e, i) => tileLeft(e, i) + TILE_W / 2;
const wedgeTopOf = (e) => RAMP_Z0 + RAMP_SLOPE * (STYLUS_X - (fenceX(e) - RAMP_BACK));

function tileSvg(e, i) {
  const left = tileLeft(e, i);
  const plan = { x: left, y: -TILE_D / 2, w: TILE_W, d: TILE_D, r: 1.4 };
  const out = [S(k.slabOf(plan, BOARD_TOP, TILE_H, P, 5, 0.5), { tone: "hi", crease: "lo" }),
    L(k.planOutline(k.insetPlan(plan, 2.2), TILE_TOP, P, 5), { tone: "faint" }),
    L(k.lineOnTop([left + 3, -3], [left + TILE_W - 3, -3], TILE_TOP, P), { tone: "lo" })];
  for (const dy of [1.5, 5]) {
    out.push(L(k.onTop([[left + 3, dy], [left + TILE_W - 3, dy]], TILE_TOP, P), { tone: "faint", free: true }));
  }
  out.push(D([at([left + 3, TILE_D / 2 - 3, TILE_TOP]), at([left + TILE_W - 3, TILE_D / 2 - 3, TILE_TOP])], { size: 0.4 }));
  return out.join("");
}
const tileShape = (e, i) => A.slab({ x: tileLeft(e, i), y: -TILE_D / 2, w: TILE_W, d: TILE_D, r: 1.4 }, BOARD_TOP, TILE_H, { name: `tile.${i}`, owner: "windows" }, 5);

function fenceParts(e) {
  const f = fenceX(e);
  const plate = { x: f, y: -9, w: FENCE_T, d: 18, r: 1.2 };
  const web = { x: f - 4, y: -5, w: 9, d: 10, r: 1.2 };
  const cap = { x: f - RAMP_BACK / 2, y: -7, w: RAMP_BACK + 10, d: 14, r: 1.4 };
  const poly = [[0, 0], [RAMP_LEN, 0], [RAMP_LEN, RAMP_LEN * RAMP_SLOPE]];
  const RF = G.frameOf([f - RAMP_BACK, 0, RAMP_Z0], [0, 1, 0], [1, 0, 0], [0, 0, 1]);
  return [
    { name: "fence.plate", plan: plate, z: 22, h: 12, svg: S(k.slabOf(plate, 22, 12, P, 3, 0.5), { tone: "mid" }), shape: A.slab(plate, 22, 12, { name: "fence.plate", owner: "fence" }, 3) },
    { name: "fence.web", plan: web, z: 34, h: 15.25, svg: S(k.slabOf(web, 34, 15.25, P, 3, 0.4), { tone: "mid" }), shape: A.slab(web, 34, 15.25, { name: "fence.web", owner: "fence" }, 3) },
    { name: "fence.cap", plan: cap, z: 49.25, h: 6, svg: S(k.slabOf(cap, 49.25, 6, P, 4, 0.5), { tone: "mid" }), shape: A.slab(cap, 49.25, 6, { name: "fence.cap", owner: "fence" }, 4) },
    { name: "fence.ramp", plan: null, z: RAMP_Z0, h: RAMP_LEN * RAMP_SLOPE, svg: S(G.prismOf(RF, poly, -8, 0, P), { tone: "hi", crease: "lo" }), shape: A.prism(RF, poly, -8, 0, { name: "fence.ramp", owner: "fence" }) },
  ];
}

function board(e) {
  put("board", [BOARD.x + BOARD.w / 2, 0, BOARD_TOP / 2], S(k.slabOf(BOARD, 0, BOARD_TOP, P, 6, 0.8), { tone: "mid" }) +
    L(k.planOutline(k.insetPlan(BOARD, 4), BOARD_TOP, P, 6), { tone: "lo" }) +
    D(k.corners(BOARD, 8).map(([x, y]) => at([x, y, BOARD_TOP])), { size: 0.45 }),
    [rec(A.slab(BOARD, 0, BOARD_TOP, { name: "board", owner: "board" }, 6))]);
  [-20, 15].forEach((y, i) => {
    const rail = { x: -24, y, w: 186, d: 5, r: 1 };
    solid(`board.rail.${i}`, [-24 + 93, y + 2.5, BOARD_TOP + 1.5], k.slabOf(rail, BOARD_TOP, 3, P, 3, 0.4), A.slab(rail, BOARD_TOP, 3, { name: "board.rail", owner: "board" }, 3), { tone: "mid" });
  });
  const ls = { x: -28, y: -7, w: 5, d: 14, r: 1.2 };
  solid("board.stop.l", [-25.5, 0, BOARD_TOP + 4], k.slabOf(ls, BOARD_TOP, 8, P, 3, 0.4), A.slab(ls, BOARD_TOP, 8, { name: "board.stop.l", owner: "board" }, 3), { tone: "mid" });
  const rs = { x: STOP_R, y: -7, w: 7, d: 14, r: 1.4 };
  solid("board.stop.r", [STOP_R + 3.5, 0, BOARD_TOP + 6], k.slabOf(rs, BOARD_TOP, 12, P, 3, 0.5), A.slab(rs, BOARD_TOP, 12, { name: "board.stop.r", owner: "board" }, 3), { tone: "mid" });

  for (let i = 0; i < NT; i++) {
    put(`tile.${i}`, [tileCenter(e, i), 0, TILE_TOP], tileSvg(e, i), [rec(tileShape(e, i))], { attrs: ` data-part="tile.${i}"` });
  }
  fenceParts(e).forEach((part) => {
    put(part.name, [part.plan ? part.plan.x + (part.plan.w || 0) / 2 : fenceX(e) - RAMP_BACK / 2, 0, part.z], part.svg, [rec(part.shape)], { attrs: ` data-part="${part.name}"` });
  });
}

// ---------------------------------------------------------------------------
// executor
// ---------------------------------------------------------------------------

function executor() {
  const box = { x: SERVO.x, y: SERVO.y, w: SERVO.w, d: SERVO.d, r: 3 };
  solid("servo", [SERVO.x + SERVO.w / 2, SERVO.y + SERVO.d / 2, 13], k.slabOf(box, 0, 26, P, 5, 0.8), A.slab(box, 0, 26, { name: "servo", owner: "servo" }, 5), { tone: "mid" });
  put("servo.detail", [SERVO.x + SERVO.w / 2, SERVO.y + SERVO.d / 2, 26], L(k.planOutline(k.insetPlan(box, 3), 26, P, 5), { tone: "faint" }) +
    D(k.corners(box, 5).map(([x, y]) => at([x, y, 26])), { size: 0.45 }), [rec(A.slab(box, 0, 26, { name: "servo.cover", owner: "servo" }, 5))], { bias: 2 });
  const flangeF = G.frameAlong([SERVO.x + SERVO.w, SCREW_Y, SCREW_Z], [1, 0, 0]);
  put("servo.flange", [SERVO.x + SERVO.w, SCREW_Y, SCREW_Z], S(G.disc(0, 3, 11, flangeF, P, { steps: 32 }), { tone: "mid" }) +
    D(G.dotsOf(flangeF, 3, 8, 6, P, { all: true }), { size: 0.45 }),
    [rec(A.disc(flangeF, 0, 3, 11, { name: "servo.flange", owner: "servo" }))]);
  put("coupling.a", [SERVO.x + SERVO.w + 4.5, SCREW_Y, SCREW_Z], S(G.disc(3, 6, 6.5, flangeF, P, { steps: 28 }), { tone: "hi" }),
    [rec(A.disc(flangeF, 3, 6, 6.5, { name: "coupling.a", owner: "coupling" }))]);
  put("coupling.b", [SERVO.x + SERVO.w + 8, SCREW_Y, SCREW_Z], S(G.disc(6, 9, 6.5, flangeF, P, { steps: 28 }), { tone: "hi" }),
    [rec(A.disc(flangeF, 6, 9, 6.5, { name: "coupling.b", owner: "coupling" }))]);

  put("screw", [(SCREW_X0 + SCREW_X1) / 2, SCREW_Y, SCREW_Z], S(rodX(SCREW_X0, SCREW_X1, SCREW_Y, SCREW_Z, SCREW_R), { tone: "hi", crease: "none" }),
    [discShape(SCREW_X0, SCREW_X1, SCREW_Y, SCREW_Z, SCREW_R, "screw", "screw")]);
  [SCREW_Y - 13, SCREW_Y + 9].forEach((y, i) => {
    const plate = { x: SCREW_X1 - 5, y, w: 10, d: 4, r: 1 };
    solid(`screw.stand.${i}`, [SCREW_X1, y + 2, 10], k.slabOf(plate, 0, 20, P, 3, 0.4), A.slab(plate, 0, 20, { name: "screw.stand", owner: "screw" }, 3), { tone: "mid" });
  });
  const cap = { x: SCREW_X1 - 5, y: SCREW_Y - 13, w: 10, d: 26, r: 1 };
  solid("screw.stand.cap", [SCREW_X1, SCREW_Y, 22], k.slabOf(cap, 20, 4, P, 3, 0.4), A.slab(cap, 20, 4, { name: "screw.stand.cap", owner: "screw" }, 3), { tone: "mid" });
  const foot = { x: SCREW_X1 - 5, y: SCREW_Y - 9, w: 10, d: 18, r: 1 };
  solid("screw.foot", [SCREW_X1, SCREW_Y, 3], k.slabOf(foot, 0, 6, P, 3, 0.4), A.slab(foot, 0, 6, { name: "screw.foot", owner: "screw" }, 3), { tone: "mid" });
}

function nut(e) {
  const f = fenceX(e);
  const nx = f - 8;
  const top = { x: nx - 4, y: SCREW_Y - 7, w: 8, d: 14, r: 1.2 };
  const cheekA = { x: nx - 4, y: SCREW_Y - 7, w: 8, d: 3, r: 0.8 };
  const cheekB = { x: nx - 4, y: SCREW_Y + 4, w: 8, d: 3, r: 0.8 };
  const riser = { x: nx - 3, y: SCREW_Y + 7, w: 6, d: 6, r: 1.2 };
  const arm = { x: nx + 4, y: 0, w: f - (nx + 4), d: SCREW_Y, r: 1.4 };
  put("nut", [nx, SCREW_Y, SCREW_Z + 8], S(k.slabOf(top, 20, 8, P, 3, 0.5), { tone: "hi", crease: "lo" }) +
    S(k.slabOf(cheekA, 12, 8, P, 3, 0.3), { tone: "mid" }) +
    S(k.slabOf(cheekB, 12, 8, P, 3, 0.3), { tone: "mid" }) +
    S(k.slabOf(riser, 20, 10, P, 3, 0.4), { tone: "mid" }) +
    S(k.slabOf(arm, 22, 6, P, 4, 0.4), { tone: "mid" }),
    [rec(A.slab(top, 20, 8, { name: "nut.top", owner: "screw" }, 3)),
      rec(A.slab(riser, 20, 10, { name: "nut.riser", owner: "screw" }, 3)),
      rec(A.slab(arm, 22, 6, { name: "nut.arm", owner: "screw" }, 4))], { attrs: ` data-part="nut"` });
  put("nut.cheek.a", [nx, SCREW_Y - 5.5, 16], S(k.slabOf(cheekA, 12, 8, P, 3, 0.3), { tone: "mid" }),
    [rec(A.slab(cheekA, 12, 8, { name: "nut.cheek", owner: "screw" }, 3))], { attrs: ` data-part="nut.cheek.a"` });
  put("nut.cheek.b", [nx, SCREW_Y + 5.5, 16], S(k.slabOf(cheekB, 12, 8, P, 3, 0.3), { tone: "mid" }),
    [rec(A.slab(cheekB, 12, 8, { name: "nut.cheek", owner: "screw" }, 3))], { attrs: ` data-part="nut.cheek.b"` });
}

// ---------------------------------------------------------------------------
// flight recorder: a circular chart facing the viewer
// ---------------------------------------------------------------------------

function recorder() {
  const f = G.frameAlong([REC.cx, REC.cy, REC.cz], [0, 0, 1]);
  put("chart.body", [REC.cx, REC.cy, REC.cz + 8], S(G.disc(0, 16, REC.r, f, P, { steps: 48, bevel: 0.7 }), { tone: "mid", crease: "lo" }),
    [rec(A.disc(f, 0, 16, REC.r, { name: "chart.body", owner: "recorder" }))]);
  const grid = [];
  for (const r of [7.5, 14, 20]) grid.push(L(k.ring(REC.cx, REC.cy, r, REC.cz + 17.1, P, 48), { tone: "faint" }));
  put("chart.paper", [REC.cx, REC.cy, REC.cz + 16], S(G.disc(16, 16.8, REC.r - 1, f, P, { steps: 48 }), { tone: "hi", crease: "none" }) +
    grid.join("") + chartSpokes(0),
    [rec(A.disc(f, 16, 16.8, REC.r - 1, { name: "chart.paper", owner: "recorder" }))], { attrs: ` data-part="chart.grid"` });
  const post = { x: REC.cx - 5, y: REC.cy - 5, w: 10, d: 10, r: 2 };
  solid("chart.post", [REC.cx, REC.cy, REC.cz / 2], k.slabOf(post, 0, REC.cz, P, 4, 0.5), A.slab(post, 0, REC.cz, { name: "chart.post", owner: "recorder" }, 4), { tone: "lo" });
}

function chartSpokes(phase) {
  const out = [];
  for (let i = 0; i < 12; i++) {
    const a = (i / 12) * Math.PI * 2 + phase;
    out.push(L(k.lineOnTop([REC.cx + Math.cos(a) * 7.5, REC.cy + Math.sin(a) * 7.5], [REC.cx + Math.cos(a) * 20.5, REC.cy + Math.sin(a) * 20.5], REC.cz + 17.1, P), { tone: i % 3 === 0 ? "lo" : "faint" }));
  }
  return out.join("");
}

function pen(e) {
  const reach = clamp(9 + (e - E_HOME) * 0.55, 3, 19);
  const arm = { x: REC.cx - REC.r + 3, y: REC.cy - 2.5, w: REC.r - 3, d: 5, r: 1.2 };
  const head = { x: REC.cx - reach - 3, y: REC.cy - 4, w: 6, d: 8, r: 1.4 };
  put("pen", [REC.cx - reach, REC.cy, REC.cz + 21], S(k.slabOf(arm, REC.cz + 16.8, 3, P, 3, 0.3), { tone: "mid" }) +
    S(k.slabOf(head, REC.cz + 19.8, 6, P, 3, 0.4), { tone: "hi" }) +
    S(G.disc(0, 3.4, 0.8, G.frameAlong([REC.cx - reach, REC.cy, REC.cz + 19.8], [0, 0, -1]), P, { steps: 16 }), { tone: "hi", lit: true }),
    [rec(A.slab(arm, REC.cz + 16.8, 3, { name: "pen.arm", owner: "recorder" }, 3)),
      rec(A.slab(head, REC.cz + 19.8, 6, { name: "pen", owner: "recorder" }, 3))], { attrs: ` data-part="pen"` });
}

// ---------------------------------------------------------------------------
// generations: counter, pawl and the lever that lifts the feeler
// ---------------------------------------------------------------------------

function generations() {
  const post = { x: GEN.cx - 4, y: GEN.cy - 4, w: 8, d: 8, r: 1.6 };
  solid("gen.post", [GEN.cx, GEN.cy, 34], k.slabOf(post, 0, 68, P, 4, 0.5), A.slab(post, 0, 68, { name: "gen.post", owner: "generation" }, 4), { tone: "lo" });
  const box = { x: GEN.cx - 26, y: GEN.cy - 14, w: 52, d: 20, r: 3 };
  const windowSvg = [];
  const windowShapes = [rec(A.slab(box, GEN.cz - 12, 24, { name: "gen.counter", owner: "generation" }, 5))];
  const wy0 = GEN.cy + 8, wz0 = GEN.cz - 5, wz1 = GEN.cz + 8;
  const digits = [L(k.segment([GEN.cx - 20, wy0, wz0], [GEN.cx + 20, wy0, wz0], P), { tone: "lo" }),
    L(k.segment([GEN.cx + 20, wy0, wz0], [GEN.cx + 20, wy0, wz1], P), { tone: "lo" }),
    L(k.segment([GEN.cx + 20, wy0, wz1], [GEN.cx - 20, wy0, wz1], P), { tone: "lo" }),
    L(k.segment([GEN.cx - 20, wy0, wz1], [GEN.cx - 20, wy0, wz0], P), { tone: "lo" })];
  for (let i = 0; i < 3; i++) {
    const dx = GEN.cx - 13 + i * 13;
    digits.push(k.faceTextSvg(k.sideMatrix([dx, wy0], GEN.cz + 3, P, "left"), String((i * 3 + 1) % 10), { size: 5, tone: "hi", anchor: "middle" }));
    if (i) digits.push(L(k.segment([dx - 6.5, wy0, wz0], [dx - 6.5, wy0, wz1], P), { tone: "faint" }));
  }
  put("gen.counter", [GEN.cx, GEN.cy - 4, GEN.cz], S(k.slabOf(box, GEN.cz - 12, 24, P, 5, 0.8), { tone: "mid" }) +
    digits.join(""), windowShapes, { bias: 5 });
  const leverPost = { x: 36, y: GEN.cy - 4, w: 8, d: 8, r: 1.6 };
  solid("gen.lever.post", [40, GEN.cy, 21], k.slabOf(leverPost, 0, 42, P, 4, 0.5), A.slab(leverPost, 0, 42, { name: "gen.lever.post", owner: "generation" }, 4), { tone: "lo" });
  const leverBase = { x: 36, y: GEN.cy - 4, w: 8, d: 8, r: 1.6 };
  solid("gen.lever.base", [40, GEN.cy, 47], k.slabOf(leverBase, 42, 10, P, 4, 0.5), A.slab(leverBase, 42, 10, { name: "gen.lever.base", owner: "generation" }, 4), { tone: "mid" });
  const leverF = G.frameAlong([40, GEN.cy, 52], [0, 0.18, 1]);
  put("gen.lever.rod", [40, GEN.cy, 77], S(G.disc(0, 50, 2.6, leverF, P, { steps: 20 }), { tone: "mid" }),
    [rec(A.disc(leverF, 0, 50, 2.6, { name: "gen.lever.rod", owner: "generation" }))]);
  const knobF = G.frameAlong([40, GEN.cy + 4, 102], [0, 0.18, 1]);
  put("gen.lever.knob", [40, GEN.cy + 4, 107], S(G.disc(0, 10, 5, knobF, P, { steps: 24 }), { tone: "hi" }),
    [rec(A.disc(knobF, 0, 10, 5, { name: "gen.lever.knob", owner: "generation" }))]);
  const pawl = { x: GEN.cx + 9, y: GEN.cy - 4, w: 18, d: 6, r: 1.4 };
  put("gen.pawl", [GEN.cx + 18, GEN.cy - 1, GEN.cz - 18], S(k.slabOf(pawl, GEN.cz - 18, 6, P, 4, 0.4), { tone: "mid" }),
    [rec(A.slab(pawl, GEN.cz - 18, 6, { name: "gen.pawl", owner: "generation" }, 4))], { attrs: ` data-part="gen.pawl"` });
  const linkA = [GEN.cx + 27, GEN.cy, GEN.cz - 18], linkB = [36, GEN.cy, 51];
  put("gen.link", [GEN.cx + 20, GEN.cy, GEN.cz + 6], L(k.segment(linkA, linkB, P), { tone: "lo" }),
    [rec(A.tube([linkA, linkB], 0.5, { name: "gen.link", owner: "generation" }))]);
  const liftA = [STYLUS_X + 2, STYLUS_Y, 72], liftB = [42, -26, 70], liftC = [42.6, GEN.cy, 62];
  put("gen.lift", [STYLUS_X + 12, -22, 74], L(k.segment(liftA, liftB, P), { tone: "lo" }) + L(k.segment(liftB, liftC, P), { tone: "lo" }),
    [rec(A.tube([liftA, liftB], 0.5, { name: "gen.lift", owner: "generation" })), rec(A.tube([liftB, liftC], 0.5, { name: "gen.lift", owner: "generation" }))]);
}

// ---------------------------------------------------------------------------
// assemble
// ---------------------------------------------------------------------------

base();
wanted();
comparator();
board(E_HOME);
executor();
nut(E_HOME);
recorder();
pen(E_HOME);
generations();

const under0 = barUnderOf(HOME_PHASES);
const zd0 = desiredTopOf(HOME_PHASES);
const zr0 = wedgeTopOf(E_HOME);
const diff0 = zd0 - zr0;
const theta0 = -Math.asin(Math.max(-0.32, Math.min(0.32, diff0 / (2 * ARM))));
const zEndL0 = PIVOT[2] + diff0 / 2;
const zEndR0 = PIVOT[2] - diff0 / 2;

put("beam", PIVOT, beamSvg(theta0), [rec(A.prism(beamFrame(0), BEAM_POLY, -ARM, ARM, { name: "beam", owner: "comparator" }))], { attrs: ` data-part="beam"` });
put("plunger.l", [FEELER_L_X, 0, (zd0 + zEndL0) / 2], plungerSvg(FEELER_L_X, 0, zd0, zEndL0 - 4),
  [rec(A.disc(G.frameAlong([FEELER_L_X, 0, zd0], [0, 0, 1]), 0, Math.max(1, zEndL0 - 4 - zd0), 1.8, { name: "plunger.l", owner: "comparator" }))], { attrs: ` data-part="plunger.l"` });
put("plunger.r", [STYLUS_X, STYLUS_Y, (zr0 + zEndR0) / 2], plungerSvg(STYLUS_X, STYLUS_Y, zr0, zEndR0 - 4),
  [rec(A.disc(G.frameAlong([STYLUS_X, STYLUS_Y, zr0], [0, 0, 1]), 0, Math.max(1, zEndR0 - 4 - zr0), 1.8, { name: "plunger.r", owner: "comparator" }))], { attrs: ` data-part="plunger.r"` });
const pointerAngle = (theta) => -8 * theta;
const pointerD = (a) => {
  const wy = COL_C[1];
  return k.lineOnTop([PIVOT[0] - Math.sin(a) * 3, wy + Math.cos(a) * 3],
    [PIVOT[0] + Math.sin(a) * (DIAL_R - 5), wy - Math.cos(a) * (DIAL_R - 5)], DIAL_Z + 10.2, P);
};
{
  const a0 = pointerAngle(theta0);
  const wy0 = COL_C[1];
  const tail = [PIVOT[0] - Math.sin(a0) * 3, wy0 + Math.cos(a0) * 3, DIAL_Z + 10.2];
  const tip = [PIVOT[0] + Math.sin(a0) * (DIAL_R - 5), wy0 - Math.cos(a0) * (DIAL_R - 5), DIAL_Z + 10.2];
  put("pointer", [PIVOT[0], COL_C[1], DIAL_Z + 10],
    `<path data-part="pointer" class="iso-line" data-tone="lit" data-free="" d="${pointerD(a0)}" style="stroke-width:1.2"/>`,
    [rec(A.tube([tail, tip], 0.45, { name: "pointer", owner: "comparator" }))]);
}

A.settle(R, P);
R.items.sort((a, b) => a.key - b.key);
const groups = R.items.map((item) => `<g class="it" data-name="${item.name}"${item.attrs ?? ""}>${item.svg}</g>`).join("");

const LABEL_TEXT =
  "A bench-top reconciliation instrument on a granite plate. Five eccentric cams under a sprung follower bar set the layout that is wanted. Five window tiles on the board are packed by a fence driven through a plain coupling by a lead screw, and a feeler riding the fence's ramp reads where they actually are. A comparator beam on a column presses down on both feelers; its tilt drives a dial, the screw and a circular chart recorder. A counter and a pawl at the back bump the generation and lift the feeler to re-observe.";

const svg = k.figureSvg({ width: W, height: H, label: LABEL_TEXT, body: [groups] });

const DATA = {
  P,
  camXs: CAM_XS, camZ: CAM_Z, camR: CAM_R, camE: CAM_E, camT: CAM_T, rollerR: ROLLER_R,
  fbarH: FBAR_H, fbarD: FBAR_D, fbarX0: FBAR_X0, fbarX1: FBAR_X1,
  pivot: PIVOT, arm: ARM, colC: COL_C, dialZ: DIAL_Z, dialR: DIAL_R, feelerL: FEELER_L_X,
  board: BOARD, boardTop: BOARD_TOP, tileW: TILE_W, tileD: TILE_D, tileH: TILE_H, tileTop: TILE_TOP,
  nt: NT, stopR: STOP_R, packed: PACKED, eMin: E_MIN, eMax: E_MAX, eHome: E_HOME,
  slope: RAMP_SLOPE, rampLen: RAMP_LEN, rampBack: RAMP_BACK, rampZ0: RAMP_Z0, stylusX: STYLUS_X, stylusY: STYLUS_Y, fenceT: FENCE_T,
  screwY: SCREW_Y, screwZ: SCREW_Z, screwR: SCREW_R, screwX0: SCREW_X0, screwX1: SCREW_X1,
  rec: REC, gen: GEN, homePhases: HOME_PHASES,
};

const latheSource = readFileSync(join(SKILL, "kit/lathe.mjs"), "utf8")
  .replace(/^import .*$/m, "")
  .replace(/^export /gm, "");
const latheScript = `const G = (() => {${latheSource}\nreturn { frameAlong, frameOf, pointOf, disc, ringBand, solidOf, prismOf, sphereOf, circleOf, arcOf, ribsOf, dotsOf, viewOf, scaleOf, dot3, add3, sub3, mul3, unit3, len3, lightOf, toneOf, radialOf, bandsOf };})();\n`;
const LIVE = readFileSync(join(HERE, "live.js"), "utf8").replace("__DATA__", JSON.stringify(DATA));

const body = k.plateHtml({
  fig: "Fig 1",
  title: "Reconciler bench",
  hint: "Drag the cams and the windows",
  readout: "reconciled · 5 windows",
  keys: [
    { mark: "lit", label: "The comparator beam and the two feelers: the beam tilts only by the difference between wanted and actual" },
    { mark: "raised", label: "Five cams under the sprung follower bar: the layout that is wanted" },
    { mark: "edge", label: "The plain coupling between servo and screw: the platform boundary into macOS" },
    { mark: "dotted", label: "The circular chart records every correction; press G and the counter and pawl bump the generation" },
  ],
  caption:
    "Rovr keeps two models apart: what macOS reports and what the layout engine wants. Here the wanted side is a sprung bar resting on five cams, the reported side a feeler riding a ramp the fence carries, and the comparator beam between them tilts only by the difference. That tilt drives a dial and, through a plain coupling, a lead screw that packs the window tiles until the two agree. Nothing assumes the screen is where it was left: drag a tile out of place and the loop closes it; pull the lever and the counter ticks, the feeler lifts off the ramp, and the machine re-observes before it acts again. The chart is the flight recorder.",
  body: `<style>@media (max-width:560px){.rc-stage{transform:scale(1.08);transform-origin:50% 42%}.iso-plate{padding:46px 12px 78px}}</style><div class="iso-stage rc-stage" tabindex="0" role="group" aria-label="Reconciler bench. Drag a cam to change the wanted layout, drag a window tile to simulate macOS moving it, press G to bump the generation, Home to pack the row.">${svg}<span class="rc-sr" role="status" aria-live="polite" data-status></span></div>`,
});

const page = k.pageHtml({ title: "Rovr: the reconciler bench", theme: THEME, body, script: kitScript() + latheScript + LIVE, width: 780 });
const out = join(HERE, THEME === "light" ? "rovr-reconciler-light.html" : "rovr-reconciler.html");
writeFileSync(out, page);
console.log("wrote", out, `${(page.length / 1024).toFixed(0)} KB`, `${R.items.length} items`, `scale ${G.scaleOf(P).toFixed(3)}`, `E=${E_HOME} zo=${zr0.toFixed(1)} zd=${zd0.toFixed(1)}`);

if (process.argv.includes("--at")) {
  const marks = { counter: [GEN.cx, GEN.cy - 4, GEN.cz], chart: [(REC.x0+REC.x1)/2, REC.y, REC.z + 20], cam: [CAM_XS[2], 0, CAM_Z], fence: [fenceX(E_HOME), 0, 40], beam: PIVOT, dial: [PIVOT[0], COL_C[1], DIAL_Z + 10], tiles: [tileCenter(E_HOME, 2), 0, TILE_TOP], plate: [0, 0, 0] };
  for (const [name, point] of Object.entries(marks)) console.log(name, at(point).map((v) => v.toFixed(1)).join(","));
  process.exit(0);
}

A.auditOrExit(R, P, { allow: [["bar.springs.back", "roller.4"]] });
