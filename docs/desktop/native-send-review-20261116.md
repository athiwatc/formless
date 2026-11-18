# Native confirmation of complete pending files before Send and Steer

The daemon-backed composer now requires confirmation of its exact draft and
complete pending attachment metadata before every Send or Steer, including an
empty inventory and a quote that needs no cost consent. Opening the review,
reading metadata, copying details and receiving forecast/consent replies never
start provider work. The initial focus is Cancel. Confirm is an explicit user
action; the view rechecks draft, active input composition, Session selection,
modal ownership, Client identity, connection and action before dispatch.

The Client checks that Ping advertises both `pending_attachment_inventory_v1`
and `reviewed_attachment_send_v1`. It reads the complete bounded inventory and
then requests the daemon forecast. Unsupported, foreign, incomplete or oversized
metadata fails closed with a TUI/update handoff. There is no fabricated empty
inventory or fallback to legacy Send. Direct raw Send intents are refused by
the actor, so the composer preparation path cannot be bypassed that way.
Resume keeps its existing forecast/recovery path and does not send this draft.

The modal displays the full draft and every accepted record as JSON-escaped
text, including names, private paths, recorded sizes, references, duplicates
and unknown fields. Copy details contains `{forecast,draft,pending_files}` with
complete accepted values; control/directional escaping preserves JSON meaning.
Neither the native display nor copying opens attachment paths or reads their
bodies. The daemon's existing local forecast may inspect files when estimating
inputs; that remains local and starts no provider request.
The normal editor still supports its existing 1 MiB draft limit, but native
send review accepts at most 64 KiB of draft UTF-8. Larger drafts stay editable
and untruncated and require the TUI. Inventory bounds remain 64 rows / 64 KiB
stored metadata and 128 KiB native reply JSON after wire decoding. These limits
do not establish a total process-memory, native rendering or latency bound.

After confirmation, the actor writes `SendReviewedMessage` with the full
expected inventory, exact draft, selected mode and one action UUID. The daemon
compares and consumes that metadata with message append in one transaction;
another client's changes cause rejection rather than unreviewed consumption.
Metadata equality does not freeze mutable referenced contents, identify a
particular storage row, establish permission or grant Tool authority. All
existing forecast, provider, trust and security rules still apply.

Queue is unavailable in the daemon-backed composer and commands. A persistent
composer notice directs deferred messages to Formless TUI. There is no native
Queue button or legacy Queue fallback. The synthetic fixture retains its old
Queue demonstration. The daemon endpoint also rejects implicit deferral when
a Run can no longer accept steering. Deferred attachment ownership is required
engineering before Queue can be restored safely.

The preview client requires `reviewed_send_mode_v1` alongside the inventory and
reviewed-send capabilities. Older peers can still be used for other supported
workflows. They receive no native Send or Steer preparation or mutation from
this client. The matching Core rejects a reviewed Send if the Session has become
active and rejects a reviewed Steer if it has become idle. This prevents changing
the confirmed mode after review. Exact Run identity binding remains open.

## Ownership and uncertain outcomes

Preparations and unwritten sends bind the Client UUID, physical connection,
view epoch, selection generation, Session and observed status revision. Changes
retire pending reads, quotes, consent and unwritten requests. A sent request is
not discarded when the view changes: its receipt and uncertainty must survive.
Copy, Confirm, Cancel and backdrop controls bind the presented action UUID;
callbacks for a retired dialog cannot copy or cancel a newer one. Composition
and draft equality are rechecked immediately before the view commits.
These local fences are not an atomic guarantee against unobserved external
changes; the daemon transaction provides the pending-metadata comparison.

A successful acknowledgement clears the retained unknown identity only when
its started or steered flag matches the reviewed mode and it includes a bounded
Run ID. A reply for the other mode leaves the outcome Unknown. A failed or
missing response, malformed success, queued response, timeout or partial write
remains Unknown. Even an error can occur after message persistence. The Client
blocks new work for that Session, preserves the draft and directs inspection
to the TUI. Stop, snapshots, another Session's action or unrelated replies do
not reconcile it. No action is automatically retried, including on reconnect.

This deliberately treats even a known daemon validation error conservatively,
because the current error envelope does not certify a before-append stage.
There is no native override or speculative recovery from an empty inventory.
The daemon receipt lasts ten minutes on one connection; the native unknown
guard lasts for this Client worker, including physical reconnects. Closing the
Client, replacing its Profile or restarting loses that guard. None is a safe
retry mechanism or durable idempotency guarantee. A future reconciliation UI
needs an authoritative operation receipt protocol.

## Evidence and remaining acceptance

Pure actor tests cover exact draft/Session/capability checks, raw Send and Queue
refusal, unavailable/foreign metadata, required confirmation, complete payload
and key binding, all ownership fields, unwritten retirement and written receipt
retention. Framed connection schedules cover lost, failed and malformed replies,
including a postcommit-style error; only a specific success retires Unknown.
View tests cover no-price/empty confirmation, focus and keyboard reachability,
complete 64-record copying at simulated 200%, stale dialog callbacks and foreign
Client/inventory rejection. Existing draft/IME, cancellation and consent tests
continue to use the new native review payload.

The private live-view fixture uses the configured primary Send key, actual modal
controls, real IPC and a fake provider. Accepted Send consumes one reviewed file;
accepted Steer explicitly reviews an empty inventory and extends the live Run.
A peer attachment change after opening review rejects the Send, leaves both
files pending and the transcript unchanged, preserves the draft, makes no
provider request, and refuses a second Send after its Unknown notice is
processed. This is TestPlatform evidence, not real OS keyboard/IME acceptance.
Commands, counts, source/log hashes and limitations are frozen separately in
`native-send-checks.json`. Exploratory logs are retained.

Required native Attach/drop/image paste, safe Detach identity, external-editor
and reference mutation, deferred Queue ownership, durable reconciliation and
broader parity remain unfinished. Production streaming remains disabled.
Hosted desktop CI awaits billing/spending restoration; native Mac/Windows
IME/accessibility/clipboard/scaling/picker/filesystem/lifecycle, Windows/Linux
execution and native performance remain open. Licensing/distribution and
signing/notarization/clean install/update gates are unchanged. No native renderer,
uncertain foreground action, billing control or blocked hosted job is used.
