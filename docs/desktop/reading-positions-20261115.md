# Window-local Session reading positions

Continues local `bbde4a2543e69e1f1a2eae2fa321e12bc233e303` in the isolated
takeover checkout. The parent reported independent review found no confirmed
blocker in that comparison commit; source and evidence hashes were inspected,
not tests rerun. This increment needs exact-patch review. Epic #1753 and
native/product/integration acceptance remain open.

## Behavior and ownership

Leaving a loaded Session remembers the persisted message at the top of its
conversation if the user has scrolled away from latest. Returning first shows
an unloaded view, then restores that message after the existing Session and
selection-generation checks accept a canonical snapshot. A reply for another
Session or an older selection cannot install data or consume the pending
position. Leaving a still-loading view never overwrites its earlier saved
position. A normal refresh does not replay that stored bookmark.

Positions use Session ID and persisted message sequence, not list index or
pixel offsets. Removing earlier messages or appending later ones cannot
retarget the saved position to a neighbouring message. If the saved message
is absent, the view follows latest, forgets that position and shows a polite
status notice. The notice clears on a later accepted snapshot or Session
change. Returning to latest before leaving also forgets the older position.
Only finished message identities can be saved; the provisional streaming row
is excluded even in the synthetic reducer seam. Transport deltas remain off.

Canonical snapshot replacement overlapping the current reading row now
preserves its message identity too. The new regression first reproduced
refresh drift from message 80 to 75 (`reading-position-focused-rerun.log`).
The replacement finds the current persisted message again, rather than using
an invalidated positional list anchor. An unchanged prefix before the reading
row keeps the kit's existing anchor handling. This is coarse message-level
restoration: after an overlapping replacement it aligns to the message's top,
not its former intra-row pixel offset. It establishes no snapshot/event
ordering guarantee and does not enable streaming.

The memory policy is 64 least-recently-used Session positions, at most 256
UTF-8 bytes per Session ID and one sequence number. The one pending Session
identity has the same bound. No message body, draft, selection, pixel offset,
whole-output modal, dialog, preview-page range or second transcript is kept.
Directory pruning examines available Sessions once using a temporary set
bounded by these 64 IDs. Unavailable IDs are pruned; archiving an open Session
parks its position before the view is cleared, permitting later explicit
restore/open. Deleted entries are forgotten. Profile attachment replacement
clears positions even if connection counters and Session IDs repeat.

Positions are for this window, not persisted to disk or preferences. Eviction
only loses a reading location and never user-written text. This slice adds
no RPC, daemon mutation, clipboard operation, filesystem write or automatic
Session activation. Existing composer/draft/undo and modal guards retain
ownership; returning does not reopen an old dialog or whole-output view.

## Validation and limits

Two pure-model tests cover independent Session keys, replacement, follow-latest
forgetting, LRU touches/eviction, invalid identity bounds, pruning and clearing.
Six headless shell tests exercise actual directory/key activation and the
recording client boundary: delayed/stale/foreign snapshots; restoration after
removed/appended rows; current-reading preservation on refresh; exact draft
retention; leaving a still-loading view; wheel-based latest resumption;
archive/restore and deletion; Profile replacement with repeated IDs; modal
closure; missing/empty snapshots at 480x360 and 200%; and exclusion of a
provisional row through the synthetic reducer with its simulated flush clock.
Restoration emits no wire request.

The initial failures remain in sibling evidence logs: a test-only resize API
signature error, wrong synthetic live-row identity, missing test observation
for the kit's jump button and an offscreen test-setup header. Setup now uses
the real directory/key path, wheel events and the harness's simulated bounds
and flush clock. The snapshot drift failure was fixed in implementation,
not weakened. Exact final commands, exits, source/log/artifact hashes and
suite totals are recorded in the evidence manifest.

Final offline validation passed 212 GUI-free model tests, 217 GUI/Thai model
tests, 184 headless shell tests and two CLI tests, strict desktop clippy
(including compilation of the unrun renderer target), Windows GUI-free
all-targets static checking, formatting and whitespace checks. The focused
run passed both model cases and all six new shell cases. No dependency,
protocol, daemon/core, relay or manifest/lockfile changes were needed.

These tests use TestPlatform and recorded update seams, not real OS keyboard,
IME, screen-reader, clipboard, trackpad, fonts or display scaling. No daemon
code or request schema changed; the previous private-daemon/fake-provider
comparison evidence remains at its recorded revision and is not represented
as a new-head native acceptance pass. No renderer or desktop app was run.

## Integration holds

The original conflicting partial checkout remains untouched with ownership
unresolved. Existing Formless Sol Validation approval and the prior uncertain
launch/control outcome remain unresolved and were not repeated or bypassed.
Native IME/accessibility/input/scaling, Windows/Linux GUI, filesystem/picker,
remaining parity (including fork and richer/persistent view state), performance
and distribution acceptance remain open. No push, PR update or merge occurred.

Review this exact increment, resolve ownership and native gates, recheck live
main/#1776/#2081, then validate the agreed combined revision before integration.
A clean isolated branch and passing synthetic tests do not complete the user's
finished-GPUI/clean-main request.
