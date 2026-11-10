# formless-desktop (Phase 0 spike)

An optional native desktop shell for Formless built on
[GPUI Kit](https://github.com/longbridge/gpui-kit) 0.9.1. It is the Phase 0
compatibility and design spike of epic #1753. It is a scaffold, not a usable
client: it renders a synthetic fixture, never starts, contacts or configures
a Daemon Core, and only Chat has a view. The other destinations show a "not
built" notice.

Findings, measurements, the parity inventory and the open blockers are in
[docs/desktop/phase0-findings.md](../../docs/desktop/phase0-findings.md).

## Why it is outside the workspace

This crate has its own `[workspace]` table and its own `Cargo.lock`. It is not
a member of the repository workspace, so `cargo build --workspace`,
`cargo test --workspace`, the release workflows and the root `Cargo.lock` do
not see GPUI. Run every command below from this directory.

## Layout

| Path | What it is | GUI dependency |
|---|---|---|
| `src/model.rs` | Presentation model: destinations, Run state, composer actions, layout breakpoints and modal overlays, focus regions, Session list selection, drafts bounded by count and bytes | none |
| `src/fixture.rs` | Deterministic synthetic Sessions and timeline blocks (Thai, CJK, emoji, right-to-left) | none |
| `src/shell.rs` | The GPUI Kit window over the model | `gui` feature |
| `src/main.rs` | `formless-desktop` binary | `gui` feature |
| `tests/shell.rs` | Headless tests of the real shell view | `test-support` feature |
| `tests/render.rs` | Offscreen render of seven shell states through GPUI's real renderer (macOS, Linux) | `test-support` feature |

## Commands

```sh
# Model and fixture tests only: no GPUI crate is in this build's graph.
# Everything else below needs GPUI.
cargo test --no-default-features --lib --locked

# Headless shell tests (kit test window: real layout, focus, key dispatch)
# and the offscreen render check.
cargo test --features test-support --locked

# Keep the offscreen captures as PNG files to look at.
FORMLESS_DESKTOP_RENDER_DIR=/some/existing/dir \
  cargo test --features test-support --locked --test render

# The shell itself.
cargo run --release --locked
cargo run --release --locked -- --sessions 10000 --blocks 100000

# Open, print startup timings up to the first frame, quit after one second.
cargo run --release --locked -- --smoke
```

Keys in the shell: F6 / Shift+F6 move between regions, Cmd/Ctrl+B toggles the
Sessions sidebar, Cmd/Ctrl+Alt+B toggles the Context inspector. Each panel
docks when the window is wide enough (944 px for Sessions, 1280 px for
Context) and otherwise opens as a modal overlay that Escape or a click
outside dismisses. In the Sessions list, Up/Down move
the selection and Enter opens it. In the composer, Enter is the primary
action, Shift+Enter adds a line, Cmd/Ctrl+Enter is Queue during a Run.
Composer actions are recorded in the status line and never sent.
