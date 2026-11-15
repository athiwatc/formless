# Native per-Session model, Role and effort overrides

Session details now includes Model and Effort fields. Model accepts a bounded
model ID, configured alias or `role:name`; blank clears only that override and
inherits the configured default. Surrounding input whitespace is trimmed.
Effort is provider-specific; blank inherits.
The form does not enumerate models, fetch a catalog, edit Roles/providers or
claim that a saved ID/effort is supported by its provider. Unsupported values
can fail when used. The palette description includes model/Role and effort so
these controls are discoverable from the composer through Session details.

The existing daemon setter replaces model and effort together. Each Save uses
the new displayed field and the last confirmed companion, not an unsaved edit
in the other field. Both nullable values must be explicitly exposed by a
complete reply; unavailable or oversized stored values never become a blank
clear. Model IDs are bounded to 512 bytes and effort to 64; line/control and
directional-formatting characters are refused. Legacy `auto` can be inspected,
but a native replacement must choose an explicit model/Role or inheritance,
because the daemon may rewrite `auto` during a setter.

Before any setter, the actor reads the persisted Session list again. It checks
one exact target and the entire expected pair. A changed or unsupported pair
rejects without a setting write. If the desired pair already matches, the
receipt says it was observed: no setter or model test is started. Observed
paired-value changes or deletion retire unsent work; unrelated name/tag changes
do not. This is a read/compare/set guard, not an atomic compare-and-swap against
unseen external activity. The existing protocol and daemon are unchanged.

The model setter may start a configured paid background model test. That fact
is visible in both the scrolling help and a fixed footer next to Save, with a
matching accessible Save label. It starts no conversation message/Run by itself.
A running Run keeps its original model; a later Run uses the saved selection.
The private fixture explicitly disables automatic model tests. No real paid
provider request or user Profile is used by validation.

Starting a selection save synchronously retires old forecast context. New
forecasts and work starts are refused while its read/write is pending; Stop
remains available. A save is also refused while the target's earlier action
receipt is pending. Setter acknowledgements must expose both exact values.
Malformed positive replies stay Unknown. Connection loss retires the form;
there is no automatic retry. Explicit reload and an unchanged Save reread the
pair and avoid another setter. There is no durable model-setting idempotency
ledger or promise across process restarts/unseen intervening changes.

Forms and rendered saves bind the Client instance as well as the form
revision. A new Profile attachment retires all metadata saves, retains drafts
read-only, clears old pending UI ownership and requires explicit reload even
when the new Profile reuses Session IDs and a connection counter. A previous
write may have applied to the old Profile; it is never replayed in the new one.
Form revisions change on opening, attachment, field changes, reload and
acknowledged outcomes. Cached Save callbacks cannot become valid again merely because the
same Session/connection/field is shown. Draft text, unrelated metadata edits,
composer ownership and marked text remain separate. Closing the form does not
undo a Save already requested; Cancel discards only unsaved form edits.

Validation covers domain/actor bounds, paired preservation, conflicts, observed
changes, read-only no-ops, pending-work exclusion, malformed receipts, modal
revision/IME and simulated 200% keyboard access. A private real daemon and local
fake provider validate Role resolution, active-Run retention, next-Run model
selection and clearing both overrides. Private IPC loses the setter ACK,
physically reconnects and verifies that explicit unchanged Save is read-only.
These are headless checks, not native IME/VoiceOver/NVDA/clipboard/scaling,
Windows/Linux execution, paid-test execution or performance acceptance.

Full catalog discovery, Role/provider editing and the other required composer
attachment/reference workflows remain separate work. Production streaming
stays disabled, and publication still requires independent exact-head review.
