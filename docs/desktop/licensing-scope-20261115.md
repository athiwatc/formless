# Licensing scope and remaining release blockers

This follows local head `bd26cf04`, which already contains supplied main
`c410af910118045af05d4d52148a33802194398c`. Collection and development packaging
can proceed offline; this report does not clear distribution or native acceptance.
Production streaming remains disabled.

## Three missing standalone notices

[Archive evidence](licensing-evidence-20261115.json) records registry SHA-256,
published VCS receipts where available, source hashes and actual copyright/header
lines. Cached archives were checked against Cargo.lock and their Rust source
bytes compared with the extracted build inputs. None contains a named license,
COPYING, COPYRIGHT, NOTICE or AUTHORS file. Author metadata is not substituted
for a copyright notice.

| Locked package | Evidence | Concrete remaining action |
|---|---|---|
| mac 0.1.1 | [Published tag](https://github.com/reem/rust-mac/tree/66afc663b68a65633ea165c742b5a9c6734581c2) matches all eight Rust files and the manifest. README declares the Rust-style MIT/Apache choice; source has no copyright notice. | Owner review of the declared Apache option, complete terms and any required notices, or a reviewed dependency change. Do not invent an MIT holder from the author field. |
| pathfinder_geometry 0.5.1 | All nine published Rust files explicitly grant Apache 2.0 or MIT, with Pathfinder Project Developers notices (2019; angle.rs 2020). Published VCS revision `a5e98fac00f433fe2ff4b2135383d82491b8bfc5` is unavailable from upstream's tree/commit API. | Preserve these exact notices and review a complete declared-option terms bundle. Current available tree `6c3c0466` changes four source files substantively; its root MIT notice cannot silently replace these notices. |
| seahash 4.1.0 | [Pinned GitLab tree](https://gitlab.redox-os.org/redox-os/seahash/-/tree/94b632aeac099031c373599313d5b5f0acbbaec0) and published archive have no standalone notice; Cargo declares MIT. All nine Rust files lack copyright/license headers. | Obtain authoritative missing attribution, review an alternative dependency/version, or make an explicit owner distribution decision. A generic MIT template cannot supply an unknown holder. |

The [Apache text](https://www.apache.org/licenses/LICENSE-2.0.txt) supplies actual
standard terms, not proof that all attribution and redistribution obligations
for these packages are satisfied. The [MIT text](https://opensource.org/license/MIT)
contains a holder/year placeholder. Neither establishes missing historical facts.
All three empty-notice flags stay present in development BUILD.json.

## objc2 declarations versus notice completeness

Upstream [issue 826](https://github.com/madsmtm/objc2/issues/826) identifies
[ee9a7ada](https://github.com/madsmtm/objc2/commit/ee9a7ada2131f5944b8750428e265c15632f2a19)
as the fix adding full MIT/Apache/Zlib texts. The maintainer's October 2026
follow-up confirms older crates retain their Cargo-declared licenses, while
declining to confirm complete copyright attribution for earlier contributors.
The open [relicensing proposal](https://github.com/madsmtm/objc2/issues/23)
does not change the locked releases' declarations.

The four MIT-only packages are block2 0.6.2, objc2 0.6.5, objc2-encode 4.1.0
and objc2-foundation 0.3.2. Fifteen other flagged packages declare Zlib OR
Apache-2.0 OR MIT. Only their declared choices are collected. Each later text
has its own `revision` and `supplemental-reference` role, rather than being
attributed to the locked release's revision. The four MIT-only packages also
retain [the ancestor Steven Sheldon notice](https://github.com/madsmtm/objc2/blob/757cd841d0293341dbefd9b99d7548dec244ffde/LICENSE.txt)
with a `retained-ancestor-notice` role. Its removal commit `cfb19922` is in the
license-file history at all four pinned revisions. Neither those references nor
an authors list prove complete contributor attribution.

This adds 53 package notice copies (four unique upstream files), increasing the
Cargo inventory from 708 to 761. All nineteen review-limitations flags remain.
The packager still checks locked release identity, VCS receipt, metadata and
every copied file hash; the added references do not weaken those checks.

Apple SDK scope is a separate question. The public
[Xcode and Apple SDKs Agreement](https://www.apple.com/legal/sla/docs/xcode.pdf)
provides a reference for owner review, not evidence of which terms were accepted
for the generating SDK or this build. The owner needs to identify applicable
SDK agreements, generated bindings versus handwritten code, target platforms and
distribution method. No license agreement was accepted by this task.

## Integration and acceptance still required

The [acceptance matrix](acceptance-matrix-20261115.md) remains authoritative for
missing parity and native gates. No native IME, VoiceOver, clipboard, picker,
scaling, Windows/Linux GUI, signing, installation or update acceptance is added
by notice collection. The unresolved foreground approval and prior uncertain
launch/control are not retried or bypassed. The conflicting original checkout
remains preserved; no writers or Claude sessions are controlled.

The parent reports review of `4c32b7c..bd26cf04` found no confirmed new blocker,
but did not independently rerun tests. This is not combined-head merge approval.
Review receipts for the intervening runtime/protocol range after `4445d8b` are
not complete: the `3f6572f` recovery fix is confirmed, while completed receipts
for `9a14cf8` failure schedules and `4c32b7c` packaging were not recovered.
The current increment also needs independent review. Main's hosted Linux/macOS
CI is baseline evidence; it does not validate this unpublished GPUI branch.

Before integration, recover or repeat exact-range independent reviews, verify
the then-current main and ownership, and validate the combined source. Keep
streaming disabled until its causal/ordering guarantees and acceptance are
established. The owner must settle support tiers, product parity, native
performance budgets, distribution identity/channel and licensing scope before
release acceptance. No remote branch, PR or main is modified here.
