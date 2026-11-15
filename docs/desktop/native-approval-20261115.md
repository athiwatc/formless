# Native Tool approval review

The Approvals palette command opens a Profile-wide modal for one supported Tool
approval, independently of the selected Session. The dialog shows the exact
validated JSON with Session, Tool/version, capabilities, existing authority,
call arguments and available context. It escapes directional controls and
provides keyboard scrolling and copy. Default focus closes the dialog; Enter
on opening does not grant authority. Drafts and active composition retain
ownership. Unmeasurable evidence disables both decision buttons.

Allow once and Deny issue the existing ApprovalDecision protocol with Once
scope, no path substitutions and remember_denial=false. Before writing, the
actor fetches PendingApprovals and compares the complete reviewed value.
Observed approval lifecycle and Session deletion events retire stale work.
Connection, Client, operation, epoch, revision and modal ownership guards
prevent stale callbacks and automatic replay. No backend or protocol changes
are included. There is no atomic guarantee against unseen external changes;
this relies on the daemon's existing one-shot approval identifiers.

The Client retains spent operation/approval identifiers (bounded to 256
operations) and possible-write receipts for its lifetime. An acknowledgement
loss remains Unknown after reconnect, even if a fresh inventory is empty;
there is no automatic retry. Receipts are not durable across process restart.
A resolved response with audit_recorded=false is accepted with an explicit
warning. Unsupported shapes, incomplete context, batches, multiple approvals,
promotions, persistent scopes and path widening require the TUI.

Validation uses reducer/actor tests, GPUI TestPlatform and private daemon /
local fake provider fixtures. A real private Tool writes only a fixture-owned
marker after Allow once; a second request requires new approval and Deny
prevents that write. A separate private transport drops the acknowledgement
and proves the Client retains Unknown without replay. These checks do not
validate native OS input, accessibility, clipboard, scaling or Windows/Linux
execution. Production streaming remains disabled. This increment stays local
for independent review and does not alter the published draft candidate.
