# Native forecast confirmation continuation

Draft PR #2081 continues epic #1753. This increment implements native forecast
consent; it does not complete the epic. The earlier transport/recovery and
packaging reports remain historical evidence. This report supersedes their
statement that forecast confirmation requires a TUI handoff.

## Exact source and ownership

- Implementation: `336ad81bf5758ded62cd593e2d4e6a6aa2dd9bc8`.
- Implementation source for the full desktop checks:
  `b3a9a60eaa87a37d8fa59d522944ae1335a58c96`.
- That reconciled main `b7b9d3c0d31b593489b9f749da82df654b81f17a`.
- A final remote check found one more main advance, reconciled at
  `d5c052e54bc4fb7aa9c2266f64bdd093ad9a1c65`: main
  `f6149fce0719a5a1dca890735abaf4feb4266531`. Its changes are confined to Skill
  source routing, its tests and SPEC. The desktop source/tests, manifests,
  lockfile and protocol have no diff against the fully checked source above.
  The real daemon fixture and strict daemon-fixture clippy were rerun and
  passed after this merge; logs are `f6149fce-main-daemon.log` (1 passed,
  3.82 seconds) and `f6149fce-main-clippy-daemon.log`.
- Same isolated clone: `formless-gpui-1753-sol-20261115`; branch:
  `work/sol-gpui-1753-20261115`.
- Original #1776 remained at
  `f8c23e0d5726fdc7d50989109f198301feb45cc9` in both the published PR and local
  original ref. Its worktree was clean on the read-only check. Neither the
  original branch/worktree nor any original writer was changed or controlled.

No dependency, lockfile or protocol change was needed. The desktop remains an
optional workspace. A fresh normal-edge dependency check found no embedded
core/runtime/TUI: 52 package identities without GUI and 397 with GUI. The latter
is the all-platform Cargo tree, not the Mac build-package count.

## Behavior and protocol basis

Send, Steer, Queue and Resume first use `ForecastRun { for_send: true }` with
the immutable intended content (or `resume: true`). A required quote opens a
modal with Cancel focused, bounded estimates/warnings and copyable decoded
quote JSON. Missing prices remain unavailable; small monetary values do not
round down to zero. Arrow/Page/Home/End keys scroll the body. The buttons stay
inside the minimum 480×360 window at 200%, including while confirmation is
pending. The waiting message is a polite live region in the GPUI tree; actual
screen-reader speech has not been validated.

The worker retains the exact intended action, request identity, view selection
generation, raw draft basis and a context epoch. A quote never starts work.
For required consent, the explicit button sends `ConfirmForecast` on the same
connection; its acknowledgement also never starts work. The view must recheck
the exact live draft, marked-text state, Session, generation, connectivity and
epoch before committing the prepared action once. No-consent quotes use that
same view commit boundary. Invalid or rejected quotes leave the draft intact.

Editing, cancellation, navigation and reconnect retire preparations. Context
invalidation uses shared atomic state, so a full bounded command queue cannot
lose a cancellation. Retired queued quote/consent replies cannot cancel a newer
dialog or commit an old action. This cancellation does not retire an actual
mutation already dispatched: the existing request-specific unknown-outcome
guard remains intact. Review and prepared commits are also blocked when a prior
mutation outcome for that Session is unknown. No mutation retry was added.

The actual existing daemon contract was inspected in `daemon.rs`:
`ForecastRun` creates connection-owned, expiring consent records;
`ConfirmForecast` recomputes the fingerprint; admission consumes a matching
consent once and rechecks the current inputs. The desktop does not estimate,
grant policy consent automatically, or claim a new Run-ID condition. Resume
and Abandon still target a Session. The recovery explanation now explicitly
discloses that the daemon cannot condition these requests on the displayed
Run ID. Backend state can change between a displayed snapshot and admission.

The protocol still lacks snapshot watermarks and delta offsets. Streaming
display remains disabled, and the earlier real stream characterization is
not a complete replay contract. Pending decisions remain a profile-wide
notification with a TUI handoff; no native decision form is claimed.

## Fresh checks

Host: arm64 macOS 27.2 (26B123), Rust 1.100.0. All Cargo checks use the optional
manifest with `--offline --locked`, two jobs, incremental off and dev/test
debug info off. Logs below use prefix `evidence/gpui-1753-sol/b3a9a60e-` and
were collected against the clean reconciled implementation commit above.

| Check | Result / log suffix |
|---|---|
| GUI-free library | 157 passed; `model.log` |
| Model with test-support/Thai | 162 passed; `gui-tests-corrected.log` |
| Headless shell | 126 passed; same log |
| Doc tests | Passed (0 examples); `doc-tests.log` |
| Formatting | Passed; `fmt.log` |
| Strict clippy | Model, GUI, test-support, Thai and daemon fixture passed; `clippy-*.log` (26 existing core dependency warnings) |
| Windows GNU GUI-free all-targets cross-check | Passed; `windows-cross.log`; no linking or execution |
| Real daemon/local fake HTTP provider | 1 passed; `daemon.log` |
| Real-font Metal renderer | 37 states / 59 captures; `native.log`, captures in `forecast-native/` |
| Normal dependency edges | Asserted isolation; `graphs.json` |

The new model tests cover malformed/foreign quotes, consent denial, duplicate
commits, retired selection/connection contexts, unknown mutations and full
command queues. Shell tests cover default Cancel, F6 containment, Escape/focus
restoration, copied JSON, duplicate/unrelated replies, draft changes before a
deferred observer, and marked text with unchanged raw text at both commit
boundaries. Direct input-handler delivery is not OS IME acceptance.

The real private fixture uses the actual daemon and IPC with a local fake HTTP
provider and fake credentials. Forecasting starts no provider request. A quote
retired by navigation cannot send. A fresh required quote, explicit consent
ACK and commit start exactly one provider request. Total provider requests are
five, including the existing send/recovery cases; reconnect starts none. The
fixture completes in about four seconds, checks that both daemon instances
release their private Profile leases, and removes only its private tree.

The final wide forecast, minimum 200% waiting and recovery captures were
visually inspected. These are serialized view fixtures; no foreground app or
connected GUI input was exercised. Failed attempts were retained under
`forecast-*`: a focus-trap identifier mismatch in the test query, an incorrect
relative capture path, the actual waiting-state overflow, fixture-helper
placement rejected by clippy, and an incompatible combined Cargo doc/target
invocation. Corrected final results above passed independently. An initial
dependency assertion also matched the checkout's `gpui` path; the final check
compares parsed package names. Existing ICU Thai segmentation warnings remain.

## Clean development bundle

The existing packager ran with ResourceWarning treated as an error and reused
the dev cache. It did not install or launch anything. Artifact directory:
`evidence/gpui-1753-sol/package-b3a9a60e/`.

- Source: `b3a9a60eaa87a37d8fa59d522944ae1335a58c96`; `dirty: false`.
- Profile/target: dev / `aarch64-apple-darwin`.
- Binary SHA256: `6b2156897e4f4b66a334f24bf78a8ed538ce5677f7b6d154c8d6ae9ee95205a7`.
- ZIP SHA256: `a2ad9cfa1189a45844ac73abbad73ff9137989b55d94f50b38bf25bff19dd8f0`.
- `plutil -lint`, ZIP integrity, safe archive paths, executable permission and
  byte identity against the built binary passed.
- Identity remains `xyz.formless.desktop.development`; no accepted bundle
  launch, signing/notarization or installer behavior is established.
- Dependency notice collection still needs manual asset/stdlib/distribution
  review; 46 packages have metadata without root notice files.

## Remaining gates and exact blockers

The supported read of approval `proud-river-57508678` still showed no owner
affirmative reaction or reply to “Allow Computer Use to use Formless Sol
Validation?”. Its earlier launch/control outcome remains uncertain. It was
not repeated or bypassed, and no different app identity was launched as a
substitute. Resolving that existing approval/action is required before the
foreground Mac keyboard, selection, real IME, VoiceOver, human 200% and bundle
launch checks can proceed. Native OS security permissions were not changed.

A read-only `ps` inventory was denied by the sandbox (`operation not permitted`)
and was not retried through another route. Terminal/writer activity therefore
remains unverified; branch ownership/head checks do not prove a writer stopped.
An initially sandboxed Git fetch failed DNS resolution; the normal network
approval path then allowed the read-only fetch in the isolated clone. There
is no remaining fetch/publication denial from that result.

Full streaming reconciliation, native approval/question forms, Step controls,
Session metadata actions, attachments/model/Role selection, artifact/diff and
broader screen/action parity, preference persistence and measured performance
remain open. Windows/Linux native execution and packaging acceptance remain
unverified; the arranged hosted jobs have not supplied passing execution
evidence. Distribution licensing, icons, signing, installers and updates remain
release gates. Keep #2081 draft, review the exact new head, and do not merge or
close #1753 on this evidence.
