# formless-desktop (Phase 0 spike)

An optional native desktop shell for Formless built on
[GPUI Kit](https://github.com/longbridge/gpui-kit) 0.9.1. It is the Phase 0
compatibility and design spike of epic #1753. It is a scaffold, not a usable
client: it renders a synthetic fixture, never starts, contacts or configures
a Daemon Core. Chat and a read-only fixture Sessions directory have views.
The other destinations still show a "not built" notice.

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
| `src/model.rs` | Presentation model: destinations, Run state, composer actions, layout breakpoints and modal overlays, focus regions, Session list selection, text size, drafts bounded by count and bytes | none |
| `src/directory.rs` | Read-only filtering over the existing unarchived Session projection, selection by ID and bounded visual labels | none |
| `src/commands.rs` | Typed shell command registry and palette search | none |
| `src/replies.rs` | Read-only, validated `ListSessions` / `GetSession` decoding | none |
| `src/transcript.rs` | Pure transcript event application, canonical completion replacement and sticky reload-needed state; not connected to the shell or a transport yet | none |
| `src/full_view.rs` | Grapheme-safe, measured-width row index with bounded shaping candidates for the full-output view | none |
| `src/memory.rs` | Memory policy: composer limit, whole-edit rejection bookkeeping, undo accounting, text-free intent records | none |
| `src/timeline.rs` | Timeline blocks, a bounded movable window onto each long block, streamed tails rebuilt when shown and update coalescing | none |
| `examples/stream_model.rs` | Replays the synthetic stream into the model with no window and prints what keeping the streamed row up to date cost | none |
| `src/thai.rs` | Study, off by default: Thai word boundaries from ICU's dictionary (`thai-breaks` feature) | none |
| `src/fixture.rs` | Deterministic synthetic Sessions and timeline blocks (Thai, CJK, emoji, right-to-left) | none |
| `src/shell.rs` | The GPUI Kit window over the model | `gui` feature |
| `src/main.rs` | `formless-desktop` binary | `gui` feature |
| `tests/shell.rs` | Headless tests of the real shell view. The test window has no real fonts: every character is the same width | `test-support` feature |
| `tests/render.rs` | Offscreen render of twenty-five shell states through GPUI's real renderer, and measurements that need real fonts (macOS, Linux) | `test-support` feature |

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

# The same, with the Thai line-break study and its real-font measurement.
cargo test --features test-support,thai-breaks --locked

# The shell itself.
cargo run --release --locked
cargo run --release --locked -- --sessions 10000 --blocks 100000
cargo run --release --locked -- --huge-block-mb 5 --text-scale 200
cargo run --release --locked -- --window 480x360 --text-scale 200
# The same, without asking the window to grow: the reflowed shell.
cargo run --release --locked -- --window 480x360 --text-scale 200 --keep-window

# Open, print startup timings up to the first frame, quit after one second.
cargo run --release --locked -- --smoke

# Stream into the tail for 30 seconds at 100 deltas a second, print how
# display frames were delivered meanwhile, quit. Rate 0 measures the idle
# window.
cargo run --release --locked -- --stream-seconds 30 --stream-rate 100

# The model's share of that work alone: no window, no GPUI in the build.
cargo run --release --no-default-features --locked --example stream_model

# Check what a real macOS window gives assistive technology: three windows in
# the background, nothing typed or clicked; exit 0 when every check holds.
cargo build --release --locked --features ax-dump
python3 scripts/check-platform-a11y.py "$CARGO_TARGET_DIR/release/formless-desktop" /tmp/a11y-check
# Its own failure paths, with stub commands; starts no real application.
python3 scripts/test_check_platform_a11y.py

# For looking at a real window from outside: open it without coming to the
# front, quit after ten minutes, and write out the accessibility tree GPUI
# hands the platform and (macOS, `ax-dump` feature) the one the platform
# then exposes.
cargo run --release --locked --features ax-dump -- --background --lifetime 600 \
  --dump-a11y /tmp/gpui-tree.json --dump-platform-a11y /tmp/platform-tree.txt
```

Always run Cargo here with `CARGO_TARGET_DIR` set if the build output should
not land in `./target` (which is ignored).

Keys in the shell: F6 / Shift+F6 move between regions, Cmd/Ctrl+B toggles the
Sessions sidebar, Cmd/Ctrl+Alt+B toggles the Context inspector. Each panel
docks when the window is wide enough (944 px for Sessions, 1280 px for
Context) and otherwise opens as a modal overlay that Escape or a click
outside dismisses. Cmd/Ctrl with =, - and 0 change the text size between
100% and 200% and ask the window to grow to fit. Cmd/Ctrl+Shift+P, or the
Commands button in the header, opens the command palette (replacing an
open full-output viewer): type to filter,
Up/Down to choose, Enter to run, Escape to close; a command that cannot run
says why. Every button, shortcut and palette row runs through one typed
registry (`src/commands.rs`). A row that shows only part of a long block has
a View whole control: the whole block as plain text, scrolled with the arrow,
page, Home and End keys, with Copy all. Rows are fitted using the actual
monospace/fallback font advances, not a count of ASCII-width characters.
If one character cannot fit the window or safe layout bound, the view
explains the limit and keeps Copy all available. A new Session snapshot
closes the old view rather than applying old byte offsets to new text.
The text is always the
size asked for. A window smaller than that size needs (480 x 360 times the
size) is reflowed: every part is as wide as the window, the parts stack,
and the shell scrolls up and down as one page, with focus bringing each
part into view. A row shows
at most 32 KiB or 400 lines of its block, follows the end of a block that
is being streamed, and has Start, Earlier, Later and End controls when
there is more. In the Sessions list, Up/Down move
the selection and Enter opens it. In the composer, Enter is the primary
action, Shift+Enter adds a line, Cmd/Ctrl+Enter is Queue during a Run.
Composer actions are recorded in the status line and never sent.


The Sessions destination is a read-only fixture directory, not Session
management. Filter title, preview or ID (all words, case-insensitive; up to
256 Unicode scalar values), use Up/Down to choose and Enter/click to open,
or Escape/Back to Chat to leave. Input-method composition is committed
without also activating or leaving. Only visible result rows are laid out;
archived Sessions remain excluded. Each fixture Session shares the synthetic
transcript. Once a decoded snapshot has been shown, changing Session clears
that transcript rather than relabeling it; an empty loaded snapshot is kept
distinct from an unloaded one. No rename, archive, delete or daemon request
is performed. Search remains a separate future slice.
