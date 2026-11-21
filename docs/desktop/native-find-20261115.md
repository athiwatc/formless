# Isolated GPUI continuation: native Find shortcut

Continues `dfbab76828cf82b18aeff80bbd1c5f2e549b153a` in the separate
`formless-gpui-clean` checkout on `codex/gpui-takeover-20261115`.
The parent reports independent source reviews of `acabff81` and `dfbab768`
with no confirmed blocker. Those reviews inspected tests/logs without
rerunning them; this new increment is not covered by either review.

## Behavior

Cmd+F on macOS and Ctrl+F on other platforms now open the existing loaded
transcript Search through the typed command registry. The same key is
shown by the palette entry. No plain character is bound. If Search is
already open, Find focuses its query without clearing text or resetting
the chosen result. Escape retains the existing return destination and draft.

Find does nothing while a modal owns interaction or while the composer or
current browse query has marked input-method text. It neither commits nor
cancels preedit. Palette, whole-output, questions, forecast, Session details
and panel overlays remain protected by the shared modal check. Other
navigation methods retain their existing behavior.

The search remains literal and case-sensitive, over finished text already
loaded in this window, including Markdown source. Its existing query/result/
scan bounds and explicit fixture/Session scope are unchanged. It is not
Profile Search, full-history loading or search inside the current dialog.
The unchanged client load_page path emits no RPC for Destination::Search.
No new transport, backend, Provider, protocol or mutation path was added.

## Verification

Same optional manifest, isolated target, offline/locked dependencies and
bounded build environment as the preceding reports. All final checks exit 0:

| Check | Result / sibling evidence |
|---|---|
| `cargo test --features test-support,thai-breaks --test shell native_find_shortcut` | Two passed / `find-focused-second.log` |
| `cargo test --features test-support,thai-breaks --test shell` | 159 passed / `find-shell.log` |
| `cargo test --no-default-features --lib commands::tests` | Nine passed / `find-commands.log` |
| `cargo clippy --features test-support,thai-breaks --lib --test shell --test render -- -D warnings` | Passed / `find-clippy.log` |
| `cargo fmt --check`; `git diff --check` | Passed |

All Cargo test/clippy commands also use `--offline --locked` and
`--manifest-path crates/formless-desktop/Cargo.toml`. New shell cases use
the actual platform chord and cover ordinary typing, draft retention,
query focus, repeated Find without losing the selected result, Escape,
composer/Search/Sessions preedit retention, palette and whole-output modal
isolation, and no recorded composer intent. Existing minimum-size/200%
Search and modal tests remain in the full suite. Command tests cover key
uniqueness, modifier use and palette membership.

`find-focused.log` retains the first failed test compile: five trait-method
lookup errors caused by a missing EntityInputHandler trait import. The import
was added before the successful focused and full checks above. No failing
log was replaced or counted as passing.

These are headless keyboard/layout/clipboard and synthetic composition tests
on this Mac. They establish neither actual OS IME behavior nor Windows/Linux
keyboard behavior, native fonts/renderer, human scaling, clipboard or screen
reader acceptance. Renderer code was compiled by clippy, not run.

## Gates

No preserved conflicting checkout or writer was touched. No uncertain
foreground launch/control was repeated, approval bypassed, permission
changed, push, PR update, merge or new PR performed. Streaming remains
`deltas: false` and #2081 remains draft. Epic #1753 remains incomplete.
Ownership, foreground approval/prior launch outcome, real IME/accessibility/
scaling/input, Windows/Linux GUI execution, remaining parity, durable
preferences, accepted performance budgets and distribution gates remain open.
Review this exact local increment, resolve ownership and recheck live
main/#1776/#2081 before choosing any authorized integration target. A new
draft PR requires separate authorization.
