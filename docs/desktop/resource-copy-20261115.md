# Isolated GPUI continuation: resource copy ownership

Continues local commit `acabff8159a75c116036b879329d71c70fc7ae36` on
`codex/gpui-takeover-20261115` in the new `formless-gpui-clean` checkout.
The parent reported an independent source review of that source-copy commit
with no confirmed blocker. It inspected logs but did not rerun tests. That
review does not cover this new increment. Native clipboard/accessibility
acceptance remains open.

## Defect and change

Previously, the read-only Tools/Metrics/Proposals/Settings Copy details
callback looked up the current destination and current selection when it
ran. A queued action from a preceding frame could therefore copy a different
entry after selection, page refresh or navigation. It also did not check
whether a newer dialog owned interaction.

The rendered callback now captures only the destination, monotonic page
revision and selected index. Before touching the clipboard, it checks that
all three still describe the current entry and that no modal is open.
Missing/error pages and Profile replacement invalidate the target. The
complete returned JSON still copies exactly, including Unicode and escaped
whitespace; callbacks retain no JSON clone. A tooltip explicitly names the
system clipboard and possible private data.

This is a local read-only action. No request, daemon mutation, provider call,
transport/streaming change, new copy-size policy or changed backend meaning
was introduced. Disconnected cached data remains a past read; this change
does not make it current or transactional. Returning to the same unchanged
page/selection can still copy that same data.

## Validation at final source

Same isolated target, offline/locked dependencies and two-job, no-incremental,
no-debug-info environment as the source-copy report.
Every command below exited 0:

| Command (optional manifest) | Result / sibling evidence file |
|---|---|
| `cargo test --features test-support,thai-breaks --test shell resource` | Three passed / `resource-focused.log` |
| `cargo test --features test-support,thai-breaks --test shell` | 157 passed / `resource-shell.log` |
| `cargo clippy --features test-support,thai-breaks --lib --test shell --test render -- -D warnings` | Passed / `resource-clippy.log` |
| `cargo fmt --check`; `git diff --check` | Passed |

All Cargo test/clippy commands include `--offline --locked` and
`--manifest-path crates/formless-desktop/Cargo.toml`. The new regression
captures the same target used by the actual callback and delivers it after
selection change, identical-content refresh, navigation, a palette opening,
error replacement and Profile replacement. Rejected actions preserve a
sentinel clipboard; current actions and the real button retain exact JSON.
The existing resource keyboard/JSON and minimum-size/200% tests also pass.

The previously retained 188 GUI-free / 193 GUI/Thai model, eight checker
and four source-copy test results remain associated with `acabff81`; those
model/checker suites were not redundantly rerun for this shell-only follow-up.
The final 157-test shell suite includes all four source-copy regressions.
Renderer code compiled under clippy; no renderer, real application, OS
clipboard, foreground input or native accessibility execution occurred.

## State and integration

The original conflicting checkout remains untouched and its ownership
unverified. Foreground Formless Sol Validation approval and the prior
launch/control outcome remain unresolved; neither was repeated or bypassed.
No denied path/process inspection, writer interruption, Claude restart,
permission change, push, PR creation/update or merge occurred.

Streaming remains `deltas: false`. Epic #1753 remains incomplete, with real
IME/VoiceOver/NVDA, human scaling/input/selection, Windows/Linux GUI,
remaining parity, durable preferences, performance-budget and distribution
gates open. This increment needs independent exact-patch review. Keep the
new branch separate; resolve ownership and recheck live main/#1776/#2081
before choosing an authorized integration target. Review/cherry-pick the
local commits only after that resolution, then rerun the focused checks on
the integrated revision. A new draft PR requires separate authorization.
