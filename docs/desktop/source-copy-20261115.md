# Isolated GPUI continuation: exact block source copy

## Source and scope

Started at published head `cc1d0c9805230d7807a4a871396f91c0ebeec91a`,
verified against open draft PR #2081 through GitHub on 2026-11-15.
New standalone checkout: `formless-gpui-clean`, local branch
`codex/gpui-takeover-20261115`. No inherited working-tree edits were used.
The initial local clone lacked objects; a fresh, depth-one direct fetch of
that exact commit succeeded. The original and prior partial GPUI checkouts
were not inspected, modified, discarded or used as implementation sources.
No writer was interrupted and Claude was not restarted.

No tracked AGENTS.md, .agents/skills or .codex/skills exists at this head;
applicable ancestor AGENTS.md paths were checked and none existed. Local
Codex memories were consulted for historical validation limits, then checked
against this checkout's source and continuation reports.

## Design assessment

The optional, detached desktop workspace preserves existing headless clients.
The published code already has a real daemon client, canonical transcripts,
Session metadata/instructions/archive/export, explicit recorded usage,
Profile Search, forecast consent and native user-question surfaces. It is
substantially beyond the original fixture-only Phase 0 description; the
README's current continuation reports are the relevant evidence.

Transport ordering remains an implementation dependency: Subscribe still
uses `deltas: false`. Events invalidate canonical snapshots; no offset or
snapshot watermark proves safe general delta replay. This increment does
not change that contract. Native security approvals and Step recovery also
remain incomplete. It would be unsafe to describe these as complete solely
because the existing shell and model suites pass.

One independently implementable parity gap was copying a finished short
block's exact source. Previously, the full-output Copy all control was
reachable from truncated blocks or Search, while ordinary short rows had
only rendered text selection. This increment adds direct Copy source to
every visible row without changing protocol, transport or artifact handling.

## Changes

- A focusable, labelled Copy source button copies the entire stored block,
  preserving Markdown, Unicode, CRLF, tabs and trailing spaces, including
  content outside the bounded preview. No rendered text conversion occurs.
- Direct copy accepts at most 1 MiB and rejects larger text whole. Larger
  blocks retain the existing View whole / Copy all path. No new process or
  OS clipboard memory budget is asserted.
- Rendered callbacks capture the existing finished-corpus revision. At
  dispatch, the action checks revision, Chat destination, modal isolation,
  reload-needed state, block identity, live-tail status and byte limit before
  writing the clipboard. No full-source clone is captured by row callbacks.
- A polite status notice confirms copying, without moving focus, changing
  drafts, emitting a daemon command or opening a viewer. Corpus changes
  clear the old notice. Clipboard data may contain private conversation or
  Tool output; the control's tooltip and README disclose that destination.
- Four shell regressions cover exact source outside the preview, keyboard
  operation/focus, replaced sources, non-Chat destinations, modal isolation,
  reload gaps, live tails, exact-limit acceptance, whole oversize rejection,
  and keyboard reachability at 480x360 / 200%.

This is complete-block copying, not cross-block selection or a dedicated
code-fence copy action. Real system clipboard behavior and visual/native
accessibility acceptance remain unverified.

## Validation

Commands run from the new repository root with this environment:

```sh
CARGO_TARGET_DIR=/Users/athiwat/Documents/Codex/2026-11-15/task-4/gpui-target
CARGO_BUILD_JOBS=2
CARGO_INCREMENTAL=0
CARGO_PROFILE_DEV_DEBUG=0
CARGO_PROFILE_TEST_DEBUG=0
```

All Cargo commands use `--manifest-path crates/formless-desktop/Cargo.toml`,
`--offline --locked` (except fmt, which takes only the manifest).

| Command | Result |
|---|---|
| `cargo test --no-default-features --lib` | 188 passed |
| `cargo test --features test-support,thai-breaks --lib --test shell` | 193 library + 156 shell passed |
| `python3 crates/formless-desktop/scripts/test_check_platform_a11y.py` | Eight passed, stub subprocesses only; no native application |
| `cargo fmt --check` and `git diff --check` | Passed |

Strict clippy passed with `--features test-support,thai-breaks --lib
--test shell --test render -- -D warnings`. The final focused rerun,
`cargo test --features test-support,thai-breaks --test shell block_copy_source`,
passed all four new cases (152 filtered out). All listed commands exited 0.
Raw evidence is in the sibling task directory `../evidence/`; the new target
directory is separate from every prior checkout's build artifacts.
No daemon integration suite was rerun: this slice has no daemon/transport
change. The shell tests use the kit's headless clipboard and uniform-width
text system, not a real OS clipboard or fonts. No native renderer or app
launch/control was attempted. Renderer code is compiled by strict clippy;
that does not establish native pixels or acceptance.

## Remaining blockers and safe integration

The prior partial tree's ownership remains unverified; its contents were
preserved and were not adopted. Foreground Computer Use approval for Formless
Sol Validation and a prior launch/control outcome remain unresolved. Neither
was repeated, substituted or bypassed. No denied path or process inventory
was inspected and no OS permission was changed.

Actual OS IME, VoiceOver/NVDA, human 200% scaling/selection acceptance,
Windows/Linux GUI execution, broader Session/composer/context/resource
parity, durable preferences, accepted performance budgets and distribution
licensing/icons/signing/notarization/install/update gates remain open.
This change closes none of those gates and does not complete epic #1753.

Keep this local branch isolated. Review its frozen diff against the exact
published base, resolve ownership of the preserved partial tree, and recheck
live main, #1776 and #2081 before selecting an integration target. Only after
those steps and authorization should the new commit be cherry-picked onto
an agreed branch and its focused checks rerun there. No push, PR creation,
PR update or merge was performed; a separate draft PR requires separate
authorization. New code still needs independent exact-patch review.
