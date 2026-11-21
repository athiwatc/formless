# Local Pi adapter

The Pi adapter implements the shared `local_harness::HarnessRequest` contract. It does not own process supervision: the caller supplies an attempt-private runtime directory and removes it after the run.

## Use

Stage the embedded extension in the attempt-private directory, then build and run the command without a shell:

```rust
let extension = local_harness::pi::stage_extension(&attempt_private_dir)?;
let result = local_harness::pi::run(&request, &extension, handler, cancel).await?;
```

`pi::run` owns one Tokio child process, its pipes, dialog futures and shutdown. The launcher still owns extension staging and directory cleanup. Central `command_spec(Pi)` returns an actionable error pointing to `pi::run`; `command_spec_with_extension` is the lower-level RPC command builder, not a complete executor. The shared `InteractionHandler` remains core-owned.

Pi runs with `--mode rpc --no-session --no-extensions`, explicitly loads `builtin:mcp` and the staged extension, and receives the prompt literally in a JSON `prompt` command on stdin, never in argv. The response must acknowledge `success:true`. The adapter obtains `session_id` from RPC `get_state`'s `data.sessionId`. The per-attempt MCP server uses a random Formless name, stdio `command`/`args`/`cwd`, the request's selected environment variables, and direct tool exposure. The adapter does not modify Pi configuration. Pi continues to read the user's normal settings and authentication; `--no-session` avoids persisting the run's session.

Pi's configured MCP servers remain configured. The random Formless server name avoids a name collision; the ReadOnly tool allowlist keeps other MCP tools unavailable to that run, while Write uses Pi's normal tool selection.

RPC prompt text bypasses CLI `@file` expansion, including prompts beginning with `@`. There is no newline transport escape or extension input rewrite.

Pi 1.6.0 RPC does not expose its internal `expandPromptTemplates:false` option: extension slash commands, skills, templates and input interception retain their normal Pi semantics. Prompt acknowledgement must have `data.disposition:"started"`; `handled` (for example, a completed extension command) and other dispositions fail immediately instead of waiting indefinitely for an agent run that never starts.

`FORMLESS_PI_MCP_CONFIG` is an internal per-attempt JSON environment value containing the server name, command, arguments, working directory, and allowed environment-variable names. Values named by `mcp_env_vars` are not copied into this JSON value; the embedded extension copies those values from the child's environment into the MCP server configuration. Do not put credentials in command arguments. This variable must not be supplied in `HarnessRequest.environment` or `mcp_env_vars`, and the adapter does not log its contents.

For `StepAccess::ReadOnly`, Pi's built-in tool allowlist is limited to `read`, `grep`, `find`, and `ls`, plus the per-attempt MCP server. `StepAccess::Write` uses normal tool selection. These access levels replace the former Review and Build roles, respectively. This is a tool allowlist, not an OS sandbox: Pi and its extension still run with the host process's permissions, and the MCP capability token remains the server-side authority boundary.

## Questions, updates and shutdown

`extension_ui_request` dialogs map to `InteractionHandler::ask_question`:

| Pi method | Formless question | RPC response |
| --- | --- | --- |
| `confirm` | Title header, message text, single-select Yes/No | `confirmed:true` for Yes, `false` for No |
| `select` | Title and the supplied option labels, single-select | `value` with the selected label |
| `input` / `editor` | Title, no options | `value` with the entered text |

Responses use `type:"extension_ui_response"` and the dialog's `id`. No answer, an invalid answer, or cancellation sends `cancelled:true`. A request's optional positive timeout races the handler; expiry cancels the handler and sends nothing, because Pi resolves the dialog itself. Non-positive timeouts do not create a deadline; in particular, Pi's `timeout:0` means no timeout, not an expired dialog. Notifications become `HarnessUpdate::Message` without any response. Unsupported fire-and-forget UI methods are ignored. Dialog futures and the stdin writer are independent of stdout reading; stderr is continuously drained into an 8 KiB tail. Only bounded stderr diagnostics accompany execution errors; the adapter does not log prompts, answers or MCP configuration.

Completed assistant text and thinking blocks become Message and Thought updates, respectively; streaming deltas are not duplicated. Tool execution starts become ToolCall updates. Pi core does not prompt for bash/write permission, so `request_permission` is deliberately unused. A permission policy must be implemented explicitly by a Pi extension; dialogs from such an extension are still relayed as questions.

Success requires an acknowledged prompt, explicit bridge-registration evidence, a final successful assistant `message_end`, terminal `agent_end`, and non-aborted `agent_settled`, validated using the same `parse_output` reducer as the JSONL parser. This does not prove that an MCP tool was called successfully. The runner closes stdin, waits up to three seconds for process exit and rejects non-zero status. Early exit, invalid JSON and incomplete output fail with a bounded stderr tail.

Cancellation sends `cancelled:true` for every open dialog before `abort`, also cancels dialogs arriving during abort, and allows three seconds for an abort response or settlement. It returns a cancellation error, never a partial success. On errors or cancellation the runner terminates the process; on normal completion it closes stdin and waits up to three seconds for exit before killing and reaping the child.

On Unix, Pi starts in its own process group. Forced termination and dropping the run future stop that group, discover and stop descendants with `/bin/ps` (parents first, up to 64 generations), then kill descendants and their detached groups before the Pi group. This is best-effort cleanup for Pi's detached bash groups, not a sandbox or containment of processes that already daemonized and reparented before cleanup. The waitable leader is retained until cleanup completes to avoid signalling a reused group ID. Other platforms retain direct-child kill-on-drop behavior.

The extension feature-detects `pi.registerMcpServer()` and requires Pi 0.99.0 or newer. Pi 1.6.0 was verified locally during adapter research. Live RPC relay can be exercised with the ignored `local_harness::pi::rpc_tests::live_rpc_question_relay` test using isolated smoke-only configuration; ordinary product runs retain normal user authentication.

Official references: [RPC](https://docs.pi.dev/latest/rpc), [RPC commands](https://docs.pi.dev/latest/rpc-commands), [RPC extension UI](https://docs.pi.dev/latest/rpc-extension-ui), [JSON event stream](https://docs.pi.dev/latest/json), [extensions](https://docs.pi.dev/latest/extensions), and [MCP servers](https://docs.pi.dev/latest/mcp).
