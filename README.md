# rovr

Rovr is an independent macOS window-management daemon written in Rust,
built around observed state, desired state, and reconciliation.

> Status: bootstrap. The Rust core, protocol, config model, daemon, mock
> platform, diagnostics, event flight recorder, and public-API macOS bridge are
> laid out. The scripting-addition payload, loader, and privileged helper have
> landed in bootstrap form behind the platform boundary. Private SkyLight
> capabilities remain the active porting work.

## Design goals

- Never assume cached state still equals macOS reality.
- Keep one authoritative state owner in the daemon.
- Express mutations as typed actions and verify them after execution.
- Keep `unsafe` and undocumented macOS APIs out of the core.
- Make bugs diagnosable with `rovr doctor` and a bounded event flight recorder.

## Workspace

```text
crates/rovr-types         shared IDs, geometry, snapshots
crates/rovr-core          state machine, reducer, reconciler, flight recorder
crates/rovr-layout        pure BSP/stack/master/columns/monocle layout engine
crates/rovr-layout-plugin WASM layout plugin protocol, host trait and guest ABI
crates/rovr-config        declarative TOML config + validation
crates/rovr-protocol      versioned typed IPC protocol
crates/rovr-platform      platform trait, mock backend, macOS bridge boundary
crates/rovr-daemon        single-owner daemon + Unix socket server
crates/rovr-cli           `rovr` CLI client
crates/rovr-sa-payload    scripting-addition payload, Dock-injected dylib
crates/rovr-sa-loader     scripting-addition loader, root helper injecting payload into Dock
crates/rovr-sa-helper     privileged SA helper, launchd service injecting payload on request
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

`rovr doctor` prints a grouped, human-readable diagnostics report — daemon
counts, config, health, platform capabilities, and a summary of anything that
needs attention. Add `--json` for the raw payload, which is what scripts and
the GUI/menu bar consume.

On non-macOS systems the daemon uses the mock platform. This is intentional: it
lets the core and protocol be tested without WindowServer.

## Native configuration

Default path:

```text
~/.config/rovr/rovr.toml
```

Example: see [`config/rovr.example.toml`](config/rovr.example.toml).

New configurations should declare `config-version = 1`. Omitting the key is
treated as version 1 for compatibility; unsupported versions fail validation
before any configuration is applied.

Use `rovr config dump` for the minimal starter file or
`rovr config dump --full` to inspect every resolved default.

## Architecture

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) and
[`docs/ROADMAP.md`](docs/ROADMAP.md).

## Security

Rovr's eventual private macOS integration may require a reduced SIP
configuration for its scripting addition. Do not disable more platform
security than a specific capability requires. The daemon should treat the
privileged bridge as a minimal capability provider, not as general code
execution.

## License

MIT. See [`NOTICE.md`](NOTICE.md).
