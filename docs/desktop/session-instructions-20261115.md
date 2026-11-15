# Native Session standing instructions

Draft #2081 continues epic #1753 after independent source review of `76294cd5`.
Final runtime/tests: `af8b8c0c92be23c5920cafe6a0de4138853335be`.
The PR remains draft, streaming remains disabled, and the epic is incomplete.

## Behavior and scope

Session details now has an Instructions field. It uses the existing
`SetSessionInstructions` request, with the same frozen target, connection,
operation, field and explicit Save guards as the other metadata setters.
The dialog discloses that saved changes take effect at the next agent step,
including during a Run. It does not stop/start a Run or call a provider.
Leading/trailing whitespace is trimmed by the existing storage setter; blank
clears the field. Interior whitespace, line breaks and Unicode are retained.
Save replaces the whole field and has no compare-and-swap condition.

Native instruction input is bounded to 16 KiB. A fresh metadata reply that
omits `instructions` is unavailable, not an invented empty value. Oversized
instructions retain only their byte count in the editor projection, never a
truncated replacement string. Either condition disables instruction Save and
hands off to the TUI without disabling name, tags, pin or archive. A previously
owned bounded draft remains visible/copyable if an unrelated acknowledgement
reports that the current instructions have become unavailable or oversized.
Explicit Reload still discards edits as disclosed before the request.

Editing during a pending save keeps the newer local text when its ACK arrives.
Copy edit copies the exact owned text before trimming. Unknown outcomes and
reconnect retire Save until explicit fresh Reload; stale replies cannot
acknowledge a different operation or replace the current field. No mutation
is automatically retried. Composer drafts and Session activation are unchanged.

The acknowledgement audit reproduced an existing default-value hole: an
omitted `pinned` field decoded as `false` and could acknowledge an unpin.
Every metadata setter now requires its exact field to be present before typed
value/identity validation. Missing pin/archive/tag fields cannot acknowledge
`false` or empty values, and omitted instructions cannot acknowledge a clear.
Explicit valid `false`, empty list and null defaults still succeed. Malformed,
foreign or mismatched success replies become unknown outcomes.

A focused preedit test also reproduced a real Escape routing bug. The pinned
input's Escape unmarks text and propagates; a later dialog action then saw no
marked text and closed the editor on the same key. A more specific
`FormlessSessionEditor > Input` binding lets the existing live composition
guard handle Escape first. One Escape can finish preedit without cancelling;
ordinary Escape still cancels. This fixes the shared metadata editor, with
no dependency or daemon behavior change. It does not establish OS IME acceptance.

## Validation

All Cargo checks use the optional manifest, `--offline --locked`, two jobs,
incremental disabled, and dev/test debug info disabled. Raw evidence remains
under ignored `evidence/gpui-1753-sol/`.

| Check at `af8b8c0` | Result / evidence |
|---|---|
| GUI-free library suite | 180 passed; `instructions-final-model.log` |
| GUI/Thai library + shell suites | 185 + 146 passed; `instructions-final-gui-tests.log` |
| Real private daemon/local fake provider suite | Five passed; `instructions-ack-final-daemon.log` |
| Strict clippy (`-- -D warnings`) | GUI-free lib/unit tests; GUI/Thai lib + shell/render; ax-dump/Thai binary; daemon fixture passed. `instructions-final-clippy-{model,gui,bin}.log`, `instructions-ack-final-daemon-clippy.log`. 26 existing core dependency warnings remain. |
| All-feature docs | Passed, zero doctests; `instructions-ack-final-doc.log` |
| Windows GNU GUI-free all-targets check | Passed; `instructions-final-windows-model.log`; no GUI/link/execution claim |
| Native Mac fonts/Metal offscreen renderer | 51 states / 95 captures passed; `instructions-ack-native-final.log`, PNGs in `instructions-ack-native-final/` |
| Formatting, whitespace and scope | Passed; protocol, manifests, both lockfiles and relay are unchanged |

Exact runner command arrays/exit codes are in `instructions-final-checks.json`.
New pure tests cover trimming/clear semantics, unavailable/oversized values,
other-field availability, missing/malformed/foreign/oversized ACKs and explicit
defaults. The default-ACK failure is retained in
`instructions-default-ack-reproduction.log`.

The three new shell cases exercise a different chosen/open Session, frozen
target IDs, exact multiline Unicode copy, field isolation, edits typed while
waiting, explicit clear, preservation of owned drafts after unrelated ACKs,
oversized whole-edit rejection, live preedit, reconnect, late replies and no
mutation replay. The isolated Escape failure is retained in
`instructions-ime-isolated.log`; the corrected test asserts live marked text
before Escape. Earlier combined cases and their failures remain in the first
and second focused shell logs.

The actual IPC fixture sets, clears and replaces instructions while a provider
reply is deliberately held. It verifies exact typed ACKs and canonical stored
values, `running:true` throughout, and provider request count unchanged at one.
The next Run's actual fake-provider request contains the saved marker. Existing
navigation, recovery, search, question and stream-characterization cases pass;
all private fixtures/Profile leases tear down. This confirms next-Run delivery;
it is not a test of every active-Run next-step transition or a paid provider.

Native captures add multiline Instructions to the two metadata states and
oversized-instruction handoffs at wide and 480×360 / 200% sizes. Input/footer
bounds and disabled Save/Copy are asserted, and selected final minimum-size
captures were visually inspected. Long help/handoff text remains scrollable.
Pinned-layer Thai shaping diagnostics remain. These tests do not validate
foreground keyboard/selection, actual OS IME, VoiceOver speech or human usability.

## Ownership and open gates

The same isolated checkout and `work/sol-gpui-1753-20261115` branch are used.
Main remained `e9bf104fb9c1bf1bff4f56e05efa92b17a1bfc14`, already included.
Original #1776 remained `f8c23e0d5726fdc7d50989109f198301feb45cc9`; its worktree
was inspected read-only. No original branch/worktree/writer or paused session
was changed or controlled. Process inventory remains denied; old writer
terminal activity remains unverified. No alternative inspection route was used.
Separate CI fixture draft #2151 remains with its owner and was not duplicated
or promoted here.

Foreground Computer Use approval `proud-river-57508678` for Formless Sol
Validation remains unresolved; the previous launch/control outcome is uncertain.
No launch/control was repeated, substituted or bypassed, and no OS permissions
changed. Foreground control tools also became unavailable during this increment.
Mac foreground input/selection/real IME/VoiceOver, human 200% and bundle launch
remain open. Windows/Linux native GUI execution is unavailable here.

Streaming stays `deltas:false`; this increment proves no snapshot/event cutoff,
delayed-snapshot, reconnect/overflow or general delta replay safety. The
[ordering report](stream-ordering-20261114.md) still applies. Native security
approvals/Step, further Session/composer/context/resource actions, broader
general-purpose parity, durable preferences, measured performance and distribution
gates remain. Recovery commands are still Session-scoped rather than conditioned
on a displayed Run ID. Parent must review the exact new head and recheck live
main/#1776 ownership before integration. Do not merge or close #1753.
