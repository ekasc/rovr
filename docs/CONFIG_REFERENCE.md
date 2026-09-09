# ROVR config reference

File: `~/.config/rovr/rovr.toml`. Starter file:
[`config/rovr.example.toml`](../config/rovr.example.toml). The user
guide covers the edit, validate, and reload workflow.

Unknown keys are silently ignored. A misspelled option looks exactly
like an option with no effect. `rovr config dump --full` shows what
ROVR loaded. A reload that fails validation keeps the running config.

## Config version

```toml
config-version = 1
```

Schema version. Only `1` exists. An omitted key defaults to `1`.

## General

```toml
[general]
layout = "bsp"
gap = 10
padding = 8
reconcile_on_wake = true
reconcile_interval_ms = 1000
```

`layout` is the default arrangement for spaces: `bsp`, `stack`,
`master`, `columns`, `monocle`, or `float`. A workspace `layout`
overrides it for that workspace's spaces.

`gap` is pixels between tiled windows. `padding` is a uniform inner
inset around each space's layout area. Negative values fail
validation.

`reconcile_on_wake` re-reconciles after system wake.
`reconcile_interval_ms` sets the re-observation period. Values under
100ms clamp to 100ms. The idle floor is 5 seconds regardless.

`plugin` is an optional WASM layout plugin path.

## Screen padding

```toml
[layout.padding]
top = 28
right = 0
bottom = 0
left = 0
```

Pixels reserved along each display edge before tiling runs, on every
display independently. Each side defaults to `0`. Negative sides fail
validation. Padding that would consume a display (`left + right`
reaching the width, or `top + bottom` reaching the height) skips tiling
there instead of producing degenerate frames.
`space toggle-insets` collapses screen padding with the other insets.

Outer to inner, the three spacings compose as screen padding, then
`general.padding`, then `gap`.

## Focus

```toml
[focus]
follows_mouse = false
```

`follows_mouse` moves focus with the pointer. Default `false`: focus
moves on click and through focus commands.

## Animations

```toml
[animations]
enabled = true
duration_ms = 160
curve = "ease_out_quint"
```

Parsed and validated. Nothing reads these keys yet. Window moves apply
instantly. The section stays valid for the release that wires it up.

## Workspaces

One `[[workspace]]` block per named workspace:

```toml
[[workspace]]
name = "code"        # required, unique
layout = "bsp"       # default bsp
display = "main"     # default any; "main" or a display number
persistent = true    # default false; recreate after restarts
plugin = "..."       # default none; per-workspace plugin override
```

## Rules

`app` and `title` are regexes. `workspace` matches the window's
current workspace name. Every listed matcher must match. An empty rule
matches every window.

Each action scans the file top to bottom and takes its first match.
Specific rules go above general ones. Actions resolve independently:
one window takes `float` from one rule and `target_workspace` from
another.

```toml
[[rule]]
app = "^Finder$"
float = true
```

`float` excludes the window from tiling. `target_workspace` sends it
to a named workspace. `opacity` spans 0.0 to 1.0. `layer` is a window
layer integer. Under `[[rule]]`, `workspace` matches; moving uses
`target_workspace`:

```toml
[[rule]]
app = "^Slack$"
target_workspace = "chat"
```

## Scratchpads

One `[[scratchpad]]` block per named set. Members sit outside tiling
while the pad is open. `toggle` with `rovr scratchpad toggle <name>`.

```toml
[[scratchpad]]
name = "term"
app = "com.apple.Terminal"
```

`app` matches the exact bundle id. `title` matches a substring. An
omitted matcher is a wildcard.

## Keybinds

One `[[bind]]` block per global keybind, handled inside the daemon. No
helper process runs per press.

```toml
[[bind]]
key = "alt - h"                      # skhd syntax
command = "window focus-direction west"  # rovr CLI minus the binary name
```

Key syntax follows skhd (`alt - h`, `shift + alt - r`,
`cmd - return`). A combination bound in skhd as well never reaches ROVR.
`rovr config gen-skhd` emits an skhd config generated from these
blocks.

---
ROVR takes inspiration from yabai.
