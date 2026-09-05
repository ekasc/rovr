# Complete the research recommendations

## Acceptance checklist

- [x] Logical workspace registry owns per-workspace layout state; macOS Space
      identifiers remain replaceable backing identities.
- [x] BSP is the default and master-stack is available as a pure built-in
      layout.
- [x] Workspaces map to native macOS Spaces; no off-screen parking exists.
- [x] TOML supports `config-version = 1`, a minimal starter dump, and a full
      resolved-default dump.
- [x] Rules compile into typed selectors rather than parallel optional fields;
      commands remain the shared typed IPC/hotkey model.
- [x] Subscription IPC has typed heartbeats in addition to bounded subscriber
      backpressure.
- [x] Built-in global hotkeys are the native path; `gen-skhd` remains a local
      migration utility.
- [x] Per-application AX workers, absolute deadlines, unknown partial state,
      ghost-window replacement, bounded diagnostics, and mutation isolation
      are implemented.
- [x] Exercise a non-mutating live daemon on an alternate socket and record
      what macOS/TCC verification is still unavailable.
- [x] Run formatting, clippy, workspace tests, and inspect every changed file.

## Constraints

- Keep one daemon state owner.
- Keep private macOS behavior in `rovr-platform`.
- Do not add an i3 string grammar or a universal layout tree.
- Do not disturb the user's primary daemon or mutate their Spaces during live
  verification.

## Live evidence

- An isolated daemon used a float-only config, alternate socket, and alternate
  state path. It observed 54 windows with all AX mutation capabilities present,
  reported no global snapshot wedge, and emitted `Hello` followed by a typed
  heartbeat.
- A temporary Cocoa app deliberately blocked its main thread. Rovr refreshed in
  540 ms, answered a subsequent ping in 19 ms, retained the app's CG window with
  `managed = unknown`, retained known AX state for five healthy windows, and
  recorded `ax.refine_timeout` with the exact PID.
- Built-in hotkey registration succeeded live with one binding. Synthetic
  CGEvents did not trigger Carbon's global-hotkey callback, so physical-key
  firing remains a manual verification item; no code-completable gap was found.
