# Rovr Scripting Addition

Rovr ships its own privileged payload. It is **not** yabai's payload. Rovr never connects to `/tmp/yabai-sa_*.socket`.

## Socket namespace

- Rovr daemon and CLI use `/tmp/rovr-sa_<uid>.sock` (UID via `getuid()`, not `$USER`). The daemon probes this socket at startup; CLI `rovr sa status` probes it directly without the daemon.

## Protocol

- Binary framing identical to yabai's `sa.m` / `osax/common.h` (MIT © 2019 Åsmund Vikane) but with Rovr version prefix:
  - request: `[i16 LE len][u8 opcode][payload]` where `len = 3 + payload.len()` (the reader consumes `len - 2` bytes after the prefix; upstream-compatible)
  - handshake `0x01`: response `version\x00 + u32 LE attribs` — version must be `rovr-sa-1.*` (e.g. `rovr-sa-1.0`). A `yabai-sa-*` response is rejected as incompatible.
  - ops: packed LE payloads per opcode. Payload closes connection after processing; EOF is the timeout-bounded ACK.
- Opcodes implemented by the Rovr payload: `0x01 handshake`, `0x02 focus_space`, `0x03 create_space`, `0x04 destroy_space`, `0x05 move_space`, `0x07 opacity`, `0x08 opacity_fade`, `0x09 layer`, `0x0A sticky`, `0x0B shadow`, `0x0D scale`.
- Capability bits in handshake attribs report what ACTUALLY resolved inside this Dock process: `0x04 add_space`, `0x08 rem_space`, `0x10 mov_space`. The window-cosmetic ops (layer/sticky/shadow/opacity/scale) are pure SkyLight and are available whenever the payload is live.
- Every IPC carries a hard 2 s deadline (`SA_DEADLINE`); no unbounded wait.

## Payload responsibilities (strictly primitive)

- Inject into Dock, listen on the Rovr socket, execute SkyLight / private-API primitives and close.
- **Must not** contain layout policy, config parsing, or desired-state ownership.
- **Must not** decide tiling, workspaces, rules, or persistence — the Rust daemon owns that.

## Payload implementation

- Crate: `crates/rovr-sa-payload` (Objective-C, built by `build.rs` with clang into `librovr_sa_payload.dylib`).
- Space focus/create/destroy/move require Dock-internal functions that have no public or SkyLight-level equivalent. The payload resolves them at runtime by byte-pattern scanning Dock's own binary, with per-macOS-version offset/pattern tables vendored from yabai (`vendor/arm64_payload.m`, `vendor/x64_payload.m`, MIT © Åsmund Vikane — attribution preserved per `docs/YABAI_PORT.md`). If a pattern does not resolve on a given macOS build, the corresponding capability bit is simply absent from the handshake — reported honestly, never faked.
- Opacity/fade, layer, sticky, shadow and PiP/scale are direct SkyLight calls (`SLSSetWindowAlpha`, `SLSSetWindowSubLevel`, `SLSSetWindowTags`/`SLSClearWindowTags`, `SLSGet/SetWindowTransform`) and do not depend on Dock internals.
- The opacity fade uses a single-slot thread (one animated fade at a time); concurrent fades to other windows degrade to an immediate alpha set rather than piling up threads.

## Loader

- Crate: `crates/rovr-sa-loader` builds `rovr-sa-loader`, a root-only helper adapted from yabai's `osax/loader.m` (MIT © Åsmund Vikane; arm64e path based on Jeremy Legendre's work). It locates Dock, allocates a remote stack/code segment, and dlopens the payload inside Dock via a remote thread.
- The loader takes the payload dylib path as `argv[1]`; there is no hardcoded install path in the binary.

## Install lifecycle

- `rovr sa status` — probes the socket and reports one of five states:
  - `not_installed` — no socket, no installed files
  - `installed_not_injected` — files present under `/Library/Application Support/rovr/`, but Dock is not running the payload
  - `injected_compatible` — handshake answers with a `rovr-sa-1.*` version and all space-capability bits set
  - `incompatible_protocol` — something answers on the socket with a foreign/outdated version
  - `capability_missing` — compatible payload but some space-capability bits absent on this macOS build
- `rovr sa install` — locates the built artifacts (`cargo build -p rovr-sa-payload -p rovr-sa-loader`; env overrides `ROVR_SA_PAYLOAD`/`ROVR_SA_LOADER`), verifies root + SIP scope (see `docs/SA_SIP.md`), copies them to `/Library/Application Support/rovr/`, runs the loader to inject into Dock, then polls the socket for a valid handshake as verification. Non-root invocations print the exact command to run instead of attempting anything.
- `rovr sa uninstall` — removes the installed files and restarts Dock so the payload unloads.

## Doctor integration

- `rovr doctor` includes `sa: { socket, present, version, compatible, attribs, expected_prefix }` plus `snapshot_wedged_ms` for observation-worker health. Incompatible or missing payload is reported cleanly, not silently degraded.

## Versioning

- Payload/client protocol is versioned via `ROVR_SA_VERSION_PREFIX = "rovr-sa-1."`. The client rejects mismatched majors; minors remain wire-compatible. Bumping the major requires coordinated payload + client release. `rovr sa status` and `rovr doctor` surface the mismatch for remediation.

## Verification status

The payload, loader and install lifecycle compile and the protocol client is unit-tested, but injection requires SIP relaxation (recovery-mode reboot) and has NOT been verified end-to-end on a live macOS yet. Until then, SA-gated capabilities remain `[~] implemented but not verified end-to-end` in `docs/ROADMAP.md`.
