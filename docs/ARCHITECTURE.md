# Architecture

## Core loop

Rovr distinguishes what macOS **currently reports** from what the user and
layout engine **want**.

```text
macOS callbacks / polling
          |
          v
+-------------------+
| ObservedState     |
| generation-tagged |
+---------+---------+
          |               config / commands / rules
          |                         |
          |                         v
          |                +----------------+
          |                | DesiredState   |
          |                +-------+--------+
          |                        |
          +-----------+------------+
                      v
                +-----------+
                | Reconciler|
                +-----+-----+
                      |
                  Vec<Action>
                      |
                      v
                +-----------+
                | Platform  |
                | executor  |
                +-----+-----+
                      |
                      v
                    macOS
```

An action request is not an observation. After mutating macOS, the platform
must eventually observe the resulting state. If the observation does not match
the desired state, reconciliation can retry, adapt, or surface a typed error.

## Generations

Sleep/wake, Dock restarts, display topology changes, and other discontinuities
bump the observation generation. Cached objects from an older generation are
stale and cannot be used to conclude that a geometry-sensitive operation is
complete.

The first response to a discontinuity is `Action::RefreshAll`, not speculative
layout mutation.

## State ownership

The daemon's state loop owns `Engine`. IPC threads and platform callbacks submit
messages over channels. This avoids distributed mutable state and makes event
ordering explicit.

## Platform boundary

`rovr-platform::Platform` is the only interface the core needs from macOS.
Private APIs belong behind that trait.

The intended macOS implementation has three layers:

1. Rust safe wrapper implementing `Platform`.
2. Narrow C ABI bridge.
3. Objective-C/C implementation for AX, SkyLight, Dock, Mission Control and the
   scripting addition.

The scripting addition should expose capabilities, not policy.

## IPC

The protocol is newline-delimited JSON over a Unix domain socket for the
bootstrap. Requests and responses carry an explicit protocol version and
request ID. This is deliberately boring and inspectable.

A future binary framing format can be introduced as protocol v2 without
changing the internal command model.

## Flight recorder

The daemon retains a bounded ring of significant state transitions and actions.
It should be possible to reconstruct the few seconds preceding most user-visible
failures without enabling verbose logging ahead of time.
