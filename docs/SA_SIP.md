# SIP / Security — What Rovr's SA Needs and Why

> Rovr does not silently weaken system security. This document states exactly what is required and why, scoped to implemented capabilities. Do not require broader SIP reduction than the payload actually needs.

## Summary

- **No SIP change** is required to run Rovr without the SA: window enumeration, display/space observation, `move window to space` (via `SLSPerformAsynchronousBridgedWindowManagementOperation` / compat workaround), `focus space` via gesture synthesis, `set frame` / `focus window` via Accessibility, tiling, rules, layouts, subscriptions all work on stock macOS.
- **SA-gated capabilities** (create/destroy/reorder Space, layer, sticky, shadow, opacity, PiP/scale) require code injected into Dock. Injecting into Dock is gated by SIP. The payload will only request the minimum relaxation that actually enables the primitive operations Rovr uses.

## What injection needs

- The payload is a small dylib injected into Dock. On macOS with SIP enabled, injection via `task_for_pid` / `DYLD_INSERT_LIBRARIES` / scripting-addition loading is blocked. The standard ways to permit it are:
  - `csrutil enable --without debug` (also sometimes described as `--without fs` / `filesystem` depending on macOS vintage) — permits debugging / task-for-pid against system processes like Dock.
  - In some configurations, installing the SA to a SIP-protected location additionally requires `--without fs` if filesystem protection would block the install path. Rovr will choose the minimal privileged install location that works with `--without debug` alone where possible.

Rovr will **not** ask for `csrutil disable` (full SIP off). The install flow will document which `csrutil` flag(s) the current macOS build actually requires for the operations listed above, and will refuse broader relaxations.

## Why not weaker?

- The SA exists solely to expose primitive SkyLight / window-level ops that have no public API and require Dock context. The Rust daemon never runs with Dock privileges itself; it talks to Dock only over the private Unix socket `/tmp/rovr-sa_<uid>.sock` with a 2 s deadline and versioned handshake. The payload has no layout policy, no config, no desired-state — compromise of the SA cannot directly reconfigure tiling policy.

## Operationally

- `rovr sa status` reports `socket` / `present` / `version` / `compatible` / `attribs`. When injection is missing or incompatible, `rovr doctor` marks `capabilities.{create_space,destroy_space,reorder_space,layer,sticky,shadow,opacity,scale} = false` and `sa.present = false` with the expected version prefix — no silent fallback to a yabai payload (different socket namespace), so the failure mode is explicit.
- `rovr sa install` / `rovr sa uninstall` will be the only operations that touch privileged state; normal `rovr` operation never escalates.

## Next

- When the `rovr-sa-payload` crate ships, update this doc with the exact `csrutil` invocation(s) verified on each supported macOS minor, the privileged install path, and the `launchctl` / injection command. Keep the claim minimal and tested — do not copy a broader yabai instruction set without verification.
