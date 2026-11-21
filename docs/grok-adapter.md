# Grok Build ACP profile

Grok Build is the official xAI CLI (`npm install -g @xai-official/grok`, or the
installer at https://x.ai/cli/install.sh). The profile requires ACP v1 and a
stable version of at least 1.0.92 in `initialize.result._meta.agentVersion`.
Unknown versions and prereleases fail closed.

## Integration boundary

`local_harness::grok` supplies `program`, `args`, `environment`, `check_agent`,
`AUTH_HINT`, and `READ_ONLY_MODE`, wired into `grok::SPEC: acp::AgentSpec`.
Its final registry location is `local_harness/acp/`, agent id `grok`.
It is not wired into `HarnessKind` or command/result dispatch and cannot yet
run a topic step. No Grok-specific ACP transport or permission engine is
duplicated here. `grok::run` delegates directly to shared `acp::run`.
`SPEC.set_env` forces `GROK_DISABLE_AUTOUPDATER=1` after user overrides.
`StepAccess::ReadOnly` selects the read-only sandbox and plan mode; write steps
use `StepAccess::Write`. Access is independent of the harness selection.

Launch write steps with:

```text
grok --permission-mode default agent --no-leader stdio
```

Read-only steps add `--sandbox read-only` before `agent`. The shared launcher
must require plan mode (`session/set_config_option`, falling back to
`session/set_mode`), fail if it cannot select it, and automatically reject
edit/delete/move tools. Other kinds, including execute and Grok MCP `kind:other`
calls with `x.ai/tool.read_only:false`, reach the supplied permission handler;
`RejectAll` denies them when no user is available. This profile supplies no
bridge-tool exemption: captured metadata identifies `namespace:grok_build` and
`name:use_tool`, not a reliable MCP server identity. Plan acknowledgement alone does not
establish read-only enforcement. On Linux the sandbox needs bubblewrap.
Write steps retain Grok's default mode (`write_modes` is empty), matching the
successful live write-turn captures; no additional client capabilities are needed.

The shared ACP launcher owns per-session `session/new.mcpServers`, approval
handling, cancellation, supervised descendant cleanup, successful `end_turn`
validation, and text assembly. `cancelled` (including permission rejection)
must be a failure. There are no harness-pair restrictions: access is selected
per step, independently of the chosen harness.

`environment` disables updates and filters bridge-only variables from the
request overrides. The shared launcher must also filter those variables from
the inherited environment. Preserve user `XAI_API_KEY` and `GROK_HOME` there;
never serialize credentials into argv, logs, or debug output. Pass the selected
bridge variables only over the ACP pipe. No user/project configuration is
written. `--no-leader` prevents attachment to a shared agent process.

## User permissions and questions

Route `session/request_permission` through the shared user `PermissionHandler`,
not `RejectAll`. Preserve title, kind, option ids/names/kinds, and raw input.
`permission_raw_input` folds the captured toolCall `_meta` into raw input,
including `_meta["x.ai/tool"]`; do not discard this vendor metadata when mapping
to `HarnessPermissionRequest`. Only the user selects an option. `None` means
reject/cancel; `allow_always` is attempt-scoped and must never be persisted.
Fixtures drive the real shared ACP transport with supplied user choices,
permission acceptance/rejection/cancellation, authentication failure, and
read-only mode selection/denial. Core's `acp::Interactions` adapts these handlers
to the v2 `InteractionHandler` contract.

Grok's native question tool sends this **client request**, not an ACP permission:

```json
{"jsonrpc":"2.0","id":0,"method":"_x.ai/ask_user_question","params":{"sessionId":"smoke-session","toolCallId":"question_call","questions":[{"question":"Which smoke option should be used?","options":[{"label":"Local (Recommended)","description":"Use the local smoke fixture"},{"label":"Skip","description":"Skip the smoke fixture"}],"multiSelect":false}],"mode":"default"}}
```

`questions` translates to protocol `Question` values (1–4, labeled options,
optional descriptions/header, multi-select). `question_reply` converts ordered
`QuestionAnswer` values to Grok's question-text-keyed string map; selected labels
and free text are comma-separated. Empty/malformed requests, duplicate questions
or option labels, and invalid selections fail closed without logging payloads.
`vendor_request` uses the announced shared hook signature and calls the supplied
handler with a bare `Question` array, expecting an ordered `QuestionAnswer` array.
This matches `acp::Interactions::ask_question` and is fixture-tested through the
core `InteractionHandler`, including user decline. Its
`PermissionHandler` is the shared ACP trait providing `ask_question`. The
shared layer must validate session ownership and surface questions to the user,
and its blocking handler must respect cancellation. No question is auto-answered.

These replies were checked against real Grok, including the model's tool result:

```json
{"jsonrpc":"2.0","id":0,"result":{"outcome":"accepted","answers":{"Which smoke option should be used?":"Local (Recommended)"}}}
{"jsonrpc":"2.0","id":0,"result":{"outcome":"cancelled"}}
```

Accepted produces `User has answered your questions: ...`; cancellation produces
`User declined to answer the questions. Continue with the task using your best
judgment, or ask different questions.` This is a question decline, not necessarily
a cancelled turn. An error response instead produces `Tool ask_user_question
failed: Failed to reach the client for user question: ...`. A result lacking
`outcome` fails schema validation; valid outcome names are `accepted`,
`chat_about_this`, `skip_interview`, `cancelled`. All can still lead to `end_turn`,
so completion alone is not evidence that the answer was accepted.

Questions may also use bridge `AskUser` or a turn-end message. Do not rely on
top-level `--disallowed-tools ask_user_question` or `--rules` in ACP: a live probe
with both still received the native question request. The vendor hook remains
necessary even with prompt steering; no prompt wrapper is needed with this hook.
This profile does not grant arbitrary MCP
`kind:other` calls an exemption in read-only mode.

Other observed custom methods were notifications (no request id):
`_x.ai/mcp/init_progress`, `_x.ai/mcp/server_status`,
`_x.ai/mcp/servers_updated`, `_x.ai/mcp_initialized`, `_x.ai/queue/changed`,
`_x.ai/session/setup`, `_x.ai/session_notification`, `_x.ai/sessions/changed`.
No other non-standard client request was observed in these probes.

## Verification

Unit fixtures validate launch/access flags, environment handling, and captured
ACP initialization metadata, including empty and invalid responses. They
exercise `grok::SPEC` through the shared ACP driver. Question fixtures
also cover supplied user answers, multi-select/free text, decline, cancellation,
unknown methods, and malformed request/answer rejection.

```sh
cargo test -p formless-core --lib local_harness
FORMLESS_LIVE_GROK=1 cargo test -p formless-core --lib live_grok_acp_initialize -- --ignored --nocapture
```

Set `FORMLESS_GROK_BIN` if the binary is not on PATH. The ignored test only
initializes and checks the installed agent; it does not require credentials.
The separate ignored `live_grok_acp_turn` test requires a loopback provider and
`FORMLESS_GROK_MCP_SCRIPT` pointing to the Python MCP stub. It drives both a real
MCP invocation and a native question through the shared driver and supplied
fixture user handler. The reusable `~/grok-research/rust_smoke.py` starts the fake
provider, uses an isolated home/dummy environment key, and verifies that the MCP
call and accepted user answer actually reach the model, not just `end_turn`.

Separate pre-implementation live research used released Grok 1.0.92, isolated
`GROK_HOME`, a loopback fake OpenAI-compatible provider, dummy environment API
key, and a stdio MCP stub. Observed:

- `XAI_API_KEY` advertised `xai.api_key` and allowed `session/new` without an
  explicit `authenticate` request. Without it, the agent returned -32000.
- The stub started, received the capability through its environment, and
  received an approved `tools/call` through Grok's `search_tool`/`use_tool`.
- Both `default` and `dontAsk` emitted `session/request_permission` for MCP.
  Selecting `reject-once` prevented the call and returned `stopReason:cancelled`
  with `cancellationCategory:PermissionRejected`. Do not rely on `dontAsk` alone.
- Model `configOptions` and `session/set_config_option` worked. No modes were
  advertised, but `session/set_mode(plan)` returned success and emitted
  `current_mode_update`. A mode config option was rejected as unknown.
- After installing bubblewrap, read-only sandbox startup, model inference,
  approved MCP echo, and final `end_turn` worked. Mutation restrictions were
  not tested. A harmless builtin printf ran without an approval request.

No authenticated real-provider task was run. The reusable research scripts
and raw captures are outside the repository at `~/grok-research`; they are not
part of the adapter's automated fixtures or a prerequisite for unit tests.

## Official references

- https://docs.x.ai/build/overview
- https://docs.x.ai/build/cli/reference
- https://docs.x.ai/build/cli/headless-scripting
- https://docs.x.ai/build/features/mcp-servers
- https://docs.x.ai/build/features/permissions
- https://docs.x.ai/build/features/sandbox
- https://docs.x.ai/build/settings/reference
