# formless-desktop (optional native client)

An optional native desktop shell for Formless built on
[GPUI Kit](https://github.com/longbridge/gpui-kit) 0.9.1. Without `--connect`,
it preserves the synthetic Phase 0 prototype of epic #1753. With `--connect`,
it connects to an existing local Daemon Core using the protocol's native
socket or named pipe. This is an incremental client, **not epic completion**.

## Local daemon mode

Start the daemon through the existing CLI/setup path, then pass its Profile's
socket path (or full Windows named pipe name):

```sh
cargo run --locked -- --connect /path/to/profile/daemon.sock
# Private-profile real daemon + local fake HTTP provider; no GUI or paid API.
cargo test --no-default-features --features daemon-tests --test daemon --locked
```

This mode loads real Sessions, supports New Session, Send/Steer/Queue/Stop and
Resume/Abandon for a confirmed paused or interrupted Run,
and displays read-only Tools, recorded Run Metrics, Proposals and Settings.
Session details (palette or Sessions > Details) edits name, tags, pin or archive using
one acknowledged setter per Save. It targets the chosen list entry, or the
open Session outside the list. The Archived toggle includes archived entries
in the directory; opening one shows restore details without activating a
neighbour. Archive hides a Session and retains its history; running work
continues. Restore returns it to the default list for explicit activation.
Arrow keys browse resource entries; Copy details copies all returned fields
as JSON. Loaded-text Search keeps its explicit Session scope.

Profile Search is a separate command in the palette (Go to Profile Search).
It explicitly searches persisted messages across the connected Profile,
including archived Sessions, using the daemon's query/ranking semantics.
Enter in its query submits a read; arrows choose a returned snippet and Enter
in the results opens its Session without jumping to the message. An archived
or missing Session must be restored/refreshed in Sessions before opening.
No search is replayed after reconnect. Existing pending
questions are available in the Profile-wide Questions command/header entry,
with structured single/multi-choice and multiline free text. Their Session
identity stays explicit even while another Session is displayed. Existing
approvals retain their TUI handoff. The desktop never starts a daemon
or reads its database, credentials or configuration files.

The current protocol has no snapshot watermark or token offset. This client
therefore subscribes with deltas disabled and refreshes canonical snapshots:
completion text appears when the daemon persists it, rather than streaming
token by token. Events invalidate snapshots; they are never replayed onto
snapshot text. A relevant overlapping event schedules another read. Requests
carry window selection generations, including A→B→A navigation. An unchanged
snapshot leaves the reading anchor and open source viewer alone.

Reconnect reloads state without replaying mutations. Sending is disabled until
a current Session snapshot arrives. Drafts are kept after acknowledgement,
rejection, transport failure and navigation. Send deduplication in the daemon
is connection-scoped: after an unacknowledged action, the client blocks another
send for that Session in this window. Review the outcome and continue through
the existing TUI. The same unknown-outcome guard blocks Resume. Resume uses the
existing daemon admission path. Send/Steer/Queue and Resume first request the
daemon's forecast. Required consent opens a native dialog with Cancel focused,
bounded estimates/warnings, keyboard scrolling and copyable quote details.
Neither the quote nor the consent ACK starts work by itself: the view rechecks
the exact draft, selection and IME state before committing the prepared action.
Editing, cancellation, navigation or reconnect retires pending preparations;
mutations are never automatically retried. Recovery controls never send or
clear the composer draft. Recovery requests target the Session and cannot be
conditioned on the displayed Run ID with the current protocol.
Restarting the app is not a safe retry mechanism. OS pipe-write cancellation
and native Windows/Linux behavior still require platform validation.

Question answers require an explicit Submit and a positive `answered` reply.
A closed question is not reported as accepted. Ambiguous replies or a
disconnect after an answer write retain its unknown outcome and block retrying
that question; another action's
ACK cannot clear it or an unknown Send. Draft answers remain copyable after
closure, bounded to 16 questions and 4 MiB in this window (not persisted).
Each free-text field is limited to 64 KiB; oversized committed edits are
rejected whole. Existing questions reload through a Session snapshot, so open
a Session after connecting to restore them; new question events are global.
Unsupported definitions and exhausted native recovery limits explicitly
handoff to the TUI. These forms do not implement security approval semantics.

Session details loads a fresh correlated Session list, verifies the frozen ID
and rejects missing or duplicate identities before enabling Save. This avoids
loading a transcript just to read metadata. These
existing setters replace a current field; they cannot compare against a
previous revision, so another client's intervening edit can be overwritten.
Tags are one per line and use the daemon's normalization. Names/tags inputs
are limited to 16 KiB; at most 128 tags of 256 bytes each are editable here.
Cancel and Reload discard unsaved metadata edits explicitly; neither undoes
an already sent save. Errors retain the edit. A disconnect or ambiguous ACK
freezes Save until an explicit fresh Reload; no mutation is replayed.
New Step prompts for the subscribed Session produce an explicit Formless TUI
handoff notice. The current protocol does not restore pending Step identities
in Subscribe/GetSession;
native Step control/reconnect parity remains incomplete.

The optional `daemon-tests` feature links the daemon only into the test build.
The normal desktop graph does not embed the daemon or depend on the TUI.
The remaining sections document the preserved fixture and its diagnostics;
their synthetic-only behavior applies when `--connect` is omitted.

Findings, measurements, the parity inventory and the open blockers are in
[docs/desktop/phase0-findings.md](../../docs/desktop/phase0-findings.md).

Current continuation evidence and remaining acceptance are recorded in
[the Profile Search report](../../docs/desktop/profile-search-20261115.md),
[the archive/restore report](../../docs/desktop/session-archive-20261115.md),
[the Session metadata/Step report](../../docs/desktop/session-metadata-20261115.md),
[the native question report](../../docs/desktop/native-questions-20261114.md),
[the stream-ordering report](../../docs/desktop/stream-ordering-20261114.md),
[the forecast report](../../docs/desktop/forecast-confirmation-20261114.md),
[the recovery/packaging report](../../docs/desktop/recovery-and-packaging-20261114.md)
and [the live-client report](../../docs/desktop/live-client-20261114.md). Later
fixture-only evidence notes below describe historical checkpoints.

## Development packaging

From this crate's directory, with tracked changes committed:

```sh
python3 scripts/package-desktop.py --profile dev --offline --output /new/path/formless-package
```

The standard-library-only helper (Python 3.9+) builds the optional binary with
the locked dependencies, records the commit, build command and binary/lockfile
hashes, gathers Cargo dependency notices, and verifies the ZIP's executable
against the built bytes. It refuses an existing output directory. Use
`--allow-dirty` only for an explicitly marked development inspection; the
record then says `dirty: true`. Omit `--offline` if dependencies are not cached.

macOS gets a `Formless Development.app`; Windows and Linux get a portable
development directory. Each also gets a ZIP and SHA256SUMS. The helper never
installs, signs, launches, configures a daemon or requests OS permissions. The
default profile is release; `--profile dev` reuses the development cache when
inspecting packaging. These are development artifacts, not signed installers
or accepted native releases. Icons, distribution licensing (including assets
and standard-library notices), signing/notarization, updates and clean-install
OS acceptance remain release gates. BUILD.json explicitly records that limit.
The optional desktop CI arranges the same packaging check on all three hosts;
failed or unstarted hosted jobs do not establish platform support.

## Why it is outside the workspace

This crate has its own `[workspace]` table and its own `Cargo.lock`. It is not
a member of the repository workspace, so `cargo build --workspace`,
`cargo test --workspace`, the release workflows and the root `Cargo.lock` do
not see GPUI. Run every command below from this directory.

## Layout

| Path | What it is | GUI dependency |
|---|---|---|
| `src/model.rs` | Presentation model: destinations, Run state, composer actions, layout breakpoints and modal overlays, focus regions, Session list selection, text size, drafts bounded by count and bytes | none |
| `src/directory.rs` | Read-only title/preview/ID/tag filtering, selection by ID and bounded visual labels; archived scope is opt-in | none |
| `src/search.rs` | Bounded literal matches in finished loaded text, byte-range results, source-revision validation and bounded grapheme-safe snippets | none |
| `src/profile_search.rs` | Explicit persisted Profile search, bounded results and query/operation/connection ownership | none |
| `src/shell/profile_search.rs` | Native Profile search page, selection, copy, input and focus behavior | `gui` feature |
| `src/commands.rs` | Typed shell command registry and palette search | none |
| `src/replies.rs` | Read-only, validated `ListSessions` / `GetSession` decoding | none |
| `src/forecast.rs` | Validation and bounded presentation of daemon forecast quotes; estimates and consent remain daemon-owned | none |
| `src/session_edit.rs` | Bounded Session metadata setters and acknowledgement validation | none |
| `src/shell/sessions.rs` | Native Session metadata editor with correlated reads, focus and reconnect recovery | `gui` feature |
| `src/questions.rs` | Profile-wide question identity, validated answers and bounded close-event tombstones | none |
| `src/shell/questions.rs` | Native question review, bounded answer drafts, input, focus and acknowledgement states | `gui` feature |
| `src/transcript.rs` | Pure transcript event application, canonical completion replacement and sticky reload-needed state; a trusted in-memory shell seam uses it, with no transport | none |
| `src/snapshot_guard.rs` | Unwired local Session/attachment/request ownership guard; stale replies refused before decoding, observed event overlap requests retry, no transport-ordering guarantee | none |
| `src/full_view.rs` | Grapheme-safe, measured-width row index with bounded shaping candidates for the full-output view | none |
| `src/memory.rs` | Memory policy: composer limit, whole-edit rejection bookkeeping, undo accounting, text-free intent records | none |
| `src/timeline.rs` | Timeline blocks, a bounded movable window onto each long block, streamed tails rebuilt when shown and update coalescing | none |
| `examples/stream_model.rs` | Replays the synthetic stream into the model with no window and prints what keeping the streamed row up to date cost | none |
| `src/thai.rs` | Study, off by default: Thai word boundaries from ICU's dictionary (`thai-breaks` feature) | none |
| `src/fixture.rs` | Deterministic synthetic Sessions and timeline blocks (Thai, CJK, emoji, right-to-left) | none |
| `src/shell.rs` | The GPUI Kit window over the model | `gui` feature |
| `src/main.rs` | `formless-desktop` binary | `gui` feature |
| `tests/shell.rs` | Headless tests of the real shell view. The test window has no real fonts: every character is the same width | `test-support` feature |
| `tests/render.rs` | Offscreen render of native shell states through GPUI's real renderer, and measurements that need real fonts (macOS, Linux) | `test-support` feature |

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
is performed.

Commands → Go to Search searches only finished text already loaded in this
window, including Markdown source. It is literal, case-sensitive and
non-overlapping, not a global-history or daemon search. The scope says
shared synthetic fixture, loaded Session or not loaded. A query has at
most 256 Unicode scalar values. At most 200 result positions are retained,
and matching examines at most a 4 MiB source prefix and 4096 finished
blocks. Partial results and an omitted live tail are labelled. Those are
source-scan bounds, not a CPU or peak-memory budget. Live deltas do not
rescan the excluded tail; finishing a stream refreshes the results.
Only visible rows make bounded snippets; an unusually complex grapheme
gets a named preview limitation rather than unbounded context scanning.
Up/Down choose, Enter/click opens the whole block at the match, and
Escape/Back returns to the previous destination without losing drafts or
the Sessions filter. Enter/Escape first commit input-method composition.
Activation rechecks the query, source revision, block identity and exact
UTF-8 range; stale queued actions cannot open replacement text or stack
a viewer over a newer modal. Search never loads history or sends a request.

The final Search checks were run offline and separately from the native
renderer: GUI-free and feature model tests, headless shell tests, fmt,
four clippy configurations, Python checker tests and doc tests. The two
new real-font Search states compile but remain unrun after the earlier
native pasteboard/XPC startup failure. This is not a Phase 0 acceptance
or screen-reader, real-IME, Windows/Linux or visual-render pass. See the
Phase 1 progress page for exact evidence and remaining gates.


The trusted in-memory event view boundary (`Shell::apply_transcript_event`)
now uses the same reducer as the GUI-free `Transcript` model, after a decoded
snapshot. Metadata is moved apart from the existing shared timeline, not a
second transcript copy. Deltas are coalesced; tools/notes can precede the
live tail; persisted assistant text replaces it canonically. Structural
changes splice the changed suffix to preserve an earlier reading anchor,
and invalidate old source views/Search results.
A missed, edited, restarted or conflicting transcript gets a sticky reload
notice in Chat, Search and whole output. Later events do not clear it; only
a trusted replacement snapshot does. Foreign Sessions are ignored and
switching Session drops the reducer. Synthetic fixture-stream callbacks
cannot write into decoded or unloaded Sessions. Run/composer state and
outgoing intents are not taken from transcript events.

These are fixture/view seams, not socket callbacks. The separate snapshot
guard is still unwired; it only proves local ownership and no observed
overlap, not protocol ordering or recovered state. Neither seam should be
connected directly to asynchronous replies/events. The client transport
choice, event/snapshot ordering, native acceptance and platform gates remain
open. The two new native gap-notice states compile but have not been run.
