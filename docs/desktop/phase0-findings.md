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
  Thai, CJK, emoji and right-to-left text.
- Three findings need a decision before Phase 1: there is no Thai word-break
  model in GPUI's line wrapper, the client transport lives inside
  `formless-tui`, and the protocol has no transcript pagination.
- Still required and not inferable from anything here: the Windows build and
  run, real IME composition, a screen-reader pass and performance against
  the epic's budgets. A synthetic fixture on one Mac stands in for none of
  them.
- An independent early review raised six points; §9 records what was done
  about each.

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
| Composer | Multiline, Enter / Shift+Enter / Cmd-or-Ctrl+Enter, Send / Steer / Queue by Run state, Stop in its own fixed slot, disabled reasons, draft kept after a recorded send | `Textarea`, `Button` |
| Drafts | One unsent draft per Session, bounded to 64 Sessions and 4 MiB of text in total, oldest evicted first; the newest draft is always kept even if it alone is over the byte budget | model only |
| Focus | F6 / Shift+F6 across regions, hidden panels skipped, focus never left in a region that is no longer on screen | GPUI focus handles |
| Destinations | Chat is the only view. Sessions, Settings, Tools, Metrics and Proposals replace the conversation and composer with a "not built" notice and say so in the status line. Search is not reachable. | `Label`, `Button` |
| Theme | Light and dark through the kit theme (the header toggle itself is untested); status never by colour alone | `Theme::change` |
| Intents | `Intent::SendMessage` and `Intent::InterruptRun` recorded and shown, never sent | none |

Deliberately absent: any Daemon Core connection, real Sessions or
transcripts, the action registry and command palette, Search, every
destination's page except Chat, attachments, approvals and questions,
persistence of any preference, high-contrast and colourblind themes, density
settings, the reduced-motion setting, and packaging.

Known limits of what is there:

- The header stays outside a modal overlay on purpose, because it holds the
  toggle that opened it. Its buttons therefore remain clickable while an
  overlay is up.
- Focus restoration is per region, not per control. Opening an overlay from
  a header button returns focus to the composer, not to that button.
- The overlay's accessibility semantics (dialog role, modal state, announced
  name) are not set and no screen reader has been run.
- Panel widths are fixed; nothing is resizable.
- The timeline's jump-to-latest control comes from the kit and is untested
  here. Every Session shows the same synthetic timeline.
- The draft bound counts stored drafts only. The text in the composer itself
  is unbounded, as is the kit's undo history.

The layout tokens are the epic's starting values and have not been checked at
200% text scaling.

## 4. Measurements

All on the Mac in §2. Builds used `CARGO_BUILD_JOBS=2`, `CARGO_INCREMENTAL=0`
and no debug info, so build times are an upper bound for this machine, not a
typical developer build.

### Tests

| Command | Result |
|---|---|
| `cargo test --no-default-features --lib --locked` | 22 passed (model and fixture); no GPUI crate in its dependency graph |
| `cargo test --features test-support --locked` | 22 passed (model and fixture), 16 passed (headless shell), 7 offscreen render states passed |
| `cargo fmt --check`, `cargo clippy --features test-support --all-targets --locked` | clean |

What the headless shell tests establish, on macOS, in the kit's test window:

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
- Thai text still wraps and does not overflow (measured in the headless
  test). Where it breaks was not measured; from the source, with no model
  there is no word-boundary opportunity inside a Thai run, so the break
  falls wherever the line fills.

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
| Verify kit input, virtualization, focus, Markdown and AccessKit with Thai/CJK text | Headless only; no OS IME, no screen reader. **Required gate, open.** |
| Inventory UI intents and data availability; name missing contracts | First pass in §6; approvals, credentials and setup not examined |
| Original shell, composer and inspector tokens; narrow and wide mock states | Starting tokens and three width states exist in code and as offscreen captures; no visual review with users |
| Performance fixtures, baseline measurements, accepted budgets | Synthetic static fixture, first-frame timing and peak RSS only; no budget accepted. **Required gate, open.** |
| Decide packaging and supported platform tier | Not started |

## 8. Pending gates and next steps

Phase 0 is not complete and Phase 1 has not been started. It waits for the
independent review of this checkpoint. The gates below are all still open;
none of them can be closed by anything measured so far.

| Gate | Why the current evidence does not cover it |
|---|---|
| Windows build, tests and launch | Only dependency resolution was done for Windows. Must run on the Windows PC. |
| Linux build and launch | Only dependency resolution was done. |
| Real OS IME composition (Thai, CJK, dead keys) | The harness types finished text; no input method ran. |
| VoiceOver (and NVDA on Windows) | Roles and labels were read from the accessibility tree in tests; no screen reader ran, and overlay dialog semantics are not set. |
| 200% text scaling and display-scale changes | Not tried. Layout tokens are unverified at scale. |
| Streaming and performance budgets | No streaming fixture, no frame timing, no key-to-paint, idle CPU or long-run memory. |
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
  runs only this crate's commands, with two build jobs, and takes about a
  minute on a warm target directory.
- `logs/`: environment, format, lint, both test runs, release build, the six
  smoke launches, lockfile verification, dependency graphs, resolved
  features, build plans and on-disk artifacts.
- `captures/`: the seven offscreen PNG captures.
- `compare-captures.py`: the run-to-run pixel comparison quoted in §4.
- `run-1-pre-commit/`: the run, minutes before the commit and on identical
  crate files, that the revised-head figures in §4 were read from. The
  canonical `logs/` were collected again at the commit itself, so their
  smoke timings are a further batch and differ by the usual run-to-run
  spread.

That directory is local to one machine and is not part of the repository;
anyone else reproduces it by running the script.

