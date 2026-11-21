# Explicit full-history Session forks

Continues independently reviewed local
`16f6282310ab161b095c2dcdf45bc4ce4841db6c` in the isolated takeover checkout.
The parent reported no confirmed blocker in all nine prior local increments;
source/log hashes were inspected and tests were not rerun. This increment
needs exact-patch review. Epic #1753, native/product acceptance and integration
remain open.

## Scope and confirmation

Commands → Review a new Session fork targets the open Session in Chat or the
chosen directory entry. It first opens a review page with the authoritative
source ID, bounded name preview and copy scope. Review sends no mutation.
Create fork submits the existing `ForkSession` request once, with default name
and no prefix cutoff. The daemon copies all history persisted at execution,
including applicable plan/checkpoints, model, working directory and standing
instructions under its existing transaction semantics. Unsent drafts and
unpublished live output are excluded. Working-tree files are not duplicated.
No model call or Run starts; the open Session and draft are not switched to
the child. A verified receipt shows its new ID and actual copied-through
sequence. The user chooses the child explicitly from Sessions and can rename
it through existing Details. Bounded-prefix and custom-name review remain open.

The confirmation freezes its review lifetime, connection and source identity.
It rechecks actual connection readiness, one still-listed source and modal
ownership. Directory composition is first committed without also opening a
review; composer/palette preedit is not converted into submission. Cancel,
navigation, missing source, actual connection turnover and Profile attachment
replacement cannot reuse an old confirmation. Submission rotates/spends the
review token before enqueue, including when enqueue fails. A later known
receipt cannot make the old button reusable. A new explicit review after a
verified receipt displays its own unsent scope, not the previous child receipt.

## Durable operation receipt and reconnect

Fork is a non-idempotent database mutation. A shared mutex-protected ledger
belongs to one Client lifetime and permits one reserved/sent/unknown fork at
a time across that worker's connections. `Client::send` reserves synchronously
before enqueue. Queue failure rejects only that exact unsent reservation;
connection/identity preflight failure is also known unsent. Engine dispatch
requires the same reservation and suppresses a duplicate pending command.
A completed operation UUID cannot immediately be reused.

Before writing a batch containing the fork, the worker marks that reservation
sent. This is conservative: failure of an earlier batch write can leave the
fork unknown even if its bytes were never reached. A connection guard retires
reserved-unsent work as rejected and sent work as unknown on every exit,
including EOF, timeout, explicit reconnect, cancellation and update-delivery
failure. Queued old-connection forks are discarded rather than replayed.
The shared ledger remains outside a fresh Engine during reconnect.

Only a correlated receipt with a distinct bounded child ID, exact `forked_from`
source and explicit nonnegative copied-through sequence is accepted. Missing,
foreign or malformed receipts are unknown. Negative server responses are also
held conservatively as unknown after submission; the native UI does not infer
that no durable child exists. Unknown blocks every further native fork for
that Client lifetime, including other source Sessions. Unrelated action/read
ACKs and a later old receipt cannot clear it. No list/event coincidence is used
to infer authorship or resolve an unknown child.

The ledger stores a bounded latest receipt independently of the bounded update
queue. UI polling observes changes even if a notice cannot be delivered. The
status remains visible after leaving the review page. It records identifiers,
connection/operation/sequence numbers and at most a 500-character reason,
not conversation data. The review keeps a 128-character name preview, not a
second Session/transcript store. Errors are bounded before formatting.

This is explicitly **not durable server deduplication**: replacing a Client
or restarting the application ends its ledger lifetime. Those actions do not
establish that an earlier fork failed. The UI/README instruct the user to
inspect Sessions/TUI before attempting a fork elsewhere or creating a new
lifetime after an uncertain result. The native UI offers no automatic reset,
retry, reconciliation or TUI mutation on the user's behalf. Existing Send/
question uncertain-action identities, paid-work consent and streaming policy
remain unchanged. No daemon/protocol changes were made.

## Validation and failures preserved

Three ledger/decoder model tests cover source/child lineage, malformed and
negative receipts, pending/exact operations, known-unsent versus unknown
writes, unrelated retirement, blocked retries and verified receipts. Three
Client/Engine tests cover the exact all-history RPC, required reservation,
duplicate pending commands and reused completed operations; full-queue failure;
and an actual in-memory transport disconnect/reconnect. That transport counts
exactly one fork frame across connections, rejects further reservations and
processes unrelated Stop/Run-read acknowledgements without clearing unknown.

Six headless shell tests use real kit key/focus/hit dispatch and the recording
boundary: Commands review with no mutation, explicit one-time confirmation,
verified receipt and later independent review; another Session's target with
unchanged active Session/composer/draft; cancel/navigation/modal/preedit and
missing-source guards; connection and attachment retirement with repeated IDs;
a sent operation losing its receipt and a late ACK; queue failure/new review;
and keyboard-readable review/receipt/status at 480x360 and 200%. Scrolling the
review uses scoped keys and direct child reveal through its tracked body.
These are simulated-platform checks, not native keyboard/IME/VoiceOver evidence.

A first toolbar placement broke the existing complete-row 200% assertion
(`session-fork-gui-final.log`). The Fork entry is now in Commands and the
unchanged directory row/keyboard-visibility test passes. That requirement was
not weakened or replaced by a looser size check. A missing test Client field
and worker argument-count lint failure are also preserved. Worker-owned shared
state is grouped into one argument instead of suppressing the lint.

The private-profile real-daemon/local-fake-provider scenario forks one active
source while another Session is selected and the Provider response is held.
It verifies exact persisted message content/role/meta/sequence in the child,
source lineage/receipt sequence, no queued input, no child Run and no mutation
of the source/Run. The Provider count remains one. Later lifecycle/reconnect
checks still run. The first run reached those checks but timed out at the old
three-Session fixture assertion: the verified fork made the expected count
four. The corrected assertion requires exactly four and explicitly checks
both original IDs and the verified child. Its failed log and private fixture
remain preserved; the successful run cleans only its own temporary Profile.
No real user Profile or external Provider credentials were used.

Exact final commands, exits, suite counts, source/log/patch hashes and package
provenance are recorded in the sibling evidence manifest. Broad suites were
followed by focused fork, lint, Windows and real-daemon checks after the
lint-only worker-state argument grouping. Renderer compilation is not renderer
execution; no desktop app, native picker or real OS clipboard was launched.

Validation passed 222 GUI-free model tests, 227 GUI/Thai model tests,
195 headless shell tests and two CLI tests. After the worker-state argument
grouping, both six-case fork model/transport runs and all six fork shell cases
passed again, as did strict desktop/daemon-test clippy, Windows GUI-free
all-targets checking, fmt/diff checks and all five private-daemon tests. The
unmodified optional core dependency still emitted its 26 existing warnings.
No source behavior changed after those final checks; only evidence prose and
memory-policy documentation were completed.

## Integration holds

The original conflicting partial checkout remains untouched, ownership still
unresolved. Existing Formless Sol Validation approval and the prior uncertain
launch/control outcome remain unresolved and were not repeated or bypassed.
Streaming remains disabled. Native IME/accessibility/input/scaling, Windows/
Linux GUI, picker/filesystem acceptance, broader parity, performance budgets,
licence audit and signing/notarization/install/update gates remain open.

No push, PR update or merge occurred. Review this exact local increment,
resolve ownership and native gates, recheck live main/#1776/#2081, then validate
the agreed combined revision before integration. Finished GPUI and clean main
are not claimed.
