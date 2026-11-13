# Native desktop client (epic #1753): Phase 1 progress

Phase 0 is **not** accepted. Its open gates (findings §7, §8, §16.13,
§16.14) stay open and are listed again below so this page cannot be read
as a pass. Phase 1 work here is limited to slices that are safe without
them: the desktop crate only, no daemon, domain, auth, credential or
security-control change, no backend writes, existing clients unchanged,
and the crate still optional (detached workspace, not a default build).

## Done in Phase 1

| Slice | Commit | What it is | Tests |
|---|---|---|---|
| Typed command registry | `a23f7181` | `src/commands.rs` (no GPUI): every shell command with label, palette title, group, hint, on/off state and the reason it cannot run. Header buttons, composer buttons, global shortcuts and composer Enter/secondary-Enter all run through `Shell::run_command`, which re-reads availability at the moment of running. Composer commands keep the `ComposerActions` intent semantics; they still only record an `Intent`. | 8 model, 4 shell |
| Command palette | `a92ab626` | Cmd/Ctrl+Shift+P or the header Commands button. Modal, focus-trapped, background out of the accessibility tree, focus returned to the opening control; filter, arrows, Enter/click, unavailable commands say why; input-method composition in the search field is never turned into a run or a close; fits 480x360 at 200%. | 7 shell, 2 render states (real fonts) |
| Typed reply decoding | `1470342d` | `src/replies.rs` (no GPUI, no I/O): `ListSessions` into `SessionInfo`s; `GetSession` into a `SessionSnapshot` (Session id, `running`, timeline blocks in sequence order, hidden empty assistant turns). The TUI's role mapping (`user`, `tool`, `system-note` as a new `Note` block, anything else the assistant's). A reply for another Session, a missing or malformed field, a negative or repeated sequence number is a named error, never an empty list. `Shell::show_session_snapshot` replaces the open Session's timeline (a stream in progress included) and refuses a snapshot for any other Session. Run state is not taken from it. `serde` and `serde_json` became direct dependencies (already locked; two lockfile edges, no new package). | 6 model, 1 shell (mutation-checked: removing the Session guard fails it) |

One deliberate behaviour change came with the registry: while an input
method is composing in the composer, Send/Steer/Queue are unavailable
("Finish composing text first"), so an uncommitted reading is never
recorded. Stop is not held back.

Evidence (local, git-ignored): `.screenshots/gpui-1753-phase0/p1-registry-run1/`,
`p1-palette-run1/`, `p1-palette-run2/` and `p1-replies-run1/` (fmt, clippy in
three feature sets, all crate tests; every log ends `# exit: 0`).

## Not done, and what each needs

### Phase 0 gates (still open; nothing in Phase 1 closes them)

| Gate | Needs |
|---|---|
| Windows build, render, input and accessibility at the current head | The Windows worker (separately owned; not duplicated here) |
| Linux build and render | The Linux worker (separately owned) |
| Real input methods (Thai, Japanese, dead keys) in the composer and now the palette search field | A person at the Mac |
| VoiceOver over regions, overlays, the palette, and whether it follows the palette's active-descendant row | A person at the Mac |
| 200% and display scaling looked at by a person | A person at the Mac |
| Performance budgets | A decision by the user (§11.4, §14, §16.10) |
| Packaging and supported platform tier | A decision by the user |

### Phase 1 and later (from findings §6 and §16.14)

| Item | State | Blocked on |
|---|---|---|
| Command registry and palette | Done (above) | |
| Header toggles exposed as toggle buttons (pressed state) | Not done: the kit's `Button::toggled` would change their platform role, which the macOS tree check (§16.12) would need to re-baseline | A real-window check run in the background |
| Typed decoding of daemon replies into the presentation model | `ListSessions` and `GetSession` done (above); events (`MessageAppended`, `MessageDelta`, `DeltasDropped`, `StreamRestarted`, Run events) not started | Nothing for the decoding; applying events needs the reconciliation rules in findings §6 |
| Client transport (connect, reconnect, version check, idempotent send) | Not started | Decision §5.2: extract `formless-client` or depend on `formless-tui` |
| Transcript paging | Not possible without a protocol change | Daemon work outside this epic (§5.3) |
| Destinations other than Chat (Sessions, Tools, Metrics, Proposals, Settings, Search) | Scaffold notices only | The client layer for real data; UI-only versions possible over fixtures |
| Approvals, questions, credentials, setup | Not examined, by instruction | Explicit scope from the user |
| Full-output view of a long block (§12.1) | Not started | Nothing |
| Thai line breaking wired in (§11.6, §5.1) | Study only | Decision §5.1 |
| Live-region and accessible-text performance (§16.10) | Measured, not changed | Budgets |
| Parity with the TUI (§6) | Inventory only | All of the above |

## Next slice

**Full-output view of a long block (findings §12.1).** A row shows at most
32 KiB or 400 lines of its block, with controls to move that window. A
command (in the registry and so the palette) and a row control that open
the block whole in a modal, scrollable, selectable view with the same
plain text for assistive technology, bounded by what the timeline already
holds (no new copy of a 5 MiB block per frame), with focus returned to the
row on close. UI-only, desktop crate only, no daemon change.

After that, still without waiting on §5.2: decoding of the transcript
events into timeline changes (append, delta, edit, dropped-deltas
reconciliation) as pure functions with tests, then UI-only Sessions and
Search destinations over the fixture.
