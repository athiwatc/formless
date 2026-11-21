# Persisted Session comparison in the native client

Continues independently reviewed local `852932ef80cbb2adc42a2dc26243a8978a9d79af`
in the isolated takeover checkout. The parent reported no confirmed blocker
in each prior local increment; those reviews inspected tests/logs, not reruns.
This increment needs exact-patch review. Epic #1753 remains incomplete.

## Behavior and scope

The connected Sessions directory offers Mark left, then Compare against a
different selected Session. Selection does not activate a Session or replace
its composer. The palette exposes the same two actions. Comparison uses only
the existing read-only `CompareSessions` RPC, which reads persisted histories
and recorded usage. It does not fork, summarize with a model, control a Run,
prepare a paid branch summary, or write daemon data.

A transient resource page shows authoritative left/right IDs, names, common
ancestor, fork point, diff start and changed-prefix flag; recorded usage for
each side; warnings; and whole-message differences. It does not infer a
line diff, reconstruct excluded shared history or claim live Run completeness.
The daemon's sequence scope and warnings remain available in full JSON.
Unavailable costs stay unavailable, with allocated/unallocated priced subtotals
and observed-record ranges clearly qualified. Missing usage and unpriced
records are never bounded by the displayed observed range.

Scope's Copy details copies the complete validated known-field protocol JSON.
Per-row Copy details copies the whole selected usage/warning/message record,
including exact Unicode, CRLF, whitespace and metadata. The notice discloses
private Session settings and message data. Preview text remains limited to
500 characters. Unknown future fields are not claimed to be copied; unsupported
message variants fail closed. No HTML, tool content or links are executed.

## Ownership and bounds

Each request captures the pair, actual connection and a unique operation ID.
Replies retain those identities even if another Session is selected. The
shell accepts only the current pending operation on the current attachment,
connection and destination. Refresh also freezes the rendered pair/revision.
At most two comparison reads can remain outstanding in the transport. No
comparison read is retained/replayed by reconnect. A comparison ACK cannot
retire uncertain Send or question-answer operations.

The decoder bounds raw JSON including unknown fields to 1 MiB before typed
projection, bounds pretty JSON separately, and limits 1,000 whole-message
rows. It validates echoed Session IDs, nested message ownership, unique
nonnegative per-side sequences, recognized side shapes and semantic equality
for unchanged messages. Usage counts, finite nonnegative prices, ordered
ranges, warning/unpriced collections and identity lengths are checked.
Retained row detail totals have a separate 3 MiB cap, since the complete
Scope JSON also includes per-row data. These are retention/input limits,
not a measured CPU/peak-memory budget or transport-frame replacement.

Leaving the page retires results and pending callbacks while preserving the
left mark; returning does not refetch automatically. Disconnect, attachment
replacement (even repeated counters/IDs), or disappearance from Sessions
clears the mark and data. Refresh requires an explicit still-listed pair.
Errors show no partial replacement data. Background actions respect modal
ownership and first commit directory composition without also comparing.
Existing draft and undo ownership are not changed by selecting comparison sides.

## Layout and validation evidence

A new 480x360/200% keyboard check reproduced an existing generic resource
control issue: Copy details could gain focus at y=2988, outside the visible
page (`comparison-focused-gui-rerun3.log`). Resource Refresh, Back and Copy
now have explicit focus/scroll anchors and a tracked scrolling body. The
new directory controls have anchors too. The connected directory's minimum
reflow height reserves room beneath its wrapped toolbar and comparison notice.
The corrected test reaches both pair controls and Copy via actual Tab/Enter,
checks visible bounds and copies the complete JSON.

Five new shell tests exercise real kit dispatch/hit testing with the simulated
TestPlatform: explicit reads, draft preservation, exact copies beyond previews,
selection/refresh/navigation retirement, duplicate/foreign/stale replies,
spoofed generic pages, error/missing Sessions, disconnect and same-counter
attachment replacement, modal/preedit guards and small-window keyboard
reachability. They do not exercise native OS input, screen readers or clipboard.
Four model tests and an Engine test cover decoding, size/shape rejection,
state retirement, request shape, limits and out-of-order/duplicate replies.
The existing unknown-operation reconnect regression now actually processes a
comparison reply and verifies uncertain Send/question state remains intact.

The private-profile real-daemon test adds a comparison while a local fake HTTP
Provider response is held and a different Session is selected. It compares
returned JSON to the daemon's own persisted comparison, verifies the Session
snapshot/running state remains unchanged, and checks provider request count
remains one. No external provider credentials or user Profile is involved.

The broad suites passed 210 GUI-free model tests, 215 GUI/Thai model
tests, 178 shell tests and two CLI tests. All five real-daemon/provider tests
passed after scoped local-network approval. Their first sandboxed attempt
failed before protocol execution when loopback binding returned Operation not
permitted; that failed log is retained. Unmodified core dependencies emitted
26 existing warnings. Desktop strict lint and Windows GUI-free static checks
passed. The final scroll-position reset is covered by the subsequent shell
and lint runs listed in the manifest. It resets both body and row-list offsets
for each explicit comparison read so an earlier resource's scroll position
cannot hide the new Scope row.

Exact final command results/hashes are recorded in the sibling evidence
manifest. Initial test-only compile/setup failures, decoder boundary setup
failure and the reproduced focus-reveal failure are preserved separately;
none is counted as passing evidence. No renderer harness or desktop binary
was run, installed or brought to the foreground.

## Integration holds

The conflicting original partial tree remains untouched, with ownership
unresolved. The existing Formless Sol Validation foreground approval and prior
uncertain launch/control outcome remain unresolved; neither was repeated or
bypassed. Native IME, accessibility, human scaling/input, Windows/Linux GUI,
filesystem/picker acceptance, broader parity (including fork), performance and
distribution gates remain open. Streaming remains `deltas: false`.

No push, PR update or merge occurred. Review this exact local increment,
resolve ownership and native gates, recheck live main/#1776/#2081, then validate
the agreed combined integration revision before publication/merge. The user's
finished-GPUI and clean-main goal is not claimed complete.
