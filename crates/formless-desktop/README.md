# macOS app bundle

Build an unsigned development app from the repository root:

```sh
python3 crates/formless-desktop/scripts/package-desktop.py --profile dev --output ~/formless-app
```

Double-click `Formless.app` to start the current profile's daemon if needed and
connect to it. The bundle includes the `formless` CLI; closing the window leaves
the daemon running. `--connect PATH` overrides autostart, and fixture/diagnostic
options disable implicit autostart. Outside the bundle, use `--start-daemon` to
opt in. CLI lookup tries the sibling binary, then `FORMLESS_BIN`, then PATH.
Startup failures are reported on stderr and in an alert, then the app quits.
This development bundle is unsigned; CLI dependency notices are not yet collected.

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

This mode loads real Sessions, supports New Session, Send/Steer/Stop and text-only Queue, and
Resume/Abandon for a confirmed paused or interrupted Run,
and displays read-only Tools, recorded Run Metrics, Proposals and Settings.
Session details (palette or Sessions > Details) edits name, tags, pin, archive or standing instructions using
one acknowledged setter per Save. It targets the chosen list entry, or the
open Session outside the list. The Archived toggle includes archived entries
in the directory; opening one shows restore details without activating a
neighbour. Archive hides a Session and retains its history; running work
continues. Restore returns it to the default list for explicit activation.
Arrow keys browse resource entries; Copy details copies all returned fields
as JSON. A queued copy is bound to the displayed destination, page revision
and selected entry; selection changes, refresh and open dialogs retire that
action rather than copying a replacement entry. Loaded-text Search keeps its explicit Session scope.

Commands → Review a new Session fork reviews the open Session in Chat or
the chosen entry in Sessions. Review writes nothing. Create fork then submits
one full-history fork using the daemon's default name. It copies history
persisted when the daemon executes, including applicable stored Session
context; unsent drafts and unpublished live output are excluded. The open
Session/draft stay unchanged, and the new child is not activated or started.
Its verified ID and copied-through sequence appear in the receipt/status;
choose it explicitly from Sessions and rename through Details if desired.

A shared client-lifetime reservation prevents duplicate confirmation and
reconnect replay. Known queue/preflight failures remain unsent. Once a request
is marked for writing, a lost, negative or malformed acknowledgement blocks
all further native forks and directs the user to inspect Sessions/TUI. Checking
the list does not automatically clear the guard. This is in-memory protection,
not server deduplication across new attachments/app restarts; check uncertain
outcomes before doing a fork elsewhere or starting a new client lifetime.
Bounded-prefix forks and custom-name review remain unavailable. See
[Session fork evidence](../../docs/desktop/session-fork-20261115.md).

Metrics offers Inspect Run for a selected Run row (or the same command in
Commands). It reads that exact Run's persisted status/error, message sequence
bounds, counters and reported spend, keeping both Run and Session IDs explicit.
The Summary row cannot inspect a Run. Scope's Copy details copies the complete
returned JSON, including future fields and private error/provider metadata;
other rows copy one group. Visual previews are limited to 500 characters, and
the native record limit is 1 MiB. A missing/null cost is unavailable, and no
complete bill is inferred from reported spend. Refresh is explicit; Back goes
to Metrics. Leaving, disconnect or Profile replacement retires the inspection;
no read is replayed after reconnect. This action does not control a Run or call
a model. See [Run inspection evidence](../../docs/desktop/run-inspection-20261115.md).

Commands → View Invocations or Tools → Invocations explicitly reads unfinished
Tool work across the connected Profile. Select a record for its Session,
recorded elapsed/silent time, reported progress and daemon status flags.
Refresh explicitly for another snapshot; values do not tick locally and a
Tool may finish after the read. Missing progress is unknown; 100% does not
prove completion, and abandoned does not necessarily mean cancelled. Legacy
records without durable Invocation IDs remain read only. Scope Copy details
preserves the whole returned JSON, including future fields and private labels;
other rows copy a complete record. Open Session navigates only to the selected
record's available, unarchived Session and preserves other drafts. No Tool
control or model call is made; Cancel/Abandon remains in TUI. Leaving through
Back, Commands or the Session sidebar, disconnecting or replacing the Profile
retires the snapshot and its callbacks. Reconnect never repeats this read.
Native limits are 1 MiB and 1,000 records, with no partial snapshot on error.
See [Invocation evidence](../../docs/desktop/invocations-20261115.md).

This window remembers an older reading message for up to 64 Sessions.
Returning to a Session waits for its accepted canonical snapshot, then returns
to that message's sequence identity rather than an old row index. If the
message is missing, it follows latest and says why. Returning to latest before
leaving forgets the older position. Normal canonical refreshes preserve the
current reading message when replacement overlaps it. Positions contain no
transcript or pixel/text-selection offsets; they are not saved across launches,
and Profile replacement clears them. Modal dialogs, whole-output windows and
preview-page ranges are not restored. See
[reading-position evidence](../../docs/desktop/reading-positions-20261115.md).

In the connected Sessions directory, Mark left chooses a comparison's left
Session without opening it or reading history. Select another entry with the
arrow keys, then Compare to read stored lineage, whole-message differences,
warnings and recorded branch usage. The left/right IDs and daemon sequence
scope stay explicit. A missing cost is unavailable, and priced subtotals or
observed-record ranges do not bound missing/unpriced usage. No model call,
branch summary or Run mutation is requested. Refresh is explicit; Back to
Sessions retires the result and keeps the left mark. Disconnect, Profile
replacement or a missing Session retires the mark too. No comparison is
replayed after reconnect. The native limit is 1 MiB and 1,000 message rows;
unsupported replies show an error rather than a partial comparison. Scope's
Copy details copies the whole validated JSON of known protocol fields,
including private Session settings and message metadata. Other rows copy
one complete record; their visual previews remain limited. See
[comparison evidence](../../docs/desktop/session-comparison-20261115.md).

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
security approvals are available through the Profile-wide Approvals command.
A bounded inventory of complete Tool requests supports explicit one-at-a-time
selection, Allow once or Deny, after a fresh exact identity/evidence comparison.
Batches, promotions, unsupported/mixed inventories and wider scopes retain
their TUI handoff. The desktop never starts a daemon
or reads its database, credentials or configuration files.

The current protocol has no snapshot watermark or token offset. Connected
production clients request legacy deltas when the daemon advertises
`subscribe_filter`. Assistant text and reasoning appear in a separate,
explicitly provisional live preview, never appended to canonical history.
Reasoning is labelled separately as a live preview; the daemon decides what reasoning it persists. The combined live text
is limited to 1 MiB; each displayed tail is limited to 32 KiB / 400 lines.
Updates coalesce at the existing 250 ms client refresh interval. Completion,
persisted assistant messages, Run end and accepted canonical refreshes retire
the preview. A refresh can leave a later live suffix rather than a reconstructed
answer; the preview is labelled as text since the last refresh. Dropped deltas
discard the preview and suppress the incomplete remainder until a completion,
restart or new Run boundary. Stream restart discards both channels and permits
the replacement stream. Both gaps and restarts refetch persisted history.
Reconnect and Session switches discard provisional state. Older peers without
the subscription capability retain canonical-only behavior.
The validation-only future-attempt reader remains disabled and unchanged.
Events invalidate snapshots; they are never replayed onto
snapshot text. A relevant overlapping event schedules another read. Requests
carry window selection generations, including A→B→A navigation. An unchanged
snapshot leaves the reading anchor and open source viewer alone.

Reconnect reloads state without replaying mutations. Sending is disabled until
a current Session snapshot arrives. Drafts are kept after acknowledgement,
rejection, transport failure and navigation. Send deduplication in the daemon
is connection-scoped: after an unacknowledged action, the client blocks another
send for that Session in this window. Review the outcome and continue through
the existing TUI. The same unknown-outcome guard blocks Resume. Resume uses the
existing daemon admission path. Send/Steer require the complete pending-file
inventory and daemon forecast, then explicit native confirmation even when the
inventory is empty or no cost consent is required. Cancel starts focused; the
full bounded draft/metadata, estimates and warnings are scrollable and copyable.
The confirmed message uses the daemon's atomic reviewed-send operation with no
legacy fallback. Queue uses the existing plain Queue RPC as described below. Drafts above 64 KiB require Formless TUI. Resume
retains its existing forecast/cost-consent path. See the
[native send-review contract](../../docs/desktop/native-send-review-20261116.md).

The daemon also provides an optional
[reviewed reference-removal boundary](../../docs/desktop/reviewed-reference-detach-20261116.md):
it compares complete pending metadata and removes one selected reference
occurrence without touching its target or artifact files. Session details now
provides [native reference review and confirmation](../../docs/desktop/native-reference-controls-20261116.md)
with Cancel focused first, exact selected metadata and conservative Unknown
handling. Unknown blocks further native removal and new work for its Session;
fresh reads and reconnect never resolve or replay it. Stored-file removal and
reference/attachment additions still require TUI. Client restart/replacement loses
the local guard and is not a safe retry mechanism.
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
Standing instructions also have a 16 KiB native limit. Saved changes take
effect at the next agent step, including during a Run; edges are trimmed and
blank clears. Missing or oversized instruction data disables that field,
with a TUI handoff, while other metadata fields and owned drafts remain usable.
Each success reply must explicitly contain the field it acknowledges; an
omitted field cannot acknowledge a default `false`, empty list or cleared value.
Cancel and Reload discard unsaved metadata edits explicitly; neither undoes
an already sent save. Errors retain the edit. A disconnect or ambiguous ACK
freezes Save until an explicit fresh Reload; no mutation is replayed.
Context now explicitly reads recorded Session usage for the open, loaded
Session. It labels unavailable cost and omitted optional counters, and keeps
small nonzero estimates nonzero. The reply is a past read: totals, last Run
and the capped recent-Profile Run count are sampled separately. Copy usage
contains the validated displayed projection only. This is not live billing
or a current-context meter. Session/Profile changes, snapshot invalidation and
connection loss retire results without replay; a newer snapshot in the same
selection generation does not refresh usage. No provider request is issued.

Session details also reads the existing persisted JSON bundle, Markdown or
HTML export on explicit request. A bounded plain-text preview and Copy export
are available up to 1 MiB; HTML source is never executed. Copy goes to the
system clipboard and may include private conversation/Tool/plan data. Unsaved
edits are excluded. Replies are scoped to Session, connection, operation and
format, retire on disconnect, and are never replayed. This is a past read;
Save export opens the system Save dialog and writes the complete accepted
result to a new file, up to the same 1 MiB bound. Existing files are never
overwritten, and no file is opened afterward. Cancel, a stale panel/export or
connection change writes nothing. Only one Save dialog may be pending in this
window. Native import and full-Profile backup remain unavailable.

Save export stages and syncs bytes in the selected directory and publishes
with an exclusive new-file link. Unsupported volumes fail without a direct-write
fallback; choose a volume with hard-link support or use Formless TUI. Directory
creation, replacement and automatic retry are not performed. This is not a
power-loss, latency or native OS picker/filesystem acceptance claim.

Step prompts are recovered through bounded inventory reads. One complete supported
prompt can be reviewed with native Continue, Skip, Edit or Session Step-off;
unsupported and multiple prompts use the Formless TUI. Enable Step mode is an
explicit, freshly revalidated Session control. It only turns the mode on for
this daemon lifetime; it never approves or releases a Tool. Missing sampled
mode disables the control, and an uncertain write blocks further native mode changes
for that Client without replay. See the [enable report](../../docs/desktop/step-mode-enable-20261115.md)
for ownership, receipt limits and validation. Idle Disable now changes future
Step pauses after a fresh idle/empty-inventory read; it does not release waiting
Steps or approve Tools. The daemon checks idle state again under its admission
lock and refuses changes while a Run or Step is pending. See the
[idle Step-off report](../../docs/desktop/idle-step-off-20261115.md).
Native OS acceptance and full Step
and security-approval parity remain open. See the
[native approval report](../../docs/desktop/native-approval-20261115.md).

The optional `daemon-tests` feature links the daemon only into the test build.
The normal desktop graph does not embed the daemon or depend on the TUI.
The remaining sections document the preserved fixture and its diagnostics;
their synthetic-only behavior applies when `--connect` is omitted.

Findings, measurements, the parity inventory and the open blockers are in
[docs/desktop/phase0-findings.md](../../docs/desktop/phase0-findings.md).

Current continuation evidence and remaining acceptance are recorded in
[the native export-file report](../../docs/desktop/export-file-20261115.md),
[the durable view preferences report](../../docs/desktop/view-preferences-20261115.md),
[the send-key choice report](../../docs/desktop/composer-send-keys-20261115.md),
[the remaining acceptance matrix](../../docs/desktop/acceptance-matrix-20261115.md),
[the native Find shortcut report](../../docs/desktop/native-find-20261115.md),
[the resource-copy follow-up](../../docs/desktop/resource-copy-20261115.md),
[the isolated source-copy report](../../docs/desktop/source-copy-20261115.md),
[the recorded usage report](../../docs/desktop/session-usage-20261115.md),
[the Session export report](../../docs/desktop/session-export-20261115.md),
[the standing instructions report](../../docs/desktop/session-instructions-20261115.md),
[the Profile Search report](../../docs/desktop/profile-search-20261115.md),
[the archive/restore report](../../docs/desktop/session-archive-20261115.md),
[the Session metadata/Step report](../../docs/desktop/session-metadata-20261115.md),
[the native question report](../../docs/desktop/native-questions-20261114.md),
[the stream-ordering report](../../docs/desktop/stream-ordering-20261114.md),
[the forecast report](../../docs/desktop/forecast-confirmation-20261114.md),
[the recovery/packaging report](../../docs/desktop/recovery-and-packaging-20261114.md)
and [the live-client report](../../docs/desktop/live-client-20261114.md). Later
fixture-only evidence notes below describe historical checkpoints.

## Desktop view preferences

Daemon mode loads `Formless Desktop/preferences-v1.json` beneath the user's
macOS `~/Library/Application Support`, Windows `%APPDATA%`, or Linux
`$XDG_CONFIG_HOME` (absolute paths only, otherwise `~/.config`). Fixture mode
loads no personal settings unless `--preferences PATH` is explicit.
`--no-preferences` disables all preference reads and writes; use it for
repeatable daemon diagnostics. `--text-scale P` overrides the saved size for
this launch. No override is saved automatically.

Commands → Save desktop preferences explicitly writes text size, light/dark
appearance, docked sidebar/inspector choices and send keys. Reload desktop
preferences explicitly replaces unsaved view choices; composer and answer
drafts are untouched. Changes are not automatically saved on editing or exit.
No Session identity, transcript, draft, daemon endpoint, credentials or
security decisions are stored. Window geometry, per-Session anchors, font,
density, reduced motion and system/high-contrast themes are not included.
Overlays are never reopened from preferences.

The first read creates no files. A missing file uses defaults. Invalid,
oversized or future-schema files are preserved and Save refuses to replace
them; repair or move the file, then Reload to recover. Save uses a stable
advisory lock without waiting, checks the exact file loaded by this window,
and stages/syncs a new file before replacement. A cooperating window's change
causes refusal until explicit Reload; no retry or forced overwrite occurs.
The lock file remains on disk. This is cooperative local-file protection,
not a guarantee against a noncooperating writer, hostile directory races,
filesystem stalls or power loss. File operations run only at startup and on
explicit Save/Reload; no latency or cross-platform filesystem acceptance is
claimed. Long status notices at minimum size/200% are keyboard-readable with
Up/Down, Page Up/Down, Home and End while Status holds focus.

## Development packaging

From this crate's directory, with tracked changes committed:

```sh
python3 scripts/package-desktop.py --profile dev --offline --output /new/path/formless-package
# Temporary-file checks only; no Cargo build or application launch.
python3 -W error::ResourceWarning scripts/test_package_desktop.py
```

The standard-library-only helper (Python 3.9+) builds the optional binary with
the locked dependencies, records the commit, build command and binary/lockfile
hashes, gathers Cargo dependency notices, and verifies the ZIP's executable
against the built bytes. It refuses an existing output directory. Use
`--allow-dirty` only for an explicitly marked development inspection; the
record then says `dirty: true`. Omit `--offline` if dependencies are not cached.
Clean-source commit and cleanliness checks run again after building and before
publishing staged output. ZIP timestamps use the commit time in UTC and file
modes are normalized, making archives repeatable for identical built bytes and
inputs under the same ZIP toolchain. This does not establish reproducible Rust
builds across hosts. Notice collection includes lowercase filenames and notices
under embedded data/assets/fonts; each copied source path and hash is recorded.
Collection is an inventory, not a complete licensing audit.
Reviewed immutable upstream notices and compiler-bound Rust notices are added
offline from `notices/`; lock, VCS, declaration and content changes fail closed.
Known licensing-note-only and missing-file gaps remain explicit. See that
directory's README and the accessible-performance/notice report in docs/desktop.

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

Pushes to `main` publish unsigned [GitHub prereleases](https://github.com/athiwatc/formless/releases)
with macOS DMGs (Apple Silicon and Intel), an Ubuntu 24.04 x86-64 DEB,
and a Windows x86-64 installer. Each includes the CLI from the same
commit so the app launcher can start the local daemon. Standalone CLI
archives are also attached. These are previews, not signed or notarized
distributions.

## Why it is outside the workspace

This crate has its own `[workspace]` table and its own `Cargo.lock`. It is not
a member of the repository workspace, so `cargo build --workspace`,
`cargo test --workspace`, and the root `Cargo.lock` do not see GPUI. Run every
command below from this directory.

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

# Bounded parser/conversion profile, 20 rounds, no GUI (allowed range 1..50).
cargo run --no-default-features --locked --example accessible_profile -- 20

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
docks when the window is wide enough (760 px for Sessions, 1120 px for
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
Every completed conversation block has a keyboard-reachable Copy source
control. It copies the complete exact source (Markdown, Unicode and whitespace),
including text outside the row preview, to the system clipboard. This may
include private conversation or Tool data. Direct copy is limited to 1 MiB;
larger blocks retain the existing View whole / Copy all path. Streaming tails
and transcripts requiring reload cannot be copied directly. Queued copy actions
are checked against the current source revision and modal state; they cannot
copy a same-ID replacement after navigation or snapshot changes. A status
notice confirms the clipboard write; focus and composer drafts stay in place.
This does not provide cross-block selection or a separate code-fence copy action.

With Conversation focused, Up and Down move through loaded messages, Home
moves to the first message, and End returns to the latest and resumes following.
The same actions are in Commands. The timeline's Jump to latest button has an
accessible name and returns keyboard focus to Conversation when it disappears.
These actions scroll the loaded view without fetching history or editing a draft.

The text is always the
size asked for. A window smaller than that size needs (480 x 360 times the
size) is reflowed: every part is as wide as the window, the parts stack,
and the shell scrolls up and down as one page, with focus bringing each
part into view. A row shows
at most 32 KiB or 400 lines of its block, follows the end of a block that
is being streamed, and has Start, Earlier, Later and End controls when
there is more. In the Sessions list, Up/Down move
the selection and Enter opens it. In the composer, Enter is the primary
action and Shift+Enter adds a line. The synthetic fixture also demonstrates
Cmd/Ctrl+Enter for Queue during a Run; daemon-backed Queue opens explicit review.
In fixture mode, composer actions are recorded in the status line and never sent.
In daemon mode they retain the existing forecast/consent and acknowledgement
path described above.

Connected Chat also offers **Run actions** (and matching Commands entries):
edit/resubmit the last user message, retry the latest Run with an optional
alternate model while preserving the original answer in a fork, rate the Run,
retry an in-flight provider request, compact, delete, and set/clear the daemon's
working directory. Each action reads and freezes its exact Session/Run target,
requires explicit review and confirmation, revalidates before writing, and waits
for its own acknowledgement. Edit and Retry use daemon forecast consent.
Compaction explicitly invokes the provider **without an available cost estimate**;
provider-request retry continues an already-admitted Run, not a new forecast.
Ambiguous writes are never replayed; an unknown outcome blocks further Run
actions and requires inspection in the TUI. Drafts are retained. The Context
inspector and status bar show live measured/estimated context usage; the
inspector also shows provider retry/stall state, Run usage and compaction notices.

Commands offers Use Enter for newlines / Use Enter to send. The default
remains Enter for Send/Steer and Shift+Enter for a newline. In newline mode,
Enter and Shift+Enter insert newlines and Cmd/Ctrl+Enter invokes Send/Steer.
The compact queue panel lists canonical follow-ups and supports revision-checked
editing, moving up/down and removal. It collapses by default below the scale-aware
1120px three-pane breakpoint;
Commands → Manage queued follow-ups reopens it. Queue requires a present, bounded
`queued_items` inventory. Old peers without item revisions cannot edit safely.
Enqueue is offered only during an active Run, checks for an empty pending-file
inventory before review and commit, and keeps the composer draft. It uses
`SendMessage(Queue)`: the message may start a new Run immediately if the current
one ends first. A forecast and explicit confirmation are always required; deferred
work is re-estimated by the daemon and may remain queued for cost confirmation.
No queue-to-send/steer shortcut or mutation replay is implemented.
The composer displays the current keys. Changing this per-window choice preserves
the draft, selection and undo/redo; it survives Session switches. When desktop
preferences are enabled, Save desktop preferences persists the choice for
later launches. Finish active input-method composition before changing
it. Open dialogs cannot be bypassed by the modified Enter chord.


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

Cmd+F on macOS or Ctrl+F elsewhere (also Commands → Go to Search) searches
only finished text already loaded in this window, including Markdown source.
The shortcut leaves open dialogs and active input-method composition alone.
When Search is already open, it refocuses the query without resetting its
text or selected result. It is literal, case-sensitive and
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
notice in Chat, Search and whole output. Later events are ignored until
a trusted replacement snapshot replaces the failed state. Foreign Sessions are ignored and
switching Session drops the reducer. Synthetic fixture-stream callbacks
cannot write into decoded or unloaded Sessions. Run/composer state and
outgoing intents are not taken from transcript events.

These are fixture/view seams, not socket callbacks. The separate snapshot
guard is still unwired; it only proves local ownership and no observed
overlap, not protocol ordering or recovered state. Neither seam should be
connected directly to asynchronous replies/events. The client transport
choice, event/snapshot ordering, native acceptance and platform gates remain
open. The two new native gap-notice states compile but have not been run.

The current-main integration and repaired discarded-candidate retry boundaries
are recorded in [the reconciliation report](../../docs/desktop/stream-reconciliation-integration-20261115.md).
They do not enable streaming or discharge the missing snapshot cutoff contract.

An unenabled [future-attempt protocol and admission model](../../docs/desktop/future-attempt-protocol-20261115.md)
adds a conservative producer barrier, turn/attempt correlation and byte offsets.
A [serialized reader and view adapter](../../docs/desktop/future-reader-20261115.md)
now exist behind validation-only constructors. That path remains disabled;
production uses the separate provisional legacy-delta preview described above,
not the future-attempt reader.

The [minimum production streaming plan](../../docs/desktop/production-streaming-plan-20261115.md)
records the required future-only recovery scope decision, implemented guarantees,
deterministic cutoff/publication schedules and remaining enablement gates.

Future-attempt-only v1 is the selected initial streaming scope. Its persistent
notice states that attaching or reconnecting mid-answer shows persisted history
until the answer completes or the next eligible attempt; this is not seamless
recovery. Production remains disabled. The
[real daemon-to-headless-view continuation](../../docs/desktop/v1-end-to-end-20261115.md)
checks that path and older-peer fallback without a native launch.

Session details also edits the selected Session's model/`role:name` and
reasoning-effort overrides. Blank inherits the configured default. Each Save
preserves the confirmed companion value, freshly checks the pair and blocks
new work preparation while pending. Saving requires the daemon's atomic
model/effort capability and compares both stored values inside the write
transaction. Unsupported peers receive no setting request. Saving does not
start a Provider test or conversation Run. It does not
automatically fetch a catalog or validate provider support. See the
[model selection contract](../../docs/desktop/session-model-selection-20261115.md)
for partial-reply, stale callback, no-op and unknown-outcome limits.


The Model field offers **Read catalog (contacts provider)**. This explicit read
uses the configured default provider and includes configured aliases, Roles and
recent selectors. Typing filters the bounded local snapshot after reading;
choosing a row fills the draft without saving. Save remains separate and does
not start a Provider test. No read is automatic or replayed after
reconnect. See the [catalog contract](../../docs/desktop/model-catalog-20261116.md)
for limits and headless validation scope.


The Approvals dialog now selects one request at a time from up to 16 complete
supported Tool requests, with 64 KiB total serialized evidence and 16 KiB per
request. Previous/Next grants nothing and returns focus to Close. Decisions
freshly find the selected unique approval ID and compare all its evidence;
reordering unrelated requests cannot retarget the decision. Unsupported or
duplicate rows reject the whole native inventory. See the
[multiple-request review contract](../../docs/desktop/multiple-approvals-20261116.md).


Session details → **Read pending files** explicitly reads the complete stored
metadata for that Session's next user message. It pages through all accepted
records and copies the complete JSON, including private daemon paths and future
metadata fields. Attachment paths are not opened. Other clients or a Send may
change or consume the list; read again explicitly. Unsupported daemons and
oversized/corrupt inventories show unavailable, never a fabricated empty list.
This is inspection only; attach, remove, drop/image paste and uncertain-write
reconciliation remain in Formless TUI. See the
[pending-file inventory contract](../../docs/desktop/pending-files-20261116.md).

The daemon-backed composer now confirms complete pending metadata atomically
with Send/Steer, including an empty inventory. Failed/missing/malformed send
receipts remain Unknown and block new work in that Client; replacing a Profile
or restarting is not a safe retry. Durable queue ownership, attachment mutations and
durable reconciliation remain unfinished. See the
[native send-review contract](../../docs/desktop/native-send-review-20261116.md).

## Schema-67 preview candidate

This branch retains the reviewed ordinary client/Core workflows through
`032703d`, using the same schema 67 as verified main. The complete later durable
journal/identity prototype is preserved separately at `8cd88cf`; it is excluded
from this preview. No existing prototype database can be downgraded by this
client/Core pairing. The preview originally disabled streaming and Queue; current
desktop support is documented above. Unknown guards remain
Client-lifetime. The client-only packager is carried over unchanged.
See [the exact split, compatibility tests and remaining gates](../../docs/desktop/schema67-preview-20261116.md).
