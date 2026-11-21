# Durable native desktop view preferences

Continues local `7658c8b582f0bd88b44751214bfe9cc53c36bd27` in the isolated
`formless-gpui-clean` checkout. Parent-reported independent reviews found no
confirmed blocker in the four preceding local commits, inspecting source,
tests/logs without rerunning tests. This new increment needs exact-patch review.

## Contract

Daemon mode reads a dedicated per-user `Formless Desktop/preferences-v1.json`
from macOS Application Support, Windows APPDATA or Linux XDG_CONFIG_HOME
(absolute only, otherwise HOME/.config). `--preferences PATH` selects an
explicit file; `--no-preferences` disables storage. Fixture invocations never
resolve personal preferences unless a path is explicit. `--text-scale` wins
for the current launch, without silently rewriting the saved file. The smoke
log now prints the resolved size instead of the optional raw CLI argument.

Commands exposes explicit Save desktop preferences and Reload desktop
preferences. Exactly six fields are serialized: schema version, text size,
light/dark appearance, docked sidebar and inspector choices, and send keys.
There are no Session IDs, endpoint paths, transcripts, drafts, answers,
credentials, security decisions or window bounds. Overlays are never restored.
Save is not automatic on edits or exit. Reload replaces unsaved view choices;
text, selection, undo/redo and drafts are retained. It refuses active composer
or browse-field composition and modal interaction. Applying a reload retires
pending forecast preparations through the existing epoch guard, never replaying
or committing an action.

The first read creates no file/directory/lock. Missing data uses defaults.
Files must be regular, no more than 64 KiB, and match the strict v1 schema and
supported text-size steps. Corrupt, oversized, unknown-field and future-version
files remain unchanged; defaults are used at startup and Save refuses to replace
them. Failed Reload retains current view choices. Repair/move the file and
Reload explicitly to recover. There is no invented older schema to migrate.

Save obtains a nonblocking advisory lock using a stable lock inode, compares
the exact bytes loaded by this window, writes/syncs a unique sibling temporary
file and renames it into place. Successful saves update the comparison basis;
failed writes/replacements remove staging files. Another cooperating window's
change is refused until explicit Reload. Busy locks are never waited on and
no writer is controlled or interrupted. Unix saved files/locks use private
creation mode. Ordinary preference/lock symlinks and nonregular files are
refused. This protects cooperating desktops, not hostile directory races or
uncooperative writers; no filesystem latency, power-loss, Windows/Linux native
locking/rename or durability acceptance is inferred from the Mac tests.

Status notices say whether a file is missing, saved, invalid or conflicting,
without echoing its contents. At minimum-size/200% a status can be taller than
the viewport. Focused Status now supports arrows, Page Up/Down, Home/End,
scoped to the reflowed page and blocked by modal/focus checks. Both ends are
keyboard-readable; simultaneous visibility of all prose is not claimed.

## Validation

All Cargo commands use the optional manifest, offline/locked dependencies,
the isolated task target, two jobs, no incremental compilation and no dev/test
debug info. Current toolchain observed: rustc 1.100.0. Raw evidence and exact
command/exit/hash records are in sibling task `evidence/`.

| Check | Result / log |
|---|---|
| Full GUI-free library | 198 passed / `preferences-model-final.log` |
| Full GUI/Thai library + binary unit + shell | 203 + 2 + 168 passed / `preferences-gui-final.log` |
| Final focused preference library/binary/shell | 9 + 2 + 5 passed / `preferences-focused-final.log` |
| Strict GUI/Thai clippy for lib, shell, renderer and binary | Passed / `preferences-clippy-verified.log` |
| Windows GNU GUI-free all-targets check | Passed / `preferences-windows-model.log`; no GUI/link/native execution |
| Formatting/whitespace/scope | Passed; protocol, transport, manifests, lockfiles and relay unchanged |

The final focused check and clippy follow the last notice/hint clarification
and corresponding missing-file assertion. The full suites and Windows
cross-check preceded only those user-facing string changes. No code affecting
serialization, I/O, keys, dispatch or state changed after the full suites.

Storage tests cover round trips, no writes on missing reads, schema and size
rejection, no canary-content echo, stable lock/busy behavior, two-window lost
update refusal and Reload recovery, saved-byte preservation, Unix permissions,
symlinks and non-directory paths. CLI tests inject the default-path resolver,
proving fixture/disabled/explicit modes do not even resolve personal paths;
they run pure functions, never the application main loop.

Five headless shell tests cover explicit save only, exact projection, retained
composer identity/draft/selection, corruption/conflict recovery, modal/preedit
ownership, actual palette activation and status reachability at 480x360/200%.
The serialized recording-client case retires a held preparation on Reload and
rejects its late reply without CommitPrepared/Intent or optimistic transcript.
These are view-boundary tests, not daemon/paid-Provider evidence. No app was
launched, installed, foregrounded or granted OS permission; native fonts,
clipboard, screen readers and real input methods were not exercised.

Retained failures: `preferences-model-first.log` expected seven JSON fields
instead of the six actually specified; `preferences-gui-first.log` used the
opaque readable return type with an interactive helper; `preferences-compile-second.log`
still formatted optional CLI size as Display. All were corrected. The first
shell log expected only one palette match instead of the Save and Reload
substring matches. The second shell log exposed the real long-status keyboard
reachability gap, fixed and tested in the third/fourth/final runs. No failed
log was overwritten or counted as a successful command.

## Remaining gates and integration

The [acceptance matrix](acceptance-matrix-20261115.md) now records this bounded
view-persistence slice. System/high-contrast/colorblind themes, font/density/
reduced motion, geometry and per-Session view persistence remain outside it.
Native filesystem and frontmost/native performance acceptance remain open.

The preserved conflicting tree is untouched and ownership remains unverified.
Foreground approval and the prior uncertain launch/control outcome are still
unresolved; no retry/workaround, denied-path/process inspection, writer control
or Claude restart occurred. Streaming stays disabled. No remote publication,
PR state change or merge was performed. Epic #1753 and clean-main integration
remain incomplete pending the substantive gates, exact-patch review and safe
ownership/integration resolution described in the matrix.
