# Native Session export to a new file

Continues local `f750cc2bf3016367d6fb64bb6b9084c265e39dab` in the isolated
checkout. Parent-reported independent reviews found no confirmed blocker in
all five preceding local increments; tests/logs were inspected, not rerun.
This increment needs exact-patch review. Epic #1753 remains incomplete.

## Behavior and ownership

A ready, validated Session export now offers Save export in Session details.
It opens GPUI's system new-path picker with a fixed generic name based on
JSON/Markdown/HTML format, never the daemon-supplied Session name. No data
is written before the user chooses a path. Cancel writes nothing. The app
never opens, renders, executes or reveals the saved file. The existing notice
and tooltip disclose private conversation/Tool/plan/checkpoint data.

The source is exactly the accepted export text already available to Copy:
pretty-printed JSON with preserved future fields, or exact Markdown/HTML
Unicode, whitespace and CRLF. It is a past persisted read, not unsaved edits,
a live/transactional snapshot for every format or a complete Profile backup.
The existing 1 MiB bound applies. No new export RPC, provider request, daemon
mutation, protocol or retry behavior was added.

The Save callback freezes Session, connection, export revision, format and a
unique prompt token. Starting it retires old rendered button identities.
Only one picker may remain outstanding per window, including after a panel
closes. Its async task captures identity only, not another transcript/export
copy. At path selection, the same panel/export and actual connection readiness
are checked again before any file I/O. Panel replacement, disconnect/reconnect
or stale callbacks cannot write or reopen a picker. Window/entity loss prevents
the callback from writing. Existing composer/metadata drafts stay intact;
active editor preedit blocks opening the picker without committing it.

Export read/copy controls are disabled while the native picker is pending.
Completion/cancellation/error updates the owned panel notice and a separate
local status notice, without overwriting observed daemon/unknown-Run status.
The panel notice is a polite status node. Metadata modal focus/scroll anchors
include the new button at minimum size/200%. Real OS focus behavior and screen
reader announcements remain unverified.

## File publication and staging follow-up

The selected path must be absolute. The writer creates a private unique
sibling staging file, writes/syncs all bytes, closes it, then creates an
exclusive hard link at the chosen name. Existing files, symlinks, hard links
and directories are never replaced or truncated. Two concurrent savers can
publish only one complete file. No parent directories are created. Unsupported
volumes fail; there is no direct-write fallback or automatic retry. The chosen
name is never deleted during cleanup. Staging cleanup is best effort and only
for a file successfully created by this operation. Power loss, hostile directory
races, filesystem stalls and all-volume/platform behavior are not guaranteed.

While reusing the staging pattern, source review found that the preceding
preferences helper armed its cleanup guard before exclusive create succeeded.
On a staging-name collision, that error path could remove a file it did not
create. No such collision was observed in the prior runs. Both helpers now
construct ownership guards only after successful creation, and close the file
before handling write errors so Windows cleanup is possible. Two controlled
collision regressions preserve an existing canary file. This is unrelated to
the preserved conflicting checkout; none of its files were read or adopted.

## Validation

Same optional manifest, offline/locked dependencies, isolated target, two-job,
no-incremental and no dev/test debug-info environment as prior reports.

| Check | Result / sibling evidence |
|---|---|
| GUI-free library | 205 passed / `export-save-model-final.log` |
| GUI/Thai library and pure binary unit tests | 210 + 2 passed / `export-save-gui-final.log` |
| Final full shell suite, including attachment replacement | 173 passed / `export-save-shell-final.log` |
| Strict GUI/Thai clippy: lib, shell/render and binary | Passed / `export-save-clippy-final-head.log` |
| Windows GNU GUI-free all-targets check | Passed / `export-save-windows-model.log`; no GUI/link/native execution |
| Formatting/whitespace/scope | Passed; manifests, lockfiles, protocol, client transport and relay unchanged |

The final shell/clippy checks follow the polite-status role and the explicit
export retirement on client attachment replacement. No I/O/model or portable
code changes followed the full library and Windows checks. Exact
commands, exits and hashes are in the evidence manifest.

Six file/model cases exercise exact/private bytes, whole size bounds,
existing-file refusal, bad destinations, no temporary files in normal/error
paths, existing link preservation, fixed names/format retirement, controlled
staging collision and real two-thread publication contention. The preference
collision is separately covered. These use private Mac temporary directories,
not real user exports or personal preferences.

Five new shell tests exercise the real platform-picker API through GPUI's
TestPlatform, whose response is explicitly simulated: no OS dialog opens.
They verify no writes before selection, exact output beyond the preview,
cancellation, one outstanding prompt, stale buttons, existing-file refusal,
retained drafts, panel replacement, actual connection atomic retirement before
its UI update, client attachment replacement with repeated connection/Session
IDs, preedit, focus and 480x360/200% reachability. Save emits no wire
command or optimistic transcript block. These are view/file-boundary tests,
not native OS picker/keyboard/IME, screen-reader or real daemon evidence.

The final audit added an adversarial attachment test to the unpublished
`234d866` checkpoint. It reproduced an old export being writable after client
replacement reused connection 0 and the same Session ID
(`export-save-profile-reproduction.log`). Attaching a client now explicitly
retires the export state, including its opaque revision, document and format;
the five-case focused run passes in `export-save-profile-fixed.log`. This also
retires old Copy callbacks. The original checkpoint package remains preserved
as historical evidence and is not the final artifact. No real Profile data or
native OS picker was involved in that reproduction.

`export-save-shell-first.log` retains the initial test borrow-check failure;
a query entity was cloned before its mutable update. The corrected focused
run and broad checks pass. No failure was overwritten or counted as success.
No actual application, native picker or foreground control was launched.

## Integration holds

The original conflicting partial tree remains untouched and ownership
unresolved. Foreground approval/prior launch outcome, real native input/IME/
accessibility/scaling, Windows/Linux GUI and filesystem/picker acceptance,
remaining parity, performance and distribution gates remain open. Streaming
stays disabled. No writer control, denied-path/process inspection, Claude
restart, approval bypass, push, PR update or merge occurred.
Review this exact increment, resolve ownership, recheck live main/#1776/#2081
and validate the agreed integrated revision before publishing or merging.
The user's finished-GPUI/clean-main goal is not claimed complete.
