# Formless

> **A general-purpose agent harness that boots with one bootstrap tool and grows its own capabilities for any use case.**

A fresh install of Formless exposes exactly one Tool: `create_tool`. Everything else — web access, file surgery, shell work, domain logic — is written on demand by the agent, persisted, versioned, measured and reused. Formless does not specialise in coding.

A long-lived **Daemon Core** owns the agent loop, the Tools, the Sessions and all state. The TUI and the CLI are thin, interchangeable clients of that core: they render streamed events and send intents, and they can crash or disconnect without losing work.

| Document | What it is |
|---|---|
| [SPEC.md](SPEC.md) | Normative specification (v1 plus v2 extensions) |
| [GLOSSARY.md](GLOSSARY.md) | Canonical vocabulary — read this first |
| [docs/adr/](docs/adr) | Architecture Decision Records |
| `Cargo.toml` | Rust workspace, edition 2021, Apache-2.0 |
| **§3 [Running Formless](#3-running-formless)** | **Prerequisites, build, onboarding, TUI keys, headless CLI, daemon, relay** |

---

## 1. Principles

1. **One tool at boot.** A fresh install exposes exactly `create_tool`. Everything else is built, persisted and reused. No starter Tools ship.
2. **Measured, never vibes.** Self-Optimisation is triggered and judged only by recorded Invocation outcomes. No silent or untested mutation, ever.
3. **No time-based killing.** Runs and Invocations have no step, token or time limits. The only terminators are completion, fault or the user.
4. **Deny by default.** Tools run sandboxed with only their declared Capabilities. The runtime offers nothing a Tool was not granted.
5. **Local-first, zero telemetry.** Nothing leaves the machine except calls to the configured provider endpoint.
6. **One core, many clients.** All behaviour lives in the Daemon Core. TUI, CLI and future frontends are interchangeable clients of one protocol.
7. **Everything is versioned and reversible.** Tools, promotions, config and Session checkpoints keep history; destructive actions are user-only and confirmed.

---

## 2. Architecture

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
                                    │ QuickJS sandbox │   │ OpenAI-        │
                                    │ one isolate per │   │ compatible     │
                                    │ Tool, gated by  │   │ provider       │
                                    │ Capabilities    │   │ (Chat Compl.)  │
                                    └─────────────────┘   └────────────────┘
```

* **One binary:** `formless`. The daemon is not a separate executable — it is `formless daemon` running in daemon mode, spawned automatically by clients. No system service on any OS.
* **One Daemon Core per data dir**, enforced by a lock file. `--profile <name>` runs an additional isolated daemon with its own data and config dirs.
* **Transport:** a Unix domain socket in the data dir (dir mode `0700`) on macOS/Linux, a named pipe with an owner-only ACL on Windows. No localhost-TCP fallback in v1.
* **Clients are stateless.** The daemon persists everything; killing the TUI never kills a Run.

### 2.1 Crate map

| Crate | Responsibility |
|---|---|
| `formless-core` | Daemon Core: agent loop, provider client, Session/Run management, Tool registry, metrics, optimiser, storage, compaction |
| `formless-runtime` | QuickJS sandbox (QuickJS-NG via `rquickjs`) and the Capability-gated host API, including streaming primitives |
| `formless-protocol` | Shared daemon↔client types: requests, streamed events, errors (versioned) |
| `formless-tui` | ratatui TUI client |
| `formless` | Binary entrypoint: CLI commands + default TUI launch |

---

## 3. Running Formless

### 3.1 Prerequisites

| Need | Why |
|---|---|
| A Rust stable toolchain (`rustup` recommended) | the app is one Rust workspace, edition 2021 |
| A C compiler (`cc` / `clang` / `gcc`) | the QuickJS sandbox (`rquickjs`) is compiled from source |
| An OpenAI-compatible Chat Completions endpoint that **supports tool calling** | it is the only provider Formless talks to |
| A real terminal | the default client is a full-screen TUI |

On Linux a Secret Service provider (GNOME Keyring, KWallet) keeps the API key in the OS keychain. If none is available, Formless falls back to a `0600` file in the config dir, so headless machines still work.

### 3.2 Build

```bash
git clone <this repo> && cd formless
cargo build --release          # binary at ./target/release/formless
```

`cargo run` (debug) and `cargo test --workspace` work the same way if you are working on the code.

### 3.3 First run: the onboarding wizard

```bash
./target/release/formless
```

With no arguments `formless` launches the **TUI**, which auto-starts the daemon on first use (`formless daemon serve`, detached, logging to `<data dir>/daemon.log`) and then walks the wizard:

1. **Base URL** — e.g. `https://api.openai.com/v1`, `http://localhost:11434/v1`, or any compatible gateway.
2. **API key** — stored in the OS keychain plus a `0600` fallback file (`api_key`) in the config dir, so rebuilding a dev binary never loses it.
3. **Model** — picked from the endpoint's model list, or typed by hand.
4. **Reasoning effort** — for models that accept it.
5. **Probe** — verifies connectivity, streaming and **tool calling**. If the endpoint cannot call tools the wizard refuses to continue, because the agent cannot build or run Tools without it.

The result is written to `config.toml` in the config dir (§4); you never need to edit it by hand.

### 3.4 Using the TUI

Type a task at the bottom and press **Enter**. Formless starts with only `create_tool` and builds whatever else the task needs.

| Key | Action |
|---|---|
| `Enter` | send the message, or steer the Run that is in flight |
| `Ctrl+Enter` | queue the message for after the current Run |
| `Esc` | stop the current Run / leave a dialog |
| `Tab` / `Shift+Tab` | cycle views (Chat, Sessions, Tools, Metrics, Proposals, Settings) — the bar at the top shows where you are |
| `Shift+E` (Tools view) | edit the selected Tool's JS (open it with `Enter` first); `Ctrl+S` saves it as a new version through the smoke gate |
| `PgUp` / `PgDn` | scroll the transcript |
| `Ctrl+C` | detach — the daemon keeps running, nothing is lost |

Slash commands: `/help /new /sessions /fork /search /edit /tools /metrics /proposals /propose /panes /settings /model /thinking /stop /quit`. On terminals at least 110 columns wide, `/panes` switches to a dashboard with Sessions and Tools beside the chat and Metrics on the right; the choice is remembered (`tui.multi_pane`). Reattaching later finds the same Sessions, because all state lives in the daemon, not in the client.

Both clients render assistant replies as GitHub-flavoured Markdown: headings, nested and task lists, tables (with column alignment), quotes and `> [!NOTE]`-style alerts, rules, links, strikethrough and `$math$`. Fenced code is syntax-highlighted (Rust, JS/TS, Python, Go, shell, C-family, JSON, YAML/TOML, SQL, HTML, CSS) and ```` ```diff ```` blocks are coloured. ```` ```mermaid ```` blocks become diagrams: the web app renders every Mermaid type; the TUI draws flowcharts, state diagrams, sequence diagrams and pie charts as box-drawing art and shows the source for other types, or when a diagram is wider than the pane.

### 3.5 Headless runs (no TUI)

```bash
./target/release/formless run "summarise the CHANGELOG of this repo"        # ephemeral Session
./target/release/formless run "now group those by author" --session 3f9c    # continue a Session
./target/release/formless run "audit my Downloads folder" --json            # one JSON event per line
./target/release/formless run "clean up the temp files" --yes               # auto-approve sensitive Tools
```

Approvals are not skipped by scripting: without `--yes` a sensitive Invocation **fails closed** with a clear error instead of being silently granted.

### 3.6 Daemon, diagnostics and reset

```bash
./target/release/formless daemon start     # start it if it is not already up
./target/release/formless daemon status    # pid, protocol, session count
./target/release/formless daemon stop
./target/release/formless daemon serve     # foreground, for watching logs
./target/release/formless doctor           # config, dirs, keychain, endpoint + tool-calling probe
./target/release/formless tools list       # registry inspection (also export / import)
./target/release/formless reset --force    # destructive: config, key, whole data dir
```

There is one daemon per profile, reachable over a unix socket (`<data dir>/daemon.sock`) or a named pipe on Windows. It exits on its own after `daemon.idle_grace_secs` (default 60 s) with no clients and no pending Invocations, and the next `formless` command starts it again — Sessions live in SQLite (`formless.db`), so nothing is lost.

### 3.7 Browser access via the relay (v2)

```bash
./target/release/formless relay enable     # prints https://…#k=… — open that URL
./target/release/formless relay status     # show it again
./target/release/formless relay disable --forget
```

The daemon dials out to the relay, so nothing needs an inbound port; the browser holds the pairing key in the URL fragment and the relay only ever forwards AES-256-GCM ciphertext. The web app matches the TUI feature for feature (Sessions, chat with steer/queue/stop/edit, approvals, Tools with a code editor, Metrics, Proposals, Settings) and works on phones. The default endpoint is the Worker in [`relay/`](relay/README.md) — self-host it and point `relay.url` at your own deployment; `node relay/dev.mjs --mock-provider` runs the whole thing locally for frontend work.

### 3.8 Provider options (v2)

All of these are plain config in `[provider]`; none is a vendor-specific code path.

| Setting | Effect |
|---|---|
| `api_style = "responses"` | speak the Responses API (`POST {base_url}/responses`) instead of Chat Completions |
| `tool_mode = "prompt"` | describe Tools in the prompt and parse a JSON tool call from the reply, for models without native tool calling (onboarding detects this) |
| `fallback_models = [...]` | models to fail over to, in order, when the selected one errors |
| `model = "auto"` + `routing_models = [...]` | pick a model per Run from **measured** local success and latency; `routing_fast_models` / `routing_strong_models` split simple from complex tasks |

Subscription providers sign in with OAuth2 + PKCE instead of an API key:

```toml
[provider.auth]
mode = "oauth"
authorize_url = "https://provider.example/oauth/authorize"
token_url = "https://provider.example/oauth/token"
client_id = "your-public-client-id"
```

```bash
./target/release/formless auth login     # opens the browser, listens on 127.0.0.1 for the callback
./target/release/formless auth status    # mode and token expiry, never the token itself
./target/release/formless auth logout
```

Tokens are kept in the keychain (plus a `0600` fallback file) and refreshed automatically before each provider call.

### 3.9 Profiles and environment variables

`--profile <name>` is a global flag: it gives an isolated daemon, data dir and config dir, which is the safe way to try dangerous Tools.

```bash
./target/release/formless --profile sandbox
```

| Variable | Effect |
|---|---|
| `FORMLESS_API_KEY` | provider key; wins over the keychain and fallback file |
| `FORMLESS_OAUTH_ACCESS_TOKEN` | OAuth mode: use this access token instead of stored tokens |
| `FORMLESS_OAUTH_CLIENT_SECRET` | OAuth mode: client secret for providers that need one (never stored in config) |
| `FORMLESS_BASE_URL`, `FORMLESS_MODEL` | override the configured endpoint/model for this process |
| `FORMLESS_DATA_DIR`, `FORMLESS_CONFIG_DIR` | relocate state (also how the auto-started daemon inherits them) |
| `FORMLESS_IDLE_GRACE_SECS` | how long an idle daemon lingers before exiting |
| `FORMLESS_RELAY_SECRET` | pairing secret override (the `#k=` part of the browser URL) |
| `RUST_LOG=debug` | verbose tracing on stderr |

### 3.10 Troubleshooting

| Symptom | Fix |
|---|---|
| `not configured; run formless for the wizard` | headless runs need a configured provider first — launch the TUI once |
| "does not support tool-calling" during onboarding | pick another model or endpoint; tool calling is mandatory |
| TUI renders garbage or does not respond | it needs a real TTY; run it interactively rather than through a pipe |
| Daemon will not start | read `<data dir>/daemon.log`, then `formless doctor` and `formless daemon status` |
| Keychain prompt on every launch | the `0600` `api_key` file in the config dir exists exactly for this; check it is present |
| Everything is wedged | `formless reset --force` (destroys config, key and data dir) |

### 3.11 CLI reference

| Command | Behaviour |
|---|---|
| `formless` (or `formless tui`) | Launch the TUI, auto-starting the daemon |
| `formless run "task"` | Headless Run in an ephemeral Session; `--session <id>` to continue one; `--json` for one JSON event per line; `--yes` to auto-approve sensitive Invocations for that Run |
| `formless daemon start\|stop\|status\|serve` | Daemon lifecycle |
| `formless relay enable\|disable\|status` | Encrypted browser access (v2) |
| `formless tools list\|export\|import` | Registry inspection and transfer (no secrets, no metrics) |
| `formless auth login\|status\|logout` | OAuth sign-in for subscription providers (v2) |
| `formless prompts list\|generate\|apply\|reject\|rollback` | Measured prompt/memory proposals (v2) |
| `formless update check` | Compare with the latest GitHub release; never modifies the binary |
| `formless doctor` | Config validity, data dir, keychain reachability, endpoint + tool-calling probe |
| `formless reset [--force]` | Full wipe of config, keychain entry and data dir |

Headless approvals **fail closed**: an unapproved sensitive Invocation fails with a clear error unless `--yes` is passed. Exit codes: `0` answered, `1` Run errored/paused, `2` usage/config error, `130` interrupted.

---

## 4. Data, config and profiles

* **Data dir:** `~/Library/Application Support/formless` (macOS), `$XDG_DATA_HOME/formless` (Linux), `%APPDATA%\formless` (Windows). Config lives in the matching config dir (`config.toml` plus the OS keychain for secrets).
* Contents: the SQLite DB (Sessions, Runs, Tool metadata and version index, Invocations, metric tallies, compaction checkpoints, approvals, audit events), Tool version files, artifacts, rotated daemon logs and the audit log.
* The schema is versioned; the daemon auto-migrates on start and **takes a backup before migrating**. On corruption it refuses to guess: it stops, reports, and points at the backup.
* `--profile <name>` gives an isolated daemon plus data/config dirs — intended for testing dangerous Tools.

---

## 5. The Tool system

### 5.1 `create_tool` — the Bootstrap Tool

```
create_tool(name, description, input_schema, output_schema,
            implementation_js, capabilities[], memory_mb?)
```

* The **agent writes the JavaScript**: plain JS, one self-contained file, no TypeScript, no imports.
* **Validation gate at creation:** name rules, both schemas validated against JSON Schema draft 2020-12, JS syntax-checked, then **smoke-tested** in the replay-style sandbox with empty input. Any failure rejects creation and nothing broken is saved.
* `create_tool` is immutable, excluded from Self-Optimisation, and cannot be retired or widened.
* The smoke test is conclusive only until the Tool first meets a stub (fake fetch body, unbindable `$input` scope, …); a failure after that is reported as *inconclusive* and the Tool is saved.

The other built-ins are the minimum the agent needs to grow and maintain its own registry:

| Built-in | Purpose |
|---|---|
| `find_tools(query)` | Keyword search over the registry; matches become callable in this Session |
| `read_tool(name, version?)` | Full code, schemas, Capabilities, versions, metrics and recent failures |
| `test_tool(input, name \| draft)` | Dry-run a saved Tool or an unsaved draft on real input; not saved, not measured |
| `create_module`, `retire_tool` | Shared JS modules, retirement |

Sub-agents are not a built-in: the agent builds a Tool with the `agent` Capability (`function run(i){ return agents.run(i.task); }`), so delegation is a measured, approval-gated Invocation. Several such calls in one turn run in parallel, and nesting is capped by `tools.max_agent_depth` (default 1).

Up to `tools.inline_limit` (default 40) active Tools, every Session lists them all. Beyond that a Session lists the Tools it has loaded plus the most-used ones, and reaches the rest with `find_tools`, so the registry can grow without bound.

### 5.2 Lifecycle and versioning

* Tools are **never edited in place**. Any change creates a new integer version (`v1`, `v2`, …; no semver), recording timestamp, author (`agent` | `optimiser` | `import`) and, at promotion, the stats of the version it replaced.
* A running Invocation always finishes on the version it started with; new Invocations use the current version.
* **Retire** (agent): instant and global — the Tool becomes uncallable everywhere, data is retained, and it is restorable from the Tool browser. **Purge** (permanent deletion of code, history and tallies) is user-only, from the TUI.
* Name collisions at creation are serialised by the daemon: the second creator gets the existing Tool as a new version instead of a fork.

### 5.3 Capabilities (closed taxonomy)

| Capability | Scoping | Notes |
|---|---|---|
| `net` | optionally per-domain | Unscoped `net` is allowed but flagged broad at approval |
| `fs:read` | path list | Canonicalised; symlink escapes denied |
| `fs:write` | path list | Same containment rules |
| `env` | named keys | Only the listed keys are visible |
| `process` | named commands | Executed without a shell; unscoped is flagged broad |
| `clipboard:read` / `clipboard:write` | boolean | v2; desktop clipboard access |
| `notify` | boolean | v2; desktop notification |
| `llm` | optionally per-model | v2; `llm.complete()` from inside a Tool |
| `agent` | optionally per-model | v2; `agents.run()` sub-agents from inside a Tool |

`memory_mb` (default 256, daemon max default 4 GB) is declared and governed like a Capability: shown at approval, and widening it re-prompts. A Capability the runtime cannot enforce is never offered — the taxonomy is closed and grows only with the runtime.

### 5.4 The host API inside a Tool

QuickJS-NG runs in-process. Bare JS has no ambient authority: the daemon provides the whole surface, and every entry is gated by a declared Capability.

| API | Gated by | Purpose |
|---|---|---|
| `fetch(url, {method, headers, body, bodyEncoding, encoding})` → `{status, ok, url, headers, body, text(), json()}` | `net` | HTTP; redirects followed, every hop scope-checked |
| `fetchChunks(url, opts, size, cb)` | `net` | Stream arbitrarily large bodies in bounded memory |
| `fs.read(path, {encoding})` / `fs.write` / `fs.append` | `fs:read` / `fs:write` | Whole-value access for small data |
| `fs.exists` / `fs.stat` / `fs.list(dir, {details})` | `fs:read` | Existence, metadata, directory listing |
| `fs.mkdir` / `fs.remove(path, {recursive})` / `fs.rename(from, to)` | `fs:write` | Tree changes |
| `fs.readChunks(path, size, cb, {encoding})` / `fs.readLines(path, cb)` | `fs:read` | Streaming reads — never load large data whole |
| `env.get(key)` | `env` | Read one declared key |
| `process.run(cmd, args, {cwd, env, stdin, stdinEncoding, encoding})` → `{code, signal, stdout, stderr}` | `process` | Execute a declared binary, no shell |
| `tools.call(name, input)` | — | Composition (nested Invocations are recorded and measured) |
| `clipboard.read()` / `clipboard.write(text)` | `clipboard:read` / `clipboard:write` | v2; OS clipboard |
| `notify(title, body)` | `notify` | v2; desktop notification |
| `llm.complete(prompt, {system, model})` | `llm` | v2; one non-streaming completion via the configured provider |
| `agents.run(task, {system, model})` | `agent` | v2; sub-agent over the same Tools (depth-limited, no step limit), returns its final text |
| `requireModule(name)` | — | v2; load a shared module created with `create_module` |
| `emit(chunk)` | — | Stream large output to an artifact file |
| `console.log`, `sleep`, `setTimeout`, `JSON`, `cryptoRandom()`, `randomBytes(n)` | — | The rest of the surface — nothing else exists |

The surface is deliberately near-minimal: it holds only effects JS cannot produce itself. Every data option takes `encoding: "utf8"` (default) or `"base64"`, so Tools can move any binary data. Anything higher-level (URL parsing, hashing, compression, file formats) the agent writes itself, usually as a shared module. `run` may be `async`. A Tool reports failure by **throwing**, and a returned `{error: …}` is counted as a failure anyway, so swallowed errors never inflate the success rate.

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

### 5.5 Composition rules

* `tools.call` records and measures nested Invocations exactly like top-level ones.
* Composition cannot launder permissions: the called Tool runs under **its own** declared Capabilities, and the outer Tool's Capabilities also apply. Depth is bounded and cycle-safe.
* Shared code lives in versioned, immutable modules (`create_module`, loaded with `requireModule`); module loading is smoke-tested when a Tool is created.
* In smoke tests the v2 side-effecting APIs are stubbed; in optimiser replay they are denied.

---

## 6. Invocations: async by design

* Every Tool call is an **Invocation**: one Tool version, one input, recorded and measured.
* Past a soft threshold (default 300 s) the Invocation becomes a **Pending Invocation** — still running, with a handle the agent holds (`check_invocation`, `await_invocation`, `abandon_invocation`) instead of a timeout killing it.
* Outcomes are tagged distinctly: success, error, `resource` (JS heap cap hit), `user-cancelled`, `abandoned`. User-cancelled and malformed model tool-calls never masquerade as Tool bugs, so the optimiser cannot fix what the user interrupted.
* Stalled provider requests follow the same pending pattern.

---

## 7. Sessions, Runs and Steer

* **Session** — a resumable conversation plus Run history; many run concurrently against one Daemon Core. Deleting a Session removes payloads but keeps anonymised tallies.
* **Run** — one unit of work: user message → final answer. No step, token or time limit; it ends on completion, fault or interruption.
* **Steer** — a user message sent while a Run is active, injected at the next step boundary.
* **Compaction** — a persisted summary checkpoint that replaces old history and Tool output as context grows, so long Sessions keep going without losing the trail.
* **Artifacts** — large Tool output is written to an artifact file; only a bounded preview enters model context.

---

## 8. Life of a Run

1. A client connects to the daemon socket and sends a Run intent; the daemon creates or reuses the Session.
2. The agent loop sends the conversation plus every active Tool's schema to the provider.
3. The model either answers or calls a Tool. A call that needs a broad Capability pauses for approval.
4. The daemon executes the **Invocation** inside the QuickJS sandbox, limited to the Tool's declared Capabilities and `memory_mb`; large output streams into an artifact.
5. The outcome is recorded and folded into the Tool's metrics. The loop repeats until the model produces a final answer.
6. Answers stream to the client. Steers are injected at step boundaries; if the client dies, the Run keeps running and can be reattached or resumed.
7. Offline, when a Tool's success rate drops below the bar, a Candidate version is generated and must win a sandboxed replay before it replaces the current one.

---

## 9. Self-Optimisation

Measured per Tool, pooled across all Sessions:

| Metric | Role |
|---|---|
| **Tool Success Rate** | Primary trigger: share of Invocations that finish without runtime error, not user-cancelled, with output validating against the output schema. No time term. |
| Retry Rate | Hidden-failure signal: successful Invocations immediately retried or rewritten |
| Latency p50/p95, Cost per Success | Wall time and provider tokens per successful Invocation |
| Validation Failure Rate | Schema rejections — points at a badly described Tool |

The loop:

1. **Trigger** — success rate below **90%** over the last **≥ 20** Invocations (rolling window of 100).
2. **Candidate** — a background optimiser job queues and only runs when no active Run is waiting on that Tool, so it never interrupts a Session.
3. **Sandboxed replay (the gate)** — the replay set is *all recorded failed Invocations plus an equal-sized sample of recorded successes* (a regression guard). It runs in a throwaway copy-on-write overlay: writes are discarded, network is denied by default (recorded responses are served where available), `process` is denied, and no real secrets are injected. A Tool that genuinely needs live network is marked **replay-limited** and can only be promoted with explicit user approval.
4. **Promotion bar** — the Candidate must score **≥ 10 points above** the current version on the full replay set, **with zero regressions** on previously successful replays. Otherwise it is discarded and logged in the Tool's history.
5. **Promotion and rollback** — atomic: running Invocations finish on their starting version, later ones use the promoted version. Every version is kept with one-action rollback in the Tool browser. Promotions, rollbacks, discards and needs-attention flags are audit-logged.

Only the primary metric triggers; the rest inform the optimiser's context.

**Prompt and memory (v2).** The optimiser can also propose one short guidance or memory addendum to the system prompt. Each proposal is scored against the current prompt on a fixed eval suite and can be applied only if it wins by the same promotion margin, and only by the user (`formless prompts`, TUI `/proposals`). Applied addenda are versioned and roll back individually; the bootstrap prompt itself is never edited.

---

## 10. Security and privacy

* **Deny by default.** A Capability is a ceiling the runtime enforces; it is never exceeded.
* **Approvals.** Broad or sensitive Capabilities prompt before first use — modal in the TUI, fail-closed in headless mode.
* **Single-user trust boundary.** Local access control is the socket/pipe file permissions on the data dir (`0700`).
* **Local-first.** Zero telemetry, no update checker, no phone-home: nothing leaves the machine except calls to the configured provider endpoint.
* **Imports are never auto-trusted.** Capabilities are displayed, approval applies, metrics start at zero, and the author is recorded as `import`.
* **Data lifecycle.** `formless reset` wipes config, keychain entry and data dir; Session deletion removes payloads but keeps anonymised tallies; only a Tool purge deletes tallies.

---

## 11. Repository layout

```
formless/
├── SPEC.md              normative v1 specification — behaviour lives here
├── GLOSSARY.md          canonical vocabulary (Tool, Run, Invocation, Steer, …)
├── docs/adr/            ADRs: daemon core, single bootstrap tool,
│                        QuickJS tool runtime, success-rate-gated optimisation
└── crates/
    ├── formless/        binary entrypoint: CLI subcommands, default TUI launch
    ├── formless-core/   daemon, agent loop, provider client, tools, storage, optimiser
    ├── formless-runtime/ QuickJS sandbox + Capability-gated host API
    ├── formless-protocol/ shared daemon ↔ client types
    └── formless-tui/    ratatui client
```

---

## 12. Development

```bash
cargo build --workspace                # debug build of the single binary
cargo test --workspace                 # unit + integration tests
cargo fmt --all                        # formatting
cargo clippy --workspace --all-targets # lints

RUST_LOG=debug cargo run -- daemon status
RUST_LOG=debug cargo run -- run "write a tool that converts a CSV to JSON"
```

Rules of the road: **no behaviour may live only in a client** — the CLI and TUI are both clients of the daemon protocol. Every Tool is validated and smoke-tested at creation, and every behavioural change should be reflected in `SPEC.md` first.

---

## 13. Non-goals

* Hosted or multi-user mode, accounts, sharing servers.
* System services, localhost-TCP transport, cloud sync or telemetry.
* Publishing signed/notarised binaries and submitting the Homebrew/winget packages (the workflow, installer and templates are in [`packaging/`](packaging/README.md); signing needs maintainer certificates).

v2 shipped what v1 left out: the encrypted Relay and Web frontend (§3.7, [`relay/`](relay/README.md)), the Responses API, prompt-based tool calling, failover and measured routing, OAuth providers (§3.8), shared modules, sub-agents, prompt/memory self-optimisation, the Tool editor and the multi-pane TUI.

---

## Further reading

1. [GLOSSARY.md](GLOSSARY.md) — the exact words (Tool, Run, Invocation, Candidate, Promotion, Compaction).
2. [SPEC.md](SPEC.md) §1–3 — principles, architecture, concepts.
3. [SPEC.md](SPEC.md) §7 — the Tool system and host API.
4. [SPEC.md](SPEC.md) §8 — metrics and Self-Optimisation.
5. [docs/adr/](docs/adr) — why the core looks the way it does.
