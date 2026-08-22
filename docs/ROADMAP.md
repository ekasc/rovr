# Roadmap — truthful status as of 2026-08-21

Legend: `[ ]` not implemented · `[~]` partial (types/IPC/schema exist but user-visible behavior not verified on macOS) · `[x]` verified complete (works on macOS without yabai, exercised by `cargo test` + manual verification)

See `TODO.md` for the 2026-05-11 audit baseline. Do not promote `[~]` to `[x]` without macOS verification.

## M0: architecture bootstrap

- [x] Rust workspace
- [x] typed IDs and snapshots
- [x] observed/desired state split
- [x] generation invalidation
- [x] pure reconciliation path
- [x] typed IPC protocol
- [x] Unix socket daemon
- [x] CLI client
- [x] config parsing and validation
- [x] mock platform
- [x] flight recorder
- [x] `doctor` command (now reports SA socket/version/caps)
- [x] macOS bridge boundary
- [x] pure BSP/stack/master/columns/monocle layout engine

## M1: useful on macOS without private APIs

- [x] enumerate windows through CoreGraphics + Accessibility (now `All` spaces, not just on-screen)
- [x] resolve CGWindowID <-> AXUIElement reliably
- [x] focus window
- [x] set window frame (+ minimize/unminimize via AX `kAXMinimizedAttribute`)
- [~] observe AX window lifecycle — polling + `CGDisplay` callback + AX `minimized/fullscreen/managed` via `kAXMinimized/AXFullScreen/Role/Subrole` (filtered dialogs/popovers); still no `AXObserver` subscriptions, no `AXHidden` handling
- [x] display topology observation
- [x] sleep/wake generation bump and complete refresh
- [~] query output compatibility layer — `rovr query windows/spaces/displays` now truthfully reports `minimized/fullscreen/managed` (via AX) but `label` still synthesized

## M2: port the yabai private capability layer

- [x] audit current yabai scripting-addition surface (`docs/YABAI_PORT.md`)
- [x] port only required SkyLight symbols behind the C ABI (`bridge.m` dlsym + macho lookup, capability probing, SA-free move-window & focus-space)
- [x] feature/capability probing instead of OS-name checks in core
- [x] move window between Spaces (SA-free via `SLSPerformAsynchronousBridgedWindowManagementOperation` / compat workaround)
- [x] focus Space SA-free via gesture synthesis (SA-preferred when live)
- [~] create/destroy/reorder Spaces — typed `Action` + `SpaceCommand` + SA client over **Rovr-owned** socket `/tmp/rovr-sa-<uid>.sock` (versioned `rovr-sa-1.` handshake, `rovr sa status` + `doctor.sa`); payload `install` still stub (bails, SIP docs in `docs/SA.md/SA_SIP.md`), so SA-gated ops still require payload when built
- [~] layer, sticky, opacity, shadow and PiP — typed `Action`s + SA opcodes over Rovr socket; gated on `scripting_addition`, SA-free fallback none
- [x] hard timeouts around every private transition — 2 s deadline on SA socket (all SA ops)
- [~] **Rovr-owned scripting-addition** — namespace, versioned protocol, handshake, `sa install|uninstall|status`, `doctor` diagnostics done; payload NOW BUNDLED (`crates/rovr-sa-payload` dylib + `crates/rovr-sa-loader` root injector, real install lifecycle). Protocol verified end-to-end against the live payload (handshake + op frames + honest capability states); Dock injection itself still requires SIP relaxation and is NOT verified end-to-end — see `docs/SA_SIP.md`

## M3: window manager

- [~] wire pure layouts into workspace desired state — `apply_layout` per-space tiling with gap/padding, now takes `&WorkspaceRegistry` for layout resolution
- [~] BSP tree mutation model — **persistent** `BspTree{Leaf|Split{axis,ratio,left,right}}` per-space (`layout_state::LayoutState{bsp}`), `insert` rightmost deterministic, `remove` collapse, `swap`/`warp`/`balance`/`rotate`/`mirror`/`set_ratio` (0.1..0.9), `sync_with_windows` (sorted), `placements` with `inset_area`; `Engine` `swap/warp/balance/set_ratio`, protocol `Window::Swap/Warp` + `Layout::Balance/SetRatio`, CLI, daemon handlers, persistence via `state.json`; not yet verified via macOS daily-drive
- [~] reactive rules — rules now COMPILE to regex matchers once per load (`Config::compile_rules`) and runtime matching uses them (no equality/substring drift); rule-derived `desired.space` rebuilt from scratch every cycle (stops matching ⇒ stops pulling); invalid regexes rejected at load; deterministic order preserved; `opacity/layer` stored but not yet reconciled to `Action`; not verified via macOS daily-drive
- [~] named workspaces — `WorkspaceRegistry{name->WorkspaceState{desired_display,persistent,backing_space}}` from `[[workspace]]`, `remap_after_snapshot` (clears stale `SpaceId`, claims unclaimed sorted by `position`, `display="main"` → focused display), `ensure_persistent`, `Engine::focus_workspace(name)` / `move_window_to_workspace`, protocol `Workspace::{Focus,MoveWindow}` + `Window::MoveToWorkspace`, CLI `workspace focus` / `workspace move-window` / `window move-to-workspace`, persists `workspaces` in `state.json`, survives restart even when macOS `SpaceId` changes; no `CreateSpace` fallback for missing persistent yet
- [~] persistent workspace restoration — logical workspaces OWN their layout state: BSP trees persist under stable workspace names (`workspace_layouts`), remaps carry state old→new backing Space; missing persistent workspaces recreated one deterministic `CreateSpace` per cycle (capability-gated); remap deterministic via config ordinals + position resume; not verified via macOS daily-drive
- [~] scratchpads — `ScratchpadState` bool + `Engine::toggle_scratchpad(name)->Vec<Action>` (locates first `app/title` match, open: `Unminimize→MoveToSpace(focused)→SetWindowFrame(800×600 centered)→Focus`, closed: `Minimize` via new `Action::SetWindowMinimized`+`bridge_set_window_minimized`), `enumerate_windows` switched to `All` so minimized scratchpad stays observed; `open==don't tile` still holds; single-window, no spawn command yet, not yet verified hide/show survives Dock restart

## M4: ecosystem

- [x] stable subscription API — `Notification::{Hello,StateChanged,LayoutChanged,ScratchpadToggled,ConfigReloaded,Unknown}`, bounded eviction, ACK
- [x] shell completions
- [~] skhd compatibility / optional built-in keybinds — `[[bind]]` → `gen-skhd` works; built-in global hotkey backend architecturally fixed for macOS: daemon MAIN thread runs the AppKit event loop (Carbon event target requirement), socket accept on a worker thread; binds validated at config load via the ONE shared command parser (`rovr_protocol::command_parser`); invalid bind commands fail load and execute nothing at runtime (never substituted); hotkey firing on a live UI session not yet verified
- [~] Swift menu-bar diagnostics UI — now at `apps/rovr-menu-bar` as diagnostics/control surface **only via public IPC** (`rovr doctor`/`query state`/`sa status`/`debug events` via `Process`/`/usr/bin/env rovr`), shows daemon/SA/capabilities/workspace/layout, `Reload Config` + `Open Diagnostics`, 5 s refresh, no layout logic in Swift
- [~] layout plugin protocol, likely WASM — wasmi 0.40 runtime with REAL resource limits: `StoreLimits` attached per call (16 MiB linear memory cap, table cap, trap-on-grow-failure) + fuel 1M timeout + no host imports; plugin output VALIDATED before use (`validate_placements`: count/duplicates/foreign/finite/positive/bounds — invalid output discarded wholesale, built-in fallback); regression tests for memory-hog containment and all validation classes; live-macOS plugin behavior not yet verified

## Explicitly deferred

- arbitrary native plugins inside the daemon
- distributed state ownership
- a full Rust rewrite of every private macOS call
- GUI-first configuration
