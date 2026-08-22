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
  It also reports the full lifecycle picture (`-- lifecycle --` section): privileged **service** state (`not_installed`, `installed`, `registered`; `awaiting_approval` is reserved for future SMAppService flows), the installed payload sha256 vs the install-time marker, and whether the **injected** payload differs from the **installed** one (replacing the dylib on disk does NOT update code already mapped into Dock — status flags that mismatch explicitly).
- `rovr sa install` — full install lifecycle:
  1. locates the built artifacts (`cargo build -p rovr-sa-payload -p rovr-sa-loader -p rovr-sa-helper`; env overrides `ROVR_SA_PAYLOAD`/`ROVR_SA_LOADER`/`ROVR_SA_HELPER`)
  2. verifies root + SIP scope (see `docs/SA_SIP.md`)
  3. copies payload dylib (0644), loader (0744) and helper (0744) into root-owned `/Library/Application Support/rovr/`
  4. registers the privileged LaunchDaemon (`com.rovr.sa-helper`) via `launchctl bootstrap system`
  5. triggers the initial injection by running the loader directly (root)
  6. polls the socket for a verified handshake as the console user
  7. writes `payload.installed.json` (installed/injected sha256 + handshake version) and reports actual capability bits

  Failure at any stage is reported honestly; success is only claimed after a verified handshake, never merely because files were copied.
- `rovr sa uninstall` — unregisters the LaunchDaemon first (guaranteeing no further reinjection), removes the plist, helper, loader, payload, identity marker, stale SA socket, then restarts Dock (required: code already mapped into Dock cannot be evicted any other way). Nothing privileged is left behind.

## Automatic reinjection

The Rovr scripting addition lives INSIDE Dock and dies whenever Dock dies or the machine reboots. Rovr restores it automatically without sudoers:

```
normal rovr daemon
    |  observes: Dock PID change / SA socket gone / handshake dead
    v
bounded reinjection state machine   (crates/rovr-platform/src/macos/reinject.rs)
    |  ONE request per attempt, single-flight, backoff 5s→15s→45s,
    |  max 4 attempts per Dock generation, quiet until Dock changes again
    v
privileged helper (root LaunchDaemon, socket-activated)
    |  inject() ONLY: authenticates peer, resolves Dock ITSELF,
    |  validates fixed root-owned artifacts, runs fixed loader on fixed payload
    v
daemon re-probes handshake within a bounded window → capabilities refresh
```

Division of responsibility:

- **Unprivileged daemon**: Dock lifecycle detection, SA health probing, retry policy, verification, diagnostics. All failure paths degrade to non-SA operation — every other Rovr capability keeps working if injection fails.
- **Privileged helper** (`crates/rovr-sa-helper`): nothing but `inject()` and `status()`. No polling, no timers, no window-management policy, no config, no arbitrary targets.

### Privileged helper security model

- The request frame is exactly `{magic, proto, opcode, uid}` — there is structurally NO field for a pid, path, command or environment (unit-tested).
- Payload/loader/helper paths are compile-time constants pointing at the root-owned installed artifacts; the helper resolves Dock itself via `NSRunningApplication` and never trusts a caller-supplied PID.
- Callers are authenticated with `getpeereid()`: kernel peer uid must equal BOTH the uid in the request AND the owner of `/dev/console` (the GUI session user). Sockets are UID-specific (`/tmp/rovr-sa_<uid>.sock`), so an injection is always bound to the requesting console session; other users' processes are refused.
- Artifacts are re-validated before every use: regular files only (`lstat` + `O_NOFOLLOW` — symlinks refused), root-owned, expected modes, install directory not group/other-writable.
- The helper runs the loader via `posix_spawn` with a FIXED argv and a fixed minimal environment — nothing inherited from the request or daemon.
- Event-driven only: launchd starts the helper when a client connects; no root process polls anything.

### Service management — why not SMAppService

Apple's current API, `SMAppService.daemon(plistName:)` (macOS 13+), registers a LaunchDaemon whose executable must be contained in the calling app bundle's `Contents/Library/LaunchDaemons`. Rovr is distributed as cargo-built CLI binaries with no `.app` bundle, so SMAppService is technically unusable without repackaging the entire distribution model. Rather than shipping a pseudo-SMAppService, Rovr uses the explicit minimal fallback documented here: a root-owned `/Library/LaunchDaemons/com.rovr.sa-helper.plist` registered once via `launchctl bootstrap` by `sudo rovr sa install`. This is standard launchd registration performed by an explicit root command — it requires NO sudoers modification, NOPASSWD or otherwise, and creates no generic root-execution hole: the service accepts exactly two fixed-frame requests and can only ever run the fixed loader against the fixed payload. If Rovr later ships inside an app bundle, migrating to SMAppService is a packaging change, not a protocol change.

## Doctor integration

- `rovr doctor` includes `sa: { socket, present, version, compatible, attribs, expected_prefix }` plus `snapshot_wedged_ms` for observation-worker health. Incompatible or missing payload is reported cleanly, not silently degraded.
- It also includes `sa_reinject`: the daemon's live reinjection lifecycle — `phase` (healthy/injecting/verifying/failed), `generation` and `dock_pid` (every attempt is keyed to the Dock generation), `attempts_this_generation`, `retry_in_secs` (active backoff), `pending`, `last_result`, `last_error` and the fixed `helper_socket`. No secrets or privileged internals are exposed.

## Update behavior

Replacing the installed payload dylib does NOT update code already mapped into Dock. `rovr sa status` detects this: install writes a `payload.installed.json` marker recording the installed sha256 AND the sha256 that was actually injected; if the current file hash differs from the injected hash, status reports `injection: STALE` with the remediation (`sudo rovr sa install`). Dock is never killed automatically during an update — reinjection into the running Dock happens only via the explicit install command or the daemon's bounded automatic path (which injects the currently installed build into a NEW Dock).

## Versioning

- Payload/client protocol is versioned via `ROVR_SA_VERSION_PREFIX = "rovr-sa-1."`. The client rejects mismatched majors; minors remain wire-compatible. Bumping the major requires coordinated payload + client release. `rovr sa status` and `rovr doctor` surface the mismatch for remediation.

## Verification status

The payload, loader, helper and install lifecycle compile and the protocol client + reinjection state machine are unit-tested, but injection requires SIP relaxation (recovery-mode reboot) and has NOT been verified end-to-end on a live macOS yet. Automatic reinjection (Dock restart AND reboot recovery) is NOT marked done: both must be demonstrated in a real interactive session per docs/ROADMAP.md before claiming `[x]`. Until then, SA-gated capabilities remain `[~] implemented but not verified end-to-end` in `docs/ROADMAP.md`.
