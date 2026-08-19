# Roadmap

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
- [x] `doctor` command
- [x] macOS bridge boundary
- [x] pure BSP/stack/master/columns/monocle layout engine

## M1: useful on macOS without private APIs

- [ ] enumerate windows through CoreGraphics + Accessibility
- [ ] resolve CGWindowID <-> AXUIElement reliably
- [ ] focus window
- [ ] set window frame
- [ ] observe AX window lifecycle
- [ ] display topology observation
- [ ] sleep/wake generation bump and complete refresh
- [ ] query output compatibility layer for yabai scripts

## M2: port the yabai private capability layer

- [ ] audit current yabai scripting-addition surface
- [ ] port only required SkyLight symbols behind the C ABI
- [ ] feature/capability probing instead of OS-name checks in core
- [ ] move window between Spaces
- [ ] create/destroy/focus/reorder Spaces
- [ ] layer, sticky, opacity, shadow and PiP capabilities
- [ ] hard timeouts around every private transition

## M3: window manager

- [ ] wire pure layouts into workspace desired state
- [ ] BSP tree mutation model (insert/remove/rotate/mirror)
- [ ] reactive rules
- [ ] named workspaces
- [ ] persistent workspace restoration
- [ ] scratchpads

## M4: ecosystem

- [ ] stable subscription API
- [ ] shell completions
- [ ] skhd compatibility / optional built-in keybinds
- [ ] Swift menu-bar diagnostics UI
- [ ] layout plugin protocol, likely WASM

## Explicitly deferred

- arbitrary native plugins inside the daemon
- distributed state ownership
- a full Rust rewrite of every private macOS call
- GUI-first configuration
