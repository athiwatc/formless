# Google Antigravity adapter

Formless connects to Antigravity through the official `agy acp` server (Agent Client Protocol v1), using the installed CLI and its existing sign-in. `local_harness::antigravity::run(&HarnessRequest, &mut dyn PermissionHandler, &CancellationToken)` starts `agy acp --disable-telemetry`, negotiates ACP v1, opens a session whose `session/new.mcpServers` holds only the Formless bridge (`mcp_command`, `mcp_args`, and the `mcp_env_vars` values), sends the prompt, and returns the streamed agent message. No Antigravity settings or MCP config files are written. The bridge's variables are sent over the ACP pipe and removed from the `agy` process environment, and `agy` stderr is discarded.

The ACP client itself is shared in `local_harness::acp` (`AgentSpec`, `run`, `PermissionHandler`). Antigravity supplies `antigravity::SPEC`: the `agy acp` arguments, the 1.5.14 version floor, the sign-in hint and the `plan` read-only mode. Handlers also receive each `session/update` through `on_update`.

- **Sign-in:** Formless never calls ACP `authenticate` or handles credentials. If `agy` is not signed in, `run` returns "Antigravity is not signed in. Run `agy` ... sign-in".
- **Approvals:** every `session/request_permission` goes to the `PermissionHandler`, except tools the spec identifies as the run's Formless bridge, which are approved once (Antigravity identifies none, so none are auto-approved). `RejectAll` rejects them all, and Formless never passes `--dangerously-skip-permissions`. For `ReadOnly` steps, Formless starts `agy acp --sandbox` (agy's terminal restrictions), switches the session to plan mode, and rejects edit, delete and move tools without asking; other tool kinds still go to the handler. If the session offers no plan mode, the run fails.
- **Cancellation and cleanup:** when the token is cancelled mid-turn, Formless sends ACP `session/cancel`, answers any further approval request as cancelled, and gives up after 5 seconds. `agy` runs in its own process group (a Job object on Windows), so killing it also kills the bridge and any tool processes it started.
- **Model:** a requested model is selected with `session/set_config_option` and fails if the session does not offer it.
- **Unsupported versions:** an agent that is not ACP v1, reports no version, or is older than 1.5.14 gets a capability error asking the user to run `agy update`.
- **Headless:** `command_spec` (one-shot `agy -p`) still fails closed, because headless mode has no per-invocation MCP option.

## Verification status

| Check | Live (released `agy` 1.5.14, Linux x64) | Fixtures |
| --- | --- | --- |
| `initialize` ACP v1, agentInfo version | Observed | Captured live response |
| Unauthenticated `session/new` | Observed: JSON-RPC -32000 "Authentication required", returned before the MCP server starts | Captured live response |
| Signed-in `session/new` starts the bridge with its env | Not observed (no signed-in account) | ACP v1 schema shapes only |
| Prompt turn, approvals, plan mode, model selection | Not observed | ACP v1 schema shapes only |

Fixture coverage does not show that the released `agy` starts the bridge. To check on a machine where `agy` is already signed in, run:

```sh
FORMLESS_LIVE_AGY=1 cargo test -p formless-core live_agy_acp -- --ignored --nocapture
```

Set `FORMLESS_AGY_BIN` if `agy` is not on PATH. The test opens a session whose bridge is a shell stub, and checks that it started with the run's capability variable.

## Documented CLI capabilities

Google documents `agy -p` / `--print` for a single non-interactive prompt, `--model` for selecting the session model, and `--output-format json` or `stream-json` for machine-readable results. Set the child process working directory to the requested workspace; the CLI documentation describes conversation history as scoped to the current working directory. JSON output includes `conversation_id`, `status`, and `response`; stream-json emits `init`, `step_update`, and final `result` events. `--conversation <id>` resumes a specific conversation, while `-c` / `--continue` resumes the latest conversation for the active workspace.

These headless features are not used by Formless. The CLI MCP guide documents local stdio and remote MCP configuration through the interactive manager, plugins, a global `~/.gemini/config/mcp_config.json`, or workspace-level `.agents/mcp_config.json`. Those configuration paths are not per-attempt injection, and this adapter does not modify them.

## Project-only MCP registration

A user can register a stdio server in `<workspace>/.agents/mcp_config.json` (`command`, `args`, `env`, `cwd`) without Formless writing it. That still cannot bind a Formless run securely:

- `env` values are literal; the docs describe no variable interpolation and do not say whether the server inherits the `agy` process environment, so a per-run capability token can only be supplied by writing it into the file.
- The entry is shared by every `agy` session in that workspace, not scoped to one attempt.
- When a workspace and global server share a name, "the server that is already running keeps it", so the requested server is not guaranteed to be the one used.
- MCP tools default to Ask; headless runs need a persistent allow rule or `--dangerously-skip-permissions`.

Missing capability: a documented per-invocation way to attach a stdio MCP server, or to pass per-run environment to a configured one, for `agy -p`. This is why Formless uses ACP instead.

## Separate SDK

The Python Antigravity SDK documents per-agent MCP setup through `LocalAgentConfig(mcp_servers=[McpStdioServer(...)])`. It is a separate Python runtime and authentication path: its quickstart uses `GEMINI_API_KEY` or Vertex credentials. It is not substituted for the installed CLI adapter and does not establish reuse of `agy` cached credentials.

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
