# Native persisted Session export

Draft #2081 continues epic #1753 after independent source review of `2dfcde43`.
Runtime and tests: `8d74bd2dd6234d9d262fe16a2fc684b4a5261544`.
The PR remains draft, streaming remains disabled, and the epic is incomplete.

## Behavior and actual contract

Session details now explicitly reads the existing `ExportSession` request as
JSON, Markdown or HTML. This is a read of persisted data; it does not export
unsaved composer/metadata edits, write a file, import anything or call a
Provider. Copy export sends the complete returned text to the system clipboard
only after an explicit action. The dialog discloses private conversation,
Tool data, plans and checkpoints, and that a result is a past read.

The audited existing JSON handler produces a `formless.session/1` bundle
inside a storage read transaction. It includes the handler's messages,
checkpoints, plan fields and selected metadata; it is not a complete Profile
backup or an assertion that every Session setting/resource is preserved.
Markdown and HTML use the existing transcript conversion and return
`{name,format,content}`. Their handler reads Session metadata then messages;
this increment does not invent a transaction/watermark for those reads.
HTML is presented as plain source text, never executed or opened in a browser.

Export payloads do not include a Session identity. Ownership is therefore
established by the actual correlated request envelope, captured Session ID,
operation UUID, connection and requested format. It is not inferred from the
active Session or a reply name. Format/schema mismatches fail closed. Future
JSON fields are preserved in the pretty-printed bundle; Markdown/HTML copy
exact returned Unicode, whitespace and line endings.

The native copy limit is 1 MiB of text, with a 16 KiB name limit. JSON writes
to a bounded serializer rather than allocating an unbounded pretty-printed
second copy. Oversized exports are rejected whole with a TUI handoff. The
preview is plain text, at most 4,000 characters, with an explicit truncation
notice; Copy includes the complete accepted text. The transport's existing
frame bound still applies before decoding, so this is a copy/preview budget,
not a new server pagination or backend cost guarantee.

At most two export reads may remain outstanding per connection. Export is
explicit, has no automatic retry/refresh/reconnect replay, and never activates
a Session. A pending read disables another request in that panel. New reads,
errors, disconnect and panel replacement retire earlier Copy callbacks;
unique view UUIDs prevent reopening the same Session from reusing an earlier
callback identity. Results for another Session/format/operation/connection
cannot install. Composer and metadata drafts remain intact. Existing unknown
Send/question and metadata-save safeguards remain independent of these reads.
Native Save file/import and full-Profile export remain open parity items.

## Validation

All Cargo checks use the optional manifest, offline locked dependencies,
two jobs, no incremental compilation, and dev/test debug info disabled.
Raw logs, captures and command arrays remain in ignored
`evidence/gpui-1753-sol/`.

| Check | Result / evidence |
|---|---|
| GUI-free library suite | 184 passed; `export-final-model.log` |
| GUI/Thai library + shell suites | 189 + 149 passed; `export-final-gui-tests.log` |
| Real private daemon/local fake Provider | Five passed; `export-verified-daemon.log` |
| Strict clippy (`-- -D warnings`) | GUI-free lib/tests, GUI/Thai lib + shell/render, ax-dump/Thai binary, and daemon fixture passed; `export-final-clippy-{model,gui,bin}.log`, `export-final-render-clippy.log`, `export-verified-clippy-daemon.log`. Core dependency retains 26 existing warnings. |
| All-feature docs | Passed, zero doctests; `export-verified-doc.log` |
| Windows GNU GUI-free all-targets check | Passed; `export-final-windows-model.log`; no GUI/link/native execution claim |
| Native Mac fonts/Metal offscreen rendering | 51 states / 99 captures passed; `export-native-confirmed.log`, PNGs in `export-native-confirmed/`, hashes/source in `export-native-manifest.json` |
| Formatting/whitespace/scope | Passed; protocol, root manifests, both lockfiles and relay unchanged |

Exact command arrays/exit codes are in `export-final-checks.json`,
`export-verified-native-checks.json` and `export-native-final-result.json`.
The first six-phase model/GUI run passed in full. After it, only daemon test
diagnostics/assertions and the render test's enabled-state assertion changed;
those targets and their strict clippy configurations were rerun successfully.

Pure tests cover exact Unicode/HTML-source fidelity, future JSON fields,
wrong format/schema, byte limits, whole rejection, delayed/replaced operations
and panel identity retirement. Engine tests deliver replies out of order
across selection changes, reject duplicates/foreign connections, and enforce
the outstanding-read cap. The actual reconnect transport regression now
processes an export reply alongside Stop, metadata and unrelated-question ACKs
and verifies that neither unknown Send nor unknown-question outcome is cleared.

Three new shell cases cover a different chosen/open Session, explicit reads,
no mutation requests, exact full Copy despite truncated preview, errors and
stale callbacks, retained composer/metadata drafts, live preedit guards,
disconnect/late replies/panel replacement without replay, modal Escape and
keyboard reachability at 480x360 / 200%. These use the serialized recording
boundary and do not count as daemon evidence.

The real daemon/fake-Provider case exports all three formats for A while B is
selected and A's first Provider reply is held. It checks the request-owned
export equals the daemon's persisted result, includes the saved user prompt,
excludes the held assistant output, keeps `running:true`, and leaves Provider
request count at one. All five cases and private fixture/Profile lease cleanup
pass. This is actual local IPC and fake-Provider execution, not paid-Provider
or foreground acceptance.

Native render captures add HTML-source/Copy and oversized-export states to
the two existing metadata layouts. Read/Copy controls and modal footer bounds
are asserted; selected final wide and minimum-size captures were visually
inspected. The minimum-size oversized capture shows keyboard-reachable format
controls, not a claim that all error prose is simultaneously visible. The
body remains scrollable. Thai shaping diagnostics remain at the pinned layer.
No human usability or screen-reader/OS IME claim follows from these captures.

Failure history is retained. `export-first-shell.log` has test API/lifetime
compile errors. `export-second-shell.log` exposed missing frame settling and
an uncleared reused Directory query in the new tests. Third/fourth logs retain
the query-entity borrow compile errors; the fifth focused run passes all three
cases. `export-final-daemon.log` retains a missing diagnostic match arm compile
failure, corrected before the successful actual fixture run. The first native
run in `export-verified-native.log` stopped because an enabled control reports
no disabled flag rather than `false`; the corrected renderer passed in full.
No failing evidence was overwritten or counted as successful validation.

## Fresh development artifact

Packaging ran with ResourceWarning treated as an error, `--profile dev
--offline`, from clean `99f3dcc21aaa413f0e8e9bbd6955df2eb333c4e3`. Only README
and this report differ from tested runtime `8d74bd2`. Output is
`evidence/gpui-1753-sol/package-99f3dcc`.

- Binary SHA256:
  `bb48e00363f4a843f1fd0a21e792434909d671af390e4bce71eeac0015d9c3ca`.
- ZIP SHA256:
  `69789f601d5bc3ee2a070bcb5e595f3b19ebdf2467e981c299df8dbc9ea84ad4`.
- Provenance/plist identity, safe archive paths/types, integrity, executable
  mode/byte identity, hashes and 21 system-library loads passed. Evidence:
  `export-final-package.log`, `export-final-package-verification.json`.
- 655 archive files, 415 packages / 649 notice files; 46 packages lack root
  notices. Manual licensing/assets/stdlib review remains.
- Linker ad-hoc signing, no Team ID, unbound plist, no sealed resources.
  No app was installed or launched; distribution signing/notarization and
  native installation/launch/update acceptance remain open.

## Ownership and open gates

Same isolated checkout and `work/sol-gpui-1753-20261115` branch. Main remains
`e9bf104fb9c1bf1bff4f56e05efa92b17a1bfc14`, already included. Original #1776
remains `f8c23e0d5726fdc7d50989109f198301feb45cc9`; its worktree was checked
read-only and clean. No original branch/worktree/writer or paused Cloud session
was changed or controlled. Process inventory remains denied, and old writer
terminal activity is still unverified; no alternative inventory route was used.
Separate CI fixture draft #2151 remains with its owner, untouched here.

Computer Use approval `proud-river-57508678` for Formless Sol Validation still
has no owner response in the supported card read. Its prior launch/control
outcome remains uncertain. No launch/control was repeated, substituted or
bypassed and no OS permissions changed. Foreground control tools remain
unavailable. Mac foreground keyboard/selection, actual OS IME/VoiceOver,
human 200% acceptance and bundle launch remain blocked. Windows/Linux native
GUI execution is unavailable here; hosted jobs must supply their own evidence.

Streaming remains `deltas:false`. This increment proves no snapshot/event
cutoff, delayed-snapshot consistency, reconnect/overflow or general delta
replay safety; the [ordering report](stream-ordering-20261114.md) still applies.
Native security approvals/Step, remaining Session/composer/context/resource
actions, broader general-purpose parity, durable preferences, performance
budgets, icons/licensing/distribution signing/notarization/install/update and
platform acceptance remain open. Recovery is still Session-scoped rather than
conditioned on displayed Run ID. Parent must review the exact new head and
recheck live main/#1776 ownership before any integration. No merge or epic
completion claim.
