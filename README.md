<a id="readme-top"></a>

<div align="center">

<h1>Formless</h1>

<p><strong>One conversation that runs your AI agents.</strong><br/>
Talk in Main Chat, organize work into Topic Chats, and let Formless drive the harnesses you already use.</p>

<p>
  <a href="https://github.com/athiwatc/formless/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License: Apache-2.0"></a>
  <a href="https://github.com/athiwatc/formless/releases"><img src="https://img.shields.io/github/v/release/athiwatc/formless?sort=semver" alt="Releases"></a>
  <a href="#install"><img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey.svg" alt="Platforms: macOS, Linux, Windows"></a>
  <a href="#security-and-privacy"><img src="https://img.shields.io/badge/telemetry-none-success.svg" alt="Telemetry: none"></a>
</p>

<p>
  <a href="https://formless.athiwat.xyz"><strong>Docs</strong></a>
  &middot;
  <a href="https://github.com/athiwatc/formless/issues/new?labels=bug">Report a bug</a>
  &middot;
  <a href="https://github.com/athiwatc/formless/issues/new?labels=enhancement">Request a feature</a>
</p>

</div>

## What it is

Formless is a local orchestrator for AI work. You talk to one assistant; when a
request turns into real work, Formless moves it into its own Topic Chat, plans
the steps and hands each step to an agent harness installed on your machine,
such as Claude Code, Codex, or Muse Code. Their questions, permission requests
and results come back into the same conversation.

It is not only for code. A flow is whatever steps you describe — *scan receipts
→ do the accounting → write the report* works the same way as *build → review*.

```text
Main Chat
├── "What's the weather in Bangkok?"            answered in place
└── Topic: Ship the billing export              approved by you
    ├── step: plan            → Claude   (read-only)
    ├── step: implement       → Codex    (own worktree)
    ├── step: review          → Claude   (read-only)
    └── Topic: Fix the CSV edge cases           nested topic
```

## How it works

| | |
|---|---|
| **Main Chat** | One everyday conversation. Questions are answered in place; the assistant can suggest moving a request into a topic. |
| **Topic Chats** | A topic owns one piece of work: a repo, a problem, a project. Topics nest to any depth — a single person may use *Main → Topic*, a team *Main → Project → Story → step*. |
| **Your consent first** | A suggestion moves nothing. Messages move and work starts only after you approve in the client; a chat reply such as "yes" is not consent. |
| **Advisory flows** | You can outline steps and pick a harness per step. The outline is advice: the Formless model in the Topic Chat decides the actual steps, may retry a failed step on another harness, and decides when the work is done. |
| **Isolated steps** | Each step gets its own conversation and private workspace — a Git worktree for Git topics, or a fresh directory otherwise. Declared file inputs are staged separately. Read-only Git steps check out the exact input revision. |
| **Questions and permissions** | Formless can answer ordinary harness questions only when verified evidence from your approved request or a matching prior human answer explicitly supports the choice. Otherwise it asks you. User-only questions and permissions remain yours to decide. |
| **Scoped context** | A harness reads its own topic branch — the topic and its ancestors — not Main Chat or sibling topics, unless you allow wider context for that topic. |
| **Nothing auto-merges** | Steps produce Git revisions, files or text. Formless never merges or pushes. |

## Harnesses

Integrated adapters use each harness's official local interface and a private,
per-step MCP bridge back to the topic. Install and sign in to a harness the
usual way; Formless does not ship or log in to them for you. Availability
distinguishes installation from sign-in: an unknown sign-in status is not proof
that a harness is signed in or authenticated.

Current topic-step adapters are Claude Code and Codex via their official
non-interactive CLIs, Pi via its attempt-private RPC adapter, OpenCode and
Antigravity through their ACP integrations, and Muse Code via its `muse serve`
protocol. Grok Build and additional ACP profiles are still in progress.
This list describes implemented integrations, not authenticated end-to-end validation.

For topic runs with `agy` not on PATH, set `FORMLESS_TOPIC_ANTIGRAVITY_EXE`
to its absolute path. `FORMLESS_AGY_BIN` applies only to the opt-in
`FORMLESS_LIVE_AGY` live check in [the adapter guide](docs/antigravity.md).

Any integrated harness can take any step, including the same harness on
both sides of a hand-off. A harness's own permission modes are not an OS
sandbox; the isolated worktree or directory is the boundary.

## Install

Build from source with Rust stable, Cargo, and a C compiler. On Linux, also
install `pkg-config` and the D-Bus development headers. See the
[build requirements](https://github.com/athiwatc/formless/blob/main/CONTRIBUTING.md#local-checks)
for details.

Install with `cargo install --git https://github.com/athiwatc/formless --locked formless-bin`,
then run `formless` to start the daemon and open the setup wizard.

If [GitHub Releases](https://github.com/athiwatc/formless/releases) has a matching
asset for your platform, see the [macOS/Linux install guide](docs/usage.md#install)
for the [installer script](https://raw.githubusercontent.com/athiwatc/formless/main/scripts/install.sh)
and packages, or see the [Windows ZIP guide](docs/windows-install.md). For a
headless setup without a published release, [build the Docker image from source](docs/docker.md#building-it-yourself)
and use `formless` as the local image name.

The wizard needs a model endpoint with tool calling: OpenAI-compatible Chat
Completions or Responses, or native Anthropic Messages. Ollama and LM Studio
work locally without a key. Full details are in [Using Formless](docs/usage.md).

## Clients

One daemon owns all state; every client is a thin view of it, so closing one never stops work.

| Client | Start it |
|---|---|
| Terminal UI | `formless` — Main Chat and nested Topic Chats via `/main`, `/topics`, and `/topic` |
| Desktop app | native GPUI app with Main Chat and nested Topic Chats ([overview](crates/formless-desktop/README.md)) |
| Browser | `formless relay enable` — end-to-end encrypted, with Main Chat and nested Topic Chats; works on phones ([relay](relay/README.md)) |
| CLI | `formless run "…"` — scriptable, with JSON output; `formless chat` opens an interactive line-mode chat |

Keys and commands: [docs/keys.md](docs/keys.md). Every CLI command: [docs/cli.md](docs/cli.md).

## Under the hood

Formless still has its own agent: a long-lived daemon with a sandboxed Tool
registry. The model starts with a small fixed set of built-in Tools and writes
any other Tool it needs as capability-gated JavaScript in a QuickJS sandbox.
Tools are versioned, measured on every call and improved only when a candidate
beats the current version on replayed inputs. MCP servers, Skills, memory and
scheduled Jobs plug into the same registry.

```text
 TUI · Desktop · Web · CLI ──── socket / named pipe ────▶ Daemon Core
                                                         ├─ Main Chat · Topics · Runs (SQLite)
                                                         ├─ Tool registry ─▶ QuickJS sandbox
                                                         ├─ Providers: Chat Completions · Responses · Anthropic
                                                         └─ Harness steps ─▶ Claude Code · Codex · Pi · OpenCode · Antigravity · Muse Code
```

Architecture, the Tool system and self-optimisation: [docs/architecture.md](docs/architecture.md).
Normative behaviour: [SPEC.md](SPEC.md). Vocabulary: [GLOSSARY.md](GLOSSARY.md).

## Security and privacy

- **Local-first.** Conversations, topics and Tools live in a local SQLite store; the daemon listens only on an owner-only socket or named pipe.
- **No telemetry.** Network traffic is your providers, the harnesses you run, an optional relay and optional local-model downloads.
- **Deny by default.** Tools reach only the capabilities they declare; sensitive calls can require approval (`formless --secure`).
- **Harness boundaries.** Harnesses get a scoped, per-attempt bridge: no approval, start, merge or push operations, and no way to decide their own permissions.

More: [docs/capabilities.md](docs/capabilities.md) · [docs/troubleshooting.md](docs/troubleshooting.md).

For security reports, see [SECURITY.md](SECURITY.md); do not post vulnerability
details in public issues.

## Documentation

| | |
|---|---|
| [Using Formless](docs/usage.md) | Install, setup, TUI, headless runs, relay, providers, profiles |
| [CLI reference](docs/cli.md) | Every command and global flag |
| [Architecture](docs/architecture.md) | Daemon, Tools, Sessions and Runs, self-optimisation, development |
| [Keys and commands](docs/keys.md) | Generated keyboard reference |
| [MCP](docs/mcp.md) · [Skills](docs/skills.md) · [Jobs](docs/jobs.md) | Extending Formless |
| [Pi adapter](docs/pi-adapter.md) · [Antigravity](docs/antigravity.md) · [Muse Code](docs/muse-code.md) | Topic-step adapter details |
| [Gallery](docs/gallery.md) | Example tasks |
| [SPEC.md](SPEC.md) | The specification |

## Contributing

Behaviour changes go in [SPEC.md](SPEC.md) first, and no behaviour lives only
in a client. See the [contributor guide](https://github.com/athiwatc/formless/blob/main/CONTRIBUTING.md)
for setup, local checks, change recipes, and pull request guidance. See
[Architecture → Development](docs/architecture.md) for development details and
[packaging/README.md](packaging/README.md) for releases.

## License

Apache-2.0. Formless can download local session-naming models at runtime; their licenses are listed in [Architecture](docs/architecture.md#local-naming-models).

<p align="right">(<a href="#readme-top">back to top</a>)</p>
