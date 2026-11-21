# Native idle Session Step-off

The existing Step-mode status row now exposes Disable Step mode when the
selected Session is known idle, has no recoverable Run, and has a complete
empty Step inventory. Its visible help states that Off stops future pauses
and leaves waiting Steps/approvals requiring review. It changes the existing
ephemeral Session setting for this daemon, not Tool security authority.
The normal constructor continues to disable production token streaming.

The typed action binds Client, physical connection, selected Session, selection
generation, view epoch, operation UUID and explicit target boolean. The worker
reads GetSession again before any write. Disable requires the same Session,
strict running=false, no unfinished Run, known complete empty pending_steps
and sampled mode=true. `unfinished_run` must explicitly be null. Missing,
malformed, partial or legacy metadata rejects the write. If a fresh read already
shows Off, the result is read-only. Disable requires `idle_step_off_v1`. An older
peer receives no Disable request. Enable continues to use the existing API.

Both directions share the existing status revision and reservation ledger.
Observed Run/Step changes, deletion, navigation and reconnect retire unsent
work. Only an exact Session/target/success receipt accepts a setting write;
lost or malformed receipts remain Unknown, block both directions for that
Client and never replay. Deduplication retains at most 256 spent operation
UUIDs for the Client lifetime and is not durable across process restart.

`DisableStepModeIfIdle` repeats the idle check at the mutation boundary. It
holds the daemon's Run admission lock while checking active and publishing
Runs, and its Store writer lock while checking Session existence and the latest
persisted Run status. A running, recoverable or unrecognized latest status
blocks Disable. The mode and waiter locks serialize the empty-waiter check with
registration and prevent removing the mode underneath a waiting Step. The RPC
never grants approval or releases a waiter. Other clients retain the existing
`SetStepMode` API, and a later explicit Enable can still change the mode. The
check covers work admitted by this daemon and does not coordinate external
writers to the Profile database.

Focused actor/model tests cover false receipts, exclusive shared reservations,
changed owners and missing/busy/pending/incomplete state. TestPlatform checks
cover explicit dispatch, exact draft ownership, composition/modal/background
guards and keyboard access at a simulated 200% minimum window. A private real
daemon/fake provider exercises Enable then idle Disable through the actual
view and checks that neither starts work; accepted text and drafts survive.
A private Unix peer loses each target's ACK, physically reconnects, shows
the changed setting and proves Unknown still prevents replay. Native OS input,
VoiceOver/NVDA, scaling and platform acceptance remain unverified.

The increment remains isolated for independent review. It does not clear full
Step/security parity, broader composer/resource workflows, hosted desktop
billing, native/performance or licensing/distribution gates.
