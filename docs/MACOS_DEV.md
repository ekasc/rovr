# macOS development

## Prerequisites

- macOS on Apple Silicon or Intel
- Xcode Command Line Tools
- current stable Rust toolchain
- Accessibility permission for the built `rovrd` binary

The bootstrap bridge uses CoreGraphics for observation and Accessibility for
frame/focus mutation. It dynamically resolves `_AXUIElementGetWindow` because
CGWindowID-to-AX-window identity is not part of the public Accessibility API.
The scripting addition is not ported yet.

## Build

```sh
cargo build --workspace
cargo test --workspace
```

Run the daemon in a terminal first. This makes TCC failures and bridge errors
visible:

```sh
RUST_LOG=rovr=debug cargo run -p rovr-daemon --bin rovrd -- --foreground
```

In another terminal:

```sh
cargo run -p rovr-cli -- doctor
cargo run -p rovr-cli -- query displays
cargo run -p rovr-cli -- query windows
```

## Accessibility

If `doctor` reports observation but frame/focus operations fail, verify the
actual `rovrd` executable has Accessibility permission in System Settings.
During development the executable changes frequently, so TCC behavior can be
annoying. Do not misdiagnose a permission failure as a reconciler bug.

## Current bridge guarantees

Implemented:

- visible normal-window enumeration
- app/title/bundle identifier best-effort metadata
- display enumeration
- focused window best-effort detection
- set AX window position/size
- focus/activate AX window
- capability probing for the private CGWindowID -> AXUIElement resolver

Not implemented:

- Spaces
- Mission Control
- scripting addition
- private SkyLight operations
- AX lifecycle callbacks
- minimized/fullscreen state
- robust multi-display space identity

The daemon compensates for missing callbacks with periodic complete snapshots.
That polling path is intentional scaffolding and remains useful later as a
reconciliation safety net.

## Private API port rule

Do not call SkyLight directly from Rust command handlers. Add a narrow bridge
operation, probe its capability, implement `Platform::execute`, and then expose
it to core as a typed `Action`.
