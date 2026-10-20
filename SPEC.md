# Formless — Full Application Specification (v1)

**Status:** Accepted (grilling session Q1–Q67). **Audience:** implementers of v1.
**Companion docs:** [GLOSSARY.md](/Users/athiwat/RustroverProjects/formless/GLOSSARY.md) (canonical language — spec uses those terms exactly), ADRs in `docs/adr/`.

Formless is a general-purpose agent harness that boots with a single Bootstrap
Tool (`create_tool`) and builds every other Tool it needs, for any use case —
it does not specialise in coding. A Daemon Core owns the agent loop, Tools,
Sessions, and state; a TUI (v1) and a headless CLI are clients of that core.
The harness self-optimises its Tools, driven only by measured Tool Success
Rate. A hosted Web frontend via an encrypted Relay is a v2 direction (see
Non-Goals), and the Daemon Core is designed so that future frontends attach
without redesign.

## 1. Principles

1. **One tool at boot.** A fresh install exposes exactly `create_tool`.
   Everything else is built, persisted, and reused. No starter Tools ship.
2. **Measured, never vibes.** Self-Optimisation is triggered and judged only
   by recorded Invocation outcomes. No silent or untested mutation, ever.
3. **No time-based killing.** Runs and Invocations have no step/token/time
   limits. The only terminators are completion, fault, or the user.
4. **Deny by default.** Tools run sandboxed with only their declared
   Capabilities. The runtime offers nothing a Tool was not granted.
5. **Local-first, zero telemetry.** Nothing leaves the machine except calls
   to the user-configured provider endpoint. No update checker, no phone-home.
6. **One core, many clients.** All behaviour lives in the Daemon Core. TUI,
   CLI, and future frontends are interchangeable clients of one protocol.
7. **Everything is versioned and reversible.** Tools, promotions, config, and
   Session checkpoints keep history; destructive actions are user-only and
   confirmed.

## 2. Architecture

### 2.1 Workspace and binary

- Cargo workspace, Apache-2.0, producing **one binary**: `formless`.
- Crates:
  - `formless-core` — Daemon Core: agent loop, provider client, Session/Run
    management, Tool registry, metrics, optimiser, storage, compaction.
  - `formless-runtime` — QuickJS sandbox (QuickJS-NG via `rquickjs`) and the
    Capability-gated host API, including streaming primitives.
  - `formless-protocol` — shared daemon↔client types (requests, streamed
    events, errors), versioned (see §13).
  - `formless-tui` — ratatui TUI client.
  - CLI commands live in the binary entrypoint (no separate crates/binaries).
- The daemon is not a separate executable: it is `formless` running in daemon
  mode, spawned by clients (see §9). No system service on any OS.

### 2.2 Processes and clients

- One Daemon Core per data dir (single instance, enforced by lock; §9.4).
  Optional `--profile <name>` runs an additional isolated daemon + data dir.
- Clients: TUI (v1 UI), CLI (§12), and any future frontend. Clients are thin:
  they render streamed events and send intents; they hold no authoritative
  state and can disconnect/reattach without losing work (daemon persists all
  state; killing a TUI never kills the daemon or a Run).

### 2.3 Transport

- macOS/Linux: Unix domain socket in the data dir (mode `0700` dir, socket
  accessible only to the owning user).
- Windows: named pipe with an ACL restricted to the owning user.
- Identical framed protocol over both transports. No localhost-TCP fallback
  in v1. Local access control = OS filesystem/pipe permissions only (single-
  user trust boundary; see Security, §14).

## 3. Concepts (summary; GLOSSARY.md is canonical)

| Term | Meaning in one line |
|---|---|
| Bootstrap Tool | The one tool at first boot: `create_tool`. Immutable, never optimised. |
| Tool | Persisted, versioned, sandboxed JS capability built via `create_tool`. |
| Session | Resumable conversation + Run history; many run concurrently. |
| Run | One unit of work in a Session (user message → final answer). Unlimited. |
| Invocation | One execution of one Tool version with one input. Recorded + measured. |
| Pending Invocation | Invocation past the soft threshold (300s); still running, handle held by agent. |
| Steer | Message during a Run, injected at the next step boundary (Enter default). |
| Capability | Declared, scopable permission a Tool needs. Ceiling, never exceeded. |
| Candidate | Optimiser-proposed Tool version; must win sandboxed replay to be promoted. |
| Promotion | Replacing the current Tool version with a winning Candidate; rollback kept. |
| Retired Tool | Agent-removed Tool: uncallable, retained, restorable; user-only purge. |
| Compaction | Persisted summary checkpoint replacing old history/tool output. |
| Daemon Core | Long-lived process owning loop, Tools, Sessions, state. |
| Relay | Future encrypted intermediary for a hosted frontend. **Not v1.** |

## 4. Provider (v1: OpenAI-compatible only)

### 4.1 Scope

- v1 supports exactly one provider type: any **OpenAI-compatible** endpoint
  speaking **Chat Completions**: `POST {base_url}/chat/completions` with
  Bearer auth and SSE streaming, and `GET {base_url}/models` for discovery.
- `base_url` is used **verbatim** (the daemon appends only `/chat/completions`
  and `/models`); non-`/v1` layouts work. Free-form extra headers (e.g.
  organisation/project) are configurable per provider and sent on every call.
- Future (v2 log): every provider type, including subscription/OAuth-based
  providers, and Responses-API-style endpoints. Provider quirks are expressed
  as config, never provider-specific code paths in v1.

### 4.2 Configuration and selection

- Onboarding collects `base_url` + API key, then fetches `/models`; the user
  picks a **model** and a **reasoning effort**. If `/models` fails, manual
  model-name entry is offered. Model + effort are a daemon default,
  overridable per Session, changeable mid-Session, effective on the **next
  Run** (never mid-Run).
- `reasoning_effort` is sent verbatim when set. If the endpoint rejects an
  unknown parameter (HTTP 400), the daemon retries **once** without the
  offending parameter and posts a visible note in the Session (never silent).
- Per-model `context_tokens` (configurable, sensible default) drives
  Compaction, because context size is not discoverable from the API.
- Sampling params (`temperature`, `max_tokens`) are **not** exposed in v1;
  provider defaults rule.
- API key storage: OS keychain (macOS Keychain, Windows Credential Manager,
  Linux Secret Service) with a `0600`-permission file fallback. Keys are
  never written to `config.toml` in plaintext, never logged, never sent
  anywhere except the configured endpoint.

### 4.3 Endpoint probe (onboarding gate)

Before the first Session, the wizard probes the endpoint: connectivity,
streaming, and a **test tool-call**. If tool-calling is unsupported, session
start is refused with a clear explanation. Native tool-calling is required
in v1; a prompt-based (text/JSON) fallback is explicitly out (§17).
Thinking-mode endpoints that reject a forced `tool_choice` are re-probed
with `tool_choice` omitted; a voluntary test tool-call still passes.

### 4.4 Provider failure handling

- 429 / 5xx / dropped stream: automatic retry with exponential backoff,
  bounded (default 5 attempts). Exhaustion ⇒ the Run **pauses** and the error
  is surfaced in the TUI; a Run never silently fails.
- Hung request: after 300s the agent is told the request is still pending
  (same philosophy as Pending Invocations, §6.4); nothing is killed.
- Malformed tool-calls from the model are fed back to the model as a
  correction prompt and counted in a **separate** malformed-call counter —
  they never pollute Tool Success Rate.

## 5. Agent loop, Sessions, and Runs

### 5.1 Session

- A Session is created, renamed, resumed, deleted, exported, and imported
  from the TUI/CLI. It holds the full transcript, Run history, per-Session
  model/effort overrides, and compaction checkpoints (all in SQLite).
- Many Sessions run concurrently against one daemon, including two clients
  attached to the *same* Session (§10.4). Sessions never see each other's
  transcripts; Tools and metrics are shared (§7, §8).
- Out of v1: Session fork, cross-Session search, edit-and-retry of past
  messages (§17).

### 5.2 Run lifecycle

- Each user message in a Session starts a **Run**: the agent loops
  (think → tool call(s) → observe) autonomously until it produces a final
  answer, is interrupted, or pauses on provider failure/limit-free errors.
- **No limits:** there is no max-step, token-budget, or wall-clock cap on a
  Run. Config contains no such settings. The user is the limit.
- Completed Runs are appended to Session history. A Run interrupted by user
  action, provider exhaustion, or daemon crash is marked accordingly and is
  resumable per §5.4/§9.5.
- Multiple tool calls emitted in one model turn execute **in parallel**, each
  in its own isolated sandbox instance; results are returned together.
  Approval gates (§10.3) are evaluated per Tool before the batch starts:
  approved calls run, held calls wait on one combined prompt.

### 5.3 Talking to a busy agent

- **Enter during a Run = Steer** (the default): the message is injected at
  the next step boundary without killing in-flight work. **Ctrl+Enter =
  queue** for after the Run ends (binding configurable). When idle, Enter
  simply starts a new Run.
- `Esc` interrupts the current Run (in-flight Invocations are cancelled and
  recorded as user-cancelled, §8.2). `Ctrl+C` detaches the TUI client only.

### 5.4 Interruption and resumption

- Interrupting never corrupts state: last completed tool call, transcript,
  and checkpoint are persisted before the Run is marked interrupted.
- Resuming a Session (same or later daemon lifetime) restores transcript,
  pending handles (§6.4), and compaction checkpoints exactly.

## 6. Invocations: async by design

### 6.1 Execution model

- Every Invocation runs on its own sandbox thread with isolated memory
  (per-Tool `memory_mb`, §7.5). One hung or spinning Tool never blocks the
  daemon or other Sessions.
- The daemon tracks elapsed time and a **last-activity heartbeat** (any host
  call/output counts as activity). After a configurable silence period
  (default 5 min) an Invocation is visually flagged `possibly stalled` in
  the TUI. The flag is advisory only — **nothing is ever auto-killed**.

### 6.2 Soft threshold → Pending Invocation

- At the soft threshold (default **300s, configurable**), an unfinished
  Invocation is **not** terminated. The agent is told it is still running and
  receives a **Pending Invocation handle** instead of a result.

### 6.3 Accessing a Pending Invocation

Two mechanisms, both harness plumbing (not registry Tools, so the one-Tool
purity is untouched):

1. **Poll/await:** the agent may check the handle's status or await it at any
   point in its loop. Awaiting is how a Run waits, unbounded.
2. **Push delivery:** if the Invocation completes while the agent takes other
   steps, its result is delivered at the next step boundary.

- If the agent attempts a final answer while handles it created are still
  pending, the daemon reminds it **once**. The agent may then await them or
  explicitly **abandon** them. Abandoned ≠ cancelled: the Invocation still
  finishes and is recorded; abandonment is noted on the record.

### 6.4 Outcomes

An Invocation ends only by: completion (success/failure by §8), sandbox fault
(including memory-cap fault), or user cancellation. User-cancelled
Invocations are tagged distinctly so the optimiser does not “fix” a Tool the
user merely interrupted. Provider-request stalls follow the same pending
pattern (§4.4).

## 7. The Tool system

### 7.1 `create_tool` — the Bootstrap Tool

- Signature (conceptual): `create_tool(name, description, input_schema,
  output_schema, implementation_js, capabilities[], memory_mb?)`.
- The **agent writes the JavaScript** (plain JS; QuickJS has no TypeScript).
  Every session exposes `create_tool` **plus every active Tool** directly as
  callable functions (name + description + input schema). There is no
  catalog-search step.
- **Validation gate at creation:** name rules (§7.3), both schemas validated
  (JSON Schema draft 2020-12), JS syntax-checked, and smoke-tested in the
  replay-style sandbox (§8.4) with empty input. Any failure ⇒ creation is
  rejected with the error; nothing broken is ever saved.
- `create_tool` is **immutable, excluded from Self-Optimisation**, and cannot
  be retired, edited, or widened by the agent or the optimiser.

### 7.2 Tool lifecycle

- Tools are never edited in place. Any change (agent-driven or optimiser
  Candidate) creates a **new integer version** (`v1`, `v2`, …; no semver).
  Each version records timestamp, author (`agent` | `optimiser` | `import`),
  and, at promotion, the stats of the version it replaced.
- A running Invocation always finishes on the version it started with; new
  Invocations use the current version.
- **Retire:** the agent may retire a Tool (must state a reason in the
  Session). Retirement is instant and global (uncallable everywhere), data
  retained, restorable from the Tool browser. **Purge** (permanent deletion
  of code, history, and metric tallies) is user-only, from the TUI.
- Name collisions at creation are serialised by the daemon: a second creator
  gets the existing Tool and creates a new version instead of a duplicate.

### 7.3 Identity rules

- Names: `snake_case`, 1–64 chars, case-insensitively unique among **active**
  Tools. A retired Tool's name is reusable only by restoring or purging it.
- Input/output contracts: JSON Schema draft 2020-12. Input-validation failure
  returns the schema error to the agent as a failed Invocation (metrics
  category 4, §8.1) — never a daemon crash.

### 7.4 Capabilities (closed taxonomy)

| Capability | Scoping | Notes |
|---|---|---|
| `net` | optionally per-domain | Unscoped `net` allowed but flagged “broad” at approval |
| `fs:read` | path list | Canonicalised; symlink escapes denied (§14) |
| `fs:write` | path list | Same containment rules |
| `env` | named keys | Only listed keys visible |
| `process` | named commands | Executed without a shell; unscoped flagged “broad” |

- `memory_mb` (default 256) is declared and governed like a Capability:
  shown at approval, widening re-prompts, bounded by a daemon max
  (default 4 GB).
- No clipboard/notification capabilities in v1. A Capability the runtime
  cannot enforce is never offered.

### 7.5 QuickJS runtime and host API

- Engine: QuickJS-NG via `rquickjs`, embedded in-process. Tools are
  single-file plain JS; the daemon provides the entire API surface (bare JS
  has no ambient authority of any kind):
  - `fetch(url, opts)` — gated by `net`; streamed response body supported.
  - `fs.read/write` (whole-value, small data) and `fs.readChunks(path,size)` /
    line iterator (streaming) — gated by `fs:*`.
  - `env.get(key)` — gated by `env`. `process.run(cmd, args)` — gated by
    `process`, no shell.
  - `tools.call(name, input)` — composition (§7.6).
  - `console`, timers, `JSON`, crypto-random. Nothing else.
- Memory cap: the JS heap is capped at the Tool's declared `memory_mb`.
  Exceeding it faults **only that Invocation**, tagged `resource`. The cap is
  a heap cap, not a data-size cap: streaming APIs exist so data Tools
  (chunked file reads, streamed fetch, streamed artifact output) process
  arbitrarily large data in bounded memory. The bootstrap prompt (§7.7)
  teaches: “stream anything large; never load it whole.”
- Tool output may be streamed; the daemon writes it directly to an artifact
  file (§9.6), with only a bounded preview entering model context.

### 7.6 Composition

- A Tool may invoke other Tools via `tools.call`. Nested Invocations are
  recorded and measured like top-level ones. Composition cannot launder
  permissions: the called Tool runs under **its own** declared Capabilities
  and the outer Tool's Capabilities also apply. Nesting depth is bounded by
  the sandbox implementation (cycle-safe), faults tagged as Invocation
  failures.
- Out of v1: shared JS modules/libraries (every Tool is one self-contained
  file; duplication is accepted to keep versioning/retirement atomic), and
  LLM calls from inside Tools (only the agent loop talks to the provider).

### 7.7 Bootstrap system prompt

- The daemon carries a **versioned bootstrap system prompt** (frozen in v1,
  not optimised). It states: you have only `create_tool`; build small,
  single-purpose, minimally-Capabilitied Tools on demand; validate/test each
  after creating it; stream large data; prefer retiring broken Tools and
  creating new versions over silent workarounds. The prompt documents the
  exact host API of §7.5 so first-try Tool success is maximised.
- No starter Tools ship. First-run UX is the wizard (§10.1) plus an optional
  unfilled first-message prompt.

### 7.8 Import / export

- Single Tools or the whole registry export as bundle files (JS + schemas +
  Capability declarations + `memory_mb`; **no secrets, no Invocation
  history, no metrics**).
- Imports are **never auto-trusted**: Capabilities displayed, first-run
  approval applies (§10.3), metrics start at zero, author recorded as
  `import`. A name collision imports as a new Tool, never a silent overwrite.

## 8. Metrics and Self-Optimisation

### 8.1 What is measured (per Tool, pooled across all Sessions)

1. **Tool Success Rate (primary trigger):** share of Invocations that finish
   without runtime error, not user-cancelled, with output validating against
   the output schema. (No time term — §6 removed timeouts by design.)
2. **Retry Rate:** share of “successful” Invocations immediately retried or
   rewritten by the agent (hidden-failure signal; optimiser context).
3. **Latency p50/p95** and **Cost per Success** (wall time + provider tokens
   attributable per successful Invocation; optimiser context).
4. **Validation Failure Rate:** share of Invocations rejected on input/output
   schema (points at a badly described Tool; optimiser context).

All thresholds in this section are **configurable** (§13); defaults stated.

### 8.2 Accounting rules

- Tags: `user-cancelled`, `resource` (OOM), `abandoned`, and malformed model
  tool-calls (counted separately, §4.4) never masquerade as Tool bugs.
- Metrics pool across Sessions (volume is required for significance).
  Session deletion removes payloads but retains anonymised tallies (§15);
  Tool purge removes tallies too.
- Retired Tools keep their metrics but are never optimised.

### 8.3 Trigger and generation

- **Trigger:** success rate **< 90%** over the last **≥20** Invocations
  (rolling window: last 100). Only the primary metric triggers.
- A **background optimiser job** in the daemon queues on trigger and executes
  only when no active Run is waiting on that Tool (B-with-C politeness: it
  never interrupts a Session). It uses the session model by default, or an
  optional separate optimiser model + reasoning effort.
- **Guardrails (all configurable):** per-Tool cooldown (default 1h), max
  attempts per Tool per day (default 3), daemon-wide optimisation token
  budget. After **3 consecutive failed Candidates** for one Tool, optimising
  stops, the Tool is flagged `needs attention` in the Tool browser, and the
  user is surfaced the history (the in-Session agent may still fix it
  manually via a new version).
- **Candidate scope:** code, description, and schemas may all change (bad
  descriptions are a first-class failure cause). **Capabilities and
  `memory_mb` may never be widened by a Candidate** — widening always
  requires user approval (§10.3). Given `resource`-tagged failures, the
  optimiser must attempt streaming/chunking fixes before other changes.
- Candidate inputs: current Tool version, redacted recorded Invocations
  (§8.4, §15). The optimiser LLM call goes only to the user's configured
  endpoint.

### 8.4 Sandboxed replay (the promotion gate)

- Replay set: **all recorded failed Invocations + an equal-sized sample of
  recorded successes** (regression guard).
- Environment: throwaway **copy-on-write overlay sandbox** — filesystem
  writes go to a discarded overlay; **network denied by default**
  (recorded responses served where available); `process` denied; **no real
  secrets injected** (redacted recordings only). Nested `tools.call`s
  execute inside the same overlay under their own Capabilities.
- A Tool that genuinely requires live network to replay is marked
  **replay-limited**; its Candidate can only be promoted with explicit user
  approval in the TUI, never automatically.

### 8.5 Promotion

- **Bar:** Candidate replay success ≥ **10 points** above the current
  version's replay score on the full replay set, **and zero regressions** on
  previously-successful replays. Otherwise the Candidate is discarded and
  logged in the Tool's history.
- Promotion is atomic: running Invocations finish on their starting version;
  subsequent Invocations use the promoted version. Every version is retained
  with **one-action rollback** in the Tool browser. Promotions, rollbacks,
  Candidate discards, and `needs attention` flags are audit-logged (§14).

## 9. Daemon Core

### 9.1 Lifecycle

- Clients auto-start the daemon if it is not running and reattach if it is.
  The daemon stays alive while **any Run/Invocation is active OR any client
  is connected**. When both reach zero, it waits a **60s grace period**
  (configurable), then shuts down gracefully, flushing Sessions, metrics,
  and checkpoints. State lives on disk, never only in memory.

### 9.2 Concurrency

- Unlimited concurrent Sessions (subject to host resources); parallel
  Invocations per §5.2. The daemon is the **single writer** of all state, so
  multiple clients can never diverge (§10.4).

### 9.3 Protocol

- Versioned request/event protocol (`formless-protocol`): client requests
  (start/steer/queue/interrupt Run, approve, browse/manage Tools and
  Sessions, settings, export/import) and daemon→client streamed events
  (model content and reasoning deltas, tool-call lifecycle, pending/stall
  notices, approval requests, compaction notices, optimiser activity).
- A client whose protocol version mismatches the daemon's **refuses to
  attach** with a clear message (no silent incompatibility).

### 9.4 Instances, profiles, storage, data dirs

- Single instance per data dir (lock file); starting a second daemon against
  the same dir is refused and points at the running one. `--profile <name>`
  gives an isolated daemon + data/config dirs (intended for testing
  dangerous Tools).
- Data dir (OS-appropriate): `~/Library/Application Support/formless`
  (macOS), `$XDG_DATA_HOME/formless` (Linux), `%APPDATA%\formless`
  (Windows); config in the matching config dir (`config.toml` + keychain/
  fallback key file). Contents: SQLite DB, Tool version files, artifacts,
  rotated daemon logs, audit log.
- **SQLite** holds Sessions, Runs, Tool metadata/versions index,
  Invocations, metrics tallies, compaction checkpoints, approvals, audit
  events. Schema is versioned; the daemon auto-migrates on start and takes a
  **backup before migrating**. On corruption it refuses to guess: it stops,
  reports, and points at the backup (see Edge Cases, §16).

### 9.5 Crash recovery

- The daemon never idle-closes mid-Run (§9.1) but can crash/be killed. On
  restart, affected Runs are marked **Interrupted**; the in-flight Invocation
  is recorded `interrupted — outcome unknown` and is **never automatically
  re-executed** (side effects unknowable). On reattach the TUI offers
  **Resume** (continue from the last completed tool call, with the
  interruption visible to the agent in context) or **Abandon**. All other
  Sessions/history are untouched.

### 9.6 Artifacts

- Large Tool outputs are stored as artifact files (streamed writes, §7.5);
  only a bounded preview (configurable length) enters model context. Outputs
  are referenced by artifact id in transcripts so compaction/resume can
  re-read them. Retention caps apply (§13, §15).

## 10. TUI (v1 user interface)

### 10.1 First-run onboarding wizard

- If no valid config exists, first launch runs a TUI wizard: (1) enter
  `base_url` + API key, (2) fetch `/models` (manual fallback), pick model +
  reasoning effort, (3) run the endpoint probe (§4.3) incl. test tool-call;
  failure blocks Session start with an explanation, (4) optionally type a
  first message (nothing pre-filled, no starter Tools).

### 10.2 Home (chat) view

- Single-column streaming chat: model deltas, inline **collapsible** tool
  calls (name, version, input, status, elapsed; Pending Invocation badges
  with live elapsed + `possibly stalled` flags, §6), compaction notices,
  optimiser activity notes. Full scrollback is preserved across modal
  overlays and view switches.

### 10.3 Approvals (modal)

- First execution of a Tool whose Capabilities are sensitive (`fs:write`,
  `net`, `process`, `env`, or any widening) requires approval. Approval is
  **per-Tool and survives version bumps; it re-prompts only when
  Capabilities/`memory_mb` widen**, showing the capability diff (new items
  highlighted, broad unscoped grants flagged). Replay-limited promotions
  (§8.4) use the same modal. In parallel batches, one combined modal covers
  all held calls (§5.2). Modals are keyboard-driven, never lose scrollback.

### 10.4 Sessions, Tools, and Settings views

- **Sessions view:** list/create/rename/resume/delete/export/import. Two
  TUIs may attach to the same Session simultaneously: both are full
  participants (like two terminals on one tmux session); messages are
  attributed by client; an approval is resolved by **whoever answers
  first** (the other modal closes: “answered elsewhere”).
- **Tools view (browser):** list with success-rate/latency summaries, detail
  per Tool — descriptions, schemas, Capabilities, `memory_mb`, version
  history (author, timestamps, replaced-version stats), Invocation tallies,
  optimiser history, `needs attention` flags. Actions: rollback, restore
  (retired), purge (confirmed, user-only), export/import, audit log view.
  Tool JS is shown **read-only**; there is no in-TUI code editor in v1.
  There is no standalone metrics dashboard in v1 — metrics live here.
- **Settings view:** edits file-backed config (§13) and per-Session
  model/effort. Safely hot-reloadable values apply immediately; data-dir /
  transport-class changes state “restart required”. Includes the full-reset
  action (§15).

### 10.5 Input and keys

- Enter: send / Steer while running (Ctrl+Enter: queue; configurable).
  `Esc`: interrupt Run. `Ctrl+C`: detach client. `Tab` (and slash commands)
  switch views. Mouse supported; **every** action is keyboard-reachable.
  Slash commands (mirror the views): `/new`, `/sessions`, `/tools`,
  `/model`, `/settings`, `/stop`, `/approve`, `/thinking`, `/help`.
  Plain text is always chat.
- Terminal colours respected, `NO_COLOR` honoured. Narrow terminals remain
  usable (single-column-first design; no multi-pane dashboard in v1).

### 10.6 Live feedback (v2)

- **Colour:** roles, statuses, list selections, borders, and approval
  prompts are colour-coded; the chat has a friendly empty state and
  `/help` lists the shortcuts. `NO_COLOR` disables all colour.
- **Real-time thinking:** when the endpoint streams reasoning deltas
  (`reasoning_content`, or `reasoning` on some endpoints) the daemon
  forwards them as `ReasoningDelta` events and the TUI shows them live in
  a dimmed, separate "Thinking" block (`/thinking` toggles it). Reasoning
  is never fabricated when the endpoint emits none, never shown as the
  assistant's answer, and never persisted as message content; on
  tool-call turns it is kept only as provider metadata so thinking-mode
  endpoints that require it echoed back keep working.
- **Agent activity:** while a Run is active the status line always shows
  what the agent is doing — an animated spinner, the current step
  (waiting for model, thinking, responding, running a named Tool,
  awaiting approval), and elapsed time in that step — so silent waits
  never look like a hang.

## 11. Compaction

- Trigger: Run/Session approaching the model's `context_tokens` (§4.2).
- Preference order: **server-side compaction if the endpoint explicitly
  supports it, otherwise daemon-side summarisation** (the common path) using
  the Session model or an optional separate compaction model. Server-side is
  opt-in via `compaction.server_side`; the daemon then calls
  `POST {base_url}/compact` with `{model, input}` and accepts a
  `{summary}`/`{output}`/`{content}` response, falling back to daemon-side
  summarisation (with a visible note) if that call fails. Either way, the
  output is the same persisted checkpoint format.
- Contents: older turns and tool results are summarised into the checkpoint;
  the system prompt, recent turns, pending handles, and artifact references
  are preserved verbatim. Large outputs are already artifacts (§9.6);
  compaction summarises previews + references, never the only copy.
- Compaction is **visible** (notice in chat; checkpoint shown in Session
  detail) — never silent — and persisted in SQLite so resumed Sessions
  continue exactly.

## 12. CLI

- `formless` (no args) launches the TUI (auto-starting the daemon).
- `formless run "task"` — headless Run (§5 semantics) in an ephemeral
  Session, or `--session <id|name>` to continue an existing one. Streaming
  human-readable output on stdout; daemon logs on stderr; `--json` emits one
  JSON event per line (the §9.3 event stream). **Approvals fail closed** in
  headless mode: an unapproved sensitive Invocation fails with a clear error
  (approve first in the TUI), unless `--yes` is passed (auto-approve for
  that Run only). Exit codes: `0` answered, `1` Run errored/paused, `2`
  usage/config error, `130` interrupted.
- `formless daemon start|stop|status` — daemon management (same lifecycle
  rules as §9.1).
- `formless tools list|export|import …` — registry inspection/transfer
  (same trust rules as §7.8).
- `formless doctor` — diagnostics: config validity, data dir, keychain
  reachability, endpoint connectivity + tool-calling probe result.
- `formless reset` — full wipe (§15); typed confirmation in TUI contexts,
  `--force` required headless.
- The CLI is a client of the same protocol; **no behaviour lives only in
  the CLI**.

## 13. Configuration (complete settings table)

- File: `config.toml` in the OS config dir; sections `[daemon] [provider]
  [optimiser] [metrics] [tools]`. Env vars `FORMLESS_*` override file values.
  Per-Session model/effort live in SQLite, not the file. Nothing configurable
  is a hidden constant: this table is exhaustive for v1.

| Key | Default | Meaning |
|---|---|---|
| `daemon.idle_grace_secs` | 60 | Idle-close grace after last Run + last client (§9.1) |
| `daemon.stall_warning_secs` | 300 | Silence before `possibly stalled` flag (§6.1) |
| `daemon.log_max_bytes` / `log_keep` | 10 MB / 5 | Daemon log rotation (§9.4) |
| `provider.base_url` | — (wizard) | Verbatim endpoint base (§4.1) |
| `provider.api_key_ref` | keychain | Key storage pointer, never plaintext (§4.2) |
| `provider.extra_headers` | {} | Free-form headers per request (§4.1) |
| `provider.model` / `reasoning_effort` | — (wizard) | Daemon-wide defaults (§4.2) |
| `provider.context_tokens` (per model) | 128000 | Compaction trigger basis (§4.2, §11) |
| `provider.max_retries` | 5 | Bounded backoff attempts (§4.4) |
| `tools.pending_threshold_secs` | 300 | Soft async handoff threshold (§6.2) |
| `tools.default_memory_mb` | 256 | Default per-Invocation heap (§7.4) |
| `tools.max_memory_mb` | 4096 | Daemon bound on declared memory (§7.4) |
| `tools.artifact_preview_chars` | 2000 | Context preview of artifact outputs (§9.6) |
| `metrics.success_window` | 100 | Rolling window for success rate (§8.3) |
| `metrics.trigger_min_invocations` | 20 | Minimum sample before trigger (§8.3) |
| `metrics.trigger_success_below` | 0.90 | Trigger threshold (§8.3) |
| `metrics.retention_per_tool` | 200 | Recorded Invocations kept per Tool (§15) |
| `optimiser.cooldown_secs` | 3600 | Per-Tool attempt cooldown (§8.3) |
| `optimiser.max_attempts_per_day` | 3 | Per-Tool daily cap (§8.3) |
| `optimiser.give_up_after_failures` | 3 | Consecutive failures → `needs attention` (§8.3) |
| `optimiser.promotion_margin_points` | 10 | Replay win required (§8.5) |
| `optimiser.model` / `reasoning_effort` | (session) | Optional separate optimiser model (§8.3) |
| `optimiser.token_budget` | (none) | Optional daemon optimisation budget (§8.3) |
| `compaction.model` | (session) | Optional separate compaction model (§11) |
| `compaction.server_side` | false | Prefer `POST {base_url}/compact` when supported (§11) |
| `tui.queue_key` | Ctrl+Enter | Queue-vs-steer binding (§10.5) |

## 14. Security

- **Trust boundary:** the daemon serves only the owning local user (socket/
  pipe ACL). Any local process running as that user is trusted; nothing
  network-facing exists in v1.
- **Sandbox:** QuickJS has zero ambient authority. Every privileged effect
  goes through a daemon host call checked against the Tool version's declared
  Capabilities + approved state. JS interrupt support makes user
  cancellation actually stop executing code.
- **Path containment:** `fs:*` scopes are canonicalised at approval and at
  each access; `..` and symlink escapes outside the granted roots are denied.
- **Process:** `process.run` execs the named binary directly (no shell, no
  string interpolation); only commands in the Tool's declared scope run.
- **Network:** `fetch` honours the `net` scope (domain allowlist when
  scoped); redirects may not escape a domain scope.
- **Approvals** (§10.3) are the human gate for sensitive first use and for
  any Capability widening (including Candidate/optimizer-originated changes).
- **Audit log:** every approval decision, promotion, rollback, discard,
  retirement, restore, purge, import, reset, and optimiser give-up is
  recorded (actor, time, Tool, capability diff) and viewable in the Tools
  view. Tool inputs/outputs recording follows §15 redaction.

## 15. Privacy and data lifecycle

- **Zero telemetry.** No analytics, no update checks, no crash reporting.
  The only outbound traffic is to the user's configured provider endpoint
  (sessions, optimiser, compaction) — always with redacted secrets in
  optimiser paths (§8.3).
- **Recording:** Invocation inputs/outputs are recorded for metrics + replay,
  with secret-pattern redaction (API keys, bearer tokens, configured
  patterns), capped at `metrics.retention_per_tool` (default 200/Tool).
  Recorded payloads never leave the data dir except redacted, to the user's
  own endpoint, for optimisation.
- **Deletion:** Session delete removes transcripts + Invocation payloads but
  keeps anonymised per-Tool tallies (payloads gone, counts stay). Tool purge
  removes everything incl. tallies. **Full reset** (Settings or
  `formless reset`) stops the daemon and deletes config, keychain entry, and
  the entire data dir, with explicit confirmation.
- **Imports/exports** contain no secrets, metrics, or history (§7.8).

## 16. Error handling and edge cases (normative)

- **Endpoint unreachable mid-Run:** retries per §4.4, then Run pauses with
  the error surfaced; resume retries the request (no state loss).
- **Invalid key / model not found / `/models` down:** wizard blocks with
  the provider's error verbatim + `doctor` hints; manual model entry offered.
- **Tool creation storm (agent looping `create_tool`):** allowed (no Run
  limits), but each creation is validated/smoke-tested (§7.1) and visible
  live in chat; Esc is the stop. Creation attempts are audit-logged.
- **Candidate for a retired/deleted Tool:** cancelled; retired Tools are
  never optimised (§8.2). Promotion racing a retirement: retirement wins;
  the Candidate is discarded and logged.
- **Two creators, same name:** serialised (§7.2). **Two approvers, same
  modal:** first answer wins (§10.4).
- **Daemon already running, older/newer protocol:** client refuses with
  versions shown (§9.3). **Second daemon, same data dir:** refused (§9.4).
- **SQLite corruption:** daemon stops, reports, points at the pre-migration
  backup; never reinitialises silently. **Disk full:** writes fail loudly;
  the Run pauses; no partial Tool version is ever visible.
- **Huge Tool output / huge file:** streamed to artifact (§9.6); context
  receives preview only. **JS infinite loop:** burns its own sandbox thread
  until user cancel (§6.1); daemon stays responsive.
- **Malformed model tool-calls:** correction loop (§4.4), separately
  counted. **Empty-input smoke test failing at creation:** creation
  rejected (§7.1) — Tools must tolerate the smoke input or declare why via
  schema (schemas requiring fields will fail empty input; the smoke test
  uses a schema-minimal generated input in that case).

## 17. Non-goals (v2 log — explicitly out of v1)

- Web frontend and encrypted **Relay** hosting (daemon→relay, hosted FE).
- Hosted/multi-user mode, accounts, sharing servers.
- Subscription/OAuth providers; Responses-API endpoints; prompt-based
  tool-calling fallback for non-tool-calling models.
- Prompt/memory self-optimisation (system prompt frozen in v1 §7.7);
  routing/model auto-selection.
- Shared JS modules; LLM calls from inside Tools; in-TUI Tool code editor;
  standalone metrics dashboard; persistent multi-pane TUI.
- Session fork, cross-Session search, edit-and-retry.
- Distribution polish: signed release binaries, Homebrew/winget packages,
  any update mechanism (v1 installs via `cargo install` / source build).
- Additional Capabilities (clipboard, notifications, sub-agents).
- Remember startup options: relaunching restores the previous setup
  (base URL, model, reasoning effort) and the saved API key, so the
  wizard is skipped — or shown pre-filled with a "keep saved key" choice —
  instead of asking the user to re-enter the key on every start. The key
  is only requested again when it is changed, missing, or after a reset.

## 18. Roadmap and acceptance criteria

Each milestone is independently usable; later milestones add, never redesign.

### M1 — Chat core

Daemon (lifecycle, protocol, SQLite, config/keychain), provider client
(streaming, retries, reasoning effort, probe), onboarding wizard, TUI chat +
Sessions views, compaction (daemon-side).
**Accept when:** fresh install completes the wizard against a real
OpenAI-compatible endpoint (incl. a local one); unsupported-tool-calling
endpoints are refused at probe; two TUIs share the daemon and one Session;
idle-close + auto-restart works on macOS, Linux, Windows; a long Session
compacts visibly and resumes after daemon restart byte-identically.

### M2 — Tools

`create_tool` + validation/smoke gate, QuickJS sandbox + full host API
(incl. streaming + artifacts), registry/versioning/retire/restore, approvals,
metrics recording, Tools view.
**Accept when:** from an empty registry the agent builds, validates, and
reuses Tools across Sessions; a Tool exceeding its Capabilities is denied at
the host-call layer (demonstrated for fs escape, unscoped process, env
leak); unapproved sensitive first-run blocks on the modal and headless runs
fail closed; per-version metrics match recorded Invocations exactly.

### M3 — Self-optimisation

Metrics aggregation, trigger, background optimiser, sandboxed replay,
promotion/rollback, guardrails, audit surfacing in Tools view.
**Accept when:** a Tool driven below trigger yields a Candidate that is
promoted only on a ≥10-point zero-regression replay win (demonstrated with
an intentionally broken Tool); replay provably touches no real files/network/
secrets; capability-widening Candidates route to human approval; three
failed Candidates flag `needs attention` and stop; rollback restores the
prior version's behaviour; `create_tool` is untouched throughout.

### M4 — Async, CLI, and data portability

Pending Invocations (handle poll/await + push delivery, abandon), server-side
compaction when supported, headless CLI (`run --json`, exit codes), Tool +
Session export/import, profiles, doctor, reset.
**Accept when:** a >300s Invocation hands off, completes in the background,
and its result reaches the agent (both paths) with the Run correctly
finishing; daemon crash mid-Run recovers as Interrupted with no re-execution;
`formless run --json` streams events and exits per §12; an exported Tool
imports on a second profile untrusted, with fresh metrics and first-run
approval; reset wipes everything incl. keychain.

### Cross-cutting acceptance (all milestones)

- Zero outbound traffic in tests except to the configured endpoint
  (asserted with a recording mock endpoint).
- Every setting in §13 changes behaviour without recompilation; no hidden
  constants for any settled default.
- Every §16 edge case has an automated test or scripted demonstration.

## 19. Testing strategy (summary)

- Unit: schema validation, capability path containment, metrics math and
  trigger/promotion bars, compaction checkpoint round-trip.
- Integration: daemon against a **mock OpenAI-compatible server** (scripted
  streaming, tool-calls, 429/5xx, malformed calls, no-tool-calling probe
  failure); QuickJS sandbox escape suite (one test per Capability boundary).
- End-to-end: scripted TUI/CLI flows for M1–M4 acceptance lists; crash-kill
  daemon mid-Run and mid-Promotion recovery tests.
- No test may require a real provider key; all provider behaviour is mocked
  or pointed at a local compatible endpoint.



