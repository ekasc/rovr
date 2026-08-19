# M3 Window Manager Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn rovr from an observation/mutation tool into a tiling window manager by driving the existing pure `rovr-layout` engine from `DesiredState`, then layering workspace, rule, and scratchpad semantics on top.

**Architecture:** M3 is decomposed into six independently-shippable sub-phases (M3a–M3f). M3a is the foundation: it runs `rovr_layout::compute` every reconcile cycle over the observed windows of each Space and writes the resulting frames into `DesiredState.windows[].frame`; the existing `reconcile` already emits `SetWindowFrame` for those targets. Later phases add stateful BSP mutation, reactive rules, named workspaces, scratchpads, and persistence. Each phase is verifiable through `MockPlatform` plus a contained live test.

**Tech Stack:** Rust workspace; `rovr-layout` (pure, zero macOS deps), `rovr-core` (Engine / DesiredState / reconcile / layout wiring), `rovr-config` (Config), `rovr-platform` (MacPlatform at runtime, MockPlatform for tests). macOS mutation only via `rovr-platform`.

## Global Constraints

- macOS state is untrusted; never assume a mutation succeeded; re-observe/verify after sensitive ops (PRODUCT.md Core Principle).
- Capabilities, not OS-name checks, belong in core.
- Surgical changes; no new dependencies unless they cut complexity; prefer boring explicit code (AGENTS.md).
- `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` must pass for every task/phase.
- **Recompute `DesiredState.windows` fully from `ObservedState` each cycle** (idempotent; no accumulation / no stale entries).
- **Tile-in-place:** M3a writes only `frame`, never `space` — no cross-Space moves, no move-then-frame ordering hazards.
- **Skip floating and fullscreen windows** from tiling (leave their `frame` target `None`).
- Engine gains a `config: Config` field; daemon sets it at startup and on reload.

---

## Phase overview

| Phase | Delivers | Depends on | Risk level |
|-------|----------|------------|------------|
| **M3a** | Global layout tiling wired into desired state | — (foundation) | Low |
| **M3b** | Stateful BSP mutation: insert/remove/rotate/mirror | M3a | Medium |
| **M3c** | Reactive rules (float now; workspace-assign later) | M3a (float), M3d (assign) | Medium |
| **M3d** | Named workspaces bound to macOS Spaces | M3a; M3f (persist) | Medium |
| **M3e** | Scratchpads (toggle special workspace) | M3d | Medium |
| **M3f** | Persistent workspace restoration | M3d, M3b (layout state) | High (needs persistence layer) |

Each phase is its own detailed plan when executed. **M3a is detailed below**; M3b–M3f are scoped with interface + risk so their plans can be written without re-deriving context.

---

## M3a: Global layout tiling (fully detailed)

**Goal:** Every observed, managed (non-floating, non-fullscreen) window is tiled within its current macOS Space's display area on every reconcile cycle, by writing `frame` targets into `DesiredState`. Existing `reconcile` already emits nothing for layout — it emits `SetWindowFrame` for desired frames, so no new `Action` variant is needed.

### Task M3a-1: Add `config` to `Engine`

**Files:**
- Modify: `crates/rovr-core/src/engine.rs` (struct + `new` + `Default`)
- Modify: `crates/rovr-core/Cargo.toml` (ensure `rovr-config` and `rovr-layout` are deps)

**Interfaces:**
- Consumes: `rovr_config::Config` (must derive `Default`), `rovr_layout::compute`, `rovr_layout::LayoutRequest`, `rovr_layout::LayoutKind`, `rovr_types::{Rect, WindowId, SpaceId, DisplayId, WindowSnapshot, SpaceSnapshot, DisplaySnapshot}`
- Produces: `Engine { observed, desired, flight_recorder, config: Config }`

- [ ] **Step 1: Write the failing test**

```rust
// crates/rovr-core/src/engine.rs (inside #[cfg(test)])
#[test]
fn engine_requires_config_field() {
    let engine = Engine::new(Config::default());
    assert_eq!(engine.config, Config::default());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rovr-core engine_requires_config_field`
Expected: FAIL (no `Engine::new` / no `config` field)

- [ ] **Step 3: Write minimal implementation**

```rust
// crates/rovr-core/src/engine.rs
use rovr_config::Config;
// ... existing imports ...

#[derive(Debug, Default)]
pub struct Engine {
    pub observed: ObservedState,
    pub desired: DesiredState,
    pub flight_recorder: FlightRecorder,
    pub config: Config,
}

impl Engine {
    pub fn new(config: Config) -> Self {
        Self {
            observed: ObservedState::default(),
            desired: DesiredState::default(),
            flight_recorder: FlightRecorder::default(),
            config,
        }
    }
    // ... existing methods unchanged ...
}
```

Ensure `crates/rovr-core/Cargo.toml` lists `rovr-config` and `rovr-layout` (add if absent; verify with `cargo tree -p rovr-core | grep -E 'rovr-config|rovr-layout'`).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rovr-core engine_requires_config_field`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/rovr-core/src/engine.rs crates/rovr-core/Cargo.toml
git commit -m "core: give Engine a config field for layout wiring"
```

### Task M3a-2: `apply_layout` module

**Files:**
- Create: `crates/rovr-core/src/layout.rs`
- Modify: `crates/rovr-core/src/lib.rs` (publish `pub mod layout;`)

**Interfaces:**
- Consumes: `Config`, `ObservedState`, `DesiredState`, `rovr_layout::{compute, LayoutRequest, LayoutKind}`, `rovr_types::{DisplayId, Rect, SpaceId, WindowId, WindowSnapshot}`
- Produces: `pub fn apply_layout(config: &Config, observed: &ObservedState, desired: &mut DesiredState)`

- [ ] **Step 1: Write the failing test**

```rust
// crates/rovr-core/src/layout.rs (inside #[cfg(test)])
#[test]
fn apply_layout_tiles_managed_windows_and_skips_fullscreen() {
    use rovr_types::{
        DisplayId, DisplaySnapshot, LayoutKind, ProcessId, Rect, SpaceId, SpaceSnapshot,
        WindowId, WindowSnapshot,
    };

    let mut observed = ObservedState::default();
    let d1 = DisplayId(1);
    let s1 = SpaceId(11);
    observed.displays.insert(
        d1,
        DisplaySnapshot {
            id: d1,
            frame: Rect { x: 0.0, y: 0.0, width: 1440.0, height: 900.0 },
            label: None,
            focused: false,
            generation: 0,
        },
    );
    observed.spaces.insert(
        s1,
        SpaceSnapshot {
            id: s1,
            display_id: d1,
            label: None,
            focused: false,
            generation: 0,
            position: 0,
        },
    );
    let mk = |id: u32, fs: bool| WindowSnapshot {
        id: WindowId(id),
        pid: ProcessId(1),
        app: String::new(),
        bundle_id: None,
        title: String::new(),
        frame: Rect { x: 0.0, y: 0.0, width: 100.0, height: 100.0 },
        space_id: Some(s1),
        display_id: Some(d1),
        focused: false,
        minimized: false,
        fullscreen: fs,
        managed: true,
        generation: 0,
    };
    observed.windows.insert(WindowId(1), mk(1, false));
    observed.windows.insert(WindowId(2), mk(2, false));
    observed.windows.insert(WindowId(3), mk(3, false));
    let wf = WindowId(9);
    observed.windows.insert(wf, mk(9, true));

    let mut config = Config::default();
    config.general.layout = LayoutKind::Bsp;
    config.general.padding = 12; // NONZERO so a double-inset would shift the bbox by 12px
    config.general.gap = 8;
    let padding = config.general.padding as f64;

    let mut desired = DesiredState::default();
    apply_layout(&config, &observed, &mut desired);

    assert!(desired.windows[&WindowId(1)].frame.is_some());
    assert!(desired.windows[&WindowId(2)].frame.is_some());
    assert!(desired.windows[&WindowId(3)].frame.is_some());
    assert!(desired.windows[&wf].frame.is_none()); // fullscreen skipped

    // Exact-area assertion: compute() insets display.frame by padding ONCE, so the
    // bounding box of all placements must equal inset(display.frame, padding), NOT
    // display.frame - 2*padding (which a double-inset bug would produce).
    let ids = [WindowId(1), WindowId(2), WindowId(3)];
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;
    for id in ids {
        let f = desired.windows[&id].frame.unwrap();
        min_x = min_x.min(f.x);
        min_y = min_y.min(f.y);
        max_x = max_x.max(f.x + f.width);
        max_y = max_y.max(f.y + f.height);
    }
    let eps = 1.0;
    assert!((min_x - padding).abs() < eps, "min_x={min_x} expected {padding}");
    assert!((min_y - padding).abs() < eps, "min_y={min_y} expected {padding}");
    assert!((max_x - (1440.0 - padding)).abs() < eps, "max_x={max_x} expected {}", 1440.0 - padding);
    assert!((max_y - (900.0 - padding)).abs() < eps, "max_y={max_y} expected {}", 900.0 - padding);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rovr-core apply_layout_tiles_managed_windows_and_skips_fullscreen`
Expected: FAIL (module/file absent)

- [ ] **Step 3: Write minimal implementation**

```rust
// crates/rovr-core/src/layout.rs
use std::collections::HashMap;

use rovr_config::Config;
use rovr_layout::{compute, LayoutKind, LayoutRequest};
use rovr_types::{DisplayId, WindowId, WindowSnapshot};

use crate::{DesiredState, ObservedState};

fn is_managed(w: &WindowSnapshot) -> bool {
    !w.fullscreen
}

/// Recompute tiling targets for every observed managed window and write them
/// into `desired.windows[].frame`. Idempotent: rebuilt from `observed` each call.
///
/// `area` is the raw display frame; `rovr_layout::compute` insets it by `padding`
/// internally (lib.rs:34), so we must NOT pre-inset here (that would double-inset).
pub fn apply_layout(config: &Config, observed: &ObservedState, desired: &mut DesiredState) {
    let kind: LayoutKind = config.general.layout;
    let gap = config.general.gap as f64;
    let padding = config.general.padding as f64;

    // Drop desired entries for windows no longer observed (no staleness).
    desired.windows.retain(|id, _| observed.windows.contains_key(id));
    for id in observed.windows.keys() {
        desired.windows.entry(*id).or_default();
    }

    // Group managed windows by the display of the Space they sit on.
    let mut by_display: HashMap<DisplayId, Vec<WindowId>> = HashMap::new();
    for w in observed.windows.values() {
        if !is_managed(w) {
            if let Some(t) = desired.windows.get_mut(&w.id) {
                t.frame = None; // floating / fullscreen: do not tile
            }
            continue;
        }
        match (w.space_id, w.space_id.and_then(|s| observed.spaces.get(&s))) {
            (Some(_), Some(space)) => {
                by_display.entry(space.display_id).or_default().push(w.id);
            }
            _ => {
                if let Some(t) = desired.windows.get_mut(&w.id) {
                    t.frame = None;
                }
            }
        }
    }

    for (display_id, window_ids) in by_display {
        let Some(display) = observed.displays.get(&display_id) else {
            continue;
        };
        let area = display.frame; // compute() insets this by padding ONCE
        let request = LayoutRequest {
            area,
            windows: &window_ids,
            gap,
            padding,
            split_ratio: 0.5,
        };
        if let Ok(placements) = compute(kind, request) {
            for p in placements {
                if let Some(t) = desired.windows.get_mut(&p.window) {
                    t.frame = Some(p.frame);
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rovr-core apply_layout_tiles_managed_windows_and_skips_fullscreen`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/rovr-core/src/layout.rs crates/rovr-core/src/lib.rs
git commit -m "core: apply_layout writes tiling frames into desired state"
```

### Task M3a-3: Wire `apply_layout` into `Engine::apply_event`

**Files:**
- Modify: `crates/rovr-core/src/engine.rs` (`apply_event` + `apply_snapshot` retain)

**Interfaces:**
- Consumes: `crate::layout::apply_layout`
- Produces: `apply_event` now runs `apply_layout` before `reconcile`

- [ ] **Step 1: Write the failing test**

```rust
// crates/rovr-core/src/engine.rs (inside #[cfg(test)])
#[test]
fn apply_event_emits_set_frame_for_tiled_windows() {
    use rovr_types::{
        DisplayId, DisplaySnapshot, LayoutKind, ProcessId, Rect, SpaceId, SpaceSnapshot,
        WindowId, WindowSnapshot,
    };

    let mut config = Config::default();
    config.general.layout = LayoutKind::Bsp;

    let d1 = DisplayId(1);
    let d2 = DisplayId(2);
    let s1 = SpaceId(11);
    let s2 = SpaceId(22);

    let mk_win = |id: u32, space: SpaceId, display: DisplayId| WindowSnapshot {
        id: WindowId(id),
        pid: ProcessId(1),
        app: String::new(),
        bundle_id: None,
        title: String::new(),
        frame: Rect { x: 0.0, y: 0.0, width: 100.0, height: 100.0 },
        space_id: Some(space),
        display_id: Some(display),
        focused: false,
        minimized: false,
        fullscreen: false,
        managed: true,
        generation: 0,
    };

    let snap = PlatformSnapshot {
        windows: vec![
            mk_win(1, s1, d1),
            mk_win(2, s1, d1),
            mk_win(3, s1, d1),
            mk_win(4, s2, d2),
            mk_win(5, s2, d2),
        ],
        spaces: vec![
            SpaceSnapshot { id: s1, display_id: d1, label: None, focused: false, generation: 0, position: 0 },
            SpaceSnapshot { id: s2, display_id: d2, label: None, focused: false, generation: 0, position: 1 },
        ],
        displays: vec![
            DisplaySnapshot { id: d1, frame: Rect { x: 0.0, y: 0.0, width: 1440.0, height: 900.0 }, label: None, focused: false, generation: 0 },
            DisplaySnapshot { id: d2, frame: Rect { x: 1440.0, y: 0.0, width: 1440.0, height: 900.0 }, label: None, focused: false, generation: 0 },
        ],
        complete: true,
    };

    let mut engine = Engine::new(config);
    let actions = engine.apply_event(Event::Snapshot(snap));
    let framed: Vec<_> = actions
        .iter()
        .filter(|a| matches!(a, Action::SetWindowFrame { .. }))
        .collect();
    assert_eq!(framed.len(), 5); // all 5 managed windows tiled
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p rovr-core apply_event_emits_set_frame_for_tiled_windows`
Expected: FAIL (no frames emitted today)

- [ ] **Step 3: Write minimal implementation**

```rust
// crates/rovr-core/src/engine.rs
use crate::layout::apply_layout;

pub fn apply_event(&mut self, event: Event) -> Vec<Action> {
    self.flight_recorder.record("event", format!("{event:?}"));
    match event {
        Event::Snapshot(snapshot) => self.apply_snapshot(snapshot),
        Event::WindowDestroyed { window } => {
            self.observed.windows.remove(&window);
            self.desired.windows.remove(&window);
        }
        Event::SpaceDestroyed { space } => {
            self.observed.spaces.remove(&space);
            for target in self.desired.windows.values_mut() {
                if target.space == Some(space) {
                    target.space = None;
                }
            }
        }
        // ... other arms unchanged ...
    }
    apply_layout(&self.config, &self.observed, &mut self.desired);
    let actions = reconcile(&self.observed, &self.desired);
    actions
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p rovr-core apply_event_emits_set_frame_for_tiled_windows`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/rovr-core/src/engine.rs
git commit -m "core: run apply_layout before reconcile in apply_event"
```

### Task M3a-4: Daemon sets `engine.config`

**Files:**
- Modify: `crates/rovr-daemon/src/main.rs` (startup + reload handler)

**Interfaces:**
- Consumes: `Engine::new(config)`, `Daemon.config`, `Daemon.config_path`

- [ ] **Step 1: Write the failing test** — N/A (integration); verify by `cargo build` of daemon + a `doctor` field check that layout config is read. Use the existing `Config` reload path.

- [ ] **Step 2: Implement**

```rust
// crates/rovr-daemon/src/main.rs (main / Daemon construction)
let config = Config::load(&config_path).unwrap_or_default();
let mut engine = Engine::new(config.clone());
// ... existing snapshot -> apply_event -> execute ...
```

In the `Config(command) => ConfigCommand::Reload { path }` arm, after `self.config = config; self.config_path = path;`, also set `self.engine.config = config;`.

- [ ] **Step 3: Run**

Run: `cargo build -p rovr-daemon && cargo test --workspace`
Expected: build + tests green

- [ ] **Step 4: Commit**

```bash
git add crates/rovr-daemon/src/main.rs
git commit -m "daemon: feed config into Engine for layout wiring"
```

### Task M3a-5: End-to-end MockPlatform + contained live verification

- [ ] **Step 1 (unit, MockPlatform):** Reuse `MockPlatform` with a canned `PlatformSnapshot` (2 displays, windows as above) and assert the actions returned by the daemon/engine path include `SetWindowFrame` for all managed windows, frames strictly inside the display `frame`, and no `SetWindowFrame` for the fullscreen window. Mirror the existing `reconcile` test's `window()`/`snapshot()` builders.

- [ ] **Step 2 (live, contained):** On macOS, create ONE throwaway Space (`rovr space create`), open 2–3 TextEdit/Terminal windows on it, start `rovrd` with a tiling config (`general.layout = "bsp"`, `gap = 12`, `padding = 12`), screenshot before/after, assert windows are tiled within the Space's display area (pixel-diff or frame query via `rovr query windows`). Then `rovr space destroy` the throwaway Space and confirm original layout is restored. Do NOT tile the user's primary Space in this test.

- [ ] **Step 3: Commit verification note** — no code change; record the live result in the PR description.

**M3a acceptance:** `scripts/check.sh` green; MockPlatform test asserts exact tiled-frame set; live test shows tiling on the throwaway Space only.

---

## M3b: Stateful BSP mutation (insert / remove / rotate / mirror)

**Goal:** User/command-driven BSP tree manipulation. `insert`/`remove` are already implied by recompute over the current window set; `rotate` (cycle split orientations) and `mirror` (flip primary axis) require **orientation state**.

**Interface:** `Engine` gains `layouts: HashMap<SpaceId, LayoutState>` where `LayoutState { order: Vec<WindowId>, orientation: Orientation }`. New commands `Engine::rotate_layout(space)`, `Engine::mirror_layout(space)` mutate `LayoutState`; `apply_layout` (M3a) consults `LayoutState` when building the `LayoutRequest` (e.g., reorder `windows` by `order`, or pre-flip `area` for mirror). `compute` stays stateless; orientation is expressed as window-order / area transform before the call.

**Risk:** Stateless `compute` cannot represent orientation → must store `LayoutState`. Mitigation: keep `compute` pure; push orientation into request construction. Verify with a test asserting rotate changes the emitted frame set.

**Depends on:** M3a.

## M3c: Reactive rules

**Goal:** Match `WindowSnapshot.app`/`title` against `config.rule[]`. **Float rules** (skip tiling) are implementable now: `apply_layout` consults a `RuleMatcher` and marks matched windows unmanaged. **Workspace-assignment rules** set `desired.windows[id].space` (cross-Space move) and are deferred to M3d (needs the workspace binding).

**Interface:** `fn matches(rule: &RuleConfig, w: &WindowSnapshot) -> bool`; `apply_layout` calls it; floating match → `frame = None`.

**Risk:** workspace assignment introduces cross-Space moves (ordering hazard M3a explicitly avoids). Mitigation: ship float rules in M3c; gate assignment behind M3d.

**Depends on:** M3a (float); M3d (assign).

## M3d: Named workspaces

**Goal:** Bind `config.workspace[]` (name, layout, display, persistent) to macOS Spaces; surface names in `query`; allow per-workspace `layout` to override the global in `apply_layout`.

**Interface:** `WorkspaceBinding: HashMap<WorkspaceName, SpaceId>` (in-memory, persisted by M3f). `apply_layout` looks up the Space's workspace and uses its `layout` if present.

**Risk:** macOS Spaces are reorderable (M2 added reorder!) and unnamed, so binding by position breaks. Mitigation: bind by `SpaceId` at assignment time; persist the map (M3f). Within a session an in-memory map suffices.

**Depends on:** M3a; M3f (for persistence across restart).

## M3e: Scratchpads

**Goal:** Toggle a window/app into a special overlay workspace (`scratchpad toggle <name>` / `window scratchpad <window>`), shown on top regardless of current Space.

**Interface:** builds on M3d workspace model + `MoveWindowToSpace` + `SetWindowLayer` (M2c). Scratchpad = a hidden/overlay Space.

**Depends on:** M3d.

## M3f: Persistent workspace restoration

**Goal:** Persist workspace→Space bindings, window→Space assignments, and `LayoutState` across daemon restarts.

**Interface:** minimal JSON state file loaded at startup, saved on change. Reuses `Config`/snapshot shapes.

**Risk:** persistence is a separate PRODUCT.md goal; building a full layer here is scope creep. Mitigation: ship a minimal JSON file for the M3d/M3b state only; do not generalize.

**Depends on:** M3d, M3b.

---

## Foolproofing Dissection

The plan is foolproof **because** of the Global Constraints plus the phase decomposition. Residual risks and how each is contained:

- **R1 — Off-screen / hidden-Space window gap (M1, accepted limitation, not a bug).** Observed state drops windows on non-visible Spaces; M3a tiles only observed windows, so hidden-Space windows are simply not managed. This is consistent with the current architecture and breaks nothing. Documented; not claimed as supported.
- **R2 — Fullscreen detection gap (M1 bridge, accepted).** `WindowSnapshot.fullscreen` is hardcoded `false` in the bridge today, so M3a cannot yet skip fullscreen windows by observation. macOS generally ignores `SetWindowFrame` on fullscreen windows (no harm), and once the bridge reports `fullscreen` truthfully, `is_managed` will skip them. Low risk.
- **R3 — Stale desired entries for closed windows.** Contained by **full regenerate each cycle** (`desired.windows.retain(observed)` + rewrite), so staleness is bounded to one cycle; `reconcile` already only acts on desired entries.
- **R4 — Cross-Space move ordering hazard.** Contained by **tile-in-place**: M3a writes only `frame`, never `space`. Space assignment is explicitly deferred to M3c/M3d.
- **R5 — Layout area correctness.** `apply_layout` passes the **raw** display `frame` as `area`; `rovr_layout::compute` insets it by `padding` exactly once internally (lib.rs:34), so placements stay within `inset(display.frame, padding)`. The M3a-2 unit test asserts the exact bounding box of all placements equals `inset(display.frame, padding)` using a NONZERO padding — a double-inset would shift the bbox by `padding` px and fail the assertion. Frames verified within display bounds in M3a-5.
- **R6 — Multiple displays / Spaces.** Grouping is by `space.display_id`, so each display's Spaces tile independently with their own area.
- **R7 — Live-test destructiveness.** Contained: live verification runs only on a throwaway created Space with test windows, then destroyed; primary Space is never tiled during the test. A `--dry-run` (log intended frames without executing) is a cheap optional add.
- **R8 — Relayout latency.** Only `Event::Snapshot` is produced today (WindowCreated/Closed have no producer), so new/closed windows are reflected on the next poll (`reconcile_interval_ms`). Bounded, acceptable for MVP; M3 later adds lifecycle event producers to trigger immediate relayout.
- **R9 — BSP rotate/mirror needs state.** Acknowledged; M3b introduces `LayoutState` and keeps `compute` pure. M3a does not pretend to rotate.
- **R10 — Workspace ↔ Space binding ambiguity.** Contained by binding to `SpaceId` (not position) in M3d; M3a/d avoid position binding entirely.
- **R11 — Persistence dependency for M3f.** M3f is explicitly last and may ship a minimal JSON file rather than a general persistence layer, so it does not block M3a–M3e.
- **R12 — Rule workspace-assignment move hazard.** Contained: M3c ships float rules only; assignment waits for M3d.

**Go / no-go:** M3a is safe to execute now (reuses the tested engine, idempotent, tile-in-place, fully MockPlatform-verifiable). M3b–M3f are gated behind their dependencies and each gets its own detailed plan at execution time.
