# AGENTS.md

Read `PRODUCT.md` before architectural work.

## Rules

### 1. Think Before Coding

Don't assume. Don't hide confusion.

Before implementing:

- Read the relevant code first.
- Search `../yabai/` before guessing macOS/private API behavior.
- If uncertainty materially affects correctness or architecture and cannot be resolved from the repo, ask.
- Otherwise state the assumption and choose the most reversible option.
- If a simpler approach exists, say so.
- Push back on unnecessary complexity.

### 2. Simplicity First

Write the minimum code that correctly solves the task.

- No unrequested features.
- No speculative abstractions.
- No configurability that wasn't requested.
- No new dependency unless it removes real complexity.
- Prefer boring, explicit code.
- If 200 lines could reasonably be 50, simplify it.

### 3. Surgical Changes

Touch only what is necessary.

- Don't refactor unrelated code.
- Don't clean up adjacent code.
- Match existing style.
- Mention unrelated bugs or dead code, don't fix them.
- Remove only things made obsolete by your own change.

Every changed line should be necessary for the task, its verification, or cleanup caused by it.

### 4. Plan Big Changes

Before any substantial or multi-file change, write a short TODO list.

Each item should have a verifiable result.

Keep the TODO list updated while working.

Do not create ceremonial plans for trivial edits.

### 5. Verify

Do not claim completion from code inspection alone.

Run the narrowest relevant checks first, then broader affected checks.

When applicable:

    cargo fmt --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

Inspect the final diff for unrelated changes.

Never hide failing checks.

If something could not be verified, state exactly what was not verified and why.

## Git Boundaries (critical)

- Git operations are only performed when the user explicitly issues them:
  `git commit`, `git push`, branch creation/deletion, PR creation/merging,
  remote or tag mutations, and any deployment promotion.
- Editing a file in the working tree is not a git operation and needs no
  permission. Committing or pushing that edit does.
- When in doubt, make the file change, show what changed, and ask before
  running any git command.

## PR Communication (critical)

- PRs are authored by the user's account. Write PR titles and bodies as clean,
  professional release notes from the account owner: what changed and why.
- No meta-commentary, no instructions to the operator, no first/second-person
  notes about deployment process or review flow.

## Rovr-Specific

- `../yabai/` is read-only.
- Yabai is a behavioral/macOS-internals reference, not an architectural template.
- If Yabai conflicts with `PRODUCT.md`, Rovr wins.
- Port capabilities, not Yabai architecture.
- Keep macOS/private API hacks inside the platform layer.
- Never assume a macOS mutation succeeded. Re-observe and verify.
- `Compiles != done.`
