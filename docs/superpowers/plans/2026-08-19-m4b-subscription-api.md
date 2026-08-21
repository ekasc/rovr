# M4b — Stable subscription API

**Goal:** Let clients subscribe to the daemon and receive a streaming feed of
notifications (state changes, layout changes, scratchpad toggles, config
reloads). This is the second slice of M4 "ecosystem" and the architectural
keystone for the rest of M4 (menu-bar UI, external scripts all consume it). It
extends the existing Unix-socket IPC from strictly request/response to
request/response **plus** long-lived subscription streams, without disturbing
the command path.

**Why now:** M4a shipped completions. The roadmap lists "stable subscription
API" first among M4's ecosystem items; observable state is a core product goal
(PRODUCT.md: "observable", "event subscriptions"). Completions are pure CLI;
this is the first daemon-visible ecosystem feature.

**Design**
- **Protocol (`rovr-protocol`):** add `Command::Subscribe` (subscribe-all; v1
  has no filter — filters are a later, additive change). Add a stable
  `Notification` enum, the broadcast contract:
  - `Hello { protocol_version: u16 }` — sent on subscribe.
  - `StateChanged { generation: u64 }` — a reconcile/command cycle mutated
    observed state; clients re-query `rovr query state`.
  - `LayoutChanged { space: SpaceId, horizontal: bool, reversed: bool }` — a
    BSP orientation change (axis/reversed as primitives, no core-type dep).
  - `ScratchpadToggled { name: String, open: bool }`.
  - `ConfigReloaded`.
  `Notification` carries only `rovr-types` (`SpaceId`) + primitives, so
  `rovr-protocol` stays independent of `rovr-core`. `serde(tag = "type")` makes
  the wire format stable and self-describing. `Response` is unchanged — the
  subscribe ack is just `Response::ok(id, {"subscribed": true})`.
- **Daemon:** introduce a subscriber registry `Arc<Mutex<Vec<UnixStream>>>`
  shared by the socket-accept loop and `state_loop`.
  - `handle_client` intercepts `Command::Subscribe` *before* the one-shot
    envelope path: writes the ack, registers a `stream.try_clone()` in the
    registry, broadcasts `Hello`, then loops reading the connection to detect
    client disconnect (EOF/error) and returns. The clone lives in the registry
    until `broadcast_notification` prunes it on a failed write.
  - `state_loop` gains the registry. After `daemon.handle(req)` it broadcasts a
    command-specific `Notification` (layout/scratchpad/config) or
    `StateChanged` (everything else); on `recv_timeout` (refresh) it broadcasts
    `StateChanged`.
  - `broadcast_notification` locks the registry, writes `serde_json` + `"\n"` to
    each subscriber, flushes, and prunes subscribers whose write errored
    (dead connections).
  - `handle` gains a `Command::Subscribe` arm returning an informative error for
    the (unused) one-shot path, keeping the match exhaustive.
- **CLI:** add `TopCommand::Subscribe`. `main` routes it (before the socket
  send path) to `run_subscribe`, which connects, sends `Command::Subscribe`,
  then prints each incoming notification line until EOF. `map_command` gets an
  `unreachable!` arm (subscribe handled in `main` before `map_command`).

**Plan (foolproofed)**
- `rovr-protocol/src/lib.rs`: `Command::Subscribe`; `Notification` enum;
  `#[cfg(test)]` round-trip test.
- `rovr-daemon/src/main.rs`: `Arc/Mutex` import; `Notification` import;
  `Axis` import (`rovr_core::layout_state`); `subscribers` registry in
  `run_socket_server`; `handle_client(stream, tx, subscribers)` subscribe
  branch; `state_loop(daemon, rx, subscribers)` broadcasts;
  `broadcast_notification` + `broadcast_for_command` helpers; `handle`
  `Command::Subscribe` arm; `#[cfg(test)]` `m4b_broadcast_writes_...` via
  `UnixStream::pair()`.
- `rovr-cli/src/main.rs`: `TopCommand::Subscribe`; early `run_subscribe` path;
  `run_subscribe` helper; `map_command` `unreachable!` arm.
- `docs/ROADMAP.md`: mark `shell completions` (M4a) and `stable subscription
  API` (M4b) done.

**Foolproofing (risks)**
- R1: `UnixStream` is `Clone` (dup fd); the registry holds clones for writing
  while `handle_client` keeps the original for disconnect detection. No double
  ownership of one fd's read/write direction.
- R2: subscriber writes happen under a short `Mutex` hold; small-scale (M4),
  acceptable. Failed writes prune the subscriber so a dead client can't wedge
  the loop.
- R3: `handle_client` for subscribe must NOT send an `Envelope` to `state_loop`
  (that would close the connection). It returns after the disconnect loop.
- R4: `state_loop` clones `envelope.request.command` to drive
  `broadcast_for_command` without moving the request out of `envelope`.
- R5: `Engine.layouts`/`scratchpads` are `pub`; daemon reads orientation via
  `orientation.axis == Axis::Horizontal` and `scratchpads.is_open(name)`. No
  new engine API needed.
- R6: protocol `Notification` avoids core types (uses `horizontal: bool,
  reversed: bool`) so `rovr-protocol` has no new crate dependency.
- R7: `Command::Subscribe` added to `handle`'s match keeps it exhaustive; the
  CLI never reaches it (streaming path), so it only guards the one-shot path.

**Verification:** `cargo clippy --workspace --all-targets -- -D warnings` clean;
`cargo test -p rovr-protocol` (Notification round-trip) green;
`cargo test -p rovr-daemon` (`m4b_broadcast_writes_notification_to_subscribers`
via `UnixStream::pair()`) green; `cargo test -p rovr-core` still 21. No live
daemon run (yabai active → clash).

**Deferred:** per-event filters / selective subscription, backpressure,
subscriber auth, replay of last state on subscribe, typed window/space change
events (full diffs), WASM plugin events.
