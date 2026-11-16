# Native confirmation for one pending reference occurrence

Session details → Read pending files now exposes a review button for each
supported reference on the current eight-entry page. Stored-file entries have
no Remove button. Selecting a reference displays its complete escaped JSON
metadata, Session and absolute occurrence number. Cancel receives focus first;
only explicit “Remove this reference only” confirmation queues a mutation.
Existing Copy retains the complete ordered inventory, including duplicates and
unknown fields. This does not add native attachment creation or file deletion.

The action binds Client UUID, physical connection, epoch, form revision,
accepted-read revision, Session, index and a fresh operation UUID. The Session
whose details were opened may differ from the active conversation. Composition
and modal checks apply both when arming and confirming. Stale Confirm/Cancel
callbacks cannot target a later review. Navigation, closing/reloading the form, changing its field or saving metadata,
Profile replacement and context invalidation retire unwritten reservations.
A request that may have been written cannot be cancelled this way.

The worker negotiates `reviewed_reference_detach_v1` and uses only
`DetachReviewedReference` with the full reviewed inventory. Unsupported/stale
reviews fail before writing with a TUI/update handoff; there is no legacy Detach
fallback. The daemon's existing atomic compare/delete boundary removes one
metadata occurrence without reading/deleting its target or artifact files.
Metadata equality is neither durable row identity, frozen file contents nor
proof of consent; explicit native confirmation is a separate UI requirement.

A shared Client-lifetime ledger records Reserved, Sent, Removed, known-unwritten
Rejected or Unknown. Receipt delivery does not depend on space in the view-update
queue. Only an exact successful receipt binding Session, index and operation
finishes as Removed. Every written error, malformed receipt, timeout or lost
reply is conservatively Unknown. The old inventory is retired after Removed or
Unknown; no current list is fabricated. A fresh explicit read does not reconcile
an operation and cannot clear Unknown, even when it returns an empty inventory.

While a removal is pending or unknown, further reference mutations are blocked
for this Client. Starting work for its Session is also blocked; Stop and reads
remain available. The actor also blocks previously prepared confirmation/commit
while that Session has a pending removal. A separate worker uncertainty identity
survives framed reconnection, so other acknowledgements cannot clear the removal
or cause replay. No automatic retry occurs. Client replacement/restart loses the
local guards; neither is a safe retry mechanism. Profile replacement keeps an
unknown warning in an existing details form, but is not durable reconciliation.

The ledger retains 256 spent operation IDs without eviction for one Client
lifetime and fails closed thereafter. The daemon's distinct receipt cache is
connection-local, globally bounded to 256 completed operations and ten minutes.
It is not durable idempotency. A later explicit TUI action needs its own informed
review; native controls provide no Unknown override.

## Headless evidence and remaining gates

Tests exercise exact selection of duplicate references, stale callbacks, stored
rows, IME composition, default Cancel and restored read focus, Profile replacement,
all 64 inventory positions and keyboard reachability at simulated 200% scale.
Pure/actor/framed tests cover ownership, missing capability, queue saturation,
spent IDs, pre-write retirement, exact/error/malformed/lost replies, blocked
prepared work and a fresh framed connection whose empty read and Stop receipt
cannot clear Unknown or replay a mutation. These use TestPlatform or in-memory
framing, not native OS input, accessibility or a rendered app.

The real-IPC TestPlatform path uses a private daemon, database and fake provider.
One case confirms the second of two identical directory references and removes
only that occurrence. Another adds a peer attachment after review: the mutation
returns Unknown, retains all four records, and a processed new Send is refused
while its draft remains. Both check artifact records immediately before/after
the attempted removal, stored bytes, reference-target sentinel, no appended
messages and zero provider calls. This proves neither exhaustive schedules nor
native OS execution.

Core/protocol, manifests, lockfiles and workflows are unchanged from the reviewed
`d28c7cf` boundary. The new native increment requires independent review before
publication. Production streaming remains disabled and native Queue unavailable.
Native Attach/drop/image paste, stored-file removal, external editor/reference
addition, safe deferred Queue ownership and durable reconciliation remain.
Hosted desktop CI is still blocked by GitHub payments/spending; root-only CI
excludes optional desktop. The unresolved foreground approval/uncertain launch
has not been retried or bypassed. Native Mac/Windows input/accessibility/scaling,
platform execution, performance and licensing/distribution gates remain open.
