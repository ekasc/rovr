# rovr

ROVR is a tiling window manager for macOS. A daemon observes your
displays, spaces, and windows, computes a tiled layout, and keeps
reality matched to it, through a typed Rust core that never trusts
cached state.

Status: experimental but daily-usable. Tiling, workspaces, rules,
in-process keybinds, live config reload, and SketchyBar sync all work.
See [`docs/ROADMAP.md`](docs/ROADMAP.md) for what's next.

## Quick start

Requires: macOS 13+, Xcode Command Line Tools, a Rust toolchain
(`cargo`).

```sh
git clone https://github.com/ekasc/rovr.git && cd rovr
./scripts/install.sh
rovr ping
rovr doctor
```

Then grant Accessibility permission: open **System Settings** >
**Privacy & Security** > **Accessibility** and enable `rovrd`.
Restart the daemon and open a few windows to tile. The full
walkthrough is in [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md).

Config lives at `~/.config/rovr/rovr.toml`
([example](config/rovr.example.toml),
[reference](docs/CONFIG_REFERENCE.md)). Save, then
`rovr config reload`. Saving alone changes nothing.

## Documentation

Using ROVR:

- [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md): install, concepts,
  configuration, keybinds, daily use, status bar, troubleshooting
- [`docs/CONFIG_REFERENCE.md`](docs/CONFIG_REFERENCE.md): every config
  option
- [`docs/STATUS_BAR.md`](docs/STATUS_BAR.md): SketchyBar and external
  status integrations

Scripting addition (optional, for space create/destroy/reorder and
window cosmetics):

- [`docs/SA_SIP.md`](docs/SA_SIP.md): what SIP relaxations it needs
  and why, install and status
- [`docs/SA.md`](docs/SA.md): protocol, privileged helper, verification

## Design goals

- Never assume cached state still equals macOS reality.
- Keep one authoritative state owner in the daemon.
- Express mutations as typed actions and verify them after execution.
- Keep `unsafe` and undocumented macOS APIs out of the core.
- Keep the command interface stable and scriptable without carrying
  forward legacy internal architecture.
- Make bugs diagnosable with `rovr doctor` and a bounded event flight
  recorder.

## Workspace

```text
crates/rovr-types          shared IDs, geometry, snapshots
crates/rovr-core           state machine, reducer, reconciler, flight recorder
crates/rovr-layout         pure BSP/stack/master/columns/monocle engine
crates/rovr-layout-plugin  WASM layout plugin host
crates/rovr-config         declarative TOML config + validation
crates/rovr-protocol       versioned typed IPC protocol
crates/rovr-platform       platform trait, mock backend, macOS bridge boundary
crates/rovr-daemon         single-owner daemon + Unix socket server
crates/rovr-cli            `rovr` CLI client
crates/rovr-sa-payload     scripting-addition payload dylib
crates/rovr-sa-loader      scripting-addition loader
crates/rovr-sa-helper      scripting-addition privileged helper tool
apps/rovr-menu-bar         Swift menu-bar companion app
```

Developer notes: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md),
[`docs/MACOS_DEV.md`](docs/MACOS_DEV.md).

## Security

Some capabilities need SIP partially reduced for the scripting
addition (see [`docs/SA_SIP.md`](docs/SA_SIP.md)). Don't weaken platform
security beyond what a specific capability needs. The privileged bridge
is a minimal capability provider, not general code execution.

## License

MIT. See [`NOTICE.md`](NOTICE.md) for project lineage.

---
ROVR takes inspiration from yabai.
