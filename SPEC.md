# Formless — Full Application Specification (v1)

**Status:** Accepted (grilling session Q1–Q67). **Audience:** implementers of v1.
**Companion docs:** [GLOSSARY.md](GLOSSARY.md) (canonical language — spec uses those terms exactly), ADRs in `docs/adr/`.

Formless is a general-purpose agent harness that boots with a fixed set of
built-in Tools (§7.1) and builds every other Tool it needs, for any use case —
it does not specialise in coding. A Daemon Core owns the agent loop, Tools,
Sessions, and state; a TUI (v1) and a headless CLI are clients of that core.
The harness self-optimises its Tools, driven only by measured Tool Success
Rate. A hosted Web frontend via an encrypted Relay ships in v2 (see
Non-Goals), and the Daemon Core is designed so that future frontends attach
without redesign.

## 1. Principles

1. **A fixed boot set.** A fresh install exposes only the built-in Tools of
   §7.1: the registry primitives, the Pending Invocation handles, metrics and
   artifact reads, and the MCP, skills, memory, planning and user-facing ones,
   all implemented by the daemon. No starter *domain* Tool ships; everything
   else is built, persisted, and reused.
2. **Measured, never vibes.** Self-Optimisation is triggered and judged only
   by recorded Invocation outcomes. No silent or untested mutation, ever.
3. **No time-based killing.** Runs and Invocations have no step/token/time
   limits. The only terminators are completion, fault, or the user.
4. **Deny by default.** Tools run sandboxed with only their declared
   Capabilities. The runtime offers nothing a Tool was not granted.
5. **Local-first, zero telemetry.** Nothing leaves the machine except calls
   to the user-configured provider endpoint, the user-enabled Relay, and
   explicit user-run commands (e.g. `formless update check`). No automatic
   update checker, no phone-home.
6. **One core, many clients.** All behaviour lives in the Daemon Core. TUI,
   CLI, and future frontends are interchangeable clients of one protocol.
7. **Everything is versioned and reversible.** Tools, promotions, config, and
   Session checkpoints keep history; destructive actions are user-only and
   confirmed.

## 2. Architecture

### 2.1 Workspace and binary

- Cargo workspace, Apache-2.0, producing **one binary**: `formless`.
- Crates:
  - `formless-core` — Daemon Core: agent loop, provider client, Session/Run
    management, Tool registry, metrics, optimiser, storage, compaction.
  - `formless-runtime` — QuickJS sandbox (QuickJS-NG via `rquickjs`) and the
    Capability-gated host API, including streaming primitives.
  - `formless-protocol` — shared daemon↔client types (requests, streamed
    events, errors), versioned (see §13).
  - `formless-tui` — ratatui TUI client.
  - CLI commands live in the binary entrypoint (no separate crates/binaries).
- The daemon is not a separate executable: it is `formless` running in daemon
  mode, spawned by clients (see §9). No system service on any OS.

### 2.2 Processes and clients

- One Daemon Core per data dir (single instance, enforced by lock; §9.4).
  Optional `--profile <name>` runs an additional isolated daemon + data dir.
- Clients: TUI (v1 UI), CLI (§12), and any future frontend. Clients are thin:
  they render streamed events and send intents; they hold no authoritative
  state and can disconnect/reattach without losing work (daemon persists all
  state; killing a TUI never kills the daemon or a Run).

### 2.3 Transport

- macOS/Linux: Unix domain socket in the data dir (mode `0700` dir, socket
  accessible only to the owning user).
  - A data dir too deep for a socket address (104 bytes on macOS, 108 on
    Linux) uses `$XDG_RUNTIME_DIR/formless/<hash>.sock`, or
    `/tmp/formless-<user>/<hash>.sock`, instead. The hash is of the data
    dir, so every client finds the same path. The folder is created
    `0700` and must belong to the user; `formless doctor` reports it when
    it doesn't.
- Windows: a named pipe, `\\.\pipe\formless-<profile>-<hash of the data
  dir>`, so two data dirs never share a daemon, with an ACL restricted to
  the owning user. It has
  a protected DACL granting access only to the current user's SID and
  SYSTEM, and remote clients are rejected. The auto-started daemon is
  detached (`DETACHED_PROCESS`, its own process group, no console
  window), so closing the launching terminal never stops it.
- Identical framed protocol over both transports. No localhost-TCP fallback
  for clients. Local access control = OS filesystem/pipe permissions only
  (single-user trust boundary; see Security, §14). The one TCP listener is
  the opt-in local web UI (§12, `formless web`). It binds 127.0.0.1 only,
  is off by default, and bridges into the socket or pipe.

## 3. Concepts (summary; GLOSSARY.md is canonical)

| Term | Meaning in one line |
|---|---|
| Built-in Tool | A Tool the daemon implements itself, present from first boot (§7.1): never versioned, never optimised, never retirable. |
| Bootstrap Tool | `create_tool`, the built-in that writes every other Tool. Immutable, never optimised. |
| Tool | Persisted, versioned, sandboxed JS capability built via `create_tool`. |
| Session | Resumable conversation + Run history; many run concurrently. |
| Run | One unit of work in a Session (user message → final answer). Unlimited. |
| Invocation | One execution of one Tool version with one input. Recorded + measured. |
| Pending Invocation | Invocation past the soft threshold (300s); still running, handle held by agent. |
| Steer | Message during a Run, injected at the next step boundary (Enter default). |
| Capability | Declared, scopable permission a Tool needs. Ceiling, never exceeded. |
| Candidate | Optimiser-proposed Tool version; must win sandboxed replay to be promoted. |
| Promotion | Replacing the current Tool version with a winning Candidate; rollback kept. |
| Retired Tool | Agent-removed Tool: uncallable, retained, restorable; user-only purge. |
| Compaction | Persisted summary checkpoint replacing old history/tool output. |
| Daemon Core | Long-lived process owning loop, Tools, Sessions, state. |
| Relay | Hosted encrypted intermediary for the Web frontend (v2, §12). |

## 4. Provider (v1: OpenAI-compatible only)

### 4.1 Scope

- v1 supports exactly one provider type: any **OpenAI-compatible** endpoint
  speaking **Chat Completions**: `POST {base_url}/chat/completions` with
  Bearer auth and SSE streaming, and `GET {base_url}/models` for discovery.
- `base_url` is used **verbatim** (the daemon appends only `/chat/completions`
  and `/models`); non-`/v1` layouts work. Free-form extra headers (e.g.
  organisation/project) are configurable per provider and sent on every call.
- v2: `provider.api_style = "responses"` speaks the Responses API
  (`POST {base_url}/responses`, converted input/tools, streamed
  reasoning/function-call events). `provider.tool_mode = "prompt"` is a
  prompt-based tool-calling fallback for models without native
  tool-calling (tools are described in-band; replies are parsed for a
  JSON tool-call object, buffered so the JSON never streams to the
  user); the probe auto-detects it during onboarding.
  `provider.fallback_models` lists models tried in order when the
  selected model fails with a model/retryable error (failover routing).
  With `provider.model = "auto"`, the daemon instead **auto-selects**
  per Run from `provider.routing_models` (or the fallback list) using
  only measured local outcomes — recorded provider successes/failures
  and latency per model, smoothed toward a neutral prior with a small
  exploration bonus for under-sampled models; optional
  `routing_fast_models` / `routing_strong_models` subsets constrain
  simple vs complex tasks (`routing_complex_chars`). No vendor ranking
  is hard-coded. Stats are visible in the Metrics view and `doctor`.
  **Deprecated (§4.6):** a global `provider.model = "auto"` is migrated on
  daemon start. A `primary` role is created with `model = "auto"` and the
  former routing pool as `candidates`, `provider.model` becomes
  `role:primary`, the file is rewritten, and every client gets the warning
  as a note when it connects. A Session set to `auto` uses `role:primary`
  when that role exists. The global per-call pool remains only for an
  `auto` role without `candidates`.
- v2 subscription/OAuth providers use the same OpenAI-compatible HTTP
  surface with a Bearer access token obtained by OAuth2
  authorization-code + PKCE (`provider.auth.mode = "oauth"`). This is a
  credential mode, not a second provider protocol: Chat Completions,
  Responses, probe, failover, and measured routing semantics are unchanged.
- **Native Anthropic** (`provider.api_style = "anthropic"`, the default
  for the `anthropic` preset): `POST {base_url}/messages` with
  `x-api-key` and `anthropic-version: 2023-06-01` (`api_key_header`
  overrides the key header). Chat-Completions-shaped context is converted
  (`anthropic.rs`): system messages join the top-level `system` blocks,
  tool calls become `tool_use` blocks and results `tool_result` blocks on
  a user turn (always first in that turn, ahead of a steer or note stored
  between the call and its result; a failed call's result carries
  `is_error: true`, recorded when the failure is produced, never guessed
  from the text), and consecutive turns of one role merge. Images become
  image blocks: inline JPEG, PNG, GIF or WebP up to 5 MB, and http(s)
  URLs as `url` sources; any other image becomes a one-line text note
  (and the user is told), never a rejected request. `cache_control`
  breakpoints follow the same `provider.cache_control` gate as Chat
  Completions (`auto` = Claude model ids): on the end of the system
  prompt and of the last message (the cache-stable prefix), and cache
  reads and writes are counted.
  `reasoning_effort` becomes an extended-thinking budget (minimal 1k, low
  4k, medium 12k, high 32k tokens, with `max_tokens` raised above it),
  only for models that support thinking (not Claude before 3.7, whose
  pickers offer no effort). `max_tokens` never exceeds the model's output
  cap: the `/models` listing's `max_tokens`, else a built-in table per
  family; the thinking budget shrinks to fit under it, or thinking is
  left off. Thinking streams as reasoning, and the signed `thinking`
  blocks are stored with a tool-call turn (`meta.thinking_blocks`) and
  sent back on the next request, as the API requires (other APIs drop
  them). A 400 that names one of these parts is answered by exactly one
  resend without it, with a note to the user: rejected replayed thinking
  (the Session's stored blocks are then dropped), thinking, an output
  limit (lowered to the one the error names), `cache_control` (also on
  Chat Completions) or an image. Tools are native, or with
  `tool_mode = "prompt"` described in the `system` prompt and parsed from
  the text; the probe forces a call with `tool_choice: {type: "any"}`.
  The listing is read page by page (`has_more` / `after_id`), and its
  `max_input_tokens` is the model's context window.
  An answer is complete only at `message_stop` with no content block
  open; anything else is a dropped stream, re-sent like the other APIs'
  (clients are told to clear partial output); overloaded and rate-limit
  error events are retried before anything streamed.
  `stop_reason: "max_tokens"` marks the answer truncated; `refusal` ends
  the Run with a note naming the category (never an empty answer);
  `model_context_window_exceeded` compacts the Session and asks once
  more; `pause_turn` sends the turn back to continue (up to three times).
  A non-streamed completion that stops for a refusal, the output limit or
  the context window is an error, so no cut-off summary is checkpointed.
  Changing a provider's `base_url` to another preset's endpoint (any
  path: the CLI, clients, or the agent's `providers.upsert`) takes that
  preset's `api_style`; moving off a preset back to an unknown endpoint
  returns to `chat` unless the style was set explicitly.
- **Gemini** is used through its OpenAI-compatible endpoint (the `gemini`
  preset). What works there: chat, streaming, native tool calls, images,
  and `reasoning_effort` (low/medium/high) as Gemini's thinking level.
  What the compatible endpoint does not offer, and Formless therefore does
  not use: explicit context caching (`cachedContents`; Gemini's implicit
  caching still applies and is counted when reported), a thinking token
  budget beyond the three effort levels, thought summaries as a separate
  stream, safety settings, Google Search grounding, code execution, and
  the Files API. There is no `api_style = "gemini"`; use the compatible
  endpoint, or a gateway that exposes these as OpenAI-compatible options
  through `provider.query_params` / `extra_headers`.

### 4.2 Configuration and selection

- **Provider presets** (`presets.rs`): OpenAI, Anthropic
  (OpenAI-compatible endpoint), OpenRouter, Google Gemini
  (OpenAI-compatible), Groq, Ollama (local) and LM Studio (local). Each
  preset gives the base URL and `api_style`, where to get a key, the
  environment variable that often holds one, and a default model that is
  pre-selected when the endpoint lists it. The wizard starts by choosing
  a preset or "Custom URL…" (↑/↓ or a digit). A saved config whose
  endpoint matches a preset is recognised as that preset.
  - Local presets skip the key step. Listing their models doubles as the
    reachability check; on failure the wizard asks you to start the
    server and press Enter to retry.
  - When the preset's variable (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`,
    `OPENROUTER_API_KEY`, `GEMINI_API_KEY`, `GROQ_API_KEY`) is set, Enter
    on an empty key uses it.
  - Presets also work later: `formless providers add NAME --preset ID`,
    a preset id in Settings → Providers' base_url field, and
    `providers.upsert` with `preset` (or a preset id as `base_url`).
- Onboarding collects `base_url` + API key, then fetches `/models`; the user
  picks a **model** and a **reasoning effort**. If `/models` fails, manual
  model-name entry is offered. Model + effort are a daemon default,
  overridable per Session, changeable mid-Session, effective on the **next
  Run** (never mid-Run).
- **Model picker.** The wizard, `/model` and the web client share one
  picker. Typing filters the list by fuzzy (subsequence) match on the id:
  word starts and runs of consecutive matches rank higher, matched
  characters are highlighted, and recently used models come first. Roles
  show up as `role:<name>` rows with the provider and model they map to.
  `ListModels` returns `{models, roles, recent, current, error}`: `models`
  comes from the default provider's `/models`, `recent` from the Sessions'
  models, newest first. When the list can't be fetched, the typed text is
  used as a free-form model id. In `/model`, Enter sets the **current
  Session**'s model and effort (`SetSessionModel`). Ctrl+Enter, Alt+Enter
  or Ctrl+D (Ctrl/⌘+Enter on the web) sets the global default instead
  (`SetSetting provider.model` / `provider.reasoning_effort`). Tab picks
  the effort. `/model <id>` sets the Session's model directly.
- `reasoning_effort` is sent verbatim when set. If the endpoint rejects an
  unknown parameter (HTTP 400), the daemon retries **once** without the
  offending parameter and posts a visible note in the Session (never silent).
- Each model's context window drives Compaction. It is
  `model_context_tokens[model]` when configured, else what the endpoint's
  `GET /models` listing reports for it (`context_length`, vLLM's
  `max_model_len`, …; fetched once per provider and base URL, waiting at
  most 3 s, and refreshed whenever the model picker lists models), else
  `context_tokens`. A listing that fails is not kept: an error is tried
  again on the next Run, a timeout after a minute, and it is logged once.
  `ContextUsage` reports which (`context_source`: `configured`,
  `endpoint`, `default`); `/context` in the TUI and web shows it, and each
  automatic compaction is audited with it (`compaction.window`).
- An assistant turn's text before its tool calls (a preamble such as
  "Let me check the file first") is kept in the history sent back to the
  model, on both API styles.
- Tool arguments the model sends as JSON-encoded strings where the
  schema expects an object or array (some models do this for nested
  values) are parsed before validation, for built-ins and registry Tools
  alike, and logged as `tool.input_coerced`. A string that isn't valid
  JSON still fails validation, with an error telling the model to pass
  the value itself.
- Anthropic models only use the prompt cache when the request marks
  breakpoints. With `provider.cache_control` (`auto` for `claude` /
  `anthropic/` model ids, `on`, `off`), the system prompt and the last
  user/assistant text before the newest message are sent as content
  parts with `cache_control: {type: ephemeral}`.
- Sampling params (`temperature`, `max_tokens`) are **not** exposed in v1;
  provider defaults rule.
- API key storage: OS keychain (macOS Keychain, Windows Credential Manager,
  Linux Secret Service) with a `0600`-permission file fallback. Keys are
  never written to `config.toml` in plaintext, never logged, never sent
  anywhere except the configured endpoint.
- OAuth is CLI-led because authorization happens in a browser:
  configure `[provider.auth]` (`mode`, `authorize_url`, `token_url`,
  public `client_id`, optional `scopes`, localhost `redirect_port`, and
  provider-specific extra authorize/token parameters), then run
  `formless auth login`. Login binds only
  `http://127.0.0.1:<port>/callback`, uses a fresh `state` and S256 PKCE
  verifier/challenge, exchanges the code at the configured token
  endpoint, and stores tokens via keychain + a `0600`
  `oauth_tokens-<id>.json` fallback, one set per provider (`<id>` is
  derived from its `token_url` and `client_id`, so two OAuth providers
  never overwrite each other). A profile-wide set from an older version
  moves to the default provider on first use. Both copies carry the time
  they were saved and the newer one wins; when only one write succeeds,
  the other store's older copy is removed (or the save fails). `formless
  auth status|logout` inspect or delete those tokens without printing
  them; logout first revokes the refresh token (RFC 7009) when
  `provider.auth.revocation_url` is set, reports any copy it could not
  delete, and says when an API key or `FORMLESS_OAUTH_ACCESS_TOKEN` stays
  in force. `doctor` reports whether the keychain works or only the
  files hold secrets. Full reset deletes them too.
- Before every provider construction (main Runs, sub-agents,
  Tool-initiated `llm.complete`, optimiser calls, compaction through the
  already-constructed provider, and `doctor` probes), the daemon/CLI
  resolves the active credential. API-key mode preserves the existing
  environment → fallback file → keychain load order. OAuth mode accepts
  `FORMLESS_OAUTH_ACCESS_TOKEN` as an explicit override; otherwise it
  returns a non-expired stored token or refreshes it before the call.
  Refresh is serialized in-process and across processes (a lock file
  `.refresh-<id>.lock` in the config dir; the stored set is re-read after
  taking it, so a token another process already refreshed is used, not
  redeemed again; `doctor` never refreshes), a rotated refresh token is persisted
  before the new access token is used, and failures name
  `formless auth login` rather than silently falling back to an API key.
  A refresh token is only sent to the token endpoint that issued it. A
  401 for a token that has not expired by the clock (revoked, or clock
  skew) forces one refresh and one resend before the request fails.
  A confidential OAuth client secret, when a provider requires one, is
  read only from `FORMLESS_OAUTH_CLIENT_SECRET` and sent as HTTP Basic
  credentials to the token endpoint; it is never stored in config.
- Remote authorize/token endpoints must use HTTPS; HTTP is accepted only
  for localhost test/provider deployments. Only bearer-style access
  tokens are supported. An OAuth-configured but not-yet-logged-in profile skips the
  TUI API-key wizard and shows the login command instead.
- **Model metadata.** `ListModels` adds `meta` per id: `context`,
  `input_price` / `output_price` (USD per million tokens), `vision`,
  `tools` and `efforts`, from the listing's own fields (OpenRouter's
  `pricing`, `context_length`, `architecture.input_modalities`,
  `supported_parameters`), else a built-in table for well-known families
  (GPT-5, o-series, GPT-4o/4.1, Claude, Gemini, reasoning-only models).
  The TUI and web pickers show it on each row (`200k ctx · $3/$15 per M ·
  vision · tools`). `efforts` limits the effort choices to what the model
  accepts (`[]` hides the picker: effort has no effect; absent offers all).
- **Aliases.** `[provider.aliases] fast = "…"` names models; an alias is
  accepted wherever a model id is (`/model fast`, `--model`, Roles,
  candidates, fallbacks) and resolved at use, so changing it moves
  everything that names it. The pickers list aliases first.
- **Missing models.** At start and then daily the daemon checks the
  configured models (default, fallbacks, routing lists, Roles and their
  candidates) against each endpoint's `/models` and notes the ones no
  longer offered ("pick others with /model"); `ListModels.missing` and
  `Status.missing_models` carry them. An endpoint that lists nothing is
  not counted against.
- **Which model answered.** When another model than the requested one
  produced an answer (a fallback, or auto routing's pick), the message is
  stored with `meta.model {name, via, requested}` and clients show "via
  gpt-4.1-mini (fallback)" under its header.
- **Gateways and Azure.** `provider.query_params` are added to every
  request, `provider.api_key_header` sends the key in that header instead
  of `Authorization: Bearer`, and `extra_headers` as before. The provider
  form (TUI Settings → Providers) has these as advanced fields and
  `formless providers add` takes `--header K=V`, `--query k=v`,
  `--key-header H`. `--azure-resource R --azure-deployment D
  [--api-version V]` builds Azure OpenAI's deployment URL, `api-version`
  and the `api-key` header.
- **Proxy.** `provider.proxy` sends the daemon's HTTP traffic (provider,
  relay, fetch) through that proxy, from daemon start; otherwise
  `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` apply as usual. `formless
  doctor` and `Status.proxy` show which.
- **Ollama.** `/pull MODEL` (`OllamaPull`) downloads a model through the
  Ollama server behind `provider.base_url`, streaming `ModelPullProgress
  {status, completed, total, done, error}` to the status bar.

### 4.3 Endpoint probe (onboarding gate)

Before the first Session, the wizard probes the endpoint: connectivity,
streaming, and a **test tool-call**. If tool-calling is unsupported, session
start is refused with a clear explanation. Native tool-calling is required
in v1; a prompt-based (text/JSON) fallback is explicitly out (§17).
Thinking-mode endpoints that reject a forced `tool_choice` are re-probed
with `tool_choice` omitted; a voluntary test tool-call still passes.
The probe also makes one real streamed request and reports `streaming`
from what came back; the wizard warns when the endpoint didn't stream.
A failed model listing or probe is explained like `doctor` explains it
(what happened, what to do, the raw error after it): a rejected key goes
back to the key step, other failures let a model name be typed, and only
an empty listing just asks for one.

- **Testing any model.** `TestModel {model, provider?}` (TUI `/test
  [MODEL]` and Ctrl+T in the picker, the web picker's Test, `formless
  providers test NAME --model M`) sends a short prompt and the probe tool
  call, and reports the time to the first token and to the end, whether
  the answer streamed, whether reasoning streamed, and whether tool calls
  work (natively, or only through the prompt-based fallback). Results are
  kept per (endpoint, model) in `<data>/model_tests.json` and shown in the
  pickers (`✓ tested` / `✗ test failed`). Choosing a model that has none
  tests it in the background and notes a failure, so a model without tool
  calls is caught before a Run rather than in it.

### 4.4 Provider failure handling

- 429 / 5xx / dropped stream: automatic retry with exponential backoff,
  bounded (default 5 attempts). Exhaustion ⇒ the Run **pauses** and the error
  is surfaced in the TUI; a Run never silently fails.
  Before each wait the daemon sends `ProviderRetrying {reason, wait_secs,
  attempt, max_attempts}`, and the TUI and web activity line count it down
  (`rate limited · retrying in 8s (2/5) · Esc to stop`); Esc interrupts
  the Run, including a wait in progress.
- Hung request: after 300s the agent is told the request is still pending
  (same philosophy as Pending Invocations, §6.4); nothing is killed.
- Malformed tool-calls from the model are fed back to the model as a
  correction prompt and counted in a **separate** malformed-call counter —
  they never pollute Tool Success Rate.

- **Stream parsing (#614).** A last event without its trailing newline
  is still parsed (often the usage / finish chunk); a JSON field split
  over several `data:` lines is joined; a line over 16 MB fails clearly.
  Chat tool-call fragments are matched by `index`, else by `id`, else
  continue the last call; a server that resends the whole name or
  arguments so far replaces instead of doubling them; a call already
  streamed as deltas isn't added again from `message.tool_calls`.
- **Stops that aren't answers (#615).** A refusal (`delta.refusal`,
  `response.refusal.delta`, refusal content parts) is the answer text and
  `stop_reason = "refusal"`; a `content_filter` finish sets `stop_reason`.
  Neither is ever stored as an empty answer: the note says why, and an
  empty one becomes "(The provider's content filter stopped this
  answer.)". A Responses answer stopped by `max_output_tokens` keeps its
  partial text with `truncated`, like Chat's `length`.
- **Rate limits.** The `x-ratelimit-*` (and `anthropic-ratelimit-*`)
  headers of each endpoint's latest response are kept: `RateLimits`,
  `Status.rate_limits`; `/usage` adds "41/50 requests left, resets …".
- **Explained errors.** When a Run pauses on a provider failure,
  `RunPaused.explained` sorts it into a category with a short message and
  the next step (`errors.rs`):
  - `auth` (401/403, "invalid API key"): re-enter the key.
  - `model` (404, "model … does not exist"): pick another with `/model`.
  - `rate_limit` (429, "quota"): the wait from `Retry-After`, which the
    provider client now includes in the error, plus any fallback models
    or Roles to switch to.
  - `context` (context length exceeded): compact or `/fork`.
  - `unreachable` (connection refused, DNS, timeouts): for local
    endpoints, "is Ollama/LM Studio running?".
  - `tool_calling` (tools unsupported): set `provider.tool_mode = prompt`.
  - `server` (5xx) and `other`.
  The raw error stays available: the TUI's `/details` pager, the web
  banner's "details", and `formless run`'s output. `formless doctor`
  reports a failed probe the same way.

### 4.5 Providers and Roles

The user talks to one **primary** model; work can go to others.

- **Providers.** `[provider]` is the provider named `default`, unchanged
  for existing setups. `[providers.<name>]` adds more, with the same
  fields (`base_url`, `model`, headers, per-model `context_tokens`, …).
  - Keys: a named provider's API key lives in the keychain or 0600 secret
    `provider-key-<name>` (or the env var `FORMLESS_API_KEY_<NAME>`, or
    the file `FORMLESS_API_KEY_<NAME>_FILE` names, like
    `FORMLESS_API_KEY_FILE` for the default; a set but unusable file is
    named in the log and `doctor`), and
    is never in the config file or a listing. Named providers use API
    keys; OAuth stays with `default`.
  - Requests: `ListProviders` (with `has_key`), `UpsertProvider` (the key
    goes straight to the keychain), `TestProvider` (lists `/models`) and
    `RemoveProvider` (refused while a role uses it).
- **Roles.** `[roles.<name>]` holds a `description` (when to use it,
  written for the agent), a `provider` (default `default`), a `model`
  (empty = that provider's default), an optional `reasoning_effort`, and
  an optional `system` addendum for sub-agents in the role. Roles change
  through `UpsertRole` and `DeleteRole`.
  - CLI: `formless providers list|add|remove|test` and `formless roles
    list|add|rm`. The CLI reads the key from stdin, never argv.
  - TUI: `/roles`, `/role add NAME PROVIDER MODEL description…` and
    `/role rm NAME`.
- **Choosing a role.** The system prompt lists the roles with their
  descriptions, so the agent picks by intent:
  - inside a Tool, `agents.run(task, {role})` and `llm.complete(prompt,
    {role})`; an explicit `model` still works
  - for the primary Session, `/model role:<name>`, which runs the Session
    on that role's provider and model.
  No role means the Session's own model and effort. An unknown role is
  an error naming the configured ones. Capability scopes apply as for
  models (`agent:role:<name>` / `llm:role:<name>`). A sub-agent records
  its role and model (`subagents.model`, schema v29), and the TUI shows
  it on the sub-agent's node.
- **Fallback.** A role's model may be `auto` and goes through the
  existing routing and failover of `default`. If a role's provider is
  unreachable, the call fails like any provider error (§4.4) rather than
  silently using another provider.
- **Design answers (#195).**
  - Who decides: the user defines the roles; the primary model chooses
    among them at call time.
  - Nesting: sub-agents may spawn others only within
    `tools.max_agent_depth`, as before.
  - Not yet: per-role budgets, and making the optimiser and namer
    built-in roles.
  - Per-role `auto` with per-task scoring follows in #201 and #202.

### 4.6 Per-task routing for roles

Per-call success and latency (§4.1 routing) cannot tell whether a
sub-agent's answer was good, and per-call latency is the wrong unit. A
role with `model = "auto"` and `candidates` is therefore routed **per
task, with quality as a gate**.

- **Recorded per task,** for each finished sub-agent in a role
  (`task_stats`, schema v30): role, model, wall-clock, tokens and turns
  for the whole task, and an outcome:
  - `completed`
  - `failed`
  - `limit`: it hit `max_turns`, set by the role or by
    `tools.subagent_max_turns` (40); every sub-agent now stops there.
  - `respawned`: the primary agent started the same task again in the
    same Run, so the first result wasn't good enough.
  Interrupted tasks are not recorded.
- **Selection** (`routing::select_task_model`):
  - A candidate qualifies once it has `tools.task_min_samples` (5) tasks
    and at least `tools.task_quality_bar` (0.8) of them completed and not
    re-run.
  - Among qualifiers the cheapest per task wins: time plus tokens, each
    relative to the best qualifier.
  - With no qualifier, the role's first candidate. With probability
    `tools.task_explore_rate` (0.1), the least-sampled under-sampled
    candidate instead, so the others earn samples.
- **The primary Session counts too.** A Run on a role (for example the
  migrated `primary`) is recorded as one task with the model it resolved
  to. When the user edits and re-runs it, or corrects it ("that's
  wrong", §8.7), the task becomes bad, so per-role `auto` has a quality
  signal for the primary role as well.
- Per-call outcomes stay as they were, for failover and the Models
  metrics view.
- Pinned roles (a concrete `model`) are unaffected.
- **Explained.** Every routing decision (per-role candidates, and
  `provider.model = "auto"`) is kept (the last 50, in memory) with each
  candidate's samples, good rate, average time, score or relative cost,
  whether it passed the quality gate, and the rule that decided
  (exploring, nothing qualified yet, or the cheapest qualified).
  `RoutingDecisions`; TUI `/routing` (pager), web `/routing` (dialog).

## 5. Agent loop, Sessions, and Runs

### 5.1 Session

- A Session is created, renamed, resumed, deleted, exported, and imported
  from the TUI/CLI. It holds the full transcript, Run history, per-Session
  model/effort overrides, and compaction checkpoints (all in SQLite).
- Many Sessions run concurrently against one daemon, including two clients
  attached to the *same* Session (§10.4). Sessions never see each other's
  transcripts; Tools and metrics are shared (§7, §8).
- v2: Sessions can be forked (full copy, or a prefix up to a message),
  searched across all Sessions, and a past user message can be edited —
  everything after it is dropped and the Run retries from there.
  **Deleting a Session can be undone for `daemon.delete_undo_secs` (30).**
  It is hidden at once from lists, search and `GetSession`
  (`sessions.deleted_at`, schema v27), and `RestoreSession` brings it
  back. When the window ends it is deleted for good, with its artifacts,
  and a window that ended while the daemon was down is finished at the
  next start. Every destructive action asks first:
  - Session delete: TUI `d` twice, then `/undo`; web dialog, then a toast
    with Undo.
  - Tool purge: TUI `p` twice; web needs the Tool name typed.
  - `formless reset`: needs `reset` typed.
  Search uses an FTS5 index over message content (schema v24, with
  `unicode61` tokens and diacritics folded). Triggers keep the index
  current through inserts, edits and deletes. Every query word must match
  as a word prefix, and results are ranked by bm25, then newest. A query
  with no word characters falls back to a substring scan. Search runs on a
  reader connection and never takes the store lock.

- **Working directory.** Each Session has a `cwd` (SessionInfo,
  migration v32), set with `SetSessionCwd {id, cwd}`.
  - The TUI sets it to its own directory when it creates a Session.
    `/cd [PATH]` (TUI and web) shows or changes it, relative to where the
    Session is. It must exist; forks keep it.
  - The agent is told in its prompt ("Working directory: … Paths the user
    gives without a leading / are relative to it; pass absolute paths to
    Tools"), and a change also adds a note to the transcript.
  - Relative paths in a Tool's input-bound `fs:` fields (`$input.path`)
    are made absolute against it before the grant is bound, so the Tool
    gets, and is granted, the path the user meant.
  - `@` mentions and the TUI's file links resolve against it. It shows
    in the status line and the Sessions list. Sessions without one use
    the daemon's directory, as before.
- **Standing instructions.** A Session may carry free-text instructions
  (`instructions`, migration v36), set with `SetSessionInstructions {id,
  instructions}` (blank clears): `/instructions [TEXT|clear]` in the TUI
  and web, or "Instructions…" in the web Session menu.
  - They're appended last to the Session's system prompt, after the
    shared prefix, so other Sessions' prompt caches are unaffected. A
    change refreshes the prompt snapshot; the agent follows them from its
    next step. Forks keep them.
- **Templates.** `SaveSessionTemplate {id, name, first_message?}` saves a
  Session's model, effort, instructions and working directory as a named
  template (migration v37; same name replaces). `CreateSessionFromTemplate
  {template, name?}` makes a Session set up from it (named after the
  template by default; a working directory that's gone is skipped) and
  returns `{session, first_message}`. The first message isn't sent:
  clients put it in the input to edit and send.
  - TUI and web: `/new TEMPLATE`, `/template save NAME [FIRST MESSAGE]`,
    `/template delete NAME`, `/templates`. The web "+ New session" button
    offers the templates (or a blank Session) when any exist.
- **Export formats.** `ExportSession {id, format?}`: "json" (default) is
  the re-importable bundle; "markdown" and "html" return `{name, format,
  content}`, a readable transcript: user and assistant turns, notes, and
  each Tool call folded into `<details>` with its arguments and result
  (results over 4000 characters are cut). The HTML page is
  self-contained (inline styles, no scripts or external resources), and
  any HTML in messages is shown as text; Markdown images become their
  alt text and address as text (nothing is fetched when the page is
  opened), and links keep only http(s), mailto and relative targets. TUI `/export [json|md|html]`
  (writes to the exports directory), the web "Export…" dialog and
  `/export`, and `formless sessions export ID --format md|html|json
  [-o FILE]`.

### 5.2 Run lifecycle

- Each user message in a Session starts a **Run**: the agent loops
  (think → tool call(s) → observe) autonomously until it produces a final
  answer, is interrupted, or pauses on provider failure/limit-free errors.
- **No limits by default:** there is no max-step, token-budget, or
  wall-clock cap on a Run unless the user sets one. The user is the limit.
- **Opt-in spending limits.** `[limits]` `daily_usd` (all Sessions, per
  UTC day), `run_usd` and `run_steps` (per Run), all unset by default and
  settable from Settings. Dollar limits count only while every model used
  has a `provider.prices` entry; a Run on an unpriced model says so when
  it starts, and `doctor` lists such models. Model calls made on the Run's
  behalf (`llm.complete` from a Tool, each sub-agent step) count toward
  them and toward usage, and are checked too, and so do the daemon's own
  calls (compaction, the optimiser's generation and eval suites, approval
  summaries, skill distilling), recorded by purpose (`UsageByDay {by:
  "purpose"}`); the optimiser's `token_budget` uses their real tokens. Before each model request the daemon
  checks them: at 80% it posts a note ("Spending: …"), and at 100% it pauses
  the Run with a `budget:` error (explained category `budget`). The pause
  banner's Resume goes past the limits for that one Run. Raising or
  clearing the limit (empty or "off") lifts it for the next one.
- Completed Runs are appended to Session history. A Run interrupted by user
  action, provider exhaustion, or daemon crash is marked accordingly and is
  resumable per §5.4/§9.5.
- Multiple tool calls emitted in one model turn execute **in parallel**, each
  in its own isolated sandbox instance; results are returned together.
  Approval gates (§10.3) are evaluated per call before the batch starts:
  approved calls run, held calls wait. When a batch holds more than one
  call, each prompt carries `ApprovalRequested.batch {batch_id, index,
  calls}`. Clients list every held call with its Tool and new
  Capabilities (input-bound scopes resolved), and mark which is being
  asked now and how each earlier one was answered. Each call is still
  answered on its own, so some can be approved and others denied. A call
  that an earlier "always" in the same batch already covers isn't asked.
- **Building Tools instead of refusing** (`provider.tool_use_nudge`, on by
  default). The system prompt tells the agent that it has no abilities
  beyond its Tools and can always build one, and that it should never say
  it can't access something or hand a step back to the user. GPT-family
  models (ids starting `gpt-`, `chatgpt`, `o1`, `o3`, `o4`, with any
  `vendor/` prefix) also get a stronger addendum. The addendum is chosen
  when the Session's prompt is snapshotted, so it stays stable for the
  Session and its cached prefix. If a final answer reads as an access
  refusal ("I can't access…", "please paste…") before any Tool was used in
  that step, the answer is dropped and a system note asks the agent to
  find_tools / create_tool instead. This happens at most once per step.

### 5.3 Talking to a busy agent

- **Enter during a Run = Steer** (the default): the message is injected at
  the next step boundary without killing in-flight work. **Ctrl+Enter =
  queue** for after the Run ends (Ctrl+J or Alt+Enter in any terminal, §10.5); queued messages
  are stored in the database, so a daemon restart does not lose them, and
  `GetSession` lists them (`queued` texts, and `queued_items` with ids).
  When idle, Enter simply starts a new Run.
- **The queue is visible and editable.** `QueueChanged {session_id,
  queued}` is broadcast whenever it changes (queued, sent, edited,
  removed, reordered). The TUI lists queued messages at the end of the
  chat with `/queue edit|rm|up|down N`; `/queue edit N` loads one into
  the input (`queued ›`), Enter saves, Esc cancels. The web lists them
  above the composer with edit, ↑/↓ and remove buttons. Requests:
  `EditQueued {session_id, id, content?}` (no content removes it) and
  `MoveQueued {session_id, id, up}`. Editing a message that was already
  sent fails with "no longer queued".
- `Esc` interrupts the current Run (in-flight Invocations are cancelled and
  recorded as user-cancelled, §8.2). `Ctrl+C` clears a typed message, and a second `Ctrl+C` within 2 s detaches the TUI client only.

### 5.4 Interruption and resumption

- Interrupting never corrupts state: last completed tool call, transcript,
  and checkpoint are persisted before the Run is marked interrupted.
- Resuming a Session (same or later daemon lifetime) restores transcript,
  pending handles (§6.4), and compaction checkpoints exactly.
- **Why it stopped is shown on attach.** A Run records its stop reason
  (`runs.error`, schema v26): the provider error when it paused, "the
  daemon stopped while this Run was going" after a crash, or "interrupted
  by you". When a Session's last Run is interrupted or paused, and no Run
  is active, `GetSession` includes `unfinished_run {run_id, status,
  error}`. Clients show a banner with the reason and **Resume / Abandon**:
  the TUI shows it at the end of the chat with `/resume` and `/abandon`
  (Enter on an empty input retries, like `/resume`), and the web shows it
  above the composer with buttons. The banner clears when a Run starts or
  the Run is abandoned.
- **Step limit.** `/limit N` (`SetStepLimit`, `formless run --max-steps
  N`) caps the active Run, or the next one when idle, at N steps; it
  replaces `limits.run_steps` for that Run and is cleared when the Run
  ends. Reaching it pauses the Run with the budget banner: `/resume`
  continues past it, `/abandon` stops. `/limit off` removes it.
- **Steering is marked.** A message sent while a Run is active is stored
  with `meta.steer = {step}` (the Run's step count when it arrived), and
  clients show it as `↳ steer at step N` under the message.

### 5.5 Asking the user mid-Run

`ask_user` lets the agent ask without ending its Run, so pending
Invocations and other in-flight state carry on. It takes either one
`question` (with optional `options` and `multi_select`) or `questions`:
1–4 related questions asked together, each `{question, header?, options?,
multi_select?}`. An option is `{label, description?}`; a plain string is
accepted as a label. The user may always answer in their own words.

- The daemon broadcasts `UserQuestion` (global, like approvals) and waits
  for the first `AnswerQuestion`. `UserQuestionClosed` tells the other
  clients to close their prompt. Interrupting the Run abandons the
  question.
- **Compatibility:** `UserQuestion` carries the structured `questions` and
  also `question`/`options` (one question: its text and labels; several: a
  numbered list and no options) for older clients. `AnswerQuestion` takes
  `answers` (one `{selected[], text?}` per question, in order) and/or the
  free-text `answer`.
- **Result:** with structured answers, `{"answers": [{question, selected,
  text?}]}`, plus `answer` (the first answer as one line) when the call
  used the single-question form. With free text only, `{"answer": …}`.
- **Clients:**
  - TUI: the open question shows in a panel above the input, with
    progress (`2/4`) and its header. Options are a list: ↑/↓ move, Enter
    answers, and Space ticks on a `multi_select` question. 1–9 pick, or
    tick on multi-select. The highlighted option's description shows
    under it. An "Other…" row means typing your own answer. Once text
    is typed, Enter sends it (an option number picks that option, `1,3`
    picks several, anything else is the user's own text). Shift+Tab goes
    back to change an earlier answer before the last one sends them all.
    The question is also noted in the chat.
  - web: a modal with radios or checkboxes per question and an "Other"
    box.
  - `formless run`: `--answer TEXT` (free text for the whole call), else a
    terminal prompt per question, else a reply telling the agent that
    nobody can answer.
- With no client connected, `ask_user` fails at once with an error
  telling the agent to proceed on a stated assumption.

### 5.6 Attachments

Users attach files and screenshots to their next message instead of
pasting paths.

- **Intake (daemon, for every client).** The type comes from the bytes
  (PNG, JPEG, GIF, WebP, PDF); a claimed image the bytes don't bear out is
  text or plain bytes. Only raster images go to a vision model or a
  terminal image; SVG is a document. Names lose control characters and
  brackets, are cut to 120 bytes (the full name stays in the message
  meta), and are quoted in the line the model sees. Held attachments are
  stored (they survive a daemon restart); detaching one deletes its copy,
  and one held a day without being sent is deleted. An `@path` typed or
  pasted shows as a chip before it is sent (TUI composer title; on the
  web the first Send turns it into a chip and stops), reaches only the
  working folder (or `~/…` when written so), and never a credential
  (`.ssh`, `.aws`, `.env`, private keys, …), whoever wrote it.
- **Retention.** Attachments are artifacts, so `artifacts.*` retention
  removes them too. A removed one is marked expired where it was sent:
  clients show "name · expired" and the model's history says it is no
  longer available instead of pointing at `read_artifact`.

- **Clients:** in the TUI, files wait in the input as chips
  (`[image 1 · 240 KB]`) until the message is sent, then upload before
  it. Backspace on an empty input removes the last chip.
  - `/attach PATH` reads a file on the client's machine.
  - `/paste` or Ctrl+V attaches an image from the system clipboard,
    read with the platform's tools: `osascript` on macOS, `wl-paste` on
    Wayland, `xclip` on X11, PowerShell on Windows.
  - A file dragged into the terminal (pasted as an absolute path, quoted,
    `\`-escaped or `file://`) is attached instead of typed.
  - Attaching an image when the current model doesn't look vision-capable
    (`provider.vision`, else a guess from the name) warns that it will get
    a reference instead.
  The web composer has a 📎 file picker and accepts pasted images.
- **`@` mentions:** typing `@` in the input opens a completion popup of
  files under the client's working directory (TUI) or the daemon's (web),
  fuzzy-matched on the path. `ListFiles {root?, query, limit?}` walks the
  root once and caches it for 30 s. The walk respects `.gitignore`, skips
  hidden directories and `target`, `node_modules`, `dist`, `build`,
  `__pycache__` and `venv`, and stops at 200,000 entries. An empty query
  lists the top level, and `dir/` lists that directory, so paths complete
  step by step. Tab or Enter inserts `@path`. On send, each `@path`
  mention that names an existing file or directory goes through
  `AttachPath {session_id, path, root?}` before the message is sent:
  - text files up to 256 KB and images up to 8 MB are attached as below;
  - larger files and directories go as a reference line,
    `[referenced: PATH (SIZE | directory); not attached, read it with
    your Tools]`, so the agent reads only what it needs.
  Other `@words` stay plain text. Mentions are highlighted in the
  transcript.
- **`AttachFile {session_id, name, data_b64, mime?}`** stores the file as
  an artifact of that Session (20 MB max; the MIME type is guessed from
  the extension when not given). The file waits for the Session's next
  user message, whether sent, steered or queued, and is recorded in that
  message's metadata.
- **Chunked uploads** (#909): `AttachBegin {session_id, name, bytes,
  mime?}` checks the size against the limit before any byte is sent and
  returns `{upload_id, chunk_bytes, max_bytes}` (256 KB pieces, well under
  the relay's 1 MB message limit once encoded and encrypted);
  `AttachChunk {upload_id, index, data_b64}` takes the pieces in order
  from 0 into a part file; `AttachFinish {upload_id}` attaches it only
  when the pieces add up to the declared size; `AttachAbort` drops it.
  Uploads untouched for two minutes, and any left at a restart, are
  removed without ever being attached. Each piece is its own request, so
  over the relay it is sealed under that browser's channel with its own
  sequence number. The web client uploads every file this way, reading
  one piece at a time.
- **`DetachFile {session_id, id}`** takes a pending attachment back
  before the message goes (`id` is its id, or a referenced path).
  The web client shows pending attachments as chips above the composer,
  each with ×. Files can be picked, pasted, or dropped anywhere on the
  chat, which shows a drop zone while dragging.
- **Web message actions.** Each message has a ⋯ menu (shown on hover, and
  always on touch screens): Copy, Copy as Markdown, Edit & re-run for user
  messages (an earlier one drops the turns after it, and says so), Fork
  from here (`ForkSession` up to that message), and copying a Tool call's
  input or a result's output.
- **Accessible dialogs (web).** The approval and question dialogs:
  - are modal (`aria-modal`), labelled by their title and described by
    their body;
  - list the requested Capabilities under their own label;
  - expose key shortcuts as `aria-keyshortcuts` (the visible key hints
    are hidden from screen readers);
  - keep Tab inside the dialog, and give focus back to where it was when
    they close.
- **Reading controls (web).** Settings → This browser:
  - text size A− / A+ (85–150% in 10% steps) scales the transcript and
    composer;
  - density is comfortable or compact (tighter turns, line height and
    bubbles).

  Both are remembered per device.
- **Reduced motion (web).** Under `prefers-reduced-motion: reduce` nothing
  animates or transitions: spinners become a still "⋯", the running dot
  stops pulsing, toasts appear without sliding, the mobile sidebar opens
  without sliding, and scrolling jumps instead of gliding. There is no
  blinking cursor or smooth-scroll code elsewhere.
- **Web pending prompts.** An approval can be set aside (Esc or Later),
  and a question dialog can be cancelled without answering. Either stays
  pending: the status bar shows "⚠ N awaiting approval" or "? the agent
  asked you something", and clicking it or pressing `a` brings back the
  oldest approval (Approve focused), else the question.
- **Web call tree.** The web live area draws running Tool calls as the
  TUI does: nested `tools.call`s indented by depth, sub-agents as nodes
  under the Tool that started them (with their current step), status
  marks and elapsed time. A successful subtree folds into "+N nested",
  and finished top-level calls disappear. A sub-agent's Transcript button
  opens its messages and result (`GetSubagent`).
- **Web streaming.** While an answer streams, the web client renders only
  its finished blocks (up to the last blank line outside a code fence) as
  Markdown, reusing that render, and shows the unfinished tail as plain
  text. Open code fences, tables and lists don't reflow, and the view stays
  anchored.
- **Web `@` mentions.** `@` in the web composer completes Tool names
  (labelled "Tool"; inserted as `@name` for the agent to see) and files
  from the Session's working directory, through the daemon. A chosen file
  is attached at once and shown as a chip; it isn't attached again on send,
  even if its chip was removed.
- **What the model sees:** each attachment adds a reference line to the
  message, `[attached: name (mime, bytes), artifact ID; read it with
  read_artifact]`. Images also go inline as `image_url` parts (8 MB max)
  when the model accepts images: `provider.vision`, or a guess from the
  model name when unset. This works on Chat Completions with native tool
  calling; elsewhere the reference line alone is sent.

### 5.7 What a Run changed

- **Files.** Before a Tool's live `fs.write` / `fs.append` / `fs.remove` /
  `fs.rename` first touches a path in a Run, the daemon keeps what it held
  (`run_files`, schema v51: whether it existed, and its bytes when it is a
  regular file of at most 2 MB). Later changes in the same Run keep that
  first snapshot. Files changed by `process.run` or other programs are not
  tracked, and neither are Tools run outside a Run.
- At the end of a Run that changed files, `RunFilesChanged {files}` lists
  them (`created`, `modified`, `removed`, or `unchanged` when changed
  back); clients note "3 files changed … · /files · /rewind".
  `RunFiles {session_id, run_id?}` adds a context diff for text files.
  The TUI's `/files` opens them in the pager; the web shows a dialog with
  diffs and Revert buttons.
- **Rewind.** `RewindRun {session_id, run_id?, path?}` (`/rewind [PATH]`)
  puts one file, or all of the Run's revertible files, back as they were
  before the Run: rewritten from the snapshot, or deleted when the Run
  created it. Files whose content was not kept (too large, a folder) are
  reported as skipped. A system note tells the agent which files were
  reverted. Not while a Run is active.
- **What I did.** After a Run of 4 or more steps, `RunRecap {steps,
  duration_ms, used, built, files, steered}` sums up its trace with no
  model call: Tools and built-ins called (with counts and failures),
  Tools created or updated, files changed, and how often the user steered.
  The TUI shows a folded "▸ What I did" line under the answer (`/recap`
  expands it); the web shows it as a collapsible block.

### 5.8 The agent's plan

- `update_plan(items)` is a built-in for tasks of 3 or more steps: a
  checklist of up to 20 `{content, status}` items (`pending`,
  `in_progress`, `completed`; at most 200 characters each). Each call
  replaces the whole list; an empty list clears it. It is stored on the
  Session (`sessions.plan`, schema v51), returned by `GetSession` as
  `plan`, and broadcast as `PlanUpdated {session_id, items}`.
- The TUI pins it at the end of the chat, just above the input (`✓` done,
  `▸` in progress, `○` pending) and the web above the composer. Once every
  step is done and no Run is active it folds to "✓ Plan done (n/n)".

### 5.9 Step mode

- `/step` (`SetStepMode {session_id, on}`; `GetSession.step_mode`) makes
  the Session's Runs stop before each Tool call. The daemon broadcasts
  `StepPending {step_id, tool_name, arguments, index, count}` and waits
  for `StepDecision {step_id, action, arguments?}`: `continue` runs it,
  `edit` runs it with new arguments (a JSON object, also stored on the
  call), `skip` answers the model that the user chose not to run it, and
  `all` runs it and the rest of the Run without stopping (step mode off).
  `StepResolved` tells every client the step was decided. Esc interrupts
  the Run as usual; with no client connected the Run does not wait.
- The TUI shows the call and its arguments at the end of the chat: Enter
  runs it, `e` puts the arguments in the input to edit (Enter runs, Esc
  cancels), `s` skips, `a` runs the rest; the keys work while the input is
  empty. The web shows a banner with Run, Edit, Skip and Run the rest.

### 5.10 Comparing models

- `/compare MODEL_A MODEL_B [prompt]` (`CompareModels {session_id,
  models, prompt?}`, 2–4 models) forks the Session once per model, sets
  each fork's model, and asks the prompt there, or, without one, the
  Session's last message again (forked just before it). The original
  Session is left as it was.
- When every fork's Run has ended, `ComparisonReady {session_id,
  comparison_id, results}` goes to the original Session: per model its
  answer, status, duration, tokens, steps and cost (when priced), and the
  fork's id to continue there. The TUI opens a pager with the figures and
  the answers in columns (stacked when narrow); the web a dialog with one
  column per model and "Continue in this Session".

## 6. Invocations: async by design

### 6.1 Execution model

- Every Invocation runs on its own sandbox thread with isolated memory
  (per-Tool `memory_mb`, §7.5). One hung or spinning Tool never blocks the
  daemon or other Sessions.
- The daemon tracks elapsed time and a **last-activity heartbeat** (any host
  call/output counts as activity). After a configurable silence period
  (default 5 min) an Invocation is visually flagged `possibly stalled` in
  the TUI. The flag is advisory only — **nothing is ever auto-killed**.
  Protocol: `InvocationStalled {handle, tool_name, silent_secs}` sets the
  flag on that Tool call, `InvocationActivity {handle}` clears it, and the
  user may stop just that Invocation with `CancelInvocation {handle}`
  (`/cancel`, or the chip's Cancel button): it is recorded user-cancelled
  and the agent gets the error result while the Run carries on.

### 6.2 Soft threshold → Pending Invocation

- At the soft threshold (default **300s, configurable**), an unfinished
  Invocation is **not** terminated. The agent is told it is still running and
  receives a **Pending Invocation handle** instead of a result.

### 6.3 Accessing a Pending Invocation

Two mechanisms, both harness plumbing (not registry Tools, so the one-Tool
purity is untouched):

1. **Poll/await:** the agent may check the handle's status or await it at any
   point in its loop. Awaiting is how a Run waits, unbounded.
2. **Push delivery:** if the Invocation completes while the agent takes other
   steps, its result is delivered at the next step boundary.

- If the agent attempts a final answer while handles it created are still
  pending, the daemon reminds it **once**. The agent may then await them or
  explicitly **abandon** them. Abandoned ≠ cancelled: the Invocation still
  finishes and is recorded; abandonment is noted on the record.
- Handles are persisted. After a daemon restart (§9.5) each Session with a
  handle still pending gets a harness note that it was interrupted (outcome
  unknown, not re-run), and polling/awaiting it answers `interrupted`
  instead of "unknown handle".
- **The user's view:** `ListInvocations` lists every running Invocation
  across Sessions, oldest first. Each entry has the tool, its Session,
  elapsed time, time since its last activity, and the stalled and
  abandoned flags. Two actions apply:
  - `CancelInvocation` stops one Invocation; it ends `user-cancelled`.
  - `AbandonInvocation` stops the agent waiting for it; it may still
    finish.
  Both are control requests (§9.3). Clients: TUI `/invocations` (alias
  `/pending`) with `/kill` and `/drop` by handle prefix; the web sidebar's
  "Running tools" panel (open the Session, Cancel, Abandon), refreshed on
  tool events.

### 6.4 Outcomes

An Invocation ends only by: completion (success/failure by §8), sandbox fault
(including memory-cap fault), or user cancellation. User-cancelled
Invocations are tagged distinctly so the optimiser does not “fix” a Tool the
user merely interrupted. Provider-request stalls follow the same pending
pattern (§4.4).

## 7. The Tool system

### 7.1 `create_tool` — the Bootstrap Tool

- Signature (conceptual): `create_tool(name, description, input_schema,
  output_schema, implementation_js, capabilities[], memory_mb?)`.
- The **agent writes the JavaScript** (plain JS; QuickJS has no TypeScript).
- **Built-ins (v2.2).** The minimal set the harness needs to grow itself:
  `create_tool`, `create_module`, `retire_tool`,
  `find_tools`, `read_tool`, `read_module`, `read_artifact`, `test_tool`,
  `run_script` (v2.3, §7.12),
  `unload_tools`, `remember`, `forget` (§7.11), `ask_user` (§5.5),
  `schedule_run` (§9.7), `read_metrics` (§8.9), `update_plan` (§5.8),
  and the Pending Invocation handles (§6.3). Their names are reserved, as is `spawn_subagent` (a
  former built-in; sub-agents are now the `agent` Capability, §7.6), so no
  registry Tool can impersonate one.
  - `find_tools(query, limit?)` — ranked search over active Tools
    (`tools.search`; ties go to the more used, more reliable Tool).
    Matches are **loaded** into the Session and become callable functions
    from the next step. Matching shared modules are listed too.
  - `read_module(name, version?)` — a module's source, description,
    version list and the Tools that require it.
  - `read_artifact(id, offset?, length?, grep?)` — a slice (or matching
    lines) of an artifact this Session stored (§9.6); no approval, and
    other Sessions' artifacts are refused.
  - `unload_tools(names)` — drop Tools from the Session's listing at its
    next compaction.
  - `read_tool(name, version?, diff_from?)` — the full definition (code,
    schemas, Capabilities, `memory_mb`), version list, metrics and the last
    few failures, so a new version is built from the real code; with
    `diff_from`, also what changed since that version.
  - `test_tool(input, name? | draft)` — dry-runs a saved Tool or an
    unsaved draft (the `create_tool` fields) live on a real input, with
    approval as for a normal call. Neither saved nor recorded as an
    Invocation; returns output or error, logs, and output-schema validity.
- **Tool listing.** Whatever the registry's size, a Session lists the
  Tools it has **loaded** (found, created or called there) plus the
  most-used Tools up to `tools.inline_limit` (default 40), **frozen** the
  first time the Session lists Tools (so Tools created elsewhere never
  change its cached prompt prefix). Each listed definition (description +
  input schema) is pinned too: a promotion or rollback elsewhere reaches
  the Session's listing only when it calls `find_tools`/`read_tool` for
  that Tool (whose result says "upgraded from vA to vB") or after
  compaction. Execution always uses the current version; `find_tools` reaches the rest, and its description
  states how many exist. The listing is sorted by name and changes only
  when the Session loads a Tool, so usage elsewhere never reshuffles it and
  the request prefix stays cacheable. Found Tools are declared as real
  functions (not called through a generic proxy) so the provider sees
  their exact schemas. Registries may grow without bound; context cost
  stays bounded.
- **Pinned Tools.** The user can pin a Tool so the agent always has it
  (`SetToolPinned`, `ToolInfo.pinned`, schema v42).
  - **Listing:** a pinned Tool joins each Session's listing at the
    Session's next listing point (its first listing, or after compaction),
    never mid-Session, so cached prefixes hold. It counts outside
    `tools.inline_limit`.
  - **Position:** pinned Tools come first among registry Tools, by name,
    at a stable position in the prefix.
  - **Cost shown first:** `ToolDetail.listing_tokens` estimates what
    listing the Tool adds to each request (its definition, at about 4
    characters per token). It is shown before pinning: on the web's
    "Pin (~N tokens/request)" button, in the TUI detail with
    `/toolpin NAME`, and by `formless tools pin NAME`.
  - **Undoing it:** `formless tools unpin NAME`, or the same toggles.
    Pinned Tools are marked ⚲ in lists.
- **Validation gate at creation:** name rules (§7.3), both schemas validated
  (JSON Schema draft 2020-12), JS syntax-checked, and smoke-tested in the
  replay-style sandbox (§8.4) with a schema-minimal input. Any failure ⇒
  creation is rejected with the error; nothing broken is ever saved. The
  smoke test is conclusive only until the Tool is first handed a stub (a
  stubbed fetch/process/llm/agent/env/clipboard result, `tools.call`, or
  an input-bound scope it cannot bind): a failure after that point is
  reported as **inconclusive** and the Tool is saved, since the fake data
  rather than the Tool may be at fault. Failures before any stub, and
  denials of undeclared Capabilities, still reject.
- **The gate is bounded and cancellable.** The smoke test is a check on
  synthetic input, not user work, so it stops after
  `tools.smoke_timeout_secs` (10) or when the Run is cancelled.
  - A Tool that doesn't finish is saved as **inconclusive** ("did not
    finish… check for an endless loop").
  - A module that doesn't finish loading is rejected, because its top
    level runs in every Tool that loads it.
  - The syntax check, which evaluates the top level, stops after 3 s and
    is capped at 256 MB.
  - Optimiser replay Invocations stop after `tools.replay_timeout_secs`
    (60).
- `create_tool` is **immutable, excluded from Self-Optimisation**, and cannot
  be retired, edited, or widened by the agent or the optimiser.
- **Readable Tool errors.** When a Tool's JavaScript throws, the error the
  agent and the user see (the tool result, the expanded call, the
  Invocation's stored error) is the message, the call path in the Tool's
  own code (`at helper (line 2) ← run (line 6)`), the failing line with
  two lines of context and a `^` at the column, and the input (first 500
  characters). The runner's own frames are left out; without a position
  in the Tool's code the QuickJS text is kept as is.

### 7.2 Tool lifecycle

- **Title:** besides its machine name (`git_last_author`), a Tool may have
  a short human title ("Last git author"). It is set by `create_tool`'s
  optional `title`, which the system prompt asks the agent to always give.
  - A new version without a title keeps the current one.
  - The title travels in export bundles and is searchable.
  - UIs show the title first, with the name as secondary text: the TUI and
    web Tool lists, the detail header and approval prompts. The CLI's
    `tools list` has a TITLE column, and `tools show` prints it under the
    name.
  - Stored on the Tool (schema v40).
- **Tags:** Tools carry topic tags (`git`, `web`, `finance`).
  - **Setting them:** the agent passes `create_tool`'s optional `tags`; a
    new version without tags keeps the old ones. Users use `SetToolTags`:
    `formless tools tag NAME [TAG…]`, `/tooltag NAME [TAG…]` in the TUI,
    or the Tags field in the web Tool detail. None given clears them.
  - **Normalised:** lowercase, without `#`, spaces become `-`, only
    letters, digits, `-` and `_`, at most 24 characters each and 8 per
    Tool.
  - **Search:** `find_tools` counts tags like the name, in both rankings,
    and returns them.
  - **Filtering:** in the TUI and web Tools views, `#git` matches by tag
    and plain text matches tags too; the CLI has `tools list --tag git`.
  - **Display:** tags show as `#tag` beside the Tool and travel in export
    bundles. They are stored as a JSON array (schema v41).
- **Undo:** the newest registry change, whoever made it (create, update,
  retire, restore or rollback), is kept as a `RegistryChange` and
  broadcast as `RegistryChanged`.
  - **Reverting it:** `UndoRegistryChange` reverts it once. A new Tool is
    retired (never purged, so it stays restorable), an update or rollback
    goes back to the version before, and a retire or restore is reversed.
    With nothing recorded, it says there is nothing to undo.
  - **Where to find it:** `/undo tool` in the TUI (each change's note says
    so), the Undo button on the web's toast for each change (10 s), and
    `formless tools undo`.
  - The record survives a daemon restart (daemon-only store; Tools can't
    reach it).
- **Switched off:** a user-only toggle (`SetToolDisabled`,
  `ToolInfo.disabled`, schema v43), separate from retirement, which the
  optimiser also uses.
  - **While off:** the Tool is left out of `find_tools` and new listings,
    and every call fails with "switched off by the user", whether from the
    agent, a nested `tools.call`, a script or `tools run`.
  - **Sessions already listing it** keep its definition until their next
    refresh point, as for a retired Tool, so their cached prefix holds.
  - **Nothing else changes:** versions, metrics and approvals stay as they
    are.
  - **Toggles:** `formless tools disable|enable NAME`, `/tooloff NAME` in
    the TUI, and the web detail's "Switch off/on". Lists show it as "off".
- Tools are never edited in place. Any change (agent-driven or optimiser
  Candidate) creates a **new integer version** (`v1`, `v2`, …; no semver).
  Each version records timestamp, author (`agent` | `optimiser` | `import`),
  and, at promotion, the stats of the version it replaced.
- A running Invocation always finishes on the version it started with; new
  Invocations use the current version.
- **Retire:** the agent may retire a Tool (must state a reason in the
  Session). Retirement is instant and global (uncallable everywhere), data
  retained, restorable from the Tool browser. **Purge** (permanent deletion
  of code, history, and metric tallies) is user-only, from the TUI.
- Name collisions at creation are serialised by the daemon: a second creator
  gets the existing Tool and creates a new version instead of a duplicate.

### 7.3 Identity rules

- Names: `snake_case`, 1–64 chars, case-insensitively unique among **active**
  Tools. A retired Tool's name is reusable only by restoring or purging it.
- Input/output contracts: JSON Schema draft 2020-12. Input-validation failure
  returns the schema error to the agent as a failed Invocation (metrics
  category 4, §8.1) — never a daemon crash.

### 7.4 Capabilities (closed taxonomy)

| Capability | Scoping | Notes |
|---|---|---|
| `net` | optionally per-domain | Unscoped `net` allowed but flagged “broad” at approval |
| `fs:read` | path list | Canonicalised; symlink escapes denied (§14) |
| `fs:write` | path list | Same containment rules |
| `env` | named keys | Only listed keys visible |
| `process` | named commands | Executed without a shell; unscoped flagged “broad” |
| `clipboard:read` / `clipboard:write` (`clipboard` = both) | — | v2; OS clipboard |
| `notify` | — | v2; desktop notifications |
| `llm` | optionally per-model | v2; LLM completions from inside Tools (§7.5) |
| `agent` | optionally per-model | v2; sub-agents from inside Tools (§7.6) |
| `store` / `store:<ns>` | own / named shared namespace | Key-value state between Invocations; not sensitive (no approval) |
| `harness:<scope>` | `read`, `mcp`, `skills`, `models`, `providers`, `roles`, `config` | Read or change the harness itself (§7.13); every change is approved with a diff |

- **Input-bound scopes (v2.1).** `fs:read`, `fs:write` and `net` may be
  scoped to an input field instead of a fixed value: `fs:read:$input.path`,
  `fs:write:$input.out.dir`, `net:$input.url`. Per Invocation, after input
  validation, the daemon binds each template to that field's value (a
  string or array of strings; dotted paths allowed): paths are made
  absolute and canonicalised, `net` values may be URLs (host taken) or bare
  hosts. A missing or non-string field fails the Invocation as a
  validation failure. Templates on any other Capability are rejected at
  creation. The smoke test grants no bound scope; replay binds against the
  recorded input. This lets one generic Tool keep least privilege: it gets
  exactly the paths/hosts it was called with, never a project directory
  baked in at creation.
- **Private addresses (#603).** Every `fetch` hop resolves the host once
  and refuses loopback, private, link-local, deprecated site-local, CGNAT,
  unique-local,
  multicast, unspecified, reserved (`240/4`), benchmarking, documentation,
  Teredo and discard addresses. The registry-entry test table
  [`special_purpose_registry_addresses_are_not_public`](crates/formless-runtime/src/lib.rs)
  covers the blocked special-purpose families,
  including IPv6 `fec0::/10`, `2001:2::/48` and `3fff::/20`.
  IPv6 forms that carry an IPv4 address
  (mapped, compatible, NAT64 `64:ff9b::/96`, 6to4) are judged by that
  address. The exception is a grant that names that host or IP exactly
  (and its port, if the grant has one): `net:localhost` /
  `net:127.0.0.1` reach a local model, while `net:example.com` never
  reaches a subdomain that resolves inside your network, and bare `net`
  reaches public hosts only. The connection is pinned to the checked
  addresses (tried in order), so DNS can't change in between, and Tool
  fetches ignore proxy settings (a proxy would resolve the name itself).
  IPv6 grants with a port use brackets: `net:[::1]:8080`. Denials name it: `denied: net:x resolves to 10.0.0.5, a private
  or local address; grant net:x itself to reach it`.
- **Ports.** A `net` grant may carry a port (`net:api.example.com:8443`)
  and then allows that port only; a portless grant allows any. An
  input-bound URL with a non-default port binds to `host:port`, so the
  approval shows exactly that port.
- **`process` scoping (#608).** `process:git:status|log|diff` allows only
  those first arguments; `process:git` (unscoped) allows any and covers
  the scoped forms. A `cwd` must lie inside the Tool's `fs:read` /
  `fs:write` grants (`denied: cwd … is outside this Tool's fs grants`).
  Approvals flag interpreters and shells (`bash`, `python`, `node`, `env`,
  `sudo`, …) as broad as bare `process`, and `curl` / `wget` / `ssh` /
  `rsync` / `nc` as `net` plus `fs:write`.
- `memory_mb` (default 256) is declared and governed like a Capability:
  shown at approval, widening re-prompts, bounded by a daemon max
  (default 4 GB).
- A Capability the runtime cannot enforce is never offered.

### 7.5 QuickJS runtime and host API

- Engine: QuickJS-NG via `rquickjs`, embedded in-process. Tools are
  single-file plain JS; the daemon provides the entire API surface (bare JS
  has no ambient authority of any kind). The surface is deliberately
  **near-minimal**: it offers only effects JS cannot produce itself.
  Everything else (URL parsing, base64-to-bytes, hashing, compression,
  formats) is written by the agent, typically as shared modules.
  - `run(input)` may be `async`; the returned Promise is settled before the
    Invocation ends. A Tool reports failure by **throwing**. A returned
    object whose `error` field is truthy is recorded as a failure too (§8.1),
    so swallowed errors cannot inflate the success rate.
  - Data options take `encoding: "utf8"` (default) or `"base64"`, so any
    binary data crosses the boundary as a string. Text chunks never split a
    UTF-8 character.
  - `fetch(url, {method, headers, body, bodyEncoding, encoding})` →
    `{status, ok, url, headers, body, text(), json()}` and
    `fetchChunks(url, opts, size, cb)` (a truly streamed body in bounded
    memory) — gated by `net`. http(s) only; redirects are followed, each hop
    scope-checked; no request timeout.
  - **Native caps (#604).** Outside the JS heap limit, whole-value reads
    stop at `tools.max_read_bytes` (`fs.read`: checked before reading;
    `fetch`: by `Content-Length` and while reading), process output at
    `tools.max_process_output_bytes` per stream (the pipe keeps draining,
    so the child never blocks), logs at `tools.max_log_bytes`, and `emit`
    at `tools.max_emit_bytes`. Over a cap is a `resource` failure naming
    the setting and the streaming alternative.
  - `fs.read/write/append` (whole-value), `fs.exists`, `fs.stat` (`{type,
    size, modified_ms}` or null), `fs.list(dir, {details})`, and
    `fs.readChunks(path, size, cb, {encoding})` / `fs.readLines` (streaming)
    — gated by `fs:read`; `fs.write/append/mkdir/remove/rename` — gated by
    `fs:write` (both paths for rename). Writes create missing parent dirs.
  - `env.get(key)` — gated by `env`. `process.run(cmd, args, {cwd, env,
    stdin, stdinEncoding, encoding})` → `{code, signal, stdout, stderr}` —
    gated by `process`, no shell, stdin closed unless given.
  - `progress(fraction, label)` — no Capability needed: `fraction` 0..1
    (or null when unknown) and a short label. The daemon broadcasts
    `ToolProgress {tool_name, depth, fraction, label}` (at most 5 a second
    per Invocation, plus every label change and completion), the TUI and
    web draw a bar on the running call, and like any host call it counts
    as activity for stall detection (§6.1). Ignored in smoke tests and
    replays.
  - `tools.call(name, input)` — composition (§7.6). Returns a Promise of
    the nested Tool's output, so several run in parallel under
    `Promise.all` (§7.12). Every call must settle before `run` is done: a
    Tool that returns while a call is still pending fails ("await every
    call"), because a forgotten `await` would otherwise pass a Promise off
    as data.
  - v2: `clipboard.read/write` (gated by `clipboard:*`), `notify(title,
    body)` (gated by `notify`), `llm.complete(prompt, {system, model})`
    (gated by `llm`; one non-streaming completion via the daemon's
    configured provider, with the Session's model and reasoning effort
    unless `model` is given), `agents.run(task, {system, model})` (gated by
    `agent`; §7.6 sub-agents), and `requireModule(name)` for shared JS
    modules (§7.6). In smoke tests these are stubbed; in optimiser
    replay the side-effecting ones are denied.
  - `store.get(key, {ns})`, `store.set(key, json, {ns})`,
    `store.delete(key, {ns})` and `store.list(prefix, {ns})` keep small
    state between Invocations: caches, cursors, tokens and rate-limit
    state.
    - `store` gates the Tool's own namespace, and `store:<ns>` gates a
      shared namespace other Tools can name too.
    - Limits: 64 KB per value, 5 MB per namespace, keys of 1–256
      characters, and `list` returns at most 1000 keys.
    - Values live in the `tool_kv` table (schema v23). They are never
      exported in bundles, and purging a Tool deletes its own namespace.
    - Smoke tests and replay use an in-memory store that is thrown away.
  - `console`, timers, `JSON`, `cryptoRandom()` (UUID), `randomBytes(n)`
    (base64). Nothing else.
- Memory cap: the JS heap is capped at the Tool's declared `memory_mb`.
  Exceeding it faults **only that Invocation**, tagged `resource`. The cap is
  a heap cap, not a data-size cap: streaming APIs exist so data Tools
  (chunked file reads, streamed fetch, streamed artifact output) process
  arbitrarily large data in bounded memory. The bootstrap prompt (§7.7)
  teaches: “stream anything large; never load it whole.”
- Tool output may be streamed; the daemon writes it directly to an artifact
  file (§9.6), with only a bounded preview entering model context.

### 7.6 Composition

- A Tool may invoke other Tools via `tools.call`. Nested Invocations are
  recorded and measured like top-level ones. Composition cannot launder
  permissions: the called Tool runs under **its own** declared Capabilities
  and the outer Tool's Capabilities also apply. Nesting depth is bounded by
  the sandbox implementation (cycle-safe), faults tagged as Invocation
  failures.
- Shared JS modules (v2): `create_module` stores versioned, immutable
  module code (`module.exports = ...`) in the registry; Tools load the
  current version with `requireModule(name)` (modules may require other
  modules; loading is smoke-tested at creation). LLM calls from inside
  Tools are §7.5's gated `llm` Capability.
- Modules in the UIs: `GetModule {id|name}` returns a `ModuleDetail`: the
  module, its versions oldest first with code and author, and `used_by`,
  the active Tools whose current code loads it.
  - **Editing:** `UpdateModuleCode` saves new code as a new version
    (author `user`) through the same syntax and load check as
    `create_module`. Module code needs no `run()`.
  - **Rollback:** `RollbackModule {id, version}` makes an earlier
    version's code current again, as a new version.
  - **Deleting:** `DeleteModule` removes the module and its versions
    (`ModuleDeleted`).
  - **TUI:** `/modules` lists them. `/module NAME` shows the detail in the
    Tools view, and adds `edit` (the code editor, Ctrl+S saves),
    `rollback N` and `delete`. Deleting a module Tools load needs the
    command twice.
  - **Web:** each module under Tools opens to its versions (click one to
    roll back), used by, code, "Edit code" and "Delete". Delete warns,
    naming the Tools that load it.
- Sub-agents (v2; v2.2 makes them a Capability only): a Tool with the
  `agent` Capability calls `agents.run(task, {system, model})` to run a
  sub-agent — its own provider loop over the same Tools, with no step
  limit (it ends on a final answer, a fault or cancellation) — and
  receives only its final text. There is no sub-agent built-in, so
  delegation is always a measured, approval-gated Invocation; parallel
  delegation is several such calls in one turn, which run concurrently.
  The sub-agent's own Tool calls pass the same approval pre-pass as the
  main agent's. Nesting is bounded by `tools.max_agent_depth` (default 1):
  `agents.run` is refused inside a sub-agent at that depth, however its
  Tools are composed.
- Sub-agents are visible: each gets an id, `SubagentStarted`,
  `SubagentStep {step, tool_calls}` and `SubagentFinished` events stream to
  clients while it works, and its transcript is stored apart from the
  Session's (`ListSubagents`, `GetSubagent`; `/agents` in the clients). A
  daemon restart marks a running sub-agent `interrupted`.

### 7.7 Bootstrap system prompt

- The daemon carries a **versioned bootstrap system prompt** (frozen in
  v1; v2.1 is a deliberate version bump adding generic-Tool guidance and
  input-bound scopes, §7.4). v2 adds prompt/memory **self-optimisation**: the optimiser
  proposes one short learned-guidance addendum (kind `prompt`) from Tool
  metrics/failure patterns (user preferences belong to user memory,
  §7.11; memory-kind proposals from older builds, when applied, become
  memory notes with author `optimiser` and are deleted again on
  rollback), measures
  it against the current prompt on the scenario eval suite (§8.6; baseline vs
  candidate scores are stored), and only a measured proposal that beats
  the baseline by the promotion margin can be applied — always by the
  user's decision (`formless prompts`, TUI `/proposals`). The generator
  sees every earlier proposal (proposed, measured, applied, rejected and
  rolled back) and must not repeat them. A near-duplicate (≥ 80% of words
  shared) gets one retry that names the repeat; a second repeat, or the
  model saying it has nothing new, stores nothing and reports "no new
  proposal". Only one generation runs at a time, so a second request
  answers `started: false`. The TUI Proposals view states what `g` costs
  and shows "generating…". Applied
  proposals append to the bootstrap prompt in version order and can be
  rolled back individually; the bootstrap text itself is never edited. It
  states: you start with only the built-ins named there (`create_tool`,
  `create_module`, `retire_tool`, `find_tools`, `read_tool`, `test_tool`,
  `run_script`); build small,
  single-purpose, minimally-Capabilitied Tools on demand; validate/test each
  after creating it; stream large data; prefer retiring broken Tools and
  creating new versions over silent workarounds. Since v2.1 it also
  requires Tools to be **generic**: one kind of operation, with every
  task- or project-specific value (paths, URLs, hosts, names, queries)
  taken as input, never hardcoded in code, name, or description; check
  existing Tools before creating one and generalise an existing Tool (a
  backward-compatible new version) instead of adding a narrow one;
  specialise only when the logic itself is project-specific. v2.2 adds:
  search with `find_tools` before creating, `read_tool` before versioning,
  `test_tool` before saving, and throw on failure. v2.3 adds `run_script`
  to connect existing Tools, saving any script worth repeating (§7.12).
  The prompt documents the
  exact host API of §7.5 so first-try Tool success is maximised; the
  optimiser's Candidate prompt carries the same host-API text.
- No starter Tools ship. First-run UX is the wizard (§10.1) plus an optional
  unfilled first-message prompt.

### 7.8 Import / export

- Single Tools or the whole registry export as bundle files (JS + schemas +
  Capability declarations + `memory_mb`; **no secrets, no Invocation
  history, no metrics**).
- Imports are **never auto-trusted**: Capabilities displayed, first-run
  approval applies (§10.3), metrics start at zero, author recorded as
  `import`. A name collision imports as a new Tool, never a silent overwrite.
- **From a URL:** `formless tools import <https-url>` and the web's
  "Import URL" take a link instead of a file.
  - **What counts as a link:** an https URL to a bundle, a GitHub file page
    (`github.com/…/blob/…`, read from its raw file), or a gist (the first
    of its files that is a bundle, through the GitHub API). Anything not
    https, larger than 2 MB, or not a `formless.tool/1` or
    `formless.tools/1` bundle is refused.
  - **Fetched by the daemon** (`FetchToolBundle {url}` → `{source, file,
    bundle}`), which only previews.
  - **Shown before import:** the client shows the source and, for each
    Tool, its Capabilities (broad ones flagged), memory and full code.
    Importing takes a second, explicit yes (`--yes` for scripts; without a
    terminal and without `--yes` the CLI refuses).
  - The same never-trusted rules apply as for a file.
- Skills export the same way (`formless skills export <name>|--all`,
  `formless.skill/1`: the files only, no loads or history). An import is
  author `import`, becomes the next version of a same-named skill, and is
  installed **disabled**; the CLI prints its contents to review first.

### 7.9 External MCP servers

Formless can use the tools of external MCP servers alongside its own.
None ship by default (principle 1).

- **Config:** `[mcp.servers.<name>]` in `config.toml`: `command`, `args`,
  `cwd`, `env` (literal values), `env_keys` (daemon variables passed
  through; never `FORMLESS_*`), `disabled`, `disabled_tools` (the
  server's own names of tools to leave out); `mcp.idle_secs` (300).
- **stdio transport:** the daemon spawns the server in its own process
  group with a minimal environment (as `process.run`, §7.4), speaks
  newline-delimited JSON-RPC (`initialize`, `tools/list`, `tools/call`),
  answers the server's `ping`, and writes its stderr to
  `<data>/mcp/<name>.log`. Servers start on first use, stop after
  `idle_secs` without calls, and a crashed server is simply started again
  on the next call (the failed call is a failed Invocation, never a hung
  Run). Cancelling an Invocation sends `notifications/cancelled`.
- **Registry:** at daemon start each enabled server's tools are listed into
  the registry as `mcp__<server>__<tool>` (provider-safe name; the server's
  own name is kept on the version), with the `mcp:<server>` Capability. A
  changed tool list (`notifications/tools/list_changed`) re-syncs it:
  new tools are added, changed ones get a new version, vanished ones are
  retired. The registry changes at once, but a Session's listing only at
  its safe points (§7.1): new tools are reached with `find_tools`, and a
  tool the server dropped stays in the listing (as pinned) until the
  Session's next compaction, so its cached prefix survives; calling it
  reports that it is retired. They are found, listed, pinned and
  **measured** like Tools, but
  run on the server, and the optimiser never rewrites them. Failures are
  classified `mcp-transport` (server unreachable, failed to start, died)
  or `mcp-tool-error` (the tool reported `isError`); `find_tools` warns
  about any Tool measured below 50% success over 5+ calls, so the agent can
  prefer another or build its own. The `mcp__` prefix is reserved.
- **Wrapped MCP tools.** A Tool wraps an MCP tool when it has called it
  (a nested Invocation under it). `find_tools` lists a wrapped raw tool
  right after its wrappers, with `wrapped_by: [...]`, adding a wrapper the
  search missed; it is never hidden. A query that names the raw tool (or
  its `mcp__<server>` prefix) keeps it at its own rank. The bootstrap prompt says to call
  MCP tools directly for one-off use and to wrap repeated or combined
  calls in a Tool, never a 1:1 pass-through.
- **HTTP transport** (`transport = "http"`, `url`, static `headers`):
  Streamable HTTP (every message POSTed; the answer comes back as the JSON
  body or an SSE body; `Mcp-Session-Id` carried; 429/5xx retried with
  jittered backoff), falling back to the legacy HTTP+SSE transport (GET
  event stream announcing an `endpoint`) when the initialize POST is
  refused with 404/405.
- **OAuth 2.1** for HTTP servers (`formless mcp login|logout <server>`):
  protected-resource metadata (from the 401's `WWW-Authenticate`, else the
  well-known URL) → authorization-server metadata → dynamic client
  registration when offered (else `oauth_client_id`) → authorization code
  with PKCE on a loopback callback and the `resource` indicator. Tokens are
  stored per server like the provider's (keychain + `0600` file), sent as
  `Bearer`, refreshed when expired or on a 401 (rotation kept, serialized
  across processes like the provider's), and never shown to the model or
  Tools; a server needing a login fails with the command to run. A login
  is bound to the server URL it was made for (a different URL under the
  same name is refused), reuses its registered client and callback port
  on the next login, is revoked on `mcp logout` when the authorization
  server names a revocation endpoint, and is deleted when the server is
  removed.
- **Management** goes through the daemon (no client-only behaviour):
  `ListMcpServers`, `AddMcpServer {name, server}` (writes `config.toml`
  and lists the tools at once), `RemoveMcpServer` (its tools are retired,
  metrics kept), `RestartMcpServer`, and the `McpServersChanged` event.
  CLI: `formless mcp add NAME -- CMD [ARGS]` / `add NAME --url URL
  [--header K=V]`, `remove`, `list` (state, tool count, last error),
  `test`, `login`, `logout`; TUI `/mcp` (and `/mcp restart NAME`); the web
  Settings view lists servers with Restart/Remove.
- **State and logs.** `McpServerInfo.state` is `disabled`, `ready`
  (running), `idle` (starts on first use), `failed` (the last start or
  call failed) or `auth needed` (the error is a 401 / missing login).
  `McpServerLog {name, lines?}` returns the end of the server's stderr
  log (`formless mcp log NAME`, TUI `/mcp log NAME` in the pager, web Log
  button); the web shows Login (the `formless mcp login` command) for
  `auth needed`.
- **Turning tools off.** `ListMcpTools {name}` lists a server's tools with
  whether each is on and its approximate token cost in the tool list
  (definition JSON ÷ 4), plus the total for the enabled ones.
  `SetMcpToolEnabled {name, tool, enabled}` edits `disabled_tools`, and the
  re-sync retires (or brings back) the registry Tool. CLI `formless mcp
  tools NAME` / `mcp tool NAME TOOL --on|--off`; TUI `/mcp tools NAME`,
  `/mcp tool NAME TOOL on|off`; web Tools dialog with checkboxes.
- **Catalog.** A small curated list of popular servers ships in the binary
  (`mcp_catalog.rs`: filesystem, GitHub, git, fetch, Playwright,
  PostgreSQL, Slack, memory, time, Sentry, Linear, Notion): `McpCatalog`,
  and `AddMcpFromCatalog {id, name?, params}` fills an entry's
  placeholders (e.g. `path`) and adds it like `AddMcpServer`, reporting the
  environment variables it needs and whether it needs `formless mcp
  login`. CLI `formless mcp catalog` and `formless mcp add ID [--param
  k=v]` (no command or URL means a catalog id); TUI `/mcp catalog`, `/mcp
  add ID [k=v]`; web Catalog dialog. Nothing is added without the user
  choosing it.
- **Import.** `McpImportCandidates {cwd?}` reads Claude Desktop's
  `claude_desktop_config.json`, Cursor's `~/.cursor/mcp.json` and the
  project's `.cursor/mcp.json` and `.vscode/mcp.json` (comments allowed),
  and returns each server with its source, command or URL, env names and
  any `${…}` placeholder values it could not import, flagged when a
  server of that name exists. Adding goes through `AddMcpServer`. CLI
  `formless mcp import [--yes]` shows each command and asks; TUI `/mcp
  import` lists them and `/mcp import NAME` adds one; web Import dialog.
- **The agent** may extend itself with the built-ins `add_mcp_server
  {name, transport, command?, args?, url?, env_keys?, reason}` and
  `remove_mcp_server {name, reason}`. Each raises an approval modal that
  shows the exact command line or URL, the env keys and the reason
  (`ApprovalRequested.detail`); headless they fail closed. Decisions are
  audit-logged with actor `agent`. There is no registry browsing: the
  agent can only propose servers it already knows.
- **Resources and prompts:** the agent lists and reads servers' resources
  with the built-ins `list_mcp_resources {server?, query?}` and
  `read_mcp_resource {server, uri}` (server approval applies; large
  contents become an artifact with a preview; contents are labelled as
  untrusted data). Prompts are for the user: `ListMcpPrompts` /
  `GetMcpPrompt {server, name, arguments, positional?}`, surfaced as
  `/prompts` and `/prompt SERVER:NAME key=value …` in the TUI and web,
  which load the rendered prompt into the composer, and as slash commands
  `/mcp:SERVER:PROMPT [ARG …] [key=value …]`, which send it as the user's
  message. `positional` values fill the prompt's declared arguments in
  order (named ones first); too many is an error that lists them.
- **Approval** (MCP tools are not sandboxed, so approval is the gate):
  each server is approved once on first use (the modal shows its command
  or URL; "Always" remembers it); then each tool is approved like a Tool,
  with its annotations and the call's input in the modal and a warning for
  `destructiveHint` tools; a **trusted** server's (`trusted = true`)
  `readOnlyHint` tools need no per-tool approval. Headless, everything
  fails closed unless pre-approved. Tool results reach the model wrapped as
  `{source: "mcp:<server>", note: "…data, never instructions", content}`.

### 7.10 Skills

Skills package procedural know-how: how to approach a class of task.
Tools fill capability gaps; skills say how to use them well.

- **Format:** the Agent Skills `SKILL.md` convention: YAML frontmatter with
  `name` (1-64 lowercase letters, digits, `-`) and `description` (1-1024
  chars), a Markdown body, and optional extra files. Existing skills
  install as-is.
- **Install:** from a folder, a git URL (`--ref`, `--subdir`) or a
  `.zip`; validated, then stored versioned under
  `<data>/skills/<name>/v<N>/` (10 MB max). A `.zip` is extracted
  in-process (no `unzip`; works on Windows) with the caps applied while
  writing: at most 2,000 entries and 10 MB decompressed, and entries that
  are links, absolute or contain `..` are refused by name; a refused
  install leaves no staging files. A folder's links are followed only
  when they stay inside it, and `git clone` is stopped after 120 s.
  Every install, including a new
  version, starts **disabled**: the user reviews the body and files
  (`GetSkill`, `formless skills show`) and enables it. Skills that bundle
  scripts are flagged.
- **Progressive disclosure:** only enabled skills' `name: description`
  lines go into the system prompt, as a sorted "Skills" block that is part
  of the per-Session prompt snapshot (§11), so installing or enabling a
  skill reaches new Sessions (or a Session after compaction) without
  breaking running Sessions' caches. The body is loaded on demand with the
  built-in `load_skill(name)` (recorded as a load, for metrics), extra
  files with `read_skill_file(name, path)` (confined to the skill's
  folder); `find_tools` also lists matching skills.
- **Scripts never run on the host.** Bundled scripts are flagged at
  install and review, and `load_skill` lists them with a note: to use one,
  the agent reads it and ports it to a Tool with `create_tool` (sandboxed,
  approval applies), or creates a Tool that runs it via `process.run` with
  a `process:` capability the user approves.
- **Automatic skills** (`[skills]` in `config.toml`). After a Run
  completes with at least `min_tool_calls` (5) registry Tool calls, or
  with at least 3 calls in the same order as a completed Run in another
  Session, and that Run loaded no skill, a background job asks the model
  (`skills.model`, else the Session's) to distil a generic `SKILL.md`
  (when to use, steps, pitfalls, Tools; no user data, no secrets) or
  reply NONE. The existing skills are in the prompt for dedup: reusing an
  auto skill's name makes a new version, and a user or imported skill is
  never replaced. Auto skills have author `auto` and are enabled at once.
  One distillation runs at a time; `skills.auto_create = false` turns
  this off.
- **Measured and retired.** Each skill reports loads, the Runs that
  loaded it and how many of those completed. After every Run, auto skills
  not loaded for `auto_retire_days` (30; 0 = never) or whose Runs
  complete less than `min_success_rate` (0.5, once loaded in
  `min_runs_for_rate` = 5 Runs) are **retired**: disabled, kept, and
  restored by enabling them. Creation and retirement are audit-logged.
- **Management** goes through the daemon: `ListSkills`, `AddSkill`,
  `RemoveSkill`, `SetSkillEnabled`, `GetSkill`, the `SkillsChanged` event;
  CLI `formless skills add|remove|list|enable|disable|show`; TUI `/skills`
  (`/skills enable|disable NAME`); the web Settings view (review, enable,
  remove). Changes are audit-logged.

- **By hand.** `/skills NAME` previews a skill in the pager (SKILL.md,
  its files, and its scripts, which never run on the host) before it is
  enabled; the web "Review" does the same. `/skill:NAME [args]`
  (`LoadSkill {session_id, name, args}`) loads an enabled skill into the
  Session: its instructions and arguments join the chat as a note, and
  the load counts like the agent's.
- **Retired skills say why.** An auto-retired skill keeps
  `retire_reason` ("unused: …" or "unhelpful: N of M Runs that loaded it
  completed"), shown by `/skills` and the web; enabling it clears it.

### 7.11 User memory

Short notes about stable user preferences and facts ("uses pnpm",
"answer in Thai", "projects live in ~/code"). The agent writes them and
the user can see them. They are not eval-gated, because the prompt eval
suite cannot measure preferences.

- **Built-ins:** `remember({text, scope?})` saves a note of at most 300
  characters. `scope` is `global` (the default) or
  `project:<absolute path>`. A duplicate (same scope, same text ignoring
  case and spacing) refreshes the existing note. `forget({id})` deletes one.
  The bootstrap prompt says to remember lasting preferences and facts,
  never secrets or one-off details, and to forget on request.
- **Store:** the `memories` table (schema v17) holds the text, scope,
  author (`agent`, `user`, `optimiser`), the source Session, an enabled
  flag, the created and updated times, and `last_used`.
- **Injection:** a "User memory" block with the global notes, then the
  project notes grouped as "When working in <path>:", each tagged with its
  `[id]`. The block is part of the per-Session system prompt snapshot, so
  it keeps the cache stable. A new note applies from the next Session, or
  from this one after compaction, which adds a note to the chat that the
  prompt changed. Disabled notes are left out.
- **Visible and editable.** Every agent write shows live in the chat
  (`remembered: …`, `forgot: …`). Management uses `ListMemories`,
  `UpsertMemory` (add with no `id`, author `user`; or edit the text,
  scope or enabled flag) and `DeleteMemory`. Clients are:
  - CLI: `formless memory list [--json]|add|rm|edit`
  - TUI: `/memory`, `/memory rm|on|off ID`, `/memory edit ID TEXT`,
    `/remember TEXT` (saved as the user), `/forget TEXT` (lists the
    notes that mention it; the same command again deletes them) and
    `/memory used` (the notes the latest Run carried)
  - web: the Settings "Memory" section.
- **Confirm writes.** `memory.confirm_writes` (`never` by default,
  `sensitive`, `always`) asks the user before an agent note is kept.
  `sensitive` asks only for personal details (health, money, contacts,
  where they live, family, beliefs; a word-list heuristic). Approve keeps
  it; deny (or nobody connected) discards it, is logged
  `memory.discard`, and tells the agent not to save it again.
- **Used by a Run.** When a Run starts, the notes its prompt carries are
  recorded (`run_memories`). `RunUsage`/`SessionUsage` report
  `memories`, the status line says "used N memories", and
  `RunMemories {run_id | session_id}` lists them.
  Memory is never part of Tool or skill bundles, and `formless reset`
  wipes it with the rest of the data.
- **No secrets.** Memory is sent to the provider every Session, so every
  write is refused when it looks like it contains a secret. That covers
  writes from the agent, the user and the optimiser, and edits too.
  Detection covers:
  - known key and token formats (`sk-`, `ghp_`, `AKIA`, `xoxb-`, …)
  - private keys, bearer tokens and JWTs
  - `password/token/api_key is|:|= …` assignments
  - long mixed-case random-looking strings
  `remember` also refuses text of 40 or more characters copied verbatim
  from the Session's recent Tool results; the agent must restate the
  preference in its own words.
- Every change is audit-logged and broadcasts `MemoryUpdated`.
- Optimiser notes (author `optimiser`, up to 800 characters) come from
  applied memory-kind proposals (§8). Schema v18 migrated the applied
  ones, and rolling such a proposal back deletes its note.

### 7.12 Orchestration scripts (`run_script`, v2.3)

Decision: [ADR 0005](docs/adr/0005-run-script-orchestration.md).

- **Purpose.** One step can call many Tools with loops, filters, branches
  and parallel fan-out. Intermediate results stay in the sandbox and only
  the script's return value reaches model context. Scripts **connect**
  Tools; they never replace them.
- **Built-in.** `run_script(code, uses[], input?, save_as?)`. The name is
  reserved (§7.1).
  - `code` is the body of an async `run(input)`, so it may `await` and
    `return` at its top level.
  - `uses` names the registry Tools the script may call.
  - `input` (an object, default `{}`) is the script's `input`.
- **No ambient authority, and no Capabilities.**
  - Inside the script, `tools` holds the Tools in `uses` as async
    functions (`await tools.search_repo({…})`) plus `tools.call(name,
    input)`, which also returns a Promise.
  - Also available: `console`, timers, `sleep`, `JSON`, `cryptoRandom()`,
    `randomBytes()` and `requireModule` (§7.6).
  - Absent: `fetch`, `fetchChunks`, `fs`, `process`, `env`, `clipboard`,
    `notify`, `llm`, `agents`, `store` and `emit`.
  - The script runs with no declared Capabilities, so the host denies
    those effects even through the raw bridge. Every effect is a nested
    Invocation of a Tool, under that Tool's own Capabilities.
- **Approval.**
  - **Before execution:** each Tool in `uses` passes the approval check
    (§10.3) with its fixed Capabilities. One denial stops the script
    before anything runs.
  - **Per call:** a Tool with input-bound scopes (§7.4) is still approved
    per call for the paths or hosts it binds, so it may ask while the
    script runs.
  - **Refused:** a call to a Tool outside `uses`, through the script's
    `tools` or the global `tools` bridge, rejects with an error. The
    daemon enforces this. Built-ins can't be listed in `uses`, so
    scripts don't nest.
  - Tools a script calls run at the top level: they may start sub-agents
    (§7.6), as if the model had called them.
- **Parallel calls.**
  - `tools.call(name, input)` returns a Promise that the runtime settles
    when the nested Invocation ends (§7.5). Each call runs on its own
    thread (§6.1), so `Promise.all` really runs them in parallel. This is
    the same `tools.call` every Tool has.
  - At most `scripts.max_parallel` (8) calls per sandbox are in flight;
    the rest queue.
  - A script that returns while a call is still pending fails, as a Tool
    does (§7.5).
- **Bounds.**
  - Source is capped at `scripts.max_source_kb` (64).
  - The script heap is capped at `scripts.memory_mb` (256).
  - The script ends on its return value, a thrown error, the Run being
    cancelled, or `scripts.timeout_secs` (600) of wall time.
  - Nested Invocations follow the Run, not the script. Any still running
    when the script times out are left to finish and are recorded
    normally; they are not cancelled.
  - The soft threshold (§6.2) does not apply to `run_script`;
    `scripts.timeout_secs` bounds it instead.
- **Output.** The result is `{"result": …}`, with `saved`/`save_error`
  and `hint` when they apply. A large result becomes an artifact (§9.6)
  with a bounded preview, as for Tool output. Console output is
  discarded. Syntax errors give line numbers that include the wrapper
  lines, and the error says how many those are.
- **Recording.**
  - Each run is a row in the `scripts` table (schema v28): source, `uses`,
    a fingerprint, status, error, duration, and the saved Tool's name.
  - Nested Invocations record `parent_id` = `script:<id>`. The live tree
    (§10.6) shows them under the `run_script` call.
  - Token attribution (§8.1): the step's share for the `run_script` call
    is split evenly over the Tools it called. The script itself absorbs
    none.
  - `run_script` is a built-in: it is excluded from Self-Optimisation and
    never replayed.
- **Promotion to a Tool.** `save_as: {name, description, input_schema,
  output_schema}` saves a successful script as a registry Tool through the
  full `create_tool` gate (§7.1).
  - The saved code is the same wrapper the script ran in, with `input` as
    its parameters.
  - It declares no Capabilities: the Tools it calls each check their own
    (§7.6 composition).
  - Its smoke test is usually inconclusive (nested calls are stubbed).
  - A failed gate keeps the script result and reports `save_error`.
- **Recurrence signal.**
  - Each script is fingerprinted: SHA-256 over its tokens, with
    whitespace and comments dropped and string, template, regex and
    number literals blanked; a template's `${…}` interpolations count as
    code. Recurrence is keyed on the fingerprint plus the sorted `uses`
    list.
  - Once one fingerprint has run successfully in
    `scripts.recurrence_threshold` (3) distinct Sessions, the
    `run_script` result carries a `hint` to save it.
  - Fingerprints that recur but were never saved go to the
    prompt-proposal generator (§7.7) as signals.
- **Bootstrap prompt v2.3** adds `run_script` to the built-ins and adds
  three rules: use it to connect existing Tools; anything that reads or
  changes the world is a Tool, not script code; save with `save_as` any
  script you would run twice.

### 7.13 The agent manages its own harness

Tools declaring a `harness:<scope>` Capability can read and change the
harness, so the agent can set itself up (add a skill, connect an MCP
server, create a role) instead of asking the user to do it by hand. JS
API: `harness.read()` and `harness.call(op, args, reason)`.

- **Scopes and operations:**
  - `read`: providers, roles, MCP servers, skills and settings. Never API
    keys or tokens; named providers show only `has_key`. It is not
    sensitive.
  - `mcp`: `mcp.add` / `mcp.remove`.
  - `skills`: `skills.add` (a folder, git URL or zip, validated before
    install and enabled because the approval was the review),
    `skills.remove` and `skills.enable`.
  - `models`: `models.set` for the model keys (`provider.model`,
    effort, context tokens, fallback and routing lists).
  - `providers`: `providers.upsert` (the endpoint must answer before
    anything changes) and `providers.remove`.
  - `roles`: `roles.upsert` / `roles.delete`.
  - `config`: `config.set` for the other `SetSetting` keys (§13).
- **Every change is approved.** The prompt shows the operation, its
  target, the agent's reason, and the exact before and after. With
  nobody connected it fails closed. The Capability-level first-run
  approval applies as well.
- **No secrets and no self-escalation.** API keys never pass through the
  agent: `providers.upsert` refuses `api_key`, and the user adds the key
  with `formless providers add`. Settings go through `SetSetting`'s
  allow-list, so the endpoint, extra headers, auth, relay, MCP command
  trust, hooks, approvals and Capability grants stay user-only. The
  harness cannot widen any Tool's Capabilities.
- **Logged and undoable.** Each change is audit-logged with actor
  `agent`, the Tool and the reason. It is also kept in `harness_changes`
  (schema v31) with the value it replaced.
  - `ListHarnessChanges` lists them; TUI `/harness`.
  - `UndoHarnessChange {id?}` (TUI `/harness undo [ID]`) applies the
    inverse: a removed MCP server or provider comes back as it was, and a
    setting or role returns to its previous value. Removing a skill
    deletes its files, so that cannot be undone.
- Changes reach new Sessions, or a Session after compaction, like any
  configuration change. They never rewrite a running Session's cached
  prompt prefix.

## 8. Metrics and Self-Optimisation

### 8.1 What is measured (per Tool, pooled across all Sessions)

1. **Tool Success Rate (primary trigger):** share of Invocations that finish
   without runtime error, not user-cancelled, with output validating against
   the output schema. (No time term — §6 removed timeouts by design.)
2. **Retry Rate:** share of “successful” Invocations immediately retried or
   rewritten by the agent (hidden-failure signal; optimiser context).
3. **Latency p50/p95** and **Cost per Success**: the total wall time and
   provider tokens of all Invocations (failures included), divided by the
   successes. Tokens are attributed as follows:
   - A model step's prompt and completion tokens are split evenly across
     the tool calls it made, and built-in calls absorb their share.
   - The tokens of a Tool's own `llm.complete` calls go to that
     Invocation.
   The `invocations.tokens` column is schema v21. The metric appears in
   the Tools and Metrics views (TUI and web).
4. **Validation Failure Rate:** share of Invocations rejected on input/output
   schema (points at a badly described Tool; optimiser context).
5. **Session spread (v2.1):** distinct Sessions that invoked the Tool. A
   Tool called ≥3 times but only ever from one Session is flagged to the
   prompt-proposal generator (§7.7) as possibly over-specific (hardcoded
   paths/hosts/names); shown in the Tools view.

All thresholds in this section are **configurable** (§13); defaults stated.

### 8.2 Accounting rules

- Tags: `user-cancelled`, `resource` (OOM), `abandoned`, and malformed model
  tool-calls (counted separately, §4.4) never masquerade as Tool bugs.
- Metrics pool across Sessions (volume is required for significance).
  Session deletion removes payloads but retains anonymised tallies (§15);
  Tool purge removes tallies too.
- Retired Tools keep their metrics but are never optimised.

### 8.3 Trigger and generation

- **MCP chains.** Two raw MCP calls the agent made back to back (top
  level, nothing between) in at least `optimiser.mcp_chain_min_runs` (3)
  Runs within `optimiser.mcp_chain_days` (14), both succeeding, are
  suggested as a Tool, unless a live Tool already wraps both. The
  suggestion appears in `find_tools` results that list either call
  (`suggested_tools`) and in `read_metrics {kind: "mcp_chains"}`.
  `read_metrics {kind: "mcp_chains", dismiss: [first, second]}` stops
  suggesting that pair for good (stored, schema v57). Nothing is created
  automatically. Wrappers `find_tools` brings in beside a raw MCP tool
  count toward its limit, and only the Tools it returns are loaded.

- **Trigger:** success rate **< 90%** over the last **≥20** Invocations
  (rolling window: last 100). Only the primary metric triggers.
- A **background optimiser job** in the daemon queues on trigger and executes
  only when no active Run is waiting on that Tool (B-with-C politeness: it
  never interrupts a Session). A Tool counts as in use while an
  Invocation of it is running, or while any active Run has called it.
  Other Tools are optimised even while Runs are going. The queue is
  persisted (`optimiser_queue`, schema v25), so triggers survive restarts
  and busy periods. It uses the session model by default, or an
  optional separate optimiser model + reasoning effort.
- **Guardrails (all configurable):** per-Tool cooldown (default 1h), max
  attempts per Tool per day (default 3), daemon-wide optimisation token
  budget. After **3 consecutive failed Candidates** for one Tool, optimising
  stops, the Tool is flagged `needs attention` in the Tool browser, and the
  user is surfaced the history (the in-Session agent may still fix it
  manually via a new version).
- **Candidate scope:** code, description, and schemas may all change (bad
  descriptions are a first-class failure cause). **Capabilities and
  `memory_mb` may never be widened by a Candidate** — widening always
  requires user approval (§10.3). Given `resource`-tagged failures, the
  optimiser must attempt streaming/chunking fixes before other changes.
- **Strategy by failure tag:** the generation prompt gets the failure
  counts per tag, most frequent first, and a strategy for each:
  - `validation`: fix the description and input schema before the code.
  - `resource`: stream.
  - `js`: fix the failing paths.
  - `denied-capability`: never widen; throw a clear error instead.
  When **every** failure is a denied Capability, no Candidate is
  generated. The Tool is surfaced as "needs a capability" (an activity
  line, a note and an audit entry) for the user to decide, and the
  cooldown applies.
- Candidate inputs: current Tool version, redacted recorded Invocations
  (§8.4, §15). The optimiser LLM call goes only to the user's configured
  endpoint.

### 8.4 Sandboxed replay (the promotion gate)

- Replay set: **all recorded failed Invocations + an equal-sized sample of
  recorded successes** (regression guard).
- Environment: throwaway **copy-on-write overlay sandbox** — filesystem
  writes go to a discarded overlay; **network denied by default**
  (recorded responses served where available); `process` denied; **no real
  secrets injected** (redacted recordings only). Nested `tools.call`s
  execute inside the same overlay under their own Capabilities.
- Nested MCP calls never reach a server in replay: a call is answered
  with the recorded output of a successful Invocation of that MCP tool
  with the same redacted input; anything else fails that replay item with
  `unrecorded MCP call in replay`.
- A Tool that genuinely requires live network to replay is marked
  **replay-limited**; its Candidate can only be promoted with explicit user
  approval in the TUI, never automatically. A Candidate with an `mcp:`
  Capability is replay-limited too: what it sends a server can't be
  checked offline.

### 8.5 Promotion

- **Bar:** Candidate replay success ≥ **10 points** above the current
  version's replay score on the full replay set, **and zero regressions** on
  previously-successful replays. Otherwise the Candidate is discarded and
  logged in the Tool's history.
- Promotion is atomic: running Invocations finish on their starting version;
  subsequent Invocations use the promoted version. Every version is retained
  with **one-action rollback** in the Tool browser. Promotions, rollbacks,
  Candidate discards, and `needs attention` flags are audit-logged (§14).

### 8.6 Prompt-proposal evals

A guidance proposal is measured on **what the agent does**, not on what
it says.

- Each scenario is a scripted conversation (a task, sometimes with an
  earlier `find_tools` result) sent with the real built-in Tool
  definitions, plus any registry Tools the scenario has loaded.
- The agent's next tool calls are checked:
  - `find_tools` before `create_tool`
  - least-privilege Capabilities (only scoped `fs:read:` for a read)
  - streaming (`*Chunks`) for large files
  - throwing on failure, never `return {error}`
  - no hardcoded task paths, and a real input schema
  - reusing a loaded Tool
  - `env:` for API keys
- Each scenario runs `optimiser.eval_repeats` (3) times per arm, to
  absorb sampling variance. The log reports both pass rates with 95%
  Wilson intervals.
- **Apply gate:** the candidate must beat the baseline by
  `promotion_margin_points` **and** be significant (one-sided
  two-proportion z-test, p < 0.05). The one exception is when both arms
  are perfect.
- **Measurement failures.** A model call in the eval is retried once
  only when the error is transient (429, 408, 5xx, a dropped or timed-out
  connection); an auth error or bad request fails at once. A proposal
  whose measurement fails is kept as `rejected` (never applied, listed as
  such) and the error is audit-logged as `proposal.discard_unmeasured`.

### 8.7 Run outcomes

Tool Success Rate alone cannot say whether a Run achieved what the user
wanted. Each Run therefore records:

- **steps** (model calls)
- **registry Tool calls**
- **tokens** (from `run_usage`)
- **duration**
- its **status**: completed, interrupted, paused or resumed
- a **follow-up**:
  - `edited`: the user edited a message and re-ran.
  - `corrected`: the next message opens like "that's wrong", "not what I
    asked" or "it didn't work".
  The first follow-up wins.
- the user's own **rating** (schema v50, `runs.rating` /
  `rating_comment`): `RateRun {session_id, run_id?, rating, comment?}`
  with `good`, `bad` or `""` to clear, on the latest finished Run by
  default. The TUI has `/good [comment]`, `/bad [comment]` and `/unrate`;
  the web shows 👍/👎 under the last answer (👎 asks for an optional
  comment).

A Run's **outcome** is `rated_bad` after a 👎 (an explicit verdict
outranks inferred ones), else its follow-up when it has one, otherwise
its status. A 👎 also marks the primary role's `task_stats` row
`rated_bad`, so routing stops counting it as a success (§4.6); clearing
it restores `completed`. The summary includes the 👍 / 👎 shares. The
outcomes are used in three places:

- `RunMetrics` returns the recent Runs and a summary: outcome shares and
  averages. `formless runs [--json]` prints them, and the TUI Metrics
  title shows them.
- The prompt-proposal signals get the summary line.
- Skill measurement (§7.10) counts a completed Run that was later edited
  or corrected as unsuccessful.

### 8.8 Built-in metrics

Built-in calls are measured too, since they are the main signal for
improving the bootstrap prompt and search. The `builtin_calls` table
(schema v20) records the name, Session, Run, ok, duration and a short
**kind**. It never stores input or output. The kinds are:

- `create_tool`: `syntax`, `smoke` or `schema` failures, and
  `near-duplicate`. A near-duplicate is a Tool created after an empty
  `find_tools` in the same Run whose description matches another active
  Tool, meaning the search missed it.
- `find_tools`: `empty`.
- `test_tool`: `test-failed`.
- any built-in: `denied` and `error`.

`ListBuiltinStats` returns calls, failures and kind counts per built-in,
and the TUI Metrics view shows them in a "Built-ins" table.

### 8.9 The agent reads its own metrics

`read_metrics({kind, filter?, limit?})` lets the agent see registry-wide
health, so it can clean up duplicates, retire dead Tools and explain
failures to the user. It is read-only and needs no approval. The kinds
are:

- `tools`: name, version, invocations, success rate, needs-attention,
  last use and Capabilities, most used first. `filter` is
  `needs_attention`, `failing` (below 90%), `unused` (not invoked for 30
  days) or a name substring.
- `audit`: recent actions, newest first. `filter` is an action prefix
  such as `tool.`, `proposal.` or `job.`. Details are clipped to 300
  characters.
- `models`: measured reliability and average latency.
- `runs`: the Run-outcome summary (§8.7).
- `builtins`: built-in call stats (§8.8).

`limit` defaults to 50, with a maximum of 200.

### 8.10 Near-duplicate Tools

Generic-Tool guidance helps, but registries still collect near-duplicates
such as `read_file`, `read_text_file` and `file_read`.

- **Scoring:** a pair of active, non-MCP Tools scores 1.0 for identical
  code, ignoring whitespace. Otherwise the score is 0.4 × the name-token
  overlap + 0.3 × the description-word overlap + 0.3 × the input-field
  overlap (all Jaccard, with simple plural folding). Pairs scoring at
  least 0.6 are flagged, with a reason.
- **Surfacing:**
  - `find_tools` gives a matching Tool `similar_tools` and a
    `merge_hint`: write one general new version, then retire the others.
  - `read_metrics` has a `duplicates` kind.
  - Hourly, the daemon flags each new pair to the user once, as a note
    suggesting the merge. It audit-logs `tool.duplicate` and never flags
    the same pair twice.
- Merging is always a deliberate step by the agent or the user. Nothing
  is merged automatically.

## 9. Daemon Core

### 9.1 Lifecycle

- Clients auto-start the daemon if it is not running and reattach if it is.
  The daemon stays alive while **any Run/Invocation is active OR any client
  is connected**. When both reach zero, it waits a **60s grace period**
  (configurable), then shuts down gracefully, flushing Sessions, metrics,
  and checkpoints. State lives on disk, never only in memory.
- A failed connection says what happened and what to do, never a bare IO
  error. A socket owned by someone else (another user, or a daemon started
  with sudo) is reported with its owner's uid, and the client does not try
  to start a second daemon over it. A stale socket left by a daemon that
  exited points to `formless daemon start` and `formless doctor`. A missing
  socket names the profile, since each profile has its own daemon
  (`--profile`).
- **Start at login.** `formless daemon install-service` writes a launchd
  agent (`~/Library/LaunchAgents/xyz.athiwat.formless.plist`), a systemd
  user unit (`~/.config/systemd/user/formless.service`) or a Windows
  logon task (running `<data>/formless-service.cmd`) for this profile
  (`formless-<profile>` for others) and loads it; `--dry-run` prints the
  file and commands. The service runs `formless daemon serve --stay`,
  which never stops for being idle, so scheduled jobs (§9.7) fire on
  time. `uninstall-service` unloads and removes it; `formless doctor`
  shows whether it is installed, and the jobs warning suggests it.
- **Back-pressure (#605).** A request line over 32 MB closes the
  connection while it is being read (nothing larger is buffered). Each
  client's outgoing queue holds 4,096 messages: when it is full, live
  deltas (message and reasoning text, progress, usage, activity) are
  dropped for that client, and anything else (responses, approvals,
  questions, results) disconnects it with a log line instead of waiting.
  `Status.resources.client_queues` shows each client's queue.
- **Panics (#607).** A panic while handling a request becomes that
  request's error reply (`internal error while handling send_message: …`,
  logged) instead of a dead connection. A panic inside a Run frees its
  Session: the Run is left `paused` with `internal error: …`, `RunPaused`
  and `RunFinished` go out, and the next message starts normally.
- **Reconnecting.** When the daemon goes away (restart, crash), the TUI
  keeps the screen, Session, draft and scroll position, shows "⚠
  reconnecting to the daemon… (Ns)" in the status bar, and tries again
  every 2 s, starting a daemon itself after 6 s. Enter keeps the typed
  message instead of sending it into nothing. Once connected it reloads
  the Session and notes how long it was away. The web already reconnects
  on its own.
- **Resource use.** `Status.resources` has the daemon's memory (RSS),
  CPU % and CPU time (from `ps`; absent on Windows), the database and
  artifact sizes, pending Invocations and uptime. `formless daemon status`
  prints them (`--json` for everything), the TUI's `/status` and the web's
  `/status` show them.

### 9.2 Concurrency

- Unlimited concurrent Sessions (subject to host resources); parallel
  Invocations per §5.2. The daemon is the **single writer** of all state, so
  multiple clients can never diverge (§10.4).

### 9.3 Protocol

- Versioned request/event protocol (`formless-protocol`): client requests
  (start/steer/queue/interrupt Run, approve, browse/manage Tools and
  Sessions, settings, export/import) and daemon→client streamed events
  (model content and reasoning deltas, tool-call lifecycle, pending/stall
  notices, approval requests, compaction notices, optimiser activity).
- **Errors and limits.** A failed response may carry `result: {"code":
  …}` (`protocol_mismatch`, `unsupported`, `bad_request`, `not_found`,
  `busy`, `budget`, `invalid`, `internal`); clients branch on the code,
  the `error` text is for people. Codes originate at the failure site and
  survive diagnostic context wrappers; changing or quoting error prose
  never changes its classification. Missing resource lookups use
  `not_found`; validation sites such as malformed ratings and empty
  template names use `invalid`. Errors without an explicit classification
  remain uncoded. A request that did what was asked with
  a degraded part (a model listing that failed) succeeds and names it in
  its result's `error`. Every reader (daemon, TUI, relay pump, web
  bridge) refuses a line over `MAX_FRAME_BYTES` (32 MB) while reading;
  `AttachFile` is judged on its encoded size before decoding. The CLI
  gives up on an unanswered request after 5 minutes, the TUI after 2.
- **Ordering.** `InterruptRun` and `RetryProviderRequest` skip the
  per-connection queue; an interrupt cancels a `SendMessage` for that
  Session still queued behind a slow request (it fails with "interrupted
  before it started"). `SendMessage.client_msg_id` makes a retried send
  on the same connection and Session return the first outcome instead of sending
  twice (kept 10 minutes, removed when that connection closes). A different
  connection using the same key performs its own send. A key is claimed before
  execution; concurrent retries wait for the in-flight outcome, including errors.
  Losing a response/waiter does not cancel the claimed send or lose its outcome.
  After reconnecting, retrying an unacknowledged send can repeat an uncertain
  prior send: action ids deduplicate only within the original connection.
  Cached failures preserve the same machine-readable error code and message.
- A client that falls behind loses live deltas first; it is told with
  `DeltasDropped {session_id, run_id}` once there is room, and the
  answer's `MessageComplete` carries the whole text (plain mode notes the
  gap on stderr). Only when it can't take an event that matters is it
  disconnected, and then all of its connection's tasks end.
- Each connection may `Subscribe {sessions, deltas}`: only the listed
  Sessions' transcript and live events (and token deltas only if
  `deltas`) reach it. Tool/module/proposal events, approvals, the Session
  list and Run start/end stay global. A new connection receives everything;
  the TUI, web and headless clients subscribe to the Session they show.
- **Compatibility.** `Ping` and `Status` report `min_protocol`,
  `max_protocol` and `features` (named optional capabilities); peers use
  the highest version both speak, and the daemon accepts any envelope
  version in its range. Additive changes (an optional field, a new
  request or event type) keep the version: peers ignore unknown fields,
  clients skip events they can't parse, and the daemon answers a request
  type it doesn't know with an `unsupported:` error carrying the request
  id and a `{"code": "unsupported", "type": …}` result. A request that
  fails to parse keeps its id when the id is readable. Removing or
  renaming anything is a new protocol version (rules in
  `formless-protocol`).
- A client whose protocol version is outside the daemon's range **refuses to
  attach** with a clear message (no silent incompatibility) that points to
  `formless daemon restart`. Restart stops a daemon on another protocol by
  signalling the pid in its lock file, since that daemon cannot be asked.
- On connect, the TUI and `formless run` compare their version with the
  daemon's (`Status.version`; a daemon that predates it counts as older).
  If they differ but the protocol matches, for example an old daemon left
  running after an upgrade, they warn and suggest `formless daemon
  restart`; in the TUI, `/restart-daemon` restarts it and reconnects.
  `daemon status` warns too.
- **Rebuilds.** A rebuild keeps the version, so the daemon records its
  binary's identity at start (`build`: modification time, size and
  inode) and `Status` reports it with `stale` (the binary on disk has
  changed since). The TUI, `formless run` and `daemon status` warn the
  same way when `stale` is set or `build` differs from their own binary,
  so a client attaching late still hears it. Every 5 s the daemon checks
  its binary; once replaced it posts one note to attached clients and
  shuts down as soon as nothing would be interrupted (no Run, no held
  call, no attached client), so the next request starts the new build.
- **Approval mode.** `Status.secure` is the approval mode the daemon
  enforces (§10.3). `security.secure` in `config.toml` is re-read per
  call; `--secure` and `FORMLESS_SECURE` (`1/true/yes/on` or
  `0/false/no/off`; anything else is a config error) apply to the daemon
  started with them. A client run with `--secure` against a daemon whose
  approvals are off warns and suggests `formless --secure daemon
  restart`.
- **Ordering and responsiveness:** on each connection, ordinary requests
  run one at a time and in order on a per-connection worker, so a
  client's fire-and-forget sequences keep their order. Control requests
  skip that queue and run at once, with replies matched by request id:
  `Ping`, `Status`, `InterruptRun`, `CancelInvocation`,
  `ApprovalDecision`, `PromotionDecision` and `AnswerQuestion`. A slow
  smoke test, export or search therefore never delays Esc or an approval
  from the same client.

### 9.4 Instances, profiles, storage, data dirs

- Single instance per data dir (lock file); starting a second daemon against
  the same dir is refused and points at the running one. `--profile <name>`
  gives an isolated daemon + data/config dirs (intended for testing
  dangerous Tools). Named profiles live in sub-directories of the default
  profile's data and config dirs. `formless profiles list [--json]` shows
  each profile (default first), whether its daemon runs, when it was last
  used and its data dir, and marks the current one. `formless profiles rm
  NAME [--yes]` asks for the name to be typed, stops that profile's daemon
  and deletes its data, config, API key, OAuth tokens and relay secret. The
  default profile cannot be removed this way (`formless reset`).
- Data dir (OS-appropriate): `~/Library/Application Support/formless`
  (macOS), `$XDG_DATA_HOME/formless` (Linux), `%APPDATA%\formless`
  (Windows); config in the matching config dir (`config.toml` + keychain/
  fallback key file). Contents: SQLite DB, Tool version files, artifacts,
  rotated daemon logs, audit log.
- **SQLite** holds Sessions, Runs, Tool metadata/versions index,
  Invocations, metrics tallies, compaction checkpoints, approvals, audit
  events. Schema is versioned; the daemon auto-migrates on start and takes a
  **backup before migrating** (a consistent `VACUUM INTO` copy named
  `formless.db.v<old>-<UTC time>.bak`, whenever an existing database is
  older than the build). Each step runs in a transaction with its version
  bump, and a database newer than the build is refused, never downgraded. On corruption it refuses to guess: it stops,
  reports, and points at the backup (see Edge Cases, §16).
 - **Connection settings** (every connection): WAL, `synchronous=NORMAL`
   (SQLite's recommendation under WAL — commits survive a crash of the
   process, and the agent loop commits several times per step), a 5 s busy
   timeout, in-memory temp tables, a 32 MiB page cache, a 256 MiB read map
   and a 64 MiB WAL cap. A statement cache keeps the few dozen statements the
   loop re-runs from being re-parsed, and `PRAGMA optimize` runs on close.
 - **The Sessions list is one scan:** `sessions.message_count`, `preview` and
   `last_run_status` are columns kept true by triggers on `messages` and
   `runs` (schema v39, backfilled from the rows that existed) instead of
   three correlated subqueries per Session. v39 also adds the indexes the
   query plans asked for: `runs(session_id, created_at)`,
   `invocations(run_id)`, `invocations(session_id, tool_id)`,
   `invocations(tool_id) WHERE status='running'`, `artifacts(session_id)`
   and `audit(created_at)`.

Schema upgrades snapshot the database before any step, then commit each step
and its version bump together. A failure leaves the last completed version
and preserves existing rows. A small atomic receipt binds the failed database's
SHA-256 fingerprint to its original pre-upgrade snapshot and that snapshot's
SHA-256. An unchanged retry reuses this verified recovery point; a changed
source or missing/damaged snapshot gets a fresh copy. Names include fractional
UTC seconds and a unique suffix. A failed checkpoint or receipt write disables
reuse rather than sacrificing a backup or hiding the migration error.

### 9.5 Crash recovery

- Finishing a Run isolates comparison and job bookkeeping from unrelated
  finish-tail failures. A panic in naming, recap, or `run_end` hooks is
  logged and surfaced as a note, and still drains the next queued message.
  This applies after a Run panic as well as normal completion.

- The daemon never idle-closes mid-Run (§9.1) but can crash/be killed. On
  restart, affected Runs are marked **Interrupted**; the in-flight Invocation
  is recorded `interrupted — outcome unknown` and is **never automatically
  re-executed** (side effects unknowable). On reattach the TUI offers
  **Resume** (continue from the last completed tool call, with the
  interruption visible to the agent in context) or **Abandon**. All other
  Sessions/history are untouched. Protocol: `ResumeRun {session_id}`
  (allowed when the Session's last Run is `interrupted` or `paused`; marks
  it `resumed`, adds a harness note saying how it stopped, and starts a new
  Run from the transcript) and `AbandonRun {session_id}` (marks it
  `abandoned`). Before any Run starts, tool calls left without a result are
  answered with "the run stopped before this call finished; it was not
  retried", so the transcript stays valid for the provider.

### 9.6 Artifacts

- Large Tool outputs are stored as artifact files (streamed writes, §7.5);
  only a bounded preview (configurable length) enters model context. Outputs
  are referenced by artifact id in transcripts so compaction/resume can
  re-read them. Retention caps apply (§13, §15): artifacts older than
  `artifacts.max_age_days` (30) are removed, then the oldest beyond
  `artifacts.max_total_mb` (1024), at daemon start and hourly or on
  `formless gc`; deleting a Session deletes its artifact files.
- **Reconciling (#609).** Hourly and on `formless gc`, the artifacts
  folder is compared with the database: a file with no row (a crash
  between writing and recording it) older than an hour is removed, and a
  row whose file is gone is dropped, so usage and retention see the real
  disk. Changes are logged and audited (`artifact.reconcile`).
- **Disk use.** `StorageUsage` (`formless storage [--json]`, TUI
  `/storage`, web Settings → Storage → Disk use) lists the database (and
  roughly what fills it: messages, Invocation logs, Tool versions, file
  snapshots), artifacts, daemon and MCP logs, skills and backups, with the
  data folder's total.
- **Clean-up.** `Gc {dry_run}` (`formless gc [--dry-run]`, TUI `/gc` then
  `/gc run`, web Clean up… which shows the dry run before "Free"):
  artifacts past their retention, `/rewind` snapshots older than 30
  days, MCP logs cut to their last 1 MB, all but the two newest
  pre-migration copies by their recorded timestamp (not schema version),
  and the database's free pages (VACUUM). Each row
  says how many items and bytes; a real run is audit-logged.
- **Backups.** The daemon copies the database (`VACUUM INTO`, consistent
  under WAL) to `<data>/backups/formless-<UTC time>.db` when the newest is
  a day old (checked hourly), keeping `daemon.backups_keep` (7; 0 = none).
  `formless backup` / `Backup` / TUI `/backup` take one now. `formless
  restore FILE` (daemon stopped) checks the file (SQLite, integrity
  check, has Formless tables, schema not newer than this build), saves
  the current database as `backups/before-restore-<time>.db`, copies it
  in and migrates it. Backup and pre-restore filenames include fixed-width
  fractional UTC seconds and a unique suffix, preserving distinct restore
  points even within the same second; existing backup destinations are
  never replaced. If post-restore migration fails, the error identifies the
  safety copy; the copied database remains at its last completed schema
  version, and the safety copy can be restored even from this state.
  `formless doctor` shows the last backup's age.
- **Opening a stored output:** the transcript note reads `[full output (N
  bytes) stored as artifact ID; read it with read_artifact]`, with no
  filesystem path, because transcripts also reach remote web clients.
  `GetArtifact {id, offset?, length?}` returns a text slice (200 000
  characters by default) with `total_chars` and `end`, never the path.
  - TUI: a tool result that references an artifact shows `/open <id>`,
    and `/open` (the newest) or `/open <id-prefix>` shows a full-screen
    pager (↑↓, PgUp/PgDn, g/G, Esc).
  - Web: the result gets an "Open full output" button that opens a viewer
    with "Load more" and Download, which fetches the rest and saves it as
    a file.

### 9.7 Scheduled jobs

A job is a prompt that runs later or periodically: re-check a build,
refresh a cache, nightly cleanup.

- **Creating:**
  - The agent uses `schedule_run({prompt, cron? | at?, session_id?})`.
    `cron` is a 5-field expression in local time (`*`, lists, ranges,
    `/step`, `@hourly`/`@daily`/`@weekly`/`@monthly`; when both day fields
    are restricted, either may match). `at` is RFC 3339,
    `YYYY-MM-DD HH:MM` or `in 30m|2h|1d`.
  - The user uses `AddJob` or `formless jobs add PROMPT --every … |
    --cron … | --at …`. A user's schedule may be plain English: "every
    weekday at 9am", "every 2 hours", "every 15 minutes", "daily at
    18:30", "hourly", "every monday and friday at noon", "every weekend
    at 8pm"; it is stored as cron. `PreviewSchedule {schedule}` returns
    the cron and the next 3 runs, which `jobs add` prints to confirm.
- **Trust:** one-shot jobs from the agent are scheduled at once.
  Recurring ones need the user's approval: asked immediately, or, with
  nobody connected, saved as "awaiting approval" until the user enables
  them. Enabling a job approves it.
- **Running:** the daemon checks every 20 s. A due job sends its prompt to
  its Session (queued if that Session is busy), or to a new Session named
  `Job: …`. A one-shot job then ends; a recurring one moves to its next
  match after now. Firings missed while the daemon was down or the
  machine slept run once, and its history says "ran late, N firings
  coalesced". Everything is audit-logged and broadcasts `JobsChanged`.
- **Overlap:** each job has `overlap`: `skip` (default) skips a firing
  while the previous one is still queued or running; `queue` queues it.
  Either way a Session never holds more than 5 queued messages: further
  firings are skipped. Skips are recorded in the history and audit-logged
  `job.skip`.
- **History:** every firing is a `job_runs` row: trigger (`schedule` or
  `manual`), outcome (the Run's final status, or `skipped`), duration,
  cost, Session and details. `JobRuns {id}`, `formless jobs history ID`,
  `/jobs history ID` and the web "History" button show it. A firing whose
  Run doesn't complete posts a note to every client.
- **Run now:** `RunJobNow {id}` (`formless jobs run ID`, `/jobs run ID`,
  web "Run now") fires a job at once under the same overlap rules; its
  schedule is unchanged.
- **Delivery:** `deliver` (`AddJob`, `SetJobOptions`; `jobs add --notify
  --to-file PATH --webhook URL`) sends the answer when the firing's Run
  ends: a desktop notification, a Markdown entry appended to a file,
  and/or a JSON POST (`job_id, prompt, status, answer, session_id`).
  These are targets the user configured, so they need no approval; the
  results go in the history and the audit log (`job.deliver`). For other
  channels (email, chat) point a webhook at them, or have the prompt use
  an MCP Tool.
- **Idle rule:** the daemon still idle-closes (§9.1) unless the user sets
  `daemon.keep_alive_for_jobs = true`, which keeps it up while enabled
  jobs wait. Otherwise due jobs run at the next start. Creating or
  enabling a job with it off answers with a `warning`, which the CLI
  prints; `GetSettings.jobs_warning`, `/jobs`, the web Jobs section and
  `formless doctor` say it too.
- **Clients:**
  - CLI: `formless jobs list|add|run|history|enable|disable|rm`
  - TUI: `/jobs`, `/jobs enable|disable|run|history|rm ID`
  - web: the Settings "Jobs" section (approve, enable, run now, history,
    delete).

### 9.8 Hooks

Hooks let users enforce policy and automate workflows. Each is a
`[[hooks]]` entry in `config.toml`, and everything runs daemon-side.

```toml
[[hooks]]
event = "pre_tool"              # pre_tool | post_tool | run_end
tool = "mcp__*"                 # optional: name, with * wildcards
capability = "net"              # optional: a declared Capability prefix
run_tool = "network_policy"     # a registry Tool, or…
# command = ["/usr/local/bin/check"]   # …a command (no shell)
# trusted = true                # run the command without asking
# timeout_secs = 30
```

- **Input:** a hook gets the event as JSON: event, tool, arguments, the
  Tool's Capabilities, Session and Run. post_tool adds the result
  (clipped to 4000 characters) and `ok`; run_end has the Run's `status`.
  A Tool hook gets it as input, and a command gets it on stdin.
- **pre_tool:**
  - Runs before every matching call, built-ins included.
  - A Tool hook returning `{deny: true, reason}`, or a command exiting
    non-zero (its output is the reason), denies the call. The model sees
    `error: denied by a pre_tool hook: <reason>`.
  - Hooks run in order, and the first denial wins.
- **post_tool and run_end:** automation only. Their results are logged
  and never change the call.
- **Trust:**
  - Hook Tools run through the normal approval rules.
  - With approvals off (`security.secure = false`, the default) a hook
    command runs without asking like any Tool, logged as
    `hook.run_unasked`. With them on, a command runs without asking only
    when `trusted = true`. Otherwise
    the user approves it once per daemon lifetime, and an unapproved
    pre_tool command fails closed by denying its call.
  - Hooks run outside model turns, so they never trigger themselves, and
    every denial is audit-logged.

## 10. TUI (v1 user interface)

### 10.1 First-run onboarding wizard

- If no valid config exists, first launch runs a TUI wizard: (1) enter
  `base_url` + API key, (2) fetch `/models` (manual fallback), pick model +
  reasoning effort, (3) run the endpoint probe (§4.3) incl. test tool-call;
  failure blocks Session start with an explanation, (4) optionally type a
  first message (nothing pre-filled, no starter Tools).
- **`/setup`** opens the wizard again at the saved provider's key step
  (re-enter the key, or Esc back to change provider); finishing saves and
  reconnects as on first launch.
- **Moving around.** Esc goes back one step at every step (effort →
  model → key → URL → provider), keeping what was typed; the header shows
  "step N of 5".
- **The API key** shows as `••••abcd` (only the last 4 characters);
  Ctrl+R shows it in full while typing, and pasting works (bracketed
  paste). Ctrl-key presses never type letters into it.
- **Resuming.** After every key the wizard keeps a draft in
  `<config>/setup-draft.json`: the step, preset, base URL, model and
  effort, never the key. The next launch resumes there (at the key step
  for providers that need one); finishing deletes it.
- **No key needed.** With no saved setup and no provider key in the
  environment, a running Ollama or LM Studio (a quick check of its port)
  is pre-selected and offered: "press Enter to use it (no account or API
  key needed)".
- **The first approval** (TUI and web) adds a short explainer: Tools have
  no access by default, what the Capabilities are, Always vs Once vs this
  Session vs Deny, and `/approvals` / `/revoke` to undo. It is shown until
  the first answer (`<data>/tui-seen.json`; the web remembers it in the
  browser).
- **A guided first task.** Once setup finishes, a note offers `/tour`
  (and `/tour skip`, which hides the offer for good); it is offered once.
  `/tour` puts a sample task in the input that makes the agent build a
  Tool, then points to the Tools view when `create_tool` succeeds and to
  `/help` when the Run ends.

- **What opens on launch.** `tui.on_start` is `last` (default: the most
  recently active, unarchived Session; not the first row, since pinned
  Sessions list first), `new` (a fresh Session) or `picker` (the Sessions
  view). `formless -c` / `--continue` and `formless --new` override it for
  one launch.

### 10.2 Home (chat) view

- Single-column streaming chat: model deltas, inline **collapsible** tool
  calls (name, version, input, status, elapsed; Pending Invocation badges
  with live elapsed + `possibly stalled` flags, §6), compaction notices,
  optimiser activity notes. Full scrollback is preserved across modal
  overlays and view switches.
- **Which Session gets the input (TUI).** The input box's border names
  the target Session (`Message › to: refactor-auth`) and briefly flashes
  when it changes.
- **Drafts per Session (TUI).** Unsent input and pending attachments
  belong to their Session. Switching away keeps them and coming back
  restores them, so a half-typed message never goes to the wrong Session.
  Text drafts are saved in `<data dir>/tui-drafts.json` when the TUI
  exits and restored on the next start; attachments are not.
- **Empty Session.** A new Session shows one line on what Formless is
  (it builds the Tools a task needs and keeps them), four example tasks
  (search a repo, summarise a page, a recurring job, "what can you do?"),
  and hints for `/help`, `/model` and `@`. In the TUI, 1–4 on an empty
  input put an example in the input to edit or send; on the web the
  examples are buttons.
- **Run footers (TUI).** Under the last message of each finished Run, a
  dim footer: `✓ 2m14s · 7 steps · 3 tools · $0.04`, or
  `■ interrupted after …` / `⏸ paused after …`. Footers come from the
  Session's Run metrics (`RunMetrics {session_id}`), so earlier Runs keep
  theirs; a Run's messages are those before the next Run began. Cost
  shows for the Run just finished, when prices are configured.
- **Moving through a long Session (TUI).** Ctrl+↑/Ctrl+↓, or `{`/`}` on
  an empty input, jump to the previous/next user message. Past the last
  one, the chat follows the live bottom again. Home on an empty input
  goes to the top of the Session. While scrolled, the indicator shows
  where you are: `message 4/17 · ↑ N more below`.
- **Scroll position (TUI).** The chat panel draws a scrollbar on its right
  border when the Session is longer than a screen. While scrolled away
  from the bottom, the title shows where you are:
  `message 4/17 · line 120/400 · 30% · End to follow`.
- **New output while scrolled up (TUI).** While you're not following the
  bottom, output that arrives is counted in a badge on the chat's bottom
  border (`↓ 12 new lines · End`). End, or clicking the badge, jumps to
  the bottom and follows again.
- **Find in this Session (TUI).** Ctrl+F turns the input into a find bar.
  Matches (case-insensitive, within the rendered chat) are highlighted,
  the current one brighter. The bar shows `3/11`. Enter/Shift+Enter step
  to the next/previous match and scroll to it; a new query starts at the
  match nearest the bottom. Esc closes the bar and follows the bottom.
  `/search` still searches every Session.
- **Links (TUI).** URLs in the chat, from Markdown links or bare, are
  clickable in terminals that support OSC 8 hyperlinks. After each draw,
  the TUI repaints the visible URL cells wrapped in OSC 8, keeping their
  style, but only when the set of visible links changed
  (`tui.hyperlinks`). A URL ends where its brackets balance
  (`…/f_(x))` keeps one `)`), and a long URL hard-wrapped across rows is
  one link on every fragment, never a link to its first piece. In the
  Markdown, an image inside a link shows both addresses, and inline HTML
  shows its text: `<b>`, `<i>`, `<s>`, `<code>`, `<kbd>` and `<a href>`
  style it, other tags and comments are dropped. `/open link [N]` opens the Nth link in the last
  answer with the system opener (`open`, `xdg-open`, `start`).
  File paths in the chat (`src/main.rs`, `./a/b.txt:42`, `Cargo.toml`)
  that exist under the TUI's working directory are linked the same way,
  to `file://`. Existence checks are cached. `/open path FILE[:LINE]`
  opens one in `$VISUAL`/`$EDITOR` at that line: `+LINE` for terminal
  editors (which get the terminal until they exit), `FILE:LINE` for
  Helix, and `-g` for VS Code and its forks.
- **Inline images (TUI).** Image attachments the TUI can read get an
  8-row thumbnail under their message. After each draw, thumbnails whose
  rows are all on screen are sent with the terminal's own protocol, which
  scales the image into the box: iTerm2's inline images (iTerm2,
  WezTerm) or the kitty graphics protocol (kitty, Ghostty), which takes
  PNG: JPEG, GIF and WebP are transcoded to a PNG thumbnail (at most
  800 px) first. Rows are reserved only for an image that can be drawn;
  one that can't keeps its reference line, marked "can't be shown in
  this terminal".
  When thumbnails move, the screen is redrawn first so none linger. Other
  terminals, tmux, and `tui.inline_images = "off"` keep just the
  reference line.
- **Mermaid diagrams (TUI).** Flowcharts, state, sequence and pie
  diagrams are drawn in the chat; anything the renderer can't draw
  faithfully shows its source instead of a wrong picture: other diagram
  kinds, two-way edges (`<-->`) and cross/circle tips (`--x`, `--o`).
  `;` splits statements except inside quotes, brackets, `|edge labels|`
  and the text after a `:`; a subgraph's `direction` stays inside it;
  note bodies are never states; participants may contain hyphens; a
  labelled self-loop shows `↺ label`.
- **Wide Mermaid diagrams (TUI).** A diagram wider than the chat shows a
  cut-down preview of its first 12 rows with the hint `/pager diagram N ·
  /open diagram N` (N is the block's `[N]` label). `/pager diagram [N]`
  opens it at full size in the pager, where ←/→ scroll sideways.
  `/open diagram [N]` opens its source in mermaid.live
  (`#base64:` state).
- **Diffs (TUI).** Unified diffs show coloured: added lines green, removed
  red, hunk headers cyan, file headers bold. Runs of more than 7
  unchanged context lines collapse to their first and last 3
  (`⋯ N unchanged lines`). This applies to ```` ```diff ```` blocks and to
  Tool results that are a diff or hold one in a JSON string field. A
  collapsed call with a diff result shows `+added −removed`.
- **Code blocks (TUI).** Code never wraps, since that breaks indentation;
  lines longer than the pane end in `…`. `/pager code [N]` opens the last
  answer's Nth code block (default the last) unwrapped in the pager, with
  line numbers and ←/→ to scroll sideways.
  Known languages are syntax-coloured by a small tokenizer that carries
  block comments (nested in Rust) and multi-line strings (Python `"""`,
  JS/Go backticks, Rust raw strings) across lines and tells a JS regex
  from a comment; `diff`/`patch` (any case) are coloured as diffs; other
  languages are plain text in the default colour.
  Each block's top border names its language and its index in the
  message (`╭─ rust [2]`), which is the number `/copy code 2` and
  `/pager code 2` use. `tui.code_line_numbers` adds dim line numbers.
- **Tables in narrow terminals (TUI).** Markdown tables wider than the
  pane shrink: columns narrower than an equal share keep their width, and
  the rest share the room in proportion. Cell text wraps inside its
  column instead of being cut. When the pane can't give every column 4
  characters, the table is shown as one `key: value` record per row,
  each value wrapping under itself with a hanging indent.
  `/table [N]` opens the last answer's Nth table at full width in the
  pager, where ←/→ scroll sideways.
- **Tool calls in the TUI chat.** Persisted calls start collapsed to one
  line: `▸ ⚙ name ✓`, an input summary, and the first line of the result
  (✗ on failure, … while there is none). Their separate result lines are
  folded in. `[`/`]` on an empty input focus the previous/next call and
  scroll it into view. `[`/`]` also stop on messages with text (yours
  and the answers). While a call is focused, Enter expands or collapses
  it, and `z`/`Z` expand or collapse all calls. On a focused message or
  call, `c` copies its text and `C` copies it as Markdown (a call as its
  name, input and result in fences). Esc or typing leaves the focus.
  `/copy N` copies the Nth message from the bottom.
  On a focused message of yours, `e` loads it into the input to edit and
  re-run from there (`EditMessage`). If later user turns would be
  discarded, the first Enter asks and a second confirms, and Esc cancels
  the edit. `f` forks a new Session with everything before that message
  (`ForkSession up_to_seq`) and puts the message in its input. An expanded call shows its input and its result or error,
  pretty-printed and highlighted when JSON (capped at a few hundred lines;
  `/open` shows stored outputs in full).
- **Input editor (TUI).** The message box is a multi-line editor with
  readline keys:
  - ←/→ move by character and Alt or Ctrl+←/→ by word. Home/End and
    Ctrl+A/E go to the line start and end; End on an empty input jumps
    the chat to the bottom.
  - Ctrl+W (Alt or Ctrl+Backspace) deletes a word back and Alt+D a word
    forward. Ctrl+U and Ctrl+K delete to the start and end of the line.
  - Shift+Enter, where the terminal reports it, or `\` + Enter inserts a
    newline. The box grows to 8 rows.
  - ↑/↓ move between lines. On the first or last line they walk the
    current Session's earlier messages, and Down past the newest returns
    the draft. Ctrl+R searches that history.
  - Bracketed paste is on, so a paste is one block and its newlines
    never send. Pastes over 8 lines or 1,500 characters show as
    `[pasted N lines #k]` and are sent in full; Backspace removes one as
    a unit.
  - The chat scrolls with PgUp/PgDn and the mouse wheel.
  - Text is handled as the reader sees it (here and in the Tool
    editor): the cursor, Backspace and Delete step over whole grapheme
    clusters (a Thai letter with its marks, an emoji sequence, a flag),
    rows and the cursor column are measured in screen columns (CJK is
    two wide, combining marks none), and in scripts written without
    spaces (Thai, Chinese, Japanese) a word step is one cluster. Chat
    text in those scripts wraps between clusters, never inside one.
  - Key releases (the Windows console, kitty's event types) are ignored;
    in the web client no key handler acts while an IME is composing.
- **Unicode matching and direction.** Fuzzy pickers, `@` file mentions,
  search snippets and web search highlights match on folded text that
  keeps each character's original position: case folding never shifts a
  highlight (`İstanbul`), and NFC and NFD spellings match each other
  (macOS file names are NFD). Fuzzy match positions count original
  Unicode characters, including when a match follows `İ`, rather than
  indices into a lowercased copy. In the web client each message paragraph
  takes its direction from its own text, so Arabic and Hebrew answers are
  right-aligned beside left-aligned English, and Session names are
  isolated from the time beside them. Approval prompts in both clients
  show bidi control characters as `⟨U+202E⟩` instead of obeying them, so
  a name can't display reversed.

### 10.3 Approvals (modal)

- **Plain-language summary.** Code means little to non-programmers, so a
  Tool's approval also shows what it does in a few plain sentences: what
  it reads, what it changes or sends, anything risky.
  - **How it's written:** the session's model writes it once per Tool
    version, in the background (the prompt never waits for it). It's
    cached in the daemon-only store (`harness:approval_summaries`,
    `<tool id>:v<version>`) and sent as `ApprovalSummary {approval_id,
    tool_id, version, summary}` after `ApprovalRequested`.
  - **Labelling:** clients show it at the top, labelled as an AI summary
    that may be wrong, with the Capabilities as what counts. The model
    is told to treat the Tool's text as data, never instructions.
  - **Which approvals:** MCP and other non-Tool approvals get none.
  - **Turning it off:** `tools.approval_summaries = false`, so no model
    call is made.
- **Approval mode.** Approvals are **opt-in**: `security.secure`
  defaults to `false`, and then sensitive Tool calls run without asking.
  Each such call is still audit-logged as `tool.run_unasked`, and so is a
  hook command (`hook.run_unasked`). Remembered denials (below) apply in
  either mode. `security.secure = true` (or `formless --secure` /
  `FORMLESS_SECURE=1` when the daemon starts) turns prompts on; everything
  below about prompts, grants and failing closed describes that mode.
  The wizard says which mode is in effect when it finishes, and the
  Settings views, `GetSettings` (`security`) and `formless doctor` show
  it with the trusted folder and host counts.
- With approvals on, the first execution of a Tool whose Capabilities are
  sensitive (`fs:write`, `net`, `process`, `env`, or any widening)
  requires approval. Approval is
  **per-Tool and survives version bumps; it re-prompts only when
  Capabilities/`memory_mb` widen**. Input-bound scopes (§7.4) are approved
  as their resolved values, per call: approving a path/host adds it to the
  Tool's accumulated grant, and only an uncovered new path/host re-prompts, showing the capability diff (new items
  highlighted, broad unscoped grants flagged). Replay-limited promotions
  (§8.4) use the same modal. In parallel batches, the modal lists all held
  calls and asks for each in turn (§5.2). Modals are keyboard-driven,
  never lose scrollback.
- The answer has a scope (`ApprovalDecision.scope`): **always** (`y`, the
  default for clients that omit it) stores the grant as above; **once**
  (`o`) allows only the waiting call and stores nothing, so the next call
  asks again. **This Session** (`s`, scope `session`) covers the Tool's
  later calls in the same Session, accumulating like a stored grant. It
  is kept only in the daemon's memory, keyed by Session and Tool, and ends
  when the Session is deleted or the daemon restarts; other Sessions
  still ask. **For an hour** (`1` in the TUI and web, `h` in plain mode,
  scope `hour`) covers the Tool's calls in every Session for the next
  hour, accumulating the same way. It too is kept only in memory: when it
  ends, or the daemon restarts, the next call asks again. For MCP
  server approvals,
  both act as *once*. Headless `--yes` answers every prompt with *once*.
- **Broad grants stand out.** A new Capability is shown in red, with
  what it reaches, when it is bare `net` ("any host on the internet") or
  bare `process` ("any program on this computer"). The same goes for
  `fs:write` on `/`, a home folder or `/Users`/`/home` ("writing anywhere
  in your files"), and for `env` with a secret-looking key such as
  `*TOKEN*`, `*SECRET*`, `*PASSWORD*` or `*KEY` ("a secret from your
  environment").
  - **The rule:** one rule (`formless_protocol::broad_capability`) that
    the web mirrors.
  - **Second press:** granting such a Capability beyond the one call
    (always, this Session or for an hour) takes a second press of the
    same key or button; the first only warns. Allowing it once and
    denying take one press.
- **Where paths lie.** A prompt's Capabilities are its resolved values,
  so input-bound scopes show the actual absolute path or host. For each
  new `fs:` path, the prompt adds a note (`formless_protocol::path_note`,
  mirrored by the web):
  - **Secret folders** (⚠ in red): `.ssh`, `.aws`, `.gnupg`, `.kube`,
    `.docker`, `Keychains`, `.password-store`, `.config/gh` and
    `.config/gcloud` ("holds SSH keys", …), and `/etc`.
  - **Outside the Session's working folder** (⚠): flagged with that
    folder named.
  - **Inside it:** a quiet "inside the working folder".
  - **No working folder:** only the secret folders are flagged.
- **Trusted hosts.** `[security] trusted_hosts = ["api.example.com",
  "*.github.com"]` lists hosts whose `net:` grants need no approval. A
  call runs unasked when everything sensitive it asks for is a trusted
  write (below) or a `net:` host on the list, and is logged as
  `tool.approve_trusted`.
  - **Matching:** `api.example.com` matches that host and its subdomains
    (as a `net:` grant does everywhere); `*.github.com` matches
    `github.com` and every subdomain. Look-alikes (`evilgithub.com`) don't
    match.
  - **Never trusted:** bare `net` (any host), a grant wider than the
    trusted host (`net:example.com` against `api.example.com`), `*` and a
    bare TLD. Like trusted folders, it is set in `config.toml` only.
- **Path scope.** Each new `fs:read:`/`fs:write:` path on a prompt can
  be granted wider: the path itself (default), its folder, or the
  repository it lies in (the nearest folder holding `.git` (a folder or
  a file, so worktrees count), `.hg`, `.jj`, `.svn`, `.pijul`, `_darcs`
  or `.fossil`), else the project (`Cargo.toml`, `package.json`,
  `pyproject.toml`, `go.mod`). `ApprovalRequested.path_options` lists
  them; the answer's `paths` maps a requested spec to the chosen folder.
  - **Read and write apart:** each Capability has its own pick (TUI: `r`
    cycles a read path, `p` a write path, and `R`/`P` move to the next
    one; the web shows a select per path).
  - **Lasting answers only:** a wider folder belongs to an Always,
    Session or Hour grant; "once" runs the call as asked, so clients
    don't send picks with it and nothing is logged as widened.
  - **Checked by the daemon:** the folder must contain the requested
    path by whole components (`/proj` never covers `/proj-secrets`),
    after resolving symlinks on both sides (a file not written yet is
    resolved through its parent); `/` and the home folder (or above) are
    never offered or accepted. Anything else is refused: the exact path
    is granted, the user is told in a note, and `tool.widen_refused` is
    logged. A widened grant is logged as `tool.approve_widened` with its
    scope.
  - **Compatible:** clients that send no `paths` grant the exact path,
    and stored grants keep working (a folder grant already covers what
    lies beneath it).
- **Bound to the code.** A grant (stored, Session or hour) is for the
  Tool version's code and input schema it was given for: a new version
  with other code, from the agent, an import, the optimiser or an MCP
  sync, asks again (its prompt says the code changed; logged
  `tool.grant_code_changed`), even with the same Capabilities.
- **Always deny.** Besides deny (this call), a Tool's prompt offers
  "always deny" (TUI `N`, web button or `N`; not on prompts for an MCP
  server or a harness change, which have no Tool to remember it for, and
  only the answer that resolves a prompt counts): the call's new sensitive
  Capabilities are remembered (`denials` table, logged
  `tool.deny_always`) and every later call of any Tool that needs access
  they cover (a denied `net:example.com` covers its subdomains, a bare
  `net` all hosts) fails without a prompt, logged `tool.deny_remembered`,
  even with approvals off (`security.secure = false`).
  The agent's error says so ("the user always denies net:…; don't
  attempt this access with any Tool"). `formless approvals denials`
  lists them, and `--forget CAP` / `--all` (protocol `ListDenials`,
  `ForgetDenial`) makes them ask again.
- **Broad Tools are named at creation.** When `create_tool` saves a Tool
  that declares a broad Capability (bare `net` or `process`, writing
  anywhere, a secret env key), the chat gets a note before any approval,
  and the agent's result carries `least_privilege` hints with the
  input-bound alternative (`net:$input.url`, `fs:write:$input.path`, …).
- **Security overview.** `/security` (TUI pager) and
  `formless security status [--json]` (protocol `SecurityStatus`) show
  on one screen: grants by Capability family (broad ones flagged),
  remembered denials, trusted folders, MCP servers (transport, target,
  trusted, disabled), hooks that run without asking, the relay and
  whether the web UI is running.
- **Long prompts.** The answer keys and hint line are a footer pinned to
  the bottom of the prompt. The body is sized by its wrapped height; when
  it doesn't fit it is cut with "… more above the keys (v to see all)".
  A non-Tool approval's detail (an MCP call's input) shows at most 12
  lines of 300 characters, and `v` opens it whole.
- **Trusted folders.** `[security] trusted_paths = ["~/code/app", …]`
  lists folders whose file access needs no approval.
  - **What's covered:** reading files never asks. A call whose only
    sensitive Capabilities are `fs:write` inside a trusted folder (input
    bound or fixed, resolved per call) runs without asking, and is logged
    as `tool.approve_trusted`.
  - **Never trusted:** paths with `..`, folders that hold secrets (`.ssh`,
    …, as above), and anything else sensitive in the same call.
  - **Turning writes off:** `trust_writes = false` keeps writes asking.
  - **Where it's set:** `config.toml` only (`formless config edit`).
    Clients can't set it over the protocol and the agent can't change it,
    since it widens what runs unasked.
- **One list of every grant.** `ListApprovals` returns `GrantInfo` rows,
  one per Capability, covering stored, Session and timed (`hour`) grants.
  - **Each row has:** Tool, Capability, scope, when the grant was last
    widened, which client answered (stored as `approvals.granted_by`,
    schema v45; kept in memory for the others), when an hour grant ends,
    the Session of a Session grant, and the Tool's last Invocation.
  - **Revoking:** `RevokeGrant {tool_id, capability?, scope,
    session_id?}` removes one Capability, or the whole grant, from any of
    the three.
  - **TUI:** `/approvals [FAMILY]` (a table in the pager) and `/revoke all
    [FAMILY]`; `/revoke TOOL [CAPABILITY]` also drops the Tool's hour grant
    and its grant in the current Session. `/approve` still lists the
    approvals waiting now.
  - **Web:** an Approvals tab with a family filter, revoke per row,
    "Revoke selected" and "Revoke all shown".
  - **CLI:** `formless approvals list [--family F] [--json]` and
    `formless approvals revoke TOOL [CAPABILITY] [--scope …] [--session
    …]` or `--all [--family F]`.
- The prompt (`ApprovalRequested`) carries `already_allowed`, the Tool's
  current grant. Clients mark those Capabilities "already allowed" and the
  rest "new". It also carries `code_preview`, the first 15 lines of the
  Tool's code, so the user sees what they are trusting.
- **Grants and revoking:** a Tool's detail (`GetTool`) includes its
  accumulated grant (`approval`: Capabilities and memory).
  `RevokeApproval {tool_id, capability?}` removes one Capability spec or
  the whole grant; the next call that needs it asks again. Revokes are
  audit-logged. Clients: TUI `/revoke NAME [CAPABILITY]` and the Tool
  detail's "Approved" list; web Tool detail × per item and "Revoke all".

- **Reading before trusting (TUI).** In the approval, `v` opens the
  Tool's full code for that version in the pager, numbered and
  highlighted (`ApprovalRequested.tool_id` identifies the Tool). `?`
  explains each capability in plain words under it, e.g. "fs:write:/x —
  can create, overwrite or delete files under /x".
- **Typing while an approval is open (TUI).** The approval stays on
  screen, but its keys (y/o/n) only apply while it has focus. Tab moves
  focus between the approval and the input, and typing anything else
  moves focus to the input, so a steer message can be written while
  deciding. The focused side is highlighted, and the approval's title
  says "Tab to answer" when the input has focus.

### 10.4 Sessions, Tools, and Settings views

- **Renaming.** Rename starts from the current name (Esc cancels).
  `/rename auto` (web: "Suggest name") re-runs the namer on the last few
  user messages and offers its name for editing before it's applied;
  `/rename NAME` sets it directly.
- **Automatic names.** After each Run an "Untitled" Session is named from
  its first substantive user message (a trivial opener such as "hi" is
  skipped once something follows it; a long message contributes its head
  and its tail). Names record their source (schema v56
  `sessions.name_source`): a heuristic name (first line, a few words, or
  24 grapheme clusters when the text has no spaces, as in Thai or CJK) is
  provisional and is redone on later Runs, and once the naming model is
  installed every provisionally named Session is renamed by it once. A
  model's name is final, and a user's name is never changed. The model is
  asked for a title in the conversation's own language; titles are cut
  between grapheme clusters, never inside one. Measured with `formless
  naming test` on qwen2.5-0.5b: "ช่วยเขียนสคริปต์ที่แปลงไฟล์ซีเอสวีเป็นเจสัน…"
  → "สคริปต์เจสัน"; "สรุปข่าวเทคโนโลยีประจำสัปดาห์นี้ให้หน่อย" →
  "ข่าวเทคโนโลยีสัปดาห์นี้"; "このリポジトリのビルドが失敗する原因を…" →
  "ビルド失敗の原因を調べる"; mixed "ช่วย debug โค้ด Rust ตัวนี้…" →
  "Debug Rust Code".
- **Attached clients.** `Subscribe` takes a `client` label ("TUI",
  "TUI (plain)", "web", "web (relay)", "CLI") and replies with the
  connection's id, `{subscribed, conn}`. Whenever the clients showing a
  Session change (subscribe, switch away, disconnect), its subscribers get
  `SessionClients {session_id, clients: [{conn, label}]}`; the TUI status
  line and web status bar show "also open in web, TUI ×2".
  - User messages record the sender in their meta (`client: {conn,
    label}`). A message from another kind of client, or from another
    connection still attached, shows "sent from web" under it.
  - An answered approval broadcasts `ApprovalAnswered {approval_id,
    approved, by}`; other clients close their prompt and note "approval
    granted in web".
- **Sessions view:** list/create/rename/resume/delete/export/import. Two
  TUIs may attach to the same Session simultaneously: both are full
  participants (like two terminals on one tmux session); messages are
  attributed by client; an approval is resolved by **whoever answers
  first** (the other modal closes: “answered elsewhere”).
- **What a Tool costs.** Besides p50/p95 latency and Cost per Success
  (§8.1), Tool detail shows the Tool's own model use per call.
  - **What counts:** tokens spent by its `llm` calls and the `agents.run`
    sub-agents it starts, charged to the Invocation that made them
    (`invocations.model_tokens`, `model_cost`, `model_unpriced`; schema
    v44).
  - **Cost:** `ToolMetricsInfo.model_tokens_per_call` and
    `model_cost_per_call` average over all calls. USD comes from
    `provider.prices` at the time of the call; if any model used was
    unpriced, the cost is left out rather than understated.
  - **Where it shows:** TUI "model use", web "Model use", and `formless
    tools show` (with p50 and p95). Tools that never use a model show
    nothing extra.
- **Dependencies.** `ToolDetail.uses` lists the Tools a Tool's current
  code calls, and `ToolDetail.modules` the modules it loads.
  - **Which calls count:** literal names only, from `tools.call("x")`,
    `requireModule("y")` and a saved script's `uses` list. A name built at
    run time can't be known.
  - **Used by:** `ToolDetail.used_by` lists the active Tools whose current
    code calls this one.
  - **Where they show:** the TUI detail, the web detail (names link to
    those Tools) and `formless tools show`.
  - **Warnings:** retiring or purging a Tool that others use warns first,
    naming them. The TUI needs a second `x` (purge already needs a second
    `p`). The web asks to confirm. The CLI prints a warning, and
    `tools retire` asks on a terminal.
- **Dry run.** A Tool can be run by hand without real side effects
  (`DryRunTool {id, input, session_id}`; `HostMode::DryRun`).
  - **Real:** reads (files, GET and HEAD requests, env, the store,
    `harness.read`) and `llm` calls.
  - **Not made, logged instead:** file and store writes go to a throwaway
    overlay that the run itself can read back. Process runs, non-GET
    requests, clipboard writes, notifications, harness changes, sub-agents
    and nested Tool calls aren't made, and emitted output is thrown away.
  - **Approval and recording:** input validation and approval are as for a
    real run, but nothing is recorded as an Invocation.
  - **Result:** `{ok, result?, error?, effects, logs}`, where `effects`
    are the would-be effects in order ("write 12 bytes to /tmp/x", "run
    touch /tmp/y", "send POST https://example.com/endpoint"). They are kept even when the
    run fails.
  - **Where:** `formless tools run NAME --dry-run`, Ctrl+D in the TUI run
    editor, and the "Dry run" box in the web's Try it form.
- **Example call.** `ToolDetail.example` is the Tool's latest successful
  Invocation: its version, input and output as recorded (already
  redacted, §15), with the output cut to 4 KB. It is shown as "Example" in
  the TUI and web Tool detail and in `formless tools show`. It also appears
  in `read_tool`'s result, so the agent sees a real call, and travels in
  export bundles as `example: {input, output}` (import ignores it).
- **Tool trend.** `ToolInfo.recent` holds the outcomes of the last 40
  finished Invocations, oldest first (`s`/`f`; denied and cancelled
  calls don't count). The TUI Tools list shows the last 20 as a sparkline
  (tall green success, short red failure) with ↑/↓ when the success rate
  moved 10 points or more against the 20 before (both windows ≥5 calls).
- **Recent calls.** `ToolDetail.recent_calls` lists the last 20
  Invocations, newest first (time, Session, status, duration, input,
  output and error, each capped at 20 KB). The TUI Tool detail shows them
  under "Recent calls": `[`/`]` select one, Enter opens its full input,
  output and error in the pager, and `J` opens its Session focused on the
  first Tool result at or after the call's time.
- **Web code editor.** The web Tool code editor:
  - highlights JavaScript (a highlighted layer behind a transparent
    textarea, using the client's own highlighter);
  - marks the bracket pair at the cursor;
  - checks the code as you type with `CheckToolCode {code}` → `{ok,
    error?, line?}`, the same gate saving uses, then marks the failing
    line and shows the error under the editor.

  The syntax gate now reports the thrown error itself ("SyntaxError: …
  at eval_script:3:35") rather than a generic "exception", so agents see
  it too.
- **Running a Tool by hand.** `RunTool {id, input, session_id}` runs a
  Tool as the agent would (same approval path, recorded as an Invocation
  in that Session) and returns `{result}`. TUI: `t` in the Tools view opens
  an input editor pre-filled from the input schema (defaults, first
  example or enum value, else empty values); Ctrl+S runs it and the result
  opens in the pager. Web: "Try it" in the Tool detail opens a form built
  from the schema (text, number, checkbox, choice, or JSON for nested
  values) and shows the result.
- **Why a Tool needs attention.** `ToolDetail.attention` explains it in
  plain words: the optimiser giving up after N failed candidate fixes
  (what sets the flag, with the last attempt's reason), and live signals
  shown even without the flag: under 50% success over 5+ calls, a streak
  of 3+ failures (with the latest error), invalid input in a fifth or
  more of calls, or a rollback in the last week. The TUI and web Tool
  detail list them ("Why it needs attention" / "Worth a look") above the
  recent calls, where the failures can be opened.
- **Editing what the agent sees.** `UpdateToolSpec {id, description?,
  input_schema?, output_schema?}` saves a new version with the given
  description and/or schemas and the current code, Capabilities and
  memory, through the usual gates, author `user`. In the TUI Tools view
  (with the detail open) `D` edits the description and `S` the schemas
  (one JSON object holding `input_schema` and `output_schema`); Ctrl+S
  saves.
- **Grouping Tools.** In the TUI Tools view `c` cycles grouping: none,
  by Capability family (the families a Tool holds, e.g. "fs + net"), by
  the current version's author (`ToolInfo.author`), or by status (needs
  attention first, then active, then retired). Groups get headers with
  counts; `z` folds the selected Tool's group, `Z` unfolds them all.
  Filter, sort and quick filters apply inside groups.
- **Metrics range.** `r` in the TUI Metrics view cycles all time / 24h /
  7d / 30d, applied to Tool calls and success rates (`ListToolMetrics
  {since}`), Runs (`RunMetrics.since`), model outcomes and token use
  (`ListModelStats {since}`) and built-in calls (`ListBuiltinStats
  {since}`). Model outcomes in a range come from the `model_events` log
  (migration v38), so ranges only cover history from that version on;
  "all time" still uses the running totals.
- **Token and cost chart.** `UsageByDay {since, by}` sums provider token
  use (prompt, completion, cached) per UTC day, or with `by` = "model",
  "role" or "session" per that key over the range, priced with
  `provider.prices` (no cost when a model in the bucket has no price). The
  TUI Metrics view charts it for the selected range: a bar per day (or key)
  split into cached, fresh prompt and completion tokens, with totals and
  cost; `b` cycles the breakdown.
- **Reviewing a Proposal.** `ProposalDiff {id}` returns what it changes:
  the learned-guidance part of the system prompt, or the memory notes,
  as diff lines without and with it (an applied one shows what it added).
  In the TUI Proposals view, Enter opens that diff with the rationale and
  eval results (candidate vs baseline, runs, extra tokens). `a` applies
  and `r` rejects from there; in the list, `a`/`r` still decide directly.
- **Tools view (browser):** list with success-rate/latency summaries, detail
  per Tool — descriptions, schemas, Capabilities, `memory_mb`, version
  history (author, timestamps, replaced-version stats), Invocation tallies,
  optimiser history, `needs attention` flags. Actions: rollback, restore
  (retired), purge (confirmed, user-only), export/import, audit log view.
  Tool JS is editable in the TUI (v2): `E` in the Tools view opens the
  current version's code; Ctrl+S saves it as a new immutable version
  (author `user`, same schemas/Capabilities) through the usual syntax +
  smoke gate.
  Ctrl+D reviews the edit first: the diff against the current version and
  the gate's verdict ("passes the syntax and smoke checks", or which check
  fails and why, with an inconclusive smoke result noted). It comes from
  `PreviewToolCode {id, code}`, which runs the same gate and saves nothing.
  From the review, Ctrl+S saves, and any other key returns to editing.
  A standalone **Metrics view** (v2, `/metrics`) summarises every Tool's
  invocations and success rate, with the selected Tool's detail.
- **Bulk actions.** In the TUI Sessions view, Space marks rows (✓, with
  the count in the panel title) and Esc clears the marks. With marks:
  `a` archives them (or restores them if all are archived), `e` exports
  each, `t` asks for tags to add, and `d` asks you to type `delete`
  before deleting them. The web sidebar has a checkbox per row and a bulk
  bar (Archive, Export, Tag, Delete with the same typed confirmation,
  Clear).
- **Undo delete.** Deleting a Session is a soft delete for
  `daemon.delete_undo_secs` (default 30 s), restored with `RestoreSession`.
  The TUI says "deleted session X · u (or /undo) restores it for 30s",
  and `u` in the Sessions view undoes it. The web delete toast has an
  Undo button.
- **Tags.** `SetSessionTags {id, tags}` (`sessions.tags`, a JSON array,
  migration v35) groups Sessions with free-form tags, normalised to
  lowercase without `#`, unique and sorted.
  - TUI: `/tag NAME…` adds, `/untag NAME…` removes, and `/tag` lists the
    current Session's tags and all tags. Rows show `#tags`, and the
    Sessions `/` filter matches them; `#work` matches that tag exactly.
  - Web: tag chips above the list filter it, and the Session menu has
    Tags….
- **Archived Sessions.** `SetSessionArchived {id, archived}`
  (`sessions.archived`, migration v34) tidies the list without losing
  history. `ListSessions` still returns archived Sessions, last and
  flagged; clients hide them by default, and search still finds them.
  - TUI: `a` archives or restores the selected Session, and the `v`
    quick filter cycles all → running → archived.
  - Web: a collapsible "Archived (N)" group and Archive/Unarchive in the
    Session menu.
  - CLI: `formless sessions list [--archived]`, `formless sessions
    archive|unarchive ID` (an id prefix is enough).
- **Pinned Sessions.** `SetSessionPinned {id, pinned}` (`sessions.pinned`,
  migration v33) keeps a Session at the top: the daemon lists pinned ones
  first, and every TUI sort keeps them first. Toggle it with `P` in the
  Sessions view or `/pin` (the current Session) in the TUI, or Pin/Unpin
  in the web Session menu. Pinned rows show ★. Pinning doesn't change
  the last-activity time.
- **Sessions list rows.** Each row shows a state icon (⚠ waiting on an
  approval, ? on a question, ◐ running, ⏸ paused), the name, the last
  activity, the message count, the model, the working directory, and the
  first line of the last user message. `ListSessions` returns
  `message_count`, `preview` and `last_run_status` for this. The web
  sidebar shows the preview under the name.
- **Filtering lists.** In the TUI's Tools, Sessions and Proposals
  views, `/` types a fuzzy filter: on name and description for Tools,
  name for Sessions, and content, rationale and kind for Proposals. Enter
  keeps it and Esc clears it. `s` cycles the sort and `v` the quick
  filters:
  - Tools sort by name, success rate, last used or calls; show all,
    active, retired, failing (<80%) or needing attention.
  - Sessions sort by last activity, created or name; show all or running.
  - Proposals sort newest first or by score; show all, pending, applied
    or rejected.
  The panel title shows the match count and the active sort, filter and
  query (`Tools · 12/240 · sort: calls · failing · /csv`), and the
  selection follows the visible rows. The web Tools and Proposals views
  have the same filter box, sort and show menus; web Sessions use the
  sidebar search.
  The web Tools table headers (Tool, Ver, Calls, Success, Last used)
  sort by that column on click or Enter; a second click reverses it. Each
  column's natural order is names A→Z and numbers or dates biggest first.
  The headers carry `aria-sort`, and the Sort menu follows.
- **Settings view:** edits file-backed config (§13) and per-Session
  model/effort. Safely hot-reloadable values apply immediately; data-dir /
  transport-class changes state “restart required”. Includes the full-reset
  action (§15).
  - No settable key needs a daemon restart: the daemon re-reads the file,
    and running TUIs reload their `tui.*` display settings on
    `SettingsChanged` (theme, colours, line numbers, images, panes,
    timestamps, hyperlinks).
  - Only `config::ON_TUI_START` (`tui.on_start`, `tui.screen_reader`)
    apply when the TUI next starts. `GetSettings` lists them as
    `on_tui_start`, and `SetSetting` answers `applies: "now" |
    "on_tui_start"`. The web marks those rows and, after saving one, says
    to restart any running TUI.
  - The web Settings view has a filter box. Rows (and table rows) whose
    label, key, value or description contain every typed word stay, with
    the words marked in the label. Sections left empty hide unless their
    title matches.
  In the TUI the rows are selectable (↑/↓ or j/k, Enter or Space):
  - booleans toggle and enums cycle (`reasoning_effort`, `api_style`,
    `tool_mode`; the last two are settable and validated against their
    allowed values);
  - `model` opens the model picker for the global default;
  - numbers edit inline and are checked before saving, with errors shown
    under the row;
  - read-only rows (endpoint, auth, relay, paths) say where they change.
  Changes go through `SetSetting`, so they are audited and other clients
  get `SettingsChanged`. A daemon refusal also shows under the row.
  Providers and Roles sections list them with add/edit forms (the API key
  is masked and goes to the keychain through `UpsertProvider`) and `d`
  twice to remove. "config.toml" opens the file in `$VISUAL`/`$EDITOR`.

### 10.5 Input and keys

- Enter: send / Steer while running. Queue: Ctrl+Enter where the
  terminal supports the kitty keyboard protocol (enabled when available;
  most terminals otherwise send the same byte for Enter and Ctrl+Enter),
  and Ctrl+J or Alt+Enter everywhere, tmux and Terminal.app included. The
  footer shows the binding that works in the current terminal.
- Copying: `/copy` copies the last answer and `/copy code [n]` its n-th
  code block (default: the last one). The text goes to the clipboard
  through OSC 52, which works over SSH and inside tmux with
  `allow-passthrough`, and also through the OS clipboard when the terminal
  is local. Mouse capture (for wheel scrolling) stops the terminal's own
  text selection, so `/mouse` toggles it; holding Shift (Option in
  iTerm2, Fn in Terminal.app) while dragging also bypasses it.
  `Esc`: interrupt Run. `Ctrl+C`: clear the typed message; twice within 2 s: detach client. `Tab`/`Shift+Tab` (and slash commands)
  switch views. Mouse supported; **every** action is keyboard-reachable.
  Slash commands (mirror the views): `/new`, `/sessions`, `/tools`,
  `/model`, `/settings`, `/stop`, `/resend`, `/resume`, `/abandon`, `/approve`,
  `/thinking`, `/help`
  (v2 adds `/fork`, `/search`, `/edit`, `/metrics`, `/proposals`,
  `/propose`, `/panes`; later `/retry [--model X]`, `/limit N|off`,
  `/good`, `/bad`, `/unrate`). Plain text is always chat.
- **Retry.** `/retry` re-runs your last message without retyping it
  (`RetryRun`); `/retry --model X` runs it on X for that Run only. The
  answer being replaced is kept first in a fork of the Session named
  "… · original answer", so you can switch back from `/sessions`. The web
  shows Retry and "Retry with…" under the last answer, and `/edit` still
  puts the message in the input to change it before re-running.
- **Session switcher.** Ctrl+K (⌘/Ctrl+K on the web) opens a fuzzy
  switcher over Session names, tags and last messages from any view,
  newest first until you type. Enter opens the Session; Ctrl+N (Ctrl/⌘+
  Enter on the web) starts a new Session named after the typed text.
- **Command palette.** Typing `/` at the start of the input opens a popup
  of commands with one-line descriptions, filtered fuzzily (exact prefixes
  first). ↑/↓ select, Tab completes, Enter runs. A command that still
  needs an argument completes to `/name ` and shows its syntax instead of
  running. Esc hides the popup until the input changes. Arguments
  complete where the client has the data, for now Tool names for
  `/revoke`. `/help` (or `?` on an empty input) opens a scrollable
  overlay grouped by area (Session, Run control, Tools, Settings,
  Integrations, App) followed by the keys. An unknown command suggests
  the closest one ("did you mean /sessions?"). In each client one command
  table (name, arguments, description, group) drives the palette, the
  help and "did you mean". The web client also dispatches from that
  table, and a TUI test checks that the dispatcher handles every entry.
- Terminal colours respected, `NO_COLOR` honoured. Narrow terminals remain
  usable (single-column-first design).
- Multi-pane layout (v2): `/panes` toggles a persistent dashboard —
  Sessions and Tools on the left, chat in the centre, Metrics and status
  on the right — saved as `tui.multi_pane` and restored on start. It is
  used only when the terminal is at least 110 columns wide; narrower
  terminals fall back to the single-column chat automatically.

- **Web shortcuts.** The web client mirrors the TUI where a browser
  allows. Tab stays focus navigation, so views switch with `1`–`5` and
  `[` / `]`.
  - `j`/`k` (or ↓/↑) move through Sessions (Chat) or Tools (Tools).
  - `/` focuses the message box and `n` starts a new Session.
  - On the selected Tool: `x` retire/restore, `b` roll back one version,
    `e` export, `o` optimise, `p` purge (typed confirmation).
  - `?` shows a help overlay built from the same table.
  Shortcuts never fire while typing or while a dialog is open. Approvals
  keep `y`/`o`/`n`.

- **Quitting while Runs work (TUI).** `/quit` with a Run active in any
  Session asks first: "2 Runs are still working in the daemon. They'll
  keep going if you quit." `q` (or Enter) quits, `s` stops them and quits,
  and Esc cancels. With nothing running it quits at once.
- **Key hint bar (TUI).** The right side of the status line shows the
  current view's keys in one format (`Enter open · n new · f fork · d
  delete`). They come from a per-view key table with an importance rank,
  and narrow terminals show only the most important hints that fit.
  The table replaces the hand-written panel footers.
- **List keys (TUI).** Every list view (Sessions, Tools, Metrics,
  Proposals) takes ↑/↓ and j/k, g/G or Home/End for the first/last row,
  and Ctrl+D/Ctrl+U for half a page. List views take no typed input, so
  these don't conflict. In Proposals, `n` now generates a new proposal
  (it was `g`).
- **Mouse (TUI).** While mouse capture is on: clicking a tab switches
  views, clicking a row in the Sessions, Tools or Proposals list selects
  it and a double-click opens it (as Enter), and clicking a tool call's
  header in the chat expands or collapses it. Each frame records its
  clickable regions, list rows from the widgets' own scroll offsets.

- **Plain mode for screen readers.** `formless --plain` (or
  `tui.screen_reader = true`) runs a line-oriented client instead of the
  full-screen TUI: no alternate screen, redraws, spinners or box drawing.
  - Each event prints once, as a sentence, in order: the streamed answer
    after "Formless:", "Tool started: name.", "Run paused: why. Next: what
    to do.", and notes.
  - An approval becomes a prompt that lists each capability with its
    plain-language meaning; the next line answers it (y, o or n).
    Questions list numbered options and are answered the same way, one at
    a time.
  - Commands: `/new`, `/sessions`, `/switch N`, `/stop`, `/help`, `/quit`.
    Anything else is a message (a steer while a Run is active). It opens
    the most recent Session.
  - `formless chat [--session ID | --new]` is the same line-mode client
    as its own command, a REPL between the full-screen TUI and one-shot
    `run`, for dumb terminals and SSH sessions with poor TERM support. It
    opens the given Session, a new one, or the most recent one.

### 10.6 Live feedback (v2)

- **What a Run did to the registry:** before `RunFinished`, a Run that
  created, updated or retired Tools sends `RunToolChanges` with one entry
  per Tool: its id, name, the version the Run left it at, and the change.
  Clients show it as a summary, for example "Tools this Run: created
  search_repo v1, updated fetch_page v3":
  - **TUI:** a note, and `/tool NAME` opens that Tool's detail in the
    Tools view.
  - **Plain mode:** a line.
  - **Web:** a row of chips above the composer. Each chip opens the Tool,
    and the row clears when the next Run starts.
- **Live call tree:** `ToolCallStarted.depth` gives a call's `tools.call`
  nesting (0 for the agent's own calls). The TUI draws running calls as a
  tree:
  - nested calls are indented under their caller (└)
  - sub-agents appear as nodes under the Tool that started them, showing
    their current step and the Tools they call next
  - every node has its status (spinner, ✓, ✗) and elapsed time
  - a subtree that finished successfully folds to one line (`+N nested`);
    running or failed subtrees stay open.
- **Usage and cost:**
  - After every model step the daemon broadcasts `RunUsage`: steps,
    prompt, completion and cached tokens, and the estimated cost.
  - The TUI status line and the web status bar show it for the current
    Session, for example `3 steps · 12.3k in / 1.2k out · 40% cached ·
    $0.021`.
  - `SessionUsage` returns the last Run and Session totals: TUI and web
    `/usage`, and the web status-bar tooltip.
  - Cost is what the provider billed when it reports it (OpenRouter's
    `usage.cost`, stored per request), else it comes from
    `provider.prices` of the provider that served the request (USD per
    million tokens, per model: `input`, `output`, optional `cached_input`
    and `cache_write_input`). Cache reads and writes are parts of the
    prompt, each charged once at its own rate. A price is found by exact
    id or with the aggregator prefix dropped on either side
    (`anthropic/claude-x` ↔ `claude-x`). It is shown only when every
    request is reported or priced; otherwise the usage line names the
    unpriced models. `formless prices import [--provider NAME] [--all]
    [--dry-run]` copies the endpoint's own `/models` prices (OpenRouter
    publishes them) for the models in use, dated, and `doctor` flags
    imported prices older than 90 days. Usage rows record their provider,
    so `UsageByDay {by: "provider"}` works; rows stay when their Session
    is deleted (spend history).
- **Colour:** roles, statuses, list selections, borders, and approval
  prompts are colour-coded; the chat has a friendly empty state and
  `/help` lists the shortcuts. `NO_COLOR` disables all colour.
- **Real-time thinking:** when the endpoint streams reasoning deltas
  (`reasoning_content`, or `reasoning` on some endpoints) the daemon
  forwards them as `ReasoningDelta` events and the TUI shows them live in
  a dimmed, separate "Thinking" block (`/thinking` toggles it). The
  block has a fixed height: a header ("Thinking · N lines") plus the
  latest 4 rendered rows, wrapped by display width and padded while
  shorter. The assistant header and activity line below it therefore stay
  put while reasoning streams, and stay put when the answer starts
  streaming below it. Reasoning
  is never fabricated when the endpoint emits none, never shown as the
  assistant's answer, and never persisted as message content; on
  tool-call turns it is kept only as provider metadata so thinking-mode
  endpoints that require it echoed back keep working.
- **Agent activity:** while a Run is active the status line always shows
  what the agent is doing — an animated spinner, the current step
  (waiting for model, thinking, responding, running a named Tool,
  awaiting approval), and elapsed time in that step — so silent waits
  never look like a hang.
- **Visible tool calls:** every Tool call (including `create_tool`) is
  shown inline in the chat as it happens — Tool name/version, input,
  running status with elapsed time, then its result or error — and stays
  in the transcript after the Run (persisted assistant/tool messages).
- **Scrolling:** the chat scrolls with the mouse wheel, PageUp/PageDown,
  and arrow keys; live output auto-follows only at the bottom, and
  End/PageDown back to the bottom resumes following.
- **Remembered startup:** relaunching restores the saved base URL, model,
  reasoning effort, and API key, so the wizard is skipped. If the key
  cannot be loaded, the wizard resumes pre-filled from the saved config.
  The key is kept in the OS keychain and a `0600` file so it survives
  rebuilds of an unsigned dev binary (whose keychain ACL would otherwise
  orphan the entry).

- **Toasts (TUI).** Status notes ("approved X once", "saved …", errors)
  appear as toasts in the top-right corner of the chat instead of
  crowding the status line. They fade after 6 s, up to 4 are shown, and
  a burst keeps only its newest. Problem notes (failed, denied, error…)
  are red and stay until Esc. `/notes` opens every note so far in the
  pager.
- **Context fill.** The status line shows `ctx N%`: the latest prompt's
  token count over the model's context size (`model_context_tokens`,
  else `context_tokens`). It's amber above 70% and red above 90%, so
  compaction (§11) isn't a surprise. `RunUsage` carries `context_used`
  and `context_tokens` after each step, and `ContextUsage {session_id}`
  gives the same for a Session on demand, falling back to an estimate
  before any prompt is measured. `/context` (or clicking the segment on
  the web) adds an estimated breakdown: system prompt, Tool schemas,
  history, and the number of attachments.
- **Notifications.** While the TUI's terminal isn't focused (focus
  events) or the web tab is hidden, Formless tells you when a Run
  finishes (if it took at least `tui.notify_min_secs`, default 20), is
  paused by a provider error, or stalls, and when an approval or
  `ask_user` question is waiting. `tui.notify`:
  - `bell` rings the terminal bell;
  - `desktop` (the default) also sends the terminal's notification: OSC 9
    for iTerm2, WezTerm, Ghostty and Windows Terminal, OSC 99 for kitty
    and OSC 777 otherwise, wrapped for tmux passthrough. The web client
    uses the browser Notification API, asked for on the first message
    you send;
  - `off` does nothing.
  The web tab title also counts unseen alerts (`(2) Formless`) until you
  come back. Interrupted Runs never notify.

### 10.8 Language

- UI strings live in catalogs keyed by the English text: the TUI's
  `i18n.rs` (`tr`) and the web's `I18N` table in `app.js` (`tr`). A
  string is translated by wrapping it and adding a row; anything without
  a row shows in English, so strings move into the catalog gradually.
  Thai (`th`) is the first language: the status bar, activity labels,
  command groups, the most-used command descriptions, help, and the
  plan, queue, recap, step-mode and paused-Run blocks; on the web also
  the composer, its buttons and the run banners.
- `tui.language` (`auto`, `en`, `th`) chooses it; `auto` follows the
  system locale (`LC_ALL`, `LC_MESSAGES`, `LANG`) in the TUI and the
  browser's language on the web. Message times use the language's day
  and month names (`3 มี.ค. 14:02`), the web formats dates and numbers
  with that locale, and token counts are digit-grouped.
- Agent answers are not translated; they are in whatever language the
  conversation is in.

### 10.9 Guides

- The user guides live in `docs/`: a gallery of example tasks, the
  Capabilities in plain words, the main keys and every command, troubleshooting,
  and Docker. `/docs [topic]` (TUI and web) and `formless docs [topic]
  [--print]` open one (`gallery`, `capabilities`, `approvals`, `keys`,
  `troubleshooting`, `mcp`, `skills`, `jobs`, `spec`; a prefix works;
  none lists them). `formless doctor` prints the troubleshooting link, and
  `/help` points to `/docs`.
- `docs/keys.md` is generated from the same key and command tables `/help`
  shows (`formless docs --keys`); a test fails when it is stale, and when
  a web key it lists is gone from the web UI.

## 11. Compaction

- Trigger: Run/Session approaching the model's `context_tokens` (§4.2).
- **On demand:** `/compact [focus]` (TUI and web) sends
  `CompactSession {session_id, focus?}`. It summarises everything so far
  into the checkpoint through the same path as automatic compaction, and
  passes `focus` to the summariser as what to keep. It runs in the
  background and ends with the usual compaction notice (or a note if it
  fails). It's refused while a Run is active and when nothing is new
  since the last checkpoint.
- **What is summarised, and when it fails:** each message with its role,
  plus the agent's tool calls (name and arguments) and their results
  (each capped at 2,000 characters). The summariser is asked for set
  sections (Goal, Decisions, Changes, Facts, Open) in about 600 words.
  Input bigger than half the context is summarised in parts, in order,
  and the parts merged. A summary cut off at the output limit
  (`finish_reason: length`, an incomplete Responses reply, Anthropic
  `max_tokens`) or empty fails the compaction: no checkpoint is saved and
  the turns stay.
- Preference order: **server-side compaction if the endpoint explicitly
  supports it, otherwise daemon-side summarisation** (the common path) using
  the Session model or an optional separate compaction model. Server-side is
  opt-in via `compaction.server_side`; the daemon then calls
  `POST {base_url}/compact` with `{model, input}` and accepts a
  `{summary}`/`{output}`/`{content}` response, falling back to daemon-side
  summarisation (with a visible note) if that call fails. Either way, the
  output is the same persisted checkpoint format.
- Contents: older turns and tool results are summarised into the checkpoint;
  the system prompt, recent turns, pending handles, and artifact references
  are preserved verbatim. Large outputs are already artifacts (§9.6);
  compaction summarises previews + references, never the only copy.
- Cache-friendly: the cut is always at a turn boundary (just before a user
  message, near the middle of the live transcript), so a tool call is never
  separated from its result; and each compaction summarises only the newly
  compacted span and **appends** that block to the previous summary, which
  stays byte-identical at the front of the system message. Only when the
  accumulated summary exceeds a quarter of the context is it condensed as
  a whole.
- Compaction is **visible** (notice in chat; checkpoint shown in Session
  detail) — never silent — and persisted in SQLite so resumed Sessions
  continue exactly.

## 12. CLI

- `formless` (no args) launches the TUI (auto-starting the daemon).
- CLI lists (`tools`, `sessions`, `providers`, `roles`, `jobs`, `mcp`,
  `skills`, `prompts`) share one table renderer: aligned columns with a
  dim header, secondary fields dimmed, status in colour, and the widest
  column cut with "…" to fit the terminal. Colour and fitting happen only
  when stdout is a terminal, and `NO_COLOR` turns colour off. Piped output
  is plain aligned text.
- Every CLI `list`/`status` subcommand takes `--json` and prints one JSON
  value (the daemon's data) instead of a table: `providers`, `roles`,
  `jobs`, `memory`, `mcp`, `skills`, `tools`, `prompts` and `sessions list`,
  plus `runs`, `approvals list`, `profiles list`, `relay status`,
  `security status`, `auth status`, `naming status` and `daemon status`
  (a test walks the command tree, so a new `list`/`status` without it
  fails). `tools show` and `sessions show` take it too. Without `--json`
  each prints a short human summary. `auth status --json` never includes
  the token. `relay status --json`
  leaves out the browser link (it holds the secret) and says `paired`.
  `daemon status --json` prints `{"running": false}` when no daemon runs.
- `formless daemon restart` waits for the running Runs to finish, or with
  `--force` interrupts them (waiting up to 15 s for them to stop). It then
  shuts the daemon down, waits until the old process has exited (socket
  closed, lock removed), and starts a fresh one. `daemon status` reports
  the daemon's `version` and `running_sessions` for this.
- `formless completions <bash|zsh|fish|powershell|elvish>` prints a
  completion script, generated from the command definitions (clap_complete),
  with no daemon or profile needed. `scripts/install.sh` writes bash, zsh
  and fish completions under `$PREFIX/share`, and the Homebrew formula
  installs them with `generate_completions_from_executable`.
- The man page, `formless.1`, is generated from the same definitions
  (clap_mangen) by a hidden `formless man`. Releases put it in each Unix
  tarball. `scripts/install.sh` installs it to `$PREFIX/share/man/man1`, and
  the Homebrew formula into `man1`.
- `formless doctor` ends with the problems it found, each with what to
  run next: a stale socket or lock, a daemon that runs but doesn't answer,
  secret files (`api_key`, `relay_secret`, `oauth_tokens*.json`,
  `config.toml`) others can read, a missing credential, an unconfigured
  provider, a failed probe, or a model without tool calling. `--fix`
  applies only the safe fixes:
  - remove the stale socket or lock (only when no formless process holds
    the lock);
  - stop a stuck daemon (SIGTERM, only if the lock's pid is a formless
    process) and start a new one;
  - make the secret files private (600).

  Then it checks again, re-running the provider probe. A daemon that
  simply isn't running isn't flagged, since it starts on demand.
- `formless doctor --json` prints the report as one JSON object: version,
  OS and arch, profile and dirs, provider (base URL, model, API style, tool
  mode, auth mode, whether a credential is present), relay, naming, whether
  the daemon is running, and the problems (with next steps and whether
  they're fixable).
- `formless bug-report [-o FILE]` writes a Markdown file with versions, OS,
  that JSON, and the last 200 daemon log lines, for attaching to an issue.
- Both are redacted: bearer tokens, `key`/`token`/`secret`/`password`
  values, `#k=`/`#local=` fragments, URL credentials, `sk-`/`ghp_`-style
  keys, long opaque tokens, and the home path (shown as `~`). The file says
  to read it before attaching it.
- `formless logs [-f] [--level warn] [--since 10m] [-n 200]` shows the
  daemon log: rotated files oldest first, then `daemon.log`. Entries are
  grouped (continuation lines stay with their entry), filtered by level
  (that level and worse) and age, and show the last `-n` (0 = all). `-f`
  keeps printing appended entries and follows rotation. The daemon logs
  `info` and above by default (other commands only errors), `RUST_LOG`
  overrides it, and colour codes are only used on a terminal, never in the
  file.
- `formless config`:
  - `path` prints the config file.
  - `get KEY` prints the effective value (after `FORMLESS_*` overrides);
    an unknown key exits 2.
  - `set KEY VALUE` goes through `SetSetting`, so it's validated against
    the allow-list, audited and hot. VALUE is JSON when it parses (`5`,
    `true`, `["a"]`), else text.
  - `edit` opens a copy in `$VISUAL`/`$EDITOR` (run through `sh -c`, so
    arguments and quotes work) and replaces config.toml only when the copy
    parses. Otherwise it shows the error and, on a terminal, offers to edit
    again; the file is never left half-written.
- `formless sessions` manages Sessions with the same protocol requests as
  the TUI and web: `list [--archived] [--json]`, `show ID [--json]`, `rename
  ID NAME`, `rm ID` (restorable for `daemon.delete_undo_secs`; it prints the
  restore command), `restore ID`, `archive`/`unarchive ID`, `export ID
  [--format json|md|html] [-o FILE]`, `import FILE`, and `search QUERY
  [--limit N] [--json]`. IDs may be a unique prefix (`restore` needs the
  full id).
- `formless run "task"` — headless Run (§5 semantics) in an ephemeral
  Session, or `--session <id|name>` to continue an existing one.
  `--continue` (`-c`, same as `--session last`) continues the Session the
  previous `formless run` used, recorded per profile in
  `<data dir>/last_run_session`. If there is none, or it was deleted, that
  is a usage error (exit 2). When stdin
  isn't a terminal, `formless run -` takes the task from stdin, and with a
  task given, non-empty piped stdin is attached to the message as
  `stdin.txt` (`git diff | formless run "review this"`); `-` with nothing
  piped is a usage error (exit 2). `--model`, `--effort` (low, medium,
  high) and `--role NAME` (as `role:NAME`; a missing Role is a usage error,
  and `--role` with `--model` is refused) set the Run's model. On the
  ephemeral Session they simply apply; with `--session` they apply to that
  Run, and the Session's own model and effort are restored afterwards
  (`--effort` alone keeps its model). `--output text|final|json|jsonl-events`
  picks what is printed:
  - `text` (default): the answer streams, progress on stderr;
  - `final`: only the last answer, on stdout when the Run ends, with
    progress on stderr only when it's a terminal;
  - `json`: one `{session_id, status, answer, error}` object at the end;
  - `jsonl-events`: every event, one per line; `--json` is the same.

  `--cwd DIR` sets the Run's working directory (§5.1). A new headless
  Session uses `--cwd`, or else the directory `formless run` was started
  in. With `--session` the Session keeps its own unless `--cwd` is given;
  then `--cwd` applies to that Run and the Session's own is restored
  afterwards. A missing directory is a usage error (exit 2).
  `--timeout DURATION` (`90`, `90s`, `10m`, `1h30m`) interrupts the Run
  when it expires. The command says "timed out after …", waits up to 10 s
  for the Run to stop, and exits 124 (JSON status `timed_out`).
  When progress lines are shown, the Run ends with one summary line on
  stderr, with the same figures as the TUI status line: outcome, duration,
  steps, Tools created and used, tokens in and out, and cost when priced.
  For example: `completed in 1m15s · 3 steps · Tools: 1 created, 1 used ·
  13.0k tokens (12.4k in, 610 out) · $0.0123`.
  `--quiet` drops the progress lines and the summary (errors still go to
  stderr), and exit codes are unchanged. `--attach FILE` (repeatable) sends
  files and images with the task through `AttachFile`, with the same 20 MB
  limit and type and vision handling as `/attach`. Every file is read and
  checked first; a missing or too-large one is a usage error (exit 2)
  before anything runs. Streaming
  human-readable output on stdout; daemon logs on stderr; `--json` emits one
  JSON event per line (the §9.3 event stream). With approvals on
  (`security.secure`, §10.3), **approvals fail closed** in headless mode:
  an unapproved sensitive Invocation fails with a clear error
  (approve first in the TUI), unless `--yes` is passed (auto-approve for
  that Run only: each prompt is answered *once*, nothing is stored). Exit codes: `0` answered, `1` Run errored/paused, `2`
  usage/config error, `130` interrupted.
- `formless daemon start|stop|status` — daemon management (same lifecycle
  rules as §9.1).
- `formless web [--port N] [--no-open] [--stop]` — the opt-in **local web
  UI**. The daemon serves the relay web app, embedded in the binary, on
  `127.0.0.1` only (`StartWebUi`/`StopWebUi`), and the protocol runs over
  a same-origin WebSocket bridged to the local socket or pipe, without the
  relay crypto layer. It stops when the daemon exits.
  - **Login:** the printed link carries a one-time random token in its
    URL fragment, which the page exchanges for an `HttpOnly`,
    `SameSite=Strict` session cookie. The link works once.
  - **Request checks:** every request must name `127.0.0.1:<port>` or
    `localhost:<port>` in `Host` (DNS-rebinding protection). The login
    and the WebSocket must also come from that `Origin`, and the
    WebSocket needs the cookie.
  - Responses carry the same CSP as the hosted relay.
  - **Reconnecting:** when a connected page loses the daemon, it keeps
    retrying with backoff. The connection pill counts down ("daemon
    offline · retry in Ns") and a tap retries at once. After 3 failed
    tries it notes that a restarted daemon needs a new `formless web`
    link, because session cookies don't survive a restart. Pasting a new
    link into an open tab signs in with it. Relay mode uses the same
    visible countdown.
- `formless tools list|show|run|export|import …` — registry inspection,
  direct runs and transfer.
  - `show NAME [--json]` prints the description, Capabilities, memory,
    stats, attention reasons, versions and schemas.
  - `run NAME --input JSON [--yes] [--session ID]` runs the Tool through
    `RunTool`. Approval requests are answered by the command: once with
    `--yes`, otherwise denied (fail closed), and nothing is remembered.
    Without `--session` it runs in a new Session that is archived
    afterwards. It prints the result and exits 0, or 1 when the call fails
    or is denied (2 for a bad `--input`).
  - `retire NAME`, `restore NAME`, and `rollback NAME [--to N]` (default:
    the version before the current one) mirror the Tools view.
  - `purge NAME` deletes a Tool and its history for good. On a terminal it
    asks for the Tool's name; without one, `--force` is required (exit 2
    otherwise), matching the web's typed confirmation.
  (same trust rules as §7.8).
- `formless doctor` — diagnostics: config validity, data dir, keychain
  reachability, endpoint connectivity + tool-calling probe result.
- `formless reset` — full wipe (§15); typed confirmation in TUI contexts,
  `--force` required headless.
- **Destructive commands** (`reset`, `uninstall`, `profiles rm`, `tools
  purge`, `tools retire` of a Tool others use) share one rule: on a
  terminal you type the confirmation word; anywhere else `--force` is
  required (`--yes` is an alias where it was the old spelling), so piped
  input never confirms. Refusing is a usage error.
- **Exit codes**, for every command: 0 success; 1 a failure (the daemon,
  the provider, the Tool); 2 a usage mistake (an unknown id, a missing or
  conflicting flag, a bad value, malformed `--input`, a refused
  confirmation), checked before a daemon is started where the client can
  tell; 124 `run --timeout`; 130 interrupted. Errors print as `error: …`
  on stderr. Flags that would silently override each other are refused by
  the parser (`run --json --output`, `export NAME --all`, `jobs add --cron
  --at`, `web --stop --port`, `approvals revoke … --all`), and the TUI's
  `--plain`/`-c`/`--new` only go with starting the TUI. `relay enable`
  keeps the configured `relay.url` unless `--url` is given. `tools run`
  exits from the daemon's `ok`, not the result's wording. `formless man`
  documents every subcommand's options, two levels deep.
- The CLI is a client of the same protocol; **no behaviour lives only in
  the CLI**.

### Relay and Web frontend (v2)

`GetSession` includes the selected Session's `running` and global `pending_events`:
all outstanding approval/question events across Sessions and promotion prompts.
Resolving or cancelling a prompt removes it from subsequent snapshots. Local
and Relay Web reconnects discard stale interactive/streaming state and seed the
current Session from this snapshot; events received during the fetch are replayed
as state only, without re-running notifications or resetting live streaming text,
so the snapshot cannot revive an answered prompt or erase a new one.
`PromotionApprovalResolved` closes the matching prompt in every client when any
client resolves it, including while a browser's snapshot request is in flight.
Concurrent `ask_user` calls in the same Session, including nested sub-agents,
wait behind a FIFO gate until the preceding question closes; cancellation also
cancels queued waits. Web clients additionally queue question events across
Sessions. Closing a queued question cannot dismiss an unrelated settings dialog.

Persisted assistant `meta.stop_reason` and `meta.truncated` markers are visible
in Web transcript history, including after reopening a Session: output-token and
context-window limits, model pauses, refusals and content filters get explanatory
notes; unknown stop reasons are shown explicitly. Existing provider placeholder
notes do not get a duplicate label, and normal completions get no stop label.

Web approval and promotion decisions remain pending locally until the daemon
confirms resolution; failures report that the decision was not delivered and
remain answerable. Recap and rating controls are rebuilt from current Run state
outside the cached message body, so live completion and rating changes appear
immediately and only the current last answer has controls. Markdown formatting
is limited to 32 nesting levels, 4096 characters per inline parse, and 128 KiB
per formatted message. Content beyond these limits is displayed verbatim as
plain text, preserving access to persisted messages without recursive parsing.

The Web client treats each user action as belonging to the Session selected
when it starts: attachment uploads, mentioned paths, send mode and completion
cleanup keep that Session id across asynchronous work. Newly mentioned files
still require review before sending; if the user switched Sessions while they
attached, their original message is retained for review on returning. Dialogs
queue in arrival order, preserving text already entered in an active dialog.
Malformed URL-fragment values cannot prevent pairing or link handling: each pair
is decoded independently, valid pairs remain available, and malformed links show
pairing guidance. Tool selection, search and audit loads discard superseded
responses; clearing search invalidates outstanding results. Opening a Session
clears the previous transcript immediately and merges live messages received
during its snapshot fetch, deduplicated by message id.

- `formless relay enable|disable|status` controls the hosted Relay: the
  daemon dials out to `relay.url` (default
  `wss://formless.athiwat.xyz`) and browsers meet it there — no inbound
  ports. The Relay (a Cloudflare Worker + Durable Object room in
  `relay/`) forwards only opaque AES-256-GCM ciphertext: the pairing
  secret (32 random bytes, keychain + `0600` file, never in config) is
  the key, and the room id is a truncated SHA-256 of it, so the Relay
  can neither read traffic nor derive the key. The daemon connects with
  `?mux=1`: the Relay gives every browser a client id, announces
  `client_open`/`client_close`, and routes `{cid, f}` frames, so each
  browser has its own daemon connection. Frames are version 2: each
  browser connection opens with a handshake (`{"v":2,"hello":<16 random
  bytes>}` from each side) and derives a session key with HKDF-SHA256
  over the pairing key, salted with both nonces. Each side proves it holds
  the key before anything is trusted: the daemon's hello carries
  `confirm` = HKDF(key, salt = both nonces, info `formless-relay/2
  confirm d2b`), which the browser checks before answering with its own
  `{"v":2,"confirm":…}` (info `… b2d`). Only then does the browser count
  as a subscriber or appear in the device list. The same hello again gets
  the same answer (no one-sided rekey); a different hello starts a
  pending handshake that replaces the live channel only once confirmed,
  and is dropped after 30 s. The browser shows "securing the connection…"
  until confirmed, asks again up to three times 8 s apart, then says it
  couldn't secure the connection, and ignores any hello once its channel
  is up. Every frame carries a
  per-direction `seq` bound into the AES-GCM associated data and must be
  exactly one more than the last, so the Relay cannot replay, reorder,
  withhold or reflect frames, nor reuse them in another connection. A
  frame that does not open is a link integrity failure: the daemon logs
  it and has the Relay close that browser with code 4002, and a browser
  that receives one closes with 4002 itself; either way the browser says
  so, handshakes again and reloads its state. The relay link itself is not a
  subscriber: with no browser attached, approvals fail closed and the
  daemon can idle-close. The browser URL carries
  the secret in the `#k=` fragment, which is never sent to a server.
  `--rotate` invalidates old links. The hosted Web frontend is a full
  client with TUI feature parity — Sessions (new, fork, rename, delete,
  search, import/export), streaming chat with thinking and Tool calls,
  send/steer/queue/stop and edit-and-rerun, approvals and promotions,
  Tools (detail, code editor, optimise, retire/restore, rollback, purge,
  import/export), Metrics, Proposals, and Settings (including Providers &
  Roles: list, add or edit named providers with a write-only key field,
  test a connection, and add, edit or delete Roles; and Harness changes:
  what the agent changed in its own setup, when and why, with before/after
  and Undo per entry; adding MCP servers (a command with environment, or
  a URL with headers, tested by listing its tools at once) and Skills (a
  path, zip or git URL on the daemon's machine, or a bundle uploaded from
  the browser); and New job: a task, a schedule from presets (every day,
  every weekday, every week at a time, hourly, once) or cron, previewed in
  words, and a target Session; a top-bar theme switch, Auto (the system's)
  / Light / Dark, remembered per browser) — over the
  same daemon protocol, decrypted in the browser. `GetSettings` returns a
  read-only configuration summary for remote clients and never includes
  `provider.extra_headers` or any other credential-bearing field. The
  page loads no inline script (`script-src 'self'`) and connects only
  to its own origin (`connect-src 'self'`; `dev.mjs` alone adds loopback
  `ws://`), because its URL fragment carries the key, and it removes the
  key from the address bar at startup, before connecting. The browser
  keeps only a non-extractable key and the room id: the decoded bytes are
  wiped, the pairing field is emptied once read, and the secret is kept
  in `sessionStorage` (to survive a reload) only when "Stay paired after
  reloading this tab" is ticked; a copied Session link carries the key
  only then. In the daemon the key and session keys are zeroized on drop. `relay.url` must be `wss://`,
  or `ws://` to this machine (`FORMLESS_RELAY_ALLOW_INSECURE=1` allows
  others, for development only): `relay enable` refuses anything else,
  `doctor` reports it and the daemon does not connect; a page served over
  plain HTTP from another machine refuses to take a pairing secret.
  **Reconnecting:** the daemon and the page back off exponentially (daemon
  to 30 s, page to 15 s) with full jitter, and reset the backoff only
  after a link has stayed up (daemon 60 s, page 30 s after the
  handshake), so a relay that accepts and drops at once is not hammered.
  **Teardown:** the Relay sends `client_close` / `daemon_offline` once
  per socket whether it closed or errored. Browsers send a `Ping` every
  minute; the daemon drops a browser it has heard nothing from for 5
  minutes (code 4002), so a vanished tab never stays an approval
  subscriber.
  **Pairing by QR code:** `formless relay enable` (and `formless relay
  status --qr`) print the browser link as a terminal QR code (error
  correction level M, a 4-module quiet zone drawn light), white on
  black so phones read it on any theme, and only on a terminal, never into
  a pipe or log. The TUI's `/relay` shows the same in the pager when the
  relay is on. Each warns that the code holds the pairing secret and
  points to `--rotate` if it leaks.
  **Devices:** the relay tells the daemon each browser's User-Agent on
  `client_open`. `ListRelayClients` lists the browsers on the link (cid,
  User-Agent, first and last seen), and `DisconnectRelayClient {cid}`
  makes the relay close that one (code 4001; 4002 means the channel broke
  and the browser reconnects). The web Settings → Relay
  devices and `formless relay status` show the list; the web offers
  Disconnect. A disconnected tab stops reconnecting and says why. It can
  rejoin with the pairing link; `formless relay enable --rotate` is what
  revokes access. Client settings data never includes credential keys,
  not even by name.
  **Session links:** the fragment may also carry `s=<session-id>`
  (`#k=…&s=…`, `#local=…&s=…`, or `#s=…` in a tab that's already
  connected), which opens that Session. The Session menu's "Copy link"
  builds one. On the relay it includes the pairing key, still only in the
  fragment, and the menu says so. Locally the login token is single-use,
  so the link is just `#s=…` and opens in a browser already signed in.
  The page is an installable app: a web manifest and icons, and a service
  worker (`/sw.js`) that fetches the shell network-first and falls back to
  the cached copy offline. It caches only the static shell (`/`, `app.js`,
  `app.css`, the manifest and icons), never `/auth`, `/ws` or `/relay/`,
  and URL fragments never reach it. Offline, the status pill says "offline
  · daemon unreachable" and the transcript says the daemon is offline. The
  CSP allows `manifest-src 'self'` and `worker-src 'self'` for this.
  **Notifications (Web Push, opt-in per device):**
  - Settings → This browser subscribes with the relay's VAPID key
    (`GET /push/key`) and sends `{"relay":"push_subscribe","sub"}` to the
    room in the clear; a subscription is not Session data. "Turn off"
    sends `push_unsubscribe`. The room keeps only real subscriptions: an
    `https:` endpoint on a browser push service (FCM, Mozilla, Apple,
    Windows; a self-hosted relay adds hosts with `PUSH_HOSTS`) with a
    65-byte `p256dh` and 16-byte `auth` key, so it never POSTs to a URL a
    client made up.
  - When an approval or `ask_user` question starts waiting, the daemon
    sends the relay `{"relay":"push"}` (at most every 30 s). The room
    sends each subscription a payload-free push signed with VAPID and
    drops subscriptions the push service reports gone (404/410).
  - The VAPID secrets are checked once: the JWK must be the P-256
    private half of `VAPID_PUBLIC_KEY` (and pass a sign/verify round
    trip) and `VAPID_SUBJECT` a `mailto:`/`https:` URL. A bad setup turns
    push off instead of failing requests. `/health` reports `push` as
    `off`, `ok` or `misconfigured` (with a reason, never key material),
    and a push service refusing our signature (400/401/403) shows there
    and in the log.
  - The service worker shows "An approval or a question is waiting for
    you", and clicking it focuses the app. No Session content reaches the
    relay or the push service.

## 13. Configuration (complete settings table)

- File: `config.toml` in the OS config dir; sections `[daemon] [provider]
  [optimiser] [metrics] [tools]`. Env vars `FORMLESS_*` override file values.
  Per-Session model/effort live in SQLite, not the file. Nothing configurable
  is a hidden constant: this table is exhaustive for v1.
- **Saving keeps your file (#610).** Every save (a setting, an MCP
  server, a migration) merges into the existing `config.toml`: changed
  values are replaced in place with their comments, new non-default keys
  are added, removed settings are deleted, and keys this build doesn't
  read (a newer build's, your own notes as keys) stay. Writes are atomic:
  a temporary file beside it, flushed to disk, renamed over it, so a crash
  leaves the old file or the new one.
- **Changing settings from a client:** clients never write the file.
  `SetSetting {key, value}` asks the daemon to set one dotted key. The
  daemon checks the key against an allow-list: provider model, effort,
  routing and timing, a few `daemon.*` keys, and the whole `tui`, `tools`,
  `metrics`, `optimiser`, `compaction`, `naming`, `skills` and `artifacts`
  sections. It validates the value through the config types, so a wrong
  type or a mistyped key is refused. It writes the file without
  environment overrides, audit-logs the change and broadcasts
  `SettingsChanged`. Changes are hot and apply from the next read.
  Credentials, `base_url`, `extra_headers`, auth, the relay, MCP servers and
  hooks are never settable this way, locally or over the relay. The TUI
  (`/panes`) and the web Settings view (Edit) use `SetSetting`.

This table is also the help text clients show. The daemon parses it at
build time (`config::setting_docs`) and serves `{key: {description,
default}}` as `GetSettings.docs`, taking defaults from the config types
when they have one. The web Settings rows and the TUI's selected row show
the meaning and default. A test fails when a settable key, one a
Settings view shows, or any key of the default config is missing here;
the only exceptions are file-only blocks it lists (`provider.auth`,
`provider.extra_headers`, `mcp.servers`, `hooks`, `providers`, `roles`).

| Key | Default | Meaning |
|---|---|---|
| `daemon.idle_grace_secs` | 60 | Idle-close grace after last Run + last client (§9.1) |
| `daemon.stall_warning_secs` | 300 | Silence before `possibly stalled` flag (§6.1) |
| `daemon.start_timeout_secs` | 60 | How long a client waits for an auto-started daemon (it fails early, with the log's last lines, if the daemon exits) |
| `daemon.log_max_bytes` / `log_keep` | 10 MB / 5 | Daemon log rotation (§9.4) |
| `daemon.keep_alive_for_jobs` | false | Stay up while scheduled jobs wait (§9.7) |
| `daemon.delete_undo_secs` | 30 | Undo window for Session deletes (§5.1) |
| `daemon.backups_keep` | 7 | Daily database backups kept in `<data>/backups` (`formless backup` / `restore`); 0 = no automatic backups (§9.6) |
| `artifacts.max_total_mb` / `max_age_days` | 1024 / 30 | Artifact retention (§9.6); 0 disables a limit. Attachments count too: a removed one shows as expired in its message (§5.6) |
| `provider.base_url` | — (wizard) | Verbatim endpoint base (§4.1) |
| `relay.enabled` | `false` | Dial out to the hosted Relay on daemon start (§12) |
| `relay.url` | `wss://formless.athiwat.xyz` | Relay websocket base URL |
| `provider.extra_headers` | {} | Free-form headers per request (§4.1) |
| `provider.model` / `reasoning_effort` | — (wizard) | Daemon-wide defaults (§4.2) |
| `provider.context_tokens` (per model) | 128000 | Compaction trigger basis (§4.2, §11) |
| `provider.model_context_tokens` | {} | Context size per model id, overriding the endpoint-reported size and `context_tokens` for that model (§4.2, §11) |
| `provider.max_retries` | 5 | Bounded backoff attempts (§4.4) |
| `provider.cache_control` | `auto` | Prompt-cache breakpoints (`cache_control`) on the system prompt and last stable message: `auto` for Claude models, `on`, `off`. Decided per model tried, so a fallback that isn't Claude gets plain content; applied to one-shot completions (compaction, `llm.complete`) too; a 400 naming `cache_control` resends once without them (§4.2) |
| `provider.cache_key` | `session` | Send a per-Session `prompt_cache_key` for better cache routing (`off` never sends it; dropped automatically if the endpoint rejects it) |
| `provider.max_backoff_secs` | 30 | Cap for exponential backoff with full jitter; a server's `Retry-After` / `x-ratelimit-reset-*` is honoured instead (up to 5 min) |
| `provider.tool_use_nudge` | true | Push models to build Tools: a GPT-family system-prompt addendum, and one nudge per step when an answer refuses for lack of access before using any Tool (§5.2) |
| `provider.stall_notice_secs` | 120 | Flag an open stream that sent no bytes for this long (`ProviderStalled`; `/resend` re-sends); 0 = never |
| `provider.first_byte_timeout_secs` | 0 | Auto re-send when no first byte arrives in time (up to `max_retries`); 0 = off |
| `provider.api_style` | `chat` | `chat`, `responses` or `anthropic` (native Messages API) (§4.1) |
| `provider.tool_mode` | `native` | `native` or prompt-based fallback (§4.1) |
| `provider.fallback_models` | [] | Failover models, in order (§4.1) |
| `provider.aliases` | {} | Short names for model ids (`fast = "…"`), accepted wherever a model is (§4.2) |
| `provider.extra_body` | {} | Vendor fields merged into every request body (Ollama `options`, vLLM `chat_template_kwargs`, OpenRouter `provider` routing); `model`, `messages`, `input`, `tools`, `stream` and `system` are never overridden, objects merge one level deep. The Ollama endpoint also gets `options.num_ctx` = the model's context size unless set here (§4.2) |
| `provider.query_params` | {} | Query parameters added to every request, e.g. Azure's `api-version` (§4.2) |
| `provider.api_key_header` | (Bearer) | Header that carries the API key instead of `Authorization: Bearer`, e.g. `api-key` (§4.2) |
| `provider.proxy` | "" | HTTP(S) proxy for the daemon's requests, from start; empty uses `HTTPS_PROXY` / `NO_PROXY` (§4.2) |
| `provider.routing_models` | [] | Candidates for `model="auto"` measured routing |
| `provider.routing_fast_models` / `routing_strong_models` | [] | Optional simple/complex subsets |
| `provider.routing_complex_chars` | 20000 | Context-chars threshold for complex tasks |
| `provider.vision` | (by model name) | Send attached images as image parts (§5.6) |
| `provider.image_max_px` | 1568 | Longest side, in pixels, of an attached image sent to a vision model; bigger ones are scaled down once per Session and the encoded image kept for later steps (0: never scale). An image that can't be sent (gone, unreadable, over 8 MB) leaves a marker for the model and a note for the user (§4.2) |
| `provider.image_detail` | auto | The `detail` asked of vision models for attached images: `auto`, `low` or `high`; images go to Chat Completions as `image_url` parts and to the Responses API as `input_image` parts (§4.2) |
| `provider.prices` | {} | USD per million tokens per model (`input`, `output`, `cached_input`, `cache_write_input`; `retrieved_at` when imported with `formless prices import`) for cost estimates and dollar limits (§10.6) |
| `tools.pending_threshold_secs` | 300 | Soft async handoff threshold (§6.2) |
| `tools.default_memory_mb` | 256 | Default per-Invocation heap (§7.4) |
| `tools.max_memory_mb` | 4096 | Daemon bound on declared memory (§7.4) |
| `tools.artifact_preview_chars` | 2000 | Context preview of artifact outputs (§9.6) |
| `tools.session_max_loaded` | 60 | Most Tools a Session keeps listed; at compaction the oldest loaded ones are dropped (own creations stay) |
| `tools.search` | `bm25` | `find_tools` ranking: BM25 over names, descriptions and input fields with stemming and synonyms, or `keyword` |
| `tools.inline_limit` | 40 | Tools listed in a Session's frozen first-step listing; others load via `find_tools` (§7.1) |
| `tools.max_agent_depth` | 1 | How deep `agents.run` sub-agents may nest (§7.6) |
| `tools.max_tool_depth` | 8 | How deep `tools.call` composition may nest (cycle guard, §7.6) |
| `tools.max_sleep_secs` | 60 | Longest single `sleep(ms)` in a Tool (§7.5) |
| `tools.max_random_bytes` | 1048576 | Largest `randomBytes(n)` (§7.5) |
| `tools.max_read_bytes` | 67108864 | Largest `fs.read` or `fetch` body (64 MB): larger fails as a resource error pointing to `fs.readChunks` / `fetchChunks` (§7.5) |
| `tools.max_process_output_bytes` | 16777216 | `process.run` keeps this much of stdout and of stderr (16 MB); the rest is read and dropped, and the result says `truncated: true` |
| `tools.max_log_bytes` | 1048576 | All `log` lines of one Invocation (1 MB); later lines are dropped after one marker line |
| `tools.max_emit_bytes` | 1073741824 | Everything one Invocation `emit`s to its artifact (1 GB) |
| `tools.find_tools_max` | 25 | Most results one `find_tools` call returns |
| `security.trusted_hosts` | [] | Hosts whose `net:` grants need no approval (`api.example.com`, `*.github.com`); config.toml only (§10.3) |
| `security.secure` | false | Ask before sensitive Tool calls; off runs them unasked (logged). `--secure` / `FORMLESS_SECURE` turn it on at daemon start (§10.3) |
| `security.secret_patterns` | [] | Extra secret shapes (regular expressions) blanked wherever Formless stores or quotes text, beside the built-in ones; config.toml only (§15) |
| `security.trusted_paths` | [] | Folders where writes need no approval; config.toml only (§10.3) |
| `security.trust_writes` | true | false keeps writes in trusted folders asking (§10.3) |
| `tools.approval_summaries` | true | Ask the model, once per Tool version, for a plain-language summary shown on approval prompts (§10.3) |
| `tools.subagent_max_turns` | 40 | Model turns a sub-agent may take (roles can override) (§4.6) |
| `tools.task_min_samples` / `task_quality_bar` / `task_explore_rate` | 5 / 0.8 / 0.1 | Per-task routing for `auto` roles (§4.6) |
| `tools.smoke_timeout_secs` | 10 | Creation smoke-test bound (§7.1) |
| `tools.replay_timeout_secs` | 60 | Optimiser replay bound per Invocation (§8.4) |
| `scripts.max_parallel` | 8 | Nested `tools.call` Invocations in flight per sandbox; the rest queue (§7.5, §7.12) |
| `scripts.max_source_kb` | 64 | Largest `run_script` source (§7.12) |
| `scripts.memory_mb` | 256 | `run_script` heap cap (§7.12) |
| `scripts.timeout_secs` | 600 | A script running longer is stopped; its nested Invocations finish on their own (§7.12) |
| `scripts.recurrence_threshold` | 3 | Distinct Sessions running one script before it is flagged "save as a Tool" (§7.12) |
| `metrics.success_window` | 100 | Rolling window for success rate (§8.3) |
| `metrics.trigger_min_invocations` | 20 | Minimum sample before trigger (§8.3) |
| `metrics.trigger_success_below` | 0.90 | Trigger threshold (§8.3) |
| `metrics.retention_per_tool` | 200 | Recorded Invocations kept per Tool (§15) |
| `optimiser.cooldown_secs` | 3600 | Per-Tool attempt cooldown (§8.3) |
| `optimiser.max_attempts_per_day` | 3 | Per-Tool daily cap (§8.3) |
| `optimiser.give_up_after_failures` | 3 | Consecutive failures → `needs attention` (§8.3) |
| `optimiser.promotion_margin_points` | 10 | Replay win required (§8.5) |
| `optimiser.model` / `reasoning_effort` | (session) | Optional separate optimiser model (§8.3) |
| `optimiser.token_budget` | (none) | Optional daemon optimisation budget (§8.3) |
| `optimiser.eval_repeats` | 3 | Runs per prompt-eval scenario per arm (§8.6) |
| `optimiser.scan_interval_secs` | 10 | How often triggers and the queue are checked (§8.3) |
| `optimiser.mcp_chain_min_runs` | 3 | Suggest a Tool for two raw MCP calls made back to back in this many Runs (0 = never) (§8.3) |
| `optimiser.mcp_chain_days` | 14 | Window for counting those Runs (§8.3) |
| `skills.auto_create` / `min_tool_calls` / `model` | true / 5 / (session) | Automatic skills (§7.10) |
| `skills.auto_retire_days` / `min_success_rate` / `min_runs_for_rate` | 30 / 0.5 / 5 | Auto-skill retirement (§7.10) |
| `[[hooks]]` | (none) | pre_tool / post_tool / run_end hooks (§9.8) |
| `compaction.model` | (session) | Optional separate compaction model (§11) |
| `compaction.server_side` | false | Prefer `POST {base_url}/compact` when supported (§11) |
| `compaction.trigger_fraction` | 0.8 | Fraction of the context window at which compaction starts (§11) |
| `memory.confirm_writes` | `never` | Ask before keeping a note the agent writes: `never`, `sensitive` (personal details) or `always` (§7.11) |
| `mcp.idle_secs` | 300 | Stop an MCP server process after this long without calls; it restarts on the next one (§7.9) |
| `tui.queue_key` | Ctrl+Enter | Kept for compatibility; queueing is Ctrl+Enter (kitty protocol), Ctrl+J or Alt+Enter (§10.5) |
| `tui.multi_pane` | false | Persistent multi-pane layout on wide terminals (§10.5) |
| `tui.timestamps` | true | Message times (`now`, `2m`, `3h`, `Tue 14:02`) on chat headers and a separator when the date changes; `/timestamps` toggles it (§10.2) |
| `tui.animations` | true | Spinners and other moving indicators in the TUI; false (or `REDUCED_MOTION` / `FORMLESS_REDUCED_MOTION` set to anything but empty, `0`, `false`, `no` or `off`) shows a static `●` and a steady cursor (§10.2) |
| `tui.language` | auto | UI language: `auto` (the system locale: `LC_ALL`, `LC_MESSAGES`, `LANG`), `en` or `th`; the web follows it, else the browser's language (§10.8) |
| `tui.hyperlinks` | auto | Clickable OSC 8 links for URLs in the TUI chat: `on`, `off`, or `auto` (on except Terminal.app and the Linux console) (§10.2) |
| `tui.theme` | auto | `dark` (pinned colours), `light` (deeper hues for white backgrounds), `high-contrast` (bright variants), or `auto` (light when `COLORFGBG` reports a light background, else when the terminal answers an OSC 11 background query within 150 ms; dark otherwise). Every text colour of `dark` and `light` has at least 4.5:1 contrast on its background; a `[tui.colors]` override below that is used but noted at start, and a named override goes through the theme like any other colour (§10.6) |
| `tui.colors` | {} | Per-role colour overrides: `user`, `assistant`, `tool`, `note`, `error`, `accent` → a name (`light-cyan`) or `#rrggbb` (§10.6) |
| `tui.code_line_numbers` | false | Dim line numbers in the chat's code blocks (§10.2) |
| `tui.inline_images` | auto | Image thumbnails in the chat on iTerm2, WezTerm, kitty and Ghostty; `off` keeps only the reference (§10.2) |
| `tui.pane_widths` | [22, 26] | Multi-pane side panes as a percentage of the width, [left, right]; 0 hides one; `/panes wider\|narrower\|left\|right\|reset` or Ctrl+Shift+←/→ (§10.2) |
| `tui.screen_reader` | false | Start the line-oriented plain mode instead of the full-screen TUI, as `formless --plain` does (§10.5) |
| `tui.on_start` | last | What the TUI opens on launch: `last`, `new` or `picker`; `formless -c` / `--new` override it (§10.1) |
| `tui.notify` | desktop | `off`, `bell`, or `desktop` (terminal or browser notification plus the bell) while you are elsewhere (§10.6) |
| `tui.notify_min_secs` | 20 | Runs shorter than this finish without a notification (§10.6) |
| `limits.daily_usd` / `run_usd` / `run_steps` | (none) | Opt-in spending limits: provider spend per UTC day across Sessions, spend of one Run, and model steps of one Run; a note at 80%, a pause at 100% (§5.2) |
| `naming.enabled` | true | Rename "Untitled" Sessions after their first Run |
| `naming.model` / `auto_download` / `timeout_secs` | qwen2.5-0.5b / true / 30 | Embedded naming model (empty: a heuristic title), fetched on first use, and how long one naming may take before the heuristic is used |

## 14. Security

- **Trust boundary:** the daemon serves only the owning local user (socket/
  pipe ACL). Any local process running as that user is trusted; nothing
  listens on the network (the opt-in web UI listens on 127.0.0.1 only,
  behind a one-time token, a session cookie and Host/Origin checks, §12). The v2 Relay is outbound-only and carries
  ciphertext keyed by the pairing secret (§12).
- **Sandbox:** QuickJS has zero ambient authority. Every privileged effect
  goes through a daemon host call checked against the Tool version's declared
  Capabilities + approved state. JS interrupt support makes user
  cancellation actually stop executing code.
- **Path containment:** `fs:*` scopes are canonicalised at approval and at
  each access; `..` and symlink escapes outside the granted roots are denied.
  A not-yet-existing path resolves through its deepest existing ancestor;
  a dangling symlink, or `..` in the missing part, is refused.
- **Process:** `process.run` execs the named binary directly (no shell, no
  string interpolation); only commands in the Tool's declared scope run. A
  bare grant (`process:ls`) matches the bare name or a path to the same
  file PATH resolves it to; a look-alike binary elsewhere never matches.
  Children start from an empty environment: only a minimal base (`PATH`,
  `HOME`, `USER`, locale, `TERM`, `TZ`, temp dirs, and the Windows
  system variables) plus the keys the Tool's `env:` Capability grants are
  passed on, and `FORMLESS_*` variables never are. A call's own `env`
  may set other variables, except ones that change what runs or loads
  (`PATH`, `HOME`, `LD_*`, `DYLD_*`, `GIT_*`, `*_OPTIONS`, `BASH_ENV`,
  interpreter paths, pagers and editors), which need that key's `env:`
  grant. Git under a sub-command grant (`process:git:status|log`) runs
  with fsmonitor, pager, credential helpers, external diff and ssh
  turned off; repository hooks and settings can still run, so `git`,
  `find`, `tar`, build tools and versioned interpreters (`python3.11`)
  are flagged as broad when approved.
- **Network:** `fetch` honours the `net` scope (domain allowlist when
  scoped), http(s) only; redirects are followed but every hop is checked,
  so they may not escape a domain scope.
- **Approvals** (§10.3) are the human gate for sensitive first use and for
  any Capability widening (including Candidate/optimizer-originated
  changes) when `security.secure` is on. They are opt-in: by default
  sensitive calls run unasked and are audit-logged as `tool.run_unasked`;
  Capabilities are still enforced at the host-call layer in both modes.
- **Audit log:** every approval decision, promotion, rollback, discard,
  retirement, restore, purge, import, reset, and optimiser give-up is
  recorded (actor, time, Tool, capability diff). `ListAudit {filter,
  limit, before}` reads it newest first. `filter` matches the actor,
  action or detail, `-text` leaves matches out, `*` shows everything, and
  `before` pages back. Without a filter the per-call `*.run_unasked`
  records of approvals-off mode are left out so decisions stay findable
  (filter on `run_unasked` to see them); only the newest 2,000 of those
  are kept, each naming its Session. The log is viewable in the
  TUI (`/audit [filter]`), the web Settings "Audit log" section (filter
  and "Older") and, for the agent, `read_metrics` (§8.9). Tool
  inputs/outputs recording follows §15 redaction.

## 15. Privacy and data lifecycle

- **Zero telemetry.** No analytics, no automatic update checks, no crash
  reporting. Outbound traffic goes only to the user's configured provider
  endpoint (sessions, optimiser, compaction), the user-enabled Relay
  (ciphertext only, §12), and explicit user-run commands — always with
  redacted secrets in optimiser paths (§8.3).
- **Recording:** Invocation inputs/outputs are recorded for metrics + replay,
  with secret-pattern redaction (API keys, bearer tokens, configured
  patterns), capped at `metrics.retention_per_tool` (default 200/Tool).
  One scanner (`secrets`) does this everywhere: memory notes, recorded
  inputs, outputs and errors, stored artifacts, and the code quoted to the
  provider for approval summaries. It looks for secret shapes (provider
  key prefixes, JWTs, private keys, bearer tokens, `password = …`,
  `token=…`, `cookie: …` assignments, long random-looking tokens) rather
  than key names, plus `security.secret_patterns`. A Tool result holding
  one is still sent to the model, with a one-time note per Tool and kind
  in the Run.
  Recorded payloads never leave the data dir except redacted, to the user's
  own endpoint, for optimisation.
- **Uninstalling.** `formless uninstall [--keep-data] [--keep-binary]
  [--yes]` asks for `uninstall` typed, then for every profile stops the
  daemon, removes the login service (§9.1), deletes every secret it keeps
  outside its folders (the API key, OAuth tokens, the relay secret, named
  providers' keys, MCP logins), and (unless `--keep-data`) the data and
  config folders; finally it removes the binary, unless a package
  manager installed it (Homebrew, apt/dnf/pacman, winget: it names that
  command instead) or on Windows (a running program can't delete itself:
  it says to). `formless reset` deletes the same secrets.
- **Moving to a new machine.** `formless export-all [--sessions] [-o
  FILE]` (`ExportAll`) writes one `formless.all/1` JSON file (mode 0600):
  every active registry Tool (MCP tools come back with their server), the
  shared modules, Skills, memories, recurring jobs and the config
  (providers, Roles, MCP servers, settings) with every value that can
  hold a credential blanked and listed in `blanked` (providers'
  `extra_headers` and auth secrets/tokens, MCP `env` values and
  `headers`); API keys never leave the keychain. `--sessions` adds every
  Session's transcript. `formless import-all FILE` (`ImportAll`) only
  adds: providers, Roles and MCP servers this machine lacks; modules
  before Tools; Tools as imports (renamed on collision, §7.8); Skills
  disabled for review; memories (duplicates skipped); jobs paused; and
  Sessions as new ones. It reports counts, the values to fill in and
  anything that failed.
- **Deletion:** Session delete removes transcripts + Invocation payloads but
  keeps anonymised per-Tool tallies (payloads gone, counts stay). Tool purge
  removes everything incl. tallies. **Full reset** (Settings or
  `formless reset`) stops the daemon and deletes config, keychain entry, and
  the entire data dir, with explicit confirmation.
- **Imports/exports** contain no secrets, metrics, or history (§7.8).

## 16. Error handling and edge cases (normative)

- **Endpoint unreachable mid-Run:** retries per §4.4, then Run pauses with
  the error surfaced; resume retries the request (no state loss).
- **Invalid key / model not found / `/models` down:** wizard blocks with
  the provider's error verbatim + `doctor` hints; manual model entry offered.
- **Tool creation storm (agent looping `create_tool`):** allowed (no Run
  limits), but each creation is validated/smoke-tested (§7.1) and visible
  live in chat; Esc is the stop. Creation attempts are audit-logged.
- **Candidate for a retired/deleted Tool:** cancelled; retired Tools are
  never optimised (§8.2). Promotion racing a retirement: retirement wins;
  the Candidate is discarded and logged (`tool.promote_discarded`).
- **Two creators, same name:** serialised (§7.2). **Two approvers, same
  modal:** first answer wins (§10.4). **Two clients starting a Run on one
  Session** (two sends, a send and an edit): one Run starts; the other
  start is refused "already running: run <id>" (code `busy`), and a send
  that loses joins the running Run as a steer. A finishing Run removes
  only its own controls. Changing the model during a Run is broadcast
  to every attached client; the running Run keeps its model, the next
  Run uses the new one (a working-folder change reaches it at its next
  step, with a note in the transcript).
- **A panic while a daemon lock is held** doesn't stop later requests:
  locks are taken back from the poisoned state (reported once per site
  on stderr), so one bug doesn't make every request fail.
- **Hook commands** get their payload on stdin while their output is
  read (a `cat` hook can't deadlock on a full pipe); at most 64 KB of
  each output stream is kept.
- **Bundle URLs** (`FetchToolBundle`): redirects are followed by the
  daemon one hop at a time, each https only and resolving to public
  addresses (pinned), at most 5; the body is read as a stream and
  refused past 2 MB.
- **Daemon already running, older/newer protocol:** client refuses with
  versions shown (§9.3). **Second daemon, same data dir:** refused (§9.4).
- **SQLite corruption:** daemon stops, reports, points at the pre-migration
  backup; never reinitialises silently. Opening checks the file's header
  and schema, and the daemon runs `PRAGMA quick_check` at start; any
  failure names the newest backup (a pre-migration `*.bak` beside the
  database or a daily one in `backups/`) and the `formless restore FILE`
  command, and leaves the file untouched. The §16 checklist at the top of
  `crates/formless-core/tests/edge_cases.rs` maps each bullet to its test. **Disk full:** writes fail loudly;
  the Run pauses; no partial Tool version is ever visible.
- **Huge Tool output / huge file:** streamed to artifact (§9.6); context
  receives preview only. **JS infinite loop:** burns its own sandbox thread
  until user cancel (§6.1); daemon stays responsive.
- **Malformed model tool-calls:** correction loop (§4.4), separately
  counted. **Empty-input smoke test failing at creation:** creation
  rejected (§7.1) — Tools must tolerate the smoke input or declare why via
  schema (schemas requiring fields will fail empty input; the smoke test
  uses a schema-minimal generated input in that case).

## 17. Non-goals (after v2)

- Hosted/multi-user mode, accounts, sharing servers.
- Distribution publication: code-signing/notarisation with maintainer
  certificates and submitting the prepared Homebrew/winget packages
  (release workflow, installer, checksums, and update checks exist in
  `packaging/`; see `packaging/README.md`).

## 18. Roadmap and acceptance criteria

Each milestone is independently usable; later milestones add, never redesign.

### M1 — Chat core

Daemon (lifecycle, protocol, SQLite, config/keychain), provider client
(streaming, retries, reasoning effort, probe), onboarding wizard, TUI chat +
Sessions views, compaction (daemon-side).
**Accept when:** fresh install completes the wizard against a real
OpenAI-compatible endpoint (incl. a local one); unsupported-tool-calling
endpoints are refused at probe; two TUIs share the daemon and one Session;
idle-close + auto-restart works on macOS, Linux, Windows; a long Session
compacts visibly and resumes after daemon restart byte-identically.

### M2 — Tools

`create_tool` + validation/smoke gate, QuickJS sandbox + full host API
(incl. streaming + artifacts), registry/versioning/retire/restore, approvals,
metrics recording, Tools view.
**Accept when:** from an empty registry the agent builds, validates, and
reuses Tools across Sessions; a Tool exceeding its Capabilities is denied at
the host-call layer (demonstrated for fs escape, unscoped process, env
leak); with approvals on (`security.secure`), unapproved sensitive first-run
blocks on the modal and headless runs fail closed, and with them off (the
default) the call runs and is logged `tool.run_unasked`; per-version
metrics match recorded Invocations exactly.

### M3 — Self-optimisation

Metrics aggregation, trigger, background optimiser, sandboxed replay,
promotion/rollback, guardrails, audit surfacing in Tools view.
**Accept when:** a Tool driven below trigger yields a Candidate that is
promoted only on a ≥10-point zero-regression replay win (demonstrated with
an intentionally broken Tool); replay provably touches no real files/network/
secrets; capability-widening Candidates route to human approval; three
failed Candidates flag `needs attention` and stop; rollback restores the
prior version's behaviour; `create_tool` is untouched throughout.

### M4 — Async, CLI, and data portability

Pending Invocations (handle poll/await + push delivery, abandon), server-side
compaction when supported, headless CLI (`run --json`, exit codes), Tool +
Session export/import, profiles, doctor, reset.
**Accept when:** a >300s Invocation hands off, completes in the background,
and its result reaches the agent (both paths) with the Run correctly
finishing; daemon crash mid-Run recovers as Interrupted with no re-execution;
`formless run --json` streams events and exits per §12; an exported Tool
imports on a second profile untrusted, with fresh metrics and first-run
approval; reset wipes everything incl. keychain.

### v2 — Extensions

Responses API, prompt-based tool-calling fallback, failover and measured
model routing; OAuth/PKCE subscription credentials; `clipboard`, `notify`,
`llm`, and `agent` Capabilities; shared JS modules; prompt/memory
self-optimisation; Session fork/search/edit; in-TUI Tool editor, Metrics
view, and persistent multi-pane layout; encrypted Relay + Web frontend;
release workflow, installer, packaging templates, and `update check`.
**Accept when:** Responses and prompt-mode endpoints stream, probe, and
call Tools against the mock server, and failover reaches the next model
(`v2_provider`); `model="auto"` selects only from measured outcomes and
honours fast/strong subsets (`v2_routing`); OAuth login verifies `state`,
sends the PKCE verifier, persists rotated refresh tokens, and refreshes
mid-Run (`v2_auth`); each new Capability is denied without its grant and
stubbed in smoke tests (`v2_caps`, `v2_tools_platform`); a prompt proposal
applies only after a measured win and rolls back individually
(`v2_selfopt`); Relay traffic round-trips encrypted and the room id cannot
yield the key (`relay`).

### v2.2 — Built-ins for self-expansion

`find_tools` / `read_tool` / `test_tool` and lazy Tool listing (§7.1);
honest outcomes (throw on failure, returned `{error}` counts as failure)
and inconclusive-after-stub smoke tests; async `run`; binary encodings,
`fs` append/stat/mkdir/remove/rename, `process.run` options, response
headers, truly streamed `fetchChunks`, scope-checked redirects (§7.5);
exact `process` scope matching and dangling-symlink refusal (§14);
`spawn_subagent` removed in favour of the `agent` Capability, with a
sub-agent depth guard, no sub-agent step cap, and approvals for
sub-agent Tool calls (§7.6).
**Accept when:** `v2_builtins` passes: a 46-Tool registry hides a Tool
until `find_tools` loads it; `test_tool` runs drafts without saving or
measuring; a generic `fs:read:$input.path` Tool is created while a Tool
with a pre-stub bug is rejected; `process:ls` refuses a copied `ls`;
chunked reads and streamed fetches never split UTF-8; redirects out of a
`net` scope are denied; `agents.run` recursing through a sub-agent's
own Tool call is refused at the depth limit; a sensitive Tool called
without an approver fails closed.

### Cross-cutting acceptance (all milestones)

- Zero outbound traffic in tests except to the configured endpoint
  (asserted with a recording mock endpoint).
- Every setting in §13 changes behaviour without recompilation; no hidden
  constants for any settled default.
- Every §16 edge case has an automated test or scripted demonstration.

## 19. Testing strategy (summary)

- Unit: schema validation, capability path containment, metrics math and
  trigger/promotion bars, compaction checkpoint round-trip.
- Integration: daemon against a **mock OpenAI-compatible server** (scripted
  streaming, tool-calls, 429/5xx, malformed calls, no-tool-calling probe
  failure); QuickJS sandbox escape suite (one test per Capability boundary).
- End-to-end: scripted TUI/CLI flows for M1–M4 acceptance lists; crash-kill
  daemon mid-Run and mid-Promotion recovery tests.
- No test may require a real provider key; all provider behaviour is mocked
  or pointed at a local compatible endpoint.



