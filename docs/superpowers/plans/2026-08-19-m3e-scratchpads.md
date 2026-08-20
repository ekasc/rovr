# M3e — Scratchpads

**Goal:** Add scratchpad support to the engine. Windows matching a named
`scratchpad` config are excluded from tiling **when that scratchpad is open**.
Toggling a scratchpad flips its open/closed state in the engine; a closed
scratchpad's members rejoin normal tiling. This delivers engine-complete,
testable scratchpad behavior plus full IPC/CLI plumbing.

**Why now:** `Config` already drives layout (M3a–M3d) and floats (M3c) via
config. A scratchpad is a named, toggleable float-set. The per-milestone
thread is "wire an existing/intended config concept into the engine"; M3e
completes it for scratchpads.

**Design**
- `ScratchpadConfig { name: String, app: Option<String>, title: Option<String> }`
  in `rovr-config` (`#[serde(default, rename = "scratchpad")]`). Added to
  `Config.scratchpads: Vec<ScratchpadConfig>`. `name` is required; the struct
  still derives `Default` (String/Option default fine).
- `ScratchpadState(HashMap<String, bool>)` in `layout_state.rs` (mirrors
  `Layouts`): `is_open(name) -> bool`, `toggle(name)`. Default closed (false).
- `matches_scratchpad(w, &config.scratchpads) -> Option<String>`: first
  scratchpad whose `app` (exact bundle id) and `title` (substring) match
  (`None` = wildcard). Returns its `name`.
- `apply_layout` gains a 5th param `scratchpads: &ScratchpadState`. The skip
  condition adds:
  `|| matches_scratchpad(w, &config.scratchpads).map_or(false, |name| scratchpads.is_open(&name))`.
  A scratchpad member floats **only** when its pad is open; closed → tiles.
- `Engine.scratchpads: ScratchpadState` (Default). `Engine::toggle_scratchpad(name)`
  flips `self.scratchpads.toggle(name)`.
- IPC: `ScratchpadCommand::Toggle { name }`, `Command::Scratchpad(ScratchpadCommand)`.
  Daemon arm mirrors `Command::Layout` (mutate engine → re-snapshot →
  `apply_event` → `execute_and_refresh`). CLI `rovr scratchpad toggle <name>`.

**Scope note:** True macOS show/hide on toggle needs platform visibility
control not yet in the bridge (it hardcodes `managed`/`fullscreen`). The
engine-complete behavior (open→float, closed→tile) is observable and tested
now; the platform hide is a later enhancement (deferred).

**Plan (foolproofed)**
- `rovr-config/src/lib.rs`: `ScratchpadConfig` + `Config.scratchpads`.
- `rovr-core/src/layout_state.rs`: `ScratchpadState`.
- `rovr-core/src/layout.rs`: imports (`ScratchpadConfig`, `ScratchpadState`);
  `matches_scratchpad`; `apply_layout` 5th param + skip condition; update 5 test
  call sites to pass `&ScratchpadState::new()`; add `m3e_open_scratchpad_floats_member`,
  `m3e_closed_scratchpad_tiles_member`.
- `rovr-core/src/engine.rs`: import `ScratchpadState`; `scratchpads` field;
  `toggle_scratchpad`; update `apply_event` call site + 2 test call sites; add
  `m3e_toggle_scratchpad_flips_open`.
- `rovr-protocol/src/lib.rs`: `ScratchpadCommand` + `Command::Scratchpad`.
- `rovr-daemon/src/main.rs`: import + handler arm.
- `rovr-cli/src/main.rs`: import, `TopCommand::Scratchpad`, `ScratchpadArgs`/
  `ScratchpadSubcommand`, `map_command` arm.

**Foolproofing (risks)**
- R1: `apply_layout` signature change ripples to every call site (engine
  `apply_event` + 7 tests). Compiler-enforced; update all.
- R2: `ScratchpadConfig` derives `Default` (String/Option default) — fine.
- R3: closed scratchpad (default) → member tiles (frame `Some`). Tested.
- R4: match = app exact + title substring, same convention as rules; `None` wildcard.
- R5: `is_tileable` unchanged; existing `m3a2`/`m3a2b`/`m3c` tests still pass
  (new arg `ScratchpadState::new()` → all pads closed → no effect).
- R6: serde `rename = "scratchpad"` → TOML `[[scratchpad]] name = "term"`.

**Verification:** `cargo clippy --workspace --all-targets -- -D warnings` clean;
`cargo test -p rovr-core` green (incl. 2 new layout tests + 1 engine test).
No live test (yabai live → clash, recorded lesson).

**Deferred:** platform show/hide on toggle (bridge visibility control), per-pad
default-open config, multi-window scratchpad targeting by space.
