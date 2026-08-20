# M4a — Shell completions

**Goal:** Give the `rovr` CLI first-class shell completions (bash/zsh/fish/powershell/elvish) so users can tab-complete subcommands, their arguments, and enum values. This is the first slice of the M4 "ecosystem" milestone and the only M4 item fully isolatable to `rovr-cli` (zero daemon/core change). It is immediately user-valuable for a CLI-driven WM and verifiable offline.

**Why now:** M3 finished the window-manager core (tiling, rules, workspaces, scratchpads, persistence). M4 is the ecosystem layer. Completions are the lowest-risk, highest-clarity first deliverable and establish M4 cadence without a daemon refactor. Subscription API / keybinds / menu-bar UI / WASM plugins follow as M4b–M4e.

**Design**
- `clap_complete` provides `Shell` (a `ValueEnum`) and `generate(shell, &mut Command, bin_name, &mut dyn Write)`. The CLI already derives `Parser`/`Subcommand`, so `Cli::command()` (via `clap::CommandFactory`) yields the full clap `Command`.
- Add `TopCommand::Completions { shell: Shell }`. Completions are generated and printed, then the process returns — they never touch the daemon socket (a completion script is static CLI metadata).
- In `main()`, match `Completions` *before* the socket send path and return early. `map_command` still receives every other variant; it gains a `Completions { .. } => unreachable!(...)` arm (documented: completions are handled in `main` before `map_command` is ever called).
- Test: build `Cli::command()`, generate the zsh script into a `Vec<u8>`, assert it contains the known subcommand tokens (`query`, `layout`, `scratchpad`, `completions`). Offline, deterministic.

**Plan (foolproofed)**
- `Cargo.toml` (workspace): add `clap_complete = "4"` to `[workspace.dependencies]` (matches clap 4).
- `crates/rovr-cli/Cargo.toml`: add `clap_complete.workspace = true`.
- `crates/rovr-cli/src/main.rs`:
  - `use clap::CommandFactory;` and `use clap_complete::{generate, Shell};`
  - `TopCommand::Completions { shell: Shell }` (with `#[command(about = "...")]`).
  - `main()`: early-return for `Completions`; add `generate_completions(shell)` helper.
  - `map_command`: add `TopCommand::Completions { .. } => unreachable!("handled in main before map_command")`.
  - `#[cfg(test)] mod tests` with `m4a_completions_include_top_commands`.

**Foolproofing (risks)**
- R1: `Cli::command()` requires `CommandFactory` in scope — import it. `Shell` must be `clap_complete::Shell` (not `clap::Shell`, which is a re-export only in some versions) — use `clap_complete::Shell` explicitly.
- R2: `generate` signature in clap_complete 4 is `generate<S: Into<Shell>>(shell, &mut Command, bin_name, &mut dyn Write)`. Pass `Shell` directly; `&mut Vec<u8>` coerces to `&mut dyn Write`.
- R3: adding a `TopCommand` variant forces `map_command` exhaustive — handled with the `unreachable!` arm so the type system stays honest and the runtime path is unreachable by construction.
- R4: `main()` early-return means `cli.command` is moved only in the `Completions` arm (which returns); the fall-through keeps `cli.command` owned for `map_command`. No double-move.
- R5: no daemon/protocol change — `Command`/`Request`/`Response` untouched; completions are pure CLI surface.

**Verification:** `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test -p rovr-cli` green (incl. `m4a_completions_include_top_commands`); `rovr completions zsh` (or bash/fish) prints a valid script. No live daemon needed.

**Deferred:** auto-install helpers per shell, completion of dynamic values (e.g., live window/space ids from the daemon), fig/nushell generators.
