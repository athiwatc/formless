# Profile-wide persisted message search

Draft #2081 continues epic #1753 after independent source review of `0df24dd2`.
Final runtime/tests: `2ca88d50746f62ca32a018d8b6cf62e6b6e98790`.
Initial implementation: `919c93a702f3ce4292c2ad554653ddd41cab458b`; the final
follow-up guards repeated keyboard submission while a search is pending.
This is another parity increment, not epic completion. Streaming is disabled.

## Behavior and protocol

Go to Profile Search in the shared command palette opens a separate transient
page. Existing Search continues to find literal, case-sensitive text only in
the loaded transcript. Profile Search uses the unchanged `SearchSessions`
request to search persisted messages across the connected Profile, including
archived Sessions. It uses the daemon's query and ranking semantics rather
than pretending to be a literal local search. Source audit confirms the handler
is a storage read, with no provider request or mutation. FTS prefix terms and
the punctuation-only substring fallback remain daemon behavior.

Search requires an explicit Search click or Enter in the query. Editing does
not send requests. Repeated Enter with the same pending query is ignored; an
explicitly edited query can replace its old read even before input observers run. Queries are trimmed and limited to 256 characters; requests
ask for at most 50 matches. Two outstanding searches per connection bound the
worker's pending search count. This does not bound the daemon's underlying
scan cost or establish a search performance budget.

Replies retain operation UUID, connection and normalized query. A replacement
query retires previous rows before its reply, and old/foreign/duplicate replies
cannot install results. The decoder rejects oversized result lists before
deserializing rows, duplicate Session/message identities, missing identities,
malformed fields and unsupported field sizes. Disconnect retires pending and
displayed results. Reconnect never resubmits a search; retry is explicit.
Search reads cannot acknowledge or clear an uncertain mutation.

Results are labelled as snippets. Arrow movement or clicking chooses a preview;
it does not activate a Session. Enter from the results or Open Session uses the
row's captured reply revision, rechecks the live query and connection, and
requires exactly one unarchived Session in both current Session projections.
A missing/archived/ambiguous target opens nothing and explains the Sessions
refresh/restore path. There is no fallback to a neighbouring Session. Opening
uses existing Session activation/snapshot loading and draft retention. It
does not jump to the matching message, as the page explicitly discloses.
Results describe the last explicit search, not a continuously refreshed corpus.

Copy result copies the validated returned search fields as JSON, including
Session/message identity, role, sequence, date and snippet. It does not copy a
full message or pretend the snippet is complete. Copy and Open both recheck
captured revision and live marked text at dispatch. Neither operation submits
provider work. No view reads daemon storage/configuration directly.

The native page separates query Enter (Search) from result Enter (Open Session).
Composition Enter finishes preedit before another press can search/open; arrows
do not move results while marked text is active. Escape finishes composition
before navigating back. Both transient search scopes return to their underlying
destination without a Back loop. Tab retains normal control navigation, and
focused controls/rows are revealed within the scrolling body through resize
and 200% text scaling. No new global text-editing shortcut was added.

## Checks and evidence

All Cargo commands use the optional manifest, `--offline --locked`, two jobs,
incremental disabled, and dev/test debug info disabled. Evidence remains under
ignored `evidence/gpui-1753-sol/`.

| Check at runtime `2ca88d5` | Result / evidence |
|---|---|
| `cargo test --no-default-features --lib` | 175 passed; `profile-search-final-model.log` |
| GUI/Thai library + shell suites | 180 + 143 passed; final UI source in `profile-search-repeat-final-ui-tests.log` |
| Real private daemon/local fake provider suite | Five passed; `profile-search-final-daemon-second.log` |
| Strict clippy (`-- -D warnings`) | GUI-free lib/unit tests; GUI/Thai lib + shell/render; ax-dump/Thai binary; daemon fixture passed. `profile-search-final-clippy-model.log`, `profile-search-repeat-final-clippy.log`, `profile-search-repeat-final-bin-clippy.log`, `profile-search-final-daemon-clippy.log`. 26 existing core dependency warnings remain. |
| All-feature docs | Passed, zero doctests; `profile-search-final-doc.log` |
| Windows GNU GUI-free all-targets check | Passed; `profile-search-final-windows-model.log`; no GUI/link/execution claim |
| Native Mac fonts/Metal offscreen renderer | 49 states / 91 captures passed; `profile-search-repeat-native-final.log`, images in `profile-search-repeat-native-final/`, hashes in `profile-search-native-manifest.json` |
| Formatting/whitespace and unchanged dependency/protocol scope | Passed; no manifest, lockfile, protocol or relay change |

The runner's exact command arrays and exit codes are in
`profile-search-final-checks.json`; final UI checks follow the focus-ring padding
correction and repeated-Enter guard. Selected minimum-size query, result and error captures were visually
inspected, including the final corrected query focus ring. Existing native
Thai shaping diagnostics remain. This is not foreground OS input, native IME,
screen-reader speech or human usability acceptance.

New pure tests reject stale/foreign query replies, disconnected results,
ambiguous identities, oversized data and stale row versions. The actual
engine correlation test delivers two replies out of order, changes selected
Session, verifies the original queries/UUIDs, caps pending reads and checks
that only read-only SearchSessions requests were dispatched. New shell tests
cover explicit submission, no typing-triggered RPC, Copy fidelity, selection
before activation, composer preservation, delayed replies, archived targets,
reconnect without resubmission, preedit, 200% keyboard visibility, focused
control bounds and transient return navigation.

The actual daemon fixture searches a persisted assistant reply while its
Session is archived. A correlated result arrives over real local IPC with the
expected Session/snippet; the fake provider request count stays at one before
and after search. It then restores the fixture Session and completes the
existing transport/recovery cases. All five private fixtures/leases tear down.
This does not claim a paid provider or foreground native integration pass.

Failed attempts are retained: stateful-anchor API/import and tuple-index type
compile errors; the new reply missing from the fixture's exhaustive diagnostic
match; a nested-scroll anchor leaving the selected row offscreen; and native
200% query reveal leaving Search below the viewport. The correction uses the
body scroller's direct-child API, aligns query/results at its top and preserves
focus-ring padding. Earlier failing logs/captures are not final validation. A later repeated-Enter
regression reproduced a duplicate read despite the disabled Search button
(`profile-search-repeat-enter-reproduction.log`). The handler now checks the
live query and pending state before starting another read; the complete UI
suites, strict UI/bin lint and full native renderer passed after that fix.

## Ownership and remaining gates

Same isolated checkout and `work/sol-gpui-1753-20261115` branch. Main remained
`e9bf104fb9c1bf1bff4f56e05efa92b17a1bfc14`, already included. Original #1776
remained `f8c23e0d5726fdc7d50989109f198301feb45cc9`; its worktree was clean on
read-only inspection. No original writer/branch/worktree or paused session was
changed or controlled. Process inventory remains denied and terminal activity
unverified; no alternate inspection route was used. The separate relay fixture
fix belongs to #2151 and was not duplicated or promoted here.

The existing foreground Computer Use approval `proud-river-57508678` for
Formless Sol Validation remains unresolved; the earlier launch/control outcome
is uncertain. No launch/control was repeated, substituted or bypassed, and no
OS permissions changed. Foreground Mac keyboard/selection/real IME/VoiceOver,
human 200% and bundle launch remain open. Windows/Linux native GUI execution
is unavailable here; the portable check does not substitute for it.

Streaming remains `deltas:false`. This increment establishes no snapshot/event
cutoff or general delta replay safety; delayed snapshots, reconnect and overflow
ordering remain open. See the [ordering report](stream-ordering-20261114.md).
Native security approvals/Step, further Session/composer/context/resource
actions, broader general-purpose parity, durable preferences, measured budgets
and distribution gates remain. Recovery remains Session-scoped rather than
conditioned on a displayed Run ID. Keep #2081 draft; no merge or epic close.
