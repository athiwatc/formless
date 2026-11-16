# GPUI completion and integration acceptance matrix

This is an implementation/evidence inventory for epic #1753, starting from
published draft #2081 head `cc1d0c9805230d7807a4a871396f91c0ebeec91a` and the
isolated local continuation. It is not acceptance, a complete parity claim,
or authorization to merge while substantive gates fail.

## Current implementation versus completion

| Requirement | Present evidence / implementation | Still needed |
|---|---|---|
| Optional native target; existing headless clients compatible | Detached desktop workspace; unchanged root/desktop manifests and lockfiles in this continuation; kit facade | Revalidate combined main after integration; supported platform policy |
| Daemon connection and canonical Sessions/transcripts | Existing client, request ownership and snapshots; local suites; prior real daemon/fake-Provider evidence | Latest native/live acceptance; general delta replay still unproved |
| Ordered streaming / reconnect | `deltas: false`; canonical snapshots and event invalidation; unknown mutations not replayed; reconciliation failure now freezes further events until a trusted snapshot | A constrained future-attempt barrier/correlation protocol and admission model now have unit/private-IPC evidence; validation-only reader/view integration now has transport and headless evidence; independent combined review and full streaming acceptance remain required before enabling deltas |
| Sessions | Directory, New Session, metadata/tags/pin/archive, instructions, persisted export and bounded new-file Save through the system picker; explicit persisted Session comparison with recorded usage; bounded window-local reading-message restoration; explicitly reviewed full-history forks with client-lifetime unknown-outcome protection | Bounded-prefix/custom-name forks and paid branch-summary workflows; native import and full-Profile backup; OS picker/volume acceptance; persistent/finer per-Session view-state restoration |
| Conversation and large output | Virtualized blocks, bounded previews, whole-output viewer, exact block source copy | Cross-block selection/copy, dedicated code-copy/link opening, richer attachments/artifacts/diagrams and native semantics |
| Composer | Send/Steer/Queue/Stop, preserved drafts, IME guards, forecast consent; new per-window send-key choice; bounded manual per-Session model/Role and effort overrides with coupled-value revalidation; explicit bounded model catalog suggestions; explicit bounded pending-file metadata inspection | Attachments/drop/image paste/external editor, per-provider catalog/Role editing, stored-file removal/reference additions and durable reconciliation |
| Context/resources | Recorded Session usage; bounded read-only Tools/Metrics/Proposals/Settings and exact details copy; explicit exact-identity recorded Run inspection; bounded read-only unfinished Tool Invocation snapshots, exact details and associated Session navigation | Artifact detail selection, Invocation control/history, Run pinning/navigation, scoped diff and remaining editable resource actions |
| Questions/security decisions/Step | Native user questions, forecast consent; bounded Step inventory restored after reconnect; native Continue/Skip/Edit/Session-Step-off for one complete supported prompt; freshly validated Session Step-mode enable and idle Step-off; one selected complete Tool request from a bounded inventory supports Allow once or Deny; unsupported Step/security review hands off to TUI | Security batches/promotions/wider scopes and larger evidence; larger/multiple Step prompt controls; native decision/IME and remaining reconnect/control acceptance |
| Search/navigation | Loaded literal Search and native Find; separate persisted Profile Search | Remaining global action discovery and resource navigation parity; native usability acceptance |
| Layout/focus/scaling | Responsive panes/overlays, modal traps/restoration, reflow and headless 200% checks | Resizable panel preferences, real selection/input/trackpad, human 200% and multimonitor/HiDPI acceptance |
| Themes/preferences | Light/dark and 100–200% text; explicit versioned Save/Reload of view, docked-panel and send-key choices; corruption/conflict handling | System/high-contrast/colorblind themes, density/reduced motion/font choices, geometry/per-Session view persistence and native filesystem/latency acceptance |
| Accessibility and IME | Semantic/headless tests; historical Mac adapter/capture evidence | VoiceOver/NVDA, real Thai/Japanese/dead-key input, current-head native semantics; session position/count adapter limitation |
| Performance | Historical fixture measurements, bounded caches and virtual lists | Accepted budgets and current-head frontmost/native measurements including accessibility cost; no synthetic pass substitutes for them |
| Platforms | Mac compilation/headless tests; prior native Mac evidence; historical Windows smoke evidence at older code | Current-head native Windows GUI and Linux build/run/render/accessibility; actual declared support tier |
| Distribution | Existing reproducible development-bundle helper; notices/provenance/hashes | Manual dependency/assets/stdlib licence audit, icons, signing/notarization, install/launch/update/clean-install acceptance |

Inventory sources: the desktop README, source modules and continuation reports
linked there, plus the published epic's screen/interaction and validation plan.
A row's existing implementation is not evidence that all its behaviors are
production-ready. The local continuation has no fresh real OS execution.

## Integration holds

The user's goal is a finished GPUI client and clean main with PRs integrated.
That goal does not make the failed or missing evidence above pass.

1. Preserve the conflicting original partial tree until ownership or safe
   recovery is established; do not adopt its unverified edits.
2. Combined independent source review is complete at published `fd1e8ec`,
   including prior runtime/protocol, failure-schedule, packaging, native Tool
   approval and historical-compatibility correction scopes. Reviewers checked
   hashes/logs without independently rerunning tests. The later isolated idle
   Step-off increment requires its own exact-head review before publication.
3. Resolve the existing foreground approval and prior uncertain launch/control
   outcome through the supported approval/evidence path. No foreground action
   is repeated or substituted here, and no writer/Claude session is controlled.
4. Obtain the required native/platform, product-parity, performance and
   distribution evidence above; fix failures before claiming completion.
5. Recheck live main/#1776/#2081 and ownership, choose the agreed integration
   branch, review the combined diff, then rerun appropriate aggregate and
   native checks there. Only merge when substantive gates pass.

Draft #2081 now contains reviewed `fd1e8ec`, published as a guarded normal
fast-forward and still unmerged, with main `9810f8f2` included. Root CI
`43765829218` passed on macOS/Linux at checkout `9e141f8c`, whose tree matches
`fd1e8ec`; the root workspace excludes the optional desktop. Desktop run
`43765829247` never started any job because of account payment/spending
restrictions. No blocked job was retried or runner/billing configuration
changed. Later idle Step-off work stays on a separate local branch for review.
The original conflicting checkout and all previous branches/evidence remain
preserved. Neither this inventory nor successful source review is native or
full-product acceptance, and no clean-main/all-PRs merge claim is made.

Current-main local integration and the concrete remaining streaming contract are
recorded in [the reconciliation report](stream-reconciliation-integration-20261115.md).
A conflict-free local merge is not combined acceptance or publication approval.

The later [headless completion report](headless-completion-gates-20261115.md)
classifies checks that can run safely without the unresolved foreground action,
records concrete model performance limits, and prepares integration with main
`c410af9`. Package notice/determinism checks improve development evidence without
clearing native, product-parity, performance or distribution acceptance.

The subsequent [parser profile and notice report](accessible-performance-notices-20261115.md)
improves the measured dev/test parser cost without changing semantics and adds
version-bound upstream/Rust notices. Three empty inventories, nineteen
licensing-note-only cases and native/performance/distribution acceptance remain
explicitly unresolved.

The [licensing scope follow-up](licensing-scope-20261115.md) retains later full
upstream texts and an earlier MIT notice with separate provenance, preserving
all nineteen scope flags and all three empty-notice flags. It identifies exact
archive evidence and owner decisions without treating file counts or partial
review receipts as release acceptance.

The [Step inventory report](step-inventory-20261115.md) adds recovery of currently
waiting prompt identities and bounded read-only previews, plus owner retirement
on cancellation/drop. Native decision authority and full Step parity remain open.

The [native Step decision report](native-step-decisions-20261115.md) adds explicit
complete-prompt review with fresh worker validation and a non-replayed receipt
ledger. Its size/scope limits and native acceptance gates remain explicit.

The [bounded stream delivery report](stream-delivery-20261115.md) adds batching
only for admitted validation-reader live text. It preserves event identity/order
and fail-closed recovery, while production streaming remains disabled pending
complete review and acceptance.

Run/Step metadata overlap is additionally fenced by a checked connection-local
lifecycle revision; see [status read fence](status-read-fence-20261115.md). This
covers observed same-sequence status/barrier races and retires unsent reviews,
without clearing the production streaming or native acceptance gates.

The future-only admission gate now retains an observed scoped producer cursor
floor even for ignored idle Delta/Closed frames. Repeated or decreasing future
cursors fail closed before a delayed Start can revive a prefix; see
[observed cursor schedules](observed-cursor-20261115.md). This adversarial-ordering
hardening does not enable production streaming or clear native acceptance gates.

The [minimum production streaming plan](production-streaming-plan-20261115.md)
states the future-only versus seamless-recovery owner decision, maps implemented
ordering obligations to evidence, and sequences combined review, portable/native
acceptance and the eventual separately reviewed production-policy change. New
deterministic model schedules cover cutoff/receipt staging, ownership retirement,
canonical storage/publication and wire permutations; production remains disabled.

The owner selected future-attempt-only v1 for the initial implementation. A
persistent visible limitation and actual fake-provider/daemon/IPC/client to
TestPlatform view checks are documented in the
[v1 path report](v1-end-to-end-20261115.md). This records the scope decision and
headless evidence, not production enablement or seamless recovery acceptance.

The [Step-mode enable report](step-mode-enable-20261115.md) adds an explicit
ON-only Session setting with fresh validation, a bounded Client-lifetime receipt
ledger and no replay after uncertain writes. Native controls, OS accessibility,
platform execution and full parity remain acceptance gates.

The [idle Step-off report](idle-step-off-20261115.md) adds the missing Session
mode control when fresh idle and complete empty-Step metadata is available.
It changes future pauses, leaves existing waiters pending, shares non-replayed
Client-lifetime receipts with Enable, and explicitly does not promise atomic
idle-only application against unseen work from other clients. Native/full
parity and platform, performance and distribution acceptance remain open.

The [Session model selection report](session-model-selection-20261115.md) adds
bounded manual model/Role and effort overrides through Session details. The
existing setter may trigger a configured paid model test, so its form has a
persistent visible notice. Complete paired values are freshly checked; work
preparation waits for the setting receipt. Read-only no-ops, missing metadata,
stale form callbacks and physical reconnect are covered headlessly. This does
not establish provider availability, catalog/Role editing, native usability or
other composer parity, and the latest local increment still needs review.

The [reviewed-send boundary](reviewed-send-20261116.md) compares complete pending
metadata and consumes it with message append under one transaction. The
[native confirmation path](native-send-review-20261116.md) now uses it after
explicit review of the full bounded draft and inventory, including empty
inventories. Unsupported peers fail closed and uncertain responses are retained
without replay. Queue/deferred ownership, attachment mutations and durable
reconciliation remain required. Native, hosted, performance and distribution
gates remain open and production streaming remains disabled.

The [reviewed reference-removal boundary](reviewed-reference-detach-20261116.md)
compares a complete inventory and removes one selected reference occurrence
under one transaction, leaving targets and artifacts untouched. Bounded
connection-local receipts suppress duplicate operations within their retention
window. [Native reference selection and confirmation](native-reference-controls-20261116.md)
now binds the accepted inventory and exact occurrence, focuses Cancel first and
keeps written errors/lost replies Unknown without replay. This increment needs
independent review; stored-file removal, attachment/reference additions and
durable reconciliation remain. It does not restore Queue or clear native,
hosted, performance or distribution acceptance.
