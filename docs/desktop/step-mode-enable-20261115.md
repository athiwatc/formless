# Explicit Session Step-mode enable

This increment continues reviewed local `ca0765cb` in a separate branch. At the
start of this work, origin/main remained `9810f8f2`, already included by the
integration. The conflicting original tree and all previous branches are
preserved. Production streaming remains disabled.

## Behavior and scope

The selected owned Session shows its strictly sampled Step-mode state. When it
is known off, Enable Step mode reserves one explicit operation, freshly reads
GetSession and sends only `SetStepMode { on: true }`. Missing or malformed mode,
a foreign Session, a changed view/connection or an observed intervening lifecycle
event prevents writing. Already-on fresh state finishes without a setting write.
This setting is ephemeral to the current daemon. It does not approve, release,
start, resume or retry a Tool or Run. The existing Step decision modal separately
supports Session Step-off while releasing one fully reviewed supported prompt;
idle Step-off, larger/multiple prompts and security approvals remain TUI paths.

The control preserves the composer entity, exact draft, undo and live transcript.
Draft/forecast changes do not cancel a Session mode operation; Profile/selection
and connection changes cancel unsent operations synchronously. Modal/background
and composition guards prevent accidental activation. Keyboard controls remain
reachable at the minimum window and 200% text. Wrapped metadata grows the page
instead of collapsing history; help text is constrained to the available width.
This is TestPlatform layout evidence, not native scaling or accessibility acceptance.

## Transport, ordering and receipts

The operation binds Client UUID, physical connection, selection generation,
view epoch, Session and fresh operation UUID. Its owner follows the command and
pending requests. Last-owner drop distinguishes known-unsent from possibly-written
Unknown. A pending or Unknown operation blocks other enables for that Client.
Old callbacks cannot spend a UUID again; a bounded 256-operation lifetime tombstone
set fails closed at capacity. This is not durable across Client/process restarts.

Before possible setter I/O, the worker marks the operation Sent and increments
its checked local status revision. The setter receipt increments it again. Both
invalidate sampled metadata, forecasts and unsent Step reviews; reads sampled
before the write or before its receipt cannot restore stale mode. UI mode is
restored only from a fresh owned snapshot, not optimistically from the receipt.
Positive receipts require exact Session, boolean true and response success.
Malformed positive receipts are Unknown. No mutation is automatically replayed,
including on reconnect. Observed selected Session deletion now retires metadata
and unsent mode work. These are connection-local observed-event guarantees,
not an atomic daemon revision or a fence against unseen external changes.

Client context invalidation also cancels a reserved native Step decision directly
through its ledger; cancellation cannot be lost to command queue saturation and
cannot demote an already-Sent receipt. A draft change still does not cancel mode.

## Validation and remaining gates

Ledger and worker tests cover strict receipt decoding, token bounds, owner drop,
queue saturation, stale contexts/revisions, duplicate callbacks, selected deletion,
synchronous retirement and metadata reads around setter I/O. Headless UI tests
cover draft/entity preservation, modal/composition, Profile replacement, keyboard
Space/Enter and minimum-window 200% reflow.

A private real daemon and fake provider exercise the complete UI/Client/daemon
path in canonical and future-attempt validation modes. Canonical enable starts
zero provider calls. During a held validation answer, enable preserves its live
prefix and owner, adds no provider request, and the canonical answer completes.
A separate controlled Unix peer loses the setter ACK: fresh mode is on after
physical reconnect, but the retained Unknown receipt prevents another native
enable and only one setter is observed. This peer is synthetic and Unix-only.

Exact source-bound commands, counts and hashes are retained in the task evidence
manifest. No native app, renderer, OS input/accessibility, Windows/Linux GUI,
current-head package or distribution acceptance is implied. Pending foreground
approval and the uncertain prior launch remain untouched. Full combined runtime
review, required parity, performance and licensing/distribution gates remain open;
this work is local and unpublished.
