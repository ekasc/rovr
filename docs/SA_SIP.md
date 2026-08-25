# SIP / Security — What Rovr's SA Needs and Why

> Rovr does not silently weaken system security. This document states exactly what is required and why, scoped to implemented capabilities. Do not require broader SIP reduction than the payload actually needs.

## Summary

- **No SIP change** is required to run Rovr without the SA: window enumeration, display/space observation, `move window to space` (via `SLSPerformAsynchronousBridgedWindowManagementOperation` / compat workaround), `focus space` via gesture synthesis, `set frame` / `focus window` via Accessibility, tiling, rules, layouts, subscriptions all work on stock macOS.
- **SA-gated capabilities** (create/destroy/reorder Space, layer, sticky, shadow, opacity, PiP/scale) require code injected into Dock. Injecting into Dock is gated by SIP. The payload requests only the relaxations that actually enable those primitive operations.

## Exact relaxations required

Injection uses `task_for_pid` against Dock plus a remote-thread `dlopen` of the payload dylib. That requires:

1. **Debugging restrictions disabled** — permits `task_for_pid` against system processes:
   - `csrutil enable --without debug` (recovery mode)
2. **Filesystem protections disabled** — the loader binary and payload dylib live under `/Library/Application Support/rovr/`, and task ports for platform binaries are additionally gated on filesystem policy:
   - `csrutil enable --without fs` (recovery mode)
3. **Apple Silicon only**: `-arm64e_preview_abi` boot-arg — arm64e PAC requires signing injected code with the preview ABI:
   - `sudo nvram boot-args="-arm64e_preview_abi"` (then reboot)

Combined recovery-mode command: `csrutil enable --without debug --without fs`.

Rovr will **not** ask for `csrutil disable` (full SIP off). `rovr sa install` checks `csrutil status` for exactly these two flags and refuses to attempt injection (with remediation instructions) when they are absent.

## Why not weaker?

- The SA exists solely to expose primitive SkyLight / window-level ops that have no public API and require Dock context. The Rust daemon never runs with Dock privileges itself; it talks to Dock only over the private Unix socket `/tmp/rovr-<uid>/sa.sock` in a user-owned `0700` runtime directory, with peer-credential checks and a 2 s deadline and versioned handshake. The payload has no layout policy, no config, no desired-state — compromise of the SA cannot directly reconfigure tiling policy.
- The installed loader is mode 744 (root-only execute), the dylib 644; both live in a root-owned directory. Normal Rovr operation never escalates.

## Operationally

- `rovr sa status` reports one of five states (`not_installed`, `installed_not_injected`, `injected_compatible`, `incompatible_protocol`, `capability_missing`) so a broken install is explicit, not silent.
- When injection is missing or incompatible, `rovr doctor` marks `capabilities.{create_space,destroy_space,reorder_space,layer,sticky,shadow,opacity,scale} = false` while `sa.present = true` preserves the raw incompatible version as evidence — no silent fallback to a yabai payload (different socket namespace).
- `rovr sa install` / `rovr sa uninstall` are the only operations that touch privileged state.

## Privileged helper — no sudoers, ever

Automatic SA reinjection uses a narrowly scoped root LaunchDaemon (`com.rovr.sa-helper`, socket-activated at `/var/run/rovr-sa-helper.sock`). There is NO sudoers rule — NOPASSWD or otherwise — and no generic root-execution path anywhere in Rovr:

- The request frame carries only `{magic, proto, opcode, uid}`; it cannot name a pid, dylib, command or environment variable.
- The helper resolves Dock itself (`NSRunningApplication`, bundle id `com.apple.dock`) and validates the fixed root-owned loader/payload (regular files, no symlinks, root-owned, exact modes) before every use.
- Peer credentials (`getpeereid`) must match the requesting uid AND the `/dev/console` owner, tying every injection to the requesting GUI session.
- A random local process cannot make the helper inject arbitrary code into arbitrary processes — the only executable it can ever run is the installed `rovr-sa-loader` against the installed `librovr_sa_payload.dylib`.

Full model in `docs/SA.md` ("Privileged helper security model").

## Verification status

The SIP check logic is implemented in `rovr sa install`, and the injection flow **was exercised end-to-end on 2026-08-24** on macOS 26.5 with exactly the relaxations above (`--without debug --without fs` + `-arm64e_preview_abi`). Injection into the live Dock, all space mutations, and cosmetics were verified (see `docs/SA.md`). Two build-level requirements were discovered during verification: the payload AND loader must be built `-arch arm64e` to match Dock, and the loader's Mach-O caps byte must be patched from ABI v1 (`0x81`, modern clang default) to v0 (`0x80`, what Dock runs) — both handled in the crates' `build.rs`. Reboot recovery remains undemonstrated.
