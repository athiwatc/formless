# Google Antigravity adapter

The `HarnessKind::Antigravity` adapter is wired into local-harness dispatch but currently fails closed. Formless supplies an MCP server for each harness attempt, but the documented `agy` CLI interface does not provide a per-invocation mechanism to inject that stdio server. The adapter therefore returns an actionable capability error; it does not construct a command, launch `agy`, or write global or workspace MCP configuration. Its output parser is available for documented JSON and stream-json output.

## Documented CLI capabilities

Google documents `agy -p` / `--print` for a single non-interactive prompt, `--model` for selecting the session model, and `--output-format json` or `stream-json` for machine-readable results. Set the child process working directory to the requested workspace; the CLI documentation describes conversation history as scoped to the current working directory. JSON output includes `conversation_id`, `status`, and `response`; stream-json emits `init`, `step_update`, and final `result` events. `--conversation <id>` resumes a specific conversation, while `-c` / `--continue` resumes the latest conversation for the active workspace.

These CLI features are documented but are not launched by this adapter. The CLI MCP guide documents local stdio and remote MCP configuration through the interactive manager, plugins, a global `~/.gemini/config/mcp_config.json`, or workspace-level `.agents/mcp_config.json`. Those configuration paths are not per-attempt injection, and this adapter does not modify them.

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
