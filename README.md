# rovr

**Rovr is an experimental macOS window-management daemon built around observed
state, desired state, and reconciliation.**

It is intended as a next-generation architectural fork of yabai: keep the hard
won macOS integration knowledge, replace the increasingly coupled control plane
with a typed Rust core, and make recovery from stale OS state a first-class
behavior.

> Status: bootstrap. The Rust core, protocol, config model, daemon, mock
> platform, diagnostics, event flight recorder, and public-API macOS bridge are
> laid out. Private SkyLight/scripting-addition capabilities are deliberately
> behind the platform boundary and are the next major porting target.

## Design goals

- Never assume cached state still equals macOS reality.
- Keep one authoritative state owner in the daemon.
- Express mutations as typed actions and verify them after execution.
- Keep `unsafe` and undocumented macOS APIs out of the core.
- Preserve a path to yabai command compatibility without preserving yabai's
  internal architecture.
- Make bugs diagnosable with `rovr doctor` and a bounded event flight recorder.

## Workspace

```text
crates/rovr-types      shared IDs, geometry, snapshots
crates/rovr-core       state machine, reducer, reconciler, flight recorder
crates/rovr-layout     pure BSP/stack/master/columns/monocle layout engine
crates/rovr-config     declarative TOML config + validation
crates/rovr-protocol   versioned typed IPC protocol
crates/rovr-platform   platform trait, mock backend, macOS bridge boundary
crates/rovr-daemon     single-owner daemon + Unix socket server
crates/rovr-cli        `rovr` CLI client
```

## Example

Start the daemon:

```sh
cargo run -p rovr-daemon --bin rovrd -- --foreground
```

Then:

```sh
cargo run -p rovr-cli -- ping
cargo run -p rovr-cli -- doctor
cargo run -p rovr-cli -- query windows
cargo run -p rovr-cli -- debug events
```

On non-macOS systems the daemon uses the mock platform. This is intentional: it
lets the core and protocol be tested without WindowServer.

## Native configuration

Default path:

```text
~/.config/rovr/rovr.toml
```

Example: see [`config/rovr.example.toml`](config/rovr.example.toml).

## Architecture

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) and
[`docs/ROADMAP.md`](docs/ROADMAP.md).

## Security

Rovr's eventual private macOS integration may require the same reduced SIP
configuration used by yabai's scripting addition. Do not disable more platform
security than a specific capability requires. The daemon should treat the
privileged bridge as a minimal capability provider, not as general code
execution.

## License

MIT. See [`NOTICE.md`](NOTICE.md) for project lineage.
