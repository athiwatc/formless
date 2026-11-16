# Local licensing evidence: remaining preview release gates

This investigates independently reviewed preview `9144bb5` using only the locked
registry archives, extracted build inputs, collected immutable snapshots and
existing local development package. It changes no runtime, dependency, notice
manifest, package output or license choice. All earlier branches and artifacts
remain preserved. No further packaging omission was substantiated by this audit.

The [technical evidence inventory](preview-license-local-evidence-20261116.json)
records exact release identities, archive/manifest hashes, published VCS receipts,
collected roles, source markers and scope limitations. All 22 affected archive
checksums match Cargo.lock; regular archived files match the extracted inputs.
The collected snapshots match their hashes; where a Git blob identity is
recorded, that identity also matches.
This verifies bytes and existing provenance records, not new upstream ancestry,
complete rights chains or legal applicability. No external source was fetched.

## Missing collections versus unresolved legal scope

| Release/group | Verified local facts | Remaining distribution decision/evidence |
|---|---|---|
| mac 0.1.1 | No standalone notice or source copyright header was identified. The published README declares MIT/Apache-2.0; its primary-author paragraph is author metadata. | Establish acceptable use of a declared option and any required attribution/notice scope, or select a reviewed dependency alternative. Do not turn the author paragraph into an invented copyright notice. |
| seahash 4.1.0 | Cargo declares MIT. No standalone notice or source copyright/license header was identified in the locked archive. README discusses the algorithm, not a full grant. | Authoritative missing attribution/grant evidence, or a reviewed dependency/version alternative. The local archive does not resolve the holder; no template is filled in. |
| pathfinder_geometry 0.5.1 | All nine exact leading copyright/dual-license headers are now preserved, including the 2020 angle.rs notice and eight 2019 notices. Archive, VCS, full source hash and exact prefix checks passed. | Review the declared license path and complete package obligations. This is no longer an empty notice collection, but its scope flag stays open. No newer upstream MIT notice is substituted for this release. |
| Four objc2-family MIT-only releases | block2 0.6.2, objc2 0.6.5, objc2-encode 4.1.0 and objc2-foundation 0.3.2 declare MIT. Each retains its historical licensing explanation, later MIT reference and earlier Steven Sheldon notice. | Establish complete applicable attribution and grant coverage for these historical releases; current reference presence alone does not prove it. SDK scope remains separate. |
| Fifteen other flagged releases | dispatch2 0.3.1 and fourteen framework 0.3.2 releases declare Zlib OR Apache-2.0 OR MIT. Each retains its explanation and all three later full reference texts. | Choose and validate an applicable declared option and remaining obligations for the locked release. The later reference revision is not relabeled as its publishing revision. SDK scope remains separate. |

There are 72 collected notice/reference records for the nineteen objc2-related
flags, plus nine Pathfinder source headers. The existing package still has two
empty collections, twenty scoped-warning packages and 770 dependency notice
records. None of those warning flags should be removed based on this audit.

The complete 9,723-byte Apache text is already present under the package's Rust
toolchain notices, hash `62c7a1e35f56406896d7aa7ca52d0cc0d272ac022b5d2796e7d6905db8a3636a`.
It matches the collected immutable Rust source text. Therefore the package does
not lack every copy of Apache terms. That reference neither selects an Apache
option for mac/Pathfinder nor settles their particular grant/notice obligations.
Duplicating it or inventing a holder would not resolve the remaining facts.
No new full-license text is synthesized or attributed to a different publisher.

## What the SDK evidence establishes

The retained upstream licensing explanation explicitly raises SDK derivation
and distribution uncertainty. The archives also contain 1,013 Rust files in
sixteen affected releases with explicit automatic header-translator markers.
Those are evidence of generation, not proof of the generating SDK version,
which agreement was accepted, what rights cover each portion, or the owner's
intended redistribution method. Copyright-named API constants and methods are
not treated as copyright notices.

This task does not read personal agreement receipts, accept an SDK license,
invoke a license-acceptance command, or infer rights from successful compilation.
The owner needs to identify the applicable SDK/toolchain inputs and distribution
scope, then obtain the appropriate licensing review or choose dependency/support
changes. Existing later-text and ancestor-notice references remain useful evidence
without clearing these questions. A different declared license for some crates
does not by itself settle SDK-derived content.

## Safe remaining work and holds

The audit found no additional year-bearing source copyright notice omitted by
the existing source-header collection. That filename/year-pattern scan is bounded
technical evidence, not proof that no other attribution obligation exists. All
known nine notices remain packaged. Existing notice bindings reject drift; no
check, warning or legal uncertainty is weakened to make the package appear ready.

Owner/legal decisions remain for the two empty collections, Pathfinder terms,
nineteen objc2/SDK scopes and broader assets/stdlib/component coverage. Signing,
notarization, installation/update and target-specific distribution are separate.
The existing preview package remains a development artifact at `9144bb5`; this
documentation audit does not relabel it as a build of a later commit.

No denied time/sysctl operation, uncertain foreground launch or listener test is
retried. CPU/RSS, native/platform/performance, hosted billing and full required
parity/security gates remain open. Schema stays 67; streaming, Queue and journaling
remain disabled. There is no personal-data access, install, publication or merge.
