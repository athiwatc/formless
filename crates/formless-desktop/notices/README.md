# Reviewed notice snapshots

Packaging is offline. It never fetches notices from the network. `UPSTREAM.json`
binds these snapshots to the desktop Cargo.lock, package name/version, Cargo
licence declaration, repository metadata and published VCS receipt. At collection
time, cached `.crate` archives were SHA-256 checked against Cargo.lock; their VCS
receipt bytes matched the extracted source. Upstream notice bytes were verified
against Git blob hashes in the exact referenced repository tree. Paths, URLs,
resolved symlink chains, revisions and hashes are retained in the manifest.
The workspace crate uses this repository's unchanged LICENSE, with its hash
checked again at packaging time. No current-branch or neighbouring-version
licence text is substituted for an unavailable pinned source.

The collector checks lock/VCS/repository/declaration/content bindings, path
containment, regular files and size limits. Changed inputs require refreshed
technical receipts; there is no network fallback. Source/hash records appear in
DEPENDENCIES.json and the manifest is included as UPSTREAM_NOTICES.json.

This is evidence collection, not a legal compatibility or distribution decision.
Nineteen objc2-related release snapshots contain an upstream licensing
explanation. Supplemental full grant texts from upstream's later `ee9a7ada`
fix are now retained with their own revisions and roles: MIT for four MIT-only
releases, all three declared options for fifteen multi-licensed releases.
The four MIT-only packages also retain the earlier Steven Sheldon MIT notice
from `757cd841`, the parent of the license-explanation change in their source
histories. Supplemental references do not replace release notices, select a
distribution license, or establish complete attribution. The maintainer confirmed
old declarations but did not confirm the later copyright notices are sufficient.
Apple SDK implications and earlier contributor attribution remain explicit
limitations in each package record and BUILD.json. File presence does not
resolve them. Three packages still have no
collected notice file:

- `mac 0.1.1`: the published release tag's Cargo manifest and all eight Rust
  source files match the locked archive, but that tree has no notice file.
- `pathfinder_geometry 0.5.1`: all nine published Rust files have explicit
  Apache/MIT headers and 2019/2020 Pathfinder Project Developers notices, but no
  standalone terms. Its published revision is unavailable through the declared
  upstream tree/commit API (HTTP 422); a newer available tree has substantive
  source differences and is not substituted.
- `seahash 4.1.0`: the pinned GitLab tree is available but has no root notice
  file; the published crate also has none. No grant text was synthesized.

`TOOLCHAIN.json` binds Rust's top-level Apache/MIT/COPYRIGHT texts to the compiler's
exact reported source commit, with upstream Git blob verification. It also
preserves the matching installed toolchain's COPYRIGHT-library.html with its hash
and relative origin. Four files and their provenance are bundled under
toolchain-notices. Component/target coverage, assets and distribution obligations
still need review. A compiler change requires fresh verified snapshots.

To refresh, inspect locked archive/VCS receipts, retrieve corresponding immutable
upstream text, verify Git blob and SHA-256 hashes, update the manifest and review
scope. Never fill unknown copyright holders or grant terms from templates. Run
package tests, package twice from clean source and verify receipt hashes and
reproducibility. The task evidence directory retains collection scripts, network
receipts and unresolved probes; missing-file reports are preserved.

See the [licensing scope and release report](../../../docs/desktop/licensing-scope-20261115.md)
for authoritative references, locked archive evidence and owner decisions.
