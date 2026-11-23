# ACP harness profiles

`local_harness::acp_agents` supplies specs and a single-entry registry for these
13 harnesses. `agent_ids()`, `spec(id)`, `run(id, request, handler, cancel)` and
`open_session(id, request)` are available to callers. Topic outlines and attempt
inputs validate `HarnessKind::Acp` selectors against this registry.
`local_harness::run(request, interaction_handler, cancel)` dispatches ACP and
Antigravity sessions through the core interaction handler; Antigravity retains
its own spec. Core topic steps dispatch registered ACP profiles when
`HarnessKind::Acp` is paired with a valid `acp_agent`. The named Gemini topic
fixture covers launch, permission forwarding, completion, and cancellation
using a scripted ACP process. Topic fixtures also cover readiness-selected
executable identity, explicit overrides, workspace decoys, and fail-closed
missing/disappearing executables. Installed-CLI checks cover initialize/readiness
only; no authenticated model turns or native Windows .cmd execution have been
verified.

`HarnessRequest.access` (`StepAccess::ReadOnly` or `Write`) controls mode/argv
selection. Permission IDs, names, and kinds reach the interaction handler unchanged;
vendor tool-call `_meta` is kept under `raw_input._meta` by the shared adapter.
An unanswered permission is rejected. The adapter
does not cache approvals; core's AllowAlways cache is scoped to the attempt, and
each run starts a fresh agent process/session. Text messages/thoughts, tool calls,
and plans are mapped to core updates using the shared `acp::Interactions` adapter.

Install each vendor's CLI separately and put its executable on PATH, or supply
`HarnessRequest.executable`. Formless never installs agents, handles credentials,
or passes credentials in argv. Sign in using the vendor CLI or configure its
environment outside Formless. Agent names and ACP v1 are checked, without minimum
version requirements; Cursor does not report agentInfo and is protocol-checked only.

## Commands, access, and live evidence

Every row was **live-verified 2026-11-21: initialize**, unauthenticated, on the
listed version. **Session/new was also live-verified successfully for Junie,
Auggie, and Devin only. Prompt turns were not live-tested for any agent.**
Other session/new calls returned authentication/configuration errors, not working
sessions. The read-only settings below come from captured help/session options;
they have fixture coverage, not live prompt-enforcement verification.
The installed binaries also accepted the final ReadOnly launch argv in live
initialize-only checks. Kimi's rejected `acp --plan` flag was removed before
the successful 13-agent check.

| Agent | ACP launch command | Sign-in/configuration outside Formless | Review/read-only behavior | Tested version |
|---|---|---|---|---|
| gemini | `gemini --acp` | Run `gemini` to sign in with Google, or set `GEMINI_API_KEY` | Help-documented `--approval-mode plan`; try ACP `plan`, otherwise permission filter | 0.70.0 |
| copilot | `copilot --acp --stdio` | `copilot login` | Help-documented `--plan`; try ACP `plan`, otherwise permission filter | 1.0.114 |
| cursor | `cursor-agent acp` | `cursor-agent login` or `CURSOR_AUTH_TOKEN` | Help-documented `--mode plan`; try ACP `plan`, then `ask`, otherwise permission filter | 2026.11.19-f005e9d |
| droid | `droid exec --output-format acp` | Run `droid` to sign in, or set `FACTORY_API_KEY` | No extra flags/mode; permission filter. Exec help describes read-only by default; ACP enforcement not live-tested | 1.17.1 |
| kiro | `kiro-cli acp` | `kiro-cli login` or `KIRO_API_KEY` | No documented read-only flag/mode in captured help; permission filter | 3.5.0 |
| junie | `junie --acp=true` | Run `junie` and sign in with Junie Account in browser | Select `plan` in the actual `mode` option; fail closed if unavailable/refused | npm 3110.7.0; agent 26.9.7 (3110.7) |
| qwen | `qwen --acp` | Run `qwen`, use `/auth` or configure model/`OPENAI_API_KEY` | Help-documented `--approval-mode plan`; try ACP `plan`, otherwise permission filter | 0.28.3 |
| kimi | `kimi acp` | `kimi login` | Try ACP `plan`, otherwise permission filter; no extra argv (`acp --plan` is rejected) | Kimi Code CLI 2.7.2 |
| vibe | `vibe-acp` | Run `vibe` to sign in through Mistral AI Studio, or set `MISTRAL_API_KEY` | No read-only flag/mode in captured help; permission filter | 3.1.4 |
| auggie | `auggie --acp` | `auggie auth login` | No built-in read-only mode verified; permission filter (extension-specific `--plan` is not assumed) | 1.2.2 |
| goose | `goose acp` | `goose configure`; configure `GOOSE_PROVIDER`, model and provider credentials | No read-only flag/mode in captured help; permission filter | 1.60.2 |
| cline | `cline --acp` | `cline auth` | Help-documented `--plan`; try ACP `plan`, otherwise permission filter | 3.0.75 |
| devin | `devin acp` | `devin auth login` | Select ACP `plan`, then `ask`; fail closed if unavailable/refused | 3000.14.30821 |

Review-only argv is appended to the launch command shown. No auto-approve,
yolo, trust-all, or bypass flag is passed. The shared review permission filter
rejects `edit`, `delete`, and `move` without asking; other kinds, including
`execute` and unknown/missing kinds, go to the caller's permission handler.
These profiles do not identify any bridge tools for automatic approval.
An agent's internal auto-approved actions cannot be controlled by a permission
callback; this is not an OS sandbox guarantee.

Write runs explicitly select Junie's captured `default` mode and Devin's
captured `accept-edits` mode, failing if they cannot be set. All other write-mode
lists are empty and keep the agent's default. Devin never selects `bypass`.

Junie's live `mode` option has no category, while `brave_mode` has category
`mode`. Selection prefers option ID `mode`, then category `mode`, and selects
only a value in the spec's candidate list. It never chooses `on`, `brave-auto`,
or `bypass` as a review mode. Category-only options remain supported for Antigravity.

For unadvertised candidates the shared runner tries `session/set_mode` with the
first candidate. Non-strict profiles tolerate rejection and retain their argv
settings plus permission filter; Junie and Devin do not. Goose reports missing provider
configuration as JSON-RPC -32603, so its spec maps that code as well as -32000 to
its setup hint. Other -32603 errors from Goose also receive that hint. Offered
`authMethods` IDs are appended to setup errors; credentials/error payloads are
not echoed by the shared runner.

## Availability

`acp_agents::availability(id, executable, environment)` returns core's
`HarnessAvailability` for a registry agent. It checks executable presence on PATH
(the supplied environment's PATH takes precedence), or an explicit absolute
executable path. Core's ListHarnesses and named-agent readiness checks honor
`FORMLESS_TOPIC_ACP_EXE` as an absolute executable override; otherwise each
registry agent's command is resolved separately on PATH. Installed remains true if the CLI is present but cannot
answer the readiness check.

The probe starts the normal ACP command, sends initialize then `session/new`
with an empty `mcpServers` list in a fresh private temporary cwd, and kills the
process group afterward. The combined RPC deadline is eight seconds. No prompt,
authenticate request, bridge, credential arguments or config edits are made.
Normal run environment stripping is reused, including bridge variables. Version
comes from `agentInfo.version`, falling back to `_meta.agentVersion`; Cursor may report no version.

A profile's authentication/configuration error code means `signed_in=false` with
its sign-in hint. Session creation means `signed_in=true` only for profiles whose
unauthenticated smoke test failed at that point. Junie, Auggie and Devin open
unauthenticated sessions, so successful session creation reports unknown: sign-in
is not verifiable before a prompt. Timeouts, incompatible initialize responses,
malformed sessions and other errors also report unknown with a diagnostic. This
does not prove that a future prompt/model call will succeed. The registry entry's
`session_auth` flag records whether successful session creation is meaningful.

Core's `ListHarnesses` handler expands ACP into one availability entry per registry
id, each with `kind=Acp` and `acp_agent` set. Registry probes run concurrently on
blocking workers, capped at ten seconds each (the ACP handshake itself uses its
eight-second deadline). Single-agent readiness uses the same dispatch.

The named Gemini ordinary-question fixture uses the same supported
`user_only=false` question shape in two cases through scoped Formless MCP and a
scripted ACP stub. An exact approved choice settles through
`topic_answer_question` with its source message ID, exact quote, and nonempty
explanatory basis. An unsupported offered choice, supplied with a nonempty
basis and the exact source message ID and quote, is rejected; the question
stays pending until the human reply reaches the stub.
This covers the Formless tool and transport path, not an authenticated or live
model turn or a user-only question API.

Topic readiness resolves the ACP override or agent command from the daemon's
environment once, canonicalizes the selected absolute path, and probes that
exact path. Its internal per-step result carries both availability and the
resolved executable into `HarnessRequest.executable`; it also carries the
selected absolute invocation path captured before canonicalization as internal
`executable_argv0`. On Unix, native executables receive the selected invocation
path as `argv[0]`, preserving native symlink-dispatched shims. For shebang
scripts, the kernel passes the canonical script path to the interpreter, so the
script's `$0` is the canonical path rather than the symlink alias; script-based
basename dispatch is not preserved. Readiness and topic launch use that same
canonical script path. Launch does not reread the override or PATH, so a
workspace-local executable cannot replace the readiness selection. This
identity is internal: public
availability and ListHarnesses retain the existing `HarnessAvailability` shape.
If preparation cannot return an executable identity, including on timeout or
worker error, the step stops before launch with the existing retry/substitute
guidance. If the selected path disappears before spawn, launch fails without
fallback. Each retry or approved substitute gets fresh preparation. Unknown
authentication remains ready only when installed and `signed_in` is not
`Some(false)`. This pins path selection, not executable contents.

## Installation observations

- Use `@moonshot-ai/kimi-code` for Kimi 2.7.2. Its executable is `kimi`, not
  `kimi-code`. Python `kimi-cli` 1.52.0 is a deprecation stub and exits without
  speaking ACP; it is not compatible with this profile.
- Cline 3.0.75's scratch npm installation omitted the `.bin/cline` symlink;
  launching the package's executable `node_modules/cline/bin/cline` worked.
  Override `HarnessRequest.executable` if the installed CLI is not on PATH.
- Junie's npm launcher searches for its installation under HOME. With an isolated
  HOME, point the live test at the installed version's actual `junie` binary,
  not the npm launcher or home-relative update shim.
- Kimi's root help lists `--plan`, but `kimi acp --plan` rejects it as an unknown
  option. No read-only argv is added; its ACP `plan` mode is tried instead.
- Cursor's initialize response contains neither agentInfo nor a version in
  `_meta`; its tested version comes from `--version`.
- Auggie's unauthenticated session advertises only `model` with empty choices;
  Devin also returns empty model choices. Do not assume a requested model exists.
- Droid's undocumented `acp-daemon` output format also negotiated successfully;
  the profile uses the help-documented `acp` format instead.

## Tests

`acp_agents/captured.json` contains real initialize and session/new/error
responses from the unauthenticated smoke run. Droid's transient device-pairing
code is redacted. Successful session responses are used unchanged; authenticated
session shapes for agents blocked on sign-in and all prompt/permission/update
traffic are explicitly synthetic. Each profile checks launch argv, identity,
setup errors/auth method IDs, prompt completion, update forwarding, verbatim
permission options, and review permission rejection without running a real agent.

Run harness fixtures with `cargo test -p formless-core local_harness --locked`.
Run the named Gemini ordinary-question fixture with
`cargo test -p formless-bin --test topic_mcp_stdio named_gemini_topic_question_uses_grounding_and_human_fallback --locked`.
For the ignored **live initialize-only** test, place installed executables or
symlinks in a directory, include any interpreter needed by their launchers in PATH,
and run:

```sh
FORMLESS_LIVE_ACP_BIN_DIR=/absolute/path/to/binaries \
  cargo test -p formless-core live_acp_agents_initialize_installed_binaries --locked -- --ignored --nocapture

# Also check that each CLI accepts the read-only argv in ACP mode:
FORMLESS_LIVE_ACP_BIN_DIR=/absolute/path/to/binaries \
  cargo test -p formless-core live_acp_agents_initialize_read_only_installed_binaries --locked -- --ignored --nocapture
```

The live test clears the child environment, isolates HOME/XDG, skips missing
binaries, requires at least one binary, and times out each initialize after 20s.
It never authenticates, creates an ACP session, or sends a prompt.

The separate ignored **live availability** test runs initialize and session/new
using the public readiness probe. It clears inherited credentials/environment,
retains PATH for interpreters, and gives each agent a fresh isolated HOME/XDG tree:

```sh
FORMLESS_LIVE_ACP_BIN_DIR=/absolute/path/to/binaries \
  cargo test -p formless-core live_acp_agents_availability_installed_binaries --locked -- --ignored --nocapture
```

It never sends a prompt or authenticates. Fixture tests cover real captured auth
errors (including Goose -32603), version reporting, authenticated-shaped success,
Junie/Auggie/Devin's ambiguous success, missing/non-executable binaries, unknown
errors, malformed sessions, deadlines, temporary-cwd cleanup and descendant cleanup.
