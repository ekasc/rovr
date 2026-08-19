# M3c — Float rules

**Goal:** Honor the existing `Config.rules` (`RuleConfig`) so windows matching a
float rule are excluded from tiling. The `RuleConfig` schema already exists in
`rovr-config`; M3c only wires it into the layout engine. No new config schema,
no new IPC, no bridge changes.

**Why now:** M3a hardened `is_tileable = w.managed && !w.fullscreen` but the
bridge hardcodes `managed: true`, so real floating only becomes possible once
rules drive it. yabai semantics: a matching rule forces a window to float
regardless of `managed`.

**Design**
- `RuleConfig { app: Option<String>, title: Option<String>, workspace: Option<String>, floating: Option<bool> }` (already in `rovr-config/src/lib.rs`). TOML: `[[rule]] app = "com.apple.Safari" float = true`.
- In `apply_layout`, a window is floated (skipped from tiling) when:
  `!is_tileable(w) || matches_float_rule(w, &config.rules, observed)`.
- `matches_float_rule(w, rules, observed)`: for each rule where `floating == Some(true)`, all *specified* (Some) fields must match the window:
  - `app` → `w.bundle_id == Some(app)` (exact reverse-DNS bundle id).
  - `title` → `w.title.contains(title)` (substring).
  - `workspace` → resolve `w.space_id` → `observed.spaces[..].label == Some(workspace)`.
  If every specified field matches, the window floats.
- `is_tileable` itself is unchanged (`w.managed && !w.fullscreen`) — keeps M3a semantics and the existing `m3a2b` test green.

**Plan (foolproofed)**
- `crates/rovr-core/src/layout.rs`: add `matches_float_rule`; change the skip
  condition in `apply_layout` to also consult rules. `config` and `observed` are
  already in scope.
- Tests (layout.rs `#[cfg(test)]`):
  - `m3c_app_rule_floats`: window with `bundle_id = "com.apple.Safari"` + rule
    `{app:"com.apple.Safari", float:true}` → `frame == None`; a non-matching
    window on the same space → tiled.
  - `m3c_title_rule_floats`: `title.contains("Modal")` + rule `{title:"Modal", float:true}` → skipped.
  - `m3c_no_rule_tiles`: empty `config.rules` → managed non-fullscreen windows tiled (regression guard).

**Foolproofing (risks)**
- R1: rule omits a field (None) → that field is treated as "match anything". Only fully-specified-or-partial matches still require the stated fields to match.
- R2: `floating == None` or `Some(false)` → never forces float (only `Some(true)` floats). Safe default.
- R3: `workspace` match needs `observed.spaces` label; if space missing/unnamed, `workspace` rule simply won't match (no panic — `and_then` short-circuits).
- R4: `m3a2`/`m3a2b` still pass (no rules → behavior identical to M3a).
- R5: Determinism: rules evaluated in order, first matching `Some(true)` floats; order-stable. No global mutation.
- R6: Backward compatible: a window the bridge reports as `managed` but matching a float rule now floats (correct yabai behavior); previously it tiled.

**Verification:** `cargo clippy --workspace --all-targets -- -D warnings` clean;
`cargo test -p rovr-core` green (incl. 3 new tests). No live test (yabai live → clash, recorded lesson).

**Deliverable:** branch `m3c-float-rules`, PR for audit/merge.
