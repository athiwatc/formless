# Self-optimisation is gated solely by measured Tool Success Rate and sandboxed replay

The optimiser may act only on measured outcomes: Tool Success Rate below 90% over at least 20 Invocations triggers a background Candidate, and that Candidate is promoted only if it beats the current version by at least 10 points on sandboxed replay (all recorded failures plus an equal sample of successes, with zero regressions). Replay runs in a discarded copy-on-write overlay with network denied, `process` denied, and no real secrets, so tests can never touch real files or services; Candidates may change code, description, and schemas but never widen Capabilities without human approval. We rejected silent auto-rewriting (unverifiable mutations compound) and approve-every-change (an approval bottleneck that defeats a background optimiser); versioned, evidence-gated promotion with full rollback is the middle path. After three consecutive failed Candidates the Tool is flagged `needs attention` and optimisation stops.

**Status:** accepted

**Consequences:** Invocation recording (redacted, retention-capped) is a load-bearing subsystem, not logging; without trustworthy recordings there is no replay and no optimisation.
