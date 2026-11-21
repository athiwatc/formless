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
| Transcript events | `1e86a5e6` | `src/transcript.rs` (no GPUI or I/O): applies the protocol's `MessageAppended`, `MessageDelta`, `MessageComplete`, `MessagesEdited`, `DeltasDropped` and `StreamRestarted` to a decoded snapshot. The first delta starts a provisional row with an id outside the persisted sequence range; canonical assistant text replaces it. Tools/notes can arrive during it. Foreign Sessions are ignored, exact duplicates do not erase a newer partial answer, invalid/misordered text or a conflicting live Run requests reload instead of being combined. Edits, transcript-scoped/legacy/global gap warnings and restarts latch an explicit reload-needed state until a new snapshot. Snapshot decoding now validates each message's Session and retains the highest sequence even for hidden tool-only turns. Trusted shell wiring is described below; transport snapshot/event ordering and Run lifecycle handling remain future client work. | 14 event-model, 2 reply-model; independent read-only review |
| Width-checked full output and source invalidation | `1b960542` | `full_view::measured_row_starts` checks the actual shaped advance of each candidate, then shortens on grapheme boundaries; an ASCII count is only a hint. Each measurement is at most 8 KiB/256 graphemes, uses a separate short-lived GPUI text system (64 queries per batch), and has a 128-entry borrowed-key fit memo, so the window's frame cache does not retain a layout of the whole transcript. An oversized/too-wide indivisible character gets an explanation and Copy all instead of silent clipping. Exact pixel width/full font changes reindex the view. A new snapshot or Session invalidates it, so old offsets cannot slice shorter or different UTF-8 text, even at the same id and byte length. The summary is a bounded, focusable scroll region above pinned buttons, after a 480x360/200% test exposed overflow before and after copying. Retained offsets are boxed, with no spare vector capacity. | 6 model, 4 shell; 2 real-font states added and compiled, not run |
| Fixture Sessions directory | `8ec921ee` | A virtualized, read-only Sessions destination over the existing unarchived sidebar projection. All-word, case-insensitive title/preview/ID filter; processed queries capped at 256 Unicode scalar values; bounded visual labels; selection retained by ID without activation, and kept in view after filtering. Arrows/Enter work in the filter or after F6 enters the region; Escape/Back returns to Chat. Enter/Escape during input-method composition only commit it. Activation revalidates the ID and uses the existing draft lifecycle. A decoded snapshot is cleared on Session change instead of relabeled; loaded empty and not-yet-loaded states are distinct. The fixture explicitly shares synthetic content; no rename/archive/delete, transport or backend action exists. | 4 model, 9 shell; 2 native render states added and compiled only |
| Modal navigation and stale focus | `eb9d4913` | Commands replaces an open whole-output view; explicit navigation also dismisses it. Snapshot replacement drops obsolete conversation-row restore origins even when Commands has already replaced the viewer. | 2 shell; independent read-only review |
| Loaded-text Search | `ca612b32` | Commands → Go to Search opens a virtualized, read-only literal, case-sensitive, non-overlapping search over finished text already loaded in this window, including Markdown source. Explicit fixture/loaded-Session/unloaded scope; 256-scalar accepted query, 200 results, 4 MiB source-prefix and 4096-block scan bounds; partial and live-tail notices. Visible snippets have bounded grapheme context and a named preview limit. Result activation revalidates the captured corpus revision, current query, block identity and exact UTF-8 range before opening whole output at the match. Snapshot/Session/stream boundaries invalidate results, but live deltas do not rescan. Back preserves drafts and the Sessions filter; IME composition commits without activation/navigation. No I/O, history loading or backend actions. | 8 model, 11 shell; 2 native render states compiled only; independent read-only review |
| Snapshot request ownership guard | `63838034` | `src/snapshot_guard.rs` (no GPUI/I/O): opaque per-attachment and per-request tickets, one current pending request, no event buffer or transcript copy. New Session/connection contexts retire old work even for A→B→A or same-id reattachment; stale, duplicate and cancelled replies are refused before decoding. Relevant events observed during a request require retry rather than speculative replay. Shared event relevance classification with the transcript model. Still unwired: success proves local ownership/no observed overlap, not a server cutoff or recovered client. | 10 model; independent read-only review |
| Trusted typed-event shell view boundary | This change | The shell applies the existing pure transcript reducer after a decoded snapshot. Reducer metadata moves separately from the one shared timeline, without cloning the transcript; the GUI-free owned wrapper keeps its API. Deltas use the existing coalescer, while structural/canonical changes splice only the changed suffix to preserve an earlier reading anchor, and invalidate stale source views and Search results. First tail appearance refreshes Search; subsequent deltas do not. Sticky reload notices appear in Chat/Search/whole output and only a trusted replacement clears them. Session change drops reducer state. Synthetic stream methods require fixture provenance, including after leaving a decoded Session. No Run/composer state changes, socket code, automatic reload or recovery guarantee. | 7 shell; 2 native gap states compiled only; independent read-only review |

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


Search evidence (local, git-ignored):
`.screenshots/gpui-1753-phase0/p1-sol-search-run1/`. The final offline
checks passed: fmt, all four clippy configurations, GUI-free model tests
(124), feature-enabled model tests (129), shell tests (110), Python checker
tests (8), and doc tests. Model tests cover literal Unicode byte ranges,
exactly-full versus truncated result sets, UTF-8/empty-block scan limits,
source revisions, live-tail omission and a giant combining cluster that
cannot safely produce a bounded preview. Shell tests cover Commands
reachability, a deep match in a 2 MiB block opening in view, exact copying,
old-query/old-snapshot queued actions, unloaded versus loaded-empty
snapshots, stream completion, composition, repeated entry, keyboard Back,
draft/filter preservation, 480x360 at 200% and a queued activation behind
Commands. The kit's click helper repaints before clicking, so stale-frame
regressions invoke the same action boundary with the revision captured
by the old row. Independent read-only source review found no blocker.
The preliminary focused Search run passed 11 tests (including the existing
palette composition test); a later modal-queue regression is included in
the final 110-test shell suite.

The scan limits are not a CPU-time or process-memory guarantee. Search
retains positions rather than transcript copies, makes previews only for
visible rows and excludes the unfinished tail. Input undo/IME and font
caches remain outside its accounting. No foreground window, daemon,
transport or network was used. The two added native render states were
compiled, not run; the earlier pasteboard/XPC restriction was not retried
or bypassed. Real-font pixels, native assistive-technology behavior and
actual input methods remain open acceptance work.


Snapshot ownership follow-up evidence (local, git-ignored):
`.screenshots/gpui-1753-phase0/p1-sol-snapshot-guard-run1/`. The final offline
checks passed: fmt, all four clippy configurations, GUI-free model tests
(134), feature-enabled model tests (139), shell tests (110), Python checker
tests (8), and doc tests. Ten added model tests cover supersession,
one-shot tickets, wrong-guard ownership, A→B→A, same-Session reattachment,
retired events, cancellation/detach, named decoding errors, foreign replies,
non-transcript versus transcript/global gap events, counter exhaustion and
retaining live text when an overlapping reply is rejected. The preliminary
focused run caught a test expecting the wrong existing decoder error for a
null result; its expected named `Malformed` error was corrected, not the
decoder. Independent read-only review found no code blocker and required
the ordering limitation below to be explicit.

**This guard is not snapshot/event linearization.** `GetSession` currently
has no event watermark (`formless-protocol`'s request/response and event
shapes; `formless-core/src/daemon.rs`'s snapshot reply). A reply with no
observed interleaving may still be followed by a delayed event already in
it. Future integration must establish the server/transport ordering and
serialize event observation/application with resolve plus snapshot
installation, or revalidate queued work. Only then can it safely clear
recovery/gap state. The conservative observed-overlap policy can require
repeated retries during active streaming; no automatic retry cadence,
reconnect behavior, Run-state restoration or socket code was added.
`Shell::show_session_snapshot` remains a low-level fixture/view seam that
checks the Session id, not request freshness, and must not be connected
directly to unguarded asynchronous replies. No native renderer was run.


Typed-event view-boundary evidence (local, git-ignored):
`.screenshots/gpui-1753-phase0/p1-sol-event-view-run2/`. Final offline
checks passed: fmt, all four clippy configurations, GUI-free model tests
(134), feature-enabled model tests (139), shell tests (117), Python checker
tests (8), and doc tests. Seven new shell cases cover provisional/canonical
replacement with interleaved tools, exact canonical copying, unchanged
Run/draft/intent state, no-source and foreign/invalid events, sticky gap
warnings through later events and rejected foreign snapshots, warning
visibility in Search/viewer, clearing by trusted replacement, Search source
revisions versus per-delta stability, Session-switch ownership, late
synthetic producers, an unchanged scrolled-up reading anchor and
minimum-window 200% notice layout. The existing
GUI-free transcript tests still pass after splitting reducer metadata from
its owned timeline. Preliminary compile checks caught field/API
adaptation errors; those were corrected before the final suite.
Independent review identified a real provenance hole in an early guard:
checking only reducer presence allowed synthetic callbacks after Session
change to an unloaded source. All three synthetic APIs now require fixture
provenance, and the Session-switch regression exercises that exact flow.
The initial full run 1 passed its then-six new tests, but a later toolkit
source check found that `MessageScrollerState::reset` forces tail-following.
Structural event updates now use its anchor-preserving `splice` for only
the changed suffix, including when a coalesced live row has not reached the
virtual list yet. The seventh regression keeps an older row at the exact
same position across a delta, interleaved tool and canonical completion.
Only explicit snapshot replacement still resets the full list. Two native
gap-notice states were added/compiled, not run. There was no foreground,
daemon, network or denied renderer access.

### Ordering characterization (source evidence, not a protocol guarantee)

- `formless-core/src/daemon.rs::handle_conn_output`: a normal local socket
  has one output writer fed by the connection queue. The request worker
  awaits the handler, then enqueues its response. `send_all` independently
  enqueues matching broadcast events through the subscriber sender.
- `Request::GetSession` reads Session/messages and sampled Run state in a
  store read transaction, but that transaction is not an output-queue
  barrier or a sequence watermark. Sampling state and enqueuing its response
  are separate operations. Queue order alone does not identify which
  transcript updates are already represented in the reply.
- `formless-tui/src/client.rs` reads frames into one receiver, while
  `App::apply_reconnect_snapshot` has its own reconciliation behavior.
  Reusing only the socket reader is not equivalent to porting that state
  logic. Relay ordering has not been characterized here.

Therefore the new shell entry points remain **trusted serialized view
seams**, not network callbacks. Request/context ownership, delayed-event
handling, snapshot installation ordering and visible recovery state still
belong to a future client boundary. This slice neither selects/extracts the
transport nor treats a quiet guarded reply as proof of recovery. The local
snapshot guard and its ordering limitation above are unchanged.

## Not done, and what each needs

### Phase 0 gates (still open; nothing in Phase 1 closes them)

| Gate | Needs |
|---|---|
| Windows build, render, input and accessibility at the current head | The Windows worker (separately owned; not duplicated here) |
| Linux build and render | The Linux worker (separately owned) |
| Real input methods (Thai, Japanese, dead keys) in the composer, palette, Sessions filter and loaded-text Search | A person at the Mac |
| VoiceOver over regions, overlays, the palette, Sessions/Search and whether it follows each active-descendant row | A person at the Mac |
| 200% and display scaling looked at by a person | A person at the Mac |
| Performance budgets | A decision by the user (§11.4, §14, §16.10) |
| Packaging and supported platform tier | A decision by the user |

### Phase 1 and later (from findings §6 and §16.14)

| Item | State | Blocked on |
|---|---|---|
| Command registry and palette | Done (above) | |
| Header toggles exposed as toggle buttons (pressed state) | Not done: the kit's `Button::toggled` would change their platform role, which the macOS tree check (§16.12) would need to re-baseline | A real-window check run in the background |
| Typed decoding of daemon replies into the presentation model | `ListSessions`, `GetSession`, pure transcript-event application, trusted shell projection and a local snapshot ownership guard done (above); Run events and ordered client/transport integration not started | Snapshot/event ordering and client reconciliation before transport integration |
| Client transport (connect, reconnect, version check, idempotent send) | Not started | Decision §5.2: extract `formless-client` or depend on `formless-tui` |
| Transcript paging | Not possible without a protocol change | Daemon work outside this epic (§5.3) |
| Destinations other than Chat | Sessions has a read-only fixture directory and Search covers finished loaded text only; Tools, Metrics, Proposals and Settings remain scaffold notices | The client layer for real data; further UI-only slices can use fixtures |
| Approvals, questions, credentials, setup | Not examined, by instruction | Explicit scope from the user |
| Full-output view of a long block (§12.1) | Done (above), now width-checked and source-invalidated; not in the command palette (it needs a target block). Real-font render states exist but remain unrun | Native offscreen runtime; visual review |
| Thai line breaking wired in (§11.6, §5.1) | Study only | Decision §5.1 |
| Live-region and accessible-text performance (§16.10) | Measured, not changed | Budgets |
| Parity with the TUI (§6) | Inventory only | All of the above |

## Next slice

**Ordered client ownership before transport.** The trusted shell boundary
now exposes sticky transcript gaps, but it does not establish source
freshness. Integrate request/context ownership and characterize delayed
snapshot/event ordering before connecting a transport or clearing recovery.
The local snapshot guard supplies no protocol cutoff. Run lifecycle events
and the extraction-versus-TUI client decision remain separate work. No
backend writes, auth or security-control changes are implied by the
completed fixture/loaded-text UI slices.

The native full-output, directory, Search and gap-notice render states
still need a working runtime and visual review; the existing restriction
must not be bypassed to run them. No Phase 0 acceptance gate is closed by
these model/UI slices.
