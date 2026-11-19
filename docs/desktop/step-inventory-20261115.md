# Recoverable read-only Step inventory

Previously the daemon kept Step decision senders, but `GetSession` only returned
`step_mode` and unrelated pending events. After reconnect, the native client
could know Step mode was enabled without discovering the prompt already waiting.
The existing real-daemon test explicitly characterized that missing inventory.

`GetSession` now adds `pending_steps`, a read-only object with `prompts` and
`more`. Each prompt carries its Session, Run and Step identities, Tool name,
zero-based call index/count, original argument byte count and a UTF-8-safe
argument preview capped at 4,096 bytes. Up to 64 matching prompts are returned
in stable Step identity order. Selection scratch memory has a fixed bound independent of the total waiter
count; other Sessions' identities are excluded. Stored waiters retain only
these bounded argument previews, not an extra full argument copy. Old clients
ignore the additive field; old daemons remain compatible with an absent inventory.
No new RPC decision or permission scope is introduced.

The actual waiting future owns a retirement guard. A valid existing decision
removes its sender and inventory immediately. Wait completion, cancellation and
future drop/abort remove the remaining record and emit the existing resolution
event once. Invalid edited arguments leave the original waiter intact. Existing
Continue, Skip, Edit and All semantics are retained. A late decision after owner
retirement is rejected rather than acquiring a stale prompt.

The desktop decodes the optional inventory separately from transcript validity.
Unsupported, duplicate, foreign, oversized or inconsistent prompt definitions
produce an explicit inventory warning without substituting another Session's
prompt or discarding its canonical transcript. Missing inventory on old peers
is unavailable, not proof that no Step is waiting. Selected owned snapshots and
validation-reader status batches supply the same inventory; rendering/list reuse
cannot retain a resolved prompt merely because transcript rows are unchanged.
Selection, Profile replacement, disconnect, failed reads and reader retirement
clear the summary. Existing view-generation/connection/basis checks still apply.

The native summary is labeled as a past canonical refresh, may have changed,
and is read only. It exposes exact Run/Step identities and a bounded arguments
excerpt, with truncation explicit. Display is capped at 120 pixels so long Tool
arguments cannot consume the composer. Full arguments and decisions remain in
Formless TUI. A sampled response is not an ordering fence or mutation capability;
this increment intentionally adds no native Step decision, automatic approval,
replayed action, or persistent prompt authority. Only currently waiting daemon
state is recovered, not a prompt resurrected across daemon restart.

Headless checks cover silent snapshot restoration, unchanged transcript reuse,
retired generations/Session switches, disconnect/error cleanup, draft retention
and a 200% minimum-window composer. Pure decoding tests exercise invalid data,
legacy absence, duplicate identities and UTF-8 bounds. Focused daemon tests
exercise sender spending, invalid/valid Edit, Continue/Skip/All, cancellation,
owner abort, late decisions, session isolation, stable truncation and preview
bounds. The real private-IPC/fake-provider Step schedule now reconnects a fresh
Client in both canonical and validation-reader modes, restores the same Step
without replaying its event, observes a second client's Skip, and verifies empty
inventory after completion with exactly two fake provider requests.

Production streaming remains disabled. No desktop, renderer or OS interaction is
executed by these TestPlatform/private-daemon tests. Native Step/security controls,
actual IME/accessibility/scaling, supported Windows/Linux execution, performance,
licensing and distribution acceptance remain open. The original conflicting
checkout is preserved. The unresolved foreground approval/prior uncertain launch
is not repeated or bypassed. Current main was checked read-only and remains
`c410af910118045af05d4d52148a33802194398c`, already an ancestor of this branch.
The new core/protocol/view change and the full combined runtime range require
independent review before any publication or integration.
