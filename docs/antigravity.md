# Google Antigravity adapter

The `HarnessKind::Antigravity` adapter is wired into local-harness dispatch but currently fails closed. Formless supplies an MCP server for each harness attempt, but the documented `agy` CLI interface does not provide a per-invocation mechanism to inject that stdio server. The adapter therefore returns an actionable capability error; it does not construct a command, launch `agy`, or write global or workspace MCP configuration. Its output parser is available for documented JSON and stream-json output.

## Documented CLI capabilities

Google documents `agy -p` / `--print` for a single non-interactive prompt, `--model` for selecting the session model, and `--output-format json` or `stream-json` for machine-readable results. Set the child process working directory to the requested workspace; the CLI documentation describes conversation history as scoped to the current working directory. JSON output includes `conversation_id`, `status`, and `response`; stream-json emits `init`, `step_update`, and final `result` events. `--conversation <id>` resumes a specific conversation, while `-c` / `--continue` resumes the latest conversation for the active workspace.

These CLI features are documented but are not launched by this adapter. The CLI MCP guide documents local stdio and remote MCP configuration through the interactive manager, plugins, a global `~/.gemini/config/mcp_config.json`, or workspace-level `.agents/mcp_config.json`. Those configuration paths are not per-attempt injection, and this adapter does not modify them.

## Project-only MCP registration

A user can register a stdio server in `<workspace>/.agents/mcp_config.json` (`command`, `args`, `env`, `cwd`) without Formless writing it. That still cannot bind a Formless run securely:

- `env` values are literal; the docs describe no variable interpolation and do not say whether the server inherits the `agy` process environment, so a per-run capability token can only be supplied by writing it into the file.
- The entry is shared by every `agy` session in that workspace, not scoped to one attempt.
- When a workspace and global server share a name, "the server that is already running keeps it", so the requested server is not guaranteed to be the one used.
- MCP tools default to Ask; headless runs need a persistent allow rule or `--dangerously-skip-permissions`.

Missing capability: a documented per-invocation way to attach a stdio MCP server, or to pass per-run environment to a configured one, for `agy -p`.

## ACP path (not implemented)

`agy acp` is an official ACP server that reuses the cached `agy` sign-in. ACP `session/new` takes `mcpServers` (stdio `command`, `args`, `env`; stdio support is mandatory for ACP agents), and Google documents `--process-per-session` for editor-configured MCP servers. This is a per-session injection path without config writes, but it is a stateful JSON-RPC client rather than the one-shot `command_spec` / `parse_output` contract, so it is left for the core local-harness owner. It is unverified live.

## Separate SDK

The Python Antigravity SDK documents per-agent MCP setup through `LocalAgentConfig(mcp_servers=[McpStdioServer(...)])`. It is a separate Python runtime and authentication path: its quickstart uses `GEMINI_API_KEY` or Vertex credentials. It is not substituted for the installed CLI adapter and does not establish reuse of `agy` cached credentials.

## Verification status

No `agy` or `antigravity` executable was installed on the research machine, so CLI help and a live run were unavailable. The adapter’s output parser is based on the official CLI documentation examples; it accepts only explicit `SUCCESS` envelopes/results and preserves the documented response envelope or stream events.

## Official Google sources

- [CLI reference](https://antigravity.google/docs/cli/reference/)
- [Headless mode and output formats](https://antigravity.google/docs/cli/headless/)
- [Managing conversations](https://antigravity.google/docs/cli/conversations/)
- [Resume command](https://antigravity.google/docs/cli/commands/resume/)
- [MCP configuration](https://antigravity.google/docs/mcp/)
- [Antigravity SDK overview and authentication](https://antigravity.google/docs/sdk/overview/)
- [SDK MCP integration](https://antigravity.google/docs/sdk/mcp/)
- [SDK session persistence](https://antigravity.google/docs/sdk/lifecycle/)
- [Other editors (ACP)](https://antigravity.google/docs/ide/extensions/acp/)
- [Settings: MCP name collisions](https://antigravity.google/docs/settings/)
- [Permissions](https://antigravity.google/docs/permissions/)
- [ACP session setup](https://agentclientprotocol.com/protocol/session-setup)
