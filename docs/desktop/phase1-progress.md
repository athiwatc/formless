# Native desktop client (epic #1753): Phase 1 progress

Phase 0 is **not** accepted. Its open gates (findings §7, §8, §16.13,
§16.14) stay open and are listed again below so this page cannot be read
as a pass. Phase 1 work here is limited to slices that are safe without
them: the desktop crate only, no daemon, domain, auth, credential or
security-control change, no backend writes, existing clients unchanged,
and the crate still optional (detached workspace, not a default build).

## Done in Phase 1

| Slice | Commit | What it is | Tests |
|---|---|---|---|
| Typed command registry | `a23f7181` | `src/commands.rs` (no GPUI): every shell command with label, palette title, group, hint, on/off state and the reason it cannot run. Header buttons, composer buttons, global shortcuts and composer Enter/secondary-Enter all run through `Shell::run_command`, which re-reads availability at the moment of running. Composer commands keep the `ComposerActions` intent semantics; they still only record an `Intent`. | 8 model, 4 shell |
| Command palette | `a92ab626` | Cmd/Ctrl+Shift+P or the header Commands button. Modal, focus-trapped, background out of the accessibility tree, focus returned to the opening control; filter, arrows, Enter/click, unavailable commands say why; input-method composition in the search field is never turned into a run or a close; fits 480x360 at 200%. | 7 shell, 2 render states (real fonts) |
| Typed reply decoding | `1470342d` | `src/replies.rs` (no GPUI, no I/O): `ListSessions` into `SessionInfo`s; `GetSession` into a `SessionSnapshot` (Session id, `running`, timeline blocks in sequence order, hidden empty assistant turns). The TUI's role mapping (`user`, `tool`, `system-note` as a new `Note` block, anything else the assistant's). A reply for another Session, a missing or malformed field, a negative or repeated sequence number is a named error, never an empty list. `Shell::show_session_snapshot` replaces the open Session's timeline (a stream in progress included) and refuses a snapshot for any other Session. Run state is not taken from it. `serde` and `serde_json` became direct dependencies (already locked; two lockfile edges, no new package). | 6 model, 1 shell (mutation-checked: removing the Session guard fails it) |
| Full-output view | `ad722cf8` | A "View whole" control on every row that shows less than its block opens a modal, focus-trapped view of the whole block as plain monospace rows in a list that lays out only what is on screen, with Copy all. `src/full_view.rs` cuts the block at line breaks and at the row width (from the font's real advance), never inside a character; only row starts are kept (four bytes a row, dropped on close; in the memory policy table). Arrow, page, Home, End; Escape back to the control. Not for a block still arriving. | 5 model, 3 shell |
| Transcript events | `1e86a5e6` | `src/transcript.rs` (no GPUI or I/O): applies the protocol's `MessageAppended`, `MessageDelta`, `MessageComplete`, `MessagesEdited`, `DeltasDropped` and `StreamRestarted` to a decoded snapshot. The first delta starts a provisional row with an id outside the persisted sequence range; canonical assistant text replaces it. Tools/notes can arrive during it. Foreign Sessions are ignored, exact duplicates do not erase a newer partial answer, invalid/misordered text or a conflicting live Run requests reload instead of being combined. Edits, transcript-scoped/legacy/global gap warnings and restarts latch an explicit reload-needed state until a new snapshot. Snapshot decoding now validates each message's Session and retains the highest sequence even for hidden tool-only turns. Still not wired to the shell or a transport; snapshot/event generation ordering and Run lifecycle handling remain future client work. | 14 event-model, 2 reply-model; independent read-only review |
| Width-checked full output and source invalidation | `1b960542` | `full_view::measured_row_starts` checks the actual shaped advance of each candidate, then shortens on grapheme boundaries; an ASCII count is only a hint. Each measurement is at most 8 KiB/256 graphemes, uses a separate short-lived GPUI text system (64 queries per batch), and has a 128-entry borrowed-key fit memo, so the window's frame cache does not retain a layout of the whole transcript. An oversized/too-wide indivisible character gets an explanation and Copy all instead of silent clipping. Exact pixel width/full font changes reindex the view. A new snapshot or Session invalidates it, so old offsets cannot slice shorter or different UTF-8 text, even at the same id and byte length. The summary is a bounded, focusable scroll region above pinned buttons, after a 480x360/200% test exposed overflow before and after copying. Retained offsets are boxed, with no spare vector capacity. | 6 model, 4 shell; 2 real-font states added and compiled, not run |
| Fixture Sessions directory | `8ec921ee` | A virtualized, read-only Sessions destination over the existing unarchived sidebar projection. All-word, case-insensitive title/preview/ID filter; processed queries capped at 256 Unicode scalar values; bounded visual labels; selection retained by ID without activation, and kept in view after filtering. Arrows/Enter work in the filter or after F6 enters the region; Escape/Back returns to Chat. Enter/Escape during input-method composition only commit it. Activation revalidates the ID and uses the existing draft lifecycle. A decoded snapshot is cleared on Session change instead of relabeled; loaded empty and not-yet-loaded states are distinct. The fixture explicitly shares synthetic content; no rename/archive/delete, transport or backend action exists. | 4 model, 9 shell; 2 native render states added and compiled only |

One deliberate behaviour change came with the registry: while an input
method is composing in the composer, Send/Steer/Queue are unavailable
("Finish composing text first"), so an uncommitted reading is never
recorded. Stop is not held back.

Evidence (local, git-ignored): `.screenshots/gpui-1753-phase0/p1-registry-run1/`,
`p1-palette-run1/`, `p1-palette-run2/`, `p1-replies-run1/` and
`p1-full-view-run2/` (fmt, clippy, all crate tests; every log ends
`# exit: 0`). `p1-full-view-run1/` is kept as it ran: clippy failed there on
one test line (`useless_format`), fixed before run 2. From the full-output
view on, clippy also runs on the plain `gui` build without `test-support`,
which had not been linted before.

Transcript follow-up evidence (local, git-ignored):
`.screenshots/gpui-1753-phase0/p1-sol-transcript-run3/`. Offline, detached
lockfile: fmt; clippy for GUI-free, plain GUI, `test-support,thai-breaks`
and `ax-dump`; GUI-free model tests (106); feature-enabled model tests
(111); headless shell tests (84); Python platform-checker tests (8); and
doc tests all passed. Independent read-only review found no blocking issue;
its allocation-copy suggestion was fixed before this final run.

This is **not an aggregate/render pass**. In `p1-sol-transcript-run2/`, the
aggregate run passed its model tests but failed before any render frame:
macOS reported `com.apple.hiservices-xpcservice` connection invalid and
`+[NSPasteboard generalPasteboard]` returned NULL. Real-font rendering was
not verified. The first separate shell run also caught an old reply-test
fixture whose nested message Session was the placeholder `x`; that fixture
was corrected to match the reply before the final run, without weakening
validation. `p1-sol-transcript-run1/` is incomplete because the command
session was interrupted and is not counted as a full pass. No foreground
window, daemon, transport or network was used. No OS/session restriction
was bypassed to try to reach the renderer.

Full-output follow-up evidence (local, git-ignored):
`.screenshots/gpui-1753-phase0/p1-sol-viewer-run2/`. The final offline
checks passed: fmt, the four clippy configurations, GUI-free model tests
(112), feature-enabled model tests (117), shell tests (88), Python checker
tests (8), and doc tests. The width model
reproduces the character-count failure with double-width CJK/emoji, and
covers graphemes, tabs, bidi, CRLF/bare CR, nonmonotonic shaped prefixes,
tiny widths, oversized clusters and a heterogeneous 5 MiB line. The shell
checks shorter/same-byte-length snapshot replacement, foreign snapshots,
exact copy of the new revision, font changes at a fixed viewport, error
copy/close, and 480x360 at 200% before and after copying, with keyboard
access to the scrollable summary and controls. `p1-sol-viewer-run1/` is
kept as failed: the copied-state notice pushed a button outside the window;
that regression was fixed before run 2. Independent
read-only review found no blocking issue.

**Real-font pixels remain unverified.** No renderer retry was made after
the pasteboard/XPC startup failure above, and no OS/session restriction was
bypassed. The two new native render states exercise mixed CJK/emoji/Thai,
combining marks, bidi, tabs and CRLF at wide/100% and minimum/200%, checking
visible-row advances and start/end reachability and saving captures when
the renderer works. Clippy compiles these states but does not run them.
This is not a claim about glyph ink overhang, real-font startup cost or
Phase 0 acceptance; those still need the native runtime and visual review.

Sessions evidence (local, git-ignored):
`.screenshots/gpui-1753-phase0/p1-sol-sessions-run3/`. Offline checks passed:
fmt; all four clippy configurations; GUI-free model tests (116);
feature-enabled model tests (121); shell tests (97); Python checker tests
(8); and doc tests. The added cases
cover ID-preserving reorder/filter behavior, empty/over-limit queries,
10,000-row virtualization, visible preserved selection after filtering,
draft round trips, input-method composition, F6 navigation, decoded empty
snapshots versus unloaded content, Session ownership and 480x360 at 200%.
Independent review identified and resolved selection visibility, unbounded
status layout, the empty/unloaded distinction and a root Enter binding
that intercepted the Back button. Only query/root focus now handles list
activation; other controls receive their own raw key events. A selected
neighbor plus keyboard Back regression confirms the Session does not change.
`p1-sol-sessions-run1/` failed on stale accessibility wording; run 2's main
checks passed but its extra keyboard Back probe exposed that interception,
fixed before the final run 3. No foreground window or network was used.
Native
rendering remains unrun for the same pasteboard/XPC reason above; the two
new directory states are compile-only, not a visual acceptance pass.

Modal-navigation follow-up: opening Commands replaces an open full-output
viewer instead of leaving two dialogs stacked; explicit destination
navigation also dismisses that viewer before moving focus. Snapshot
replacement clears potentially obsolete conversation-row restore origins,
even after a palette has replaced the viewer, so dismissal cannot focus a
removed View whole control. Two added shell regressions cover
Viewer→Commands→Sessions, direct navigation, and
row→Viewer→Commands→snapshot replacement→Escape. Evidence is
`.screenshots/gpui-1753-phase0/p1-sol-navigation-run1/`: fmt, all four clippy
configurations, 116 GUI-free/121 feature-enabled model tests, 99 shell
tests, 8 Python tests and doc tests passed offline. Native rendering is
still not run. Independent read-only review found no blocker.

## Not done, and what each needs

### Phase 0 gates (still open; nothing in Phase 1 closes them)

| Gate | Needs |
|---|---|
| Windows build, render, input and accessibility at the current head | The Windows worker (separately owned; not duplicated here) |
| Linux build and render | The Linux worker (separately owned) |
| Real input methods (Thai, Japanese, dead keys) in the composer and now the palette search field | A person at the Mac |
| VoiceOver over regions, overlays, the palette, and whether it follows the palette's active-descendant row | A person at the Mac |
| 200% and display scaling looked at by a person | A person at the Mac |
| Performance budgets | A decision by the user (§11.4, §14, §16.10) |
| Packaging and supported platform tier | A decision by the user |

### Phase 1 and later (from findings §6 and §16.14)

| Item | State | Blocked on |
|---|---|---|
| Command registry and palette | Done (above) | |
| Header toggles exposed as toggle buttons (pressed state) | Not done: the kit's `Button::toggled` would change their platform role, which the macOS tree check (§16.12) would need to re-baseline | A real-window check run in the background |
| Typed decoding of daemon replies into the presentation model | `ListSessions`, `GetSession` and pure transcript-event application done (above); Run events and shell/client integration not started | Snapshot/event ordering and client reconciliation before transport integration |
| Client transport (connect, reconnect, version check, idempotent send) | Not started | Decision §5.2: extract `formless-client` or depend on `formless-tui` |
| Transcript paging | Not possible without a protocol change | Daemon work outside this epic (§5.3) |
| Destinations other than Chat | Sessions has a read-only fixture directory; Tools, Metrics, Proposals, Settings and Search remain scaffold notices | The client layer for real data; further UI-only slices can use fixtures |
| Approvals, questions, credentials, setup | Not examined, by instruction | Explicit scope from the user |
| Full-output view of a long block (§12.1) | Done (above), now width-checked and source-invalidated; not in the command palette (it needs a target block). Real-font render states exist but remain unrun | Native offscreen runtime; visual review |
| Thai line breaking wired in (§11.6, §5.1) | Study only | Decision §5.1 |
| Live-region and accessible-text performance (§16.10) | Measured, not changed | Budgets |
| Parity with the TUI (§6) | Inventory only | All of the above |

## Next slice

**Search the loaded transcript, not global history.** A separate,
read-only slice: explicit fixture/loaded-Session scope, bounded query,
results and scanning work; literal matches with source-safe byte ranges;
bounded snippets; revalidation against the exact source revision on
activation; invalidation on snapshot/Session/stream changes. No transport,
backend writes, auth or security-control changes. This must not imply that
unloaded history has been searched or silently reuse results from another
Session. The native full-output and directory render states still need a
working runtime; the existing restriction must not be bypassed to run them.

The transcript model still needs a shell/client boundary that displays its
reload-needed state and reconciles request generations before a transport
is connected. No Phase 0 acceptance gate is closed by these model/UI slices.
