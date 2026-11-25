# Formless CLI

## CLI reference

With no command, `formless` launches the TUI and auto-starts its daemon.
Root commands appear alphabetically in `--help` and the generated man page;
Clap appends its generated `help` command at the end of root help. The table
covers every visible root command, including `help`. Use `formless COMMAND --help` for arguments and defaults;
`formless help COMMAND SUBCOMMAND` also opens nested help.

| Command | Actions / useful options | Purpose |
|---|---|---|
| `formless approvals` | `pending`, `approve`, `deny`, `list`, `revoke`, `denials` | Inspect pending requests, answer one, or inspect and manage existing grants/denials. |
| `formless artifacts` | `show ID [--offset CHARS] [--length CHARS] [--json]` | Read decoded artifact text in Unicode scalar slices; not a byte-exact download. |
| `formless auth` | `login`, `status`, `logout` | Manage provider credentials (API key or OAuth). |
| `formless backup` | No action: create; `list`, `verify`, `restore`, `drill` | Create, inspect, verify, restore, or rehearse database snapshots. |
| `formless bug-report` | `-o FILE` / `--output FILE` | Write a redacted Markdown diagnostic report. |
| `formless chat` | `--session ID` or `--new` | Line-mode chat with streamed answers and inline questions. |
| `formless commands` | `list` | List local user-defined slash-command metadata without starting a daemon. |
| `formless completions` | `bash`, `zsh`, `fish`, `powershell`, `elvish` | Print a shell completion script. |
| `formless config` | `path`, `get`, `set`, `edit` | Locate, inspect, validate, or edit configuration. |
| `formless daemon` | `start`, `stop`, `restart`, `status`, `serve`, `install-service`, `uninstall-service` | Manage the daemon and its optional login service. |
| `formless docs` | Optional topic; `--text`, `--print`, `--keys` | Read bundled Markdown, open the current online guide (gallery by default), print its URL, or print the keyboard reference. |
| `formless doctor` | `--json`, `--fix`, `--restore-drill` | Diagnose setup, apply safe fixes, or rehearse the newest backup. |
| `formless export-all` | `--sessions`, `-o FILE` / `--output FILE`, `--overwrite` | Write a portable setup bundle, optionally including Session transcripts. To replace a file, pass `--overwrite` with `--output`. |
| `formless gc` | `--dry-run`, `--json` | Free reclaimable storage or preview what would be freed. |
| `formless help` | Optional command path, such as `sessions show` | Print help for the CLI or a nested command. |
| `formless import-all` | `FILE` | Add a portable setup bundle without overwriting existing entries. |
| `formless jobs` | `list`, `add`, `run`, `history`, `enable`, `disable`, `rm` | Manage scheduled prompts and inspect their history. |
| `formless logs` | `--follow`, `--level`, `--since`, `--lines`, `--jsonl` | Read or follow the daemon log and rotated files. |
| `formless mcp` | `add`, `remove`, `list`, `test`, `login`, `logout`, `catalog`, `import`, `log`, `tools`, `tool`, `prompts`, `prompt` | Manage external MCP servers and inspect their tools/logs. |
| `formless metrics` | `--since DURATION`, `--json` | Read recorded model, Tool and built-in statistics through independent reports at one cutoff. |
| `formless memory` | `list`, `add`, `rm`, `edit`, `used` | Inspect or change saved user-memory notes. |
| `formless models` | `list [--json]` | Fetch the default-provider model catalog and existing picker report; a non-null listing error prints a partial report and exits 1. |
| `formless modules` | `list`, `show`; `show --version N` | Read all modules, inspect metadata/history/dependents, or emit an exact stored source version. |
| `formless naming` | `status`, `pull`, `test` | Inspect, install, or preview the embedded Session-naming model. |
| `formless prices` | `import`, `list` | Import a model-price catalog or inspect prices and configured model references offline. |
| `formless profiles` | `list`, `rm` | List or remove isolated profiles. |
| `formless prompts` | `list`, `diff`, `generate`, `apply`, `reject`, `rollback` | Inspect existing proposal comparisons and manage measured prompt/memory proposals. |
| `formless providers` | `list`, `add`, `remove`, `test` | Manage named model providers and test a connection. |
| `formless relay` | `enable`, `disable`, `status` | Manage encrypted browser access through the relay. |
| `formless reset` | `--force` | Wipe configuration, credentials, and the current profile's data. |
| `formless restore` | `FILE` | Replace the database from a backup, preserving the current database. |
| `formless roles` | `list`, `add`, `rm` | Map named purposes to a provider and model. |
| `formless run` | `TASK`; `--estimate`, `--session`, `--output`, `--attach` and other Run options | Run one headless task, or print its local forecast without starting it. |
| `formless routing` | `--json` | Read currently available daemon routing decisions and returned candidate figures without starting a model request. |
| `formless runs` | No action: list; `show`, `cancel`, `running` | List recent Runs, inspect/cancel an exact Run, or read a live Invocation snapshot. |
| `formless security` | `status` | Inspect the daemon's grants, denials, trusted resources, and exposed interfaces. |
| `formless sessions` | `subagents`, `subagent`, `fork`, `compare`, `list`, `show`, `usage`, `files`, `rewind`, `rename`, `rm`, `restore`, `import`, `search`, `templates list/show`, `archive`, `unarchive`, `pin`, `unpin`, `tag`, `export` | Manage stored Sessions, follow a Run with `show --follow`, or inspect tracked files. `list --pinned --tag TAG` filters the fetched reply; selectors combine with `--archived`, preserving reply order. |
| `formless skills` | `reviews`, `add`, `remove`, `list`, `enable`, `disable`, `show`, `export`, `import` | Inspect and manage Skills and their retained description reviews. |
| `formless storage` | `--json` | Report disk usage by data category. |
| `formless tools` | `reviews`, `list`, `disable`, `enable`, `undo`, `pin`, `unpin`, `tag`, `show`, `diff`, `retire`, `restore`, `rollback`, `purge`, `run`, `export`, `import` | Inspect, manage, run, or transfer registry Tools. |
| `formless tui` | `--plain`, `-c` / `--continue`, `--new` | Launch the full-screen terminal client; also the default with no command. |
| `formless uninstall` | `--keep-data`, `--keep-binary`, `--yes` (alias `--force`) | Remove Formless, optionally retaining data or the binary. |
| `formless update` | No action, `check`, or `--check`; `--json` | Show newer release notes and the applicable upgrade command; never rewrite the binary. |
| `formless usage` | `--by GROUP`, `--since DURATION`, `--json` | Read full-range grouped tokens and currently computed cost estimates. |
| `formless web` | `--port`, `--no-open`, `--stop` | Open, print a link for, or stop the local browser UI. |

Human `runs show` uses labeled stored identifiers, status/error, timestamps,
message boundaries, counters and recorded token sums. Available spend is
labeled as a report of estimates, with known subtotal, optional total and
returned completeness flags; unavailable amounts are not invented as zero.
Human `runs cancel` reports a cancellation request for a matching accepting
controller, or that none was found; neither acknowledgment proves a terminal
outcome. There is no polling or additional cancellation request. JSON and the
existing status-based show exits remain unchanged.

`mcp test NAME [--json]` restarts the server and prints the full report before
returning its verdict: exit 0 for a successful reply (including zero tools),
exit 1 for an embedded restart error. JSON preserves the reply as one value
without an extra generic error line for that reported failure. Request failures
keep their existing error mapping; unexpected replies are errors. `--json` does
not make the restart read-only. `mcp add` retains its existing behavior.

`gc --json` prints the complete single daemon/offline result, including its
`dry_run`, integer `total_bytes`, `kinds` and any extra details. Use
`gc --dry-run --json` to preview without cleanup. Counts are core-reported values,
not a promise of exact physical space reclaimed. JSON changes only presentation;
retention, side effects, errors and human output remain unchanged.

`sessions pin ID` and `sessions unpin ID` display the stored pin state returned
by the daemon. `sessions tag ID` only reads tags; adding positional tags replaces
the **entire** stored tag list, and `--clear` explicitly replaces it with an empty
list (conflicting with positional tags). Tags reach storage unchanged for its
existing normalization; confirmation shows returned stored values. These three
actions accept `--json` for the selected/returned Session object.

`tools tag NAME` reads the Tool's stored tags. Pass positional tags to replace
the entire list, or `--clear` to remove all tags.

Ambiguous Session prefixes/names remain usage errors and display up to ten
matching full IDs, bounded names and archive labels. Extra matches are counted,
with commands for both archive states. Prefix errors suggest extending the ID;
name errors suggest a displayed ID. Exact-ID priority, prefix-before-name
resolution and trimmed ASCII-case-insensitive full-name matching are unchanged.

Human Session lists label the last recorded Run status and show stored tags,
retaining the pin star and existing message preview/count. Empty replies are
reported separately from an empty archive selection, which names the alternate
list command and count. Static human `sessions show` includes stored archived,
pinned, tags and last recorded Run status metadata; absent status is "not
recorded". JSON and `--follow` retain their existing contracts.

`tools list --filter FILTER` selects one stored-state predicate: `not-retired`
(including disabled Tools), `retired`, `failing` (known success rate below 0.8),
`needs-attention`, `off` (disabled) or `pinned`. It combines by AND with
`--tag TAG`, retaining exact stored-tag matching after the existing argument
lowercasing/leading-# removal. Unknown rates do not count as failing; states
may overlap. JSON uses the same selection and existing ToolInfo schema.
Human output distinguishes an empty registry from no matching rows and names
the flags to clear. `--show-pinned` adds a human PINNED yes/no column; default
piped cells and row order remain unchanged. These flags never mutate a Tool.

`formless metrics --since 7d --json` reads the three existing metrics reports
with one checked UTC cutoff. Omit `--since` for all history. Tool rows use stored
`tool_id` values; reports retain their own row order and are independent reads,
not a transactional snapshot. Runs and daily usage remain separate surfaces.

`formless sessions list --pinned --tag WORK --json` selects active pinned Sessions
whose stored tags contain exactly `work`. Only the tag argument is lowercased;
a leading `#` is not stripped. Add `--archived` to select the archived partition.
These selectors filter the existing reply on the client.

Tool selectors use an exact name or full ID. After a miss, up to three
ranked partial-name suggestions can help you retry with the original exact
spelling; these use folded subsequence matching, not edit-distance spelling
correction. A suggestion never selects or runs a Tool automatically.

`formless memory used (--session SESSION | --run RUN_ID) [--json]` reports
current note fields associated with the selected Run, in daemon order.
Session selectors use normal exact-id/prefix/name resolution and select its
latest Run, including one still running; Run ids are sent unchanged without
existence validation. Retained associations may outlive a Run, while backend
lookup/association failures can produce empty or partial reports. These are
reported current notes, not verified historical prompt contents. Disabled
notes remain present; JSON is the full returned array. Other daemon errors
retain ordinary exit behavior without a partial report.

```sh
formless memory used --session SESSION --json
formless memory used --run exact-run-id
```

`formless sessions subagents SESSION [--json]` inventories stored sub-agents
using normal Session exact-id/prefix/name resolution. Full sub-agent ids can
be passed unchanged to `formless sessions subagent SUBAGENT_ID [--json]`,
which uses exact ids with no prefix lookup. Inventory order and null versus
empty results are retained. Human detail labels the record and pretty-prints
the entire returned provider-shaped messages array; JSON preserves the full
daemon report. This is the existing unpaged transcript projection, whose
handler skips malformed stored JSON rows, with no extra integrity check,
Run start or Session mutation. Request failures produce no partial report.

```sh
formless sessions subagents SESSION --json
formless sessions subagent exact-subagent-id
```

`update --json`, `update check --json`, and `update --check --json` emit one
object: `{installed, status, newer_releases: [{tag, url, notes}]}`. Notes retain
the existing trimmed body and oldest-first order; `installed` has no `v` prefix.
The lookup follows GitHub release pages within one 15-second timeout and
excludes drafts and prereleases. `update_available` means at least one newer
stable release was found; `up_to_date` means a completed lookup found none.
`release_check_unavailable` maps to an actual HTTP 404 response,
not verified absence of releases. Error text alone never selects this status.
Completed checks exit 0; other fetch or JSON parse failures remain errors with
no success JSON. The human output format stays unchanged, and the JSON form
never runs an updater.

`runs --session SESSION --since DURATION [--limit N] [--json]` sends the
existing Session/time filters to RunMetrics before its newest-N limit. Each
selector is optional; Session lookup keeps the existing exact-ID, unambiguous
prefix and name rules. Durations use checked arithmetic and one UTC RFC3339
cutoff in the existing stored TEXT representation. The returned runs and their
summary describe that filtered, limited subset; JSON forecasts remain
independently scoped and unchanged. With neither selector, request/output
behavior is unchanged. Filters with any Runs subaction are usage errors before
connection; unknown/ambiguous Sessions necessarily fail after ListSessions.

`sessions usage SESSION [--json]` uses the existing Session selector and
buffers SessionUsage plus ContextUsage before output. Human records label
total/latest-Run counters, estimated USD cost, unpriced models and the current
prompt/context estimates. Attachment entries count stored metadata-array
entries, not tokens, unique files or pending attachments. A null latest report
means usage unavailable, not no prior Runs or a completed-Run guarantee.
JSON preserves both complete independent replies under `session_id`, `usage`
and `context`; this is not a common-snapshot guarantee. Existing context-window
discovery may contact the configured provider, so the command is not promised
offline. Reported Run count retains the backend's newest-100,000 global-window
limitation (#1458); aggregation/estimation/accounting behavior is unchanged.

`tools show NAME_OR_ID` includes stored pin state, the newest ten version
summaries (explicit numeric order) and up to five of the returned recent-call
metadata rows. Omitted counts point to `--json`, which retains the existing
complete detail contract. No new call inputs/outputs are added to these
summaries; the existing schema/example sections remain. `--code` conflicts
with JSON and writes only the exact current version source, preserving its
trailing newline state; a missing current version is an error. This intentional
human-detail layout change leaves fetching, metrics and security fields intact.
The existing GetTool all-version payload remains until protocol selectors
are addressed separately (#889).

#### Root and global flags

| Flag | Short | Availability | Meaning |
|---|---|---|---|
| `--profile` | — | Global | Select isolated profile `NAME`; separate daemon and data directory. |
| `--secure` | — | Global | Ask for approval before sensitive Tool calls; also `security.secure = true`. |
| `--plain` | — | Root/TUI startup | Use line-oriented, screen-reader-friendly output without redraws, spinners, or borders. |
| `--continue` | `-c` | Root/TUI startup | Open the most recently active Session; conflicts with `--new`. |
| `--new` | — | Root/TUI startup | Start a fresh Session; conflicts with `--continue`. |
| `--help` | `-h` | Root; also available on subcommands | Print help for the selected command. |
| `--version` | `-V` | Root | Print the executable version. |

Global flags can appear before or after a subcommand. `--plain`,
`--continue`/`-c`, and `--new` are root-only options for TUI startup: put them
before `tui`, or omit the command. They are not inherited by subcommands.
The separate `run --continue`/`run -c`
continues the Session used by the previous headless Run.

Approvals are opt-in; with them on, headless approvals **fail closed**: an
unapproved sensitive Invocation fails with a clear error unless `run --yes`
is passed.

For example, these print help without running a task:

```sh
formless --help
formless sessions --help
formless runs show --help
formless help sessions show
```

## Further reading

See [Using Formless](usage.md) for setup and usage guidance.
