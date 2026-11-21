# Reader recovery and paced transport schedules

This increment follows reader `39c1e11` and the independently identified
unchanged-baseline recovery defect. Fix `3f6572f` resets reducer metadata on
every accepted canonical baseline, while preserving unchanged rendered rows.
The failing regression on `26a7d9d` emitted an unnecessary Refresh; the fixed
test accepts a new prefix after retirement, including a hidden persisted
message that changes the sequence floor without changing visible rows.
The full 208-test headless shell suite and strict desktop lint pass at the fix.

The additional private-daemon schedules use the actual validation Client,
temporary Profiles, local IPC and an HTTP fake provider with fake credentials:

- A held candidate is cancelled by an explicit RetryProviderRequest. The reader
  retires it and accepts a fresh baseline with only the stored user message.
  A queued user message then turns over into a separate Run. Exactly three
  provider requests produce two canonical assistant messages; the cancelled
  candidate never becomes persisted history. No connection is replaced.
- A paced provider emits 5,000 indexed UTF-8 deltas, with a 5 ms minimum timer
  between emissions. A consumer drains the bounded UI update channel on a
  2 ms minimum timer, verifies every exact indexed delta, and concurrently
  receives status metadata. Final canonical content must equal the complete
  concatenation. An additional legacy Observer is deliberately not drained
  during delivery, introducing a slow subscriber alongside the measured reader.

The paced test logs `reader_sustained_metrics` with elapsed time, payload size,
observed frame rate, emit-to-drain latency percentiles, status batch count and
maximum updates per drain. The start timestamp is taken inside the fake HTTP
body producer before yielding the SSE chunk. The endpoint is receipt by the
Client consumer; it excludes GPUI layout, paint, screen presentation and input.
Minimum Tokio timers are workload parameters, not promised achieved rates.
The result is a bounded debug/test transport sample under local load, not a
production throughput SLA or a statistically controlled benchmark. CPU, memory,
long-session rendering, large-history and UI stall budgets remain unmeasured.
Existing forced undrained-UI saturation still verifies fail-closed reconnect
and canonical recovery rather than claiming lossless live delivery at all rates.

Immutable logs, exact commit/patch identities, metrics, upstream integration and
artifact hashes are recorded in the sibling task evidence manifest. The earlier
1,000-frame exploratory sample is retained separately. Main advances are merged
locally only after freezing the source increment, with shared core/protocol and
desktop identity across the merge checked explicitly.

Production streaming stays disabled. Independent review of the reader, recovery
fix and combined integration remains required. Actual new-mode Tool/Step/schema/
compaction, arbitrary EOF, server subscriber overflow, concurrent edit and
delayed metadata/question schedules still need broader coverage. Native IME,
accessibility, clipboard, scaling, picker/filesystem, Windows/Linux GUI,
performance budgets and distribution gates remain open. The unresolved
Formless Sol Validation approval and uncertain foreground action are not retried;
the original conflicting checkout is preserved. Nothing is pushed or published.
