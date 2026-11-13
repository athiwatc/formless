# Native desktop client: Phase 0 findings

Status of the compatibility and design spike for the native GPUI client
(epic #1753). This records what was actually built and measured on one Mac on
2026-11-11, what was only read from manifests or source, and what is still
unknown. It is not a completion report for Phase 0, and nothing here is a
claim about Windows or Linux beyond dependency resolution.

The spike is [`crates/formless-desktop`](../../crates/formless-desktop/README.md).

## Summary

- GPUI Kit 0.9.1 resolves, builds and runs on macOS with the existing
  toolchain, from crates.io, with no patches, git dependencies or forks.
- A disposable shell (sidebar, conversation, composer, context inspector,
  status line) is built from kit controls over a GPUI-free presentation
  model. It is a scaffold, not a client: it renders a synthetic fixture only,
  has no Daemon Core connection, and only Chat has a view. Every other
  destination shows a notice that says it is not built.
- The headless and existing clients are untouched: the spike is a detached
  Cargo workspace, so the root workspace, the root `Cargo.lock` and every
  `--workspace` command are unchanged.
- Kit text input, virtualized lists, focus regions, Markdown and
  accessibility roles were exercised in the kit's headless test window with
  Thai, CJK, emoji and right-to-left text. That window has no real fonts
  (§11.1): it proves layout and input logic, not glyph widths or where real
  text wraps.
- Three findings need a decision before Phase 1: there is no Thai word-break
  model in GPUI's line wrapper, the client transport lives inside
  `formless-tui`, and the protocol has no transcript pagination.
- Still required and not inferable from anything here: the Windows build and
  run, real IME composition, a screen-reader pass and performance against
  the epic's budgets. A synthetic fixture on one Mac stands in for none of
  them.
- An independent early review raised six points; §9 records what was done
  about each.
- §11 records the acceptance follow-up: overlay semantics and focus
  restoration, the memory policy and the two review rounds it went through,
  5 MiB and streaming stress runs, a text size setting tested to 200%, and a
  Thai line-break study. None of it closes a gate that needs a real input
  method, a screen reader, a person looking, or another platform.

## 1. Dependency pin

Read from the published crates in the crates.io registry, not from the
upstream README (whose example still says `0.7`).

| Item | Value | Source |
|---|---|---|
| Kit release | `gpui-kit = "=0.9.1"` | crates.io |
| Kit tag commit | `39ab849a93b5f15507b969164d49891db8d5b470`, `crates/kit` | `.cargo_vcs_info.json` in the published crate |
| GPUI layer | `gpui-pre = "=0.3.12"` (exact pin by the kit), with `gpui-pre-platform`, `gpui-pre-macros` at the same version | kit `Cargo.toml` |
| Kit layers | `gpui-base 0.9.1`, `gpui-component 0.9.1`, `gpui-kit-assets 0.9.1` | resolved lockfile |
| Kit features used | defaults (`component`, `assets`); `test-support` only for the headless tests | spike manifest |
| Not enabled | `gpui-fast`, `tree-sitter*`, `syntect`, `inspector`, `profiler`, `speech`, `decimal` | spike manifest |
| Edition | 2024 for every `gpui-*` crate | published manifests |
| Declared MSRV | none: no `gpui-*` crate declares `rust-version` | `cargo metadata` |
| Highest `rust-version` declared by any package in the graph | 1.92 (`oo7`, Linux only); 1.91 (`notify-types`) | `cargo metadata` |
| Toolchain used | rustc 1.99.1, cargo 1.99.1 (stable) | host |
| Licenses | Kit and GPUI crates Apache-2.0; bundled icons are Lucide (ISC, `LICENSE-LUCIDE` in `gpui-kit-assets`) | published manifests |

Graph size (`cargo tree -e normal`, unique packages, no build performed for
the non-host targets):

| Target | Packages |
|---|---:|
| `aarch64-apple-darwin` | 518 |
| `x86_64-pc-windows-msvc` | 499 |
| `x86_64-unknown-linux-gnu` | 668 |

The spike's `Cargo.lock` holds 852 packages; the root workspace lock holds
528, names no `gpui` crate and is unchanged. Without the `gui` feature the
spike's own graph is 39 packages including build and dev dependencies
(`formless-protocol` and its dependencies), none of them a GPUI crate.

Checked directly in the final detached `Cargo.lock`:

| Check | Result |
|---|---|
| Sources | 850 packages from the crates.io registry, each with a checksum; 2 path packages (`formless-desktop`, `formless-protocol`); no git source |
| Kit layer | `gpui-kit`, `gpui-base`, `gpui-component`, `gpui-component-macros`, `gpui-kit-assets`, all 0.9.1 |
| GPUI layer | 21 `gpui-pre*` packages, all 0.3.12 (plus `gpui-pre-reqwest 0.12.15`, a separately versioned HTTP crate that is locked but not built on macOS) |
| Kit's direct dependencies | `gpui-base 0.9.1`, `gpui-component 0.9.1`, `gpui-kit-assets 0.9.1`, `gpui-pre 0.3.12`, `gpui-pre-macros 0.3.12`, `gpui-pre-platform 0.3.12` |
| Built on macOS | The five kit crates and 15 `gpui-pre*` crates, every one at the versions above |
| Fast stack | Nine `gpui-fast* 0.1.14` packages are locked (see below) and none is in the build graph, with or without `test-support` |
| Lock is current | Every build and test command here ran with `--locked` |

Licenses outside the usual MIT/Apache/BSD/ISC/Zlib/Unicode set in the three
target graphs: `option-ext` (MPL-2.0, all three), `enum-iterator` (0BSD), and
on Linux only `self_cell` (Apache-2.0 OR GPL-2.0-only, usable under
Apache-2.0). No font is bundled; the shell uses system fonts. A full
third-party notice audit is still to do before any distribution.

### Things the pin taught us

1. **`test-support` drags the Fast stack into the lockfile.** The kit's
   `test-support` feature names `gpui-fast?/test-support`. Cargo resolves a
   weak dependency feature into `Cargo.lock` even when the optional
   dependency is off, so enabling `test-support` adds nine `gpui-fast*`
   packages to the lock (802 to 851 packages) and, because `gpui-fast 0.1.14`
   requires `taffy = "=0.15.0"`, holds `taffy` at 0.15.0 instead of 0.15.1.
   None of the Fast crates are compiled. Three independent checks agree:
   `cargo tree -i gpui-fast` finds nothing to print with `test-support` on
   (and no such package at all with default features); the verbose build
   plans for the test and release builds list no `gpui-fast` unit; and the
   target directory holds no `gpui-fast` fingerprint or artifact. One tool
   disagrees and should not be used for this question: `cargo metadata`
   reports the lock-level resolve, which does not prune weak dependency
   features, so it lists five `gpui-fast*` packages as reachable under
   `test-support`.
   Consequence: the lockfile must be regenerated, not incrementally updated,
   when `test-support` is first added, and `taffy` cannot move until the kit
   does.
2. **A dev-dependency on the kit defeats GUI-free tests.** An early version
   of the spike had an unconditional `gpui-kit` dev-dependency with
   `test-support`; Cargo dev-dependencies cannot be optional, so that built
   GPUI even for `--no-default-features` tests. The manifest now has no
   dev-dependency: the harness is the spike's own `test-support` feature.
   The promise is exactly this and no wider: `cargo test
   --no-default-features --lib` builds and runs the model and fixture tests
   with no GPUI crate in its graph. The shell, the binary, the headless
   shell tests and the render check all need GPUI.
3. **`submit_on_enter` is `#[doc(hidden)]`.** The composer needs "Enter
   submits, Shift+Enter adds a line", which the kit implements through
   `TextareaState::submit_on_enter(true)`. It works, but it is not part of
   the documented API, so a kit upgrade can change it without notice.
4. **Shift+Enter also emits `InputEvent::PressEnter`.** With
   `submit_on_enter`, Shift+Enter inserts the newline and still reports
   `PressEnter { shift: true }`. A handler that ignores `shift` submits on
   Shift+Enter; the spike made that mistake first and a headless test caught
   it.
5. **Kit facade works without a `gpui` dependency.** `gpui_kit::actions!`,
   `#[gpui_kit::test]`, `Render` and `IntoElement` all resolve through the
   facade; the spike never names `gpui` directly, as the epic asks.

## 2. Platform findings

| Platform | What was done | Result |
|---|---|---|
| macOS 27.2 (26B123), Apple M1 Pro, 16 GB, built-in Retina display | Debug and release build, headless tests, real window launch | See §4. Built with the Command Line Tools only: no Xcode and no `metal` shader compiler are installed. That works because the kit enables GPUI's `runtime_shaders` feature. |
| Windows | Dependency resolution only (`cargo tree --target x86_64-pc-windows-msvc`) | **Not built, not run.** The graph resolves to 499 packages and pulls four major versions of the `windows` crate (0.57, 0.58, 0.61, 0.62). The Windows gate has to run on the Windows PC. |
| Linux | Dependency resolution only | **Not built, not run.** The kit turns on both `x11` and `wayland` for `gpui-pre-platform`; system packages needed are unverified. |

Not verified anywhere yet: a packaged/signed app bundle, native menus, OS
accelerators, drag and drop, clipboard images, multi-monitor restore, HiDPI
scale changes, VoiceOver or NVDA, and real IME composition (the headless
harness types text but does not run an OS input method).

## 3. What the spike contains

| Area | In the spike | Kit pieces used |
|---|---|---|
| Shell layout | Header, Sessions sidebar, conversation, composer, Context inspector, status line | `h_flex`, `v_flex`, `Button`, theme tokens |
| Layout breakpoints | Three docked panes at 1280 px and wider. Below that the inspector leaves the row; below 944 px (264 sidebar + 680 reading minimum) the sidebar does too. Narrowing never puts a panel over the conversation by itself. | model only |
| Overlays | Below its docking width each panel's toggle opens it as a modal overlay instead: dimmed backdrop that swallows pointer input, focus moved in and trapped (F6 and Tab), Escape or a backdrop click to dismiss, focus returned to the region it came from. One overlay at a time. Widening docks it and forgets the overlay. | kit `focus_trap`, key contexts |
| Sessions list | 10,000 rows, Pinned/Active/Recent order, textual status, selection separate from activation, row element ids derived from the Session id | `v_virtual_list` |
| Timeline | 10,000 mixed blocks with stable block ids, tail following, jump-to-latest control | `MessageScroller` over GPUI `list`, `TextView::markdown` |
| Composer | Multiline, Enter / Shift+Enter / Cmd-or-Ctrl+Enter, Send / Steer / Queue by Run state, Stop in its own fixed slot, disabled reasons, draft kept after a recorded send. Accepted text is limited to 1 MiB; an edit that would pass it is rejected whole (§11.3) | `Textarea`, `Button` |
| Drafts | One unsent draft per Session, bounded to 64 Sessions and, given that no single draft exceeds it, 4 MiB of text in total. The oldest are evicted first and the user is told how many. Sessions holding a draft are marked in the list | model only |
| Focus | F6 / Shift+F6 across regions, hidden panels skipped, focus never left in a region that is no longer on screen | GPUI focus handles |
| Destinations | Chat is the only view. Sessions, Settings, Tools, Metrics and Proposals replace the conversation and composer with a "not built" notice and say so in the status line. Search is not reachable. | `Label`, `Button` |
| Theme | Light and dark through the kit theme (the header toggle itself is untested); status never by colour alone | `Theme::change` |
| Text size | 100% to 200% in 25% steps (Cmd/Ctrl with =, - and 0). Fixed sizes and layout breakpoints follow it (§11.5) | kit theme font sizes |
| Timeline rows | A row lays out one window of at most 32 KiB and 400 lines of its block. The window follows the tail of a block being streamed, and any row that shows less than its block says what is missing and can be paged (§12.1) | `TextView::markdown`, `Label`, `Button` |
| Streaming | Deltas update the data at once and the view at most once per 16 ms; only the tail row is re-measured (§11.4) | `MessageScrollerState` |
| Intents | `Intent::SendMessage` and `Intent::InterruptRun` are noted and shown, never sent. What is kept is a record of sizes; no message text is retained | none |

Deliberately absent: any Daemon Core connection, real Sessions or
transcripts, the action registry and command palette, Search, every
destination's page except Chat, attachments, approvals and questions,
persistence of any preference, high-contrast and colourblind themes, density
settings, the reduced-motion setting, and packaging.

Known limits of what is there:

- The header stays outside a modal overlay on purpose, because it holds the
  toggle that opened it. Its buttons therefore remain clickable while an
  overlay is up.
- Dismissing an overlay returns focus to the exact control that had it, if
  that control's region is still shown; otherwise to the composer or the
  conversation.
- Overlay panels expose the dialog role, a name and a description. The kit
  has no "modal" accessibility property to set, and no screen reader has
  been run.
- Panel widths are fixed; nothing is resizable.
- The timeline's jump-to-latest control comes from the kit and is untested
  here. Every Session shows the same synthetic timeline.
- Memory bounds are stated in §11.3. In short: accepted composer text and
  stored drafts are bounded; the retained intent holds no text; **no bound
  is claimed for the kit's undo and redo history**, which was not measured.
- A row shows one window of a long block at a time. A window that starts
  inside a table or a list is laid out without its beginning, and there is
  no separate full-output viewer (§12.1).
- A window too small for its text size keeps that text size and is
  reflowed into a page that scrolls up and down (§15). Nobody has looked
  at that in a real window and no screen reader has read it.

The layout tokens are the epic's starting values. At 200% text size they
scale with everything else (§11.5); nobody has looked at the result.

## 4. Measurements

All on the Mac in §2. Builds used `CARGO_BUILD_JOBS=2`, `CARGO_INCREMENTAL=0`
and no debug info, so build times are an upper bound for this machine, not a
typical developer build.

### Tests

| Command | Result |
|---|---|
| `cargo test --no-default-features --lib --locked` | 61 passed (model, memory, timeline and fixture); no GPUI crate in its dependency graph |
| `cargo test --features test-support --locked` | 61 passed, 66 passed (headless shell), 19 offscreen render states passed (37 captures) |
| `cargo test --features test-support,thai-breaks --locked` | 66 passed, 66 passed, 19 render states and the real-text Thai measurement passed |
| `cargo fmt --check`, `cargo clippy --all-targets --locked` with and without `thai-breaks` | clean |

These are the counts at the last code commit (§12). The first checkpoint
had 22, 16 and 7; the follow-up in §11 had 42, 45 and 10.

What the headless shell tests establish, on macOS, in the kit's test window.
That window's text system gives every character the same width (§11.1), so
where a bullet below speaks of text wrapping or fitting it is by character
count, not by real glyphs:

- Three panes dock at their token widths at 1440 px. At 1100 px the
  inspector is gone until its toggle is pressed, then overlays the
  conversation without covering that toggle. At 800 px the sidebar is gone
  too and Send is still visible. Widening docks the inspector again.
- The inspector overlay is modal: opening it moves focus in, F6, Shift+F6,
  Tab and Shift+Tab stay inside it, a click on Send underneath it does
  nothing, Escape closes it and focus returns to the region it came from. A
  backdrop click closes it without activating the Session row under the
  pointer.
- At 800 px the Sessions toggle (shortcut and header button) opens the
  sidebar as a modal overlay. Arrows preview, Escape closes without opening
  anything, Enter opens the Session, closes the overlay and returns focus to
  the composer.
- Widening with an overlay open docks the panel and removes the backdrop;
  narrowing again does not bring the overlay back.
- A Session's row is found under the same element id at two different list
  positions.
- Choosing Tools removes the conversation and composer, paints "Tools: not
  built" and the scaffold notice, moves focus off the vanished composer, and
  takes the composer out of the F6 cycle; Back to Chat restores the view and
  the draft.
- Typing Thai, Japanese, Korean and a ZWJ emoji sequence into the composer
  round-trips exactly; the composer reports the multiline text-input
  accessibility role and its label.
- Enter records a `Send`, Shift+Enter adds a line and records nothing, and
  the draft survives because nothing acknowledged the send.
- A running Session shows Steer and Queue, Stop keeps its position, and
  Cmd/Ctrl+Enter records `Queue`.
- F6 cycles Composer, Context, Sessions, Conversation; a closed inspector
  leaves the cycle; closing a focused inspector returns focus to the
  composer.
- Arrow keys move the Session selection without opening it, without touching
  the draft and without moving focus; Enter opens it and restores that
  Session's draft.
- With 10,000 Sessions and 10,000 blocks, at most 60 rows of each exist in
  the element tree, the newest block is present and the oldest was never
  built.
- Markdown paragraphs, a list, a table, a code block and a quote paint their
  Thai, Chinese, Japanese, Arabic and emoji text.
- An 88-character Thai paragraph with no spaces wraps onto a second line
  inside a 520 px conversation instead of overflowing.

What they do not establish: performance, OS IME behaviour, screen-reader
output, or anything on Windows or Linux. The harness dispatches synthetic
key and pointer events and reads accessibility properties; it runs no input
method and no assistive technology.

### Offscreen render

`tests/render.rs` draws the shell through GPUI's real Metal renderer with no
window shown, in seven states (1440 px light and dark, 1100 px light, 800 px
dark, the inspector overlay at 1100 px, the sidebar overlay at 800 px, and
the Tools notice), and asserts each is not a flat fill. Setting
`FORMLESS_DESKTOP_RENDER_DIR` keeps the captures as PNG files. The wide
light, medium light, narrow dark, both overlay and the Tools notice captures
were looked at once each, by the implementing agent, not by a user (the wide
dark one was not). In the dark theme the overlay backdrop's dimming is barely
visible against the dark background; that contrast needs a design pass:

- Layout, light and dark themes, the composer focus ring and the textual
  Session status all render as intended.
- Thai (with stacked marks), Chinese, Japanese, Korean, Arabic, Hebrew and
  colour emoji, including a skin-tone ZWJ sequence and a flag, all draw with
  system fallback fonts. No missing-glyph boxes were seen.
- An inline code span that starts with Arabic (`تقرير.pdf`) is laid out
  right-to-left as its own run, so it reads `pdf.` then the Arabic word.
  Whether that is the wanted behaviour for file names needs a decision.
- The first version of the narrow layout dropped the inspector over the
  conversation as soon as the window narrowed, hiding Send. The captures
  showed it; the layout now only overlays when asked.

Captures are not bit-stable between runs. Repeated captures of the same
seven states from identical code match exactly for some states and differ
for others, and which states differ changes from run to run. Every
difference seen so far lies in one 12-device-pixel-wide strip at the right
edge of the timeline, covers between a handful and about 9,000 pixels, and
is at most 2 of 255 per colour channel. That strip is where the timeline's
scrollbar is, and its fade is animated; the cause was inferred from the
position, not confirmed. It is invisible to the eye, but these captures
cannot serve as exact golden images without first pinning that animation.

This is evidence that the shell draws, not a design review.

### Build and launch

Two sets of numbers exist and they are kept apart. "Revised head" is the
committed tree: the scaffold after the early review (§9) and a final
simplification pass. "Earlier" is the first scaffold, before that review.

| Measurement | Value | Measured on |
|---|---|---|
| Release build, cold, 2 jobs, `cargo build --release --locked` | 6 min 09 s wall (638 s CPU) | Earlier (only cold build there has been; later builds were incremental) |
| Release binary | 24.8 MB, arm64 Mach-O, unstripped, default release profile | Revised head |
| Debug binary | 92.5 MB (no debug info) | Revised head |

`formless-desktop --smoke` (release) opens a real window on this Mac, draws,
and exits one second later. Time since process start and peak resident
memory, three consecutive runs each:

| Fixture | Measured on | Platform app ready | Fixture built | First frame callback | Peak RSS |
|---|---|---:|---:|---:|---:|
| 10,000 Sessions, 10,000 blocks | Revised head | 283 / 434 / 467 ms | 3 to 4 ms | 536 / 752 / 720 ms | 132 to 133 MiB |
| 10,000 Sessions, 100,000 blocks | Revised head | 593 / 446 / 412 ms | 17 ms | 829 / 760 / 704 ms | 183 to 184 MiB |
| 10,000 Sessions, 10,000 blocks | Earlier | 314 / 319 / 378 ms | 4 ms | 567 / 570 / 643 ms | 133 MiB |
| 10,000 Sessions, 100,000 blocks | Earlier | 304 / 344 / 367 ms | 16 to 18 ms | 529 / 546 / 589 ms | 184 MiB |

The very first launch after the first build took 1,571 ms with a 100-row
fixture. The spread between runs, and between batches taken minutes apart on
a machine doing other work, is larger than any difference between the
fixtures or between the two heads. These numbers say "about half a second to
a second to the first frame callback" and nothing finer; they are not a
regression signal in either direction. The launches were not watched by a
person: that a window appeared is taken from the process reaching its
first-frame callback and exiting with status 0.

"First frame callback" is GPUI's `on_next_frame`; it was not checked against
what reached the display. The stress row holds 90,000 more blocks in memory
(the fixture's strings plus the list's per-item bookkeeping); how the extra
51 MiB splits between them was not measured.

These are a handful of runs on one machine, reported as observed. They are
not the epic's performance acceptance results and no budget is met or missed
by them: there is no frame-time, key-to-paint, scroll, session-switch,
idle-CPU or long-run memory measurement yet, and no streaming fixture.
Virtualization keeping the element count low is a necessary condition for
those budgets, not evidence that they hold.

## 5. Findings that need a decision

### 5.1 No Thai word breaking in GPUI's line wrapper

`gpui-pre 0.3.12` builds its line breaker with
`icu_segmenter::LineSegmenter::new_for_non_complex_scripts` and depends on
`icu_segmenter` with only the `compiled_data` feature, so no dictionary or
LSTM model for Thai (or Lao, Khmer, Burmese) is present. Observed effects:

- Every layout of Thai text prints `ICU4X data error: No segmentation model
  for complex script: Thai` to the process output.
- Thai text still wraps and does not overflow. Measured since with real
  fonts (§11.6): between 6 and 10 of every 9 to 16 line breaks in a Thai
  paragraph fall inside a word.

Not yet examined: whether word-wise caret movement and double-click
selection in the kit's inputs have the same gap for Thai. This matters for a
Thai-writing user and is upstream of the kit. Options are an upstream
request, a Formless-owned pre-segmentation step for timeline text, or
accepting it for a first release with the limitation published.

### 5.2 The client transport is inside `formless-tui`

`DaemonClient`, `SendTracker`, reconnect and version checks live in
`crates/formless-tui/src/client.rs` and use `formless_core::Paths`. A desktop
client that reuses them as-is would depend on `formless-tui` (and with it
`ratatui` and `crossterm`) and on `formless-core`. Phase 1 needs either a
characterized `formless-client` extraction, which the epic allows only where
it removes real duplication, or a deliberate decision to depend on those
crates. The spike depends on `formless-protocol` alone to keep this open.

Related: replies are untyped. `ServerMessage::Response` carries a
`serde_json::Value`, and the knowledge of each reply's shape (for example
`GetSession` returning `messages`, `running` and `pending_events`) is spread
through the TUI's `App`. A shared typed decoding layer would be new code.

### 5.3 No transcript pagination

`Request::GetSession { id }` takes no range and the TUI reads the whole
`messages` array from one reply, bounded only by the 32 MiB frame limit
(`MAX_FRAME_BYTES`). There is no request for a window of history. The
desktop timeline can virtualize rendering (shown here with synthetic blocks)
but cannot avoid loading a whole transcript without a new protocol contract,
which is daemon work outside this epic.

## 6. Parity inventory (first pass)

From `formless-protocol` at `21aa1cb`, the TUI's view list and
`docs/keys.md`. "Data" says whether the protocol already carries what the
surface needs; it does not say the desktop client implements it. Nothing in
this table is implemented beyond what §3 lists.

| Surface | Existing requests and events | Data available | Gap or dependency |
|---|---|---|---|
| Session list | `ListSessions`, `SessionUpdated`, `SessionDeleted`, `SetSessionPinned`, `SetSessionTags`, `SetSessionArchived` | Yes: name, preview, message count, pinned, archived, tags, `last_run_status` | "Running" is not in `SessionInfo`; clients derive it from `RunStarted` / `RunFinished`. Waiting-for-decision state comes from approval and question events. |
| Session actions | `CreateSession`, `RenameSession`, `ForkSession`, `DeleteSession`, `RestoreSession`, `ExportSession`, `ImportSession`, templates, `CompareSessions`, branch summaries | Yes | Delete has an undo window (`RestoreSession`); the desktop must surface it rather than invent archive semantics. |
| Transcript | `GetSession`, `MessageAppended`, `MessageDelta`, `ReasoningDelta`, `MessageComplete`, `MessagesEdited`, `DeltasDropped`, `StreamRestarted` | Yes, whole transcript | No pagination (§5.3). `DeltasDropped` and `StreamRestarted` need explicit reconciliation in the presentation model. |
| Composer | `SendMessage` with `SendMode` `Send` / `Steer` / `Queue` and `client_msg_id`, `QueueChanged`, `EditQueued`, `MoveQueued`, `EditMessage` | Yes | Idempotent retry needs the `client_msg_id` handling now in the TUI's `SendTracker` (§5.2). |
| Run control | `InterruptRun`, `ResumeRun`, `AbandonRun`, `RetryRun`, `RewindRun`, `RateRun`, `SetStepLimit`, `SetStepMode`, `StepDecision`, `RunPaused`, `RunFinished`, `ProviderRetrying`, `ProviderStalled` | Yes | Step mode and provider retry need their own presentation. |
| Forecast | `ForecastRun`, `ConfirmForecast` | Yes | Confirmation is bound to the connection; not examined further. |
| Attachments and references | `AttachBegin` / `AttachChunk` / `AttachFinish` / `AttachAbort`, `AttachFile`, `AttachPath`, `DetachFile`, `ListFiles` | Yes | Drag and drop and image paste are native work with no evidence yet. |
| Artifacts | `GetArtifact { id, offset, length }` | Text slices only, default 200,000 characters, never a path | No binary or image delivery contract seen; "open in editor" has no path to open. |
| Run changes and diff | `RunFiles`, `RunFilesChanged`, `InspectRunReview`, `DecideReviewHunk`, `UndoReviewChange`, `CommentReviewHunk`, `RunToolChanges` | Yes: per-Run files with a text diff | Scope is one Run, which matches the epic's "never a whole-branch diff". |
| Plans, recaps, sub-agents | `PlanUpdated`, `RunRecap`, `ListSubagents`, `GetSubagent`, `SubagentStarted` / `Step` / `Finished` | Yes | None identified. |
| Questions | `UserQuestion`, `UserQuestionClosed`, `AnswerQuestion` | Yes | Must be operable before any parity claim. |
| Approvals and pending Invocations | Approval, promotion, denial and Invocation requests and events | Present in the protocol | **Not examined.** Security-control surfaces are outside this spike by instruction; they remain a required parity item. |
| Tools | `ListTools`, `GetTool`, `RunTool`, `DryRunTool`, `UpdateToolSpec`, `UpdateToolCode`, `CheckToolCode`, `PreviewToolCode`, `DiffToolVersions`, `RollbackTool`, `RetireTool`, `RestoreTool`, `SetToolDisabled` / `Pinned` / `Tags`, modules, import and export | Yes | Tool editor and hunk review are large; a read-only first pass is the realistic Phase 3 start. |
| Metrics | `RunMetrics`, `UsageByDay`, `ListToolMetrics`, `ListBuiltinStats`, `ListModelStats`, `SessionUsage`, `ContextUsage`, `RateLimits`, `StorageUsage` | Yes | Estimates versus measured values must stay distinguishable. |
| Proposals | `ListProposals`, `GenerateProposal`, `ProposalDecision`, `ProposalDiff`, `RollbackProposal`, `RunOptimiser`, `OptimiserActivity` | Yes | None identified. |
| Settings, Providers, Roles | `GetSettings`, `SetSetting`, `MergeSetting`, `SettingsChanged`, `ListProviders`, `ListModels`, `UpsertRole`, `DeleteRole`, `TestModel`, `SetSessionModel` | Yes | Provider credential entry is **not examined** (credentials are out of scope here). |
| Memory, Skills, MCP, Jobs | `ListMemories`, `UpsertMemory`, `DeleteMemory`; skill requests; MCP server, tool, prompt and catalog requests; `ListJobs`, `AddJob`, `RunJobNow`, `JobRuns` | Yes | Many surfaces; none started. |
| Search | `SearchSessions { query, limit }` returning hits | Yes, across Sessions | Find within the loaded transcript is client-side. |
| First run and setup | None found in the protocol | No | Setup is a CLI/TUI wizard; the desktop needs the epic's explicit hand-off or a shared entry point that does not exist yet. |
| Connection state | `Ping`, `Status`, `Subscribe`, protocol negotiation, `FEATURES` | Yes | Auto-start and reconnect live in the TUI client (§5.2). |
| Themes | TUI: dark, light, high contrast, colourblind | Not protocol data | The kit's `ThemeMode` is light or dark; high-contrast and colourblind palettes are Formless-owned work. |

## 7. Phase 0 checklist against the epic

| Item | State |
|---|---|
| Pin kit release family, dependency graph, toolchain and platform requirements from release manifests | Done for macOS; Windows and Linux are resolution-only |
| Build a disposable native shell on Mac | Done |
| Build it on Windows | **Not done**, needs the Windows PC. **Required gate, open.** |
| Verify kit input, virtualization, focus, Markdown and AccessKit with Thai/CJK text | Headless only, extended in §11 (overlay roles, input-method commit and composition through the kit's test bridge); no OS IME, no screen reader. **Required gate, open.** |
| Inventory UI intents and data availability; name missing contracts | First pass in §6; approvals, credentials and setup not examined |
| Original shell, composer and inspector tokens; narrow and wide mock states | Starting tokens and three width states exist in code and as offscreen captures; no visual review with users |
| Performance fixtures, baseline measurements, accepted budgets | Synthetic fixtures including a 5 MiB block and a streamed tail; first-frame timing, peak RSS and frame-delivery intervals while streaming (§11.4); no key-to-paint, scroll, idle CPU or long-run memory; no budget accepted. **Required gate, open.** |
| Decide packaging and supported platform tier | Not started |

## 8. Pending gates and next steps

Phase 0 is not complete and Phase 1 has not been started. It waits for the
independent review of this checkpoint. The gates below are all still open;
none of them can be closed by anything measured so far.

| Gate | Why the current evidence does not cover it |
|---|---|
| Windows build, tests and launch | Done separately, at `d34fab9`, on the user's Windows PC: native build, 22 model tests, 16 shell tests and fmt passed. `--smoke` timed out under a runner that started the process hidden, and the same binary then passed under a runner that hides only its console (first frame 731 ms with 10,000 Sessions and blocks; §17.6). Why the first run failed is not established; the runner comparison continues there. No Windows code change was made for it. Nobody looked at the window, and no Windows accessibility, input, scaling or streaming check has run. **Windows rendering is not accepted.** |
| Linux build and launch | Only dependency resolution was done. |
| Real OS IME composition (Thai, CJK, dead keys) | The harness types finished text, and finished Thai and Chinese characters were posted to a real window as key events (§16.4). No input method composed anything. |
| VoiceOver (and NVDA on Windows) | No screen reader ran. The real macOS window's platform accessibility tree was read and acted on (§16): that shows what is exposed, and it was not what the tests implied. What is spoken, and in what order, is unknown. The kit cannot mark an element modal, so the page behind a dialog stays in the tree. |
| A person's keyboard, pointer and trackpad | Not done. Synthetic OS-level key and wheel events were delivered to real windows in the background (§16.4). No hardware input, pointer movement, hover, drag or gesture, and the window was never frontmost. |
| 200% text scaling and display-scale changes | The shell's own text size setting goes to 200% in a window of any size. A window too small for it is reflowed, not given smaller text (§15). That is tested headlessly, checked offscreen with real fonts, reported by the process from real windows, and seen by the implementing agent in captures of a real window (§16.4). No person has looked at it, no screen reader has read it, it is not assessed against any published criterion, and no OS-level display or accessibility setting was changed. |
| Streaming and performance budgets | Frame-delivery intervals were recorded while streaming in a real window (§11.4). That is not key-to-paint, scrolling, session switching, idle CPU or long-run memory, the window was not watched, and no budget is accepted on it. |
| Thai line breaking | A study shows a workable approach on macOS fonts (§11.6). It is not wired in, and Windows and Linux fonts are untested. |
| Parity | §6 is an inventory. Only Chat has a view, with synthetic data, and the approval, credential and setup surfaces are unexamined. |
| Packaging, signing, licence notices | Not started. |

Next steps, in order:

1. Review of this checkpoint.
2. Run the same build, tests and `--smoke` on the Windows PC and record the
   result here, including any missing toolchain component.
3. Look at the shell on the Mac in person: text scaling at 200%, Thai and
   CJK IME composition in the composer, VoiceOver over the regions and the
   overlays.
4. Decide §5.1 (Thai breaking), §5.2 (client extraction) and §5.3
   (pagination dependency).
5. Add the streaming fixture (100 presentation deltas per second) and frame
   timing so the epic's budgets can be measured rather than assumed.
6. Only then start Phase 1 on the presentation model and a read-only client
   adapter.

## 9. Early review and what was done

An independent review of the scaffold while it was still in progress raised
six points. Each was checked against the code as it stood and then fixed or
scoped:

| Review point | State when checked | What changed |
|---|---|---|
| Narrow sidebar stays collapsed despite the toggle | True: below 944 px the toggle flipped a preference that could not take effect, so the Sessions list was unreachable | The toggle now opens the sidebar as a modal overlay at that width (`SidebarMode::Overlay`); covered by model and headless tests |
| Absolute inspector overlay needs a focus trap and restoration when modal | True: the overlay took no focus, trapped nothing and had no Escape or outside-click dismiss | Both overlays are modal: backdrop, focus moved in, F6 and Tab trapped, Escape and backdrop click dismiss, focus restored; covered by headless tests |
| Row ids must use a stable Session id, not an index | True: rows were `("session-row", index)` and the click handler captured the index | Row element ids come from the Session id and the click selects by Session id; timeline blocks already used their stable block id |
| Unconditional `gpui-kit` `test-support` dev-dependency defeats the no-default-features promise | Already fixed before the review arrived, and re-verified | No dev-dependency; the promise is scoped in §1 to the model and fixture tests |
| Draft cache bounded by entries only | True | Bounded by entries and by total bytes, with the newest draft always kept; limits stated in §3 |
| Navigation changes destination but keeps the conversation | True: only a status-line label changed | Other destinations replace the conversation and composer with an explicit "not built" notice; the navigation is labelled scaffold in the UI, the code and here |

## 10. Evidence

The logs and captures behind §1 and §4 are kept locally, outside the commit,
in the worktree's git-ignored `.screenshots/gpui-1753-phase0/` directory:

- `MANIFEST.md`: every command, its result, and the path of every log and
  capture, tied to the commit it describes.
- `collect.sh <worktree root> <evidence dir>`: regenerates all of it. It
  runs only this crate's commands, with two build jobs, and takes about
  four minutes on a warm target directory, most of it the streaming runs,
  each of which opens a window.
- `logs/`: environment, format, lint, the test runs with and without the
  Thai study, release build, the smoke, 5 MiB, 200% and streaming launches,
  lockfile verification, dependency graphs, resolved features, build plans
  and on-disk artifacts.
- `captures/`: the 37 offscreen PNG captures of 19 states.
- `checkpoint-c686073/`: the complete evidence set of the acceptance
  follow-up (§11), as it was.
- `checkpoint-126595a/`: the complete evidence set of the two UI defect
  fixes (§12), as it was.
- `checkpoint-5d16e93/`: the complete evidence set of the row preview
  bound (§13), as it was, with the window runs of §13.3.
- `checkpoint-c8487eb/`: the complete evidence set of the streamed-row
  change (§14), as it was, from before small windows were reflowed.
- `checkpoint-6783bf5/`: the complete evidence set of the reflowed shell
  (§15.1 to §15.4), as it was.
- `ab-bin/`: the release binaries compared in §13.3 and §14.3.
- `stream-model/`: the two replay binaries of §14.1 and their log.
- `stream-ab-837d80a-vs-c8487eb/`: the window runs of §14.3.
- `compare-captures.py`: the run-to-run pixel comparison quoted in §4.
- `checkpoint-e21668b/`: the complete evidence set of the first checkpoint,
  as it was.
- `receipt-ac004f3/`: the model and shell tests run on an exact export of
  commit `ac004f3`, with the hashes that tie the export to the commit.
- `mutation-check/`: logs of the review regressions failing with their
  fixes removed.

That directory is local to one machine and is not part of the repository;
anyone else reproduces it by running the script.

## 11. Acceptance follow-up

Work done after the first checkpoint was reviewed and published as a draft,
all local commits on top of it. Figures are from the evidence set collected
at the last code commit, on the Mac in §2.

### 11.1 Correction: the headless test window has no real fonts

The kit's headless test window (`tests/shell.rs`) runs on GPUI's stand-in
text system, in which every character is 0.6 em wide. This was found while
measuring Thai line breaks, when a zero width space measured 9.6 px.

Consequences for what has been claimed:

- Layout, focus, key dispatch, hit testing, virtualization, accessibility
  properties and all model behaviour in those tests are real.
- Anything about glyph widths, or where real text wraps, is not. The earlier
  statement that a Thai paragraph "wraps inside a 520 px conversation" is
  true of a paragraph of that many characters, not of Thai glyphs.
- Measurements that need real fonts now run in `tests/render.rs`, on the
  platform text system through GPUI's offscreen renderer. That file asserts
  it is on real font metrics (`mmmmmmmm` is at least 1.5 times as wide as
  `iiiiiiii`) so it cannot silently fall back.

### 11.2 Overlay semantics and focus restoration

- An overlay panel exposes the dialog role, its name ("Sessions",
  "Context") and a description that states the modal behaviour and the
  Escape key in words. Docked, the same panels are navigation and
  complementary landmarks. Exactly one dialog is exposed while an overlay
  is up, none otherwise.
- GPUI Kit 0.9.1 has no "modal" accessibility property on elements (its own
  dialog sets only role and label), so that flag is not set. Whether an
  assistive technology treats the content behind the dialog as unreachable
  is therefore not established.
- Dismissing an overlay returns focus to the exact control that had it: a
  header toggle reached with Tab and activated with Enter, a button inside
  a region, or the composer. If that control's region is no longer shown,
  focus goes to the composer, or to the conversation where there is no
  composer.

Tested by reading the accessibility tree and dispatching synthetic keys.
**Not observed:** VoiceOver or any other screen reader.

### 11.3 Memory policy

One stated position per place the shell holds user text (`src/memory.rs`):

| Text | Position |
|---|---|
| Accepted composer text | At most 1 MiB. |
| Stored drafts | At most 64 Sessions, and at most 4 MiB in total provided no single draft exceeds 4 MiB. The shell only stores drafts within the composer limit, so the precondition holds. |
| Recorded intent | No message text at all: kind, Session id, byte and character counts. |
| A timeline row | At most 32 KiB and 400 lines laid out, whatever the block's size. |
| Composer undo and redo history | **No bound is claimed.** What the kit retains was not measured and is excluded from every figure here. |
| The transcript | Not a view policy. How much is loaded is a protocol question (§5.3). |

How the composer limit is kept, and where it bends:

- An edit whose result is over the limit is rejected whole: the full
  previous draft and its selection come back. Nothing is cut to fit, so
  text after an insertion point is never lost and no character is split.
- The kit has no hook to refuse a multi-line edit before it happens (its
  validator applies to single-line inputs only), so the rejection is made
  just after the edit. The composer is over the limit for that instant.
- The rejection uses the kit's own undo, sent to that composer. When that
  restores exactly the previous draft the undo history survives. When it
  does not (the kit merges quick typing into one undo step and takes back
  more), the previous draft is restored by rebuilding the composer and the
  history is lost. The notice says which happened, every time.
- A clipboard larger than the whole limit is refused before insertion. Any
  smaller paste is judged on the text that results, so replacing a
  selection is accounted for exactly.
- Text an input method is still composing is not judged until the
  composition ends, so the composer can exceed the limit by the length of
  a composition.
- When a composition ends in place the kit gives no signal at all. The
  excess is judged at the composer's next change of any kind; until then it
  can be neither sent nor stored as a draft.
- A rejection is deferred work. It is tied by a ticket to one edit in one
  composer. A rejection that outlives its composer, because the Session was
  switched in between, sends no undo, restores no text and consumes no
  newer record.
- Switching Session while the composer holds text that was not accepted
  (a pending rejection, or an over-limit composition) stores the last
  accepted draft and says the rest was not kept.
- The composer is rebuilt on every Session switch, because the kit's undo
  history cannot be cleared any other way. Without that, undo in one
  Session restored another Session's draft.
- As mitigation only, the composer is also rebuilt after 4 MiB of edited
  text (bytes removed plus bytes inserted). This is accounting, not a
  measured bound.

Not covered by any test: text dropped onto the composer (no harness for
it), and anything a real input method does that the kit's test bridge does
not model.

This policy went through two independent review rounds after it was first
committed; §11.7 lists what each found.

### 11.4 Five-megabyte and streaming stress

**A 5 MiB output block.** In the headless test (debug build, stand-in text
system) the row shows a preview notice naming both sizes, lays out the
block's 390th line and not its 450th, and the whole window paints 30,841
bytes of text. Typing 30 characters into the composer took 801 ms with the block on
screen and 721 ms without it, each keystroke a full frame; the two are
within the noise of tests running in parallel. In a real window (release
build), first frame callback and peak memory:

| Fixture | First frame callback | Peak RSS |
|---|---:|---:|
| 10,000 blocks | 623 / 526 / 859 ms | 135 MiB |
| 10,000 blocks and one 5 MiB block | 551 / 570 / 501 ms | 144 MiB |

**A 5 MiB paste** into the composer is refused in about 61 ms (headless),
with nothing inserted.

**Streaming.** `--stream-seconds` streams into the tail of a 10,000-block
timeline in a real window and records the interval between display-frame
callbacks. The display runs at 120 Hz, so an undisturbed interval is 8.3 ms.

| Run | Deltas | View updates | Frames | p50 | p95 | p99 | Max | Over 16.7 ms | Over 33.4 ms |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Idle, 10 s | 0 | 0 | 1,175 | 8.3 | 8.9 | 11.7 | 33.9 | 9 | 1 |
| 100 per second, 30 s | 3,000 | 1,241 | 3,499 | 8.3 | 11.7 | 16.7 | 41.7 | 34 | 1 |
| 100 per second, 30 s | 3,000 | 1,249 | 3,516 | 8.3 | 11.7 | 16.7 | 42.9 | 27 | 1 |
| 100 per second, 30 s | 3,000 | 1,251 | 3,504 | 8.3 | 11.8 | 16.7 | 33.3 | 37 | 0 |
| 1,000 per second, 10 s | 10,012 | 419 | 1,174 | 8.3 | 11.2 | 16.7 | 41.4 | 8 | 1 |
| 100 per second, 10 s, with a 5 MiB block | 1,000 | 424 | 1,155 | 8.3 | 12.0 | 16.7 | 41.7 | 15 | 1 |
| 100 per second, 10 s, at 200% text | 1,000 | 422 | 1,168 | 8.3 | 11.9 | 16.7 | 33.3 | 12 | 0 |

Intervals are in milliseconds. No interval exceeded 100 ms in any run.

A second batch on identical code, collected a little later for the evidence
manifest, came out worse at 100 per second: p95 11.9 to 12.1 ms, p99 16.9
to 18.4 ms, 1.6% to 2.4% of intervals over 16.7 ms and none over 33.4 ms,
with the idle run at p99 12.2 ms and 0.8% over 16.7 ms. The two batches
differ by more than the runs inside either one. The machine was in use for
other work throughout.

What this is and is not:

- It is the cadence of frame callbacks, which shows when the main thread
  was too busy to take a frame. It is not key-to-paint latency, and what
  reached the display was not checked. Nobody watched the windows.
- Coalescing works as designed: 3,000 deltas became about 1,250 view
  updates, 10,000 became about 420.
- Between 0.8% and 2.4% of intervals were longer than 16.7 ms while
  streaming at the epic's 100 deltas per second, depending on the batch,
  against 0.8% idle. That share is known only to within a factor of two or
  three. An idle run had an interval over 33 ms, so the machine, not only
  the shell, contributes.
- The epic's budget is a p95 frame time of 16.7 ms at 60 Hz with 100 deltas
  per second. In both batches p95 was about 12 ms, which is consistent with
  it. They are not an acceptance
  of it: one machine, one fixture, no scrolling or typing during the
  stream. At 100 per second the streamed block stays a few tens of
  kilobytes at most. At 1,000 per second it passes the 32 KiB preview a few
  seconds in, after which the row no longer changes, so most of that run
  measures a tail that has stopped growing on screen.
- At the time of these runs a streamed block that outgrew the 32 KiB
  preview stopped showing its newest text, because the preview kept the
  head. That was a defect and is fixed (§12.1), and the runs were repeated
  with blocks that outgrow one window (§12.3).
- Headless tests cover the logic: 1,000 deltas in one burst cause one view
  update, the final state is shown without waiting, and a block being read
  further up does not move while the tail grows.

### 11.5 Text size to 200%

The shell has a text size setting. It sets the kit theme's font sizes,
multiplies the shell's own fixed sizes by the same factor, and applies the
layout breakpoints to the window width divided by it. A window at 200%
therefore lays out like one half as wide at 100%.

Tested, in the headless window:

- A 2880 px window at 200% has the same layout as a 1440 px window at 100%,
  every part twice the size to within a pixel. Kit buttons double too
  (54 × 32 to 108 × 64, 75 × 24 to 150 × 48).
- In the same 1440 px window at 200%, both panels give way, nothing is put
  over the conversation, every header and composer control is whole and
  inside the window, the draft and focus survive, typing still works, and
  the sidebar opens as an overlay at twice the size.
- The shortcuts step 100, 125, 150, 175, 200 and stop; reset returns to
  three panes with the draft and focus untouched.
- Changing the display scale factor leaves the logical layout unchanged.

Found at this point: the binary's 480 × 360 minimum window could not show
the composer or any header control at 200%; only the status line stayed
inside. That was a defect. It was first worked around with smaller text
(§12.2) and is now fixed by reflowing the shell at the size asked for
(§15).

Three 200% states are in the offscreen captures, drawn with real fonts. Two
were looked at once by the implementing agent: the 1440 px one reads as
intended; the 480 × 360 one showed the header clipped at the right edge and
the status line drawn over the composer's buttons, which was the defect
above. **Not observed:** any of this in a real window by a person; a macOS
display or accessibility setting; anything on Windows.

### 11.6 Thai line breaking: feasibility

A study, off by default (`thai-breaks` feature) and not wired into the
shell. `src/thai.rs` finds Thai word boundaries with ICU's dictionary
segmenter and can mark them with U+200B ZERO WIDTH SPACE. The dictionary is
in data the GPUI layer already links: no new package, and the lockfile
gains only a dependency edge.

Measured on macOS with real fonts, a 528-character Thai paragraph:

| Width | Unmarked: breaks | Unmarked: inside a word | Marked: breaks | Marked: inside a word |
|---|---:|---:|---:|---:|
| 240 px | 16 | 10 | 17 | 0 |
| 300 px | 13 | 7 | 13 | 0 |
| 420 px | 9 | 6 | 9 | 0 |

The 131 marks add 0 px to the paragraph's width.

So the approach is workable on this platform: GPUI breaks Thai mid-word
today, and breaks only between words once the boundaries are marked, with
no visible change otherwise. What it would take to use it:

- Marks must go into prose only, never into code, link targets or anything
  else where an extra character changes meaning. That needs the Markdown
  structure, not the source text.
- Marks must come out again when text is copied. A mark the author put
  between Thai letters cannot be told from an inserted one.
- The same measurement on Windows and Linux fonts.
- The alternative is upstream: have GPUI build its line breaker with the
  dictionary. That would fix every text surface at once, including the
  composer, and needs no marks.

Also recorded: word-wise caret movement in the kit's input crosses Thai
almost one character at a time (24 presses for 29 characters in 7 words).
That is the kit's word boundary logic, not line breaking, and marks in
displayed text would not change it.

### 11.7 Review rounds on the memory policy

| Round | Found | Done |
|---|---|---|
| First (on `6ca5c4e`) | An over-limit edit was "clamped" by keeping a prefix of the edited text, so an insertion in the middle of a full draft deleted the end of the draft, irreversibly, and the undo history went with it. Paste admission ignored a replaced selection. Draft eviction was silent. Undo accounting counted pastes only and was described as a bound. | `ac004f3`: whole-edit rejection restoring the full draft and selection; paste judged on the resulting text; evictions reported; accounting covers every edit and is described as accounting. |
| Second (on `ac004f3`) | Deferred work captured an old draft and applied it to whatever composer was current, so a Session switch in between could put one Session's text in another. A grouped undo could clear the notice before the fallback ran. A switch during an over-limit composition silently dropped it when nothing was evicted. The draft bound's precondition was unstated. | `3550eba`: generation guard; unconditional notice; switch notice; precondition documented. |
| Third (on the in-progress second fix) | The rejection record was consumed before it was validated, so a stale callback could erase a newer one. A switch while a rejection was pending stored whatever the composer showed. The undo was sent to whatever had focus. | Also in `3550eba`: ticketed records validated before use; the pending record's draft is what is stored; the undo is sent to the composer itself, from the same step that validates the ticket. |

Each regression was run with its fix removed and failed, then restored and
passed. The logs are with the local evidence. No acceptance is claimed on
the strength of this table: the fixes are for the reviewer to judge.

## 12. Two measured UI defects, fixed

Local commits `797cb77`, `f0ab2d4` and `126595a`, after the review of
`3550eba`. Figures are from the evidence set collected at `126595a`, on the
Mac in §2. Where this section and §11 disagree, this section is the current
state: §11.4's "known limit" about streamed blocks and §11.5's "Found" about
the minimum window describe the defects fixed here.

### 12.1 Long streamed blocks show their newest text

A row laid out only the head of its block, so a streamed block longer than
32 KiB or 400 lines stopped showing new text.

A row now shows one bounded window of its block, and the window moves. A
block being streamed is anchored to its tail. Any row that shows less than
its block says how much is missing before and after, and whether more is
arriving, and has Start, Earlier, Later and End controls above and below
the text. A reader who moves off the tail stays put while the stream goes
on; End resumes following. Paging forward shows every byte exactly once;
cuts fall on line breaks where there are any and never inside a character.
A window that starts inside a fenced code block is laid out with an
opening line for that block, for layout only (a short one that stands for
the real line, since §13).

Not addressed: a window that starts inside a table or list is laid out
without its beginning, and there is still no separate full-output viewer.

### 12.2 Every control stays reachable at any text size

Superseded by §15. What follows was a fallback, kept here as the record of
it: it gave a small window smaller text. No code path does that any more.

At 200% text in the 480 x 360 minimum window the header toggles and the
composer were off screen.

The text size in use is now the largest step, no larger than the one asked
for, that the window can hold (480 x 360 times the text size factor),
refitted on every resize. When it is smaller than the size asked for, the
status line says so first: what is shown, what was asked for, and the
window that would take. The shortcuts also ask the window to grow to fit.
GPUI can resize a window but cannot change its minimum size, so the fit is
the rule whenever the window does not grow.

Checked with real fonts in the offscreen renderer, in all 14 states: the
text size in use is the expected one, every header and composer control is
inside the window, and the composer is above the status line. Two captures
were looked at once by the implementing agent (the minimum window with 200%
asked for, and 960 x 720 at 200%); both read as intended. In a real window,
unwatched: a 480 x 360 window with 200% asked for settled at 960 x 720
showing 200%, and 700 x 500 with 150% at 720 x 540.

Not addressed:

- This is a fallback that keeps the shell usable. It is not 200% text in a
  480 x 360 window. A person who needs 200% text and cannot have a window
  of 960 x 720 gets smaller text and a notice. Showing 200% at the minimum
  size needs a layout that reflows (a stacked header, controls that wrap),
  which was not built, so the 200% gate in §8 stays open.
- The right-hand status text is still clipped in small windows.

### 12.3 Tests and stress, repeated

50 model tests (55 with the Thai study), 51 headless shell tests, 14
offscreen states. Each new regression fails with its fix removed.

Streaming in a real window, frame-callback intervals in milliseconds:

| Run | Updates | Frames | p95 | p99 | Max | Over 16.7 ms | Tail | Before window, not shown | After |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Idle, 10 s | 0 | 1,177 | 9.3 | 11.9 | 21.3 | 9 | | | |
| 100 per second, 30 s (three runs) | 1,213 to 1,226 | about 3,520 | 11.4 | 16.7 | 33.3 to 41.7 | 18 to 31 | 20,100 bytes | 0 | 0 |
| 100 per second, 90 s | 3,912 | 10,414 | 11.8 | 16.8 | 32.8 | 118 | 60,300 bytes | 27,732 | 0 |
| 1,000 per second, 10 s | 430 | 1,142 | 12.2 | 16.7 | 34.7 | 11 | 67,060 bytes | 34,571 | 0 |

In the last two runs the streamed block outgrew one window and nothing
after the window was left unshown: the newest text was the part on show.
Following the tail did not change the frame figures beyond their usual
spread. The same limits as §11.4 apply: this is frame-callback cadence on
one machine in use for other work, nobody watched, and no budget is
accepted on it.

Still open, unchanged: Windows (done elsewhere), Linux, real input methods,
screen readers, a person looking at 200% in a window, performance budgets,
parity, packaging. Phase 1 is not started.

## 13. Row preview bound, from independent review of `126595a`

Local commit `7e8a44a`. Figures are from the evidence set collected at
`5d16e93`, which differs from it only in this document.

### 13.1 The defect and the fix

Found by dot's review: a window that starts inside a fenced code block was
given a copy of that block's real opening line, and that line has no length
limit. An opening line with a megabyte of info string made a 2,000-byte
window lay out 1,050,580 bytes.

- The fence index now holds positions only: 32 bytes for each fenced
  block, nothing copied from the text.
- A window that starts inside a fenced block is given a short line that
  stands for the real one: the same indent and marker, at most 32 markers,
  and the language name if it ends within 64 bytes of the markers. A name
  that goes on past that is left out, not shortened.
- A row therefore never lays out more than 32 KiB and 100 bytes
  (`timeline::PREVIEW_MAX_BYTES`), whatever the block holds.
- The block's own text is not touched. A copy uses it and is exact.

Assessed as asked, and also fixed: an unfinished line was searched for a
line break from its start on every delta, so one line of a megabyte
arriving in 100-byte deltas cost about 5 GB of searching. The search now
resumes where the last one stopped, and a test counts the bytes looked at:
exactly the length of the text. Reading a finished line looks at up to four
bytes of indent, the run of markers, and, for a line that could be a fence,
the rest of that line once.

One more correction in the same code: a line of backticks with another
backtick after them is inline code, not an opening line.

Known limits of this:

- A block opened with more than 32 markers is closed, in the row, by a line
  of 32 or more of them, where the real one needs as many as the opening
  line. The index itself uses the real count, so the windows after it are
  still right.
- The index is not bounded separately from the text. A fenced block takes
  at least 8 bytes of text and 32 bytes of index, so a text made of nothing
  but empty fenced blocks has an index four times its size, more while the
  vector grows.
- A row that follows the tail copied its window, up to 32 KiB, on every
  delta, not once a frame. That cost is inside the streaming figures below.
  It has since been measured on its own and removed (§14).

### 13.2 Tests

56 model tests (61 with the Thai study), 52 headless shell tests, 14
offscreen states. New: an enormous opening line of each kind (a megabyte of
info string, of markers, of space before the name, of Thai after it) at
the head, at the tail, paged, and streamed in pieces; the short line's
rules at the limits, including a limit that falls inside a three-byte
character; a line in 10,000 pieces searched once; the index the same
however the text is cut into pieces; and, in the shell, the row for such a
block. With each of the three changes removed the matching tests fail, and
the reviewer's own test passes unchanged against the fix
(`mutation-check/fence-bound-fixes-removed.log`).

### 13.3 Stress, repeated, on a busier machine

Frame-callback intervals in milliseconds, as in §12.3:

| Run | Updates | Frames | p95 | p99 | Max | Over 16.7 ms | Tail | Before window, not shown | After |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Idle, 10 s | 0 | 1,170 | 8.9 | 20.6 | 23.3 | 16 | | | |
| 100 per second, 30 s (three runs) | 1,300 to 1,313 | about 3,390 | 12.5 to 16.6 | 17.3 to 20.8 | 31.7 to 41.7 | 72 to 101 | 20,100 bytes | 0 | 0 |
| 100 per second, 90 s | 3,920 | 10,204 | 16.6 | 17.5 | 36.0 | 191 | 60,300 bytes | 27,732 | 0 |
| 1,000 per second, 10 s | 432 | 1,106 | 16.6 | 18.5 | 33.3 | 22 | 67,006 bytes | 34,571 | 0 |

These are worse than §12.3: three to four times as many intervals over
16.7 ms, and the idle window is worse too. The machine was busier (load
average about 7.5, the window server near one full core, other agents at
work). To look for a regression, the release binary of `126595a` was
built from an export of that commit and run in turn with this one, eight
30-second runs at 100 per second in the order old, new, new, old, old, new,
new, old:

| Binary | p95 | p99 | Max | Over 16.7 ms |
|---|---:|---:|---:|---:|
| `126595a` (four runs) | 16.6 to 16.7 | 17.6 to 20.7 | 33.3 to 75.4 | 99 to 115 |
| `5d16e93` (four runs) | 16.7 | 18.2 to 20.8 | 29.5 to 59.0 | 106 to 142 |

The two ranges overlap. That is inconclusive. It does not show that the
slowdown is only the machine, and it does not show that this change costs
nothing: eight runs under uncontrolled load could hide a real difference in
either direction. What it does show is that these frame figures move by a
factor of three or more with what else the machine is doing, so neither
this table nor the one in §12.3 measures the code. No budget is accepted on
any of this.

In every run the streamed block's newest text was on show (nothing after
the window unshown).

Still open, unchanged: Windows (done elsewhere), Linux, real input methods,
screen readers, a person looking at 200% in a window, 200% text at the
minimum window size (§12.2), performance budgets, parity, packaging.
Phase 1 is not started.

## 14. Streamed-row rebuilding, measured and coalesced

Local commits `837d80a` (counting and a replay harness; behaviour
unchanged) and `c8487eb` (the change). Evidence collected at `c8487eb`.

### 14.1 Measured first

`837d80a` counts, for the streamed block, the deltas appended, the times
its row was rebuilt and the bytes copied to do it, and adds
`examples/stream_model.rs`: the synthetic stream (`fixture::stream_delta`,
the same text the binary sends) replayed into the model on a simulated
clock, with no window and no GPUI in the build. The view is brought up to
date as the shell does it, one flush 16 ms after the first delta not yet
shown. The replay source is byte-identical in the two commits. Each build
was run in turn, five rounds of 200 trials. The counts are exact and the
same in every round; the times are the range of the five rounds' medians,
for the model's work alone.

| Stream | Deltas | Rebuilds, every delta | when shown | Copied, every delta | when shown | Whole stream, every delta | when shown |
|---|---:|---:|---:|---:|---:|---:|---:|
| 100 per second, 30 s | 3,000 | 3,000 | 1,500 | 30.2 MB | 15.1 MB | 2.83 to 2.85 ms | 1.41 to 1.44 ms |
| 100 per second, 90 s | 9,000 | 9,000 | 4,500 | 214 MB | 107 MB | 19.5 to 19.7 ms | 9.7 to 9.8 ms |
| 1,000 per second, 10 s | 10,000 | 10,000 | 625 | 247 MB | 15.4 MB | 22.5 to 22.6 ms | 1.48 to 1.49 ms |
| 1,000 per second, 60 s | 60,000 | 60,000 | 3,750 | 1.87 GB | 117 MB | 170 to 171 ms | 11.2 to 11.3 ms |
| 100,000 at once | 100,000 | 100,000 | 1 | 3.18 GB | 32 KB | 288 to 290 ms | 1.11 to 1.13 ms |

Both builds end every stream showing the same row and holding the same
text (the digests the replay prints are equal).

What this says: at the epic's rate the per-delta rebuilding was never a
load. It took 2.8 ms of processor time for 30 seconds of stream, about
0.01% of one core, which no frame measurement here could have shown. What
was wrong with it is that the cost followed the deltas and not the frames:
a hundred thousand deltas arriving together stopped the thread for 0.29 s
to copy 3.18 GB that nobody would see. That, and not the 100 per second
case, is why it was changed.

### 14.2 The change

Appending a delta now adds the text and extends the fence index, nothing
else. The row's window is rebuilt by `Timeline::settle`, which the shell
calls when it builds a row for a frame and when a flush measures the tail
row: once for each time the row is shown, however many deltas arrived
since.

- The block's text is complete the moment a delta arrives and is never
  behind, so a copy is exact at any time.
- A frame drawn between two flushes still shows the newest text: the row
  is settled when it is built, not only when the flush runs.
- The row stays within `PREVIEW_MAX_BYTES`, behind or not.
- Finishing or restarting a stream, and moving a window, settle first.
- A settled row shows exactly what it showed when every delta rebuilt it.

### 14.3 Tests, and the two builds in real windows

60 model tests (65 with the Thai study), 53 headless shell tests, 14
offscreen states. New: a burst of 100,000 deltas rebuilds the row once and
shows what a block given the whole text shows of its end; a row settled
every 2, 7, 17, 250 or 4,000 deltas shows, at each of those points, what
one settled after every delta shows, through a reader's six moves in the
stream; no row is left behind when a stream is finished or restarted; a
full window that is not on the tail is never rebuilt by a stream; and in
the shell, a burst between two frames is rebuilt once, the next frame shows
the newest text before the flush has run, and the flush rebuilds nothing.
With each of five parts of the change removed, tests fail
(`mutation-check/coalesced-rebuild-removed.log`).

The two release binaries in turn, in real windows, unwatched, on a machine
in use for other work (load average 4 to 6):

| Stream | Build | Rebuilds | Copied | Intervals over 16.7 ms | Processor time |
|---|---|---:|---:|---:|---:|
| 100 per second, 30 s (four runs each) | every delta | 3,000 to 3,001 | 30.2 MB | 106 to 148 | 9.2 to 9.7 s |
| | when shown | 2,159 to 2,214 | 21.6 to 22.5 MB | 121 to 133 | 9.3 to 9.6 s |
| 1,000 per second, 10 s (two runs each) | every delta | 10,002 to 10,009 | 247 MB | 32 to 37 | 3.8 s |
| | when shown | 708 to 718 | 17.3 to 17.5 MB | 35 to 36 | 3.8 s |

The counts moved as designed. The frame figures and the processor time of
the two builds cannot be told apart in these runs, which is what §14.1
predicts and is not evidence of anything more.

In a window the saving at 100 per second is a quarter, not the half the
replay shows. These runs draw a frame at the display's rate throughout, to
time the frames, and the row is rebuilt for each frame drawn after a new
delta; at 100 deltas a second on a 120 Hz display most deltas get a frame
of their own. Rebuilds are bounded by frames, as intended, and at this rate
frames are nearly as many as deltas.

The stress runs of §13.3, repeated at `c8487eb`:

| Run | Updates | Frames | p95 | p99 | Max | Over 16.7 ms | Tail | Before window, not shown | After | Rebuilds |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Idle, 10 s | 0 | 1,168 | 9.0 | 20.8 | 22.7 | 17 | | | | 0 |
| 100 per second, 30 s (three runs) | 1,246 to 1,249 | about 3,300 | 16.7 | 20.8 to 20.9 | 36.5 to 50.0 | 129 to 149 | 20,100 or 20,104 bytes | 0 | 0 | 2,162 to 2,191 |
| 100 per second, 90 s | 3,712 | 9,954 | 16.7 | 20.8 | 41.7 | 349 | 60,300 bytes | 27,732 | 0 | 6,359 |
| 1,000 per second, 10 s | 414 | 1,100 | 16.7 | 20.8 | 33.3 | 47 | 67,056 bytes | 34,571 | 0 | 692 |

### 14.4 Limits

- The frame figures of this batch are the worst of the three batches
  (§12.3, §13.3, this one), and the idle window is as bad as in §13.3.
  Nothing here says why. The comparisons in §13.3 and above put each pair
  of builds within each other's range on the day, which is inconclusive,
  and the batches were taken on different days under different load. A
  regression somewhere in this series is not excluded. It would take a
  machine doing nothing else, or a frame-time measurement that does not go
  through the window server, to say. No budget is accepted.
- The replay times the model only, on one machine, in a release build. It
  does not time what the kit then does with each rebuilt row: parsing up
  to 32 KiB of markdown and laying it out, which is certainly much more
  work and was neither measured nor changed. The kit does that off the main
  thread; the shell test waits for it.
- The replay's clock gives two deltas a flush at 100 per second. The real
  windows gave about 2.4.
- While a row is behind, what it would lay out is older than the block's
  text, by at most the time to the next frame or flush. The bytes not yet
  taken in count as not shown after the window.

Still open, unchanged: Windows (done elsewhere), Linux, real input methods,
screen readers, a person looking at 200% in a window, 200% text at the
minimum window size (§12.2), performance budgets, parity, packaging.
Phase 1 is not started.

## 15. 200% text in the minimum window: the shell reflows

Local commit `6783bf5`. Evidence collected at it.

### 15.1 Why §12.2 was not enough

A window smaller than a text size needs was given smaller text and a
notice. That kept the controls on screen. It was not 200% text: a person
who needs 200% and has a 480 x 360 window got 100%. That fallback is gone.
The text is the size asked for in a window of any size, and what gives way
is the layout.

There is no layout that shows everything at once. 480 x 360 at 200% is the
area of 240 x 180 at 100%, about five lines of body text tall. At 200% the
header, the composer and the status line need 550 px between them, in a
window 360 px tall. So some of the shell is off screen at any moment, and
the work is in how it is reached.

### 15.2 The layout

A window at least 480 x 360 times the text size factor is whole: nothing
changes there. A smaller one is reflowed:

- Each part is as wide as the window and wraps inside it. The header's
  title takes a line of its own and its controls wrap under it. The status
  line's three parts go one under the other. The composer's actions wrap.
  Nothing is cut off at the side and the shell never scrolls sideways.
- The parts stack, header, conversation, composer, status line, and the
  shell scrolls up and down as one page with a scrollbar. The conversation
  is one screen tall at most, so the composer is never more than one screen
  below its top.
- Focus brings its part into view: when focus moves, when the window is
  resized or the text size changes, when the user types in a composer they
  had scrolled away from, and when a part is asked for by name (F6).
  Otherwise the page stays where it was scrolled.
- The status line holds no control, so on a reflowed page it is a tab stop
  itself. Without that the keyboard could not bring it into view.
- The kit's timeline keeps every wheel event that reaches it, even at its
  end. On a page that made it a trap: with the pointer over the timeline
  the page could not be moved. The wheel is now handed on to the page once
  the timeline is at the end the wheel is turning towards, and only when
  the timeline is what the pointer can reach (§15.5).
- An overlay panel covers the window and is never wider than it. The
  inspector's details scroll inside it, by the wheel and by the arrow and
  page keys.
- A reflowed window never docks a panel, however wide it is.

### 15.3 What was checked

Headless, in the kit's test window (no real fonts): 60 shell tests at this
commit (63 since §15.5) and 61 model tests (66 with the Thai study). The
ones for this:

- The text stays 200% from 480 x 360 up through every window size to one
  that holds it and back, through a wide-and-short and a narrow-and-tall
  window; controls are as tall reflowed as they are where the shell is
  whole; nothing is cut off at the side, the parts do not overlap, the end
  of the page can be scrolled to; the draft and focus are untouched and
  the composer is in view at every step.
- Every part is reached from the keyboard: Tab to each header control and
  to the status line, F6 round the regions, each in view when it has focus.
- The page stays where the wheel left it until typing, a move of focus
  (even within the part that had it) or asking for a part brings it back.
- The wheel over the timeline scrolls the timeline first, then the page;
  a timeline with nothing to scroll hands it on both ways.
- Overlay panels are exactly the window; the Sessions list and its
  destinations are inside; the inspector's last detail is reached with the
  page keys; Escape returns focus to the composer, in view.
- A wide but short window opens its panels as overlays and docks them
  again when it is tall enough.
- A destination with no view is reflowed too, and its way back is in view.
- A stream into a reflowed window shows its newest text.
- Fifty-six rounds of text size shortcuts and resizes, many of them
  reflowed, leave the draft whole and the composer focused and in view.

Offscreen, through the real renderer with real fonts (`tests/render.rs`):
19 states, 8 of them reflowed, 37 captures. For each reflowed state: the
font and rem size are the ones asked for; controls are exactly as tall as
in the whole 200% state; no part or control is outside the window's width;
the parts stack without overlapping; the header's controls do not overlap
each other; focus in each of the four parts brings it wholly into the
window (and the shell asks for the second frame that takes); the wheel
reaches the end of the page; an overlay panel is inside the window.

With each of eleven parts of the change removed, tests fail
(`mutation-check/reflow-parts-removed.log`). Two of the eleven were not
caught by the shell tests at first; a test was added for each.

In real windows, opened small and left small (`--keep-window`). Nobody
watched; these are the figures the process printed about its own layout
after moving focus to each part in turn:

| Window | Text | Page height | Scroll range | Header, conversation, composer, status line brought wholly into the window by focus |
|---|---:|---:|---:|---|
| 480 x 360 | 200% | 910 | 550 | yes, all four |
| 480 x 360 | 150% | 718 | 358 | yes, all four |
| 700 x 500 | 200% | 951 | 451 | yes, all four |
| 1440 x 500 | 200% | 951 | 451 | yes, all four |
| 480 x 360, with a 5 MiB block | 200% | 910 | 550 | yes, all four |

In each, every part was laid out from the window's left edge to its right
edge. In 480 x 360 at 200% the header is 149 px tall, the conversation 360,
the composer 227 and the status line 174. With the conversation in view the
timeline has about 280 px; with the composer in view, 133 px of timeline
show above it. Streaming for 10 s at 100 deltas a second into that window
ran and ended showing the newest text.

Four of the captures were looked at once by the implementing agent at this
commit (the header, the composer and the status line of the minimum window
at 200%, and its Sessions overlay). They read as intended.

### 15.4 Limits

- No person has seen this in a real window. (When this was written no
  picture of a real window had been taken either. Captures of the real
  window were taken later, through a tool that already had the
  permission: §16.4.)
- No screen reader ran. What it announces for a page whose parts are
  stacked, and for a status line that is a tab stop, is unknown. A
  focusable line of status text is a compromise made for the keyboard.
- This is not a conformance claim. The minimum window at 200% is smaller
  than the 320 x 256 points that WCAG 1.4.10 asks content to reflow at,
  and nothing here was assessed against that or any other criterion. OS
  text size and zoom settings were not tried.
- Bringing a part into view takes two frames after a layout change: GPUI
  scrolls to where a child was the frame before. For one frame the page is
  at its old position.
- Focus brings a part into view, not the control inside it. A part taller
  than the window shows its top. The only such part found is the notice
  for a destination with no view, and its button was moved to the top for
  that reason.
- The Sessions overlay in 480 x 360 at 200% shows less than two rows of
  the list above the destination buttons. It scrolls and the arrow keys
  work.
- The header's title is cut short with an ellipsis in a narrow window. The
  whole title is in the Context panel and the Sessions list.
- Handing the wheel on depends on what the kit reports about its timeline
  and on the order GPUI calls listeners in. Both are tested at the pinned
  versions; a kit update could change either.
- Markdown wider than the window inside a row (a table, a long line of
  code) was not looked at. Scrolling the timeline itself from the keyboard
  is the kit's and is untested, as before.
- Windows and Linux were not run.

### 15.5 The wheel over a modal panel, from independent review of `61e1958`

Local commit `055a93d`. Evidence collected at it.

Found by dot's review: on a reflowed page, with the conversation in view
and the Context panel open over it, a wheel turned over the panel moved
the page behind the panel, from 213 px to 313 px. The listener that hands
the wheel on from the timeline went by the timeline's bounds, and the
timeline was still there, under the panel. Having moved the page it
stopped the event, so the panel did not scroll either.

The listener now has a hitbox of its own and acts only when that hitbox
is in the window's hit test for the pointer. A modal overlay blocks the
pointer for everything under it, and so does anything else drawn in
front, so the wheel is then left alone and goes to what is under the
pointer.

Tests, 63 shell tests in all: the review's own test, as written; under
Context and under Sessions, with the page at each of three parts, the
page stays exactly where it was at the first turn of the wheel, at the
panel's end and past it, and back past its start, while the panel itself
scrolls by what the wheel turned; the same over the strip of backdrop
beside a panel that is narrower than the window, where nothing moves at
all; and with nothing in front, the timeline hands the wheel on as
before. With the fix removed, the review's test and both new ones fail
(`mutation-check/modal-wheel-isolation-removed.log`).

Not done: no wheel was turned in a real window. These are synthetic wheel
events in the test window, dispatched the way the platform's are.

The 200% gate in §8 stays open until a person, and a screen reader, have
been at it. Still open as well, unchanged: Windows (done elsewhere), Linux,
real input methods, performance budgets, parity, packaging. Phase 1 is not
started.

## 16. A real macOS window: what assistive technology is given, and real events

Local commits `87e02f2` and `c70f4b0` (options for inspecting a window) and
`63753dd` (the fix). The raw material is in the evidence directory under
`macos-real-window/`, with its own notes.

### 16.1 How, and what kind of evidence it is

Until now every statement here about accessibility came from the kit's
test window, and every statement about input from events the test harness
dispatches. This section is about real windows of the release binary on
macOS, and it uses four kinds of evidence that should not be confused:

- **The platform accessibility tree.** The window's own NSAccessibility
  elements, walked from inside the shell's process (`--dump-platform-a11y`,
  behind the off-by-default `ax-dump` feature), and the same window read
  from outside through the macOS accessibility API by an automation
  driver. This is what the platform exposes to any assistive technology.
  It is not what a screen reader says.
- **GPUI's tree.** What GPUI hands the platform's adapter (`--dump-a11y`).
- **Accessibility actions.** `AXPress` sent to an element, the way
  assistive technology activates a control.
- **Synthetic OS-level events.** Key and wheel events posted to the
  process and delivered through AppKit's ordinary path to a window that
  was not brought to the front. They are not a person's keyboard or
  trackpad, no input method ran, and no pointer moved.

The driver (cua-driver 0.38.0) was already installed and already had
Accessibility and Screen Recording permission. No permission or setting
was changed and VoiceOver was not started. Two things happened that were
not intended: the driver's `doctor` command contacted GitHub to look for
an update, and one read-only `osascript` query to System Events hung for
two minutes and may have raised a consent prompt, which was not answered.
Neither was repeated.

### 16.2 What the real window exposed

A 1440 x 900 window, read at three layers at once:

| Layer | What it held |
|---|---|
| GPUI's tree | 16 nodes: the window; the buttons Sessions, Context, Dark; a navigation landmark with the six destination buttons; the timeline as an empty log; the Message input; Stop; Send; an empty complementary landmark |
| Platform tree, in process | The same 16, as landmark groups, buttons and a text area; Stop and Send disabled |
| Driver, from outside | 14 of them. It leaves group elements out of its listing (it visited 22 nodes), and lists the disabled buttons without an index |

Two different things were missing, for two different reasons:

- **The landmarks were missing only from the driver's listing.** The
  platform adapter exposes them; the in-process walk shows them. That is
  the driver's filtering and not a defect of the shell.
- **The text was missing at the source.** No Session in the list, no
  message in the timeline, no Context detail, no status line, no title: none
  of it was in GPUI's own tree, so nothing after it could have it. In the
  pinned GPUI an element gets a node only if it has both an id and a
  role. A plain string, the kit's `Label` and its markdown view have
  neither.

The second is a real defect, and the tests had not shown it: they read
roles and names from the kit's test tree, which answers for the elements
asked about and says nothing of what is absent. On a reflowed page there
was a third: keyboard focus on the status line, or on the conversation,
was on nothing the platform could name.

A window's first read returns only its title-bar buttons. The adapter
starts when it is first asked, and the content is there from the second
read.

### 16.3 The fix

- Text the shell draws is a static-text node whose value is the text: the
  header's profile, the Sessions count, the block count and Run state, each
  Context detail as "name: value", the three status lines, a composer
  notice.
- The header's title is a heading named by the whole title, even where the
  window cuts it short.
- The Sessions list is a list box. Each row is an option named by its
  title and read with its status, with its selected state and its place
  among all the Sessions, in the tree GPUI hands the adapter. Not all of
  that reaches macOS: see §16.6.
- The conversation is the main landmark. Each block on screen is an
  article named by who it is from, holding the text its row lays out.
- The status line is a status region, so focus on it has a name.

In the real window afterwards the platform tree has 62 elements where it
had 18, a Session opens when its row is pressed through the accessibility
API, and focus on the status line and on the conversation is reported.

Headless: 66 shell tests. The three new ones check roles as well as
values, because a value on an element with no role is in the test tree and
not in the platform's; one mutation got past the first version of them for
exactly that reason (`mutation-check/accessibility-nodes-removed.log`).

### 16.4 Real events, in the background

All through the accessibility API or as synthetic OS-level events, to a
window that stayed 11th of 12 from the front.

| Window | Event | What followed |
|---|---|---|
| 1440 x 900 | `AXPress` on Context | The Context landmark left the platform tree |
| | 20 characters as key events: Thai, Latin, Chinese, an emoji joined by zero-width joiners | The Message field's platform value was exactly that text; Send became enabled |
| | `AXPress` on Send | Draft kept; the status line in the capture reads "Recorded Send (20 characters), not sent" |
| | Cmd-B twice | The Sessions landmark left the tree and came back |
| | F6 | Platform focus moved to the Sessions landmark |
| | Wheel over the timeline | Only the timeline's part of the window changed |
| | `AXPress` on a Session's row (after the fix) | The heading, the open row and the Context details changed to that Session |
| 480 x 360 at 200% | Cmd-B | Sessions came up as a dialog, titled, with its help text, focused |
| | Wheel over the open dialog, four times, then Escape | The dialog's list scrolled; the page's pixels after closing are identical to before opening |
| | Tab from the composer | The status line came into view |
| | F6 | The conversation came into view |
| | Wheel over the timeline at its end | The page moved to its end |
| | Shift-Tab | Focus on Dark, reported by the platform; the header came into view |

The implementing agent looked at the real-window captures of the wide
window after Send and of the reflowed window at each of those steps. They
are the first pictures of a real window in this work, and they match the
offscreen captures.

### 16.5 Not done, and what would settle it

- **A screen reader.** VoiceOver was not started: it changes how the Mac
  behaves for whoever is using it, and scripting it needs a VoiceOver
  setting that was not touched. Nothing here says what is spoken, in what
  order, or whether a status change is noticed.
- **A person's input.** No hardware keyboard, pointer, hover, drag or
  trackpad gesture, and the window was never frontmost, so activation, the
  menu bar and system shortcuts are untried.
- **An input method.** The Thai and Chinese were finished characters.
- **What the kit owns.** A block's text is its markdown source in one
  node: headings, lists, links and code inside it have no structure of
  their own. A cut block's node holds its bounded window. Rows away from
  the viewport do not exist as elements, in the Sessions list or the
  timeline.
- **Live regions.** Nothing is marked as one. A new block, a status change
  and a refusal notice are in the tree and are not pushed.
- **The page behind a dialog** is still in the tree.
- **Windows and Linux.** Not run.

Next checks, in order of use: a person with VoiceOver reading the window
top to bottom, moving by landmark and heading, opening a Session, hearing
a streamed block and a refusal; the same in the 480 x 360 window at 200%
with a dialog open; a real keyboard and trackpad pass with the window
frontmost; a Thai or Japanese input method composing into the composer;
Accessibility Inspector's audit.

Still open, unchanged otherwise: Windows (done elsewhere), Linux,
performance budgets, parity, packaging. Phase 1 is not started.

### 16.6 What macOS is given for a Session option

Independent review of `df0feea` pointed out that the platform tree shows
each Session option as `AXStaticText`. Checked against the pinned adapter
source (`accesskit_macos` 0.26.3) and against a walk of a real window with
the extended diagnostic (`6e968ce`; evidence in
`macos-real-window/attrs-wide/`, binary `formless-desktop-ax-dump-attrs`):

| Property | In GPUI's tree | What macOS is given | How known |
|---|---|---|---|
| Role | list box option | `AXStaticText`, role description "text" | Adapter maps `ListBoxOption` to static text; seen in the walk |
| Name and value | title; title with status | `AXTitle` and `AXValue` | Seen in the walk |
| Selected | yes, on the open Session | `isAccessibilitySelected` true on that one row, false on the 17 others; the list's selected rows and selected children are 1 | Seen in the walk; the adapter allows it because an option is item-like and has a selected state |
| Actions | click | press, pick, set selected | Seen in the walk; press opened a Session (§16.4) |
| Position and set size | 1 to 18, of 196 | **Not given at all.** The list reports 18 rows, the ones that exist as elements, not 196 | The pinned macOS adapter has no reference to position or set size anywhere; the Windows and Linux adapters do |

So on macOS a Session row is a selectable, pressable piece of text inside a
list, not a list row, and nothing tells assistive technology how many
Sessions there are or where a row stands among them. Whether VoiceOver
announces it as a row, reads the selected state, or lets the list be
navigated row by row is not known: no screen reader ran. "Pressed through
the accessibility API" in §16.4 is an `AXPress` action, which is how
assistive technology activates a control; it says nothing of what a person
using VoiceOver would hear or do. Ways to close this, none tried: a
different role the macOS adapter maps to a row, a count in the list's
name, or a change in the adapter.

## 17. Windows: no first frame from `--smoke`

Superseded in part by §17.6: the same binary has since passed under a
different runner. §17.1 to §17.5 are kept as written.

### 17.1 What is known

From the separate Windows run at `d34fab9` (evidence on that PC,
`SUMMARY.txt`): the build, 22 model tests, 16 shell tests and fmt passed.
`formless-desktop --smoke` did not print its `smoke:` line within 60
seconds, and the runner stopped the process tree. Its stderr held only two
lines, both

```
ICU4X data error: No segmentation model for complex script: Thai
```

Nothing below was run on Windows. It is read from the source of the pinned
crates and from this crate at `d34fab9`, and separates what that source
proves from what it only suggests. The native debugging is being done
separately.

### 17.2 Proven from source

- **When the `smoke:` line is printed.** At `d34fab9` it is printed from a
  next-frame callback, registered after the window is open and after
  `cx.activate(true)`; nothing else is printed before it. In the pinned
  GPUI, next-frame callbacks run at the start of a platform frame request.
  On Windows a frame request happens only when `WM_PAINT` reaches the
  window procedure's draw path; the window's `schedule_frame` does
  nothing there, and the only source of `WM_PAINT` while idle is a
  `VSyncProvider` thread that invalidates every window after each vsync
  (`DwmFlush`, falling back to sleeping one interval when that returns at
  once). Windows sends `WM_PAINT` only to a window that is visible. So no
  line means one of: the process never got as far as registering the
  callback, or after it did, no `WM_PAINT` reached the draw path, or the
  first frame request after it never finished.
- **Frames are not throttled away.** A window that is not active is
  limited to one frame per 33 ms by default; that cannot account for 60 s.
- **The process did lay out Thai text.** The ICU4X line comes from GPUI's
  own line wrapper, which breaks lines with
  `LineSegmenter::new_for_non_complex_scripts` and so has no Thai model by
  design: it reports that each time it is asked to wrap Thai. The fixture
  is full of Thai. Text is wrapped only while a window is laid out, so at
  least one layout of the window ran.
- **The Windows binary had debug assertions on.** ICU4X prints that line
  with `eprintln!` only when its `logging` feature is off (it is, in this
  graph) and `debug_assertions` are on; otherwise it says nothing. Every
  macOS `--smoke` log here is from a release build and has none of these
  lines. The Windows run was therefore of a debug build, or a release
  profile with debug assertions, so its timings would not be comparable
  with any macOS figure here.
- **Nothing is logged.** This crate installs no logger, so GPUI's own
  `log::debug!` and `log::warn!` messages, including those about deferred
  draws and device loss, went nowhere. A panic would have been printed;
  none was.

### 17.3 Hypotheses, not proven

In rough order of how well they fit:

1. **No `WM_PAINT` reached the window.** For example because the process
   ran where its window is never shown or composited: a service or
   scheduled task, a remote session that was disconnected, or a desktop
   that was locked. Fits a process that lays out but never paints. Told
   apart by running the same binary from a console on the unlocked
   desktop, and by whether a window appears.
2. **The first frame request after the callback never returned.** For
   example presentation blocking in DirectX or the compositor in such a
   session. A stack of the main thread and of `VSyncProvider` after ten
   seconds would show it.
3. **A slow debug build with the large fixture.** `--smoke` defaults to
   10,000 Sessions and 10,000 blocks. Only rows near the viewport are laid
   out, which makes 60 s unlikely, but no debug figure exists. Told apart
   by a release build, and by `--sessions 20 --blocks 40`.
4. **Text shaping on Windows.** DirectWrite font fallback for Thai, CJK,
   emoji, Arabic and Hebrew on a first layout. Unmeasured. Told apart by a
   fixture without those scripts, which does not exist yet.

The ICU4X lines themselves are not a cause: they are printed on macOS too
by any debug build that lays out Thai (the shell and render tests show
them), where everything works.

### 17.4 What would settle it, for the Windows debugging

1. Say whether the binary was a debug or a release build.
2. Run the release binary with `--smoke` from a console on the logged-in,
   unlocked desktop, and again with `--sessions 20 --blocks 40`.
3. If it still hangs, take stacks of the main thread and of the
   `VSyncProvider` thread after ten seconds.
4. Note whether a window appeared, and whether it was minimised.

A later build of this crate could print a line when the window opens and
when the first frame request runs, and install a logger for GPUI's own
messages. That needs a build, which is waiting until there is disk space.

### 17.5 Thai

This is the same gap as §5.1 and §11.6, seen from another side. GPUI's
line wrapper asks ICU4X for breaks with a segmenter that has no Thai
model, so it finds none inside a run of Thai, and wraps Thai only where it
must, inside words. The study in §11.6 gives the same segmenter family a
Thai dictionary and puts break opportunities between words; it is not
wired in. Nothing about the Windows run changes that study, and nothing in
it suggests the study's approach would affect the hang.

### 17.6 Update: the same binary passes under another runner

From the separate Windows work, not run here: a native debug worker ran
the same `d34fab9` binary with `--smoke` and it passed, exit 0, with
10,000 Sessions and 10,000 blocks: app ready 334 ms, fixture 12 ms, first
frame 731 ms. The two ICU4X Thai lines were printed on that successful
run too. The runner differed:

| | Runner | Result |
|---|---|---|
| First run | `Start-Process` with a hidden window style, which hides the GUI window | No first-frame line in 60 s |
| Later run | `ProcessStartInfo` with `UseShellExecute=false`, `CreateNoWindow=true`, which hides only a console; private profile; 30 s bound | Passed, first frame 731 ms |

What follows from that, and what does not:

- **The Thai lines are not the cause.** They appear on the passing run.
  This settles what §17.3 already argued from source.
- **The code is not shown to hang.** The same binary drew its first frame.
- **Hypothesis 1 in §17.3 fits and is not proven.** A window started
  hidden is not visible, Windows does not send `WM_PAINT` to a window that
  is not visible, and on Windows that is the only way the first-frame line
  can print (§17.2). That would explain the first run. It has not been
  confirmed: the controlled comparison of the two runners on the same
  binary is still going on there.
- **Debug build.** The ICU4X lines print only with debug assertions on
  (ICU4X's `logging` feature is off in this graph for the Windows target
  as well, checked with `cargo tree --target x86_64-pc-windows-msvc`), so
  the 731 ms figure is presumably from a build with debug assertions. It
  is not comparable with the macOS release figures.
- **No code change.** Nothing in this crate was changed for the timeout,
  and none is proposed. If it turns out the binary must cope with being
  started hidden, that is a decision for after the comparison.

The `--background` option added in §16 does not bear on this: it only
leaves the application inactive, and the window is still shown.

Still open, unchanged: Windows rendering accepted by a person (and every
Windows check after launch), Linux, a screen reader, a person's input,
performance budgets, parity, packaging. Phase 1 is not started.
