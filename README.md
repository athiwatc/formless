<a id="readme-top"></a>

<div align="center">

<h1>Formless</h1>

<p><strong>A general-purpose agent harness that boots with one bootstrap tool and grows its own capabilities for any use case.</strong></p>

<p>
  <a href="https://github.com/athiwatc/formless/blob/main/Cargo.toml"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License: Apache-2.0"></a>
  <a href="https://github.com/athiwatc/formless/releases/latest"><img src="https://img.shields.io/github/v/release/athiwatc/formless?sort=semver" alt="Latest release"></a>
  <a href="#32-install"><img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey.svg" alt="Platforms: macOS, Linux, Windows"></a>
  <a href="#10-security-and-privacy"><img src="https://img.shields.io/badge/telemetry-none-success.svg" alt="Telemetry: none"></a>
</p>

<p>
  <a href="https://formless.athiwat.xyz"><strong>Explore the docs »</strong></a>
  <br />
  <br />
  <a href="https://github.com/athiwatc/formless/issues/new?labels=bug">Report Bug</a>
  &middot;
  <a href="https://github.com/athiwatc/formless/issues/new?labels=enhancement">Request Feature</a>
</p>

</div>

## About The Project

A fresh install of Formless exposes exactly one Tool: `create_tool`. Everything else — web access, file surgery, shell work, domain logic — is written on demand by the agent, persisted, versioned, measured and reused. Formless does not specialise in coding.

A long-lived **Daemon Core** owns the agent loop, the Tools, the Sessions and all state. The TUI and the CLI are thin, interchangeable clients of that core: they render streamed events and send intents, and they can crash or disconnect without losing work.

## Quick start

```bash
# macOS / Linux — prebuilt binary from the latest GitHub release, checksum-verified
curl -fsSL https://raw.githubusercontent.com/athiwatc/formless/main/scripts/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"

formless          # first launch starts the daemon and walks the onboarding wizard
```

Then type a task and press **Enter**. Prerequisites, the other install paths (source build, Homebrew formula, Windows), headless use and every CLI subcommand are in [§3](#3-running-formless).

## What a session looks like

```text
user   how many TODOs are left in this repo, and who touched them last?

agent  ▸ find_tools("search files")        → nothing in the registry yet
       ▸ create_tool("search_repo")        → v1 saved, capabilities: fs:read:$input.root
       ▸ search_repo({pattern: "TODO"})    → 12 hits in 7 files (success rate 100%)
       ▸ create_tool("git_last_author")    → v1 saved, capabilities: process:git
       ▸ git_last_author({paths: […]})     → 7 authors
       There are 12 TODOs across 7 files. The newest is yours, from this morning.
```

Illustrative, not scripted: the Tools an agent builds are whatever the task needs, and they stay in the registry for the next Session — measured, versioned and retireable.

| Document | What it is |
|---|---|
| [SPEC.md](SPEC.md) | Normative specification (v1 plus v2 extensions) |
| [GLOSSARY.md](GLOSSARY.md) | Canonical vocabulary — read this first |
| [docs/adr/](docs/adr) | Architecture Decision Records |
| [relay/README.md](relay/README.md) | The Worker that powers browser access |
| [packaging/README.md](packaging/README.md) | Release process, installer, Homebrew/winget templates |
| `Cargo.toml` | Rust workspace, edition 2021, Apache-2.0 |

---

## Table of Contents

<details open>
  <summary>Expand</summary>

  <ol>
    <li><a href="#about-the-project">About The Project</a></li>
    <li><a href="#quick-start">Quick start</a></li>
    <li><a href="#what-a-session-looks-like">What a session looks like</a></li>
    <li><a href="#1-principles">Principles</a></li>
    <li>
      <a href="#2-architecture">Architecture</a>
      <ul>
        <li><a href="#21-crate-map">Crate map</a></li>
        <li><a href="#22-built-with">Built with</a></li>
      </ul>
    </li>
    <li>
      <a href="#3-running-formless">Running Formless</a>
      <ul>
        <li><a href="#31-requirements">Requirements</a></li>
        <li><a href="#32-install">Install</a></li>
        <li><a href="#33-first-run-the-onboarding-wizard">First run: the onboarding wizard</a></li>
        <li><a href="#34-using-the-tui">Using the TUI</a></li>
        <li><a href="#35-headless-runs-no-tui">Headless runs (no TUI)</a></li>
        <li><a href="#36-daemon-diagnostics-and-reset">Daemon, diagnostics and reset</a></li>
        <li><a href="#37-browser-access-via-the-relay-v2">Browser access via the relay (v2)</a></li>
        <li><a href="#38-provider-options-v2">Provider options (v2)</a></li>
        <li><a href="#39-profiles-and-environment-variables">Profiles and environment variables</a></li>
        <li><a href="#310-troubleshooting">Troubleshooting</a></li>
        <li><a href="#311-cli-reference">CLI reference</a></li>
        <li><a href="#312-updating">Updating</a></li>
      </ul>
    </li>
    <li><a href="#4-data-config-and-profiles">Data, config and profiles</a></li>
    <li>
      <a href="#5-the-tool-system">The Tool system</a>
      <ul>
        <li><a href="#51-create_tool--the-bootstrap-tool"><code>create_tool</code> — the Bootstrap Tool</a></li>
        <li><a href="#52-lifecycle-and-versioning">Lifecycle and versioning</a></li>
        <li><a href="#53-capabilities-closed-taxonomy">Capabilities (closed taxonomy)</a></li>
        <li><a href="#54-the-host-api-inside-a-tool">The host API inside a Tool</a></li>
        <li><a href="#55-composition-rules">Composition rules</a></li>
      </ul>
    </li>
    <li><a href="#6-invocations-async-by-design">Invocations: async by design</a></li>
    <li><a href="#7-sessions-runs-and-steer">Sessions, Runs and Steer</a></li>
    <li><a href="#8-life-of-a-run">Life of a Run</a></li>
    <li><a href="#9-self-optimisation">Self-Optimisation</a></li>
    <li><a href="#10-security-and-privacy">Security and privacy</a></li>
    <li><a href="#11-repository-layout">Repository layout</a></li>
    <li><a href="#12-development">Development</a></li>
    <li><a href="#13-non-goals">Non-goals</a></li>
    <li><a href="#roadmap">Roadmap</a></li>
    <li><a href="#contributing">Contributing</a></li>
    <li><a href="#further-reading">Further reading</a></li>
    <li><a href="#license">License</a></li>
    <li><a href="#contact">Contact</a></li>
    <li><a href="#acknowledgments">Acknowledgments</a></li>
  </ol>
</details>

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

### 2.2 Built with

| Layer | Choice |
|---|---|
| Language | Rust, edition 2021, one workspace |
| Terminal UI | [ratatui](https://ratatui.rs/) |
| Tool sandbox | [QuickJS-NG](https://quickjs-ng.github.io/) via [`rquickjs`](https://github.com/DelSkayn/rquickjs) |
| Async runtime | [Tokio](https://tokio.rs/) |
| Local storage | SQLite via [`rusqlite`](https://github.com/rusqlite/rusqlite) |
| HTTP and streaming | [`reqwest`](https://docs.rs/reqwest) with rustls |
| Local session naming | [Candle](https://github.com/huggingface/candle) with [LFM2.5-230M](https://huggingface.co/LiquidAI/LFM2.5-230M) |

---

## 3. Running Formless

### 3.1 Requirements

| Need | Why |
|---|---|
| A Rust stable toolchain (`rustup` recommended) — **source builds only** | the app is one Rust workspace, edition 2021 |
| A C compiler (`cc` / `clang` / `gcc`) — **source builds only** | the QuickJS sandbox (`rquickjs`) is compiled from source |
| An OpenAI-compatible endpoint that **supports tool calling** | it is the only provider Formless talks to |
| A real terminal | the default client is a full-screen TUI |

Prebuilt binaries need none of the toolchain requirements. On Linux a Secret Service provider (GNOME Keyring, KWallet) keeps the API key in the OS keychain; if none is available, Formless falls back to a `0600` file in the config dir, so headless machines still work.

### 3.2 Install

**Prebuilt binary (macOS, Linux).** The installer resolves the latest GitHub release for your platform, downloads it, verifies the `.sha256` and puts `formless` in `$PREFIX/bin` (default `~/.local`):

```bash
curl -fsSL https://raw.githubusercontent.com/athiwatc/formless/main/scripts/install.sh | sh

# overrides: PREFIX=/usr/local, FORMLESS_VERSION=v0.1.0, FORMLESS_FROM_SOURCE=1
```

**From source** (any platform with a toolchain):

```bash
git clone https://github.com/athiwatc/formless && cd formless
cargo build --release          # binary at ./target/release/formless
```

`cargo run` (debug) and `cargo test --workspace` work the same way if you are working on the code. You can also `cargo install --git https://github.com/athiwatc/formless --path crates/formless --locked`.

**Homebrew / winget.** A source formula ships at [`packaging/homebrew/formless.rb`](packaging/homebrew/formless.rb) and winget templates at [`packaging/winget/`](packaging/winget); neither is published yet, so for now use the installer or a source build. **Windows:** download the release archive and unzip it, or build from source.

> [!NOTE]
> Release artifacts are checksummed but unsigned. On macOS you may need `xattr -d com.apple.quarantine formless` after installing by hand. `formless update check` reports newer releases but never rewrites the installed binary — updates stay with the installer and checksum verification ([§3.12](#312-updating)).

### 3.3 First run: the onboarding wizard

```bash
./target/release/formless
```

With no arguments `formless` launches the **TUI**, which auto-starts the daemon on first use (`formless daemon serve`, detached, logging to `<data dir>/daemon.log`) and then walks the wizard:

1. **Provider** — pick a preset (OpenAI, Anthropic, OpenRouter, Gemini, Groq, Ollama, LM Studio) or enter any OpenAI-compatible base URL. Local presets skip the key step, and a key in `OPENAI_API_KEY` (etc.) is offered automatically. Add more later with `formless providers add NAME --preset openrouter`.
2. **API key** — stored in the OS keychain plus a `0600` fallback file (`api_key`) in the config dir, so rebuilding a dev binary never loses it.
3. **Model** — picked from the endpoint's model list, or typed by hand.
4. **Reasoning effort** — for models that accept it.
5. **Probe** — verifies connectivity, streaming and **tool calling**. If the endpoint cannot call tools the wizard refuses to continue: every Run would fail, and a prompt-mode fallback has to be chosen deliberately ([§3.8](#38-provider-options-v2)).

The result is written to `config.toml` in the config dir ([§4](#4-data-config-and-profiles)); you never need to edit it by hand. Re-run the wizard at any time with `formless doctor` for a diagnosis, or delete the config to go through it again.

### 3.4 Using the TUI

Type a task at the bottom and press **Enter**. Formless starts with only `create_tool` and builds whatever else the task needs.

| Key | Action |
|---|---|
| `Enter` | send the message, or steer the Run that is in flight |
| `/copy`, `/copy code [n]` | copy the last answer or one of its code blocks (OSC 52, works over SSH) |
| `/mouse` | release the mouse so you can select text with the terminal (again: wheel scrolling back) |
| `Ctrl+J` (or `Ctrl+Enter` / `Alt+Enter`) | queue the message for after the current Run; `Ctrl+Enter` needs a terminal with the kitty keyboard protocol (kitty, WezTerm, Ghostty, recent iTerm2) |
| `Esc` | stop the current Run / leave a dialog |
| `Tab` / `Shift+Tab` | cycle views (Chat, Sessions, Tools, Metrics, Proposals, Settings) — the bar at the top shows where you are |
| `Shift+E` (Tools view) | edit the selected Tool's JS (open it with `Enter` first); `Ctrl+S` saves it as a new version through the smoke gate |
| `PgUp` / `PgDn` | scroll the transcript |
| `↑` / `↓` | earlier messages of this Session (on the input's first/last line); `Ctrl+R` searches them |
| `Shift+Enter` or `\` then `Enter` | new line in the message |
| `←` `→` `Home` `End`, `Ctrl+A/E/W/U/K`, `Alt+B/F/D` | edit the message (readline keys) |
| `Ctrl+C` | clear the typed message; press again within 2 s to detach (the daemon keeps running, nothing is lost) |

Slash commands: `/help /new /sessions /fork /search /edit /tools /metrics /proposals /propose /panes /settings /model [ID] /thinking /stop /cancel /resend /resume /abandon /agents /quit`. Type `/` for a command palette (fuzzy filter, Tab completes, Enter runs); `/help` or `?` on an empty input lists every command grouped by area. Ctrl+V or `/paste` attaches a screenshot from the clipboard, and dragging a file into the terminal attaches it. Type `@` to reference files: a popup completes paths under the current directory (respecting `.gitignore`), and on send small files are attached while large files and directories go as a reference the agent reads with its Tools. `/model` opens a picker: type to filter (fuzzy, so `son4` finds `claude-sonnet-4`), Tab changes reasoning effort, Enter switches the current Session, Ctrl+Enter / Alt+Enter / Ctrl+D sets the global default. `/agents` lists the Session's sub-agents (started by `agents.run`) and `/agents <n>` shows one's stored transcript; their steps also show live as activity. `/cancel` stops just the running Tool the daemon flagged as pending or stalled, without stopping the Run. `/resend` aborts and re-sends a stalled provider request (clients flag a stream that has sent nothing for `provider.stall_notice_secs`, default 120). `/resume` continues a Session whose last Run was interrupted (crash, Esc) or paused (provider error) from its transcript; `/abandon` drops it. On terminals at least 110 columns wide, `/panes` switches to a dashboard with Sessions and Tools beside the chat and Metrics on the right; the choice is remembered (`tui` settings).

Approvals for broad Capabilities appear as a modal — the request names the exact host, path or binary, and the agent waits. Purging a Tool's code and history is user-only and lives in the Tools view ([§5.2](#52-lifecycle-and-versioning)).

### 3.5 Headless runs (no TUI)

`formless run` is the scriptable entry point: it starts the daemon if needed, streams the answer to stdout, and exits.

```bash
formless run "summarise the CHANGELOG of this repo"        # ephemeral Session
formless run "now group those by author" --session 3f9c    # continue a Session
formless run "audit my Downloads folder" --json            # one JSON event per line
formless run "one-line summary of README.md" --output final -q   # just the answer on stdout
formless run "triage this" --output json                   # {session_id, status, answer, error}
formless run "clean up the temp files" --yes               # auto-approve sensitive Tools
formless run "classify these titles" --model small-model --effort low
formless run "plan the migration" --role planner           # a configured Role's provider and model
formless run "what's wrong here?" --attach shot.png --attach app.log   # files and images
git diff | formless run "review this"                      # piped input goes along as stdin.txt
cat task.md | formless run -                               # `-` reads the task itself from stdin
```

Piped stdin is read only when stdin isn't a terminal; from a script that leaves stdin open without writing to it, add `< /dev/null`.

Approvals are not skipped by scripting: without `--yes` a sensitive Invocation **fails closed** with a clear error instead of being silently granted. `--timeout 10m` (also `90s`, `1h30m`) interrupts a Run that takes too long. Exit codes: `0` answered, `1` Run errored/paused, `2` usage/config error, `124` timed out, `130` interrupted.

### 3.6 Daemon, diagnostics and reset

```bash
formless daemon start     # start it if it is not already up
formless daemon status    # pid, protocol, session count
formless daemon stop
formless daemon serve     # foreground, for watching logs
formless doctor           # config, dirs, keychain, endpoint + tool-calling probe
formless tools list       # registry inspection (also export / import)
formless reset --force    # destructive: config, key, whole data dir
```

There is one daemon per profile, reachable over a unix socket (`<data dir>/daemon.sock`) or a named pipe on Windows. It exits on its own after `daemon.idle_grace_secs` (default 60 s) with no clients and no pending Invocations, and the next client starts it again — so there is nothing to supervise.

### 3.7 Browser access via the relay (v2)

```bash
formless relay enable     # prints https://…#k=… — open that URL
formless relay status     # show it again
formless relay disable --forget
```

The daemon dials out to the relay, so nothing needs an inbound port; the browser holds the pairing key in the URL fragment and the relay only ever forwards AES-256-GCM ciphertext. The web app matches the TUI feature for feature (Sessions, chat with steer/queue/stop/edit, approvals, Tools with a code editor, Metrics, Proposals, Settings) and works on phones.

The default endpoint is the Worker in [`relay/`](relay/README.md) — self-host it and point `relay.url` at your own deployment; `node relay/dev.mjs --mock-provider` runs the whole thing locally without a provider or an account.

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
formless auth login     # opens the browser, listens on 127.0.0.1 for the callback
formless auth status    # mode and token expiry, never the token itself
formless auth logout
```

Tokens are kept in the keychain (plus a `0600` fallback file) and refreshed automatically before each provider call.

### 3.9 Profiles and environment variables

`--profile <name>` is a global flag: it gives an isolated daemon, data dir and config dir, which is the safe way to try dangerous Tools.

```bash
formless --profile sandbox
```

| Variable | Effect |
|---|---|
| `FORMLESS_API_KEY` | provider key; wins over the keychain and fallback file |
| `FORMLESS_OAUTH_ACCESS_TOKEN` | OAuth mode: use this access token instead of stored tokens |
| `FORMLESS_OAUTH_CLIENT_SECRET` | OAuth mode: client secret for providers that need one (never stored in config) |
| `FORMLESS_BASE_URL`, `FORMLESS_MODEL` | override the configured endpoint/model for this process |
| `FORMLESS_DATA_DIR`, `FORMLESS_CONFIG_DIR` | relocate state (also how the auto-started daemon inherits them) |
| `FORMLESS_IDLE_GRACE_SECS` | how long an idle daemon lingers before exiting |
| `FORMLESS_RELAY_SECRET` | pairing secret override for the relay |
| `RUST_LOG` | log filter for the daemon, e.g. `RUST_LOG=debug` |

### 3.10 Troubleshooting

| Symptom | Fix |
|---|---|
| `not configured; run formless for the wizard` | headless runs need a configured provider first — launch the TUI once |
| "does not support tool-calling" during onboarding | pick another model or endpoint; tool calling is mandatory |
| TUI renders garbage or does not respond | it needs a real TTY; run it interactively rather than through a pipe |
| Daemon will not start | read `<data dir>/daemon.log`, then `formless doctor` and `formless daemon status` |
| Keychain prompt on every launch | the `0600` `api_key` file in the config dir exists exactly for this; check it is present |
| macOS refuses to run the binary | `xattr -d com.apple.quarantine formless` (releases are unsigned) |
| Everything is wedged | `formless reset --force` (destroys config, key and data dir) |

### 3.11 CLI reference

| Command | Behaviour |
|---|---|
| `formless` (or `formless tui`) | Launch the TUI, auto-starting the daemon |
| `formless run "task"` | Headless Run in an ephemeral Session; `--session <id>` to continue one; `--json` for one JSON event per line; `--yes` to auto-approve sensitive Invocations for that Run (nothing is stored) |
| `formless daemon start\|stop\|status\|serve` | Daemon lifecycle |
| `formless relay enable\|disable\|status` | Encrypted browser access (v2) |
| `formless tools list\|export\|import` | Registry inspection and transfer (no secrets, no metrics) |
| `formless auth login\|status\|logout` | OAuth sign-in for subscription providers (v2) |
| `formless prompts list\|generate\|apply\|reject\|rollback` | Measured prompt/memory proposals (v2) |
| `formless update check` | Compare with the latest GitHub release; never modifies the binary |
| `formless naming status\|pull\|test "<message>"` | Session-naming model: install state, download now, preview a title |
| `formless doctor` | Config validity, data dir, keychain reachability, endpoint + tool-calling probe |
| `formless reset [--force]` | Full wipe of config, keychain entry and data dir |

Global flag: `--profile <name>`. Headless approvals **fail closed**: an unapproved sensitive Invocation fails with a clear error unless `--yes` is passed.

### 3.12 Updating

`formless update check` prints the newest GitHub release and its URL when it is newer than the running binary; it never downloads or replaces anything. Re-run `scripts/install.sh` (it always fetches the latest release and verifies the checksum), or rebuild from source. Your data dir and Tools are untouched by an update — config is migrated automatically, with a backup taken first ([§4](#4-data-config-and-profiles)).

---

## 4. Data, config and profiles

* **Data dir:** `~/Library/Application Support/formless` (macOS), `$XDG_DATA_HOME/formless` (Linux), `%APPDATA%\formless` (Windows). Config lives in the matching config dir (`config.toml` plus the OS keychain for secrets).
* Contents: the SQLite DB (Sessions, Runs, Tool metadata and version index, Invocations, metric tallies, compaction checkpoints, approvals, audit events), Tool version files, artifacts, rotated daemon logs and the audit log.
* The schema is versioned; the daemon auto-migrates on start and **takes a backup before migrating**. On corruption it refuses to guess: it stops, reports, and points at the backup.
* `FORMLESS_DATA_DIR` / `FORMLESS_CONFIG_DIR` relocate both dirs; `--profile <name>` gives an isolated daemon plus data/config dirs — intended for testing dangerous Tools.

---

## 5. The Tool system

### 5.1 `create_tool` — the Bootstrap Tool

```text
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

Each Session's Tool list is frozen at its first step: up to `tools.inline_limit` (default 40) Tools, the most-used first, plus the Tools that Session creates or finds later. Tools created in other Sessions are reached with `find_tools`, so they never change this Session's cached prompt prefix.

### 5.2 Lifecycle and versioning

* Tools are **never edited in place**. Any change creates a new integer version (`v1`, `v2`, …; no semver), recording timestamp, author (`agent` | `optimiser` | `import`) and, at promotion, the stats of the version it replaced.
* A running Invocation always finishes on the version it started with; new Invocations use the current version.
* **Retire** (agent): instant and global — the Tool becomes uncallable everywhere, data is retained, and it is restorable from the Tool browser. **Purge** (permanent deletion of code, history and tallies) is user-only, from the TUI.
* Name collisions at creation are serialised by the daemon: the second creator gets the existing Tool as a new version instead of a fork.

### 5.3 Capabilities (closed taxonomy)

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
| `tools.call(name, input)` → Promise | — | Composition; await every call, and `Promise.all` runs several in parallel (nested Invocations are recorded and measured) |
| `clipboard.read()` / `clipboard.write(text)` | `clipboard:read` / `clipboard:write` | v2; OS clipboard |
| `notify(title, body)` | `notify` | v2; desktop notification |
| `llm.complete(prompt, {system, model})` | `llm` | v2; one non-streaming completion via the configured provider |
| `agents.run(task, {system, model})` | `agent` | v2; sub-agent over the same Tools (depth-limited, no step limit), returns its final text |
| `requireModule(name)` | — | v2; load a shared module created with `create_module` |
| `emit(chunk)` | — | Stream large output to an artifact file |
| `console.log`, `sleep`, `setTimeout`/`setInterval`/`clearTimeout`, `JSON`, `cryptoRandom()`, `randomBytes(n)` | — | Logging, timing and entropy |

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
* In smoke tests the v2 side-effecting APIs are stubbed, so creation never touches the clipboard, the network or the desktop.

---

### MCP servers

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

Skills are `SKILL.md` folders of know-how (the Agent Skills format), installed from a folder, a git URL or a `.zip`:

```bash
formless skills add https://github.com/example/skills.git --subdir pdf
formless skills show pdf      # review: new installs start disabled
formless skills enable pdf
formless skills list
formless skills export pdf -o pdf.skill.json   # files only, no metrics
formless skills import pdf.skill.json          # imported disabled, contents printed
```

The agent remembers stable preferences you state ("use pnpm", "answer in Thai") as short notes that go into new sessions. You see every one, and you can edit them:

```bash
formless memory list
formless memory add "Projects live in ~/code"
formless memory edit m1a2b3c4 --disable
formless memory rm m1a2b3c4
```

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
* **Auto-naming** — when an `Untitled` Session finishes its first Run, the daemon gives it a short title (for example "Fixing Rust Error E0502"). A small model, [LFM2.5-230M](https://huggingface.co/LiquidAI/LFM2.5-230M) (153 MB, Q4_K_M), runs in-process on the CPU, so naming spends no provider tokens and sends nothing off the machine. It runs in the background, never delays a Run, and never replaces a name you set. The model downloads once into `<data dir>/models/` on first use (pinned revision, SHA-256 verified) or with `formless naming pull`. Until it is installed, or if it fails or exceeds `naming.timeout_secs`, the title is the first few words of your message. Set `naming.enabled = false` to turn naming off, or `naming.auto_download = false` to keep the model off the network. The model is licensed under the [LFM Open License v1.0](https://huggingface.co/LiquidAI/LFM2.5-230M/blob/main/LICENSE), which limits commercial use above a revenue threshold; building without the default `embedded-llm` cargo feature removes it and leaves only first-words titles.
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

**Prompt and memory (v2).** The optimiser can also propose one short guidance or memory addendum to the system prompt. Each proposal is scored against the current prompt on scenario evals that check the agent's actual tool calls (search before create, least-privilege caps, streaming, …), run several times. It can be applied only if it wins by the same promotion margin with statistical significance, and only by the user (`formless prompts`, TUI `/proposals`). Applied addenda are versioned and roll back individually; the bootstrap prompt itself is never edited.

---

## 10. Security and privacy

* **Deny by default.** A Capability is a ceiling the runtime enforces; it is never exceeded.
* **Approvals.** Broad or sensitive Capabilities prompt before first use — modal in the TUI, fail-closed in headless mode.
* **Single-user trust boundary.** Local access control is the socket/pipe file permissions on the data dir (`0700`).
* **Local-first.** Zero telemetry and no phone-home: nothing leaves the machine except calls to the configured provider endpoint, and `formless update check` only runs when you ask it to ([§3.12](#312-updating)).
* **Imports are never auto-trusted.** Capabilities are displayed, approval applies, metrics start at zero, and the author is recorded as `import`.
* **Data lifecycle.** `formless reset` wipes config, keychain entry and data dir; Session deletion removes payloads but keeps anonymised tallies; only a Tool purge deletes tallies.

---

## 11. Repository layout

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
└── .github/workflows/   release.yml — macOS arm64/x86_64, Linux x86_64, Windows x86_64
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

Where to start:

* **Runtime / sandbox** — `crates/formless-runtime`: the host API table in [§5.4](#54-the-host-api-inside-a-tool) is the contract.
* **Agent loop, tools, optimiser** — `crates/formless-core`; [§5](#5-the-tool-system) and [§9](#9-self-optimisation) describe the invariants.
* **Protocol** — `crates/formless-protocol` is versioned; clients and daemon must be updated together.
* **Clients** — `crates/formless-tui` and `crates/formless`; the relay's web UI lives in `relay/`.

Release work is documented separately in [`packaging/README.md`](packaging/README.md).

---

## 13. Non-goals

* Hosted or multi-user mode, accounts, sharing servers.
* System services, localhost-TCP transport, cloud sync or telemetry.
* Publishing signed/notarised binaries and submitting the Homebrew/winget packages (the workflow, installer and templates are in [`packaging/`](packaging/README.md); signing needs maintainer certificates).

v2 shipped what v1 left out: the encrypted Relay and Web frontend ([§3.7](#37-browser-access-via-the-relay-v2), [`relay/`](relay/README.md)), the Responses API, prompt-based tool calling, failover and measured routing, OAuth providers ([§3.8](#38-provider-options-v2)), shared modules, sub-agents, prompt/memory self-optimisation, the Tool editor and the multi-pane TUI.

---

## Roadmap

- [x] **v1** — bootstrap Tool, measured registry, sandboxed runtime, TUI/CLI and Self-Optimisation.
- [x] **v2** — encrypted Relay and web frontend, Responses API, prompt-based tool calling, failover and measured routing, OAuth providers, shared modules, sub-agents, prompt/memory self-optimisation, the Tool editor and the multi-pane TUI.
- [ ] **Next** — tracked in [GitHub Issues](https://github.com/athiwatc/formless/issues). Signed/notarised distribution and package-manager publication remain explicit [non-goals](#13-non-goals).

---

## Contributing

Contributions are welcome, whether it is a bug report, a Tool, a client improvement or a documentation fix.

1. Open an issue for anything larger than a typo so the design can be agreed first.
2. Fork the repository and create a branch from `main`.
3. Make the change. If behaviour changes, update `SPEC.md` first; no behaviour may live only in a client.
4. Run the checks:

   ```bash
   cargo fmt --all
   cargo clippy --workspace --all-targets
   cargo test --workspace
   ```

5. Open a pull request describing the change, the reasoning and the tests you ran.

---

## Further reading

1. [GLOSSARY.md](GLOSSARY.md) — the exact words (Tool, Run, Invocation, Candidate, Promotion, Compaction).
2. [SPEC.md](SPEC.md) §1–3 — principles, architecture, concepts.
3. [SPEC.md](SPEC.md) §7 — the Tool system and host API.
4. [SPEC.md](SPEC.md) §8 — metrics and Self-Optimisation.
5. [docs/adr/](docs/adr) — why the core looks the way it does.
6. [packaging/README.md](packaging/README.md) — releasing, installing, update checks.

---

## License

Apache-2.0. See `Cargo.toml` and the license text referenced there.

The optional session-naming model, LFM2.5-230M, is not part of this repository or the binary. It is downloaded at runtime and covered by Liquid AI's [LFM Open License v1.0](https://huggingface.co/LiquidAI/LFM2.5-230M/blob/main/LICENSE) ([§7](#7-sessions-runs-and-steer)).

---

## Contact

- **Project:** [formless.athiwat.xyz](https://formless.athiwat.xyz)
- **Source:** [github.com/athiwatc/formless](https://github.com/athiwatc/formless)
- **Bugs and features:** [GitHub Issues](https://github.com/athiwatc/formless/issues)

---

## Acknowledgments

- [Best-README-Template](https://github.com/othneildrew/Best-README-Template) for the README structure this page follows.
- [Liquid AI](https://www.liquid.ai/) for LFM2.5-230M, the optional local session-naming model.
- The Rust, [ratatui](https://ratatui.rs/), [rquickjs](https://github.com/DelSkayn/rquickjs), [Tokio](https://tokio.rs/) and SQLite communities.

<p align="right">(<a href="#readme-top">back to top</a>)</p>
