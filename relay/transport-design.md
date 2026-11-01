# Bounded relay transport

This source checkpoint contains shared/browser transport and an optional Core
connection hook. The Rust endpoint pumps and Worker/dev room ledger are authored
in separate integration branches; until those branches are combined and the
complete end-to-end gates pass, this is **not completed #638 behavior**. No hosted
deployment is performed.

The design keeps the existing 32 MiB plaintext IPC ceiling. AES-GCM adds a
16-byte tag; base64 and envelope overhead require at most
`4 * ceil((32 MiB + 16) / 3) + 256` encrypted bytes. Opaque ASCII fragments
are 64 KiB, and each actual wire envelope must fit another 1024 bytes. This
avoids the hosted Durable Object's 32 MiB received-message ceiling without
lowering the logical IPC ceiling. E2E frame authentication/sequence v2 and
room authentication v3 remain distinct and unchanged.

Before daemon-authorized E2E readiness, only bounded (2048-byte) handshake
traffic is eligible; `Assembler` refuses to reserve bulk memory. After ready,
each direction has one ordered logical reassembly and an absolute 30-second
assembly deadline. Invalid declarations, duplicates, reordering and expiry
abort that assembler. The returned completed-frame lease remains charged
through the consumer's decryption/handling, including disconnect while that
consumer still holds bytes; its final delivery ACK must wait for consumption.
The wire-only budget charges the retained byte array plus a worst-case
2-byte-per-character JS string. Decrypt/plaintext/provider allocations require
separate endpoint reservations, not an implied exemption.

Delivery receipts are random 128-bit tokens, bound to the actual destination
socket identity and generation. An ACK payload cannot choose another CID's
window; guessed, old-generation, duplicate or pre-ACK tokens refund nothing.
Only receipt of a token frees its exact recorded byte charge. Calling
`close()` does not free credits: closing sockets and their native buffered
writes remain charged/count against admission until actual teardown (or a
proven receipt). Persisted attachments contain bounded receipt metadata, never
ciphertext; restoring them preserves expiry, generation and closing state.

A direction has at most eight deliveries. Each CID/direction owns one complete
wire-fragment progress reserve; only excess uses the shared 1 MiB pool. Across
16 clients and two directions the raw bound is
`32 * (64 KiB + 1024) + 1 MiB = 3,178,496 bytes`. A fourfold copy charge is
12,713,984 bytes, leaving 4,063,232 bytes of the proposed 16 MiB room policy
for bounded control/transient processing. These are retained-data accounting
bounds, not an OS/RSS or garbage-collector guarantee. Integration must enforce
client/direction counts and bound controls, rates, parser transients, closing
sockets and native runtime queues before claiming the complete policy.

Remaining integration work includes persistent DO alarms/rates/readiness,
reserved controls and actual close/error cleanup, fair per-CID endpoint pumps,
absolute handshake deadlines, browser ACK handling, bounded daemon producers
and encryption/reassembly accounting, and private network tests. In particular,
no shared event-loop branch may wait on a single CID's duplex write or ACK.
A stalled CID must not borrow other CIDs' progress reserves.

Existing 20 MiB attachments are uploaded in 256 KiB pieces. Positive fixtures
must cover sequential uploads and a full 32 MiB RPC through fragmentation,
not merely assert constants. Hosted rollout requires matching versions and is
separate from source validation. #638 stays open until end-to-end criteria pass.

Primary references retrieved 2026-11-01:

- [Durable Object limits](https://developers.cloudflare.com/durable-objects/platform/limits/)
- [Worker limits](https://developers.cloudflare.com/workers/platform/limits/)
- [Hibernation and attachments](https://developers.cloudflare.com/durable-objects/best-practices/websockets/)
- [Worker WebSocket API](https://developers.cloudflare.com/workers/runtime-apis/websockets/)

The next integration seam uses pre-send permits: the room reserves one maximum
wire envelope before asking the sender to transmit. Redemption consumes that
single-use sender permit and generates a different random destination receipt.
The sender cannot pre-ACK its permit as though it proved receiver drainage.
Unused permits may be refunded when their source closes; sent UP deliveries
survive Browser close as orphan metadata until the actual daemon destination
proves receipt or actually closes. Active and orphan CIDs share the same 16-slot
admission cap. A single compact durable ledger is the planned source of truth
for hibernation and lifecycle metadata, avoiding competing live/retired tables.


## Endpoint retention and producer boundary

The browser retains a shared 512 MiB logical transport budget across reconnects.
A request reserves its serialization/encryption peak before joining the send
chain; at most 64 RPCs may be pending. After encryption, the plaintext reference
is discarded and the lease shrinks to the actual encoded string and eight bounded
wire-copy lookahead slots. An isolated full-size request may therefore receive a
full-size response before its last outbound credit arrives. Completed incoming
assemblies keep their wire lease through decryption, with an additional explicit
plaintext/crypto reservation. Old crypto callbacks cannot publish a channel or
application message into a replacement connection. These bounds concern retained
transport work, not the rendered UI, arbitrary application state, native browser
buffers, garbage collection or OS RSS.

Core's optional relay connection hook serializes each outgoing typed message
synchronously before it can enter a queue, future or blocked producer. A bounded
counting writer checks the 32 MiB IPC ceiling before allocating the encoded Vec;
the second writer cannot exceed that measured size. Oversized RPC replies become
a small typed error that explicitly warns the operation may already have applied;
it must not invite an automatic mutation retry. Ordinary local IPC keeps its
existing typed queue and behavior.

The per-Daemon relay pool admits at most 16 owners, including canceled clients
whose tasks or leases still retain buffers. Each owner has a 4 MiB small lane and
a 320 MiB bulk ceiling; all owners share a 640 MiB bulk ceiling, for a maximum
704 MiB logical charged pool. Bulk pressure cancels a different oldest stalled
credit waiter after at least one second; it does not refund its leases until the
actual holder drops them. A healthy client's small lane cannot be borrowed by a
bulk sender. Before allocating an incoming encrypted envelope of `E` bytes,
Rust reserves `3E + 2 min(32 MiB, 3 ceil(E/4)) + 16 KiB`. The third envelope
copy covers overlapping old/new `serde_json` escape scratch during a `Vec`
reallocation: a 32 MiB raw key followed by one escape can temporarily retain
32 MiB of ciphertext plus old 32 MiB and new 64 MiB scratch allocations.
The subsequent decode/plaintext phase uses the same preallocated lease.
At the maximum encrypted envelope (44,739,520 bytes), this is 201,343,808 bytes
(about 192.02 MiB); together with one maximum serialized reply's 67,112,960-byte
lease it is about 256.02 MiB, within the unchanged 320 MiB owner ceiling. Tiny
messages use proportional decode bounds and keep their independent small lane.
The cipher-shape visitor borrows canonical ASCII traffic strings and constructs
constant errors directly for unknown/duplicate keys and every wrong JSON type;
it never first builds an attacker-sized escaped diagnostic.
Relay Core connections have eight queued work items and eight
concurrent control handlers; incoming byte leases follow parsed requests through
their queue/task lifetimes. The serialized output queue holds at most 64 items
and is also constrained by byte leases. Persistent relay subscriptions accept at
most 1024 Session entries and 64 KiB of UTF-8 Session identities, with a 256-byte
client label; refusals preserve the previous filter and label. The relay-only
interruption-cut map has the same 1024-entry/64 KiB identity ceilings. Its sole
FIFO worker captures the current request's cut before pruning cuts at or before
that arrival, so newer interrupts continue to protect every older queued send.
Overflow cancels that CID and its queued work rather than dropping an old cut.
These independently bounded metadata maps remain subject to the 16-owner cap;
ordinary local IPC retains its existing subscription/interruption policy.
Explicit disconnect commands use a 64-command channel. A full, closed or absent
relay command channel returns typed BUSY with an explicit not-queued diagnostic;
only accepted commands receive the existing disconnect audit entry. Unknown
client identities retain the ordinary disconnected=false result.
Arbitrary Store/business-handler heaps, query result construction and parser object expansion are separate concerns
(#642/#860), not claimed capped by this transport pool.

The matching wire version is selected with `transport=1`. Native socket and CID
generations are separate: room controls/ACKs use the actual native recipient's
generation, while logical data/permit/receipt packets use the browser CID's
generation. A daemon lifecycle close is `{relay:"client_close",cid,g}` with
that exact CID generation; a delayed close cannot retire a replacement generation.
Logical close immediately cancels its endpoint, but neither releases sent-UP
receipts nor frees a CID whose browser native socket is still alive.
Bulk grants wait until **both relevant readiness control consumers**
have ACKed; merely issuing a ready control is insufficient. A pending duplicate
hello may receive its original proof without resetting the handshake deadline.
After E2E readiness, known hello/confirm replays are ignored without rekeying or
subscribing again; a new hello closes that CID with 4002 and requires a fresh
WebSocket generation. A daemon teardown/replacement also closes previously bound
browser generations, so old assemblies, permits and crypto cannot cross identities.

The server ledger counts active, closing and orphan-UP CIDs in the same 16-slot
limit. Browser teardown cannot refund data already delivered to a daemon; only
that actual daemon's receipt or actual teardown can release it. Ready-control
receipts and pending lifecycle announcements are bounded durable metadata, never
persisted ciphertext. Reserved progress controls and alarm restoration are part
of the server implementation and require the corresponding native SQLite/wire
gates before any completed resource-safety claim.


Before materializing any typed relay request, Core performs a non-collecting JSON
shape prepass with serde's default 128-level recursion limit. The existing
`4 * wire_bytes + 4096` reader reservation grows before typed parsing to
`7 * wire_bytes + 4096`, plus 256 bytes
per JSON node (including keys), 512 per array, 2048 per object and 256 per map
entry, using checked arithmetic and the existing 320 MiB client ceiling. The
node/container terms cover BOTH Serde's temporary tagged-enum Content tree and
the final typed/Value representation, including Vec allocation floors and
BTreeMap nodes/splits; this is a conservative implementation budget, not an
allocator/RSS promise. The prepass collects no tree or node list, drops escape
scratch before typed parsing, and refuses a wide compact AST before it can enter
the work/control queues. A full 32 MiB string payload with few nodes remains
eligible. The enlarged byte lease follows the typed request until its worker or
control task drops it; cancellation alone never refunds a retained AST. Resource
refusals use a fixed diagnostic and close only that CID without echoing its data.
Already canceled bulk-eviction victims retain their charges but are skipped when
choosing the next stalled owner, so a stuck cleanup cannot monopolize victim
selection.

Relay parse failures drop the original diagnostic before recovery. The error
path streams only the envelope ID and at most 256 bytes of request-type metadata;
all other fields are ignored without rebuilding a Value tree. Overlong request
types produce UNSUPPORTED without echoing the type, and other malformed requests
use a fixed BAD_REQUEST diagnostic. The ordinary local IPC error path is unchanged.

Before typed parsing, the shape pass also reserves 32 KiB of fixed diagnostic
headroom and three times the maximum single decoded string/key's Debug-escape
expansion beyond its original bytes. The 7× base covers the retained raw line
(N), parser escape scratch (≤2N),
owned Content strings (≤N), and up to 3N for an ordinary diagnostic's old/new
String allocation or boxed shrink overlap. The additional 3× expansion covers
control/nonprinting characters in the initial diagnostic before the relay error
arm can drop it. The scan allocates no formatted string (printable ASCII is
exact; other
scalars use escape_debug). Pathological control-character payloads may be refused
by the byte budget before formatting, while ordinary full-size ASCII/base64
strings remain individually eligible under the 32 MiB frame ceiling. Other
already-held input/output buffers can cause an earlier, explicit resource-quota
refusal; that does not lower the logical frame ceiling. Tests guard the pinned
expected-variant
list and compare escape counting against Rust str Debug.
