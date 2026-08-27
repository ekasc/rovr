# Per-application AX workers

## Goal

Prevent one unresponsive application from blocking Rovr's window inventory or
daemon state loop. Healthy applications must remain observable while AX data
for a timed-out application remains explicitly unknown.

## Design

Keep the daemon's single state owner and the existing global snapshot worker.
Inside the macOS platform layer, split window observation into two stages:

1. Collect the authoritative CoreGraphics candidate inventory without AX.
2. Refine candidates through persistent, single-flight workers keyed by PID.

Each application worker accepts at most one query at a time and returns plain
window refinement values; it never exports AX references. A snapshot submits
all idle workers first, then collects replies against one absolute deadline.
Late replies are discarded by generation. A timed-out or already-busy worker
does not fail the snapshot: its candidate windows retain `ObservedBool::Unknown`.
Workers for exited PIDs are pruned after they are idle. AX application and
window elements receive a finite messaging timeout as a second containment
layer.

The rejected alternative is a fresh Grand Central Dispatch task per PID inside
`bridge.m`. It makes timeout cleanup and ownership of retained AX references
harder, and a permanently blocked app can accumulate abandoned tasks across
snapshots. Persistent single-flight workers make the bound structural.

## Interfaces

The bridge exposes separate operations for:

- collecting CG window candidates;
- refining AX windows for one PID;
- preserving the existing callback boundary with plain C structs.

The Rust platform owns an `AxWorkerPool` behind the global snapshot worker.
`snapshot_inner` merges AX refinements by `WindowId` and emits a complete CG
inventory even when individual AX workers time out.

## TODO

- [x] Make `BoundedWorker` use one absolute deadline and expose non-blocking
      submit/poll primitives with stale-epoch rejection.
- [x] Split CG candidate collection from per-PID AX refinement in the macOS
      bridge and apply AX messaging timeouts.
- [x] Add the persistent PID-keyed worker pool and merge partial results.
- [x] Route AX-backed window mutations through the target PID worker or an
      equivalent bounded single-flight path.
- [x] Expose snapshot wedge duration through `dyn Platform`.
- [x] Add regression tests for deadlines, single-flight behavior, partial
      results, recovery, and diagnostics.
- [x] Run formatting, clippy, workspace tests, and inspect the changed files.

Live verification completed with a temporary Cocoa app that blocked its main
thread: refresh returned in 540 ms, a subsequent ping returned in 19 ms, the
hung window remained present with AX state `unknown`, five healthy windows kept
known AX state, and the flight recorder identified the timed-out PID.

## Follow-up: observable partial failures

- [x] Add a bounded platform-diagnostic drain so per-PID AX timeouts are not
      hidden by an otherwise successful partial snapshot.
- [x] Record drained diagnostics in the daemon flight recorder with the PID and
      operation.
- [x] Verify the diagnostic path with focused tests and workspace checks.

## Acceptance criteria

- A slow PID cannot make healthy PID refinements miss the shared snapshot
  deadline.
- Repeated snapshots never run more than one AX request concurrently per PID.
- Timed-out PID fields remain `Unknown`; the CG window inventory is retained.
- A late result cannot overwrite a newer snapshot.
- AX-backed mutations return a bounded error rather than blocking the daemon.
- `rovr doctor` can report a wedged global snapshot worker.
