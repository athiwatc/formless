<a id="readme-top"></a>

<div align="center">

<h1>Formless</h1>

<p><strong>A general-purpose agent harness that boots with a small, fixed set of built-in tools and builds every other capability itself — for any use case.</strong></p>

<p>
  <a href="https://github.com/athiwatc/formless/blob/main/Cargo.toml"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License: Apache-2.0"></a>
  <a href="https://github.com/athiwatc/formless/releases"><img src="https://img.shields.io/github/v/release/athiwatc/formless?sort=semver" alt="Releases"></a>
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

A fresh install of Formless exposes **25 built-in Tools**: the ones the agent needs to grow and maintain its own registry (`create_tool`, `find_tools`, `read_tool`, `test_tool`, `run_script`, …), to reach MCP servers and skills, to keep short notes about you, and to stay in touch with you. They are implemented by the daemon rather than stored in the registry, so they are neither versioned nor optimised. Everything else — web access, file surgery, shell work, domain logic — is written on demand by the agent, persisted, versioned, measured and reused. Formless does not specialise in coding.

[§5.1](#51-the-built-in-tools) lists all 25 and what each one does; `create_tool`, the Tool factory every other capability comes out of, is covered in [§5.2](#52-create_tool--the-tool-factory).

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
| [docs/gallery.md](docs/gallery.md) | What Formless can do: 15 example tasks |
| [docs/capabilities.md](docs/capabilities.md) | What Tools can access, the risks, recommended trust settings |
| [docs/keys.md](docs/keys.md) | Every key and command (generated: `formless docs --keys`) |
| [docs/troubleshooting.md](docs/troubleshooting.md) | Common problems and their fixes |
| [docs/docker.md](docs/docker.md) | Running the daemon in Docker |
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
        <li><a href="#51-the-built-in-tools">The built-in Tools</a></li>
        <li><a href="#52-create_tool--the-tool-factory"><code>create_tool</code> — the Tool factory</a></li>
        <li><a href="#53-lifecycle-and-versioning">Lifecycle and versioning</a></li>
        <li><a href="#54-capabilities-closed-taxonomy">Capabilities (closed taxonomy)</a></li>
        <li><a href="#55-the-host-api-inside-a-tool">The host API inside a Tool</a></li>
        <li><a href="#56-composition-rules">Composition rules</a></li>
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

1. **A small, fixed boot set.** A fresh install exposes 25 built-in Tools and nothing else: the registry, memory, MCP, skills and planning primitives ([§5.1](#51-the-built-in-tools)). No domain Tool ships — web access, files, shell work and everything task-specific is built by the agent, then persisted and reused.
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
| Tool sandbox | [QuickJS-NG](https://quickjs-ng.github.io/quickjs/) via [`rquickjs`](https://github.com/DelSkayn/rquickjs) |
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

**Prebuilt binary (macOS, Linux).** The installer resolves the latest GitHub release for your platform, downloads it, verifies the `.sha256` (with `sha256sum` or `shasum`) and puts `formless` in `$PREFIX/bin` (default `~/.local`). Each installer curl attempt has a 10-second connection and 120-second transfer limit, with three retries for transient errors; a timed-out lookup suggests pinning `FORMLESS_VERSION` or using a trusted mirror (`FORMLESS_LATEST_URL` / `FORMLESS_RELEASES_URL`). It checks that `$PREFIX/bin` is writable before downloading, and replaces the binary atomically (a temp file renamed into place), so an interrupted or failed install leaves the previous version working:

```bash
curl -fsSL https://raw.githubusercontent.com/athiwatc/formless/main/scripts/install.sh | sh

# overrides: PREFIX=/usr/local, FORMLESS_VERSION=0.1.0 (or v0.1.0),
# FORMLESS_FROM_SOURCE=1 (builds this checkout when run from one, else the
# GitHub repository at FORMLESS_VERSION)
```

The installer prints the next setup and verification commands and checks
whether its `bin` directory is on `PATH`. If needed, it prints a quoted command
for your `$SHELL` (bash, zsh, fish, or a POSIX shell) and names the startup file
where you can keep it. It does not edit shell configuration files. Run
`formless` to start setup, or `formless doctor` to check your configuration;
you need a provider API key or a local model endpoint.

**From source** (any platform with a toolchain):

```bash
git clone https://github.com/athiwatc/formless && cd formless
cargo build --release          # binary at ./target/release/formless
```

`cargo run` (debug) and `cargo test --workspace` work the same way if you are working on the code. You can also `cargo install --git https://github.com/athiwatc/formless --path crates/formless --locked`.

**Linux packages.** Each release has a `.deb` (`sudo apt install ./formless_<version>_amd64.deb`) and an `.rpm` (`sudo dnf install ./formless-<version>.x86_64.rpm`); Arch users can install `formless-bin` from the AUR once it is published.

**Docker.** `ghcr.io/athiwatc/formless` runs the daemon headless on a server, reachable through the relay; volumes, the API key as a secret and profiles are in [docs/docker.md](docs/docker.md).

**Homebrew / winget.** Releases render a Homebrew formula and winget manifests and publish them once the maintainer's tap and tokens are set up ([packaging/README.md](packaging/README.md)): then `brew install athiwatc/tap/formless` and `winget install athiwatc.formless`. Until then, use the installer or a source build. **Windows:** download the release archive and unzip it, or build from source.

**Uninstall.** `formless uninstall` stops the daemon, removes the login service, deletes keychain entries and every profile's data, and removes the binary (or names the package-manager command). `--keep-data` keeps Sessions, Tools and config; `formless export-all` keeps a copy first.

> [!NOTE]
> Release artifacts are checksummed but unsigned. On macOS you may need `xattr -d com.apple.quarantine formless` after installing by hand. `formless update check` reports newer releases but never rewrites the installed binary — updates stay with the installer and checksum verification ([§3.12](#312-updating)).

### 3.3 First run: the onboarding wizard

```bash
./target/release/formless
```

With no arguments `formless` launches the **TUI**, which auto-starts the daemon on first use (`formless daemon serve`, detached, logging to `<data dir>/daemon.log`) and then walks the wizard:

1. **Provider** — pick a preset (OpenAI, Anthropic, OpenRouter, Gemini, Groq, Ollama, LM Studio) or enter any OpenAI-compatible base URL. Local presets skip the key step, and a key in `OPENAI_API_KEY` (etc.) is offered automatically. Local setup discovery honors `OLLAMA_HOST`, including custom loopback ports; remote hosts need a manually configured provider. Add more later with `formless providers add NAME --preset openrouter`.
2. **API key** — stored in the OS keychain plus a `0600` fallback file (`api_key`) in the config dir, so rebuilding a dev binary never loses it.
3. **Model** — picked from the endpoint's model list, or typed by hand.
4. **Reasoning effort** — for models that accept it.
5. **Probe** — verifies connectivity, streaming and **tool calling**. If the endpoint cannot call tools the wizard refuses to continue: every Run would fail, and a prompt-mode fallback has to be chosen deliberately ([§3.8](#38-provider-options-v2)).

The result is written to `config.toml` in the config dir ([§4](#4-data-config-and-profiles)); you never need to edit it by hand. Re-run the wizard at any time with `formless doctor` for a diagnosis, or delete the config to go through it again.

Setup names the active profile, and the TUI status line identifies a non-default
profile. If this profile needs setup while another profile has saved provider
configuration, the wizard, `formless run`, and `formless doctor` show the exact
command to open it. `formless profiles list` lists the available profiles.

### 3.4 Using the TUI

Type a task at the bottom and press **Enter**. Formless starts with the built-in Tools ([§5.1](#51-the-built-in-tools)) and builds whatever else the task needs with `create_tool`.

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

You can add your own prompt commands as Markdown files in the current profile's
`<config>/commands/` directory or the current project's `.formless/commands/`.
For example, `workflow.md` defines `/workflow`:

```markdown
---
description: Review the proposed change
---
Review $ARGUMENTS. Focus first on $1, and include @notes/review-checklist.md.
```

Invoke `/workflow src/parser.rs` to supply its first argument. Use `$$` for a
literal dollar: `$$5 per run`, `$$9.99`, and `echo $$1` become `$5 per run`,
`$9.99`, and `echo $1`. A referenced `$1`–`$9` without enough arguments stops
expansion with the filename, missing position and escape guidance before send;
the message is not sent with text silently removed.

New files and edits appear without restarting the TUI or plain client. `/help` lists them under
**Yours**; project files override global files with the same name. Builtin
commands always win, and shadowed files and malformed templates are reported.
Expansion substitutes arguments into prompt text; the full TUI then uses its
existing `@path` attachment flow. Template text does not execute shell commands
or Tools by itself.
Plain mode (`--plain` or `formless chat`) loads the current Session's templates
and sends expanded bodies as literal messages, including bodies beginning with
`/`. Its `//` escape still sends raw slash text. Invalid-template invocations
name the file and reason; typo suggestions include local and shadowed templates.
Diagnostics are announced once while present and can be announced again after
a repaired file becomes invalid again.

`formless commands list` shows the available metadata, diagnostics and shadowed
files; `formless commands list --json` returns the same shared metadata projection
used by the TUI. Listing is read-only: it does not start a daemon, create a
profile, expand a template or open its referenced attachments.
TUI/plain `/help`, the TUI picker and `commands list` show required positions
(for example `/workflow <arg1>`) and `[arguments…]` when `$ARGUMENTS` is used.
Positions count whitespace-separated words; `$ARGUMENTS` may be empty. JSON
metadata includes `max_position` and `uses_arguments`. Escaped `$$5` and literal
multi-digit `$10` do not add required arguments.

For red/green colour blindness, set `formless config set tui.theme colorblind`
(or choose **colorblind** in Settings). The TUI and CLI share an Okabe–Ito-derived
blue/orange status palette. Diffs keep bold `+`/`-` gutters; CLI status cells add
`ok`, `warn`, and `fail` markers when this theme is active or colour is disabled.
`NO_COLOR` suppresses ANSI styling without removing status information; printable
Unicode is preserved. JSON output retains its original machine-readable values.



Approvals for broad Capabilities appear as a modal — the request names the exact host, path or binary, and the agent waits. Purging a Tool's code and history is user-only and lives in the Tools view ([§5.3](#53-lifecycle-and-versioning)).

### 3.5 Headless runs (no TUI)

`formless run` is the scriptable entry point: it starts the daemon if needed, streams the answer to stdout, and exits.

```bash
formless run "summarise the CHANGELOG of this repo"        # ephemeral Session
formless run "now group those by author" --session 3f9c    # continue a Session
formless run -c "and sort them by date"                     # continue the last `formless run` Session
formless run "audit my Downloads folder" --json            # one JSON event per line
formless run "one-line summary of README.md" --output final -q   # just the answer on stdout
formless run "triage this" --output json                   # {session_id, status, answer, result, error}
formless run "clean up the temp files" --yes               # auto-approve sensitive Tools
formless run "classify these titles" --model small-model --effort low
formless run "plan the migration" --role planner           # a configured Role's provider and model
formless run --estimate "plan the migration" --json       # local preview; no Run or provider call
formless run "fix the failing test" --cwd ~/src/app        # act on that folder (default: where you run it)
formless run "what's wrong here?" --attach shot.png --attach app.log   # files and images
git diff | formless run "review this"                      # piped input goes along as stdin.txt
cat task.md | formless run -                               # `-` reads the task itself from stdin
formless run "classify these titles" --schema result.schema.json --system "Use concise labels" --max-usd 0.10 --output final --output-file result.json
formless run "return an object" --schema object.schema.json --output json | jq '.result'
```

`--schema` validates the final answer and allows one paid repair; a second
mismatch exits 3 without emitting invalid JSON in final/json mode. `--schema -`
uses stdin for the schema instead of a task or attachment. `--system` applies
only to this Run. `--max-usd` replaces its Run cap, including an intentional
increase, while the profile's daily cap still applies; zero admits no model
request. Limits are checked before each physical call, so an in-flight response
can exceed the remaining amount. Unknown spend or pricing pauses further calls.
A budget stop exits 1 and reports observed usage cost on stderr. Stopped pipeline
Runs must be resubmitted with their options; `/resume` cannot recover ephemeral
instructions. `--output-file` leaves stdout empty and atomically replaces the
file only after successful output (at most 8 MiB); failure leaves the previous
file intact.

Schema files and `--schema -` accept one leading UTF-8 BOM; the 64 KiB input
limit includes those bytes. With `--schema --output json`, the envelope keeps
`answer` as the original validated text and adds `result` as the parsed JSON
value, for example `"result": {"n": 2}`. Consumers can read `.result` directly
without parsing the answer twice. The same envelope is written by
`--output-file`. Without a schema, or when schema validation fails, `result`
is null; a schema mismatch also keeps `answer` null.

Before sending, the TUI and web composer show a local forecast for the current
draft: prompt tokens, comparable model/role Runs, likely cost and steps, and
compaction risk. No comparable history means prompt size only; unpriced models
have no dollar range. `run --estimate` prints p50/p90 steps, tokens, cost and
duration without creating a Session or Run, uploading files, refreshing credentials
or calling a provider. It may read an already-running daemon over local IPC; it
never starts one. Offline snapshots fail closed when the profile is busy.

Set `limits.forecast_confirm_usd` to require confirmation when a priced p90 exceeds
your threshold. Headless Runs need `--yes` for that confirmation. Forecasts add no
step, token or time caps. Metrics records the immutable forecast beside actual
usage, errors and coverage; interrupted or incomplete usage stays unknown.


Piped stdin is read only when stdin isn't a terminal; from a script that leaves stdin open without writing to it, add `< /dev/null`.

Approvals are **opt-in**: by default (`security.secure = false`) Tools run without asking, and each sensitive call is recorded in the audit log (`tool.run_unasked`). With approvals on (`formless --secure`, or `security.secure = true` in `config.toml`), they are not skipped by scripting: without `--yes` a sensitive Invocation **fails closed** with a clear error instead of being silently granted. `--timeout 10m` (also `90s`, `1h30m`) interrupts a Run that takes too long. Exit codes: `0` answered, `1` Run errored/paused, `2` usage/config error, `3` Run paused because schema validation failed (see `--schema` above), `124` Run timed out (see `--timeout` above), `130` interrupted.

### Sessions from the command line

```bash
formless sessions list                     # --archived, --json
formless sessions show 3f9c                # the transcript (--json for scripts)
formless sessions search "invoice"         # across every Session
formless sessions rename 3f9c "Q3 report"
formless sessions export 3f9c -o q3.md       # infer Markdown from the filename
formless sessions import bundle.json
formless sessions rm 3f9c                  # restorable for a short while: sessions restore <id>
```

`--format` accepts `json`, `markdown` (alias `md`), or `html`. When omitted,
`.md`/`.markdown`, `.html`/`.htm`, and `.json` output extensions select their
matching format, ignoring extension case. Standard output and other extensions
still default to JSON. An explicit format conflicting with a recognized output
extension is a usage error before any Session lookup or daemon connection.
On Unix, ordinary file output uses atomic replacement with existing mode bits
applied before staging content; failed pre-commit writes leave the prior export
intact. This does not claim extended ACL or xattr preservation.
The Windows implementation uses verified user-private staging and preserves an
existing file's DACL through replacement, without ignoring ACL merge failures.
It remains pending native Windows acceptance. If an OS replacement failure
retains private recovery files, the error identifies their directory for
inspection before retrying; no destructive replacement fallback is used.
Existing symbolic links keep writing through to their targets,
and descriptor/device streams retain their existing direct-write behavior.

### Reporting a bug

```bash
formless bug-report      # writes formless-bug-report-<time>.md (secrets redacted) to attach to an issue
formless doctor --json   # the same checks, machine-readable
```

### Daemon log

```bash
formless logs --level warn --since 1h      # recent warnings and errors
formless logs -f                           # follow as it's written
```

### Shell completions

The installer and the Homebrew formula install bash, zsh and fish completions. Otherwise:

```bash
formless completions zsh > ~/.zfunc/_formless      # also bash, fish, powershell, elvish
```

### Settings from the command line

```bash
formless config path                       # where config.toml is
formless config get tools.inline_limit     # effective value (env overrides applied)
formless config set tui.notify_min_secs 30 # validated, audited, applied at once
formless config edit                       # $EDITOR; saved only if it still parses
```

Map settings (`provider.prices`, `provider.model_context_tokens`,
`provider.model_audio_input`, `provider.model_pdf_input`, `tui.colors`) merge
entries by default. Each supplied entry replaces that entry's whole value;
`null` removes that entry. Use `--replace` for whole-map replacement or clearing:

```bash
formless config set provider.prices '{"claude-sonnet-4.5":{"input":1,"output":2}}'
formless config get provider.prices.claude-sonnet-4.5
formless config set provider.model_context_tokens '{"vendor/qwen2.5":32768}'
formless config set provider.model_context_tokens.vendor/qwen2.5 null
formless config set provider.prices '{}' --replace
```

After a listed map's root, the entire suffix is one literal entry name,
including dots and slashes. Quote the key in your shell if it contains spaces.
Lists still replace their complete value. A merge needs an updated daemon;
older daemons reject it without changing the file. Restart an older daemon
after updating the binary.

### 3.6 Daemon, diagnostics and reset

```bash
formless daemon start     # start it if it is not already up
formless daemon status    # pid, version, protocol, sessions, running Runs
formless daemon stop
formless daemon restart   # waits for running Runs (--force interrupts them)
formless daemon serve     # foreground, for watching logs
formless doctor           # config, dirs, keychain, endpoint + tool-calling probe
formless tools list       # registry inspection (also export / import)
formless tools show fetch_page                       # description, schemas, versions, stats
formless tools reviews                               # retained description warnings and diffs
formless tools run fetch_page --input '{"url":"https://example.com"}' --yes
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

Names use 1–64 ASCII letters, digits, `-` or `_`, starting with a letter or digit.
Default-owned names such as `commands`, `artifacts`, `backups`, `mcp`, `models`,
`skills`, `api_key` and `relay_secret` are reserved regardless of case; the full
list is in SPEC §9.4. Case collisions and linked or overlapping profile paths are
refused before setup or removal. `profiles list` warns about unsafe legacy
folders and preserves them. Back up the reported paths outside the Formless
roots and recover them manually before reset/uninstall; no legacy folder is
automatically moved or deleted. Your ordinary `commands/*.md` templates remain
part of the default profile, never a removable profile named `commands`.

```bash
formless --profile sandbox
formless profiles list           # every profile, whether its daemon runs, where its data is
formless profiles rm sandbox     # stops its daemon, deletes its data, config and secrets
```

| Variable | Effect |
|---|---|
| `FORMLESS_API_KEY` | provider key; wins over the keychain and fallback file |
| `FORMLESS_API_KEY_<NAME>` | key for a named provider; uppercase the name and replace non-alphanumeric characters with `_` |
| `FORMLESS_API_KEY_FILE`, `FORMLESS_API_KEY_<NAME>_FILE` | paths to files containing the default/named provider key |
| `FORMLESS_OAUTH_ACCESS_TOKEN` | OAuth mode: use this access token instead of stored tokens |
| `FORMLESS_OAUTH_CLIENT_SECRET` | OAuth mode: client secret for providers that need one (never stored in config) |
| `FORMLESS_BASE_URL`, `FORMLESS_MODEL` | override the configured endpoint/model for this process; trimmed, empty/whitespace-only values ignored |
| `FORMLESS_DATA_DIR`, `FORMLESS_CONFIG_DIR` | relocate state (also how the auto-started daemon inherits them) |
| `FORMLESS_IDLE_GRACE_SECS` | idle daemon timeout as a nonnegative whole number of seconds; invalid or empty values are errors |
| `FORMLESS_SECURE` | process override for `security.secure`: `1/true/yes/on` or `0/false/no/off`, case insensitive; blank is ignored, invalid values are errors |
| `FORMLESS_DAEMON_STAY` | keep the daemon alive as with `daemon serve --stay`; presence enables it |
| `FORMLESS_RELAY_SECRET` | pairing secret override for the relay |
| `RUST_LOG` | log filter for the daemon, e.g. `RUST_LOG=debug` |

Only the four config overrides listed in SPEC §13 are supported; setting an
arbitrary `FORMLESS_*` name does not override a config key. A setting saved
under the daemon's `FORMLESS_MODEL` or `FORMLESS_IDLE_GRACE_SECS` override is
persisted for later use. `config set` reports the saved value, the daemon's
effective value and the overriding variable; `config get` reads the file with
the client's own environment applied.

### 3.10 Troubleshooting

| Symptom | Fix |
|---|---|
| `not configured; run formless for the wizard` | headless runs need a configured provider first — launch the TUI once |
| "does not support tool-calling" during onboarding | pick another model or endpoint; tool calling is mandatory |
| TUI renders garbage or does not respond | it needs a real TTY; run it interactively rather than through a pipe |
| "the daemon is vX but this formless is vY" | an old daemon is still running after an upgrade: `formless daemon restart` (or `/restart-daemon` in the TUI) |
| Daemon will not start | read `<data dir>/daemon.log`, then `formless doctor` and `formless daemon status` |
| Keychain prompt on every launch | the `0600` `api_key` file in the config dir exists exactly for this; check it is present |
| macOS refuses to run the binary | `xattr -d com.apple.quarantine formless` (releases are unsigned) |
| Everything is wedged | `formless reset --force` (destroys config, key and data dir) |

### 3.11 CLI reference

With no command, `formless` launches the TUI and auto-starts its daemon.
The table covers every visible root command, including Clap's generated
`help` command. Use `formless COMMAND --help` for arguments and defaults;
`formless help COMMAND SUBCOMMAND` also opens nested help.

| Command | Actions / useful options | Purpose |
|---|---|---|
| `formless approvals` | `pending`, `approve`, `deny`, `list`, `revoke`, `denials` | Inspect pending requests, answer one, or inspect and manage existing grants/denials. |
| `formless auth` | `login`, `status`, `logout` | Manage provider credentials (API key or OAuth). |
| `formless backup` | No action: create; `list`, `verify`, `restore`, `drill` | Create, inspect, verify, restore, or rehearse database snapshots. |
| `formless bug-report` | `-o FILE` / `--output FILE` | Write a redacted Markdown diagnostic report. |
| `formless chat` | `--session ID` or `--new` | Line-mode chat with streamed answers and inline questions. |
| `formless commands` | `list` | List local user-defined slash-command metadata without starting a daemon. |
| `formless completions` | `bash`, `zsh`, `fish`, `powershell`, `elvish` | Print a shell completion script. |
| `formless config` | `path`, `get`, `set`, `edit` | Locate, inspect, validate, or edit configuration. |
| `formless daemon` | `start`, `stop`, `restart`, `status`, `serve`, `install-service`, `uninstall-service` | Manage the daemon and its optional login service. |
| `formless docs` | Optional topic; `--print`, `--keys` | Open a guide (gallery by default), print its URL, or print the embedded keyboard reference. |
| `formless doctor` | `--json`, `--fix`, `--restore-drill` | Diagnose setup, apply safe fixes, or rehearse the newest backup. |
| `formless export-all` | `--sessions`, `-o FILE` / `--output FILE` | Write a portable setup bundle, optionally including Session transcripts. |
| `formless gc` | `--dry-run` | Free reclaimable storage or preview what would be freed. |
| `formless help` | Optional command path, such as `sessions show` | Print help for the CLI or a nested command. |
| `formless import-all` | `FILE` | Add a portable setup bundle without overwriting existing entries. |
| `formless jobs` | `list`, `add`, `run`, `history`, `enable`, `disable`, `rm` | Manage scheduled prompts and inspect their history. |
| `formless logs` | `--follow`, `--level`, `--since`, `--lines` | Read or follow the daemon log and rotated files. |
| `formless mcp` | `add`, `remove`, `list`, `test`, `login`, `logout`, `catalog`, `import`, `log`, `tools`, `tool`, `prompts`, `prompt` | Manage external MCP servers and inspect their tools/logs. |
| `formless metrics` | `--since DURATION`, `--json` | Read recorded model, Tool and built-in statistics through independent reports at one cutoff. |
| `formless memory` | `list`, `add`, `rm`, `edit` | Inspect or change saved user-memory notes. |
| `formless modules` | `list`, `show`; `show --version N` | Read all modules, inspect metadata/history/dependents, or emit an exact stored source version. |
| `formless naming` | `status`, `pull`, `test` | Inspect, install, or preview the embedded Session-naming model. |
| `formless prices` | `import` | Import a model-price catalog for cost and spending limits. |
| `formless profiles` | `list`, `rm` | List or remove isolated profiles. |
| `formless prompts` | `list`, `diff`, `generate`, `apply`, `reject`, `rollback` | Inspect existing proposal comparisons and manage measured prompt/memory proposals. |
| `formless providers` | `list`, `add`, `remove`, `test` | Manage named model providers and test a connection. |
| `formless relay` | `enable`, `disable`, `status` | Manage encrypted browser access through the relay. |
| `formless reset` | `--force` | Wipe configuration, credentials, and the current profile's data. |
| `formless restore` | `FILE` | Replace the database from a backup, preserving the current database. |
| `formless roles` | `list`, `add`, `rm` | Map named purposes to a provider and model. |
| `formless run` | `TASK`; `--estimate`, `--session`, `--output`, `--attach` and other Run options | Run one headless task, or print its local forecast without starting it. |
| `formless routing` | `--json` | Read currently available daemon routing decisions and returned candidate figures without starting a model request. |
| `formless runs` | No action: list; `show`, `cancel` | List recent Runs, inspect one by ID/prefix, or cancel that exact Run. |
| `formless security` | `status` | Inspect the daemon's grants, denials, trusted resources, and exposed interfaces. |
| `formless sessions` | `fork`, `compare`, `list`, `show`, `files`, `rewind`, `rename`, `rm`, `restore`, `import`, `search`, `archive`, `unarchive`, `export` | Manage stored Sessions, follow a Run with `show --follow`, or inspect tracked files. `list --pinned --tag TAG` filters the fetched reply; selectors combine with `--archived`, preserving reply order. |
| `formless skills` | `reviews`, `add`, `remove`, `list`, `enable`, `disable`, `show`, `export`, `import` | Inspect and manage Skills and their retained description reviews. |
| `formless storage` | `--json` | Report disk usage by data category. |
| `formless tools` | `reviews`, `list`, `disable`, `enable`, `undo`, `pin`, `unpin`, `tag`, `show`, `retire`, `restore`, `rollback`, `purge`, `run`, `export`, `import` | Inspect, manage, run, or transfer registry Tools. |
| `formless tui` | No action | Launch the full-screen terminal client; also the default with no command. |
| `formless uninstall` | `--keep-data`, `--keep-binary`, `--yes` (alias `--force`) | Remove Formless, optionally retaining data or the binary. |
| `formless update` | No action, `check`, or `--check`; `--json` | Show newer release notes and the applicable upgrade command; never rewrite the binary. |
| `formless web` | `--port`, `--no-open`, `--stop` | Open, print a link for, or stop the local browser UI. |

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

`update --json`, `update check --json`, and `update --check --json` emit one
object: `{installed, status, newer_releases: [{tag, url, notes}]}`. Notes retain
the existing trimmed body and oldest-first order; `installed` has no `v` prefix.
`update_available` means the fetched page contains newer stable releases;
`up_to_date` means none in that single 50-release response, without pagination.
`release_check_unavailable` maps to an actual HTTP 404 response,
not verified absence of releases. Error text alone never selects this status.
Completed checks exit 0; other fetch or JSON parse failures remain errors with
no success JSON. Human output stays unchanged, and the JSON form never runs
an updater.

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

### 3.12 Updating

`formless update` bounds its release lookup to 15 seconds overall (5 seconds to connect), including the response body, and reports an actionable timeout error. It lists every GitHub release newer than the running binary, oldest first, with its release notes, then prints the command that upgrades it the way it was installed (`brew upgrade formless`, `cargo install … --force`, or the install script). `formless update --check` (or `update check`) stops after the report. Neither downloads or replaces anything. Re-run `scripts/install.sh` (it fetches the latest release, or `FORMLESS_VERSION`, and verifies the checksum), or rebuild from source. Your data dir and Tools are untouched by an update — config is migrated automatically, with a backup taken first ([§4](#4-data-config-and-profiles)).

---

## 4. Data, config and profiles

* **Data dir:** `~/Library/Application Support/formless` (macOS), `$XDG_DATA_HOME/formless` (Linux), `%APPDATA%\formless` (Windows). Config lives in the matching config dir (`config.toml` plus the OS keychain for secrets).
* Contents: the SQLite DB (Sessions, Runs, Tool metadata and version index, Invocations, metric tallies, compaction checkpoints, approvals, audit events), Tool version files, artifacts, rotated daemon logs and the audit log.
* The schema is versioned; the daemon auto-migrates on start and **takes a backup before migrating**. On corruption it refuses to guess: it stops, reports, and points at the backup.
* `formless backup` saves the **database only**. Copy `artifacts/` and `skills/` separately if you need their files; `config.toml`, credential files and OS keyring entries are excluded. The database itself may contain sensitive conversation or Tool data. Separate live folder copies are not a coordinated snapshot.
* `formless restore FILE` preserves a safety copy and replaces the database with the daemon stopped. It warns about missing, unreadable or foreign artifact/skill references without deleting their rows. Recorded paths are not rewritten: copying companion folders to another profile or machine does not automatically repair absolute paths. The warning checks availability, not file contents or whole-profile consistency.

Backup inspection is available with `formless backup list` and
`formless backup verify FILE`. Use `formless backup restore FILE --dry-run`
to inspect the stopped-daemon restore plan, and `formless doctor --restore-drill`
for an isolated offline rehearsal of the newest snapshot. Snapshots contain the
database only: copy artifact/skill files separately; configuration, credential
files and OS keyring data are excluded. Recorded paths are not relocated.

* `FORMLESS_DATA_DIR` / `FORMLESS_CONFIG_DIR` relocate both dirs; `--profile <name>` gives an isolated daemon plus data/config dirs — intended for testing dangerous Tools.

---

## 5. The Tool system

### 5.1 The built-in Tools

A fresh install exposes **25 built-in Tools**. They are implemented by the daemon itself rather than stored in the registry, so they have no versions, are excluded from Self-Optimisation, cannot be retired and hold no Capabilities of their own. They exist so the agent can grow and maintain its registry, reach MCP servers and skills, keep short notes about you, and stay in touch with you while it works. How often each one is called is reported by `read_metrics({kind: "builtins"})`.

Everything else — the web, the filesystem, the shell, git, file formats, domain logic — is a registry Tool the agent writes ([§5.2](#52-create_tool--the-tool-factory)).

**Growing and maintaining the registry**

| Tool | Arguments | What it does |
|---|---|---|
| `create_tool` | `name`, `title`, `description`, `tags[]`, `input_schema`, `output_schema`, `implementation_js`, `capabilities[]`, `memory_mb?` | Writes a Tool — or a new version of an existing one — after validating the name, both JSON Schemas and the JS syntax, then smoke-testing it in the sandbox. Nothing broken is saved. [§5.2](#52-create_tool--the-tool-factory). |
| `create_module` | `name`, `description?`, `implementation_js` | Adds a version of a shared JS module (`module.exports = …`) that any Tool loads with `requireModule(name)`. Versions are immutable. |
| `find_tools` | `query`, `limit?` | Keyword search over Tool names, descriptions and tags; the matches become callable in the current Session. This is how a Tool written in an earlier Session comes back, and it is the first step before writing a new one. |
| `read_tool` | `name`, `version?`, `diff_from?` | A Tool's code, schemas, Capabilities, `memory_mb`, version history, metrics and recent failures; `diff_from` shows what one version changed. Read before writing a new version, so you change a Tool rather than rewrite it from memory. |
| `read_module` | `name`, `version?` | A module's source, description and history, plus the Tools that require it. |
| `test_tool` | `input`, plus `name` or the full `create_tool` draft | Dry-runs a saved Tool or an unsaved draft on real input, under its declared Capabilities and with the usual approvals. Nothing is saved or measured; it returns the output (or the error), console logs, and whether the output matched `output_schema`. |
| `run_script` | `code`, `uses[]`, `input?`, `save_as?` | Runs a short script that may call only the registry Tools named in `uses` (built-ins are not callable from a script) — looping, filtering, branching, fanning out with `Promise.all` — and returns just the final value. The script has no Capabilities of its own, so anything that touches the world stays in a Tool; `save_as` persists a script you would run twice as a Tool. |
| `unload_tools` | `names[]` | Drops Tools from the Session's listing at the next context compaction, so the prompt carries fewer, more relevant functions. `find_tools` loads them again later. |
| `retire_tool` | `name`, `reason` | Makes a Tool uncallable everywhere, immediately. Data is retained and the user can restore it; deleting code and tallies ([purge](#53-lifecycle-and-versioning)) is user-only. |

**Invocations, artifacts and metrics**

| Tool | Arguments | What it does |
|---|---|---|
| `check_invocation` | `invocation_id` | A Pending Invocation's status: still `pending`, or its final result. |
| `await_invocation` | `invocation_id` | Waits without a deadline for that result — this is how long work finishes instead of being killed ([§6](#6-invocations-async-by-design)). |
| `abandon_invocation` | `invocation_id` | Stops waiting for a handle: the Invocation still finishes and is recorded, but its result is not delivered. |
| `read_artifact` | `id`, `offset?`, `length?`, `grep?` | Reads a slice of a large output this Session stored as an artifact, or the lines matching `grep`, with line numbers. No approval needed, no other Session's artifacts. |
| `read_metrics` | `kind`, `filter?`, `limit?` | Registry health, for cleaning up and explaining: `tools` (`needs_attention` \| `failing` \| `unused` \| a name), `audit`, `models`, `runs`, `builtins`, `duplicates`, `mcp_chains` ([§9](#9-self-optimisation)). |

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

### 5.2 `create_tool` — the Tool factory

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

### 5.3 Lifecycle and versioning

* Tools are **never edited in place**. Any change creates a new integer version (`v1`, `v2`, …; no semver), recording timestamp, author (`agent` | `optimiser` | `import`) and, at promotion, the stats of the version it replaced.
* A running Invocation always finishes on the version it started with; new Invocations use the current version.
* **Retire** (agent): instant and global — the Tool becomes uncallable everywhere, data is retained, and it is restorable from the Tool browser. **Purge** (permanent deletion of code, history and tallies) is user-only, from the TUI.
* Name collisions at creation are serialised by the daemon: the second creator gets the existing Tool as a new version instead of a fork.

### 5.4 Capabilities (closed taxonomy)

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

### 5.5 The host API inside a Tool

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
| `process.run(cmd, args, {cwd, env, stdin, stdinEncoding, encoding, timeout_ms})` → `{code, signal, stdout, stderr, timed_out, killed, truncated}` | `process` | Execute a declared binary, no shell; optional timeout kills its group |
| `process.spawn(cmd, args, opts)` → `{read, wait, kill, close}` | `process` | Read bounded `{stream, data, eof}` chunks; `wait` discards unread output; handles are reaped when the Invocation ends |
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

### 5.6 Composition rules

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

The agent remembers stable preferences you state ("use pnpm", "answer in Thai") as short notes. You see every one, and you can edit them.

A note's scope is `global` (the default) or `project:/absolute/path`. The memory
prompt lists global notes followed by project notes grouped under `When working in <path>:`. A newly added note applies from the next Session, or in the current
Session after compaction.

Manage notes with the commands below, in the TUI with `/memory` (see the
[key reference](docs/keys.md#integrations)), or on the web under **Settings → Memory**.
See [User memory](SPEC.md#711-user-memory) for details.

```bash
formless memory list
formless memory add "Projects live in ~/code"
formless memory add "Use pnpm" --scope project:/absolute/project/path
formless memory edit m1a2b3c4 --disable
formless memory edit m1a2b3c4 --scope project:/absolute/project/path
formless memory rm m1a2b3c4
```

## 6. Invocations: async by design

* Every Tool call is an **Invocation**: one Tool version, one input, recorded and measured.
* Past a soft threshold (default 300 s) the Invocation becomes a **Pending Invocation** — still running, with a handle the agent holds (`check_invocation`, `await_invocation`, `abandon_invocation`) instead of a timeout killing it.
* Outcomes are tagged distinctly: success, error, `resource` (JS heap cap hit), `user-cancelled`, `abandoned`. User-cancelled and malformed model tool-calls never masquerade as Tool bugs, so the optimiser cannot fix what the user interrupted.
* Stalled provider requests follow the same pending pattern.
* Progress bars follow invocation IDs, so parallel calls to the same Tool keep independent progress and completion states. A stalled call retains its last reported fraction and label; repeating a fraction or changing only the label does not prove forward progress. Older daemon events without IDs update a bar only when the match is unambiguous.

---

## 7. Sessions, Runs and Steer

* **Session** — a resumable conversation plus Run history; many run concurrently against one Daemon Core. Deleting a Session removes payloads but keeps anonymised tallies.
* **Run** — one unit of work: user message → final answer. No step, token or time limit; it ends on completion, fault or interruption.
* **Steer** — a user message sent while a Run is active, injected at the next step boundary.
* **Auto-naming** — when an `Untitled` Session finishes its first Run, the daemon gives it a short title (for example "Fixing Rust Error E0502"). A small model, [LFM2.5-230M](https://huggingface.co/LiquidAI/LFM2.5-230M) (153 MB, Q4_K_M), runs in-process on the CPU, so naming spends no provider tokens and sends nothing off the machine. It runs in the background, never delays a Run, and never replaces a name you set. The model downloads once into `<data dir>/models/` on first use (pinned revision, SHA-256 verified) or with `formless naming pull`. Until it is installed, or if it fails or exceeds `naming.timeout_secs`, the title is the first few words of your message. Set `naming.enabled = false` to turn naming off, or `naming.auto_download = false` to keep the model off the network. The model is licensed under the [LFM Open License v1.0](https://huggingface.co/LiquidAI/LFM2.5-230M/blob/main/LICENSE), which limits commercial use above a revenue threshold; building without the default `embedded-llm` cargo feature removes it and leaves only first-words titles.
* **Compaction** — a persisted summary checkpoint that replaces old history and Tool output as context grows, so long Sessions keep going without losing the trail.
* **Artifacts** — large Tool output is written to an artifact file; only a bounded preview enters model context.
* **Tracked path changes** — `/files` lists paths captured by live host filesystem operations, including directories created with `fs.mkdir`. `/rewind` attempts safe restoration and reports skipped paths with reasons. Programs launched by Tools and MCP tools can write outside this tracking; when those capabilities are invoked, file notes and recaps warn that untracked writes are possible, including when no paths were captured. Counts describe tracked paths only. Paths whose recovery images expired remain listed with the reason; rewind never substitutes an older Run silently. Windows restoration also requires retained original objects and live object or directory proofs where applicable. Lost or expired proofs, including process-local proofs lost on restart, leave those paths read-only rather than reconstructing authority from stored hashes.

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
2. **Candidate** — a background job runs when no active Run is waiting on that Tool. Before contacting the provider, it freezes a bounded, outcome-independent pool of recent unique inputs (default `optimiser.replay_pool_limit = 200`) and splits it into generation and holdout sets (default `optimiser.replay_holdout_percent = 50`). Only generation examples and their errors reach the candidate model; held-out inputs stay private ([SPEC §8.4](SPEC.md#84-sandboxed-replay-the-promotion-gate)).
3. **Sandboxed replay (the gate)** — Candidate and baseline run on the **same frozen held-out inputs** in separate throwaway copy-on-write overlays: writes are discarded, network is denied by default (recorded responses are served where available), `process` is denied, and no real secrets are injected. Network/MCP **replay-limited** Candidates still require explicit user approval. Legacy attempts without verifiable frozen provenance must be regenerated rather than treated as held-out evidence.
4. **Promotion bar** — by default, automatic promotion requires **≥ 10 points improvement**, **zero regressions on recorded-success inputs**, at least **20 held-out inputs** (`optimiser.replay_min_holdout`), and a two-sided exact paired McNemar **p ≤ .05** (`optimiser.replay_significance`). Attempt detail records held-out scores, 95% Wilson confidence intervals, discordant-pair counts and significance without raw held-out inputs. Otherwise-qualifying small-sample or nonsignificant wins go to `awaiting_approval`; capability-widening, schema-compatibility and replay-limited gates still apply. Candidates failing the margin/regression guard are discarded ([SPEC §8.5](SPEC.md#85-promotion)).
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

The [CI workflow](.github/workflows/ci.yml) runs on self-hosted macOS ARM64 and Linux x64
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
The workflow does not deploy or publish a release; release packaging remains in
`release.yml`. Live external-link checks remain in `docs.yml`; Playwright browser
checks in `relay/README.md` remain separate manual checks. AUR packaging tests
run only in the Linux job because they require GNU install.

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
definitions with the README §3.11 and SPEC §12 reference tables. It runs as part
of the workspace tests and rejects missing, stale, or duplicate entries.
Examples are shell-split and parsed by the real CLI without dispatching
operations, so obsolete flags fail locally. Documentation topic checks reject
ambiguous prefixes and unknown names; `docs --print` never needs a profile or
browser.

Rules of the road: **no behaviour may live only in a client** — the CLI and TUI are both clients of the daemon protocol. Every Tool is validated and smoke-tested at creation, and every behavioural change should be reflected in `SPEC.md` first.

Where to start:

* **Runtime / sandbox** — `crates/formless-runtime`: the host API table in [§5.5](#55-the-host-api-inside-a-tool) is the contract.
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
