# M3f — State persistence

**Goal:** Persist the daemon's mutable runtime state (`Layouts` per-space
orientation from M3b, `ScratchpadState` open/closed from M3e) to disk and
restore it on startup, so user toggles survive daemon restarts. Config is
already user-edited TOML (not persisted); observed/desired are transient
(recomputed from snapshots) and not persisted.

**Why now:** M3b/M3e added runtime state the user mutates; without
persistence a daemon restart loses all rotations and scratchpad toggles.
Closes the M3 milestone set.

**Design**
- `PersistedState { layouts: HashMap<String, LayoutState>, scratchpads: HashMap<String, bool> }`
  in a new `rovr-core/src/persistence.rs`. `Layouts` keys are `SpaceId`
  (`u64` newtype) serialized as **strings** — JSON object keys must be strings,
  so we never serialize `HashMap<SpaceId, _>` directly (avoids the serde_json
  non-string-key error).
- `LayoutState`/`Orientation`/`Axis` gain `Serialize, Deserialize` (plain data).
- `Engine::save_state(path)` builds `PersistedState` (SpaceId→string), pretty-
  prints JSON, creates parent dirs, writes. `Engine::load_state(path)` reads,
  parses, and repopulates `self.layouts` (string→SpaceId via `u64::parse`,
  dropping unparseable keys) and `self.scratchpads`. Both return `anyhow::Result`.
- Daemon: `state_path` field (default `~/.config/rovr/state.json` via
  `default_state_path()`); best-effort `load_state` on startup (missing file =
  first run, expected); `persist_state()` helper (best-effort) called after
  `Layout::Rotate`/`Mirror` and `Scratchpad::Toggle` mutations.

**Plan (foolproofed)**
- `rovr-core/Cargo.toml`: add `serde_json` + `anyhow` (both workspace deps,
  used elsewhere) — removes error-boilerplate for IO+serde.
- `rovr-core/src/layout_state.rs`: `Serialize, Deserialize` on `Axis`,
  `Orientation`, `LayoutState`.
- `rovr-core/src/persistence.rs`: `PersistedState`.
- `rovr-core/src/lib.rs`: `pub mod persistence;` + `pub use persistence::PersistedState;`.
- `rovr-core/src/engine.rs`: imports (`std::path::Path`, `anyhow`, `PersistedState`);
  `save_state`/`load_state`; test `m3f_persist_restore` (rotate + toggle →
  save → fresh engine load → assert orientation axis + scratchpad open).
- `rovr-daemon/src/main.rs`: `state_path` field; `default_state_path()`;
  startup `load_state` (best-effort); `persist_state()` helper; call after
  Layout + Scratchpad mutations.

**Foolproofing (risks)**
- R1: `SpaceId` is a `u64` newtype; persisting `HashMap<SpaceId,_>` directly
  would break serde_json (non-string keys). Fixed by string keys in
  `PersistedState` + explicit conversions.
- R2: unparseable space-id keys on load are dropped (`filter_map`+`ok`), never
  panic.
- R3: missing state file on startup is expected (first run) → `load_state`
  errors are logged, not fatal.
- R4: `LayoutState::clone` available (derives `Clone`); save clones, load owns.
- R5: config/observed/desired intentionally NOT persisted (config = TOML,
  observed/desired transient). Scope kept tight.
- R6: `Orientation`/`Axis` are `Copy`; serialization is lossless.

**Verification:** `cargo clippy --workspace --all-targets -- -D warnings` clean;
`cargo test -p rovr-core` green (incl. `m3f_persist_restore`). No live test
(yabai live → clash, recorded lesson).

**Deferred:** crash recovery / flight-recorder replay, config hot-reload
persistence, per-window geometry persistence, encrypted state.
