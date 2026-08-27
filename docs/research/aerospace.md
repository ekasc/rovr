# Aerospace — research note for Rovr

Scope: what the i3-like tiling window manager **AeroSpace** for macOS does
well, what it does poorly, and what Rovr should take, leave, or invert.
Sources: the official guide and the GitHub README. Where Aerospace's
design collides with `../PRODUCT.md`, Rovr wins.

## 1. What Aerospace actually is

- A Swift macOS app, no scripting addition, no SIP disable, one private
  API used (`_AXUIElementGetWindow`).
- i3-inspired tree of containers (horizontal/vertical tiles) and
  accordions (tabbed/stacked). Optional floating windows that are still
  reachable through the focus command via a geometric "smallest tiling
  container that contains the center" rule.
- Its own emulated workspaces. macOS Spaces are treated as a hostile
  substrate: hidden workspaces are physically parked in a 1px vertical
  strip in a bottom corner of a monitor, then moved back to the visible
  region on switch. The "rearrange displays so every monitor has a free
  bottom corner" is documented as a real installation step.
- Single-server / multi-client. The daemon opens a Unix-domain socket;
  the `aerospace` CLI is a thin client. A separate `subscribe` request
  flips a connection into a JSON event stream.
- 22.7k stars, public beta, one maintainer. Public-Beta, 1.0 blocked on
  the immutable-tree refactor, thread-per-app performance work, native
  tabs, and dynamic TWM.

## 2. The decisions worth copying

### 2.1 Plain-text TOML config with a "default config ships in the app"
- `~/.aerospace.toml` (or `$XDG_CONFIG_HOME/aerospace/aerospace.toml`).
- An unambiguous ambiguity error if both are present.
- Default values are layered: scalars fall back to the bundled default;
  collections fall back to empty. Bootstrap is `cp default-config.toml`.
- `config-version` is an explicit opt-in to breaking-change behavior.
  Omitting it = version 1.

Why this is good for Rovr: the same shape works. Bundling a default
config removes the "empty file the user must read the manual to fill in"
failure mode. `config-version` is the only honest way to ship breaking
changes without lying to users.

What we should improve on:
- Aerospace's default config is several hundred lines and is what every
  user actually has to read. Rovr's default should be tiny, with the
  long form behind `rovr config dump --full` or a doc link, not in the
  user-edited file.
- The "scalars fall back, collections fall back to empty" rule is a
  tidy mental model but it makes bindings *the* place where omissions
  are silently meaningful. Rovr should consider being stricter: require
  the user to spell out what they want, and let *only* the small set of
  true defaults (gaps, orientation) be implicit.

### 2.2 "Scalar falls back, vector is opt-in" applied to config reload
- `auto-reload-config` is off by default, and the user has to reload
  once manually to start the watcher. No silent half-applied state.

Rovr takeaway: keep config reload transactional and explicit. The
"parse → validate → build candidate → diff → commit" pipeline in
`PRODUCT.md` is the right shape; the lesson from Aerospace is that the
*trigger* for reload also has to be deliberate (file watch + manual
command, never both racing).

### 2.3 Binding modes as a first-class concept
- A mode is a named set of bindings. Switching modes atomically
  activates the new set. The default `main` mode is mandatory. Escape
  returns to `main`. i3-style; Aerospace didn't invent it.
- Each binding may run *multiple* commands in sequence, and may also
  end by entering another mode: `r = ['flatten-workspace-tree', 'mode main']`.

Rovr takeaway: this is the right shape for the `rules` block too.
Aerospace's `on-window-detected` is a list of `{ if, run,
check-further-callbacks }` records, evaluated in order, short-circuit
unless explicitly told to continue. That's exactly the reactive-rule
shape we want, and it should be the same shape for both key bindings
and event rules — one parser, one engine.

### 2.4 Shell-like combinators on a typed command surface
- `;`, `||`, `&&`, `|`, `( )`, with `&&` binding tighter than `||`
  (deliberately *not* matching POSIX shell — Aerospace says POSIX is
  wrong here).
- Pipe semantics match `set -o pipefail`: a piped command fails if any
  stage fails.

Rovr takeaway: this is the right idea but the wrong layer. Our IPC is
typed JSON, not a string grammar; we don't need a parser. What we *do*
need is the same ergonomic power at the *binding / rule* layer:
sequences, conditionals, and "output of query → input of action." Concretely:

- A binding/rule value is a small expression: a list of command nodes.
  No string parsing at runtime.
- A query command (`list-windows --workspace focused --app Chrome`) can
  be piped into a target command (`focus --stdin`). Equivalent to
  Aerospace's `list-windows | focus --stdin` but typed.
- Conditional short-circuit (`if test A == B then run C`) maps onto our
  rule `if` clauses, not onto shell `&&`/`||`.

In short: take the *ergonomics*, drop the *string grammar*.

### 2.5 Environment-variable context forwarded to callbacks
- When a callback fires, the daemon sets `AEROSPACE_WINDOW_ID` or
  `AEROSPACE_WORKSPACE` to identify the subject. Child processes
  inherit it. `--window-id` / `--workspace` on a command override the
  env var, which overrides the focused window.
- The docs explicitly call out: this is what lets a callback
  `move-node-to-workspace 2; layout floating` keep operating on the
  *original* window, not the one that came into focus after the move.

Rovr takeaway: this is excellent and we should steal it. Our
`exec-on-workspace-change` analogue should always set `ROVR_WINDOW_ID`
/ `ROVR_WORKSPACE` / `ROVR_DISPLAY_ID` so user scripts (Sketchybar
helpers, yabai-migration scripts, custom rules) can act on a known
target. Typed IDs (`WindowId` in `PRODUCT.md`) are exactly the right
opaque token to expose.

### 2.6 Tree layout with two normalizations
- `enable-normalization-flatten-containers`: a container with one child
  collapses into the child. Root is exempted.
- `enable-normalization-opposite-orientation-for-nested-containers`:
  nested containers must alternate H/V. Prevents
  `h_tiles > h_tiles > ...` chains that confuse the focus command.
- Both are on by default and individually disable-able.

Rovr takeaway: the *idea* is right — keep the layout tree in a
canonical form so the focus command is unambiguous. But "tiling tree
paradigm" is one of two layout families we'd want. Rovr's `PRODUCT.md`
explicitly names BSP, stack, monocle, columns, master-stack, centered
master. We should not commit to the i3 tree as the only model. Take the
*normalization discipline* (canonical representation, no
ambiguous-state configurations) and apply it to whatever tree shape we
choose.

### 2.7 Dialog heuristic, with an explicit escape hatch
- Aerospace's `isDialogHeuristic` hardcodes "no fullscreen button
  means dialog" minus a terminal-app allowlist. Mis-classified windows
  are user-fixable via `on-window-detected` callbacks.

Rovr takeaway: a `Platform::is_dialog` capability, with a user
overridable default-float rule, is exactly the shape. Keep the heuristic
in the platform layer (it depends on Accessibility details), keep the
override in user config.

### 2.8 Workspaces pinned to monitors, with named-pattern matching
- `[workspace-to-monitor-force-assignment]` with `main`, `secondary`,
  1-based position, and regex substring patterns.
- Move-to-monitor is a no-op for force-assigned workspaces.
- Empty workspaces still go to their *assigned* monitor (Aerospace
  explicitly disagrees with i3 on this and explains why).

Rovr takeaway: keep this. The disagreement with i3 is the right call.
Force-assignment + skip-on-empty would be a Rovr policy we can
implement directly on top of `DesiredState` / `ObservedState`.

### 2.9 Public Unix socket with a versioned handshake
- Location: `/tmp/bobko.aerospace-${USER}.sock`.
- One-shot 4-byte version handshake (both sides send
  `SOCKET_PROTOCOL_VERSION = 1`).
- Length-prefixed (UInt32 LE) JSON frames for everything after.
- `subscribe` flips a connection into a server-streaming mode and
  closes on client disconnect.

Rovr takeaway: this is close to right but a few corrections:

- `/tmp` is the wrong directory. The Nehir / OmniWM doc surfaced in
  research shows the better pattern: `$TMPDIR/<app>/ipc.sock` or
  `~/Library/Application Support/<app>/ipc.sock`, with `0700` on the
  parent dir and `0600` on the socket itself. Aerospace is lucky
  because no other user can read `/tmp/bobko.aerospace-${USER}.sock`
  via path, but `umask` discipline and an auth token are the standard
  answer.
- The version handshake is good. Make the version an explicit
  *protocol* version, not a build hash, and document it.
- Two modes on one socket is fine for now, but we should plan to split
  control plane (request/response) and event plane (subscribe) onto
  separate sockets. The `Aerospace #1514` issue
  ("Expose AeroSpace callbacks via socket protocol") is exactly this
  growth pain: they want third-party scripts to receive callbacks
  without modifying the config, and the single-socket model can't
  cleanly express that.
- Include `serverVersionAndHash` in every `ServerAnswer` so a stale
  CLI (the user upgraded the daemon but not the wrapper) can detect
  the mismatch. Aerospace does this; we should too.

### 2.10 CLI is a thin client, commands are the only public surface
- `aerospace <subcommand> [args]` is the CLI.
- The same `args` is what the daemon sees over the wire. CLI and IPC
  share one argument grammar.
- Manpages and shell completion ship with the binary.

Rovr takeaway: PRODUCT.md already says "The CLI is a thin IPC client."
Aerospace proves the shape works. We should commit to one typed
`Command` enum on the Rust side that generates both the CLI parser
(clap derive) and the IPC request schema. No stringly-typed CLI.

### 2.11 Crash recovery: park windows back on quit
- On quit, all windows are moved back into the visible area. If the
  daemon is about to crash, it tries to do the same.

Rovr takeaway: this is the *only* thing the "Spaces are hostile"
design lets you get for free, and it's a real correctness property.
Our equivalent should be in `PRODUCT.md`'s "Reliability" section
explicitly: a `shutdown` / `panic` handler that re-observes and
re-asserts window positions before exiting. The "verify after
mutation" rule is the same principle at a smaller scope.

## 3. The decisions to invert

### 3.1 Hide windows in a 1px strip
- Works, but it's a hack that the user has to *arrange monitors around*.
  The whole `Displays have separate Spaces` section is apology text for
  a workaround that exists only because macOS Spaces are unusable.

What we should do: do not emulate Spaces via off-screen parking.
Accept that macOS Spaces are what we map onto, and build a proper
logical-workspace layer in the core (`PRODUCT.md` already says this).
Aerospace's honest admission "Spaces are just cursed in macOS" is the
right diagnosis; their workaround is a useful pressure-release but not
the long-term answer.

### 3.2 No GUI, ever
- Aerospace's stated position: "AeroSpace will never provide a GUI for
  configuration. Status menu icon is ok, because visual feedback is
  needed."

We should be more permissive. `PRODUCT.md` lists "optional native
macOS status/debug UI" as a future direction. The value is real:
visual feedback (which window is in which workspace, what's focused,
what's the layout) is information-dense, and a debug UI helps the
"flight recorder" story. Keep configuration in TOML, but a read-only
status/inspector UI is high-leverage.

### 3.3 "Don't play nicely with macOS features"
- This is the explicit non-value in the README. They refuse to
  acknowledge macOS Spaces and ship their own.

We should not take this stance. Rovr's whole pitch is to work with
macOS, not against it. The platform layer *is* the macOS integration;
we just want it behind a typed boundary. That is the opposite of
"don't play nicely."

### 3.4 Force-assigned workspaces ignore `move-workspace-to-monitor`
- This is a leak of "config policy" into the command layer. The
  `move-workspace-to-monitor` command silently no-ops, which the user
  has to discover from docs.

Rovr takeaway: the type system should make this an error, not a silent
no-op. `move-workspace-to-workspace(W, monitor(M))` against a
force-assigned workspace should return a structured
`Err(WorkspaceForceAssigned)`. `PRODUCT.md` explicitly asks for
"typed errors" and "stable codes." Use them.

### 3.5 Hide everything except the one private API
- Aerospace is proud of using only `_AXUIElementGetWindow` as a
  private API.

Rovr already plans to use SkyLight/private APIs and a scripting
addition. That's a real capability difference, not a moral one. We
should still keep the *boundary* clean (`PRODUCT.md` Platform layer
rules) — every private symbol is a future-update hazard and should
be confined.

## 4. Specific ideas to lift directly

1. **`config-version = N` opt-in** with a deprecation warning on
   reload, identical in spirit to Aerospace's. Lets us ship
   breaking changes without lying.

2. **Layered defaults** (scalars fall back, vectors are opt-in) for
   the TOML config, but with a smaller shipped default.

3. **Binding modes + reactive rules as the same engine.** Both are
   `{ trigger, if, run }` records evaluated in order. A mode-switch
   is just a rule that runs `mode <name>`.

4. **Environment-variable context in every callback.** `ROVR_WINDOW_ID`,
   `ROVR_WORKSPACE`, `ROVR_DISPLAY_ID` are always set, child processes
   inherit them, CLI flags override them.

5. **Subscribe-mode IPC** as a first-class public feature, with a
   typed event schema. Plan from the start to add a separate event
   socket (Nehir/OmniWM pattern: split control and event planes).

6. **Tree canonicalization (normalization) as a layout-engine
   invariant**, not as a config flag. Aerospace exposes it as a flag
   because their users are i3 refugees who may want raw trees; we
   don't have that legacy.

7. **A typed `Command` enum** that's the single source of truth for
   CLI parsing, IPC schema, and shell completion. One definition,
   three derivations.

8. **Crash-safety window repaint.** A `Drop` impl on the daemon's
   window registry that re-asserts visible positions before exit.
   Borrowed from Aerospace's "place all windows back on quit" behavior,
   generalized to a flight-recorder-driven recovery story.

9. **Public, documented IPC handshake** with a `SOCKET_PROTOCOL_VERSION`
   integer and a `serverVersion` field in every reply. Lets CLI and
   daemon versions drift safely.

10. **A platform-layer dialog heuristic** with a user-overridable
    default-float rule. The "no fullscreen button ⇒ dialog" trick is
    a real win and worth copying verbatim.

## 5. What we explicitly do *not* want

- A 200-line default config users must read top to bottom.
- Off-screen window parking in a 1px strip.
- "We don't acknowledge macOS Spaces" as a design principle.
- Silent no-ops on policy-violating commands.
- A string-grammar shell parser inside the daemon. Typed command
  nodes only.
- A GUI-free stance as religion. A read-only status/inspector is
  worth the engineering.

## 6. The `refreshSession` pattern — the most important idea in the codebase

From issue #131, the maintainer's own description of the architecture:

> The core architecture of AeroSpace is built around what I call
> `refreshSession`. It's an idempotent procedure that is run on every
> relevant event from macOS or the user itself (e.g. keybind). On every
> `refreshSession`, AeroSpace checks for newly appeared windows, checks
> that windows are still in the right position, and pushes them in the
> right position.
>
> This approach makes sure that we don't rely on reliability of macOS
> events being delivered to AeroSpace at all or being delivered in the
> right order. Until macOS sends events that at least approximately
> look like something reasonable, AeroSpace can provide a reliable
> experience.

This is *exactly* the `observe → decide → execute → verify → reconcile`
loop in `PRODUCT.md`. The maintainer arrived at the same shape
empirically, after years of fighting macOS. Two implications for Rovr:

- The reconciler is the **only** place that decides whether a window
  is in the right place. Bindings, rules, and event callbacks do not
  apply layout. They only mutate `DesiredState`. The reconciler
  diffs, executes, and verifies.
- "Idempotent" is the property we should be testing for, not just
  "correct." A re-run of the same reconcile pass with no inputs
  changed must be a no-op on macOS (no extra AX calls, no extra
  window moves), and a re-run after a partial failure must converge
  to the same end state.

Concretely: we should write a fuzzer / property test that runs the
reconciler in a loop on randomized state, and asserts the loop
converges in a bounded number of iterations. The earlier an
infinite-reconcile bug is caught, the better. Aerospace hit this
class of bug — they had to add `Coalesce idempotent refreshSession
calls triggered by event handlers` (#1249) as a stability fix.

## 7. The failure modes Aerospace is still fighting

These are the concrete bugs Aerospace is wrestling with as of 0.20.x.
They are exactly the bugs `PRODUCT.md` predicts and the reasons the
project description keeps saying "macOS state is untrusted." If Rovr
reaches production, we will hit variants of all of them.

### 7.1 The AX API blocks when an app blocks (#131, #1615)

When an app's UI thread hangs (IntelliJ, Emacs `sleep 5`, Spotify
quitting, Godot opening a project), `AXUIElementCopyAttributeValue`
also blocks. The whole tiling pipeline is on the main thread, so the
whole WM freezes. Mitigation: per-app threads with a messaging
timeout (`AXUIElementSetMessagingTimeout`). Aerospace added this in
0.18.

Rovr takeaway:
- The platform layer must use a per-app async AX worker pool, not a
  serial call. Use a hard per-request timeout.
- Reconciliation cannot block on AX. If a query times out, treat
  the window as "state unknown" and re-try on the next event.
  The reconciler must make progress even when some apps are silent.
- The flight recorder should record every AX timeout with app PID
  and the attribute being queried. Without this, debugging
  "the WM froze" requires the user to reproduce it with AX
  Diagnostic Reports.

### 7.2 Dead-window detection races (`#1215`, `#1216`)

When the screen is locked, macOS starts killing app windows. The
detection logic reads a `closedWindowCache` to know which windows
existed before the lock so it can restore them. If window-1 dies
before the lock screen snapshot is taken, and window-2 dies after,
the snapshot is missing window-1 — so on unlock, only window-2 is
restored. Worse, the surviving window may be moved to the focused
workspace because the cached window identity is now stale. Fixed in
0.19 by changing the cache to "frozen" snapshots and writing
immutable `TreeNode`s, with a follow-up that copies the cache on
screen lock.

Rovr takeaway:
- Workspace state in the daemon must be a **value** with a
  generation counter, not a mutable linked structure. The pattern
  is: on every platform event, copy the current value, apply the
  change to the copy, swap atomically. Generation bumps on swap.
  Stale generations are dropped on read.
- Lock-screen and sleep are not "interesting events to handle."
  They are "re-observation boundary events." Drop the in-memory
  cache, re-enumerate from the platform, reconcile against
  `DesiredState`. This is the only way to be correct.
- Property test: simulate a sleep/wake in a fuzz harness and assert
  that the post-wake window inventory matches what the user had
  assigned pre-sleep.

### 7.3 Cross-workspace focus races after window close (`#1097`, discussion #1048)

When a window is closed, macOS fires two events in undefined order:
"window destroyed" and "focused window changed." Aerospace tries to
re-focus the previously focused window on the closed window's
workspace. macOS simultaneously tries to focus "the next window of
the same app," which may live on a different workspace. The result:
visible flicker between workspaces, focus history pollution, and
sometimes Aerospace giving up and being unable to switch back
without a click.

The workarounds discussed in the issue tracker are grim:
- A 100 ms cooldown after close before syncing native focus.
- Disabling cross-workspace native focus sync entirely (breaks
  `cmd-tab` semantics).
- A `cmd-w` macro that saves the current workspace, closes, then
  re-focuses.

Rovr takeaway:
- This is a class of bug we will not fix with a `if` branch in
  the focus code. It needs a deliberate policy:
  - Own focus ownership. macOS's "next window of the same app"
    is *informational*, not a request we must obey. The daemon
    should have a single `desired_focus` field and reconcile
    against what the platform actually has focused, with a
    re-assert on the next event loop tick.
  - Treat platform focus as a *signal*, not a *command*. Our
    intent is the source of truth; the platform focus is just
    where the cursor / keyboard input is going right now.
  - This is the correct application of
    `ObservedState`/`DesiredState` from `PRODUCT.md` — but it
    must be a hard architectural line, not a guideline.
- A bounded time-based heuristic is acceptable as a *defensive*
  measure, but the *primary* mechanism is re-assert. If the
  re-assert fails, log to the flight recorder and stop trying.

### 7.4 Borderless / titleless windows + focus jumps (`#325`, discussion #1939)

`focus right` against a borderless Alacritty or a titleless Emacs
child frame can randomly change the focused workspace, because
"the window on top of the stack" from macOS's perspective is
some other window on some other workspace. Aerospace works around
this by force-focusing the workspace on a top-of-stack change.

Rovr takeaway: a window's "parent in the focus graph" is computed
from layout state, not from macOS's window order. This is a
deliberate divergence and must be tested with adversarial inputs.
A unit test that constructs a known focus graph and applies every
event in random order, asserting the focus converges to the
expected target on the next reconcile, would have caught this.

### 7.5 Ghost windows after Tahoe upgrade (issue #1615 comments)

Closing a window via `cmd-w` or `exit` sometimes leaves an
empty-title window in Aerospace's tree. The window no longer
exists in macOS but Aerospace never received a destruction event.
Symptoms: gaps in the layout, `aerospace list-windows` shows a
row with empty title, only `aerospace close --window-id` clears
it. The workaround scripts in the issue thread are telling —
users are debouncing empty-title checks themselves, two checks in
a row, skipping the focused window. This is the maintainer's own
admission that the real fix is a reactive (non-blocking) window
inventory update.

Rovr takeaway:
- A "ghost window" is the canonical symptom of `ObservedState`
  drifting from macOS reality. It is exactly the failure mode
  `PRODUCT.md` is built to avoid. Our reconciler must, on a
  bounded cadence or on certain events, *enumerate the windows
  we believe exist* and *verify each one is still real* against
  a fresh `enumerateAllWindows()` call. Windows we believe in
  but macOS doesn't, get removed from `ObservedState` and the
  layout gets re-reconciled.
- This is the "verify" step in the loop. It must be on by
  default. Make it tunable in frequency, not in presence.

## 8. The command surface — what to mirror, what to skip

Aerospace exposes 45 CLI commands. Surveying them:

**Direct lift** (these are the right primitive set):
- `focus`, `move`, `swap`, `resize` (the four cardinal-direction
  window operations)
- `move-node-to-workspace`, `move-workspace-to-monitor`,
  `move-node-to-monitor`, `move-mouse`
- `workspace` (with `--stdin` for piping)
- `layout` (the multi-target form is great: `layout floating tiling`
  means "toggle"; `layout tiles horizontal vertical` means "set or
  toggle orientation")
- `close`, `close-all-windows-but-current`
- `join-with` (i3's *better* version of `split` — `join-with right`
  creates a parent only when needed, plays well with normalization)
- `mode` (activate a binding mode)
- `reload-config` with `--dry-run` and `--warnings-as-errors`
- `subscribe` (the IPC event stream)
- `eval` (run a shell-like string of commands)
- `enable on/off` (suspend tiling without quitting the daemon)
- `debug-windows` (record AX dump for bug reports)

**Mirror but type the `format` system**: the
`%{window-id}%{right-padding} | %{app-name}...` interpolation
syntax is good for humans but redundant when we have `--json`.
Rovr should ship `--json` and `--format=<printf-string>` on
every query, but make the printf form a thin wrapper over the
JSON, not the source of truth. The interpolation variables should
be derived from a single Rust struct (`Window::fields()`) so JSON
and printf can't drift.

**Skip or re-think**:
- Aerospace's `split` exists for i3 compat only and "has no effect
  if `enable-normalization-flatten-containers` is turned on." The
  maintainer recommends `join-with` instead. We should not ship
  `split`; it's a footgun.
- `macos-native-fullscreen` and `macos-native-minimize` — useful
  escape hatches, but bind them to a separate "macos-native"
  namespace, not the top-level command tree, so it's clear that
  these are platform-tied behaviors.
- `eval` and the shell-like grammar — see §2.4, take the
  ergonomics, drop the string parser.
- `test` / `test-not` / `true` / `false` as commands — these exist
  to make the `if` clauses in `on-window-detected` work. In our
  typed rule engine, `if` is a real expression, not a string
  command. Different shape, same power.
- The `printf-format interpolation` for `--format` (e.g. `%{window-id}`)
  is a *stringly-typed* schema. The Rust equivalent is
  `Window::to_row(&[Field::WindowId, Field::AppName])` — typed
  field selectors, not `%{name}` tokens. A field rename becomes
  a compile error instead of a silent format break.

## 9. What Aerospace's roadmap tells us about our own

From the "Project status" section of the README, what's blocking
Aerospace 1.0:

- Performance: thread-per-application to work around AX blocking.
  This is what we should ship from day one.
- Big refactor: mutable double-linked tree → immutable single-linked
  persistent tree. Needed for stability, native tabs, and to
  properly fix the "windows jumping to focused workspace" bug.
  This is the architecture we should adopt at the start.
- Shell-like combinators (`||`, `&&`, `;`, `eval`). We discussed
  this above — typed version.
- `CGEvent.tapCreate` for global hotkeys so we can distinguish
  left vs right modifier keys. Nice-to-have; not architectural.
- After 1.0: sticky windows, dynamic TWM. Both are listed in
  `PRODUCT.md` already.

What this tells us about the order of architectural investments:

1. **Immutable persistent state with generations.** Not optional.
2. **Per-app async platform queries with timeouts.** Not optional.
3. **Single typed command schema → CLI + IPC + completion.** Not
   optional.
4. **Subscribe-mode event IPC.** First-class, not an afterthought.
5. **Shell-like composition at the typed layer, not the string
   layer.** Important for ergonomics; can be v0.2.
6. **Sticky windows, scratchpads, window groups, master-stack,
   centered master, monocle.** All in `PRODUCT.md`. None of them
   need architectural new ground; they all need the layout engine
   to be flexible. The first layout primitive after BSP should be
   master-stack because it's the i3 default and the most-tested
   mental model.

## 10. Open questions for Rovr

- Do we ship a default config, or refuse to start without one?
  Aerospace ships one; we should probably also, but smaller.
- Is the binding-mode concept worth its own first-class type, or
  is it just a special case of a rule that activates a different
  set of bindings?
- Where does the i3-style tree fit relative to BSP, stack, and
  master-stack? All five are listed in `PRODUCT.md`. Aerospace only
  has the tree; we shouldn't follow them here.
- One socket with two modes (Aerospace), or two sockets (Nehir /
  OmniWM)? Two is more honest about the trust and concurrency model.
- Do we want a `macos-native-*` namespace, or do we want platform
  behaviors to be invisible at the command level and exposed only
  as capabilities (`can_native_fullscreen`)?
- The `printf`-style `--format` flag: is it worth carrying for
  human pipe-friendliness, or is `--json` + `jq` enough? Aerospace
  carries both, but every new field is a new `%{name}` token and
  the docs are now huge.
- Should `join-with` be a command, or is it a property of the
  layout engine? (It's a property in BSP; Aerospace makes it a
  command because of the tree shape.)
