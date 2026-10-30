# Changelog

All notable changes to Formless are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `CHANGELOG.md`, in Keep a Changelog format, recording the project's history
  from its first release onward.

### Changed

- `.gitignore` now ignores local agent scratch state (`.opencode/`) and
  automation screenshots (`.screenshots/`), so generated files stop showing up
  as untracked changes. `.screenshots` was already excluded from the Docker
  build context.

## [0.1.0] - 2026-10-30

The first release. Formless is a general-purpose agent harness that boots with
a single bootstrap Tool and grows the rest of its capabilities on demand: a
fresh install exposes only `create_tool`, and everything else — web access,
file surgery, shell work, domain logic — is written by the agent, then
persisted, versioned, measured and reused. A long-lived daemon owns the agent
loop and all state; the TUI, CLI and web client are thin, interchangeable
clients of it.

### Added

#### Harness core

- Bootstrap Tool model: `create_tool`, `find_tools`, `read_tool`, `test_tool`
  and `retire_tool` as the built-in surface every install starts from.
- Shared JS modules via `create_module` / `read_module`, loaded inside Tools
  with `requireModule`, versioned immutably and shared across all Tools.
- `run_script` orchestration: short scripts that call only the Tools they list,
  fan out with `Promise.all` and return just the final value, savable as a Tool.
- Further built-ins: `read_artifact` for a Session's own stored outputs,
  `unload_tools` with a per-Session cap applied at compaction, and `ask_user`
  for structured clarifying questions (options with descriptions, multi-select,
  up to four per call).
- User-defined hooks: `pre_tool` (may deny), `post_tool` and `run_end`.
- File and image attachments in user messages.
- A persistent key-value store for Tools, scoped per Tool (`store`) or shared
  (`store:<ns>`).
- `harness` Capability, letting the agent manage its own setup through approved,
  logged, undoable changes.

#### Tools

- Tool metadata and lifecycle: a short human title shown before the name, tags
  with tag filtering in `find_tools`, pinning (always listed, with the token
  cost shown first), on/off switching without retiring, and restore or purge.
- Tool inspection: recent calls with inputs and errors, an example call taken
  from the latest successful Invocation, version diffs, per-version metrics and
  rollback, uses / used-by dependency warnings before retiring or purging,
  per-call model cost, and a reason when a Tool needs attention.
- Dry-run a Tool by hand with no real side effects and the would-be effects
  listed; review a Tool's diff and save gate before saving.
- Import Tools from an HTTPS URL or gist, previewed in full first; undo the
  newest registry change from the TUI, web or CLI.
- `find_tools` BM25 ranking with stemming and synonyms, independent of registry
  size; near-duplicate detection that proposes merges.
- `read_metrics` for registry-wide health (tools, audit, models, runs) plus
  duplicate and MCP-chain reports.

#### Providers, models and routing

- Multiple providers and Roles: a Role maps a purpose to a provider and model,
  used by the primary Session, sub-agents and `llm.complete`; per-role `auto`
  routing replaces a global `provider.model = auto`, with migration.
- Native Anthropic Messages API, OpenAI Chat Completions and Responses API,
  Gemini compatible endpoints (with documented limits), Ollama (including
  pull), Azure and other gateways, and an HTTP proxy option.
- Provider presets in the setup wizard and when adding providers; model
  aliases, provider metadata, model tests and missing-model warnings; fuzzy
  model picker; per-task routing statistics per role and model with a quality
  gate for `auto`.
- Credentials as API keys or OAuth, stored in the OS keychain with key files;
  per-provider OAuth tokens with 401 refresh on every provider.
- Stream and failure handling: SSE decoding per complete line so split UTF-8
  survives, exact and bounded tool-call fragment merging, mid-stream failure
  events instead of empty answers, refusal and content-filter stops,
  `finish_reason: length` handling, stalled-stream detection with re-send,
  dropped-stream retry with a discard-partial-text signal, jittered backoff
  honouring `Retry-After` with a configurable cap, a model circuit breaker, and
  request deadlines.
- KV-cache preservation: a per-Session `prompt_cache_key`, pinned Tool
  definitions and a snapshotted system prompt per Session, with cached-token
  stats in Metrics and a `doctor` probe.
- Real token usage recorded from providers and used for compaction; embedded
  session-naming small model with `formless naming status|install|try`.

#### Context and compaction

- Incremental per-Session context cache, one leading system message with
  harness notes as user turns, compaction appends summary blocks and cuts at
  turn boundaries, and the re-pick of most-used filler Tools at compaction.
- Server-side or client-side compaction, `/compact` on demand, the
  context-window fill in the status line, and fit-to-budget thinking.

#### Sessions

- A working directory per Session, pinning favourites, archiving instead of
  deleting, tags, last activity with counts, state and preview, bulk select,
  undo for deletes, and a `Ctrl+K` switcher.
- Session templates, per-Session standing instructions, rename starting from
  the current name or suggested from recent messages, and a draft per Session.
- Rerun or fork from any earlier user message, files-changed lists with
  `/rewind`, and a "What I did" recap at the end of a Run.
- Export a Session as Markdown or HTML, shareable links that open one in the web
  client, and the list of clients attached to a Session.
- `formless sessions show/rename/rm/restore/import/search`, with `--json`;
  FTS5 search with bm25 ranking and prefix terms.

#### Runs

- Run experience: retry countdown, `/limit`, `/retry --model`, ratings, queue
  view, steer marks, reduced motion, per-Run tokens, cache hits and cost in the
  status line, a footer with time, steps and Tools under each finished Run, and
  a summary line on stderr.
- Interrupted or paused Runs get a banner with reason plus Resume and Abandon;
  queued messages and pending handles survive a daemon restart.
- A live tree of nested Tool calls and sub-agents, a running-Invocations panel
  with cancel and abandon, structured stalled-invocation events with a client
  badge and per-Tool cancel, and sub-agent events with stored transcripts
  (`/agents`).
- Interrupts: `Esc` stops `process.run` children and blocking network calls and
  releases `llm.complete` and `agents.run`; `Ctrl+C` interrupts a headless Run
  and exits 130.

#### TUI

- Rendering: Markdown, syntax highlighting, language/index/optional line
  numbers and no wrapping on code blocks (`/pager code`), coloured diffs,
  inline image thumbnails, Mermaid diagrams (preview, full size in the pager,
  mermaid.live), readable tables in narrow terminals, clickable links and file
  paths (`/open`), timestamps and day separators.
- Chat interaction: scrollbar and scroll position with a "new lines below"
  badge, expand and collapse tool calls, find within the Session, jump between
  user messages, copy any message or Tool call (`/copy` via OSC 52), `/mouse`
  for native selection, queueing from anywhere, pasting images from the
  clipboard and dropping files, and an empty-Session screen with example
  prompts.
- Input: cursor movement, history, bracketed paste and multi-line entry; a
  slash-command palette with autocomplete and a grouped `/help`; `@`-mentions
  for files; the target Session named in the input border.
- Approvals and questions: full code and capability explanations, answers with
  arrow keys, and staying able to type while an approval is open.
- Chrome and navigation: vim-style keys in every list view, a per-view key hint
  bar, mouse clicks on tabs, rows and tool calls, resize and hide side panes,
  configurable colour theme, toasts with `/notes` history, a workspace warning
  when the daemon runs another version (with restart), and a status line showing
  resource and reconnect state.
- Accessibility: a plain line-oriented mode for screen readers, honoured reduced
  motion, and still spinners.

#### Web client and relay

- Opt-in localhost web UI (`formless web`), installable as a PWA with an
  offline shell, returning pending approvals and questions, and a visible
  reconnect countdown with retry.
- Streaming answers rendered block by block without reflow, each message
  rendered once and reused across history updates, per-message actions, `@`
  mentions that complete Tools, and file chips from drops or paste.
- Settings in the browser: providers and Roles, adding MCP servers and Skills,
  a filtered settings list, editable settings through the daemon, and a Harness
  changes panel with undo.
- Tools views: sparkline of recent calls with a trend arrow, grouping by
  capability, author or status, sortable columns, running a Tool by hand, a Tool
  editor with highlighting, bracket matching and live syntax errors, and
  editing a Tool's description and schemas.
- Jobs, Sessions, Proposals (diff and eval before deciding), and Metrics with a
  time range and a tokens/cost chart.
- Themes (Auto/Light/Dark), text size and density controls, keyboard shortcuts
  with a `?` overlay, reduced motion, and a mobile layout.
- Relay: an encrypted Cloudflare Worker relay with a web frontend, frame v2 with
  per-connection session keys and sequence numbers, one daemon connection per
  browser, no traffic while no browser is attached, device pairing by QR code,
  a device list with disconnect, and opt-in Web Push for waiting approvals.

#### MCP

- stdio MCP servers, supervised, with their tools mirrored into the registry;
  Streamable HTTP transport with legacy HTTP+SSE fallback; OAuth 2.1 login,
  token storage and refresh for remote servers.
- Approval-gated `add_mcp_server` / `remove_mcp_server`, server-level and
  annotation-aware tool approval, and untrusted output labels.
- Resources exposed to the agent and prompts as slash commands; tool-list
  changes that reach Sessions only at safe points; error classes for MCP calls
  with warnings about unreliable tools in `find_tools`.
- A catalog, import from other apps, server state and logs, and per-tool on/off.

#### Skills and memory

- `SKILL.md` skills: install, review, enable and remove; `load_skill` and
  `read_skill_file` with a cache-stable skills index; bundled scripts never run
  on the host; export and import skill bundles with Tool-bundle trust rules.
- Auto-create skills from successful Runs, measured and auto-retired.
- User memory: agent-written, user-visible notes on preferences, viewable and
  editable in CLI, TUI and web, with secrets and verbatim Tool output refused,
  and applied memory proposals becoming notes.

#### Jobs

- `schedule_run` for prompts that run later (`at`) or on a cron schedule, with
  approval for recurring jobs, plain-English schedule descriptions, run now,
  history, delivery, overlap control and Jobs views in the TUI and web.

#### Metrics and optimisation

- Run-level outcomes (steps, tokens, edited or corrected follow-ups), built-in
  call measurement without payloads, Cost per Success with step and
  `llm.complete` token attribution, and `read_metrics` reports for tools, audit,
  models and runs.
- Optimiser proposals with scenario evals that judge actions and repeat runs
  behind a significance gate, per-tag fix strategies, denied-only failures
  surfacing as needs-a-capability, a cooldown, and a persisted queue that waits
  only for Tools in use.
- Opt-in spending limits with warnings and a pause, including nested spend and
  sub-agent spend inside budgets.

#### CLI

- `formless` (TUI), `run`, `chat` (line-mode REPL for dumb terminals, SSH and
  screen readers), `daemon start|stop|restart|status`, `web`, `sessions`,
  `profiles`, `tools`, `config`, `roles`, `providers`, `auth`, `mcp`, `skills`,
  `jobs`, `runs`, `memory`, `prompts`, `approvals`, `security`, `logs`,
  `doctor`, `bug-report`, `update`, `relay`, `naming`, `gc`, `uninstall`,
  `completions` and `man`.
- `run` options: `--session`, `--continue` / `--session last`, `--model`,
  `--effort`, `--role`, `--output text|final|json|jsonl-events`, `--quiet`,
  `--timeout`, `--cwd`, `--attach`, `--yes`, and the task or an attachment read
  from stdin.
- `--json` on every list and status command; one coloured, aligned table
  renderer for CLI lists that honours `NO_COLOR`.
- `doctor` lists problems with next steps and `--fix` applies the safe ones;
  `bug-report` writes a redacted report; `update` shows the release notes since
  your version and `--check` only reports.

#### Security and approvals

- Capability model with path-scoped and host-scoped grants, including ports and
  IPv6 hosts; broad grants flagged loudly and confirmed twice; trusted paths and
  trusted hosts that approve silently but stay logged.
- Approval scopes of once, this Session, one hour or always; batch approvals
  listing each held call with its Tool, new capabilities and answer; an AI-written
  plain-language summary and per-path context in prompts.
- A central list of every approval grant with revoke one, several or all, an
  audit log view (`ListAudit`, `/audit`, web Settings), and a security overview
  of everything the daemon can reach.
- Secret files written `0600`, orphaned secrets cleaned up, keys in the OS
  keychain, and a key mask with reveal in the wizard and settings.
- Private-address SSRF blocking across address families, proxy-free pooled
  connection pinning, `process.run` children given a minimal environment plus
  granted keys only, `randomBytes()` from the OS CSPRNG, and an owner-only ACL on
  the Windows named pipe.
- Fail-closed secure runs (`--secure` / `security.secure`), truthful revokes,
  daemon-reported approvals, and an audit log that is easy to find.

#### Storage and data

- SQLite storage at schema v52, with a backup before every migration and
  migrations run in transactions; read-only connection pools for client reads
  and an atomic single-INSERT message append.
- Artifacts with retention caps, `formless gc`, and Session deletion that
  removes its artifacts; daily backups and restore; export-all and import-all.
- Profile-isolated data directories (`--profile`, `formless profiles`).

#### Packaging and installation

- Prebuilt binaries for macOS, Linux and Windows with checksum verification,
  an install script, `Dockerfile` and `ghcr.io/athiwatc/formless` images tested
  before push, `.deb` / `.rpm` packages built on a matching base image and
  install-tested in a clean system, AUR, a Homebrew tap and winget.
- A generated man page and shell completions for bash, zsh, fish, PowerShell and
  elvish; start at login; a safe uninstall with an accurate summary.
- Release CI with independent publish channels and a per-channel result table;
  a Windows CI workflow.

### Changed

- `provider.model = auto` is deprecated in favour of per-role `auto`, with an
  automatic migration.
- Hidden limits moved into documented settings, with help text and defaults for
  every setting taken from the spec, and each setting labelled as applying now
  or at the next TUI start.
- Push models to build Tools instead of refusing, when a capability is missing.

### Fixed

- **Protocol and daemon:** coded protocol errors, bounded frames everywhere,
  request deadlines, plain stream restarts, interrupt ordering and idempotent
  sends; per-connection request workers with a control fast path;
  back-pressure instead of unbounded queues; panic-safe Runs; per-client event
  subscriptions; daemon start that waits, joins a start in progress and explains
  failures; connection errors that say what happened and how to fix it;
  `daemon restart` waiting for Runs (`--force` interrupts); log rotation by
  `daemon.log_max_bytes` / `log_keep` and warnings actually logged; a stale
  binary on Linux detected and reported.
- **Sessions and Runs:** phantom tool calls on the Responses API, with recovery
  for Sessions that stored one; recovery resends outside the retry budget;
  marked refusal placeholders; run state tracked per Session rather than
  globally; headless runs acting only on events from their own Session;
  `run --session` resolution; persisted pending handles reported after a restart.
- **Config and secrets:** atomic config saves that keep comments, safe saves
  from concurrent writers, secret file creation, and uninstall order that does
  not orphan secrets.
- **Tools and runtime:** `process.run` scope and environment, a capped `fs.read`,
  live `emit` files, artifact reconciliation, cancellable `sleep` and real
  `setTimeout`/`setInterval`, and a shared module snapshot instead of a per-call
  copy.
- **Rendering:** a fixed-height thinking block so the layout below never jumps,
  wide Mermaid diagrams, and code blocks that do not wrap.
- **Packaging:** a Docker image that actually works and is tested before push,
  real package dependencies and license, a strict renderer, and independent
  publish channels so one failure does not block the rest.

### Performance

- Incremental per-Session context building, a shared cached module snapshot, one
  shared HTTP client per daemon and one shared fetch client with scope-checked
  manual redirects, a config cache reloaded only on change, pooled read-only
  storage connections, denormalised Session summaries, FTS5 search, incremental
  streaming render in the TUI with messages cached by id, single-render message
  reuse in the web client, and a shared table renderer.

### Documentation

- `README` front page, contents list and copy-paste commands; `SPEC` and
  `GLOSSARY`; architecture decision records in `docs/adr`; user guides for
  capabilities, keys, Docker and troubleshooting; a gallery, `--help` text and
  setting descriptions; `/docs` in the CLI; `doctor --json` and `bug-report`
  documented.

[Unreleased]: https://github.com/athiwatc/formless/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/athiwatc/formless/releases/tag/v0.1.0
