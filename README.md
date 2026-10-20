# Formless

> **A general-purpose agent harness that boots with one bootstrap tool and grows its own capabilities for any use case.**

A fresh install of Formless exposes exactly one Tool: `create_tool`. Everything else — web access, file surgery, shell work, domain logic — is written on demand by the agent, persisted, versioned, measured and reused. Formless does not specialise in coding.

A long-lived **Daemon Core** owns the agent loop, the Tools, the Sessions and all state. The TUI and the CLI are thin, interchangeable clients of that core: they render streamed events and send intents, and they can crash or disconnect without losing work.

| Document | What it is |
|---|---|
| [SPEC.md](SPEC.md) | Normative v1 specification |
| [GLOSSARY.md](GLOSSARY.md) | Canonical vocabulary — read this first |
| [docs/adr/](docs/adr) | Architecture Decision Records |
| `Cargo.toml` | Rust workspace, edition 2021, Apache-2.0 |

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

## 3. Quick start

```bash
# build
cargo build --release

# launch the TUI (auto-starts the daemon on first run + onboarding wizard)
./target/release/formless

# headless one-shot run (no TUI)
./target/release/formless run "summarise the CHANGELOG of this repo"

# continue an existing session, machine-readable event stream
./target/release/formless run "now group those by author" --session 3f9c --json

# daemon control and diagnostics
./target/release/formless daemon status
./target/release/formless doctor
```

First run walks through an onboarding wizard: pick an OpenAI-compatible base URL, model and API key (stored in the OS keychain), then pass an endpoint probe that verifies both connectivity and tool-calling support.

### 3.1 CLI reference (v1)

| Command | Behaviour |
|---|---|
| `formless` | Launch the TUI, auto-starting the daemon |
| `formless run "task"` | Headless Run in an ephemeral Session; `--session <id>` to continue one; `--json` for one JSON event per line; `--yes` to auto-approve sensitive Invocations for that Run |
| `formless daemon start\|stop\|status` | Daemon lifecycle |
| `formless tools list\|export\|import` | Registry inspection and transfer (no secrets, no metrics) |
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
* Every Session exposes `create_tool` **plus every active Tool** directly as a callable function. There is no catalog-search step.

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

`memory_mb` (default 256, daemon max default 4 GB) is declared and governed like a Capability: shown at approval, and widening it re-prompts. A Capability the runtime cannot enforce is never offered — there is no clipboard or notification permission in v1.

### 5.4 The host API inside a Tool

QuickJS-NG runs in-process. Bare JS has no ambient authority: the daemon provides the whole surface, and every entry is gated by a declared Capability.

| API | Gated by | Purpose |
|---|---|---|
| `fetch(url, opts)` → `{status, ok, body, text(), json()}` | `net` | HTTP with streamed response bodies |
| `fetchChunks(url, opts, size, cb)` | `net` | Download arbitrarily large bodies in bounded memory |
| `fs.read(path)` / `fs.write(path, string)` | `fs:read` / `fs:write` | Whole-value access for small data |
| `fs.exists(path)` / `fs.list(dir)` | `fs:read` | Existence and directory listing |
| `fs.readChunks(path, size, cb)` / `fs.readLines(path, cb)` | `fs:read` | Streaming reads — never load large data whole |
| `env.get(key)` | `env` | Read one declared key |
| `process.run(cmd, argsArray)` → `{code, stdout, stderr}` | `process` | Execute a declared binary, no shell |
| `tools.call(name, input)` | — | Composition (nested Invocations are recorded and measured) |
| `emit(chunk)` | — | Stream large output to an artifact file |
| `console.log`, `sleep`, `setTimeout`, `JSON`, `cryptoRandom()` | — | The rest of the surface — nothing else exists |

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
* Out of v1: shared JS modules (every Tool is one file — duplication is accepted so versioning and retirement stay atomic) and LLM calls from inside Tools (only the agent loop talks to the provider).

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

## 13. Non-goals in v1

* Hosted web frontend and the encrypted Relay (v2 direction).
* Any provider that is not OpenAI-compatible Chat Completions.
* Shared JS modules or libraries between Tools.
* LLM calls from inside Tools — only the agent loop talks to the provider.
* Clipboard or notification capabilities, and any Capability the runtime cannot enforce.
* System services, localhost-TCP transport, cloud sync or telemetry.

---

## Further reading

1. [GLOSSARY.md](GLOSSARY.md) — the exact words (Tool, Run, Invocation, Candidate, Promotion, Compaction).
2. [SPEC.md](SPEC.md) §1–3 — principles, architecture, concepts.
3. [SPEC.md](SPEC.md) §7 — the Tool system and host API.
4. [SPEC.md](SPEC.md) §8 — metrics and Self-Optimisation.
5. [docs/adr/](docs/adr) — why the core looks the way it does.
