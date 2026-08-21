# Rovr Scripting Addition

Rovr ships its own privileged payload. It is **not** yabai's payload. Rovr never connects to `/tmp/yabai-sa_*.socket`.

## Socket namespace

- Rovr daemon and CLI use `/tmp/rovr-sa_<uid>.sock` (UID, not `$USER`; falls back to `$USER` only if `UID` unset). The daemon probes this socket at startup; CLI `rovr sa status` probes it directly without the daemon.

## Protocol

- Binary framing identical to yabai's `sa.m` / `osax/common.h` (MIT © 2019 Åsmund Vikane) but with Rovr version prefix:
  - request: `[i16 LE len][u8 opcode][payload]` where `len = 1 + payload.len()`
  - handshake `0x01`: response `version\x00 + u32 LE attribs` — version must be `rovr-sa-1.*` (e.g. `rovr-sa-1.0`). A `yabai-sa-*` response is rejected as incompatible.
  - ops: packed LE payloads per opcode (focus/create/destroy/move space, opacity/layer/sticky/shadow/scale). Payload closes connection after processing; EOF is the timeout-bounded ACK.
- Opcodes: `0x01 handshake`, `0x02 focus_space`, `0x03 create_space`, `0x04 destroy_space`, `0x05 move_space`, `0x07 opacity`, `0x08 opacity_fade`, `0x09 layer`, `0x0A sticky`, `0x0B shadow`, `0x0D scale`.
- Capability bits in handshake attribs: `0x04 add_space`, `0x08 rem_space`, `0x10 mov_space`. The remaining caps (layer/sticky/shadow/opacity/scale) are present when the handshake succeeds.
- Every IPC carries a hard 2 s deadline (`SA_DEADLINE`); no unbounded wait.

## Payload responsibilities (strictly primitive)

- Inject into Dock, listen on the Rovr socket, execute SkyLight / private-API primitives and close.
- **Must not** contain layout policy, config parsing, or desired-state ownership.
- **Must not** decide tiling, workspaces, rules, or persistence — the Rust daemon owns that.

## Install lifecycle

- `rovr sa status` — probe socket, report `socket` / `present` / `version` / `compatible` / `attribs` / per-cap, plus daemon's `doctor.sa` view when the daemon is reachable.
- `rovr sa install` — (payload not yet bundled in this slice) will install the Rovr payload to a privileged location and inject into Dock (see `docs/SA_SIP.md` for SIP scope). Currently reports that the payload is not yet bundled and exits non-zero.
- `rovr sa uninstall` — remove the Rovr payload and restore Dock (no-op in this slice).

## Doctor integration

- `rovr doctor` now includes `sa: { socket, present, version, compatible, attribs, expected_prefix }` alongside `capabilities`. Per-capability presence is derived from attribs + handshake success. Incompatible or missing payload is reported cleanly, not silently degraded.

## Versioning

- Payload/client protocol is versioned via `ROVR_SA_VERSION_PREFIX = "rovr-sa-1."`. The client rejects mismatched majors; minors remain wire-compatible. Bumping the major requires coordinated payload + client release. `rovr sa status` and `rovr doctor` surface the mismatch for remediation.

## Next

- Build `crates/rovr-sa-payload` (C/ObjC) behind `rovr-platform`, wire `rovr sa install|uninstall` to privileged helper / `launchctl` / Dock injection, add attestation in `rovr doctor` and flight recorder, gated integration tests for each SA op with `observe→execute→observe→verify` and timeout/compat cases.
