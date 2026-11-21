# Complete pending-file metadata inspection

The missing shared-state boundary for attachment controls is now available as
`ListPendingAttachments { session_id }`, advertised by
`pending_attachment_inventory_v1`. This is an additive protocol-v1 request;
existing clients and mutation requests retain their shapes. Older daemons may
answer unsupported. The native reader reports unavailable and directs the user
to TUI/update; it does not invent an empty list. No read is automatic on opening,
selection, field editing or reconnect, and existing Send behavior is unchanged.

The daemon reads Session existence/deletion and the pending rows in one read
transaction, returning `{session_id, attachments}` in stored FIFO order. Unknown
or already-deleted Sessions fail. The strict storage path accepts at most 64
records and 64 KiB total stored UTF-8 JSON metadata. It fetches at most 65 rows;
a SQL CASE projection avoids loading an oversized individual metadata string.
Overflow, corrupt JSON or non-record metadata fails the whole read. No row is
silently skipped, and reading never consumes, detaches, stages or expires files.
The existing permissive internal reader is unchanged for existing workflows.
Attachment paths are not opened, resolved or statted by this request.

Session details offers Read pending files after the existing metadata/export
controls. It targets the chosen Session entry, which may differ from the active
conversation. A snapshot shows eight records per page, recorded bytes and file
or directory-reference kind. Names/paths are visual previews with directional
and control characters made visible. Copy complete pending metadata preserves
all accepted records, duplicates, ordering and unknown fields as compact JSON;
directional characters are escaped without changing JSON meaning. Metadata
can contain private daemon paths and other private values. It is not file-body
copy. The native reply budget is 128 KiB after existing wire decoding, with
bounded used fields (name 512 bytes, path 4,096, ID/MIME 256). These are not a
total transport-memory or daemon-work guarantee.

Read, page and Copy callbacks bind the Client instance, physical connection,
Session/form revision and current accepted read token. Both actor and view
validate Session scope. Late, out-of-order, wrong-target and replaced-Profile
results cannot overwrite a new form. Reloading Session metadata or replacing a
Profile/connection retires cached file state; closing drops it. Marked text
prevents a read without losing the metadata/composer draft. At most two reads
are outstanding per connection; a closed form's read may occupy a slot until
its reply or reconnect. There is no automatic replay or refresh.

This is a metadata snapshot, not a write lease, deduplication receipt, guarantee
that a file still exists, proof of current pending state or reconciliation of an
uncertain write. Another client or a Send may change/consume the list immediately
after it is read. Attach/drop/image paste/path mutation, safe detach identity,
uncertain-write handling and mandatory attachment/send-review integration remain
required work. The snapshot must not be used to clear a write-uncertainty guard.
This increment supplies the inspection boundary rather than a partial picker.

Core tests cover scope, deleted Sessions, FIFO completeness, non-consumption,
corruption, per-record/aggregate metadata and row overflow. Protocol coverage
checks the additive request and advertised capability. Domain/actor tests cover
used-field/reply bounds, duplicates/unknown-field preservation, wrong targets,
out-of-order replies, read-slot bounds and older-peer errors. TestPlatform checks
all 64 records at simulated 200%, full JSON copy, composition, stale callbacks,
refresh, Profile replacement and foreign typed reports. A private daemon/fake
provider case stages files through existing peer APIs, inspects two records
without consuming them, observes peer detach only after explicit refresh, and
observes an explicit Send consume the remaining reference. Reads initiate no
provider POST; the single explicit Run accounts for the one fake-provider POST.
No real Profile, paid provider or attachment picker is used.

Native OS keyboard/IME/VoiceOver/NVDA/clipboard/scaling, platform execution,
performance, distribution and full attachment/reference parity remain open.
Production streaming remains disabled; publication requires independent
exact-head review and existing acceptance gates.
