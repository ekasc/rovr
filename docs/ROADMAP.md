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

- [x] enumerate windows through CoreGraphics + Accessibility
- [x] resolve CGWindowID <-> AXUIElement reliably
- [x] focus window
- [x] set window frame
- [x] observe AX window lifecycle
- [x] display topology observation
- [x] sleep/wake generation bump and complete refresh
- [x] query output compatibility layer for yabai scripts

## M2: port the yabai private capability layer

- [x] audit current yabai scripting-addition surface
- [x] port only required SkyLight symbols behind the C ABI
- [x] feature/capability probing instead of OS-name checks in core
- [x] move window between Spaces
- [x] create/destroy/focus/reorder Spaces
- [x] layer, sticky, opacity, shadow and PiP capabilities
- [x] hard timeouts around every private transition

## M3: window manager

- [x] wire pure layouts into workspace desired state
- [x] BSP tree mutation model (insert/remove/rotate/mirror)
- [x] reactive rules
- [x] named workspaces
- [x] persistent workspace restoration
- [x] scratchpads

## M4: ecosystem

- [x] stable subscription API
- [x] shell completions
- [ ] skhd compatibility / optional built-in keybinds
- [ ] Swift menu-bar diagnostics UI
- [ ] layout plugin protocol, likely WASM

## Explicitly deferred

- arbitrary native plugins inside the daemon
- distributed state ownership
- a full Rust rewrite of every private macOS call
- GUI-first configuration
