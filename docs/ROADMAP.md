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
- [~] **Rovr-owned scripting-addition** — namespace, versioned protocol, handshake, `sa install|uninstall|status`, `doctor` diagnostics done; **payload injection not yet bundled** (needs Dock dylib + `csrutil enable --without debug`, see `docs/SA_SIP.md`), blocks SA-gated caps

## M3: window manager

- [~] wire pure layouts into workspace desired state — `apply_layout` per-space tiling with gap/padding, now takes `&WorkspaceRegistry` for layout resolution
- [~] BSP tree mutation model — **persistent** `BspTree{Leaf|Split{axis,ratio,left,right}}` per-space (`layout_state::LayoutState{bsp}`), `insert` rightmost deterministic, `remove` collapse, `swap`/`warp`/`balance`/`rotate`/`mirror`/`set_ratio` (0.1..0.9), `sync_with_windows` (sorted), `placements` with `inset_area`; `Engine` `swap/warp/balance/set_ratio`, protocol `Window::Swap/Warp` + `Layout::Balance/SetRatio`, CLI, daemon handlers, persistence via `state.json`; not yet verified via macOS daily-drive
- [~] reactive rules — `RuleConfig{app,title,workspace,float,target_workspace,opacity,layer}` with `target_workspace` validated; `window_matches_rule` (logical workspace name via `workspaces.name_for_space`), `matches_float_rule` + `target_workspace_for_window` → writes `desired.space` so `reconcile` moves; evaluated every `apply_layout` (covers `window_created/title_changed`); deterministic pure; `opacity/layer` stored but not yet reconciled to `Action`
- [~] named workspaces — `WorkspaceRegistry{name->WorkspaceState{desired_display,persistent,backing_space}}` from `[[workspace]]`, `remap_after_snapshot` (clears stale `SpaceId`, claims unclaimed sorted by `position`, `display="main"` → focused display), `ensure_persistent`, `Engine::focus_workspace(name)` / `move_window_to_workspace`, protocol `Workspace::{Focus,MoveWindow}` + `Window::MoveToWorkspace`, CLI `workspace focus` / `workspace move-window` / `window move-to-workspace`, persists `workspaces` in `state.json`, survives restart even when macOS `SpaceId` changes; no `CreateSpace` fallback for missing persistent yet
- [~] persistent workspace restoration — `Layouts` (`SpaceId` string keys) + `ScratchpadState` + `WorkspaceRegistry` JSON persisted; BSP topology + workspace backing survive restart
- [~] scratchpads — `ScratchpadState` bool + `Engine::toggle_scratchpad(name)->Vec<Action>` (locates first `app/title` match, open: `Unminimize→MoveToSpace(focused)→SetWindowFrame(800×600 centered)→Focus`, closed: `Minimize` via new `Action::SetWindowMinimized`+`bridge_set_window_minimized`), `enumerate_windows` switched to `All` so minimized scratchpad stays observed; `open==don't tile` still holds; single-window, no spawn command yet, not yet verified hide/show survives Dock restart

## M4: ecosystem

- [x] stable subscription API — `Notification::{Hello,StateChanged,LayoutChanged,ScratchpadToggled,ConfigReloaded,Unknown}`, bounded eviction, ACK
- [x] shell completions
- [~] skhd compatibility / optional built-in keybinds — `[[bind]]` → `gen-skhd` works; **built-in global hotkey backend now in `daemon::hotkey`** (macOS `global-hotkey` 0.6, parses skhd `cmd - h` / `alt + shift - r` → `HotKey`, `GlobalHotKeyManager` kept alive, dispatches via public IPC `UnixStream` to daemon socket, separate from core policy, logs, skhd remains supported)
- [~] Swift menu-bar diagnostics UI — now at `apps/rovr-menu-bar` as diagnostics/control surface **only via public IPC** (`rovr doctor`/`query state`/`sa status`/`debug events` via `Process`/`/usr/bin/env rovr`), shows daemon/SA/capabilities/workspace/layout, `Reload Config` + `Open Diagnostics`, 5 s refresh, no layout logic in Swift
- [~] layout plugin protocol, likely WASM — `crates/rovr-layout-plugin` now **wasmi 0.40 runtime**: `WasmPlugin{engine,module,manifest}` with `load_file/load_bytes`, manifest `abi_version` check, `wasm_abi::ABI_VERSION=1`, fuel 1M (timeout), 16 MiB memory via `StoreLimits` (wasmi `ResourceLimiter`), no host imports (pure), `compute` `(i32,i32)->i64` alloc/packed, error isolation (`trap→PluginError`), `Registry::load_wasm_file/bytes`, `Engine` loads `~/.config/rovr/plugins/*.wasm` at `new()` + `reload_config`, `layout.rs` selects `general.plugin` / per-workspace `plugin` via `Registry::get`, fallback to built-in Bsp on missing/trap/fuel, `cargo test` with echo + infinite-loop fuel timeout; still needs `layout = "plugin:my"` docs + `plugin` config docs

## Explicitly deferred

- arbitrary native plugins inside the daemon
- distributed state ownership
- a full Rust rewrite of every private macOS call
- GUI-first configuration
