# Formless architecture

## Principles

1. **A small, fixed boot set.** A fresh install exposes 25 built-in Tools and nothing else: the registry, memory, MCP, skills and planning primitives ([§5.1](#the-built-in-tools)). No domain Tool ships — web access, files, shell work and everything task-specific is built by the agent, then persisted and reused.
2. **Measured, never vibes.** Self-Optimisation is triggered and judged only by recorded Invocation outcomes. No silent or untested mutation, ever.
3. **Opt-in Run step controls.** Runs have no step cap by default. Existing [step controls](#run-controls) let you choose a pause threshold.
4. **Deny by default.** Tools run sandboxed with only their declared Capabilities. The runtime offers nothing a Tool was not granted.
5. **Local-first.** The daemon stores Sessions and Tools locally. See [network activity, naming controls and telemetry](#security-and-privacy).
6. **One core, many clients.** All behaviour lives in the Daemon Core. TUI, CLI and future frontends are interchangeable clients of one protocol.
7. **Everything is versioned and reversible.** Tools, promotions, config and Session checkpoints keep history; destructive actions are user-only and confirmed.

---

## Architecture

```
┌──────────────┐                        ┌──────────────────────────────────┐
│  TUI         │                        │           Daemon Core            │
│  (ratatui)   │   framed protocol      │                                  │
├──────────────┤◀──────────────────────▶│  agent loop · provider client    │
│  CLI         │   unix socket /        │  Sessions & Runs · Tool registry │
│  (headless)  │   named pipe           │  metrics · optimiser · storage   │
└──────────────┘                        └───────┬───────────────┬──────────┘
                                                │               │
                                    ┌───────────▼─────┐   ┌─────▼──────────┐
                                    │ QuickJS sandbox │   │Chat Completions│
                                    │ one isolate per │   │ Responses      │
                                    │ Tool, gated by  │   │ Anthropic      │
                                    │ Capabilities    │   │ Messages       │
                                    └─────────────────┘   └────────────────┘
```

* **One binary:** `formless`. The daemon is not a separate executable — it is `formless daemon` running in daemon mode, spawned automatically by clients. Ordinary startup needs no separately installed service; [optional per-user autostart](usage.md#daemon-diagnostics-and-reset) is supported.
* **One Daemon Core per data dir**, enforced by a lock file. `--profile <name>` runs an additional isolated daemon with its own data and config dirs.
* **Transport:** a Unix domain socket in the data dir (dir mode `0700`) on macOS/Linux, a named pipe with an owner-only ACL on Windows. No localhost-TCP fallback in v1.
* **Clients are stateless.** The daemon persists everything; killing the TUI never kills a Run.

### Crate map

| Crate | Responsibility |
|---|---|
| `formless-core` | Daemon Core: agent loop, provider client, Session/Run management, Tool registry, metrics, optimiser, storage, compaction |
| `formless-runtime` | QuickJS sandbox (QuickJS-NG via `rquickjs`) and the Capability-gated host API, including streaming primitives |
| `formless-protocol` | Shared daemon↔client types: requests, streamed events, errors (versioned) |
| `formless-tui` | ratatui TUI client |
| `formless` | Binary entrypoint: CLI commands + default TUI launch |

### Built with

| Layer | Choice |
|---|---|
| Language | Rust, edition 2021, one workspace |
| Terminal UI | [ratatui](https://ratatui.rs/) |
| Tool sandbox | [QuickJS-NG](https://quickjs-ng.github.io/quickjs/) via [`rquickjs`](https://github.com/DelSkayn/rquickjs) |
| Async runtime | [Tokio](https://tokio.rs/) |
| Local storage | SQLite via [`rusqlite`](https://github.com/rusqlite/rusqlite) |
| HTTP and streaming | [`reqwest`](https://docs.rs/reqwest) with rustls |
| Local session naming | [Candle](https://github.com/huggingface/candle) with [Qwen2.5-0.5B-Instruct](https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF) by default, or [LFM2.5-350M](https://huggingface.co/LiquidAI/LFM2.5-350M-GGUF) |

---

## The Tool system

### The built-in Tools

A fresh install exposes **25 built-in Tools**. They are implemented by the daemon itself rather than stored in the registry, so they have no versions, are excluded from Self-Optimisation, cannot be retired and hold no Capabilities of their own. They exist so the agent can grow and maintain its registry, reach MCP servers and skills, keep short notes about you, and stay in touch with you while it works. How often each one is called is reported by `read_metrics({kind: "builtins"})`.

Everything else — the web, the filesystem, the shell, git, file formats, domain logic — is a registry Tool the agent writes ([§5.2](#create_tool--the-tool-factory)).

**Growing and maintaining the registry**

| Tool | Arguments | What it does |
|---|---|---|
| `create_tool` | `name`, `title`, `description`, `tags[]`, `input_schema`, `output_schema`, `implementation_js`, `capabilities[]`, `memory_mb?` | Writes a Tool — or a new version of an existing one — after validating the name, both JSON Schemas and the JS syntax, then smoke-testing it in the sandbox. Nothing broken is saved. [§5.2](#create_tool--the-tool-factory). |
| `create_module` | `name`, `description?`, `implementation_js` | Adds a version of a shared JS module (`module.exports = …`) that any Tool loads with `requireModule(name)`. Versions are immutable. |
| `find_tools` | `query`, `limit?` | Keyword search over Tool names, descriptions and tags; the matches become callable in the current Session. This is how a Tool written in an earlier Session comes back, and it is the first step before writing a new one. |
| `read_tool` | `name`, `version?`, `diff_from?` | A Tool's code, schemas, Capabilities, `memory_mb`, version history, metrics and recent failures; `diff_from` shows what one version changed. Read before writing a new version, so you change a Tool rather than rewrite it from memory. |
| `read_module` | `name`, `version?` | A module's source, description and history, plus the Tools that require it. |
| `test_tool` | `input`, plus `name` or the full `create_tool` draft | Dry-runs a saved Tool or an unsaved draft on real input, under its declared Capabilities and with the usual approvals. Nothing is saved or measured; it returns the output (or the error), console logs, and whether the output matched `output_schema`. |
| `run_script` | `code`, `uses[]`, `input?`, `save_as?` | Runs a short script that may call only the registry Tools named in `uses` (built-ins are not callable from a script) — looping, filtering, branching, fanning out with `Promise.all` — and returns just the final value. The script has no Capabilities of its own, so anything that touches the world stays in a Tool; `save_as` persists a script you would run twice as a Tool. |
| `unload_tools` | `names[]` | Drops Tools from the Session's listing at the next context compaction, so the prompt carries fewer, more relevant functions. `find_tools` loads them again later. |
| `retire_tool` | `name`, `reason` | Makes a Tool uncallable everywhere, immediately. Data is retained and the user can restore it; deleting code and tallies ([purge](#lifecycle-and-versioning)) is user-only. |

**Invocations, artifacts and metrics**

| Tool | Arguments | What it does |
|---|---|---|
| `check_invocation` | `invocation_id` | A Pending Invocation's status: still `pending`, or its final result. |
| `await_invocation` | `invocation_id` | Waits without a deadline for that result — this is how long work finishes instead of being killed ([§6](#invocations-async-by-design)). |
| `abandon_invocation` | `invocation_id` | Stops waiting for a handle: the Invocation still finishes and is recorded, but its result is not delivered. |
| `read_artifact` | `id`, `offset?`, `length?`, `grep?` | Reads a slice of a large output this Session stored as an artifact, or the lines matching `grep`, with line numbers. No approval needed, no other Session's artifacts. |
| `read_metrics` | `kind`, `filter?`, `limit?` | Registry health, for cleaning up and explaining: `tools` (`needs_attention` \| `failing` \| `unused` \| a name), `audit`, `models`, `runs`, `builtins`, `duplicates`, `mcp_chains` ([§9](#self-optimisation)). |

**Working with you, memory, MCP and skills**

| Tool | Arguments | What it does |
|---|---|---|
| `ask_user` | `question` or `questions[]` (up to 4), `options[]`, `multi_select?` | Asks and waits for an answer without ending the Run, ideally with the options spelled out. With no client attached it fails at once, so the agent proceeds on a stated assumption instead of blocking. |
| `update_plan` | `items[]` (`content`, `status`, up to 20) | The checklist pinned above the input for work of three or more steps; each call replaces the previous list. |
| `remember` | `text` (≤ 300 chars), `scope?` | Saves a stable preference or fact ("use pnpm", "answer in Thai") as a note that reaches later Sessions. The user sees every note and can edit or disable it. |
| `forget` | `id` | Deletes one memory note. |
| `add_mcp_server` | `name`, `transport`, `command`/`args`/`env_keys` or `url`, `reason` | Proposes an [MCP server](#mcp-servers); the user approves the exact command or URL and env keys before it is connected, and its tools then appear as `mcp__<server>__<tool>`. |
| `remove_mcp_server` | `name`, `reason` | Disconnects a server and retires its tools. |
| `list_mcp_resources` | `server?`, `query?` | Lists the files, records and documents connected servers expose. |
| `read_mcp_resource` | `server`, `uri` | Reads one of them; large contents become an artifact. |
| `load_skill` | `name` | Loads an enabled [skill's](#skills) `SKILL.md` instructions before doing the kind of task it covers. |
| `read_skill_file` | `name`, `path` | Reads one of that skill's extra files — reference docs, templates. |
| `schedule_run` | `prompt`, `at` or `cron`, `session_id?` | Runs a prompt later or on a schedule (five-field cron with month/weekday names or `@yearly`, local time); recurring jobs need the user's approval. |

Built-ins lead every Session's Tool list, in a fixed order, and are never unloaded by `unload_tools`.

### `create_tool` — the Tool factory

```text
create_tool(name, description, input_schema, output_schema,
            implementation_js, capabilities[], memory_mb?, allow_breaking=false)
```

* The **agent writes the JavaScript**: plain JS, one self-contained file, no TypeScript, no imports.
* **Validation gate at creation:** name rules, both schemas validated against JSON Schema draft 2020-12, JS syntax-checked, then **smoke-tested** in the replay-style sandbox with empty input. Any failure rejects creation and nothing broken is saved.
* Agent-authored schema updates that are breaking or unverified and have live or possible dependents require explicit `allow_breaking: true`; the classified dependency inventory and compatibility verdict are rechecked when the version is saved.
* `create_tool` is immutable, excluded from Self-Optimisation, and cannot be retired or widened.
* The smoke test is conclusive only until the Tool first meets a stub (fake fetch body, unbindable `$input` scope, …); a failure after that is reported as *inconclusive* and the Tool is saved.

`find_tools` → `read_tool` → `create_tool` is the loop a Tool goes through the first time; `test_tool` and `run_script` shorten the next pass.

Sub-agents are not a built-in: the agent builds a Tool with the `agent` Capability (`function run(i){ return agents.run(i.task); }`), so delegation is a measured, approval-gated Invocation. Several such calls in one turn run in parallel, and nesting is capped by `tools.max_agent_depth` (default 1).

Each Session's Tool list is frozen at its first step: up to `tools.inline_limit` (default 40) Tools, the most-used first, plus the Tools that Session creates or finds later. Tools created in other Sessions are reached with `find_tools`, so they never change this Session's cached prompt prefix.

### Lifecycle and versioning

* Tools are **never edited in place**. Any change creates a new integer version (`v1`, `v2`, …; no semver), recording timestamp, author (`agent` | `optimiser` | `import`) and, at promotion, the stats of the version it replaced.
* A running Invocation always finishes on the version it started with; new Invocations use the current version.
* **Retire** (agent): instant and global — the Tool becomes uncallable everywhere, data is retained, and it is restorable from the Tool browser. **Purge** (permanent deletion of code, history and tallies) is user-only, from the TUI.
* Name collisions at creation are serialised by the daemon: the second creator gets the existing Tool as a new version instead of a fork.

### Capabilities (closed taxonomy)

| Capability | Scoping | Notes |
|---|---|---|
| `net` | optionally per-domain | per-host scopes are the norm; unscoped `net` is broader, so it is approval-gated |
| `fs:read` / `fs:write` | absolute path or `$input.<field>` | an input-bound scope is granted per call for exactly the path(s) that field names |
| `process` | one binary | `process.run` never goes through a shell |
| `env` | one key | `env.get` only reveals keys the Tool declared |
| `clipboard:read` / `clipboard:write` | — | v2 |
| `notify` | — | v2 |
| `llm` | optionally a model | v2 |
| `agent` | optionally a model | v2 |

Capabilities are a **closed** list: no Tool can ask for a capability the runtime does not define, and nothing outside the declared set is reachable from the sandbox.

### The host API inside a Tool

QuickJS-NG runs in-process. Bare JS has no ambient authority: the daemon provides the whole surface, and every entry is gated by a declared Capability.

| API | Gated by | Purpose |
|---|---|---|
| `fetch(url, {method, headers, body, bodyEncoding, encoding})` → `{status, ok, url, headers, body, text(), json()}` | `net` | HTTP; redirects followed, every hop scope-checked |
| `fetchChunks(url, opts, size, cb)` | `net` | Stream large bodies in bounded memory. The whole request, including body reads, is limited by `tools.fetch_timeout_secs` regardless of chunk size. |
| `fs.read(path, {encoding})` / `fs.write` / `fs.append` | `fs:read` / `fs:write` | Whole-value access for small data |
| `fs.exists` / `fs.stat` / `fs.list(dir, {details})` | `fs:read` | Existence, metadata, directory listing |
| `fs.mkdir` / `fs.remove(path, {recursive})` / `fs.rename(from, to)` | `fs:write` | Tree changes |
| `fs.readChunks(path, size, cb, {encoding})` / `fs.readLines(path, cb)` | `fs:read` | Streaming reads — never load large data whole |
| `env.get(key)` | `env` | Read one declared key |
| `process.run(cmd, args, {cwd, env, stdin, stdinEncoding, encoding, timeout_ms})` → `{code, signal, stdout, stderr, timed_out, killed, truncated, stdout_cap_truncated, stderr_cap_truncated}` | `process` | Execute a declared binary without a shell. An optional timeout kills its group. The cap flags identify which streams exceeded the capture cap. |
| `process.spawn(cmd, args, opts)` → `{read, wait, kill, close}` | `process` | Read bounded `{stream, data, eof}` chunks; `wait` discards unread output; handles are reaped when the Invocation ends |
| `tools.call(name, input)` → Promise | — | Composition; await every call, and `Promise.all` runs several in parallel (nested Invocations are recorded and measured) |
| `clipboard.read()` / `clipboard.write(text)` | `clipboard:read` / `clipboard:write` | v2; OS clipboard |
| `notify(title, body)` | `notify` | v2; desktop notification |
| `llm.complete(prompt, {system, model})` | `llm` | v2; one non-streaming completion via the configured provider |
| `agents.run(task, {system, model})` | `agent` | v2; sub-agent over the same Tools (depth-limited, no step limit), returns its final text |
| `requireModule(name)` | — | v2; load a shared module created with `create_module` |
| `emit(chunk)` | — | Stream large output to an artifact file |
| `console.log`, `sleep`, `setTimeout`/`setInterval`/`clearTimeout`, `JSON`, `cryptoRandom()`, `randomBytes(n)` | — | Logging, timing and entropy |

Windows Job and Tool notifications use the BurntToast module in Windows
PowerShell and require an interactive login for the daemon account.

A complete Tool:

```js
// Tool: page_title — Capabilities: [net:example.com]
function run(input) {
  const res = fetch(input.url, { method: 'GET' });
  if (!res.ok) throw new Error('HTTP ' + res.status);
  const html = res.text();                      // small payloads only
  const m = /<title>([^<]*)<\/title>/i.exec(html);
  return { status: res.status, title: m ? m[1].trim() : null };
}
```

### Composition rules

* `tools.call` records and measures nested Invocations exactly like top-level ones.
* Composition cannot launder permissions: the called Tool runs under **its own** declared Capabilities, and the outer Tool's Capabilities also apply. Depth is bounded and cycle-safe.
* Shared code lives in versioned, immutable modules (`create_module`, loaded with `requireModule`); module loading is smoke-tested when a Tool is created.
* In smoke tests the v2 side-effecting APIs are stubbed, so creation never touches the clipboard, the network or the desktop.

---

### MCP servers

`formless mcp prompts [--json]` reports prompts from reachable enabled servers,
in daemon order. Discovery can start configured servers and silently skips
connection/list failures, so results may be incomplete. `formless mcp prompt
SERVER NAME --arg key=value [--arg empty=] [--json]` sends named string values
to the selected server. Values preserve spaces, additional equals and empty
strings; missing separators, empty keys and duplicate keys are usage errors
before daemon contact. Human output is the returned text verbatim, including
its trailing-newline state. The daemon projects `content.text` strings joined
with blank lines; roles and non-text parts are absent. JSON preserves the full
discovery array or daemon text report, not raw MCP messages. These commands do
not submit to a Session or change server configuration; server/daemon errors
retain their ordinary exit behavior, without promised local required/unknown
argument checks.

```sh
formless mcp prompts --json
formless mcp prompt docs summarize --arg 'topic=Unicode notes' --arg suffix=
```

Formless can also call the tools of external [MCP](https://modelcontextprotocol.io) servers. Add a stdio server to `config.toml`:

```toml
[mcp.servers.files]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/notes"]
env_keys = ["GITHUB_TOKEN"]   # daemon variables to pass through; nothing else is
```

Remote servers use HTTP (Streamable HTTP, or the older HTTP+SSE transport automatically):

```toml
[mcp.servers.docs]
transport = "http"
url = "https://example.com/mcp"
headers = { Authorization = "Bearer …" }
```

Or manage them from the CLI: `formless mcp add files -- npx -y @modelcontextprotocol/server-filesystem ~/notes`, `formless mcp add docs --url https://example.com/mcp`, `formless mcp list`, `formless mcp test files`, `formless mcp remove files` (the TUI has `/mcp`; the web app lists them under Settings). Servers that use OAuth need a one-time login (it opens your browser): `formless mcp login docs`.

Its tools appear in the registry as `mcp__files__<tool>`. The agent finds them with `find_tools`, calls need approval like any sensitive Tool, and every call is measured. Servers start on first use and stop when idle (`mcp.idle_secs`); their stderr goes to `<data>/mcp/<name>.log`.

### Skills

Third-party Tool, MCP, Skill and module descriptions are bounded single-line data.
Instruction-shaped wording is flagged for review, and description changes retain
before/after evidence. Open **Settings → Description reviews** in the web UI,
`/descriptionreviews` in the TUI, or `formless tools reviews` after reconnecting.
These warnings do not establish that unflagged metadata is safe; code and capability
approval remain separate.

Skills are `SKILL.md` folders of know-how (the Agent Skills format), installed from a folder, a git URL or a `.zip`:

```bash
formless skills add https://github.com/example/skills.git --subdir pdf
formless skills show pdf      # review: new installs start disabled
formless skills reviews pdf  # retained description warnings and before/after changes
formless skills enable pdf
formless skills list
formless skills export pdf -o pdf.skill.json   # files only, no metrics
formless skills import pdf.skill.json          # imported disabled, contents printed
```

### Memory

The agent remembers stable preferences you state ("use pnpm", "answer in Thai") as short notes. You see every one, and you can edit them.

A note's scope is `global` (the default) or `project:/absolute/path`. The memory
prompt lists global notes followed by project notes grouped under `When working in <path>:`. A newly added note applies from the next Session, or in the current
Session after compaction.

Manage notes with the commands below, in the TUI with `/memory` (see the
[key reference](keys.md#integrations)), or on the web under **Settings → Memory**.
See [User memory](../SPEC.md#711-user-memory) for details.

```bash
formless memory list
formless memory add "Projects live in ~/code"
formless memory add "Use pnpm" --scope project:/absolute/project/path
formless memory edit m1a2b3c4 --disable
formless memory edit m1a2b3c4 --scope project:/absolute/project/path
formless memory rm m1a2b3c4
```

## Invocations: async by design

* Every Tool call is an **Invocation**: one Tool version, one input, recorded and measured.
* Past a soft threshold (default 300 s) the Invocation becomes a **Pending Invocation** — still running, with a handle the agent holds (`check_invocation`, `await_invocation`, `abandon_invocation`) instead of a timeout killing it.
* Outcomes are tagged distinctly: success, error, `resource` (JS heap cap hit), `user-cancelled`, `abandoned`. User-cancelled and malformed model tool-calls never masquerade as Tool bugs, so the optimiser cannot fix what the user interrupted.
* Stalled provider requests follow the same pending pattern.
* Progress bars follow invocation IDs, so parallel calls to the same Tool keep independent progress and completion states. A stalled call retains its last reported fraction and label; repeating a fraction or changing only the label does not prove forward progress. Older daemon events without IDs update a bar only when the match is unambiguous.

---

## Sessions, Runs and Steer

* **Session** — a resumable conversation plus Run history; many run concurrently against one Daemon Core. Deleting a Session removes payloads but keeps anonymised tallies.
* **Run** — one unit of work: user message → final answer. No step cap by default; it ends on completion, fault or interruption.
* **Steer** — a user message sent while a Run is active, injected at the next step boundary.
* **Auto-naming** — when a Run finishes, the daemon can give an eligible Session with a provisional title a short name. Naming runs in the background, never replaces a name you set, and uses no provider tokens. The configured naming model runs locally on the CPU.
  Naming and automatic downloads are enabled by default. In a build with `embedded-llm`, these settings let an eligible Session with naming text fetch its missing configured catalogue model and tokenizer from Hugging Face. These are pinned assets verified with SHA-256 and stored under `<data dir>/models/`. If the model is unavailable, fails, or exceeds `naming.timeout_secs`, the daemon can use a first-words title instead. Set `naming.enabled = false` to turn off automatic naming or `naming.auto_download = false` to disable automatic downloads. These settings do not delete installed assets or disable the explicit `formless naming pull` command.
  Downloads time out after 60 seconds without data or two hours per file. After a failed automatic download, a later naming attempt can retry once five minutes have passed. Builds without the default `embedded-llm` Cargo feature use first-words titles.
* **Compaction** — a persisted summary checkpoint that replaces old history and Tool output as context grows, so long Sessions keep going without losing the trail.
* **Artifacts** — large Tool output is written to an artifact file; only a bounded preview enters model context.
* **Tracked path changes** — `/files` lists paths captured by live host filesystem operations, including directories created with `fs.mkdir`. `/rewind` attempts safe restoration and reports skipped paths with reasons. Programs launched by Tools and MCP tools can write outside this tracking; when those capabilities are invoked, file notes and recaps warn that untracked writes are possible, including when no paths were captured. Counts describe tracked paths only. Paths whose recovery images expired remain listed with the reason; rewind never substitutes an older Run silently. Windows restoration also requires retained original objects and live object or directory proofs where applicable. Lost or expired proofs, including process-local proofs lost on restart, leave those paths read-only rather than reconstructing authority from stored hashes.

#### Local naming models

Set `naming.model` to select a model. Sizes are decimal MB rounded to two places and cover the GGUF weights only. The tokenizer is downloaded separately.

| Model ID | Model | Quantization | Weights | License |
| --- | --- | --- | --- | --- |
| `qwen2.5-0.5b` | Qwen2.5-0.5B-Instruct (default) | `Q4_0` | 428.73 MB | [Apache-2.0](https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/blob/9217f5db79a29953eb74d5343926648285ec7e67/LICENSE) |
| `lfm2.5-350m` | LFM2.5-350M | `Q4_K_M` | 229.31 MB | [LFM Open License v1.0](https://huggingface.co/LiquidAI/LFM2.5-350M-GGUF/blob/657e078c94084481950a2d555a941481f715536b/LICENSE) |

### Context and compaction

Use `/context` to inspect the Session's context window and what fills it.
Formless automatically compacts as a Run approaches its model's context limit.
When the Session is idle, `/compact` requests a summary now; add a focus, for
example `/compact Keep repository constraints and unresolved decisions`, to
say what the summary should emphasize. A summary can omit detail; compaction
is not lossless.

On-demand compaction is refused while a Run is active or when nothing is new
since the last checkpoint. The ordinary daemon-side summarizer rejects an
empty or output-limit-cut summary before saving a checkpoint, retaining the
turns. Read any failure note before trying again. Provider-side compaction is
an opt-in endpoint-specific path, not a capability every provider supplies.
See [the generated command reference](keys.md#session),
[SPEC §11](../SPEC.md#11-compaction), and the
[context troubleshooting guidance](troubleshooting.md#context-pressure-or-failed-compaction).

### Run controls

Set a profile default with `limits.run_steps`, use `/limit N` or `/limit off`
in the TUI, or pass `formless run --max-steps N`. Reaching a step threshold
pauses that Run. See the existing [step-limit and resume/abandon behavior](../SPEC.md#54-interruption-and-resumption)
and [spending-limit rules](../SPEC.md#52-run-lifecycle). The client's `--timeout`
is a separate interruption option.

### Naming model catalogue

| Model ID | Model | GGUF quantization | GGUF weights (bytes) | Upstream license |
|---|---|---|---:|---|
| `qwen2.5-0.5b` | Qwen2.5-0.5B-Instruct (default) | Q4_0 | 428,730,208 | [Apache-2.0](https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct/blob/main/LICENSE) |
| `lfm2.5-350m` | LFM2.5-350M (alternative) | Q4_K_M | 229,312,224 | [LFM Open License v1.0](https://huggingface.co/LiquidAI/LFM2.5-350M/blob/main/LICENSE) |

Weights exclude the separate tokenizer files. The CLI displays whole decimal
MB by truncating: 428 MB and 229 MB, respectively.

---

## Life of a Run

1. A client connects to the daemon socket and sends a Run intent; the daemon creates or reuses the Session.
2. The agent loop sends the conversation plus every active Tool's schema to the provider.
3. The model either answers or calls a Tool. A call that needs a broad Capability pauses for approval.
4. The daemon executes the **Invocation** inside the QuickJS sandbox, limited to the Tool's declared Capabilities and `memory_mb`; large output streams into an artifact.
5. The outcome is recorded and folded into the Tool's metrics. The loop repeats until the model produces a final answer.
6. Answers stream to the client. Steers are injected at step boundaries; if the client dies, the Run keeps running and can be reattached or resumed.
7. Offline, when a Tool's success rate drops below the bar, a Candidate version is generated and must win a sandboxed replay before it replaces the current one.

---

## Self-Optimisation

Measured per Tool, pooled across all Sessions:

| Metric | Role |
|---|---|
| **Tool Success Rate** | Primary trigger: share of Invocations that finish without runtime error, not user-cancelled, with output validating against the output schema. No time term. |
| Retry Rate | Hidden-failure signal: successful Invocations immediately retried or rewritten |
| Latency p50/p95, Cost per Success | Wall time and provider tokens per successful Invocation |
| Validation Failure Rate | Schema rejections — points at a badly described Tool |

The loop:

1. **Trigger** — success rate below **90%** over the last **≥ 20** Invocations (rolling window of 100).
2. **Candidate** — a background job runs when no active Run is waiting on that Tool. Before contacting the provider, it freezes a bounded, outcome-independent pool of recent unique inputs (default `optimiser.replay_pool_limit = 200`) and splits it into generation and holdout sets (default `optimiser.replay_holdout_percent = 50`). Only generation examples and their errors reach the candidate model; held-out inputs stay private ([SPEC §8.4](../SPEC.md#84-sandboxed-replay-the-promotion-gate)).
3. **Sandboxed replay (the gate)** — Candidate and baseline run on the **same frozen held-out inputs** in separate throwaway copy-on-write overlays: writes are discarded, network is denied by default (recorded responses are served where available), `process` is denied, and no real secrets are injected. Network/MCP **replay-limited** Candidates still require explicit user approval. Legacy attempts without verifiable frozen provenance must be regenerated rather than treated as held-out evidence.
4. **Promotion bar** — by default, automatic promotion requires **≥ 10 points improvement**, **zero regressions on recorded-success inputs**, at least **20 held-out inputs** (`optimiser.replay_min_holdout`), and a two-sided exact paired McNemar **p ≤ .05** (`optimiser.replay_significance`). Attempt detail records held-out scores, 95% Wilson confidence intervals, discordant-pair counts and significance without raw held-out inputs. Otherwise-qualifying small-sample or nonsignificant wins go to `awaiting_approval`; capability-widening, schema-compatibility and replay-limited gates still apply. Candidates failing the margin/regression guard are discarded ([SPEC §8.5](../SPEC.md#85-promotion)).
5. **Promotion and rollback** — atomic: running Invocations finish on their starting version, later ones use the promoted version. Every version is kept with one-action rollback in the Tool browser. Promotions, rollbacks, discards and needs-attention flags are audit-logged.

Only the primary metric triggers; the rest inform the optimiser's context.

**Prompt and memory (v2).** The optimiser can also propose one short guidance or memory addendum to the system prompt. Each proposal is scored against the current prompt on scenario evals that check the agent's actual tool calls (search before create, least-privilege caps, streaming, …), run several times. It can be applied only if it wins by the same promotion margin with statistical significance, and only by the user (`formless prompts`, TUI `/proposals`). Applied addenda are versioned and roll back individually; the bootstrap prompt itself is never edited.

---

## Security and privacy

* **Deny by default.** A Capability is a ceiling the runtime enforces; it is never exceeded.
* **Approvals.** Broad or sensitive Capabilities prompt before first use — modal in the TUI, fail-closed in headless mode.
* **Single-user trust boundary.** Local access control is the socket/pipe file permissions on the data dir (`0700`).
* **Local-first.** Network activity includes calls to configured providers, connections to a [user-enabled relay](usage.md#browser-access-via-the-relay-v2), and [local naming model](#local-naming-models) downloads. In builds with embedded model support, automatic naming and downloads are enabled by default. Naming an untitled or provisionally named Session with suitable user text can trigger a background download if the selected catalogue model needs weights or a tokenizer. Set `naming.auto_download = false` to disable automatic downloads or `naming.enabled = false` to disable automatic naming.
* **Zero telemetry.** There is no automatic update checker; `formless update check` runs on request ([§3.12](usage.md#updating)).
* **Imports are never auto-trusted.** Capabilities are displayed, approval applies, metrics start at zero, and the author is recorded as `import`.
* **Data lifecycle.** `formless reset` wipes config, keychain entry and data dir; Session deletion removes payloads but keeps anonymised tallies; only a Tool purge deletes tallies.

---

## Repository layout

```text
formless/
├── Cargo.toml           workspace: edition 2021, Apache-2.0, version 0.1.0
├── SPEC.md              normative v1 specification — behaviour lives here
├── GLOSSARY.md          canonical vocabulary (Tool, Run, Invocation, Steer, …)
├── docs/adr/            ADRs: daemon core, single bootstrap tool,
│                        QuickJS tool runtime, success-rate-gated optimisation
├── crates/
│   ├── formless/            binary entrypoint: CLI subcommands, default TUI launch
│   ├── formless-core/       daemon, agent loop, provider client, tools, storage, optimiser
│   ├── formless-runtime/    QuickJS sandbox + Capability-gated host API
│   ├── formless-protocol/   shared daemon ↔ client types
│   └── formless-tui/        ratatui client
├── relay/               Cloudflare Worker + web frontend for browser access
├── packaging/           release notes, Homebrew formula, winget templates
├── scripts/install.sh   prebuilt-binary installer (or FORMLESS_FROM_SOURCE=1)
└── .github/workflows/   release.yml — macOS and Linux arm64/x86_64, Windows x86_64
```

---

## Development

The [CI workflow](https://github.com/athiwatc/formless/blob/main/.github/workflows/ci.yml) runs on self-hosted macOS ARM64 and Linux x64
runners for pushes to `main`, same-repository pull requests targeting `main`, and
manual dispatches. It checks Rust formatting and Clippy, runs workspace tests
(including doc tests), relay/web Node tests, relay wire conformance and offline
documentation/tooling tests, then builds the release workspace and smoke-tests
the CLI, man page and shell completions. Tested binaries, generated assets and
diagnostic logs are retained as workflow artifacts for seven days. Clippy uses
the development command below; existing warnings are reported without being
promoted to errors. Fork pull requests are skipped on the persistent runners.

Runners need the standard `self-hosted` label plus `macOS`/`ARM64` or
`Linux`/`X64`, and outbound access for GitHub Actions and dependency downloads.
The Mac needs Xcode Command Line Tools, Git, Rustup and Python 3.9+. The Linux
runner needs Git and a Debian/Ubuntu-compatible environment; CI verifies the C
compiler, Python and D-Bus development headers/pkg-config, installing missing
build dependencies through apt as root or with noninteractive sudo. Both
platforms run native jobs, including Linux runners themselves hosted in Docker.
CI installs stable Rust with rustfmt/Clippy and Node 24. Cargo dependencies are cached;
application data/config and logs use private temporary directories cleaned after
the job. Test debug artifacts are removed before release compilation to limit disk use.
Release packaging remains in `release.yml`, and live external-link checks run
in `docs.yml`. Linux CI also runs every relay Chromium suite and the loopback
push check. The runner needs Python 3.10+ with venv support, Chromium's shared libraries
and Poppler tools. CI downloads its pinned Playwright Chromium into the job
directory and includes screenshots and logs from failed suites in the diagnostic
artifact. See the [relay browser instructions](../relay/README.md#browser-regressions)
for local runs and WebKit coverage. AUR packaging tests run on Linux because
they require GNU install.

```bash
cargo build --workspace                # debug build of the single binary
cargo test --workspace                 # unit + integration tests
cargo fmt --all                        # formatting
cargo clippy --workspace --all-targets # lints

RUST_LOG=debug cargo run -- daemon status
RUST_LOG=debug cargo run -- run "write a tool that converts a CSV to JSON"
```

`cargo test -p formless-bin cli_help_tests` checks every visible command's help
without dispatching commands, and compares the rendered root help and command
definitions with the [CLI reference](cli.md#cli-reference) and SPEC §12 reference tables. It runs as part
of the workspace tests and rejects missing, stale, or duplicate entries.
Examples are shell-split and parsed by the real CLI without dispatching
operations, so obsolete flags fail locally. Documentation topic checks reject
ambiguous prefixes and unknown names. `formless docs jobs --text` reads the
Markdown compiled into this build, without a profile, daemon or browser;
`docs --print` retains bare online-URL output. Terminal `/docs --text jobs`
reads the same bundled content (a pager in the TUI, printed lines in plain
mode). A failed/unavailable desktop opener falls back to bundled text; the
current online copy is labeled separately because it may differ from this build.

Rules of the road: **no behaviour may live only in a client** — the CLI and TUI are both clients of the daemon protocol. Every Tool is validated and smoke-tested at creation, and every behavioural change should be reflected in `SPEC.md` first.

Where to start:

* **Runtime / sandbox** — `crates/formless-runtime`: the host API table in [§5.5](#the-host-api-inside-a-tool) is the contract.
* **Agent loop, tools, optimiser** — `crates/formless-core`; [§5](#the-tool-system) and [§9](#self-optimisation) describe the invariants.
* **Protocol** — `crates/formless-protocol` is versioned; clients and daemon must be updated together.
* **Clients** — `crates/formless-tui` and `crates/formless`; the relay's web UI lives in `relay/`.

Release work is documented separately in [`packaging/README.md`](../packaging/README.md).

---

## Non-goals

* Hosted or multi-user mode, accounts, sharing servers.
* System-wide services, localhost-TCP transport, cloud sync or telemetry. Optional per-user login autostart is supported ([§3.6](usage.md#daemon-diagnostics-and-reset)).
* Publishing signed/notarised binaries and submitting the Homebrew/winget packages (the workflow, installer and templates are in [`packaging/`](../packaging/README.md); signing needs maintainer certificates).

v2 shipped what v1 left out: the encrypted Relay and Web frontend ([§3.7](usage.md#browser-access-via-the-relay-v2), [`relay/`](../relay/README.md)), the Responses API, prompt-based tool calling, failover and measured routing, OAuth providers ([§3.8](usage.md#provider-options-v2)), shared modules, sub-agents, prompt/memory self-optimisation, the Tool editor and the multi-pane TUI.

---
