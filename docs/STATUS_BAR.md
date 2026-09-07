# External status interface

ROVR owns state. Consumers (SketchyBar, Hammerspoon, Übersicht, Raycast
scripts) own presentation. ROVR never executes `sketchybar`, detects it,
spawns shells on focus changes, or renders status-bar UI.

Three layers, from live to ad-hoc:

```text
ROVR
 ├── native SketchyBar Mach trigger   -> low-latency status UI
 ├── com.rovr.state.changed            -> generic integrations
 └── rovr query current                -> scripts/debugging
```

## 1. Native SketchyBar transport (live path)

On every deduped `PublicState` change the daemon fires a `rovr_state`
trigger straight into the running bar over Mach — no `rovr query`, no
shell, no `jq`, no subprocess, no polling:

```text
macOS event
  -> ROVR updates PublicState
  -> Mach message to SketchyBar
  -> item updates
```

Transport: SketchyBar's own Mach helper protocol (`src/mach.c` in the
SketchyBar repo, v2.24.0): bootstrap service `git.felix.sketchybar`,
message = NUL-joined `--trigger` argv sent as one out-of-line region.
ROVR vendors only that client send path, with two hardening tweaks:
zero-timeout send (a wedged bar can never stall the state thread) and no
awaited response (fire-and-forget). The looked-up send right is released
after each send, so nothing leaks across the daemon's lifetime. Lookup
happens per send, so a restarted bar is picked up on the next change
with no polling and no reconnect tracking.

The send runs synchronously on the daemon's single state-loop thread,
inside `maybe_publish_public_state()` right after the `PublicState`
equality check passes — so the same dedup that guards the distributed
notification guards SketchyBar updates, and sends are inherently
in-order with no queue that could backlog (each send already *is* the
latest state).

Failure is silent and local: bar absent / no port / rejected message
just reports `false`, logged at debug level only. Window management
never blocks, retries, or waits; the next state change tries again.

### Native event contract (stable)

Event name: `rovr_state`. Variables (raw ROVR ids; empty when absent —
ROVR never invents labels like `"Desktop"`, presentation is the bar's):

```text
SPACE      e.g. 1098 (empty when nothing observed yet)
DISPLAY    e.g. 2
WINDOW_ID  e.g. 482 (empty when no window focused)
PID        e.g. 1234
APP        e.g. Ghostty
TITLE      e.g. ~/src/rovr
```

Encoding: values travel as NUL-separated C strings, so spaces, quotes,
`$`, `;`, backticks, unicode, and emoji pass through literally — this is
not a shell command and is never shell-quoted. The only transformation
is NUL bytes inside app/title, which cannot cross the boundary and are
replaced with U+FFFD.

### SbarLua example (reactive path — no processes spawned)

Verified against the SbarLua API (`sbar.add("event", <name>)` for custom
trigger events, `item:subscribe(event, fn)` receiving trigger variables
as `env`):

```lua
-- ~/.config/sketchybar/init.lua (needs the SbarLua module installed;
-- see https://github.com/FelixKratz/SbarLua)
package.cpath = package.cpath .. ";/Users/" .. os.getenv("USER") .. "/.local/share/sketchybar_lua/?.so"
local sbar = require("sketchybar")

sbar.add("event", "rovr_state")

local rovr = sbar.add("item", "rovr", {
  position = "center",
  icon = { drawing = false },
  label = { max_chars = 80 },
})

rovr:subscribe("rovr_state", function(env)
  local space = env.SPACE ~= "" and env.SPACE or "?"
  local app = env.APP or ""
  local title = env.TITLE or ""

  local label
  if app == "" then
    label = space .. " │ Desktop"
  elseif title == "" then
    label = space .. " │ " .. app
  else
    label = space .. " │ " .. app .. " — " .. title
  end

  rovr:set({ label = label })
end)
```

The callback spawns nothing: rendering comes purely from the event env.

### Startup initialization

Reactive events don't reach a bar that starts after ROVR. The supported
init is a **one-time** query at SketchyBar startup (a per-event CLI call
is explicitly not the live path):

```sh
# in sketchybarrc, once at startup (@sh quotes values safely for eval;
# the only thing it cannot carry is a literal NUL, which the next
# reactive event corrects anyway):
args="$(rovr query --current | jq -r '[
  "SPACE=\(.space // "")",
  "DISPLAY=\(.display // "")",
  "WINDOW_ID=\(.window.id // "")",
  "PID=\(.window.pid // "")",
  "APP=\(.window.app // "")",
  "TITLE=\(.window.title // "")"
] | @sh')"
eval "sketchybar --trigger rovr_state $args"
```

(For SbarLua configs, one `sbar.exec("rovr query --current", ...)`
at init with the same mapping is the equivalent.)

## 2. Distributed notification (generic integrations)

`com.rovr.state.changed` via `NSDistributedNotificationCenter`, full JSON
snapshot in `userInfo["state"]`. Same triggers and same dedup as above.
For Hammerspoon, Übersicht, and other consumers — not for the live
SketchyBar item.

## 3. CLI query (scripts/debugging)

`rovr query --current` (or `rovr query current`) prints the canonical
snapshot as bare JSON. Same builder as both publication paths, so the
three layers can never drift.
