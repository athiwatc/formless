# Recorded Run inspection from Metrics

Continues local `b0d85263c095f38dcaf2711517a9f81271e69132` in the isolated
takeover checkout. The parent reported no confirmed blocker in independent
review of all eight preceding local increments; source/log hashes were
inspected, tests not rerun. This increment needs exact-patch review. GPUI
completion and native/product/integration acceptance remain open.

## Behavior and scope

Metrics exposes Inspect Run for the selected Run row, also available through
Commands. The Summary cannot inspect even if an unsupported daemon payload
places Run-looking keys in it. The target comes from the displayed metric's
bounded nonempty Run and Session identities, never a guessed prefix, active
Session, row ordinal after refresh or user-supplied path. The action calls
only the existing `GetRun` API. It does not cancel/resume/abandon a Run, start
a model, fork a Session, alter a Session/draft or perform a paid comparison.

A transient inspection page shows authoritative Run/Session IDs, recorded
status/error and timestamps, message sequence bounds, recorded counters and
optional reported spend. It is a past recorded read, not a live subscription
or completeness claim for an active Run. Raw spend is labelled as reported
information; absent/null cost stays unavailable and no complete bill is
inferred from subtotals or incomplete usage. No missing counter defaults to
zero: unsupported required fields fail rather than producing a partial record.

Scope's Copy details copies complete pretty-printed returned JSON, including
unknown future fields and private error/provider metadata. Other rows copy
one complete known group or the spend object. Unicode, whitespace, CRLF and
future fields survive JSON decoding/copy. Visual previews are limited to 500
characters and disclose truncation. No HTML or record content is executed.
Refresh explicitly rereads the same exact target. Back returns to Metrics,
which can refresh its own listing; it does not inspect another Run automatically.

## Ownership and bounds

The source callback freezes connection, Metrics destination/page revision,
selected row, exact Run/Session target and the inspection lifetime revision.
Selection, source-page refresh, navigation, modal ownership, connection
turnover or Profile replacement cannot redirect that queued action. The
inspector keeps one pending UI read; the transport permits at most two
outstanding inspections. Pending replies capture connection, operation UUID
and target. They install only on the current attachment/connection, destination
and pending operation, with duplicate and stale replies ignored.

The decoder requires exact returned Run and Session IDs, bounded status and
timestamp strings, an explicit nullable error, nonnegative integral counters
and explicit nullable nonnegative message bounds. Reversed known bounds are
refused. Optional spend must be an object or null; nested/future fields remain
opaque diagnostic data, not newly computed accounting. Raw serialization,
including unknown fields, is bounded to 1 MiB before row projection; complete
pretty JSON has a separate 1 MiB cap. Retained row detail text has a 3 MiB cap.
Metrics source-row parsing is also refused above 1 MiB. These are input and
retention limits, not measured peak-RAM/CPU or transport-frame bounds.

Leaving the page, disconnect or client attachment replacement drops its data
and pending operation; counters/IDs reused by a new client cannot revive it.
No inspection is retained or replayed by reconnect. A ready inspector also
retains its owning connection, so an old Refresh cannot cross actual atomic
connection turnover before the corresponding UI update arrives. Comparison,
Send and question-answer unknown-outcome guards are unchanged; a read ACK
cannot resolve uncertain mutations.

## Validation evidence

The new small-window test first exposed Inspect Run focused at y=-277 in a
480x360/200% page (`run-inspection-focused-gui.log`). Inner ScrollAnchors did
not preserve the resource body's ancestor offsets. Resource controls now
reveal direct body children through its tracked scroll handle, following the
existing Profile Search approach: header controls use child 0; Copy uses
child 5. Sessions' separate page anchors remain separate. Tests reach Inspect,
Copy and Back through actual Tab/Enter and assert visible bounds.

That first focused run also reproduced a queued source read after atomic
connection retirement, before its UI notice. Source identity now includes
that connection and ready Refresh checks its owning connection. Regression
coverage verifies both paths reject the stale action. The early GUI-header
syntax failure is preserved too; the header builder was rewritten with named
Refresh/Back/action values. No failed log is counted as final passing evidence.

Three model tests cover exact future/private JSON and missing spend; malformed,
missing, foreign, negative and oversized records (including pretty expansion);
and pending/retired connection/operation/target lifetimes. An Engine test
covers exact request shape, foreign Session echoes, out-of-order/duplicate
replies, invalid targets/connections and the two-read cap. The existing real
worker unknown-Send/question regression now processes an inspection ACK and
asserts neither uncertain mutation is cleared.

Five shell tests exercise actual kit key/hit/focus dispatch with TestPlatform
and the recording-client seam: explicit reads from another Session's Run,
draft preservation and exact JSON copy beyond previews; Summary rejection;
selection/page/navigation/modal retirement; generic-page spoof rejection;
wrong operation/connection/target and duplicate replies; explicit Refresh and
error pages; stale copies; disconnect, same-counter attachment replacement and
atomic turnover before the UI update; and 480x360/200% keyboard reachability.
They do not prove real OS clipboard, input, IME, accessibility or display scaling.

The private-profile real-daemon/local-fake-provider test inspects an active Run
while another Session is selected and its Provider response is held. It
compares complete JSON to the daemon's direct read, verifies the stored Run
and Session/running state stay unchanged, and checks the Provider request count
stays one. No real user Profile or external Provider credentials are involved.
Exact command exits, suite counts, source/log/artifact hashes and package
provenance are recorded in the sibling evidence manifest. The renderer and
desktop app are not run or installed.

Final offline validation passed 216 GUI-free model tests, 221 GUI/Thai model
tests, 189 headless shell tests, two CLI tests, all five private-daemon tests,
strict desktop/daemon-test clippy, Windows GUI-free all-targets static checking,
formatting and whitespace checks. The optional core dependency emitted its
26 existing warnings; no core source was changed. Daemon execution used the
scoped local-network approval for the private loopback/IPC fixture. No renderer,
real picker/clipboard or desktop foreground action was performed.

## Integration holds

The conflicting original checkout remains untouched and ownership unresolved.
Existing Formless Sol Validation approval and the prior uncertain launch/control
outcome remain unresolved; neither action was repeated or bypassed. Streaming
remains disabled. Native IME/VoiceOver/NVDA/clipboard/input/scaling, Windows/Linux
GUI, filesystem/picker, broader parity (including fork and artifact/Invocation
navigation), performance budgets and distribution acceptance remain open.

No push, PR update or merge occurred. Review this exact increment, resolve
ownership and native gates, recheck live main/#1776/#2081 and validate the
agreed combined revision before integration. The user's finished-GPUI and
clean-main request is not claimed complete.
