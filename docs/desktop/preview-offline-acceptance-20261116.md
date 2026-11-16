# Offline preview performance and source-header preservation

This continues independently reviewed schema-67 preview `10cd096` on a separate
branch. The reviewed preview, persistence prototype and previously built desktop
and matching Core artifacts remain preserved. No Core/protocol/schema, native
runtime, parser semantics or production feature policy changes are introduced.

## Measure the bounded row rather than an unbounded fixture

The existing `accessible_profile` still measures full approximately 32 KiB
function inputs. On this Mac its exploratory dev medians were about 1.5 ms for
words, 4.6 ms for Markdown and 27.2 ms for structured Markdown. Those inputs can
exceed the row's separate 400-line limit and are not frame measurements.

The new GUI-free `preview_profile` example measures the actual TimelineBlock
preview with five UTF-8 fixtures, including long single-line structured input
and expanding ordered-list labels. Inputs are about 128 KiB, but each displayed
window retains the existing byte/line caps. It separately records row setup,
cold accessibility conversion, batched cached calls and move-to-end plus fresh
conversion. Each case checks exact cache identity, one rebuild per window,
output bounds and stable content digests across rounds. Cache timing samples
are means of 1,000-call batches, not individual-hit tail latency.

A second case builds 100 and 1,000 rows, asks only five rows for accessible text
and checks that both retain the same accessible-byte total. Setup includes
source clones; the five-row conversion timing excludes full history setup.
Memory/CPU measurements from an explicitly timed headless example process are
recorded separately from Cargo build time. No renderer, OS accessibility service,
window, transport listener or production streaming path runs.

Exploratory bounded-row cold medians ranged from about 1.4 to 7.3 ms; the
structured 400-line window was about 1.7 ms. A 1,000-row history took about
13.6 ms to construct and about 2.5 ms for five cold visible conversions, retaining
20,495 accessible bytes like the 100-row case. These are local dev samples,
not accepted budgets, worst-case bounds, key-to-paint, native RSS or release
performance. No runtime optimization is justified or claimed from these samples.
The frozen exact-head evidence carries the final measurements and process limits.

## Preserve known copyright notices without inventing missing terms

The existing package omitted nine explicit copyright/dual-license headers from
`pathfinder_geometry 0.5.1`, because they appear only in Rust source files.
Collection now verifies the cached `.crate` SHA-256 against Cargo.lock, matches
all nine source files and the published VCS receipt with the extracted build
inputs, and preserves each exact leading comment block. The distinct 2019 and
2020 notices remain verbatim. No current upstream tree, inferred holder or
synthesized grant replaces the published bytes.

Each record is marked `published-source-header`, carries the full published
source hash and exact prefix length, and binds the original declared repository,
version, license, lockfile and VCS receipt. Offline packaging rechecks these
bindings, source hash and exact header prefix. Tests reject changed source,
changed snapshot even with its new hash, missing/traversal/symlink paths and
invalid or mismatched prefix lengths. Existing notice collection guards remain.

This adds nine snapshots to the 761 collected dependency notice files. Pathfinder
therefore no longer has an empty collection, but a new explicit limitation keeps
its complete chosen-license terms/distribution review open. The empty collections
for `mac 0.1.1` and `seahash 4.1.0` remain unresolved; the nineteen objc2/SDK scope
flags remain. Twenty packages now carry scope limitations. File-count improvement
is not legal clearance or a completed license bundle. No license choice is made,
external source fetched or agreement accepted here.

The earlier [licensing scope report](licensing-scope-20261115.md) remains the
historical investigation and owner-decision record. This increment closes only
preservation of known Pathfinder source notices. Attribution/terms decisions,
assets/stdlib/SDK scope, signing/notarization and distribution acceptance remain.

## Acceptance holds

All new checks are offline and use owned fixtures. The owner is resolving the
previous Computer Use approval; neither that request nor its uncertain launch
is repeated or bypassed. Hosted billing, native/platform/IME/accessibility,
clipboard/picker/scaling/lifecycle, full required parity/security and accepted
performance gates remain open. Queue, streaming and journaling stay disabled.
No personal Profile, schema migration, installation, publication or merge occurs.
The new notice/package/tooling increment requires independent review; earlier
source review does not imply release or whole-epic approval.
