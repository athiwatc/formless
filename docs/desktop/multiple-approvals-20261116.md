# One-at-a-time review of multiple Tool security requests

The Profile-wide Approvals modal accepts a complete inventory of up to 16
supported Tool requests. Previous request and Next request select one exact
request for review. The position, total, approval ID, Session ID and Tool/version
remain explicit. Only that request's complete validated JSON is displayed and
copied. Changing selection grants nothing, creates a new review revision,
resets evidence scrolling and returns keyboard focus to Close. Enter on that
default focus closes without a decision. Composer drafts remain untouched.

Allow once and Deny retain the original per-call contract: Once scope, empty
path substitutions and remember_denial=false. Before a write, the actor reads
PendingApprovals again, validates the complete bounded inventory, finds the
selected unique approval ID and compares its entire immutable prompt. The
selected request can remain eligible if unrelated complete requests are added,
removed or reordered. A missing, changed, duplicate or unsupported target
cannot fall back to the first row or current position. Observed lifecycle
changes still retire unsent work conservatively. This remains a fresh
read/compare guard, not an atomic guarantee against unseen external activity.

Each raw prompt has a 16 KiB compact serialized-JSON limit; the entire inventory
has a 64 KiB serialized limit. A counting writer rejects excess without
allocating an oversized serialized copy. These checks happen after existing
wire decoding; they are not a total transport-memory or daemon-work bound.
Every row must support the existing complete single-Tool-call schema. Duplicate
approval IDs, unsupported/mixed rows, truncated call evidence, batches,
promotions, wider scopes and path substitutions still hand off to Formless
TUI. Unsupported rows are never silently omitted to make another row eligible.
An oversized inventory is rejected whole. No bulk approval is introduced.

Rendered decisions and navigation bind the current review revision, Client
instance, physical connection and modal. A selection change retires cached
Allow/Deny, Copy and Previous/Next callbacks. Marked text prevents navigation
and decisions. Navigation is disabled after submission or while the Client
has any pending/unknown approval outcome; Reload does not clear that ledger.
A successful decision does not automatically select or decide another request.
Reload explicitly for a fresh inventory. Spent operation/approval receipts
remain Client-lifetime and bounded to 256 operations, with no durable restart
deduplication and no automatic replay after disconnect. Profile replacement
closes the review; reopen explicitly for a fresh inventory in the new Profile.
Captured callbacks cannot act on that new review even if it reuses request IDs
and a connection counter. Physical reconnect within the same Client uses
explicit Reload; neither path replays a decision. Deferred dismissal settles
the cached modal and returns focus to the page. Opening a fresh review always
focuses its own Close handle, including replacement/reopen between frames.

Validation covers inventory and aggregate bounds, duplicate/unsupported rows,
exact selection under unrelated inventory turnover, no batch decision,
missing/changed targets, stale selection/decision callbacks, pending and unknown
receipt barriers, composition and simulated 200% keyboard navigation through
all 16 requests. A private real daemon and local fake provider run two Sessions
with simultaneous Tool approvals. Allowing selected request two executes only
its private marker write, completes only that Run and leaves the other request
pending. Explicit Reload and Deny prevent the other Tool write. Both Runs finish
with four fake-provider POSTs, no persistent Tool grant and an unchanged draft.
Automatic model tests and approval summaries are disabled in the fixture; it
uses no real user Profile or paid provider.

These are source, actor and GPUI TestPlatform checks, not native OS keyboard/IME,
VoiceOver/NVDA, clipboard/scaling or Windows/Linux GUI acceptance. Larger and
unsupported security workflows, Step parity, attachment/reference workflows,
platform/performance and distribution gates remain open. Production streaming
stays disabled; this increment requires independent exact-head review before
publication or integration.
