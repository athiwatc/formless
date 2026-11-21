# Observed producer cursor floor — 2026-11-15

The validation reader previously tracked the barrier cursor and active attempt's
last cursor, but ignored a future Delta or Closed while idle without remembering
its cursor. An adversarial peer could then send a lower-cursor Start and a new
zero-offset Delta, reviving a prefix despite contradicting already-observed
producer order. The new regression failed at the prior source (`Started` instead
of `Reload`). This is admission hardening under adversarial ordering, not evidence
that the current serialized producer emits reordered frames.

A gate now keeps one observed cursor floor, initialized from its owned barrier.
Every scoped, correctly identified post-barrier attempt frame advances it,
including ignored idle Delta and Closed frames. A repeated or decreasing cursor
retires the gate before Start admission or append. Cursor gaps remain valid:
other Sessions consume intervening global cursors. Wrong attachments and foreign
Sessions remain inert. Frames at or before the barrier remain ignored while
idle, without regressing or poisoning the observed future floor. Epoch and
identity mismatches still fail closed before cursor admission.

The floor replaces the redundant active-attempt cursor. It is not a retained
prefix, replay buffer, contiguous-cursor requirement or protocol change. Fresh
owned barriers create fresh gate state. Live and staged-reader regressions check
retirement, absence of any reordered text and recovery of a later eligible
attempt. Private IPC schedules exercise both ignored Delta and ignored Closed,
then an out-of-order Start, without reconnecting; each requires one fresh barrier
before a valid future prefix becomes visible.

Production streaming stays disabled. This source-level ordering check does not
establish an atomic daemon status revision, native latency or total-memory bound,
durable receipt deduplication, general mid-attempt replay, distribution clearance
or native/platform acceptance. No native window, foreground launch, real Profile,
provider, credentials or Tool is required by the adversarial IPC fixture. The
original conflicting checkout and unresolved foreground approval are untouched.
