# Colour-independent terminal meaning

Issue #635 audit covers the TUI and CLI on the accepted #632 source. The default
is still `auto`. Select `tui.theme = "colorblind"` in Settings or with
`formless config set tui.theme colorblind`.

The single public `SemanticTone`, `colorblind_rgb` and `tone_marker` mapping is
implemented in TUI `theme.rs` and consumed by CLI `table.rs`:

| Meaning | Marker | Okabe–Ito-derived colour |
| --- | --- | --- |
| Good / successful | `ok` | Sky blue `#56B4E9` |
| Warning / caution | `warn` | Yellow `#F0E442` |
| Bad / failed | `fail` | Orange `#E69F00` |

The built-in colorblind text palette is designed for the existing dark theme
background. Its text colours are included in the existing 4.5:1 contrast check.
Explicit user RGB/indexed role overrides remain deliberate overrides; the
existing contrast diagnostic applies to those as before.

## TUI inventory

Every production named Red/Green/LightRed/LightGreen occurrence was inspected.
Unit expectations that name the unthemed ANSI palette are not status producers.

| File / rendering family | Meaning and redundant representation |
| --- | --- |
| `theme.rs`; root `fg`, `bold_fg`, `role_style` | Named green/red variants map to blue/orange. Role labels, user/assistant/tool names and explicit overrides remain text. Selection is not labelled as successful work. |
| `highlight.rs::diff_line`, Markdown fenced diffs | Added/removed source bytes retain bold `+`/`-`, including NO_COLOR. File and hunk headers stay distinct. Syntax string highlighting also remaps, but source strings are not status cells. |
| `hunk_view.rs::diff_line` | Signed bold gutter separate from line numbers; changed-word bold/underline preserved. Previous direct `.fg(ink)` bypass now uses theme mapping. Old/new numbers, CRLF/LF/EOF labels and unmodified words remain intact. |
| Proposal diff pager, Tool-version diff, editor review | Existing signed source lines become bold and use the same blue/orange mapping. Verdict text gets the status word when redundant labels are enabled. |
| Collapsed Tool diff summary | Both numeric `+N` and `−N` remain, bold in every palette. |
| Persisted and live Tool calls, recent invocation table | Existing `✓`/`✗` gain `ok`/`fail`. Running calls keep explicit running text. Errors keep their diagnostics. |
| Tool glyph/status columns | Healthy, attention and disabled retain their separate glyphs and gain `ok`, `fail`, `warn`; retired remains an explicitly named neutral state. |
| Success-rate cells: Tools, metrics, routing, built-ins, rollback, side pane, Tool detail | Percentage retains its number and threshold marker. >=90% is `ok`, >=70% `warn`, lower `fail`; absent rate stays `-`. Columns expand for markers. |
| Metrics token-usage bars | Cached/fresh/output have distinct ░/▒/█ patterns and a matching legend. Colorblind and NO_COLOR rows also retain separate component counts on their own line, with panel height accounting for that line, plus total and known cost; fresh/output remain distinguishable even when their hue is the same. |
| Tool history/trend | All last-20 tall/short glyphs remain; header explains `▆ok ▂fail`. Up/down arrows remain directional. No glyph is replaced by colour alone. |
| Tool metrics counts, optimiser history and proposal scores/status | Known outcome counts and candidate/baseline values remain attached to their status words. Numeric cells widen for added labels. Unmeasured/proposed values remain neutral. |
| Approval broad-grant body and path notes | Broad grants are explicitly labelled `warn ... broad access`; paths retain the exact scope and warning explanation. No grant policy or confirmation changes. |
| Approval, promotion, question, quit and destructive-action buttons | These are actions rather than observed statuses. `allow/once/always/deny/stop/cancel` and shortcut keys remain visible; borders, brackets and reverse/bold selection remain. Named hues remap. |
| Status bar, context pressure, pending approvals, errors/toasts and settings errors | Running/ready/reconnecting and approval counts remain words; context pressure adds `warn` at 70% and `fail` at 90%; errors add `fail` without removing diagnostics. |
| Settings, palette, switcher, schemas and editor active fields | Existing caret, brackets, underline/reverse and field labels express selection. Their green accents remap, without claiming a successful result. |
| Markdown tasks and callouts | Checked/unchecked boxes remain different shapes; `Tip`, `Caution`, etc. remain words. Named colours remap. |
| Mermaid graphs and pie/bar diagrams | Series labels, values and bar lengths remain. Named colours remap; these category colours do not assert good/bad outcomes. |
| `links.rs` terminal colour conversion | Converts the already-rendered palette (including RGB); creates no independent red/green status mapping. |
| Chat search highlight | NO_COLOR uses reverse/bold for current match and underline for other matches. Text and match counts are unchanged. |
| Relay pairing | Fixed white/black QR is emitted only with colour enabled; exact pairing URL remains available in either mode. |

## CLI inventory

`table.rs` is the sole semantic ANSI 32/33/31 producer. Its Tone mapping now
consumes the same RGB constants and inserts markers whenever colour is off or
the colorblind theme is configured. All 27 main-command Tone callsites were
reviewed: approval grants/denials, profile running state, proposals, Skill state,
provider credential presence, job approval/last status, Run history, MCP state
and errors, hooks, triggers, Tool active/off/attention and success rates.
Always/broad grants are caution (`Warn`), not an unqualified good result.

JSON remains data with unchanged meanings and no terminal styling. The command
catalog's read-only early path does not acquire a new config/provider dependency.
Stderr logging honours NO_COLOR. QR ANSI is omitted without colour while its
printed URL remains. Attachment, command-expansion and pipeline outputs already
contain text/data rather than semantic ANSI colours.

## Validation boundaries

Focused unit/render tests cover canonical RGB and text markers, theme contrast,
bold signed diffs/hunks, Tool success/failure, metrics, broad approval warnings,
Settings choice and NO_COLOR data retention. Tests select themes through a
thread-local scoped guard; they do not race by changing process-wide NO_COLOR.
CLI integration tests exercise a real PTY and private config/profile fixtures.
No provider calls, real credentials, user settings changes or native-platform
validation are implied by source review.
