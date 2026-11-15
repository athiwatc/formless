# Session archive and restore continuation

Draft #2081 continues epic #1753 after independent review of `742bbf5a`.
This increment adds archive/restore and archived Session browsing. The epic
is incomplete; streaming remains disabled and the PR remains draft.

## Scope and contract

Session details offers a fourth Archive field. Archive/Restore sends one
existing `SetSessionArchived { id, archived }` setter for the frozen Session.
Archive hides the Session from the default list and retains its history. It
does not stop its Run. Restore returns it to the default list and never
activates it or a neighbouring Session. The composer draft is retained in
the existing bounded in-window draft cache when the open Session is archived.

The directory's opt-in Archived toggle includes cached archived entries,
labelled with their archived state and restore-details behavior. Opening an
archived row reads its metadata before offering Restore, without installing
a transcript or changing the open Session. Title, preview, ID and tags can
be filtered together. Tags shown in rows are bounded previews. When a chosen
identity disappears during a source refresh, selection clears and Enter
does nothing; explicit arrows or a deliberate filter/scope change can choose
another preview. Rendering and filtering never dispatch mutations.

Metadata reads now use a fresh `ListSessions` reply instead of downloading
the full `GetSession` transcript. Independent request UUID, connection and
frozen Session ID still own the reply. Missing, duplicate or malformed target
metadata fails closed. This uses the existing bounded frame decoder; it does
not establish a smaller total list size, latency or performance budget.

Save callbacks retain the displayed Session ID, connection, field and toggle
direction. A stale Archive callback cannot silently become Restore, nor a Pin
callback Unpin, after a fresh read; stale target/connection/field callbacks
send nothing. Success must acknowledge the requested ID and boolean. Existing
unknown-outcome, reconnect, IME and explicit Reload behavior remains in force.
The setters still lack compare-and-swap: another client's intervening field
change can be overwritten, as the editor discloses. No mutation is replayed.
Unrelated metadata replies cannot clear an unknown Send or question answer.

At minimum size and 200% text, the daemon directory grows within the scrolling
page to leave room for a complete row below its wrapped toolbar. Arrow keys
reveal the list container, while the virtual list brings the chosen row to
its top. Typing a filter returns the query to view. Scope changes retain the
existing marked-text guard. Archive and Restore have explicit accessible
button labels; focus remains in the editor until cancellation.

## Verification

Final runtime and tests: `77ad3c53b2292911d984f02d5697055c732ced98`.
Raw evidence remains under ignored `evidence/gpui-1753-sol/`.
All Cargo checks use the optional manifest, `--offline --locked`, two jobs,
incremental disabled and dev/test debug info disabled.

| Check at `77ad3c5` | Result / evidence |
|---|---|
| `cargo test --no-default-features --lib` | 171 passed; `archive-final-model.log` |
| `cargo test --features test-support,thai-breaks --lib --test shell` | 176 library and 139 shell tests passed; `archive-final-gui-tests.log` |
| `cargo test --no-default-features --features daemon-tests --test daemon` | Five real daemon/provider cases passed; `archive-final-daemon.log` |
| `cargo test --all-features --doc` | Passed, zero doctests; `archive-final-doc.log` |
| Strict clippy (`-- -D warnings`) | GUI-free lib/unit tests; GUI/Thai lib + shell/render; ax-dump/Thai binary; daemon fixture passed. `archive-final-clippy-{model,gui,bin,daemon}.log`. 26 existing core dependency warnings remain. |
| Windows GNU GUI-free `cargo check --all-targets` | Passed; `archive-final-windows-model.log`. No GUI build, link or execution claim. |
| Native Mac fonts/Metal offscreen renderer | 47 states / 83 captures passed; `archive-native-sixth.log`, PNGs in `archive-native-sixth/`. |
| Formatting, whitespace, normal dependency isolation | Passed; `archive-final-{model,gui}-normal-tree.txt`, `archive-final-dependency-isolation.json`. |

The portable and lint runner records exact command arrays and exit codes in
`archive-final-checks.json`. Selected minimum-size Archive, Restore and archived
directory captures were visually inspected. New native states assert button,
footer and selected-row bounds and use actual text input for tag filtering.
Native shaping still emits the documented pinned-layer Thai diagnostics.
This does not establish foreground input, OS IME, selection, VoiceOver speech
or human usability acceptance.

The actual daemon/fake-provider fixture archives and restores a Session while
its provider reply is deliberately held, after navigation to another Session.
It checks both exact typed ACKs and canonical stored metadata, and verifies
`running: true` throughout. This exercises the real setter path; it does not
claim a native external provider or real-user workflow pass.

Adversarial coverage includes old action direction, target and connection;
no neighbour activation after archive/restore or source removal; composer
Unicode preservation; archived scope/tag filtering; IME scope guards; fresh
metadata reads that reject missing/duplicate/malformed targets; and keyboard
row visibility at 480×360 / 200%. The stale callback case invokes the actual
Save handler with a previously captured action: the test window's click helper
redraws before dispatch and cannot itself preserve a stale callback.

Failed attempts were retained. The first stale-click test incorrectly assumed
the helper did not redraw. The first native-directory fixture used a
programmatic input setter that emits no ordinary Change event. After input
delivery was corrected, native assertions found a real 200% clipping bug:
only 75 pixels were available for a 144-pixel Session row. The layout and
keyboard reveal correction followed. One reveal attempt ran in the wrong
GPUI phase; another used a virtual row's local anchor with the outer page.
The final implementation anchors the ordinary list container during rendering.
The failure logs/captures are historical, not final validation.

Two attempts encountered a full shared disk. Only this isolated checkout's
generated test executables, incremental/cross caches and static Rust build
intermediates were reclaimed. Sources, app executables/runtime assets, all
prior bundles and evidence were preserved; nothing in other worktrees was
touched. Reclamation inventories are `archive-cache-reclamation.json`,
`archive-static-cache-reclamation.json` and, after all corresponding test
processes exited successfully, `archive-final-test-cache-reclamation.json`.

## Fresh development artifact

The packager ran with ResourceWarning treated as an error, `--profile dev
--offline`, from clean `1d97c6f7995f93ced7caa21364a788e4e5f928e1`. Only README
and evidence documentation differ from tested runtime `77ad3c5`.
Output: `evidence/gpui-1753-sol/package-1d97c6f`.

- Binary SHA256:
  `acfee8370c9b6bb53d85f8462563599d0a71109c37c82948456f10dcbe32f3c7`.
- ZIP SHA256:
  `094820c8e3d832280c4fe762ccd5eb67f04ae414f1573f1aec67ce3b54769db2`.
- Provenance/plist identity, safe archive paths/types, integrity, executable
  mode and byte identity, hashes and 21 system-library loads passed.
  Evidence: `archive-final-package.log`, `archive-final-package-verification.json`.
- 655 archive files; 415 packages / 649 notice files; 46 packages lack root
  notice files. Manual licensing/assets/stdlib review remains.
- Static signature inspection reports linker ad-hoc signing, no Team ID,
  an unbound plist and no sealed resources. No app was installed or launched;
  no distribution signing or notarization occurred.

Earlier bundles/captures remain historical and do not validate this code.

## Ownership and remaining gates

The same isolated clone and `work/sol-gpui-1753-20261115` branch are used.
Main was `e9bf104fb9c1bf1bff4f56e05efa92b17a1bfc14`, already included.
Original #1776 remained `f8c23e0d5726fdc7d50989109f198301feb45cc9`; its worktree
was clean on read-only inspection. No original branch, worktree, writer or
paused session was changed or controlled. Terminal activity remains unverified
after the earlier denied process inventory; no alternative inventory was used.
Protocol, dependency manifests and both lockfiles are unchanged by this increment.

Foreground Mac keyboard, text selection, actual OS IME, VoiceOver, human 200%
acceptance and bundle launch remain blocked by existing Computer Use approval
`proud-river-57508678` for “Formless Sol Validation.” The supported card read
still had no owner response. The previous launch/control outcome remains
uncertain. It was not repeated, substituted or bypassed; no OS permissions
were changed. Offscreen renderer tests do not resolve these gates.

Windows/Linux native GUI execution, safe live-token ordering/reconciliation,
native security approvals/Step and broader action/screen parity remain open.
The [ordering report](stream-ordering-20261114.md) still applies: `deltas:false`,
event invalidation, canonical snapshots, no invented replay/cutoff contract.
Recovery commands remain Session-scoped rather than conditioned on a displayed
Run ID. Performance budgets, durable preferences, licensing/assets/stdlib
review, icons, signing/notarization, installation and updating remain open.
The unrelated relay fixture fix belongs to draft #2151 and was not duplicated.

Parent review must inspect the exact published head and recheck live main and
#1776 ownership before integration. Do not merge or close epic #1753.
