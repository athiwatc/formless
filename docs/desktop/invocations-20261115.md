# Explicit unfinished Tool Invocation snapshots

This isolated continuation starts at reviewed local `714948b6`, on top of
published draft #2081 head `cc1d0c9`. It does not complete epic #1753 or
establish native acceptance. Streaming remains disabled (`deltas: false`).

## Behavior and ownership

Commands → View Invocations and Tools → Invocations issue the existing
read-only `ListInvocations` RPC. It returns unfinished work across the Profile,
oldest first. The ordinary Tools page and restoring the transient destination
do not issue this read. Refresh is explicit, as is reopening after reconnect.

The snapshot shows returned elapsed/silent milliseconds, daemon stalled and
abandoned flags, reported progress and labels. These are past-read values,
not running clocks or inferred completion. Missing progress stays unknown;
100% is not completion. Abandoned means the agent stopped waiting, and does
not prove cancellation. No Cancel/Abandon control or Invocation mutation was
added. Legacy records lacking durable IDs are identified as such; handles
supply no durable mutation authority.

Scope Copy details contains the complete returned JSON; each other row copies
one complete object. Unknown future fields and private labels are preserved.
Pretty serialization changes whitespace, not JSON values. Copy callbacks bind
the destination, page revision and selection through the existing resource
copy guard. Open Session additionally binds the snapshot revision, connection,
record index and complete identity key; the record's Session must still be
available, unique and unarchived. Navigation uses normal activation and draft
preservation. A reused handle cannot revive a stale button. Modal and composer
preedit guards apply to reads and navigation. No Run is started or model called.

Each request/reply binds a connection epoch and operation UUID. Wrong, stale,
duplicate and generic Page updates cannot install a snapshot. At most two
transport reads may be outstanding; only one current shell read is accepted.
Leaving, disconnection or Profile replacement clears pending/loaded state and
callback ownership. Reconnect does not replay the read. Existing Send/question
unknown-outcome identities remain unchanged by a successful Invocation read.

The sidebar previously assigned Chat directly and skipped transient cleanup.
A new regression reproduced an Invocation read remaining pending forever after
sidebar activation. Both command navigation and Session activation now use one
retirement helper for Invocations, Run inspection, comparison and fork review.
The fork uncertainty ledger is not reset. A second regression covers reopening
Run inspection after leaving a pending read through the Session sidebar.

## Bounds and evidence

Raw JSON including future fields and complete pretty JSON each have a 1 MiB
cap. Snapshots have at most 1,000 records, bounded identifier/label fields,
unique handles/durable IDs, finite progress in [0,1], and typed unsigned times
and status flags. Malformed, duplicate or oversized responses show an error;
no partial list is accepted. Retained row details total at most 3 MiB, with
500-character previews and at most 1,000 bounded keys. These are input and
retention limits, not a measured transport, peak-RAM or CPU budget.

Model and transport tests cover exact private/future JSON, legacy absence,
compact-versus-pretty limits, malformed shapes, identity collisions, operation
and connection ownership, outstanding-read bounds, and preservation of unknown
Send/question outcomes. Headless GPUI tests cover explicit reads, exact Copy,
stale selection/page callbacks, Refresh, errors/empty results, unavailable
Sessions, reconnect/attachment turnover, preedit/modal guards, draft retention,
sidebar retirement and keyboard reachability at 480×360 with 200% text.

The existing private daemon/fake-provider fixture adds a capability-free Tool
that reports 50% progress and completes naturally. The desktop client observes
its exact Session/Invocation identity and private label through the real RPC,
then explicitly reads an empty pending list after completion. The provider
request count stays one; no Cancel/Abandon or user Profile is involved. These
are daemon/headless tests, not native GUI execution.

The sibling evidence manifest records exact commands, results, source/log/patch
hashes, preserved unsuccessful attempts and development-package verification.
The initial near-limit test fixture left too much pretty-print headroom and
was corrected; the separate sidebar failure was reproduced before its fix.

## Open acceptance and integration gates

The original conflicting partial tree remains preserved and unaccessed. No
remote PR/branch was modified. Parent-reported reviews through `714948b6` found
no confirmed blocker, with logs inspected rather than tests rerun. This increment
still requires exact-commit independent review and combined integration checks.

The unresolved Formless Sol Validation foreground approval/prior uncertain
launch or control action was not repeated or bypassed. Native input, IME,
VoiceOver/NVDA, clipboard, scaling, picker/filesystem, Windows/Linux GUI,
remaining parity (including Invocation control/history and artifacts), accepted
performance budgets and distribution/license/signing/install/update evidence
remain open. Development packaging is build/archive verification only.
