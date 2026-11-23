<a id="readme-top"></a>

<div align="center">

<h1>Formless</h1>

<p><strong>One conversation that runs your AI agents.</strong><br/>
Talk in Main Chat, organize work into Topic Chats, and let Formless drive the coding agents you already have installed.</p>

<p>
  <a href="https://github.com/athiwatc/formless/blob/main/Cargo.toml"><img src="https://img.shields.io/badge/license-Apache--2.0-blue.svg" alt="License: Apache-2.0"></a>
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
such as Claude Code or Codex. Their questions, permission requests and results
come back into the same conversation.

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
| **Questions and permissions** | Anything a harness asks mid-run surfaces in Formless. Only you can grant a permission; an unanswered or cancelled request is denied. |
| **Scoped context** | A harness reads its own topic branch — the topic and its ancestors — not Main Chat or sibling topics, unless you allow wider context for that topic. |
| **Nothing auto-merges** | Steps produce Git revisions, files or text. Formless never merges or pushes. |

## Harnesses

Integrated adapters use each harness's official local interface and a private,
per-step MCP bridge back to the topic. Install and sign in to a harness the
usual way; Formless does not ship or log in to them for you.

Current topic-step adapters are Claude Code and Codex, using their official
non-interactive CLIs. Pi, OpenCode, Antigravity, Grok Build, Muse Code and
additional ACP agents are still in progress.

Either integrated harness can take any step, including the same harness on
both sides of a hand-off. A harness's own permission modes are not an OS
sandbox; the isolated worktree or directory is the boundary.

## Install

```bash
# macOS / Linux — latest release, checksum-verified
curl -fsSL https://raw.githubusercontent.com/athiwatc/formless/main/scripts/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"

formless   # starts the daemon and opens the setup wizard
```

Other ways in: build from source with `cargo install --git https://github.com/athiwatc/formless --locked formless-bin`,
use the `.deb`/`.rpm` from a [release](https://github.com/athiwatc/formless/releases),
run the daemon in [Docker](docs/docker.md), or follow the [Windows guide](docs/windows-install.md).
The wizard needs a model endpoint with tool calling: OpenAI-compatible Chat
Completions or Responses, or native Anthropic Messages. Ollama and LM Studio
work locally without a key. Full details are in [Using Formless](docs/usage.md).

## Clients

One daemon owns all state; every client is a thin view of it, so closing one never stops work.

| Client | Start it |
|---|---|
| Terminal UI | `formless` — the current Session and Run interface; topic navigation is not integrated yet |
| Desktop app | native GPUI app in [`crates/formless-desktop`](crates/formless-desktop) |
| Browser | `formless relay enable` — end-to-end encrypted, works on phones ([relay](relay/README.md)) |
| CLI | `formless run "…"`, `formless chat` — scriptable, with JSON output |

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
                                                         └─ Harness steps ─▶ Claude · Codex · Pi · ACP …
```

Architecture, the Tool system and self-optimisation: [docs/architecture.md](docs/architecture.md).
Normative behaviour: [SPEC.md](SPEC.md). Vocabulary: [GLOSSARY.md](GLOSSARY.md).

## Security and privacy

- **Local-first.** Conversations, topics and Tools live in a local SQLite store; the daemon listens only on an owner-only socket or named pipe.
- **No telemetry.** Network traffic is your providers, the harnesses you run, an optional relay and optional local-model downloads.
- **Deny by default.** Tools reach only the capabilities they declare; sensitive calls can require approval (`formless --secure`).
- **Harness boundaries.** Harnesses get a scoped, per-attempt bridge: no approval, start, merge or push operations, and no way to decide their own permissions.

More: [docs/capabilities.md](docs/capabilities.md) · [docs/troubleshooting.md](docs/troubleshooting.md).

## Documentation

| | |
|---|---|
| [Using Formless](docs/usage.md) | Install, setup, TUI, headless runs, relay, providers, profiles |
| [CLI reference](docs/cli.md) | Every command and global flag |
| [Architecture](docs/architecture.md) | Daemon, Tools, Sessions and Runs, self-optimisation, development |
| [Keys and commands](docs/keys.md) | Generated keyboard reference |
| [MCP](docs/mcp.md) · [Skills](docs/skills.md) · [Jobs](docs/jobs.md) | Extending Formless |
| [Gallery](docs/gallery.md) | Example tasks |
| [SPEC.md](SPEC.md) | The specification |

## Contributing

Open an issue for anything larger than a typo. Behaviour changes go in
[SPEC.md](SPEC.md) first, and no behaviour lives only in a client. Before a PR:

```bash
cargo fmt --all && cargo clippy --workspace --all-targets && cargo test --workspace
```

See [Architecture → Development](docs/architecture.md) for CI and test details, and [packaging/README.md](packaging/README.md) for releases.

## License

Apache-2.0. Formless can download local session-naming models at runtime; their licenses are listed in [Architecture](docs/architecture.md#local-naming-models).

<p align="right">(<a href="#readme-top">back to top</a>)</p>
