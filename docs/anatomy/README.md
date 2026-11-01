# Rovr reconciler bench

An isometric figure that explains how Rovr works. The object is a bench-top
reconciliation instrument: two feelers, one set by the layout that is wanted and
one resting on what macOS actually has, press the two ends of a comparator beam;
the beam's tilt drives a dial and a lead screw that packs the window tiles until
the two agree, and a chart recorder writes down every correction.

The mapping to the architecture:

| Rovr | The machine |
|---|---|
| DesiredState | five eccentric cams under a sprung follower bar |
| ObservedState | a feeler riding the ramp the fence carries |
| Reconciler | the comparator beam on its column, and the dial it drives |
| Action / executor | the lead screw and the fence that packs the row |
| Platform boundary | the plain coupling between servo and screw |
| Generations | the counter and pawl that lift the feeler to re-observe |
| Flight recorder | the chart recorder |

## Build

```sh
node build.mjs           # rovr-reconciler.html
node build.mjs --light   # rovr-reconciler-light.html
node build.mjs --audit   # geometry audit; must end "audit passed"
node build.mjs --at      # print the screen coordinates of named parts
```

`build.mjs` imports the kit from `~/.agents/skills/anatomy/kit` (override with
`ANATOMY_DIR`). `live.js` is inlined into the page, so the two HTML files are
standalone.

## Verify

```sh
ANATOMY=~/.agents/skills/anatomy
export CHROME_PATH=<chrome-headless-shell>
node $ANATOMY/scripts/capture.mjs rovr-reconciler.html --out shots/final-dark.png --width 1000 --height 700
node $ANATOMY/scripts/lines.mjs  rovr-reconciler.html       # line ends: 0
node $ANATOMY/scripts/lines.mjs  rovr-reconciler-light.html # line ends: 0
node build.mjs --audit                                      # audit passed
```

Last verified: `audit passed` (161 solids, 94 items, one 0.25 px² depth pair
excused), `line ends: 0` in both themes, no console errors, and the loop
exercised with `drive.mjs` (nudge, settle, generation bump, re-observe).

## Controls

Drag a cam to change the wanted layout, drag a window tile to simulate macOS
moving it, `[` and `]` select a cam, arrows turn it, `Space` nudges the row,
`G` bumps the generation, `Home` packs the row.
