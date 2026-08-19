# Yabai port plan

Baseline inspected for the bootstrap: `asmvik/yabai` master at
`dd845723416f5fe92af49fad5ebab00369e07edd` (June 14, 2026).

Rovr should port **capabilities**, not copy the existing daemon architecture.

## Keep as reference

These upstream areas contain the hard-won platform knowledge:

- `src/sa.m` / `src/sa.h`: scripting-addition installation, handshake and
  daemon-side transport.
- `src/osax/common.h`: scripting-addition protocol constants.
- `src/osax/loader.m`: scripting-addition loader.
- `src/osax/payload.m`: shared payload behavior.
- `src/osax/arm64_payload.m`: Apple Silicon private implementation details.
- `src/osax/x64_payload.m`: Intel implementation details.
- `src/workspace.m`: macOS version/workspace integration.
- `src/mission_control.c`: Mission Control integration.
- `src/window.c`, `src/space.c`, `src/display.c`: platform observations.

Do not port `message.c` or `event_loop.c` wholesale. Those are precisely the
boundaries Rovr is replacing.

## Port order

### 1. Transport only

Reproduce the scripting-addition handshake in a dedicated macOS bridge module.
The Rust side should see something like:

```rust
trait ScriptingAddition {
    fn capabilities(&self) -> SaCapabilities;
    fn move_window_to_space(&mut self, window: WindowId, space: SpaceId) -> Result<()>;
    fn create_space(&mut self, display: DisplayId) -> Result<SpaceId>;
    fn destroy_space(&mut self, space: SpaceId) -> Result<()>;
    fn focus_space(&mut self, space: SpaceId) -> Result<()>;
}
```

The payload must not know about layouts, rules, named workspaces, or Rovr
configuration.

### 2. Capability probing

Do not expose `is_tahoe()` style conditionals to core. Probe once at startup and
return explicit capabilities. OS version checks are allowed inside the private
bridge when they are necessary to choose an implementation.

### 3. Window -> Space

Port the smallest private feature that gives an immediate user-visible gain:
reliable movement of a window between Spaces. Add a macOS integration test and
a flight-recorder event for request, completion and timeout.

### 4. Space lifecycle

Port create, destroy, focus and move Space operations. Every asynchronous
operation gets a deadline. No polling loop without a timeout.

### 5. Cosmetic/private window capabilities

Only after lifecycle correctness:

- layer
- opacity
- sticky
- shadow
- PiP

Each capability stays optional and independently probeable.

## License discipline

Yabai is MIT licensed. If implementation code is copied or substantially
adapted, preserve Åsmund Vikane's copyright and the upstream MIT notice in the
relevant source file or adjacent third-party notice. Do not replace upstream
attribution with Rovr attribution.

## Test strategy

Every ported private operation gets three levels of tests:

1. Pure core test: desired state produces the expected typed action.
2. Mock platform test: failure, timeout and retry policy are deterministic.
3. macOS integration test: actual WindowServer result is re-observed and
   verified instead of assuming the private call succeeded.
