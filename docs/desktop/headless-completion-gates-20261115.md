# Headless completion checks and combined integration

The isolated failure-schedule head `9a14cf8` is merged locally with supplied
main `c410af910118045af05d4d52148a33802194398c` at `f89d196`. The merge has no
conflicts and leaves desktop/core/protocol source and both lockfiles unchanged.
The main changes add CLI reports, attachment input handling and documentation.
No remote branch, PR or main is changed by this continuation.

## What can be checked without the unresolved foreground action

| Gate | Headless check | Practical limit |
|---|---|---|
| Implemented interaction parity | Existing headless GPUI suite: dispatch, focus, drafts, modal/IME guards, selection ownership, preferences, export, reading positions and reader batches | TestPlatform has synthetic fonts/input; missing product features cannot be accepted by tests of implemented ones |
| Streaming correctness | Nineteen private-daemon schedules, including Tool/Step/schema/compaction, manual retry/Queue, EOF, server pressure, delayed questions/status and edited history | Representative schedules; independent causal/ownership review, product fallback acceptance and production enablement decision remain open |
| Accessibility plumbing | Eight checker tests using controlled stub executables; model accessible-text caching/bounds and conversion timing | No Formless executable or OS accessibility tree is launched; no VoiceOver/NVDA or native semantics acceptance |
| Model performance | Existing stream-model replay and bounded full-window conversion check, using cached dev profile and no GUI | Debug timings, simulated clock and model only; no native frame, CPU/RSS, accessibility service or large-history budget accepted |
| Platform compatibility | Native Mac compilation/headless tests and installed Windows GNU no-GUI cross-check | No Linux Rust target is installed; no current Windows/Linux GUI execution or support-policy decision |
| Development packaging | Pure Python safety/determinism tests; native build/metadata, archive and notice verification; repeat package comparison | No install, launch, signing, notarization, update or clean-machine acceptance |
| Licensing inventory | All prior 649 Cargo notice files retained; lowercase Markdown licence and embedded PSL MPL notice added, totaling 651 | Forty-four built dependencies still have no collected notice file; assets, stdlib and distribution review remain open |

Native input/clipboard/picker/volume behavior, human scaling/HiDPI/trackpad,
assistive technology, security approval/Step controls and restored Step prompt
inventory remain separate gates. Cross-block selection/copy, richer attachments,
editable resources and the other missing parity items in the acceptance matrix
need implementation/product decisions; this headless pass does not supply them.

## Concrete performance evidence and limits

The existing no-GUI `stream_model` example completed its five ordinary replay
scenarios (200 trials each) and one accessible-word scenario before its fixed
debug accessibility trial count became impractical. Only that owned benchmark
session was stopped; no other process/writer was inspected or controlled. Its
log is partial evidence, not a successful complete benchmark. The 100,000-delta
burst performed one row rebuild and copied 32,431 preview bytes; the simulated
1,000-delta/s, 60-second case performed 3,750 rebuilds and copied 117,193,568
preview bytes. These are model-work counters, not measured display throughput.

The separate bounded 20-iteration check converted a 32,802-byte Markdown window
to accessible text in about 102 ms per iteration in this dev build on this Mac.
Its test name says “quick enough”, but it only prints timing and enforces no
frame budget. Passing that test cannot clear a performance gate. Accessible-word
replay in the partial example also shows nontrivial conversion cost. Optimized,
frontmost/native profiling and accepted budgets remain required; no release
profile cache is built merely to turn this observation into a pass.

## Development package changes

The collector now preserves embedded notice paths and records their SHA-256
hashes. It still refuses symlink/out-of-package notice sources and oversized
files. Root and embedded notices with the same filename cannot overwrite one
another. The pinned Public Suffix List's `data/LICENSE-MPL-2.0` and Markdown's
lowercase `license` are included; no previous collected file is removed.

ZIP entries use commit-time UTC timestamps clamped to ZIP's range/granularity,
sorted paths and normalized regular-file/executable modes. Reproducible archive
bytes require identical binary/content and ZIP implementation; this is not proof
of cross-host reproducible compilation. Source commit and cleanliness are
rechecked after building and immediately before publication from staging.
Dirty development inspection remains explicitly marked and is not a
commit-identifiable source acceptance claim. Tests simulate a late commit change
and confirm no output directory is published. Existing output is never replaced.

The exact combined head, aggregate results, repeated archive hashes, source and
notice inventories are frozen in the sibling task evidence manifest. Tests and
package inspection use the owned target with two build jobs and a 2 GiB headroom
guard. The original conflicting checkout is preserved. Production streaming
stays disabled, independent review is pending, and the unresolved Formless Sol
Validation approval/prior uncertain foreground action is not repeated or bypassed.
