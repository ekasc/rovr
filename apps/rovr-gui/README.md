# Rovr GUI

A small native macOS window that exposes Rovr's **settings, features, and
diagnostics** in one place.

It is a thin client. Every read and mutation goes through the public `rovr`
CLI over the existing Unix-socket IPC — the GUI contains no window-management
logic and talks to no private macOS API. Settings are edited as the same TOML
file the daemon owns and then hot-reloaded.

## Build and run

```sh
cd apps/rovr-gui
swift build -c release
.build/release/RovrGui
```

The app finds `rovr` the way your shell would: `$ROVR_BIN`, then `PATH` via a
login shell, then common install locations and local `target/{debug,release}`
builds. The resolved path is shown on the Diagnostics tab.

Requires macOS 13+ and a Swift toolchain (Xcode command line tools).

Headless checks (no window): `RovrGui --self-check` runs one doctor round trip;
`RovrGui --live-check` parses one `query state` and reads the event stream for a
few seconds.

## Sections

The four sections live in a collapsible sidebar (⌘⌃S), with the standard macOS
menu bar, a unified toolbar, and keyboard shortcuts: ⌘R reloads diagnostics,
⌘, opens Settings, ⌘⌃F toggles full screen, ⌘W closes the window.

**Live** — a per-Space desktop gallery driven by the daemon's event stream
(`rovr subscribe`). Each display is shown with its Spaces as separate panels,
ordered left to right; the active Space is outlined in the accent colour. Each
panel draws that Space's windows at their real geometry, so windows from
different Spaces never overlap — only windows genuinely on the same Space stack
(focused windows are filled with the accent). A dashed outline marks the
daemon's *desired* frame when it differs (drift). Below the gallery, health
sparklines (heartbeat age, observation age, reconcile streak) are sampled every
few seconds, and a ticker streams state/layout/scratchpad/config events.
Reconnects automatically.

**Diagnostics**
- Daemon reachability, protocol version, generation, and window/space/display
  counts.
- Health: state-loop heartbeat age, last observation age, reconcile failure
  streak, observation wedge.
- Accessibility control availability.
- Capability grid (each shown as `Available` / `Unsupported`).
- Scripting-addition payload state and reinjection lifecycle.
- Recent events (`rovr debug events`).

**Features**
- Live capability status.
- Controls for layout, spaces, workspaces, scratchpads, and window operations.
  Actions without an id apply to the focused window.
- An activity log showing the result of the last 50 actions.

**Settings**
- Edits `~/.config/rovr/rovr.toml` (the path the daemon reports).
- **Validate** runs `rovr config check` against the edited bytes without
  saving.
- **Save & Reload** validates, writes a timestamped `.bak`, saves, and runs
  `rovr config reload`. A failed validation never overwrites the file.

## Relationship to the menu-bar app

[`apps/rovr-menu-bar`](../rovr-menu-bar) remains the tiny always-on status item.
This app is the richer on-demand control surface. Both use only the public IPC.
