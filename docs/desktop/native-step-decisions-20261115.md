# Native decisions for complete Step prompts

This continues clean local integration `144ab335`, whose read-only inventory and
merge received parent-reported independent review without a test rerun. That
review does not cover this new controller. The prior integration branch is
preserved; this work uses a new local branch and leaves the conflicting original
checkout untouched.

## Supported interaction

Review Step appears for one supported pending prompt in the selected owned
Session when the entire original argument string fits the 4,096-byte snapshot
preview. Missing, malformed, multiple, truncated and oversized inventories stay
on the TUI path. A preview is never sufficient authority for a native decision.
The modal shows Session/Run/Step identity, Tool and call position, the complete
original text, exact Copy original and Copy edited controls, and a bounded JSON
editor. Its explicit actions are Continue original, Skip this call, Edit and
continue, and Continue; Step off. Enter initially activates Close, not a decision.
Enter in the editor inserts a line. Editor submission requires a JSON object;
original arguments are not silently reformatted.

The existing daemon's `all` action disables Step mode for the **Session**, not
just the displayed batch of calls. The modal explains that subsequent calls
will not pause until Step mode is enabled again. Normal security approvals are
unchanged. An accepted decision receipt does not establish Tool execution or
success. Enabling Step mode, larger/multiple prompt review and native security
approvals remain TUI workflows; full native parity is not claimed.

The form traps focus and uses scroll anchors to reveal controls after keyboard
navigation, resizing and text scaling. Escape ends active composition before a
second Escape closes the form. Composition blocks submission. Oversized paste
is refused; oversized non-composing edits restore the last accepted text with
an explicit undo-reset notice. One editor draft, bounded to 4 KiB and bound to
the exact Client instance and prompt definition, is kept for the window when
closing. A reconnect can restore it only for that same definition. Profile
replacement clears the private draft and retires the old review; another
Profile's matching numeric counters cannot inherit it. Copy edited remains
available for an earlier review within the same Client instance.

## Fresh validation and uncertainty

The Client synchronously reserves one decision before command enqueue. The
serialized worker first reads GetSession and requires the same sole supported
prompt: exact Session, Run, Step, Tool, argument bytes, text and call position.
It also checks Client instance, physical connection, selection generation and
view/action epoch. A changed context, cancelled read, missing field or changed
prompt rejects the action without sending StepDecision. The worker checks queued
reviews again before writing. Closing cancels a still-reserved decision through
shared state, even when the command queue is full. A possibly-written decision
cannot be recalled by closing the modal.

A dispatch owner travels with the queued command and pending request. Its last
drop records known-unsent rejection while still reserved, or unknown outcome
once the decision write may have begun. This covers queue failure, disconnect,
request timeout, worker exit and reconnect-drain paths without replaying a
mutation. The receipt ledger contains plain action data and no dispatch-owner
reference, so ownership does not form an Arc cycle. An old cancelled owner cannot
retire a newer reservation.

A positive receipt must have `ok: true` and the exact Step ID and action. A known
negative daemon response is rejected; malformed positive responses are unknown.
The unchanged Step handler validates negative cases before removing its sender.
Unknown outcomes block all native Step decisions for this Client. Canonical
absence of a prompt does not clear that uncertainty. The ledger is shared with
the UI, so a failed UI delivery cannot erase a verified receipt; repeated
callbacks also preserve it. This is Client-lifetime state, not durable receipt
history across process restarts. Daemon Step identities remain one-shot and
fresh explicit validation is required for each action.

## Evidence and remaining gates

Model and worker checks cover all four exact wire choices, fresh-read ownership,
changed/truncated prompts, context retirement before writing, cancelled read
slots, queue saturation, foreign Client ownership, accepted-receipt retention
through context/UI-queue failure, and distinct unsent/unknown drops. Headless
GPUI checks cover keyboard entry/default close, exact clipboard text, all four
buttons, repeated callbacks, invalid JSON, composition/Enter/Escape, edit bounds,
stale views, Profile replacement, saved edits, unknown-write blocking and 200%
minimum-window focus/scroll behavior.

The real private-daemon/fake-provider Step schedule exercises all four choices
in both canonical and validation-reader modes after reconnect. Each fixture
uses a nonexistent Tool, so no real Tool can execute; two fake provider requests
produce the canonical final answer. A separate controlled Unix IPC peer records
a decision request and intentionally loses its receipt. The Client must reconnect
physically, retain unknown status, refuse an explicit native retry, and show only
one observed decision write. That peer test proves transport behavior, not that
a real Tool ran. Exact commands, counts, source hashes, development artifact
receipts and initial corrected compile/lint failures are retained in task evidence.

Production streaming stays disabled. There is no desktop/renderer launch,
installation, actual clipboard or native IME/accessibility validation here.
Windows no-GUI compilation does not prove Windows/Linux GUI execution. Native
acceptance, complete required parity, performance budgets, three missing notice
bundles/attribution, nineteen objc2/SDK scope flags and distribution acceptance
remain open. The unresolved foreground approval `proud-river-57508678` and prior
uncertain launch/control are neither repeated nor bypassed. No writer or Claude
session is controlled, and no remote branch/PR/main is modified. The new native
controller and complete combined runtime/protocol range need independent review
before any publication/integration.
