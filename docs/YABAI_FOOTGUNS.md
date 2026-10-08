# Yabai footguns

A catalogue of the pitfalls Yabai hit, distilled from its `CHANGELOG.md`
(v1.0.1 → 7.1.25) and source comments, with the lesson for Rovr.

Yabai is a behavioural reference, not an architectural template. Most of these
are macOS behaviours, not Yabai bugs: the same walls are in Rovr's path, and
the point of this file is to walk into them deliberately instead of
rediscovering them one regression at a time.

Legend: **Hit** = Rovr already ran into it · **Exposed** = Rovr can hit it ·
**Covered** = Rovr already handles it.

---

## 1. AX references for windows you can't reach (Hit)

The single most recurring class of Yabai bugs.

- v7.1.7 "workaround to acquire AX-References for windows on inactive spaces"
  (#2320, #2480); v7.1.12 "…should only run at startup" (#2575).
- v7.0.0: "Windows missing an AX-reference cannot be acted upon until its space
  has become active" (#2126). A `has-ax-reference` query field was added and
  then **removed** once the workaround existed.
- The fix (`src/window_manager.c`) does **not** cache: it reconstructs the
  element with `_AXUIElementCreateWithRemoteToken`, a 0x14-byte token
  `{pid, 0, 'coco', element_id}`, brute-forcing `element_id` 0…0x7fff and
  matching by window id. (Attribution in-source: decodism / alt-tab-macos.)

**macOS reality:** `kAXWindowsAttribute` is empty for any app that is not
frontmost, and windows on inactive spaces are parked off-screen. Rovr hit this
as "Activity Monitor isn't tiling" + a per-cycle `bridge operation failed with
status 1`.

**Rovr now:** caches the AX element per window (fast path) *and* reconstructs
it via the remote token when the public API returns nothing. See
`crates/rovr-platform/src/macos/bridge.m`.

---

## 2. "What counts as a window" (Exposed)

- v6.0.3 "stricter window type filter to avoid Text Completion, Input Source
  changes and other non-windows-that-report-as-windows" (#1919/#1910/#1997).
- v6.0.4–6.0.6 split root windows from child/sub-windows (#2036/#2044).
- v2.0.0 ignore `AXPopover` and subrole `AXUnknown` (#162/#164); v3.2.0 "don't
  modify AXUnknown/AXPopover".
- v6.0.8 "windows with a non-standard window level are floating permanently
  unless `manage=on`" (#2055).
- Source: Finder exposes a phantom element with an **empty window id** (the
  desktop) that must be skipped.

**Rovr now:** filters by `NSApplicationActivationPolicyRegular` and by
subrole/buttons/activatable. That is coarser than Yabai's set; menu-bar extras,
toasts, popovers, and input-method windows are the residual risk.

---

## 3. App / process detection is unreliable (Exposed)

- v7.1.15 "Fetching NSRunningApplication for some processes fails randomly;
  delay and retry" (#2595).
- v6.0.10 apps misreport `isObservable` / `isFinishedLaunching` (#1367).
- v7.0.4 reworked background-process detection (#2190/#2168/#2194); v7.0.3 had
  to **whitelist** zathura because it self-identifies as background-only (#2168).
- v7.1.12 ignore apps being debugged (#599).

**Lesson:** any "is this a normal app" test has false negatives; retry and
allow an override.

---

## 4. Events lie (Hit)

- v3.2.1 "race condition on window-destroy notifications — their API is garbage
  and reports **duplicate** notifications for the same window" (#580).
- v7.1.4 fallback for window-destroyed detection "because of weird system
  problems when other third-party software is in use" (#2431).
- v7.1.15 "change when `window_unobserve` is called" (#2605).
- v2.2.0 "ignore window-moved events for fullscreen windows — macOS fires them
  on entering Mission Control" (#347).
- v6.0.11 Mission Control cross-monitor adjustments (#2088); v2.4.0 "prevent
  space ops while mission-control is active or the display is animating" (#417).
- v6.0.1 rare crash processing window-destroyed (#1965).

**Rovr:** hit the Mission Control case (windows floated mid-animation). Rovr
suspends observation while a display animates. Destroy events are still a
lifetime trap.

---

## 5. Frame correction can loop (Hit)

- v3.1.1 "re-adjust window frame if it breaks the region from an event not
  invoked by the user" (#16); v3.1.2 **"Revert changes because they can trigger
  a loop, causing slow window move/resize"** (#16).
- v7.0.0 "managed windows should snap back when moved incorrectly" (#1199) and
  "correct their frame when modified by external means" (#2117).
- v7.0.4 "consecutive resizes broke because it used a **cached** window frame"
  (#2182).

**Rovr:** hit the identical retile loop (the `SetWindowFrame` churn). The
lesson Yabai's own revert encodes: correcting frames needs a bounded give-up,
and never compare against a cached frame.

---

## 6. Minimized / hidden windows (Hit)

- v6.0.9 "detect windows minimized before yabai launched" (#1833).
- v2.2.3 "ignore minimized windows when an app is unhidden (Chrome)" (#300);
  "don't add a minimized window to the tree when moved" (#382).

**Rovr:** `minimized == Unknown` is treated as tileable, because macOS returns
empty AX for background windows and the conservative reading left whole
displays untiled.

---

## 7. Apple bugs, per version and architecture (Exposed)

- `SLSGetRevealedMenuBarBounds` is **broken on Apple Silicon**; reconstruct from
  `SLSGetDisplayMenubarHeight` + `CGDisplayBounds` (source).
- Broken menubar dimensions on M1 (#793).
- Bad window levels when run as a service on Ventura (#1704).
- `window_opacity_duration` broken on Catalina / Big Sur (#277).
- `SPACE_CREATED` / `SPACE_DESTROYED` semantics changed in Sequoia 15.3 (#2548).
- A `@hack` 40 ms delay between two activation events because some apps confuse
  instantaneous events.
- `workspace_use_macos_space_workaround()` gates a whole space path by macOS
  version; the move-windows path needed SIP relaxed on Sequoia (#2324) and then
  worked with SIP again (#2788).

**Lesson:** gate on capability/behaviour, never on OS name, and expect the
private API to flip per release.

---

## 8. Lifetime, threading, security (Covered / Exposed)

- Use-after-free (#375); double-free on process termination (#543); NSRunningApplication
  lifetime (#543).
- Lock-free MT bug (#240); "undefined behaviour" x86 instruction ordering (#153).
- SIGPIPE on socket write, EBADF on read (#430).
- v7.1.18 "validate scripting-addition socket message length to prevent
  **stack corruption**" (#2751).
- v7.1.11 cached window-query values to fix responsiveness with slow/frozen
  apps (#2377).

**Rovr:** SIGPIPE ignored, SA frames length-checked, bounded channels. Every
FFI object is still a lifetime trap.

---

## 9. Spaces / display model (Exposed)

- v2.1.0 "prevent the last user-space from being destroyed or moved — macOS does
  not actually support this" (#182).
- v2.2.0 "exiting Mission Control invalidates the region assigned to Spaces /
  Views, because a Space may have been dragged to another monitor" (#118).
- v7.0.0 "space --swap swaps all windows rather than macOS spaces" (#549).

**Rovr:** persistent workspaces recreate missing spaces; the "last space cannot
be destroyed" rule and Mission Control re-mapping are live edge cases.

---

## 10. Rules semantics (Covered)

- v7.0.0: new rules apply only to windows opened **after** the rule is added;
  `rule --apply` is required for existing ones (#2123).

**Rovr:** re-evaluates rules every cycle, so it does not have this split — but
it is a reminder that "config changed" ≠ "open windows changed".

---

## The two that mattered most

1. **AX refs (§1)** — Rovr now uses Yabai's remote-token reconstruction, so it
   no longer depends on having seen a window frontmost first.
2. **Retile loop (§5)** — Yabai reverted its own frame-correction change for the
   same reason; Rovr must keep any correction bounded and never cache frames.

Everything else is a checklist to handle deliberately.
