# PRODUCT.md

# Rovr

Rovr is a modern macOS window manager built using the hard-earned macOS knowledge in Yabai without inheriting Yabai's architectural debt.

Yabai is a reference implementation, not the codebase to reproduce.

## Goals

Rovr should be:

* fast
* deterministic
* scriptable
* recoverable
* observable
* easy to reason about
* resilient to macOS state becoming stale
* extensible without becoming fragile

The long-term goal is a macOS window manager with the ergonomics of modern Linux tiling window managers while respecting how macOS actually works.

## Product Direction

Rovr should eventually support:

* BSP tiling
* stack layout
* monocle layout
* columns
* master-stack
* centered master
* named workspaces
* dynamic Spaces
* persistent workspace restoration
* scratchpads
* window groups
* reactive rules
* event subscriptions
* stable versioned IPC
* hot-reloadable TOML configuration
* crash recovery
* state persistence
* diagnostics and flight recorder
* optional native macOS status/debug UI

Useful Yabai workflows may be supported where practical, but compatibility must not dictate Rovr's internal design.

---

## Architecture

```
CLI / clients
      |
      v
typed versioned IPC
      |
      v
Rust daemon
  events
  observed state
  desired state
  reconciler
  rules
  layouts
  config
  persistence
  recovery
      |
      v
safe Platform interface
      |
      v
macOS platform layer
  C / Objective-C where needed
  Accessibility
  SkyLight/private APIs
  Spaces
  Dock / scripting addition
  Mach
      |
      v
macOS
```

Rust owns product logic.

macOS-specific code exists behind the Platform boundary.

---

## Core Principle

macOS state is untrusted.

Never assume an OS mutation succeeded.

Rovr follows:

```
observe
-> decide
-> execute
-> verify
-> reconcile
```

This is the central architectural principle of Rovr.

macOS may:

* miss Accessibility events
* return stale information
* move windows without Rovr requesting it
* change state after sleep or wake
* restart Dock
* rebuild Spaces
* reconnect displays
* reject or partially apply operations

Rovr must recover instead of assuming its cache is correct.

---

## Observed State and Desired State

Rovr maintains two separate models.

```
ObservedState = what macOS currently reports
DesiredState  = what Rovr wants
```

The reconciler compares them and produces primitive actions.

Conceptually:

```
reconcile(observed, desired) -> actions
```

Examples:

```
MoveWindow
ResizeWindow
FocusWindow
MoveWindowToSpace
FocusSpace
```

The executor performs these actions through the Platform interface.

After sensitive operations, Rovr re-observes the affected state and verifies the result.

---

## State Ownership

The daemon has one authoritative owner of mutable state.

Subsystems should communicate using typed IDs rather than native references.

Examples:

```
WindowId
SpaceId
DisplayId
ProcessId
```

Do not store long-lived raw Objective-C objects, Accessibility references, SkyLight pointers, or BSP node pointers in durable core state.

Observed entities should carry generations or equivalent version markers so stale cached state can be detected.

---

## Rust Core

Rust owns:

* daemon lifecycle
* state model
* event processing
* reconciliation
* layouts
* rules
* IPC
* configuration
* persistence
* recovery
* diagnostics

Core Rust should contain essentially no unsafe code.

Unsafe operations belong close to FFI boundaries.

Prefer:

* explicit state transitions
* typed enums
* typed errors
* deterministic functions
* simple concurrency
* a clear owner of mutable state

Avoid async complexity unless there is a concrete reason for it.

---

## Platform Layer

The Platform layer provides primitive access to macOS.

It may:

* enumerate windows
* enumerate displays
* enumerate Spaces
* inspect window properties
* move and resize windows
* focus windows
* move windows between Spaces
* focus Spaces
* create or destroy Spaces
* manipulate window layers
* manipulate opacity
* manipulate sticky state
* subscribe to native events
* communicate with the scripting addition

It must not:

* choose layouts
* evaluate user rules
* decide workspace policy
* parse user configuration
* own desired state
* contain product-level behavior

The platform layer should be thin and boring.

Ugly private API behavior is acceptable there if macOS requires it.

It must stay contained there.

---

## macOS Versions

OS-specific hacks belong inside the macOS platform implementation.

Core code should not contain logic such as:

```
if macOS == Sonoma
if macOS == Sequoia
if macOS == Tahoe
```

The platform layer should expose capabilities instead.

For example:

```
can_move_window_to_space
can_create_space
can_set_window_layer
```

The rest of Rovr should depend on capabilities, not operating system names.

---

## Yabai Relationship

Expected development layout:

```
../yabai/    upstream Yabai checkout
./           Rovr
```

Yabai should be treated as read-only.

Use it to understand:

* Accessibility behavior
* SkyLight symbols
* private framework calls
* Space manipulation
* scripting addition behavior
* Dock integration
* native event sources
* macOS-version workarounds

Port capabilities and knowledge, not files or architecture.

Do not reproduce Yabai patterns such as:

* giant command parsers
* giant event-loop modules
* global mutable manager objects
* unity-build architecture
* policy mixed directly with OS calls
* raw pointer lifetime coupling
* unbounded polling
* silent failure

When implementation code is copied or substantially derived from Yabai, preserve required MIT attribution.

---

## Event Model

Native callbacks only translate macOS events into Rovr events.

Correct flow:

```
macOS callback
-> typed Event
-> update ObservedState
-> reconcile
-> Action
-> Platform
```

Callbacks should not directly apply layouts or decide policy.

Example event types:

```
WindowCreated
WindowDestroyed
WindowMoved
WindowResized
WindowFocused
SpaceChanged
DisplayAdded
DisplayRemoved
SystemWoke
SystemWillSleep
DockRestarted
```

Missing events must not permanently corrupt state because reconciliation can recover from them.

---

## Layout Engine

Layouts are pure computation.

Input:

```
usable screen area
ordered windows
layout configuration
```

Output:

```
WindowId -> Rect
```

Layout code must not call:

* Accessibility
* SkyLight
* FFI
* Platform

Layout code must never directly manipulate a real macOS window.

This allows layouts to be deterministic, unit tested, property tested, and developed without macOS.

---

## Workspaces

Rovr should eventually provide a logical workspace layer above raw macOS Space IDs.

Example:

```
code
browser
chat
music
```

A workspace may map to a Space but should not depend permanently on a volatile Space identifier.

This enables:

* named workspaces
* persistence across restart
* restoration after display reconnect
* dynamic Space creation
* workspace topology recovery

Workspace state belongs in the core.

Space manipulation belongs in the platform layer.

---

## Rules

Rules should be reactive and typed.

Possible triggers:

```
window_created
title_changed
app_launched
workspace_changed
```

Possible actions:

```
float
tile
move_to_workspace
set_opacity
set_layer
focus
```

Rules produce desired state or typed actions.

They should not directly call macOS APIs.

---

## IPC

IPC is a public interface.

It should be:

* typed
* versioned
* stable
* machine-readable
* easy to extend
* usable by third-party tools

The CLI is a thin IPC client.

The daemon owns behavior.

Do not reproduce Yabai's giant string-command parser.

Errors should have stable codes and structured details.

Rovr should eventually support event subscriptions through the same public interface.

---

## Configuration

Native Rovr configuration is TOML.

Configuration changes should be transactional:

```
parse
-> validate
-> build candidate
-> diff
-> commit
```

Invalid configuration must never leave the daemon partially reconfigured.

Reloading configuration should only disturb state affected by the change.

Compatibility with `.yabairc` can be considered later but must not distort the native design.

---

## Reliability

Rovr favors self-healing behavior.

Events such as these should trigger scoped or full re-observation:

* system wake
* Dock restart
* display topology change
* Space topology change
* suspicious mutation failure

Every OS wait must have a deadline.

No infinite polling.

Failures should be surfaced or recorded.

Rovr should maintain a bounded flight recorder containing recent significant:

* events
* actions
* state transitions
* platform errors
* reconciliation decisions

This should make failures debuggable after they occur.

---

## Testing

Pure Rust components should be heavily tested.

High-value targets include:

* reconciler
* layouts
* config validation
* IPC serialization
* state transitions
* stale-generation handling
* workspace restoration
* rule evaluation
* recovery behavior
* error mapping

Use a mock Platform implementation for deterministic testing.

Private macOS integration tests may be gated to macOS.

Regression bugs should receive regression tests whenever practical.

---

## Early Capability Order

Build Rovr vertically instead of attempting a full Yabai rewrite.

Recommended order:

```
move_window_to_space
focus_space
create_space
destroy_space
move_space
window_sticky
window_layer
window_opacity
```

Each capability should travel through the real architecture:

```
CLI / IPC
-> typed command
-> daemon
-> state or action
-> Platform
-> macOS
-> re-observe
-> verify
```

Only then move to the next capability.

---

## Prime Directive

Keep Yabai's macOS knowledge.

Replace the architecture around it.

Rovr should not merely be Yabai rewritten in Rust.

It should be a window manager designed around the reality that macOS is asynchronous, partially undocumented, and capable of changing underneath the application.

