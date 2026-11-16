# Schema-67 GPUI preview integration candidate

This branch separates the usable ordinary GPUI client and its schema-free Core
APIs from the later durable-journal/database-identity prototype. It starts at
reviewed production-parity boundary `032703d` and preserves the full later branch
at `8cd88cfa511e1d09a4a243fed4f033b81bf6f3ff` on
`codex/gpui-shipping-boundary-20261116-edc5808`. No branch, commit, partial checkout
or uncommitted work is discarded. No personal Profile/database was opened.

## Included and separated

The preview retains the published `fd1e8ec` candidate and all intervening reviewed
ordinary workflows through `032703d`: Step on/off and decisions, selected
multiple-request Tool approvals, model/Role/effort edits and catalog suggestions,
complete pending-file inspection, transactional Send/Steer review and exact
reference-occurrence confirmation. The normal client connects to an existing
daemon, uses canonical snapshots with token streaming disabled, and exposes only
capability-supported controls. Queue remains unavailable. Ordinary guards remain
Client-lifetime and bounded; they are not durable across app/client replacement.

The preview's Core writes schema 67, matching verified main `9810f8f`. Its ordered
DDL/migration prefix is byte-identical to that main's prefix. Existing schema-67
sessions, messages and pending metadata use the same tables; the newer reviewed
RPCs do not add a migration. For full new workflows, use the matching preview
Core with this client; an older main daemon may lack those optional APIs and
leave their native controls unavailable. The desktop does not start a daemon or
open its Profile storage.

The schema-68 durable success table, schema-69 database identity, bound/durable
RPCs, native journal/descriptors, receipt lookup UI and pinned preflight are not
in this preview's ancestry or source. Their complete implementation and evidence
remain on the full prototype branch. They are not reverted or deleted there.
The two reviewed GUI-only packaging scripts are copied unchanged from `8cd88cf`;
the graph/artifact guard therefore accompanies this preview without bringing its
schema changes. The existing test-support-only import guard in the reference
view is also carried over, fixing GUI-only lint without behavior changes.
No new runtime feature or enabling switch is introduced.

## Compatibility evidence and exact limits

New owned-fixture tests open a database constructed at schema 67 with Unicode
history and pending-reference metadata. They verify preservation, a reviewed
rename/removal, reopen, unchanged version and absence of the two prototype
tables. Additional future-version-stamp probes at 68 and 69 must be refused
without downgrading the version or replacing sentinel data. These probes are
not genuine prototype backups and do not establish an approved downgrade or
restore procedure.

A gated library test uses the actual ordinary Client `connected` loop and the
preview Core request handler over an in-memory framed stream. It loads exact
Unicode transcript text, renames a Session, reads the complete reference
inventory, receives one successful removal and loses the next removal's reply
after Core commits. On reconnect to a fresh Core instance (with an empty
connection-local receipt cache), the same Client's Unknown guard refuses replay
and blocks Resume while allowing an explicit inventory read. Exactly one
persisted message and one reference remain; the target file is unchanged, no
queued work is created, and a backup still reports schema 67 with neither
prototype table. The peer rejects any unexpected/provider-starting request
before calling Core, and all auto-tests/naming/Skill creation are disabled.

This is real handler/storage/client framing, with a test Subscribe acknowledgement;
it is not native socket/window/IME/accessibility execution, the full application,
a real provider run or a restart-persistent guard. The denied Unix/TCP listener
operations are not retried or counted as accepted. Existing component tests and
new exact-head checks complement this seam; no current-head native acceptance
is inferred from historical evidence.

## Data safety and integration

Do not point this older-schema Core at a Profile opened by the schema-68/69
prototype. Version refusal is intentional; this branch is not a downgrade tool.
Do not copy a prototype journal, reset Unknown, roll back a real database or use
personal data to try the preview. Existing schema-67 Core behavior for upgrading
older-than-67 profiles is unchanged from main; this task tests owned fixtures
only. Client-lifetime Unknown must be reconciled through the TUI before closing
or replacing that Client; no safe-replay permission follows from missing state.

The preview can be reviewed separately as `032703d..candidate`, together with the
already reviewed production increments `fd1e8ec..032703d`. The experimental range
`032703d..edc5808` is deliberately held out. The full branch is not mergeable into
this preview merely because both have passed source review: doing so reintroduces
migration/provisioning/recovery gates. The preview schema-67 regression tests
should fail if a future schema bump is merged without an explicit release choice.

Before publication, reverify main and draft #2081 ownership/head, then obtain
independent review of this selection and its compatibility evidence. A normal
fast-forward from current draft `fd1e8ec` is possible only while that remote head
remains unchanged. Do not force-push or overwrite another writer. No publication
or merge occurs here. Future prototype integration must preserve all work and
resolve schema rollout/recovery and trusted initial pin/storage policy separately.

Native/platform/performance, full required parity/security, hosted billing,
recorded licensing/notice/SDK distribution, signing/notarization and clean
install/update acceptance remain open. The existing foreground approval
`proud-river-57508678` and uncertain launch remain untouched. This is a concrete
preview integration candidate, not completion/merge/release approval for #1753.

The subsequent [atomic model/effort save](session-model-cas-20261116.md) is an
additive schema-67 product increment on a separate branch from the frozen
preview. It closes the stale read/write window without importing prototype
schema changes, journaling, provisioning or recovery machinery. Both frozen
preview and prototype branches remain preserved.
