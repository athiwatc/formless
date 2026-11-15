# Accessible-text profiling and notice collection

This continues combined head `4c32b7c` in the isolated integration branch.
Production streaming stays disabled. No foreground Formless action, renderer
execution, publication or change to the conflicting checkout occurs.

## Profile before changing behavior

The bounded `accessible_profile` example measures the same 32 KiB fixtures for
20 rounds with no GUI. It separates existing `markdown::to_mdast` parse/AST-drop
cost from complete accessible-text conversion. The repeated Markdown case spent
about 137 ms parsing versus 132 ms converting; structured content spent about
265 ms versus 268 ms. Timer noise aside, parsing dominates. The writer is not
replaced, and no Markdown construct or visible word is dropped.

Only the optional desktop workspace's dev/test `markdown` package is compiled at
optimization level 3. Desktop code and other dependencies retain their existing
profiles; root workspace profiles and release settings are unchanged. Cargo
receipts verify Markdown opt-level 3 and desktop opt-level 0 in both profiles,
with debug assertions and overflow checks still enabled.

| Fixture | Before median | After median | Output |
|---|---:|---:|---|
| Words, 32,798 bytes | 8.427 ms | 1.471 ms | Same length/digest |
| Markdown, 32,802 bytes | 132.496 ms | 4.891 ms | Same length/digest |
| Lists/table/links, 32,844 bytes | 267.694 ms | 28.614 ms | Same length/digest |

This improves the observed debug blocker without changing parser options,
source, AST traversal or accessibility semantics. Existing correctness and
headless interaction suites are rerun. These are local samples, not a native
SLA. The structured case still has material cost; accepted frame/CPU/RSS budgets,
native profiling and assistive-technology cost remain open. Release builds
already optimize dependencies, so no improvement to an already optimized release
parser is claimed.

## Evidence-backed notice collection

All 651 previously collected files remain. Reviewed upstream/workspace snapshots
add 57 package notice files, bringing the Cargo inventory to 708, with 41 previously
empty inventories now carrying source material. Archives were checked against
locked registry checksums, published VCS bytes verified, and downloaded files
checked against immutable Git tree/blob identities. The offline packager rechecks
lock, metadata, VCS, path, size and content bindings. Changed inputs fail closed.

Rust's three top-level notices are verified at compiler commit
`0ef1bc386059596dada8fb65b50425aab2ea637a`, and the installed toolchain's 1,512,515-byte
COPYRIGHT-library.html is preserved with its hash. Packaging requires the same
compiler commit and includes four files with provenance.

These counts are not legal clearance. Nineteen objc2-related files are licensing
explanations referencing external terms and Apple SDK implications, flagged
explicitly rather than counted as completed review. `mac 0.1.1`,
`pathfinder_geometry 0.5.1` and `seahash 4.1.0` remain without collected notice
files for the reasons in the notices README. No author, grant, current-branch
licence or guessed repository was substituted. Manual scope/asset/runtime/
component and distribution review remains necessary.

Exact source/profile receipts, before/after data, tests, collection receipts,
unresolved probes and clean artifact verification are frozen in the sibling task
evidence manifest. Native IME/VoiceOver/clipboard/scaling/picker/filesystem,
Windows/Linux GUI, missing parity, signing/notarization/install/update and original
partial-tree ownership gates remain open. Independent review is required before
integration/publication. The unresolved foreground approval/action is not retried.
