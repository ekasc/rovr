# TODO — Rovr Reality Audit (2026-05-11 inspection)

> Do not trust ROADMAP.md. This file was produced by reading the code on this commit.

## 1. What currently works end-to-end on real macOS

- **Rust workspace / IPC / config / flight recorder**: typed IDs, Observed/Desired split, generation bump on wake/Dock/display, Unix socket daemon (`/tmp/rovr-<uid>.sock`), versioned `rovr-protocol` (v1), CLI `rovr` round-trips, `rovr doctor` (minimal), config hot-reload, `state.json` persistence of `Layouts` + `ScratchpadState`, `cargo test` green, `cargo clippy` clean (mod deprecated warning).
- **Window enumeration**: `CGWindowListCopyWindowInfo` + `CGDisplayBounds` + `_AXUIElementGetWindow` mapping, per-window `app`, `bundle_id` (via NSRunningApplication), `title`, `frame`, `display_id`, app-level focused window id. Filtering of layer !=0 and tiny windows.
- **Display enumeration**: `CGGetActiveDisplayList` + `CGDisplayBounds`, main display marked focused.
- **Space enumeration (SA-free)**: SkyLight via `dlsym(RTLD_DEFAULT, …)` for `SLSMainConnectionID`, `SLSCopyManagedDisplaySpaces`, `SLSCopyManagedDisplayForSpace`, `SLSManagedDisplayGetCurrentSpace`, `SLSSpaceGetType`; per-space `id`, `display_id`, `position`, `focused`, `type`. Capability-probed, not version-gated.
- **Move window to Space (SA-free path)**: `SLSPerformAsynchronousBridgedWindowManagementOperation` (`SLSBridgedMoveWindowsToManagedSpaceOperation`, resolved via macho symtab scan) on macOS 13+, direct `SLSMoveWindowsToManagedSpace` on older builds, compat-workspace workaround (`SLSSpaceSetCompatID` + `SLSSetWindowListWorkspace` with `0x726f7672`) as fallback. Verified usable without SA on macOS 12.7+/13.6+/14.5+/15+.
- **Focus Space (SA-free path)**: gesture synthesis (`kCGSEventDockControl` / `IOHIDEventTypeDockSwipe` swipe), no SA required. Preferred SA path is tried first when SA present.
- **Set frame / Focus window**: via Accessibility (`AXUIElementSetAttributeValue` kAXPosition/kAXSize, `kAXFocusedAttribute` + `NSRunningApplication activate`).
- **Per-space tiling loop**: `layout::apply_layout` groups `managed && !fullscreen` windows by `space_id` → display frame, calls `rovr_layout::compute` per space, writes `desired.frame`; `reconcile()` emits `SetWindowFrame`/`MoveWindowToSpace`/`FocusWindow`. `poll` + `needs_refresh` path via `CGDisplayRegisterReconfigurationCallback`.
- **Layout kinds**: pure `compute()` for `bsp`/`stack`/`monocle`/`columns`/`master`/`float` with gap/padding/validation; deterministic and fully tested.
- **Layout orientation (partial)**: per-space `Orientation { axis, reversed }` with `rotate`/`mirror` cycling 4 states, persisted, toggled via `rovr layout rotate|mirror`. Area-transpose trick for horizontal BSP.
- **Rules (minimal)**: `floating == Some(true)` rules match on `app == bundle_id` && `title.contains` && `workspace == space.label`; matched windows get `desired.frame = None` (left floating). Empty rules → tile as before.
- **Scratchpads (minimal)**: config + `ScratchpadState` bool per name; `apply_layout` floats windows matching an *open* pad (any pad, not first-match). Toggle via `rovr scratchpad toggle`.
- **Named workspaces (schema only)**: `[[workspace]] name/layout/display/persistent` parsed, validated, and `resolve_layout` picks `workspace.layout` when `space.label == workspace.name`, else global. Currently dead because `space.label` is always `None` (see below).
- **Subscription API**: `rovr subscribe` streaming notifications (`Hello`, `StateChanged`, `LayoutChanged`, `ScratchpadToggled`, `ConfigReloaded`, `Unknown` forward-compat), bounded 64-slot per-subscriber channel with non-blocking `try_send` eviction; `Response::ok` ACK required before stream.
- **CLI extras**: `rovr config gen-skhd` generates skhd lines from `[[bind]]`, shell completions include top commands.
- **Menu bar**: Swift stub exists in `apps/rovr-menu-bar` (not yet a diagnostics surface).

## 2. What is only partially implemented

- **Scripting-addition transport**: `crates/rovr-platform/src/macos/sa.rs` speaks yabai's length-prefixed binary protocol (handshake 0x01, opcodes 0x02–0x0D) against `/tmp/yabai-sa_<USER>.socket` with 2 s deadline. Handshake parses `version\x00 + u32 attribs` but version is ignored. `focus_space/create/destroy/move_space/opacity/layer/sticky/shadow/scale` delegate to this client. Probe is best-effort; missing socket → capability false.
- **Space create/destroy/reorder**: exist as `Action` + `SpaceCommand` + `Engine::create_space` etc. and execute via SA; SA-free path does not exist.
- **Opacity / layer / sticky / shadow / PiP (scale)**: typed `Action`s + SA client methods exist; all require SA (reported via `capabilities.scripting_addition`). No SA-free fallback. PiP uses display bounds as rect and lets SA decide scale-in vs scale-out by comparing transform to identity.
- **Observed state fields**: `crates/rovr-platform/src/macos/mod.rs` snapshot synthesizer hardcodes `minimized = false`, `fullscreen = false`, `managed = true`, `space.label = None`, `generation = 0` (overwritten to engine generation after), `display.label = None`, `WindowSnapshot.space_id` via `SLSCopySpacesForWindows(0x7)` (best-effort). Dock-restart / sleep bump generation and trigger `RefreshAll`, but per-window AX attributes for minimized/fullscreen/role/subrole not read.
- **BSP**: `rovr_layout::bsp` is stateless recursion `split list in half` alternating vertical/horizontal; `Orientation` only transposes area / reverses order. No persistent `Node { Split { axis,ratio,left,right } | Leaf(WindowId) }`, no per-node ratio, no `swap`/`warp`/`balance`, no insertion/removal/collapse, no persistence beyond orientation, topology derived from enumeration order.
- **Workspaces**: model exists, but no `WorkspaceId` abstraction, no `desired_display`/`backing_space`/`persistent` lifecycle, no remap after restart/Dock重启/display change, no `rovr workspace focus <name>` or `move-to-workspace` commands.
- **Scratchpads**: single `bool` per named pad, no window identity, no show/hide/position/focus, no preserve-previous-workspace/frame, no "locate/spawn", can become tiled if logic regresses; multi-window explicitly deferred.
- **Rules**: only `float`; no `tile`/`move_to_workspace`/`opacity`/`layer`/`focus` actions, no event-driven evaluation (`window_created`, `title_changed`, …), no determinism harness beyond float.
- **Built-in hotkeys**: `[[bind]]` is config→skhd generator only; no global hotkey listener, no separate core-policy boundary.
- **WASM layout plugins**: `crates/rovr-layout-plugin` is protocol registry + JSON ABI (`WasmLayoutRequest`/`WasmLayoutResponse`) with no runtime, no module loading, manifest validation, fuel/timeout, memory limits, isolation, or fallback.
- **Menu bar**: stub `main.swift` only; no daemon state / SA state / capabilities / workspace / layout / errors / reload / diagnostics via public IPC.
- **Doctor / diagnostics**: `rovr doctor` prints protocol, capabilities, generation, counts, config path, layout/gap/interval; does not report SA socket path, payload version, handshake attribs, per-capability diagnostics, or injection status.
- **Persistence**: `Layouts` + `ScratchpadState` JSON to `~/.config/rovr/state.json`; workspace remap, per-node BSP, scratchpad window identity not persisted.

## 3. What is falsely marked complete

`docs/ROADMAP.md` as of this audit marks as `[x]`:

- M2 `port the yabai private capability layer` including `audit surface`, `port only required SkyLight symbols`, `capability probing`, `move window between Spaces`, `create/destroy/focus/reorder Spaces`, `layer, sticky, opacity, shadow and PiP`, `hard timeouts` — **false**: all SA-gated operations still go to `/tmp/yabai-sa_*.socket` (yabai's payload). Rovr ships no payload, no install lifecycle, no own socket namespace, no versioned payload/client protocol, no `rovr sa install|uninstall|status`. `create/destroy/reorder/opacity/layer/sticky/shadow/scale` are unavailable without yabai installed and injected into Dock.
- M3 `wire pure layouts into workspace desired state` / `BSP tree mutation model` / `reactive rules` / `named workspaces` / `persistent workspace restoration` / `scratchpads` — **false**: BSP is not a persistent tree, workspaces are label-lookup on a field that is always `None`, rules are float-only with no events, scratchpads are bool-gated float, persistence is partial. Tests named `m3a*`/`m3c`/`m3d`/`m3e` exercise the scaffold, not daily-drive behavior.
- M4 `stable subscription API` / `shell completions` / `skhd compatibility / optional built-in keybinds` / `Swift menu-bar diagnostics UI` / `layout plugin protocol, likely WASM` — **partial**: subscription + completions + skhd-gen are real; built-in keybinds, menu-bar diagnostics, WASM runtime are scaffolds.

## 4. What still depends on yabai

- **Runtime socket**: `SA_SOCKET_PATH_FMT = "/tmp/yabai-sa_{}.socket"`; `SaClient::new()` reads `$USER` and connects there. `MacPlatform::new()` probes that socket; absence means `capabilities.scripting_addition == false`.
- **Payload**: `src/sa.m` / `src/osax/*` / `src/osax/loader.m` etc. are not vendored; Rovr does not build or inject any code into Dock. `load-sa` injection (`yabai --load-sa`) is yabai-owned.
- **Operations that require yabai SA to function**: `CreateSpace`, `DestroySpace`, `MoveSpace` (reorder), `SetWindowLayer`, `SetWindowSticky`, `SetWindowShadow`, `SetWindowOpacity`, `SetWindowScale` (PiP). `FocusSpace` prefers SA but falls back to gesture synthesis; `MoveWindowToSpace` has SA-free SkyLight path but still probes SA for capability bits `OSAX_ATTRIB_*`.
- **Opportunistic dependency**: capability bits `0x04/0x08/0x10` come from yabai's handshake attribs; Rovr's `capabilities()` maps them directly.
- No `rovr sa install|uninstall|status` exists; no Rovr-owned `sa/` payload crate, no `launchctl`/`osax` loader, no version handshake, no SIP documentation beyond README's brief mention.

## 5. What prevents daily-driving Rovr today

1. **Yabai runtime dependency** — without yabai installed and `--load-sa`, Rovr loses 8 capabilities (create/destroy/reorder space, layer, sticky, shadow, opacity, Pip). Enumerate/focus/move-window still work, but Dock-injected private ops do not. The acceptance list in the north-star (create/destroy/reorder/move-to-space/layer/sticky/shadow/opacity/PiP) cannot pass without yabai. Rovr has no own SA payload, no own socket namespace (`/tmp/rovr-sa_*`), no capability handshake, no versioned protocol, no install lifecycle.
2. **Observed state lies** — `minimized`/`fullscreen`/`managed`/`label`/`generation` hardcodings mean reconciliation tiles fullscreen/minimized/floating windows, cannot distinguish `managed == tileable`, cannot trust `rovr query windows` to debug the WM, and cannot implement float/window-role rules correctly.
3. **No persistent BSP tree** — opening/closing windows reconstructs a new tree from `CGWindowList` order; `rotate`/`mirror` only flip orientation. No deterministic insert/remove/collapse, no per-node ratio, no swap/warp/balance, no topology persistence across reconcile or daemon restart.
4. **Logical workspaces missing** — macOS `SpaceId` is volatile; Rovr has no `WorkspaceId { name, desired_display, backing_space, persistent, layout }`, no `rovr workspace focus <name>` / `window move-to-workspace`, no remap after restart/Dock/display change, no recovery after volatility. The existing label-lookup path is dead (`label: None`).
5. **Scratchpads are not scratchpads** — "open == don't tile" is not show/hide/position/focus/restore/survive-reconcile. No hide (orderOut/miniaturize), no focus-on-summon, no frame restore, no per-pad window identity.
6. **Rules do not react** — no `window_created`/`title_changed`/`app_launched`/`workspace_changed` events, no typed actions beyond float, no deterministic testable evaluation of the richer policy.
7. **Keybinds require skhd** — no built-in global hotkey backend, so a standalone install still needs a second daemon for ergonomics.
8. **WASM plugins unimplemented** — no runtime (wasmtime/wasmi), no manifest/version validation, fuel/timeout/memory limits, error isolation, fallback, or `layout = "plugin:…"` selection.
9. **Menu bar is not a diagnostics surface** — does not speak public IPC, shows no daemon/SA/capability/workspace/layout/errors/reload state.
10. **Reliability gaps** — hard timeout is only on SA socket (2 s); no bounded retries/deadlines for `Action` execution, no verify-after-mutate (`observe → decide → execute → observe → verify → reconcile`), `NeedsRefresh` polled on interval, stale-generation windows produce `RefreshWindow` but no AX observer subscription, no scoped/full refresh on Dock restart / wake / display change / payload disconnect / AX observer failure, subscriber slow-path is handled but platform hangs can block the state loop.

## Execution order (north-star sequence, §Roadmap Discipline)

0. Audit (this file) — done.
1. Rovr-owned scripting addition (priority zero) — own socket ` /tmp/rovr-sa-<uid>.sock`, private-only primitive payload, no layout/policy, hard deadline per op, capability handshake, versioned payload/client protocol, `rovr sa install|uninstall|status`, `rovr doctor` SA reporting, SIP docs scoped to implemented caps.
2. Truthful observation — read `kAXMinimized`, `kAXFullscreen`, `kAXMain`/`kAXFocused`, `kAXRole`/`kAXSubrole`, frame, `valid/manageable/tileable`, per-window `space`/`display`/`generation`, `space.type`/`display.usableBounds` where available; never guess `managed == tileable`; represent unknown as unknown; handle minimized/fullscreen/floating/panels/transients/weird AX/display reconnect/Dock restart/sleep/wake/destruction.
3. Real BSP tree — `Node::{ Split{axis,ratio,left,right} | Leaf(WindowId) }`, deterministic insert/remove/collapse, per-node ratio, rotate/mirror/balance/swap/warp, topology preserved across reconcile, not derived from enumeration order, IPC + persistence, `cargo test` invariants.
4. Logical workspaces — `WorkspaceId` + backing_space remap, named focus/move, per-workspace layout, persistent declaration + recreation, survive restart/Dock/display, CLI `workspace focus` / `window move-to-workspace`.
5. Real scratchpads — named single-window toggle with reliable show/hide/position/focus, preserve workspace/frame, survive reconcile/restart, never tile as normal.
6. Reactive rules — event-driven float/tile/move_to_workspace/opacity/layer/focus, pure deterministic evaluation, no direct macOS calls.
7. Built-in keybinds, 8. WASM runtime, 9. Menu-bar polish, 10. Daily-drive hardening (bounded retries, verify, scoped refresh, `needs_refresh` + AX observer, flight recorder, `rovr doctor` + gated integration tests).

After each phase: `cargo fmt --check` + `cargo clippy -- -D warnings` + `cargo test`, manual macOS verification where relevant, update this file + `docs/ROADMAP.md` truthfully (`[ ]`/`[~]`/`[x]` only when user-visible behavior verified), commit cleanly.
