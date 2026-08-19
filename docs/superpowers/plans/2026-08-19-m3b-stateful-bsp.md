# M3b — Stateful BSP rotate/mirror

**Goal:** Let the user rotate (cycle 90°) and mirror (flip primary axis) the BSP tree per Space, via stored orientation state. `rovr_layout::compute` stays pure/stateless; orientation is expressed as a pre/post transform on the request inside `apply_layout`.

**Design**
- `Orientation { axis: Axis, reversed: bool }`, `Axis { Vertical, Horizontal }`. `Orientation::default()` = (Vertical, false) = today's BSP → M3a behavior preserved.
- `LayoutState { orientation: Orientation }`. `Engine` gains `layouts: HashMap<SpaceId, LayoutState>`.
- `rotate_layout(space)` cycles 4 states: (V,false)→(H,false)→(V,true)→(H,true)→(V,false). `mirror_layout(space)` flips `axis`, keeps `reversed`.
- `apply_layout` **groups by Space** (not display — corrects a latent M3a bug: multiple Spaces on one display were tiled as a single block). For Bsp it applies orientation:
  - `reversed` → reverse the window order before `compute`.
  - `axis == Horizontal` → transpose the request area (swap W/H) and transpose resulting frames back. Pure coordinate transform; `compute`/`rovr-layout` untouched.
  - Non-Bsp kinds ignore orientation (rotate/mirror are BSP ops).

**Plan (foolproofed)**
- Types in new `rovr-core/src/layout_state.rs` (`Axis`, `Orientation`, `LayoutState`, `Layouts`). `rovr-protocol` only needs `SpaceId`.
- `Engine`: add `layouts` field (Default → empty), `rotate_layout`/`mirror_layout`, pass `&self.layouts` into `apply_layout` in `apply_event`.
- `apply_layout(config, observed, desired, layouts)`: per-Space grouping + orientation transform.
- Protocol: `Command::Layout(LayoutCommand)` with `LayoutCommand::{Rotate{space}, Mirror{space}}` in `rovr-protocol`.
- CLI: `rovr layout rotate <space>` / `rovr layout mirror <space>`.
- Daemon: `Command::Layout` → mutate engine → `platform.snapshot()` → `engine.apply_event(Snapshot)` → `execute_and_refresh`.

**Foolproofing (risks)**
- R1: rotate/mirror on a Space with <2 windows → state changes, layout unchanged (single window fills area). No crash.
- R2: rotate/mirror on an unobserved Space id → `layouts.entry(space).or_default()` stores state; harmless (no windows to tile).
- R3: Orientation persists for the daemon session (in `engine.layouts`); not across restarts (M3f). Documented.
- R4: Per-Space grouping fixes latent multi-Space-on-one-display bug. `m3a3` (1 space/display) still yields 5 frames; `m3a2` updated for new signature, still passes.
- R5: Axis transpose verified by test asserting 2-window layout flips side-by-side → stacked.
- R6: `Orientation::default()` = M3a behavior → backward compatible; non-Bsp unaffected.
- R7: `compute`/`rovr-layout` untouched (pure). No changes to its tests.
- R8: Rotate is 4-state; mirror is 2-state (distinct ops).

**Tests**
- `m3b_rotate_flips_axis`: 2 windows, default (V) → side-by-side; rotate → stacked.
- `m3b_four_rotations_restore`: frames after 4 rotations == start.
- `m3b_per_space_independent`: two Spaces on one display, rotate only one → only that Space restacks.
- `m3a2`/`m3a3` updated for the new `apply_layout` signature; still pass.

**Verification:** `scripts/check.sh` green; `rovr-core` tests. No live test (yabai live → clash, recorded lesson).

**Deliverable:** commit + PR `m3b-stateful-bsp` for audit/merge.
