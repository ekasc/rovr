# TODO — PR #19 Correctness Blockers (2026-08-21)

> Exact blockers from review — fix in order. Status: [ ] missing | [~] implemented but not verified end-to-end | [x] verified end-to-end

## SA phase completion (2026-08-24)

- [x] Blocker 1 SA verification: injected into live Dock on macOS 26.5 (arm64e + PAC-ABI-v0-patched loader), all space ops and cosmetics verified with re-observation, `killall Dock` reinjection via helper verified (~6 s, full `0x7ff` attribs). Interop harness: `scripts/sa-interop/run.sh`. Reboot recovery still pending.
- [x] Daemon/CLI socket desync: daemon now uses shared `rovr_platform::daemon_socket_path()` (`/tmp/rovr-<uid>/daemon.sock`).
- [x] SA artifact discovery through symlinked installs (`~/.local/bin/rovr`): canonicalize `current_exe()` first.
- [x] arm64e build for payload + loader; loader caps patched 0x81→0x80 (yabai #2686 class).
- [x] Stable code signing in `install-dev.sh` (cert-anchored DR) so the Accessibility grant survives rebuilds; embedded Info.plist (`__TEXT,__info_plist`) so TCC attributes launchd-spawned rovrd correctly.
- [x] Honest capability gating: `rovr_bridge_capabilities` clears focus/frame bits when `AXIsProcessTrusted()` is false.
- [~] Known platform gap (macOS 26.5): background apps return EMPTY `kAXWindowsAttribute`; only the frontmost app's windows refine (minimized/fullscreen/managed stay `unknown`, focus-by-id works only for frontmost). Not an SA issue — affects tiling refinement generally.

## Blockers

- [~] 1. Rovr-owned scripting addition is claimed but not actually shipped
  - `crates/rovr-sa-payload`: real ObjC payload dylib (Rovr socket `/tmp/rovr-<uid>/sa.sock`, `rovr-sa-2.0` handshake, honest capability attribs, opcodes 0x02–0x05/0x07–0x0B/0x0D; SkyLight cosmetics direct, space lifecycle via vendored yabai MIT pattern tables with attribution). Scan bounds-guarded so it cannot walk off mapped memory.
  - `crates/rovr-sa-loader`: root injector (adapted from yabai loader.m, attribution preserved).
  - `rovr sa install|uninstall` wired for real (artifact discovery, root check, SIP check, copy to `/Library/Application Support/rovr/`, inject, poll handshake). `rovr sa status` reports all five states (`not_installed`, `installed_not_injected`, `injected_compatible`, `incompatible_protocol`, `capability_missing`).
  - Client framing bug found & fixed via live interop test (`len = 3 + payload_len`); socket path now keyed on `getuid()` matching the payload.
  - PREVIOUSLY VERIFIED against protocol v1 in an isolated host process: constructor, socket bind, handshake, and opacity/sticky frames. Protocol v2 adds the private runtime directory, exact frame lengths, peer checks, and status ACKs; unit-tested, but the isolated-host interop test still needs to be rerun.
  - NOT VERIFIED: injection into real Dock and actual space/cosmetic effects — requires SIP relaxation (recovery reboot), see docs/SA_SIP.md.

- [~] 2. Snapshot timeout leaks hung threads
  - Replaced spawn-and-abandon with ONE `BoundedWorker` thread (`crates/rovr-platform/src/bounded_worker.rs`): at most one observation in flight, fail-fast while wedged, detectable via `wedged_since` (exposed in `doctor.snapshot_wedged_ms`), recovery only after the timed-out closure is observed complete (no queued retries).
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

- [~] 14. Automatic SA reinjection after Dock restart/reboot (no sudoers)
  - New `crates/rovr-sa-helper`: minimal root LaunchDaemon (`com.rovr.sa-helper`, socket-activated at `/var/run/rovr-sa-helper.sock`, launchd-owned socket, on-demand). API is exactly `inject()` + `status()` over fixed `{magic,proto,opcode,uid}` frames — structurally NO pid/path/command/env fields. Resolves Dock ITSELF; validates fixed root-owned artifacts every use (lstat+O_NOFOLLOW, root-owned, modes); auth via `getpeereid` == request uid == `/dev/console` owner. Fixed argv + minimal env for the loader. Event-driven only.
  - SMAppService NOT usable: it requires the executable inside an app bundle's `Contents/Library/LaunchDaemons`; Rovr ships as bare cargo CLI binaries. Explicit fallback documented in docs/SA.md — no sudoers anywhere.
  - `reinject.rs` pure state machine: Dock-generation keyed, single-flight, bounded verify window, backoff 5s→15s→45s, max 4 attempts/generation then quiet until Dock changes (no retry storm). Integrated into MacPlatform::needs_refresh; SA cache now refreshes on version/attribs change (capabilities follow reinjection). Non-SA Rovr unaffected on failure.
  - CLI: `sa install` = files → service registration → direct injection → verified handshake → identity marker (`payload.installed.json`) → capability report. `sa uninstall` = bootout FIRST → plist/helper/loader/payload/marker/socket removal → documented Dock restart. `sa status` reports service/payload/injection states incl. installed≠injected payload mismatch. Doctor exposes `sa_reinject` lifecycle.
  - Tests: 15 platform (single-flight, generation invalidation, late-result discard, bounded retries, backoff, verify window, client protocol/refusals) + 2 CLI + 1 daemon doctor. Helper compiles clean with clang -Wall.
  - NOT VERIFIED live: initial install w/ SIP relaxation, killall Dock reinjection, reboot recovery, repeated Dock crashes, update simulation — all require a real interactive session (docs/SA.md acceptance criteria). Do not promote to [x] until demonstrated.

- [x] 14b. Latency edge-case sweep (state-loop head-of-line blocking class)
  - Root cause of "first switch slow" found by instrumentation: state loop did periodic O(N²) enumeration + synchronous waits, so requests queued behind observation cycles.
  - Fixed: per-app AX resolution in enumerate_windows (O(apps+windows)); async periodic snapshots via new `BoundedWorker::submit/poll` (`request_periodic_snapshot`/`poll_periodic_snapshot` trait methods; verification paths keep the synchronous worker); adaptive reconcile cadence (100 ms burst for ~2 s after activity, configured interval when idle); timestamp-paced gesture gate replacing SLS busy-poll; throttled short-deadline SA health probes (wedged payload can't stall the loop); cross-display Space focus (cursor warp to target display + `SLSSetActiveMenuBarDisplayIdentifier`, mirroring yabai); IPC socket keyed on real `getuid()` not `$UID`; hotkey dispatch reads its response (no EPIPE noise); bounded envelope channel.
  - Verified live: 12 rapid switches at 250 ms spacing complete in ≤240 ms each with ZERO queue waits; one-time cold-start cost only. fmt/clippy/tests clean.

- [x] 14c. yabai-parity window commands + focused-window defaults
  - New commands: window close / toggle-fullscreen / toggle-float / swap-dir /
    warp-dir / resize <edge> <delta>; focus-direction, move-to-workspace,
    set-layer accept optional ids; layout commands default to focused space;
    space focus-recent. Close/fullscreen via AX button presses.
  - Float state persists in desired state; directional ops resolve neighbors
    from observed geometry then mutate the BSP.
  - All rovr-expressible binds migrated to in-process [[bind]] config (39
    registered); skhd keeps app launches, reload, chained resizes, space
    destroy. Tests: parser/engine/daemon coverage added (112 total).

## Full checks

- `git diff --check` — clean; `cargo fmt --all -- --check` — clean
- `cargo clippy --workspace --all-targets -- -D warnings` — clean
- `cargo test --workspace` — 104 passed, 0 failed
- Swift menu-bar app: no changes made; existing build checks untouched.
- CI: not run (no CI trigger from this session).

## Multi-display + spawn-tile latency fixes (2026-08-23)

- Per-display focused Spaces: bridge reports SLSManagedDisplayGetCurrentSpace per
  display; Rust no longer collapses to a single global focused Space.
- Window display_id derived from the Space's display (sid→did map built once per
  snapshot), not CGGetDisplaysWithRect rect hit-testing.
- display="main" now means CGMainDisplayID (DisplaySnapshot.is_main), not the
  focused menu-bar display.
- Workspace remap compaction: stale backings / config add-remove-reorder trigger a
  full ordinal→position reassignment so alt-N stays N→desktop N (alt-5→desktop-4
  off-by-one bug); manual Mission Control drags tracked via last_position refresh.
- Spawn→tile flicker (~600-900ms → ~200ms):
  - state loop no longer double-observes Refresh envelopes (AX created event path);
  - wid→sid resolved in bulk via SLSCopyWindowsWithOptionsAndTags per SPACE
    (yabai approach) instead of SLSCopySpacesForWindows PER WINDOW (~100 private-API
    round trips per snapshot eliminated); fallback only for real AX windows
    (minimized). obs_ms 200-500 → ~100.
- Verified live on 2 displays: fresh BSP inserts are exact 50% ratios;
  layout balance resets drifted persisted ratios.

## Switch-latency root cause (2026-08-23, cont.)

- User confirmed switches have NO animation — lag was daemon-side, not gesture.
- Root cause: state loop ran a FULL pre-handle observation (~100-500ms with
  100+ windows) before EVERY non-Refresh command. Workspace/space focus paid it
  on every alt-N press.
- Fix: skip_pre_observe for Refresh, Workspace/Space commands, and
  focus-defaulting window commands (the latter self-refresh in handle()).
- Focus timing now logged at INFO when total_ms > 50 ("workspace focus timing
  (slow)") so regressions are visible without debug logging.
- Verified: no-op focus round-trips 17-35ms end-to-end via IPC (was 100-600ms).
- Cross-display settle-gate skip + activate-display-before-swipe also landed
  (bridge.m / macos/mod.rs).

## Simplify interactive Space focus (2026-08-23)

- [x] State loop is FIFO again; AX callbacks share one atomic Refresh wake, acknowledged before its snapshot so callbacks during observation queue the next wake.
- [x] Engine tracks an optimistic Space cursor per display; next/prev, explicit Space focus, and Workspace focus update it, while snapshots confirm or reconcile it.
- [x] Built-in hotkeys feed one persistent bounded IPC dispatcher (FIFO, no thread per press), which reads every IPC response and provides ordered bounded backpressure without blocking the AppKit listener.
- [x] Window creation enqueues an immediate Refresh for spawn tiling; focus events are handled by on-demand observation in focus-defaulting window commands plus the five-second idle recovery watchdog.
- [x] Rapid next-next-next and wrap were verified at the pure engine seam with stale observed focus; fmt, clippy, and workspace tests pass. No Space/window mutations were used for verification.

## Targeted no-wait adjacent Space stepping (2026-08-23)

- [~] `space next/prev` now emits `FocusSpaceStep { target, delta }`, preserving the optimistic per-display cursor while carrying the exact relative displacement from its current index to the target (including wrap).
- [~] The macOS fallback posts one high-velocity Dock swipe pair per displacement step on the display containing `target`, without querying current Space state or entering the absolute-focus settle gate. The scripting addition may still focus `target` directly.
- [x] Absolute `FocusSpace` and its bounded 450 ms completion gate remain unchanged for explicit/workspace focus.
- [x] Pure engine serialization, stale-observation sequence/wrap, and nonzero-displacement validation tests pass; fmt, clippy, and workspace tests are clean.
- [x] User verified rapid `alt+tab` stepping is fast on the external display.

## Luna gap sweep (2026-08-23)

- [x] Audit the current working tree against PRODUCT.md for reproducible correctness, latency, and recovery gaps.
- [x] Fix only high-confidence issues with a bounded blast radius; avoid speculative features and compensating schedulers.
  - Workspace config reorder now preserves old ordinals before rebuilding the registry, so the next snapshot performs the required ordinal-to-position remap.
  - Focus-recent/current tracking is per display, with deterministic observation and active-display fallback (focused, main, then lowest display id).
  - Hotkey reload timeout cancellation now races safely with main-thread claim; a cancelled callback cannot apply config, and a claimed callback's result is awaited.
  - Relative Space wrap displacement is intentionally unchanged: Dock swipes do not wrap, so Rovr emits the existing `-(N-1)`/`+(N-1)` displacement.
- [x] Add regression coverage at the real failure seams.
- [x] Run fmt, clippy, workspace tests, non-mutating daemon checks, and inspect the final diff.

### Deferred known gaps

- [x] P2: hotkey key syntax is parsed by the shared protocol seam and validated before config load/reload.
