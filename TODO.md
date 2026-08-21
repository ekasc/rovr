# TODO — PR #19 Correctness Blockers (2026-08-21)

> Exact blockers from review — fix in order. Status: [ ] missing | [~] implemented but not verified end-to-end | [x] verified end-to-end

## Blockers

- [~] 1. Rovr-owned scripting addition is claimed but not actually shipped
  - `crates/rovr-sa-payload`: real ObjC payload dylib (Rovr socket `/tmp/rovr-sa_<uid>.sock`, `rovr-sa-1.0` handshake, honest capability attribs, opcodes 0x02–0x05/0x07–0x0B/0x0D; SkyLight cosmetics direct, space lifecycle via vendored yabai MIT pattern tables with attribution). Scan bounds-guarded so it cannot walk off mapped memory.
  - `crates/rovr-sa-loader`: root injector (adapted from yabai loader.m, attribution preserved).
  - `rovr sa install|uninstall` wired for real (artifact discovery, root check, SIP check, copy to `/Library/Application Support/rovr/`, inject, poll handshake). `rovr sa status` reports all five states (`not_installed`, `installed_not_injected`, `injected_compatible`, `incompatible_protocol`, `capability_missing`).
  - Client framing bug found & fixed via live interop test (`len = 3 + payload_len`); socket path now keyed on `getuid()` matching the payload.
  - VERIFIED: payload loaded into an isolated host process — constructor, socket bind, handshake (`rovr-sa-1.0`), opacity/sticky op frames + EOF ack, honest `capability_missing` status via `rovr sa status`.
  - NOT VERIFIED: injection into real Dock and actual space/cosmetic effects — requires SIP relaxation (recovery reboot), see docs/SA_SIP.md.

- [~] 2. Snapshot timeout leaks hung threads
  - Replaced spawn-and-abandon with ONE `BoundedWorker` thread (`crates/rovr-platform/src/bounded_worker.rs`): at most one observation in flight, fail-fast while wedged, detectable via `wedged_since` (exposed in `doctor.snapshot_wedged_ms`), explicit epoch-based retry recovery.
  - Regression tests: 30 consecutive timeouts keep execution concurrency at exactly 1; wedge detection + fail-fast + recovery; healthy path. Real AX/SkyLight hang not reproduced on live macOS.

- [~] 3. Workspace remapping is nondeterministic
  - `WorkspaceRegistry::remap_after_snapshot` rewritten: stable config `ordinal` (persisted), unclaimed spaces sorted by position, resume-by-`last_position` pass, ordinal-order assignment, numeric/`main` display semantics. Regression test repeats the code→101/chat→102 acceptance scenario 200× — never swaps.
  - Pure-logic verification only; Dock-restart behavior on macOS not yet exercised.

- [~] 4. Persistent workspaces are not recreated
  - Engine emits one deterministic `CreateSpace` per snapshot cycle for the lowest-ordinal missing persistent workspace (gated on platform `create_space` capability); new Space id learned ONLY by observing the next snapshot, then bound by deterministic remap. Tests cover single + multi-workspace ordering.

- [~] 5. BSP persistence is keyed by volatile macOS Space IDs
  - Logical workspace now OWNS its layout state: remaps return `RemapMove`s and the engine carries `LayoutState` (BSP tree, ratios, order) old→new backing; persistence stores workspace-owned trees under the stable workspace name (`workspace_layouts`). Acceptance test: non-trivial tree survives 11→101 remap byte-identically, across save/load too.

- [~] 6. Built-in hotkeys are architecturally invalid on macOS
  - Option A implemented: daemon MAIN thread creates the hotkey manager and runs the AppKit event loop (`run_appkit_event_loop`); socket accept moved to a worker thread; state loop unchanged. Compiles; hotkey firing requires manual run on macOS UI session (not verifiable headlessly).

- [x] 7. Hotkey command syntax diverges from the real CLI
  - ONE shared parser `rovr_protocol::command_parser::parse_command` (CLI-style grammar) used by hotkey dispatch AND enforced during config validation. Flag-style binds rejected at load. Unit-tested against the exact acceptance inputs.

- [x] 8. Unmatched hotkey commands silently become Ping
  - Ping fallback deleted: invalid bind command at runtime logs an error and executes NOTHING; invalid binds fail config load/reload (`ConfigError::InvalidBindCommand`). Regression tests on parser + config load.

- [x] 9. Rule-derived desired workspace state becomes sticky
  - `desired.space` is cleared and rebuilt from scratch every `apply_layout` cycle (it is exclusively rule-owned; manual moves are one-shot actions). Test: rule matches → target set; title changes → target gone.

- [x] 10. Rules validate regexes but runtime does not use regexes
  - `Config::compile_rules()` builds `CompiledRule` (regex matchers compiled once per load/reload, config order preserved); runtime matching uses the compiled regexes. Tests: exact anchor, alternation, non-match, invalid regex rejected at load, order preservation.

- [~] 11. WASM memory limiting is claimed but not implemented
  - wasmi `StoreLimits` attached per call: 16 MiB linear memory cap, table element cap, trap-on-grow-failure, fuel budget retained, no host imports. Regression test: memory-hogging plugin contained, host registry survives. Malicious-plugin daemon survival on live macOS not yet exercised.

- [x] 12. Plugin output is not validated
  - `validate_placements`: count == requested, no duplicates/missing/foreign, finite coords, positive sizes, sane bounds. Invalid output discarded WHOLESALE → built-in fallback. Nine regression tests (empty/duplicate/missing/foreign/NaN/infinite/zero/negative/huge).

- [~] 13. Unknown macOS observation state is collapsed into misleading booleans
  - `ObservedBool { Yes, No, Unknown }` for `minimized`/`fullscreen`/`managed`; bridge passes 0/1/2 through unmapped; tiling policy conservative (any Unknown ⇒ not tiled); direction-focus excludes Unknown-minimized; query output serializes unknown honestly. Test added. Live-macOS diagnostics output not yet eyeballed.

## Full checks

- `cargo fmt --all -- --check` — clean
- `cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test --workspace` — 89 passed, 0 failed
- Swift menu-bar app: no changes made; existing build checks untouched.
- CI: not run (no CI trigger from this session).
