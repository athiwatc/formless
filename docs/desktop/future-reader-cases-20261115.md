# Additional reader schedules

Execution update: after parent-coordinated cleanup restored roughly 64 GiB of
free space, staged commit `f0561c2` compiled and passed the focused schema/EOF,
Tool, Step and compaction schedules, strict daemon-target lint and the full
16-test daemon suite. Tool, Step and compaction each exercised both subscription
modes. The historical staging note below records the earlier disk stop; it no
longer describes the validation status of `f0561c2`. Exact logs and source hashes
are frozen in `reader-cases-validated-checks.json` in the task evidence directory.
Additional failure schedules are described in `future-reader-faults-20261115.md`.

## Historical staging note

These tests extend validated head `a5f2b96`. They have passed rustfmt parsing
and whitespace checks only. They have **not been type-checked, compiled or run**.
They are not acceptance evidence and must not enable production streaming.

The Mac had 719 MiB free on entry and 559 MiB at the last pre-commit check.
No Cargo build/test, package, dependency fetch or cache cleanup was started in
this increment. Existing task-owned artifacts and caches were preserved. The
backlog coordinator needs to resolve shared disk pressure before compilation;
this task has not controlled any other writer or removed unknown files.

Prepared schedules:

- The existing Tool turn, Step handoff/reconnect and context-compaction fixtures
  now each exercise both legacy subscriptions and the validation-only reader.
  Shared fixtures preserve their request-count and canonical-history assertions.
  The reader additionally observes a held Tool prefix, final canonical results,
  and the absence of the private compaction summary from delivered content.
- Schema repair starts a real pipeline Run with an invalid streamed candidate,
  followed by a non-streamed repair. Neither the invalid candidate nor repair
  input may escape into reader batches; exactly one validated assistant result
  must be persisted after exactly two fake provider requests.
- On Unix, an owned local IPC proxy closes the reader connection while a
  provider response is held. Automatic reconnect must advance connection/view
  ownership, load only stored history, omit the old incomplete suffix, and
  obtain the canonical final answer without replaying provider work.

Once at least 2 GiB free headroom is available, first compile the existing
desktop daemon target with `--no-default-features --features daemon-tests`.
Use the same task-owned target/profile/cache settings as the previous evidence;
do not build GUI/renderer/platform variants or a new package for these test-only
changes. Run focused `future_cases::`, Tool, Step and compaction filters, strict
daemon-target lint, then the full daemon suite if the focused checks pass.
The private tests need scoped loopback/IPC permission; they use temporary
Profiles and fake credentials, with no real Profile, paid API or foreground app.
Check headroom before every Cargo invocation and stop if it falls below 2 GiB.

Independent review and execution are pending. Server subscriber overflow,
concurrent canonical edits, delayed metadata/question replies and accepted
performance/native/platform/distribution budgets remain open. Step prompt
inventory/restoration is still a known parity limitation; exercising its safe
handoff does not implement native Step controls. The conflicting original tree
and unresolved foreground approval remain untouched. No publication is made.
