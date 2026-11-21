# Local Pi adapter

The Pi adapter implements the shared `local_harness::HarnessRequest` contract. It does not own process supervision: the caller supplies an attempt-private runtime directory and removes it after the run.

## Use

Stage the embedded extension in the attempt-private directory, then build and run the command without a shell:

```rust
let extension = local_harness::pi::stage_extension(&attempt_private_dir)?;
let spec = local_harness::pi::command_spec_with_extension(&request, &extension)?;
let output = spec.command().output()?;
if !output.status.success() {
    anyhow::bail!("Pi CLI exited unsuccessfully");
}
let result = local_harness::parse_output(HarnessKind::Pi, &String::from_utf8(output.stdout)?)?;
```

The caller must also check the child exit status and retain stderr separately. `parse_output` validates the JSONL completion events and the extension's explicit bridge-registration message; it does not prove that an MCP tool was called successfully. A missing or failed extension therefore cannot be treated as a successful parsed run.

Pi runs with `--mode json --no-session --no-extensions`, explicitly loads `builtin:mcp` and the staged extension, and receives the prompt as one argument after `--`. The per-attempt MCP server uses a random Formless name, stdio `command`/`args`/`cwd`, the request's selected environment variables, and direct tool exposure. The adapter does not modify Pi configuration. Pi continues to read the user's normal settings and authentication; `--no-session` avoids persisting the run's session.

Pi's configured MCP servers remain configured. The random Formless server name avoids a name collision; the Review tool allowlist keeps other MCP tools unavailable to that run, while Build uses Pi's normal tool selection.

Pi treats an argument beginning with `@` as a file attachment even after `--`. For these prompts only, the adapter prefixes the argument with a newline and sets `restoreLeadingAt` in its extension metadata. The extension removes that one transport newline from the first input using Pi's documented `input` event transformation, so the model receives the original prompt, including literal `@` mentions. Other prompts, including those already beginning with whitespace, are unchanged. Pi also supports piped stdin, but the shared command specification does not carry stdin data; this reversible escape needs no shared launcher changes or prompt files.

`FORMLESS_PI_MCP_CONFIG` is an internal per-attempt JSON environment value containing the server name, command, arguments, working directory, allowed environment-variable names, and prompt-escape flag. Values named by `mcp_env_vars` are not copied into this JSON value; the embedded extension copies those values from the child's environment into the MCP server configuration. Do not put credentials in command arguments. This variable must not be supplied in `HarnessRequest.environment` or `mcp_env_vars`, and the adapter does not log its contents.

For the `Review` role, Pi's built-in tool allowlist is limited to `read`, `grep`, `find`, and `ls`, plus the per-attempt MCP server. This is a tool allowlist, not an OS sandbox: Pi and its extension still run with the host process's permissions, and the MCP capability token remains the server-side authority boundary.

The extension feature-detects `pi.registerMcpServer()` and requires Pi 0.99.0 or newer. Pi 1.6.0 was verified locally as the current official stable release during adapter research. A normal successful run emits `agent_settled` and a final assistant `message_end`; the parser ignores streaming deltas, thinking blocks, and tool-call content, and rejects missing bridge evidence, aborts, error stop reasons, and incomplete tool turns.

Official references: [CLI](https://docs.pi.dev/latest/cli), [CLI integration](https://docs.pi.dev/latest/cli-integration), [JSON event stream](https://docs.pi.dev/latest/json), [extensions](https://docs.pi.dev/latest/extensions), and [MCP servers](https://docs.pi.dev/latest/mcp).
