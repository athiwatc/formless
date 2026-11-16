# Explicit Session model catalog discovery

Session details → Model offers **Read catalog (contacts provider)**. Opening
the form, typing, filtering, choosing a suggestion, metadata refresh and
reconnecting never initiate a catalog request automatically. The button uses
the existing daemon `ListModels` request. That request may contact the configured
default provider and its authentication service, waits up to the existing daemon
15-second provider timeout and refreshes the daemon's model-window cache. It
requests neither a Run nor a model test. It is not an offline-only operation.

The snapshot combines provider-listed model IDs, configured alias keys,
`role:name` selectors and recent Session selectors. Duplicate exact selectors
combine provenance labels. Alias targets and Role provider/model routing remain
daemon-owned; the client does not rewrite selectors or claim availability.
Recent entries may be stale. If provider listing fails, stored choices remain
available with an explicit partial-result notice. Provider error details and
unused metadata/test records are not displayed. A contradictory nonempty
provider-model list accompanied by an error is rejected whole.

The decoder accepts at most 4,096 raw rows and 2,048 unique selectors. Each
selector has the existing 512-byte model limit. Oversized or structurally
unsupported replies fail whole. Empty, deprecated `auto`, control/bidi-bearing
and otherwise invalid native selectors are excluded whole, not normalized.
These are presentation bounds after the existing wire decode, not a total
transport-memory or daemon/provider work bound. The native UI shows at most
12 choice buttons per page, with local case-insensitive substring filtering.
A read starts unfiltered. Subsequent Model draft edits, including edits made
while waiting for the reply, filter locally; clearing the draft shows all
choices again.

Choosing a row replaces only the current Model input through its native text
edit path, preserves undo and focuses that input. The current exact input text,
form revision, catalog revision, displayed page/filter, Client instance and
physical connection must
still match. Composition, retired forms, pending metadata writes, changed
fields, changed queries and stale callbacks cannot overwrite a later draft.
Choosing a row sends no setting request. Explicit Save retains the model/effort
paired-read guards and visible paid-test notice documented in
[model selection](session-model-selection-20261115.md).

At most one catalog RPC is outstanding in a connection. Closing and reopening
a form does not replay it; a second read may need to wait for the old reply.
Results bind the requesting Client, physical connection and unique read token.
A reply may populate the same form's read-only suggestions while another field
is open. Reload, Profile attachment and physical connection changes clear the
pending token, cached snapshot and callbacks; the old reply cannot repopulate
that new form state. No catalog cache is persisted across launches.

Validation uses bounded decoder/actor tests, GPUI TestPlatform and a private real
daemon with a local fake provider. It covers explicit-only requests, pagination,
filtering, exact Role fill, separate Save, undo, simulated 200% keyboard access,
marked text, captured stale-page/query/field/read callbacks, the actual
outstanding read arriving after Reload, and Profile
replacement. The real-daemon case counts exactly one explicit provider catalog
GET, observes no POST to the Run endpoint and checks that only explicit Save
persists the Role selection. Automatic model tests are disabled in that fixture;
configured paid-test execution is not validated. These checks do not establish
native OS keyboard/IME, VoiceOver/NVDA, platform execution or performance
acceptance. Production streaming remains disabled. Per-provider catalog
management, Role/provider editing and required attachment/reference parity
remain separate work; this is not GPUI completion or distribution clearance.
