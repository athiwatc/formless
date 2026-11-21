# Run and Step status read fence — 2026-11-15

This increment fixes a reproduced same-sequence metadata race. A GetSession
request can sample an idle or paused Run, then arrive after RunStarted and a new
live attempt. Comparing the stored message sequence alone admitted that old Run
status without replacing the accepted live prefix. Step inventory has the same
observed-lifecycle overlap risk.

Every selected-Session RunStarted, RunFinished, RunPaused, StepPending and
StepResolved event now advances a checked connection-local status revision.
Canonical snapshots, live barriers and live status reads capture that revision.
An obsolete response is refused before JSON decoding. Status reads keep the
accepted live basis and request fresh metadata; overlapped barriers retire and
retry, falling back to canonical-only after three consecutive overlaps. A fresh
owned response restores controls. Foreign Sessions do not advance the revision;
exhaustion closes the connection rather than wrapping.

The UI receives a Client/connection/selection-bound invalidation. Run state
becomes unknown, recovery and Step controls lose their old inventory, and old
forecasts are retired. Draft entity, text, live prefix and stream view epoch are
preserved. The actor independently retires obsolete forecasts and unsent Step
inspections/decisions before UI callbacks run; already-sent Step decisions retain
their receipt/uncertainty handling.

Validation includes a failing regression at the previous source, passing model
schedules for malformed obsolete replies, canonical fallback, bounded barrier
retry, fresh baseline recovery, foreign events, counter exhaustion, forecasts
and unsent/sent Step ownership. A GPUI TestPlatform case checks draft/live-basis
preservation and unknown-to-fresh Run state. A private Unix IPC peer holds a
pre-Run status response, delivers RunStarted and an admitted prefix, then releases
the obsolete same-sequence status. It verifies a fresh status and continued tail
on the same basis without reconnect or replacement baseline.

This fences lifecycle changes observed by this connection. It does not add an
atomic daemon metadata revision, durable mutation deduplication, total-memory
bound or native latency guarantee. Production streaming remains disabled. Native
IME, VoiceOver, clipboard, scaling, OS picker/filesystem, Windows/Linux GUI,
performance and distribution acceptance remain open. Pending foreground approval
and the prior uncertain launch remain untouched. No remote publication occurs.
