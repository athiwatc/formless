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
| Composer | Send/Steer/Queue/Stop, preserved drafts, IME guards, forecast consent; new per-window send-key choice | Attachments/drop/image paste/external editor, remaining Role/model/reference controls |
| Context/resources | Recorded Session usage; bounded read-only Tools/Metrics/Proposals/Settings and exact details copy; explicit exact-identity recorded Run inspection; bounded read-only unfinished Tool Invocation snapshots, exact details and associated Session navigation | Artifact detail selection, Invocation control/history, Run pinning/navigation, scoped diff and remaining editable resource actions |
| Questions/security decisions/Step | Native user questions, forecast consent; bounded read-only Step inventory restored from canonical snapshots after reconnect; approvals and Step decisions explicitly hand off to TUI | Native security-approval parity; native Step decisions and remaining reconnect/control acceptance |
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
2. Finish exact-patch reviews of each new isolated commit. Parent-reported
   reviews found no confirmed blocker in `acabff81`, `dfbab768` and
   `8a18d80a`, `7658c8b5`, `f750cc2b`, `852932ef`, `bbde4a25`, `b0d85263`,
   `16f62823`, `714948b6` and combined `4445d8b`; those reviews inspected tests/logs without rerunning them.
3. Resolve the existing foreground approval and prior uncertain launch/control
   outcome through the supported approval/evidence path. No foreground action
   is repeated or substituted here, and no writer/Claude session is controlled.
4. Obtain the required native/platform, product-parity, performance and
   distribution evidence above; fix failures before claiming completion.
5. Recheck live main/#1776/#2081 and ownership, choose the agreed integration
   branch, review the combined diff, then rerun appropriate aggregate and
   native checks there. Only merge when substantive gates pass.

No remote branch/PR state was modified by this continuation. All new work
remains on the separate local branch; a published draft or PR update is not
performed merely to represent progress. No unverified clean-main or all-PRs
merge claim is made. Local patches and hash manifests remain reviewable in
the sibling task evidence directory.

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
