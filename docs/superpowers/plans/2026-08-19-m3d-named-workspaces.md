# M3d — Named workspaces (per-workspace layout)

**Goal:** Honor `Config.workspaces` (`WorkspaceConfig`) so a named workspace's
`layout` overrides the global `config.general.layout` for the Space whose
`label` matches the workspace `name`. No new config schema, no new IPC, no
bridge changes.

**Why now:** `WorkspaceConfig { name, layout, display, persistent }` already
exists in `rovr-config` but is never read. M3a–M3c wired rules/layout into the
engine; M3d completes the config→engine loop for the per-space layout choice.

**Design**
- `WorkspaceConfig.name` matches `SpaceSnapshot.label` (the same key
  `RuleConfig.workspace` uses). This keeps workspace/rule semantics coherent.
- `resolve_layout(config, space_id, observed) -> LayoutKind`:
  - If the space's `label` is `Some(label)` and a `WorkspaceConfig` with
    `name == label` exists, return its `layout`.
  - Otherwise `config.general.layout`.
  - Never panics: `and_then` short-circuits on missing/unlabeled space.
- In `apply_layout`, remove the single top-level `let kind = config.general.layout;`
  and compute `let kind = resolve_layout(config, space_id, observed);` at the
  top of the per-Space loop. `gap`/`padding` stay global.
- `WorkspaceConfig.display` / `persistent` are deliberately NOT consumed here:
  they belong to dynamic Space creation and persistence (later milestones).

**Plan (foolproofed)**
- `crates/rovr-core/src/layout.rs`:
  - Add `fn resolve_layout(...)`.
  - `CUT` the top-level `kind` line (line 63).
  - `PUT` `let kind = resolve_layout(config, space_id, observed);` as the first
    statement of the per-Space loop.
- Tests (layout.rs `#[cfg(test)]`):
  - `m3d_named_workspace_overrides_layout`: space labeled `"dev"` + workspace
    `{name:"dev", layout:Stack}` → `resolve_layout == Stack`; a space labeled
    `"other"` → global `Bsp`.
  - `m3d_unlabeled_space_uses_global`: unlabeled space → global layout.

**Foolproofing (risks)**
- R1: `WorkspaceConfig` has no `Default` derive (field `layout` can't default),
  so tests construct all 4 fields explicitly (no `..Default::default()`).
- R2: `String == &str` compares fine (`PartialEq<&str> for String`).
- R3: Missing space / `None` label → falls back to global (no panic).
- R4: No `WorkspaceConfig` at all → identical to M3a behavior (global layout).
- R5: `kind` is still used by the existing orientation/orientation-transpose
  logic; M3b rotate/mirror unaffected (orientation is orthogonal to kind).
- R6: `LayoutKind` must stay imported (already is). `WorkspaceConfig` added to
  the test-module import.

**Verification:** `cargo clippy --workspace --all-targets -- -D warnings` clean;
`cargo test -p rovr-core` green (incl. 2 new tests). No live test (yabai live
→ clash, recorded lesson).

**Deferred:** name→SpaceId IPC (focus/move-to named workspace), `display`/
`persistent` consumption, dynamic Space creation. These are larger platform
features outside M3d's engine-scope.
