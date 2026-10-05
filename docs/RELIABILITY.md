# Reliability research

Status: research notes, 2026-10-02. Grounded in three sources: failures observed
live on this machine, the upstream Yabai reference at `../yabai/` (read-only),
and external practice. Not a plan of record yet — recommendations are labelled
by priority and each names its evidence.

## 1. The reliability goal, restated

PRODUCT.md says Rovr must be *recoverable*, *observable*, and *resilient to
macOS state becoming stale*, and that "every OS wait must have a deadline" and
"failures should be surfaced or recorded". Reliability here is not "never has a
bug"; it is three properties:

1. **Availability** — the daemon answers IPC and tiles windows, or is restarted
   within seconds. It is never silently dead or silently wedged.
2. **Correctness under macOS flakiness** — a missed event, a stale cache, a
   hung app, a Dock restart, a sleep/wake must converge back to the desired
   state without user intervention.
3. **Diagnosability** — when it does fail, the cause is in a log we actually
   read, not on a stderr file nobody opens.

## 2. Failure classes actually observed (evidence)

These are not hypothetical; each was seen on macOS 27.0 (26A428) on 2026-10-01/02.

| # | Failure | Evidence | State |
|---|---|---|---|
| F1 | **State-loop thread dies; process becomes a zombie.** Socket stays open, every request returns EOF, launchd `KeepAlive` cannot fire because the process never exits. | `sample` showed main (`pthread_join`) + accept only; 1049 `sending on a closed channel` errors on 10-01, 72 on 10-03; nothing in `/tmp/rovr.err.log`. | Mitigated: main now joins the state handle and `exit(1)`s; panic hook + per-iteration `catch_unwind`. **Root cause still unknown.** |
| F2 | **SA payload aborts Dock.** A space op messaged a wrong Dock object → `doesNotRecognizeSelector:` → uncaught → `abort()`. | `Dock-2026-10-02-164750.ips`: `handle_connection` → `Dock+0x18c594` (in `move_space`) → `objc_exception_throw` → `std::terminate`. | Fixed: `@try/@catch` per opcode + `respondsToSelector:` guards. |
| F3 | **Stale SA payload cannot be evicted.** `dlopen` of an already-mapped path is a no-op; re-inject silently does nothing. | mapped file 73456 B (old) vs installed 73904 B (new); `handshake_timeout`. | Fixed operationally (restart Dock); install now advises it. |
| F4 | **Observation noise / AX timeouts.** 15/20 reported windows were accessory placeholders; `ax.refine_timeout` recurring. | `rovr query windows`; flight recorder. | Partially fixed (activation-policy filter, 20→9). |
| F5 | **Silent failure.** None of F1's deaths wrote anything to the log. | `/tmp/rovr.err.log` empty across both deaths. | Fixed: panic hook → `rovr.log`; disconnect log. |

The through-line: **Rovr failed silently and stayed failed.** Availability and
diagnosability, not layout correctness, are the weak axis.

## 3. Reference: how Yabai stays up

Surveyed from `../yabai/` (read-only). Mechanisms, with references.

### 3.1 Process structure — single owner, message queue
- Main thread runs the AppKit run loop (`src/yabai.c:350` `[NSApp run]`); native
  callbacks (AX observers, `NSWorkspace`, `SLSRegisterConnectionNotifyProc`)
  are registered on the main run loop (`application.c:57`, `mission_control.c:87`).
- A dedicated pthread runs the event loop (`src/event_loop.c:1705`), draining a
  lock-free CAS linked-list queue via `sem_wait` (`:1678`). **Callbacks only
  `event_loop_post` (`:1684`) — they never touch state.**
- All managers (`g_window_manager`, `g_space_manager`, `g_display_manager`) are
  mutated only from the event-loop thread.
- Single-instance lock via `fcntl(F_SETLK)` (`src/yabai.c:164`).

Rovr's equivalent: state loop owns `Engine` (`rovr-daemon/src/main.rs`), IPC and
callbacks submit over a bounded `sync_channel`. **Structurally equivalent and
arguably cleaner.** The difference is not the model; it is what happens when the
owner dies (see 3.7).

### 3.2 Signals
- Only `SIGCHLD` and `SIGPIPE` are ignored (`src/yabai.c:151-152`). No
  `SIGTERM`/`SIGINT`/`SIGHUP` handler; `SIGTERM` is default-terminate.
- Dock restart is *not* a signal — it is `NSApplicationDockDidRestartNotification`
  (`workspace.m:187`) → `DOCK_DID_RESTART`.

Rovr has **no signal handling at all**. It does not ignore `SIGPIPE`, so a write
to a half-closed client socket can terminate the daemon by default disposition.

### 3.3 Scripting addition lifecycle
- Manual: `yabai --load-sa` is a one-shot (`src/sa.m:369`); install writes
  `/Library/ScriptingAdditions/yabai.osax` and restarts Dock (`:231`).
- **There is no automatic reinjection after Dock restart or reboot.** No
  Dock-PID watcher.

Rovr is **ahead here**: `reinject.rs` is a generation-keyed, single-flight,
bounded-backoff state machine with a privileged helper and verified handshake.
The lesson from F3 is not "copy yabai" but "make the reload path honest".

### 3.4 Accessibility reliability
- Global `AXUIElementSetMessagingTimeout(system_element, 1.0)` (`window_manager.c:2712`).
- Per-app retry: on `kAXErrorCannotComplete`, mark `ax_retry`, destroy and
  re-post `APPLICATION_LAUNCHED` after 0.1 s (`application.c:51`, `event_loop.c:148`).
- AX calls are synchronous on the event-loop thread, bounded only by the 1.0 s
  timeout — a hung app can stall the loop for 1 s per call.

Rovr already **beats this**: per-PID `AxWorkerPool` with 500 ms timeouts and a
`BoundedWorker` that fails fast while wedged (`bounded_worker.rs`, `mod.rs:203`).
The missing piece is yabai's *re-acquire on `kAXErrorCannotComplete`*.

### 3.5 Spaces / Mission Control
- Invalid/dirty boolean model: `VIEW_IS_VALID`/`VIEW_IS_DIRTY` (`view.h:196`),
  `space_manager_mark_spaces_invalid` (`space_manager.c:1122`), full refresh on
  `SPACE_CHANGED`/`DISPLAY_CHANGED` (`event_loop.c:1015`).
- Remaps views by Space UUID on display add (`space_manager.c:1150`).

Rovr's monotonic *generation* model (ARCHITECTURE.md §Generations) is stronger.
The requirement is that **every discontinuity bumps it and issues `RefreshAll`**.

### 3.6 Sleep / wake / displays
- `NSWorkspaceDidWakeNotification` → `SYSTEM_WOKE` (`workspace.m:177`).
- `CGDisplayRegisterReconfigurationCallback` (`display_manager.c:505`).
- `SLSRegisterConnectionNotifyProc` for 1327/1328/804/808/1202/1204 (`yabai.c:322`).

Rovr has display reconfig (`bridge.m:1070`) and SLS notifications
(`bridge.m:1059`) — but **no `NSWorkspace` wake/sleep observer at all**. Sleep
is a top cause of window/Space corruption (see §4.4), and Rovr is blind to it.

### 3.7 Error discipline and supervision
- `misc/log.h`: `error` → `exit(EXIT_FAILURE)` (`:34`); most runtime AX failures
  degrade to `debug`. No watchdog, no heartbeat.
- Plist: `RunAtLoad=true`, `KeepAlive={SuccessfulExit=false, Crashed=true}`,
  `ProcessType=Interactive`, `Nice=-20`, logs to `/tmp`; **no `ThrottleInterval`**.

So Yabai's availability comes almost entirely from **launchd**, not from
in-process recovery: if the process dies, launchd restarts it. Rovr's F1 was a
process that *did not die*, which defeats that contract.

## 4. External practice

### 4.1 Rust daemons
- Consensus (Rust forum, "Long running servers, detect 'might panic'"): you
  cannot prevent a days-later panic; **use the OS process as the restart unit**,
  persist state, and let a supervisor restart. `panic = "abort"` + supervisor is
  the recommended shape; `catch_unwind` is discouraged as a general policy
  because a panic can leave shared state inconsistent.
- Implication for Rovr: `catch_unwind` per iteration (added today) is a
  *stop-gap* to keep the daemon alive through an unknown bug, not the target
  design. The target is: panic → process exits → launchd restarts from persisted
  state, with the panic logged.

### 4.2 launchd supervision
- `KeepAlive=true` restarts on **any** exit, including a clean `exit(0)`.
  `KeepAlive={SuccessfulExit=false}` restarts only on a crash — the standard
  "keep running, but let me stop it deliberately" contract.
- `ThrottleInterval` (default 10 s) is the minimum respawn delay; raising it
  (20–30 s) prevents a crash loop from hammering the machine or an API.
- loginwindow sends `SIGTERM` then `SIGKILL` on logout/restart; a daemon should
  handle `SIGTERM` to persist state.

### 4.3 Accessibility API
- `alt-tab-macos` sets a **global** `AXUIElementSetMessagingTimeout(1 s)` and
  bounds every brute-force scan by **wall-clock**, not an id ceiling
  (`AXUIElement.swift`). It also de-duplicates windows (`Set(windows)`).
- `kAXErrorCannotComplete` means the target app is unresponsive or waiting for
  input; the documented remedy is to retry or raise the timeout.
- Real apps can expose **zero** AX surface (Codex, Claude Desktop) or drop
  `AXWindows` transiently (Chromium "Find in Page"). A window manager must treat
  "no AX windows" as *unknown*, never as *no windows*.

### 4.4 Sleep / wake
- Windows and Spaces are routinely shuffled after sleep, worst with external
  displays that wake slowly (Apple communities, BetterDisplay #54, WindowLayout).
  The fix everyone converges on: observe `NSWorkspace` wake, then re-establish
  state rather than trusting it.

## 5. Recommendations, prioritized

Each names its evidence and its verification.

### P0 — availability

**P0.1 Fix the supervision contract.** (`install.sh:139`, `install-dev.sh:86`)
Change the plist to `KeepAlive={SuccessfulExit=false}` and add
`ThrottleInterval=20`. Evidence: §4.2. Verify: `launchctl print` shows the
contract; a deliberate `kill` restarts within the throttle; a clean exit does not.

**P0.2 Add a liveness watchdog for a *wedged* loop.** F1 was a dead thread, but
the state loop can also block (e.g. `execute_focus_space` retries SA 30×100 ms =
3 s synchronously; the gesture settle gate). Add: an `AtomicU64` heartbeat
updated once per loop iteration; a small watchdog thread that, if the heartbeat
is stale for N seconds, logs a full backtrace and exits for restart. Verify: a
test that blocks the loop and asserts the watchdog fires.

**P0.3 Close out F1's root cause.** The panic hook and disconnect log are live;
add a backtrace-to-file on the exit path and run a soak (§6). Until the cause is
known, P0.1/P0.2 are the safety net, not the fix.

### P1 — correctness under macOS flakiness

**P1.1 Observe sleep/wake.** Rovr has none (§3.6). Register
`NSWorkspaceWillSleepNotification`/`DidWakeNotification` in `bridge.m`, translate
to a typed event, bump the generation, issue `RefreshAll`. Evidence: §4.4,
yabai `workspace.m:177`. Verify: `pmset sleepnow`/wake on a live session; assert
`display.topology_changed`-style refresh and that windows re-tile.

**P1.2 Handle signals.** Rovr has none (§3.2). Ignore `SIGPIPE` (a broken client
write must not kill the daemon); handle `SIGTERM` to persist state and exit
cleanly; optionally `SIGUSR1` for an on-demand diagnostic dump. Verify: `kill
-TERM` persists and exits 0; a client that closes mid-write does not kill it.

**P1.3 Re-acquire AX on `kAXErrorCannotComplete`.** Port yabai's per-app retry
(§3.4): on a failed refine, drop the cached element and re-create the
application element on the next tick, rather than leaving the app permanently
`unknown`. Verify: a deliberately hung helper app (the repo already has one for
timeout tests) recovers once unblocked.

### P2 — SA robustness

**P2.1 Make `sa install` evict a stale payload.** Detect the already-mapped case
and restart Dock itself (or refuse with the instruction). Evidence: F3.
**P2.2 Make the loader report honest failure.** It sets its `0x79616265` success
magic on the spawning thread *before* the `dlopen` thread runs, so it reports
success whenever the remote thread starts. Verify the payload actually bound its
socket before returning success.
**P2.3 Keep the payload crash-contained** (done) and add a self-check that the
resolved objects respond to the selectors used, at injection time.

### P3 — diagnosability

**P3.1 Health surface.** Add to `doctor`: state-loop heartbeat age, last
successful observation age, request-queue depth, consecutive reconcile failures.
Evidence: F1/F5 — we had to reconstruct death from `sample` and thread counts.
**P3.2 Structured failure events.** Every recovery path already records to the
flight recorder; ensure the *cause* (panic message, disconnect, watchdog trip) is
recorded too, so `rovr debug events` explains a restart after the fact.

## 6. Verification strategy

Reliability claims need a harness, not inspection:

1. **Soak** — run the daemon 24–72 h with normal use; assert `doctor` responds
   every minute and the heartbeat never goes stale.
2. **Chaos injection** — script the discontinuities: `killall Dock`, sleep/wake,
   display disconnect/reconnect, hang an app, flood IPC past the queue bound,
   `kill -STOP` the state thread for 30 s. After each, assert the daemon is
   responsive and converges within a bounded time.
3. **No-silent-death invariant** — a test that any state-loop exit path logs a
   reason and, if the process survives, that it exits for launchd.
4. **Mutation-tested guards** — as done today for the panic guard: each recovery
   mechanism's test must fail when the mechanism is reverted.

## 7. What Rovr already does well

For balance: the generation model, the typed observed/desired split, the
per-PID AX worker pool with deadlines, the bounded worker, the flight recorder,
and the automatic SA reinjection are all at or beyond yabai's level. The gaps are
concentrated in **process lifecycle and discontinuity observation** (signals,
sleep/wake, supervision, watchdog), not in the reconciliation core.

## 8. Implementation status (2026-10-02)

| Item | Status | Verification |
|---|---|---|
| P0.1 supervision contract | **Done** | plist now `KeepAlive={SuccessfulExit=false}` + `ThrottleInterval=20`; `launchctl print` confirms. |
| P0.2 wedged-loop watchdog | **Done** | `HEARTBEAT_MS` updated per iteration; watchdog exits for restart if stale. Predicate unit-tested (`watchdog_threshold_clears_normal_stalls_and_interval`, `heartbeat_staleness_is_strictly_greater_than_threshold`); live trip not induced. |
| P0.3 root-cause F1 | **Open** | panic hook + disconnect log + watchdog are the safety net; the exit cause is still unknown. |
| P1.1 sleep/wake | **Done** | `NSWorkspaceDidWake/WillSleep` observers in `bridge.m` → `ROVR_EVENT_SYSTEM_WOKE` → immediate refresh. Live `pmset sleepnow` test not run (would sleep the user's machine). |
| P1.2 signals | **Done** | `SIGPIPE` ignored (verified: 25 abrupt-close clients, daemon survived); `SIGTERM` persists state, exits 0, and launchd correctly does **not** restart (`last exit code = 0`, `state = not running`). |
| P1.3 AX re-acquire | **No change needed** | `refine()` builds a fresh `AXUIElementCreateApplication(pid)` every tick (`bridge.m:1278`), so a `kAXErrorCannotComplete` app is re-acquired on the next tick; nothing is cached to go stale. |
| P2.1 `sa install` evict stale payload | Partial | Install now names the restart-Dock remedy; it does not perform the restart. |
| P2.2 loader honest failure | **Done (helper-side)** | The loader still exits 0 early, but the helper now probes the SA handshake (bounded, ~2 s) before reporting `OK`, so a no-op `dlopen` or a rejected payload becomes `INJECTION_FAILED`. Live: inject returns OK when the payload answers. The failure path is reasoned, not induced. |
| P2.3 payload crash containment | **Done** | `@try/@catch` per opcode + `respondsToSelector:` guards. |
| P3 health surface | **Done** | `doctor.result.health` = `state_loop_heartbeat_age_ms`, `last_observation_age_ms`, `reconcile_failure_streak`; `RECONCILE_FAILURE_STREAK` maintained in `refresh_observation`. Verified live and unit-tested. |

Tests added: watchdog threshold/staleness, plus the earlier panic-resilience test
(mutation-verified). Full checks: `cargo fmt --check`, clippy `-D warnings`,
`cargo test --workspace` — 253 passed.
