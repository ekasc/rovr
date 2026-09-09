# ROVR user guide

ROVR is a tiling window manager for macOS. A daemon watches your
displays, spaces, and windows, decides where everything goes, and
moves windows there. You steer it with a TOML config, keybinds, and
the `rovr` command.

You need a Mac running macOS 13 or newer, Xcode Command Line Tools,
and a Rust toolchain (`cargo`, for the build). ROVR also needs
Accessibility permission before it can move anything. That comes
second in the steps below.

## Install ROVR

1. Fetch the repo and run the installer:
   ```sh
   git clone https://github.com/ekasc/rovr.git && cd rovr
   ./scripts/install.sh
   ```
   The script builds optimized binaries, installs `rovr` and `rovrd`,
   registers the LaunchAgent that keeps the daemon alive, and writes a
   starter config if you have none. For system-wide binaries, run
   `PREFIX=/usr/local ./scripts/install.sh` instead. To remove
   everything, run `./scripts/uninstall.sh`.
2. Check the daemon answers:
   ```sh
   rovr ping
   rovr doctor
   ```
   `doctor` reports daemon health, capabilities, the config file in
   use, and a layout summary. If it cannot connect, the daemon is not
   running. See [Manage the daemon](#manage-the-daemon).

## Grant Accessibility permission

1. Open **System Settings** > **Privacy & Security** > **Accessibility**.
2. Enable `rovrd`. If you run the daemon in a terminal instead, enable
   your terminal app.
3. Restart the daemon.
4. Run `rovr doctor` and confirm `control_available`.

Without this grant, ROVR sees windows but cannot move them.

## What works without touching SIP

Stock macOS plus the Accessibility grant covers everything in this
guide so far. Only the second column needs the scripting addition:

| Capability | Stock macOS | With scripting addition |
|------------|-------------|-------------------------|
| Observe displays, spaces, windows | yes | yes |
| Tile windows, layouts, rules | yes | yes |
| Move and focus windows | yes | yes |
| Move a window to another space | yes | yes |
| Focus a space | yes | yes |
| Create, destroy, reorder spaces | no | yes |
| Window opacity, layer, sticky, shadow, scale | no | yes |
| Keybinds, subscriptions, SketchyBar sync | yes | yes |

The addition injects code into the Dock, which needs two targeted SIP
relaxations. Full SIP off is never required. Set the flags from
recovery mode, then run:

```sh
sudo rovr sa install
```

[`SA_SIP.md`](SA_SIP.md) lists the exact flags and the security model.
[`SA.md`](SA.md) documents the protocol. To see where you stand, run
`rovr sa status`. Without the addition, `doctor` reports those
capabilities as false and everything else keeps working. Nothing fails
silently.

## Try ROVR in five minutes

We open windows and move them. Each step shows a visible result.

1. Open two or three windows on one space.
2. Move focus between them:
   ```sh
   rovr window focus-direction west
   rovr window focus-direction east
   ```
   Focus follows the direction you name.
3. Rotate the arrangement, then even it out:
   ```sh
   rovr layout rotate
   rovr layout balance
   ```
   The tiles reshuffle, then equalize.
4. Pull one window out of tiling and put it back:
   ```sh
   rovr window toggle-float
   rovr window toggle-float
   ```
   The window floats free, then rejoins the tiles.

If windows rearranged, everything works. If nothing moved, the cause
is almost always the missing Accessibility grant above. The
[concepts](#rovrs-model-of-your-mac) and [commands](#run-daily-commands)
sections below explain what you just did.

## ROVR's model of your Mac

**Display** is a physical screen. ROVR tiles each display on its own.

**Space** is a macOS Mission Control space. ROVR focuses, creates, and
destroys spaces, and tracks the current space per display.

**Workspace** is ROVR's own layer on top: a named group such as `code`
with a layout and a preferred display. A persistent workspace survives
restarts. Use workspaces for durable homes (code, chat) and bare
spaces for the rest.

**Layout** is how a space arranges windows: `bsp` splits space in
alternating halves, `stack` piles windows with the top one visible,
`master` keeps one large window beside a side stack, `columns` lines
windows up evenly, `monocle` fills the area with one window, and
`float` opts out of tiling.

**Tiled vs floating.** Managed windows tile. ROVR leaves the rest
alone: native-fullscreen windows, minimized windows, whole system
spaces, windows matching a `float` rule, and background-app windows
whose state macOS will not disclose. Skipping unverifiable windows is
deliberate. Guessing would move the wrong window.

## Configure ROVR

Your config lives at `~/.config/rovr/rovr.toml`. The commented example
at [`config/rovr.example.toml`](../config/rovr.example.toml) tours the
options. [CONFIG_REFERENCE.md](CONFIG_REFERENCE.md) documents each
one. Three commands cover the workflow:

```sh
rovr config dump          # minimal starter config
rovr config dump --full   # every resolved default
rovr config check <file>  # validate a file without applying it
```

Saving the file changes nothing by itself. Reload after every edit:

```sh
rovr config reload            # reload the daemon's config file
rovr config reload <file>     # reload this file instead, and remember
                              # it as the config file from now on
```

A reload that fails validation changes nothing. The old config keeps
running and the error names the problem. A successful reload also
heals workspace and space bookkeeping, so reload first whenever state
looks odd, including after sleep or a Dock restart.

## Bind keys

`[[bind]]` blocks give you global keybinds handled inside the daemon.
A press costs no helper process:

```toml
[[bind]]
key = "alt - h"
command = "window focus-direction west"
```

Keys use skhd syntax (`alt - h`, `shift + alt - r`). Commands are
`rovr` invocations without the binary name. Start with these:

```toml
[[bind]]
key = "alt - h"
command = "window focus-direction west"

[[bind]]
key = "alt - l"
command = "window focus-direction east"

[[bind]]
key = "alt - 1"
command = "workspace focus 1"

[[bind]]
key = "shift + alt - r"
command = "config reload"
```

Keep each key combination in exactly one place. If skhd binds it too, skhd eats
the press and ROVR never sees it. A dead keybind with no error
anywhere means the press went elsewhere. `rovr config gen-skhd`
prints an skhd config generated from these blocks when you move binds
across.

## Run daily commands

Most window commands act on the focused window unless you pass an id.
Layout commands act on the focused space unless you pass `--space`.

```sh
# focus and placement
rovr window focus-direction west
rovr window move-to-space 482 3       # window 482 to space 3
rovr window move-to-workspace chat    # focused window to a workspace
rovr window toggle-float              # float the window, again to tile it
rovr window toggle-fullscreen
rovr window close
rovr window resize east +20           # push one edge outward

# spaces
rovr space focus 2
rovr space focus-recent               # flip back to the previous space
rovr space create
rovr space destroy
rovr space toggle-insets              # collapse all gaps and padding,
                                      # run again to restore

# layouts
rovr layout rotate
rovr layout mirror
rovr layout balance
rovr layout set-ratio 0.6

# workspaces and scratchpad
rovr workspace focus code
rovr workspace move-window chat
rovr scratchpad toggle term           # summon a named pad, again to dismiss
```

To see what ROVR sees:

```sh
rovr query windows    # every known window, with managed and fullscreen flags
rovr query spaces
rovr query displays
rovr query focused    # the focused window in detail
rovr query --current  # compact space, display, and focus snapshot as JSON
rovr query state      # full desired-plus-observed state (large output)
rovr subscribe        # stream daemon notifications
```

For shell completions, run
`rovr completions bash` (also zsh, fish, powershell, elvish).

## Reserve screen edges

To keep edges clear for SketchyBar, a dock, or spacing:

```toml
[layout.padding]
top = 28
right = 0
bottom = 0
left = 0
```

Padding shrinks each display's usable area before tiling runs. It
differs from `gap`, which spaces tiled windows apart. Reload to apply.
No restart is needed. Protocol, event contract, and wiring examples:
[STATUS_BAR.md](STATUS_BAR.md).

## Manage the daemon

The LaunchAgent `com.rovr.daemon` starts ROVR at login and restarts it
on failure. To intervene:

```sh
launchctl kickstart -k "gui/$(id -u)/com.rovr.daemon"   # restart
launchctl bootout "gui/$(id -u)/com.rovr.daemon"        # stop
launchctl bootstrap "gui/$(id -u)" \
  ~/Library/LaunchAgents/com.rovr.daemon.plist          # start
```

Logs go to `/tmp/rovr.log` and `/tmp/rovr.err.log`. Both files append
across restarts, so old entries linger at the top. Check timestamps
before you trust a line. Entries from a previous install look alarming
and mean nothing.

## Fix problems

**`doctor` cannot connect.** The daemon is down. Confirm the
LaunchAgent is loaded (`launchctl list | grep rovr`), then read the
tail of `/tmp/rovr.err.log`.

**Windows do not move.** Grant Accessibility permission: **System
Settings** > **Privacy & Security** > **Accessibility**, enable
`rovrd`, restart the daemon, confirm `control_available` in `doctor`.

**Edits change nothing.** You saved but did not reload. Run
`rovr config reload` after saving, every time. A reload error leaves
the old config running.

**A keybind does nothing.** Something else grabbed the combination first,
usually skhd. ROVR logs the presses it receives, so silence means the
press never arrived. Move the binding to one system.

**A window never tiles.** Run `rovr query windows` and read that
window's flags. Native-fullscreen and minimized windows stay out by
design, as do background-app windows macOS will not report on. A
`float` rule or a `float` layout on that space does the same. An
ordinary managed window that still will not tile is a real bug.
Capture `rovr query state` and the log tail.

**Padding or gaps vanished.** `space toggle-insets` collapses every
inset for the session. Run it again to restore everything.

**State looks stale after sleep or a Dock restart.** Reload.
`rovr config reload` heals workspace and space bookkeeping in addition
to loading edits.

**You need the event stream.** `rovr debug events` tails the bounded
in-daemon flight recorder.

---
ROVR takes inspiration from yabai.
