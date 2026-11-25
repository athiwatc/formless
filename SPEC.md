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
5. **Local-first.** The daemon stores Sessions and Tools locally.
   See [network activity, naming controls and telemetry](#15-privacy-and-data-lifecycle).
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
  mode, spawned by clients (see §9). Ordinary startup needs no separately installed service; optional per-user
  autostart is supported (§9.1).

### 2.2 Processes and clients

- The Grok Build ACP profile targets the official `grok agent --no-leader stdio`
  interface (ACP v1, Grok 1.0.92 or newer). It disables automatic updates with
  `GROK_DISABLE_AUTOUPDATER=1`; `XAI_API_KEY` and `GROK_HOME` remain environment
  inputs, never command arguments. Read-only steps add `--sandbox read-only`
  and require ACP plan mode plus the shared fail-closed tool permission policy.
  Permissions are user-answered through the shared handler; vendor tool metadata
  remains in raw input and `allow_always` is attempt-scoped. Native
  `_x.ai/ask_user_question` requests translate to the same user question types as
  the Formless bridge, with explicit accepted/cancelled replies. Questions may
  also use bridge `AskUser` or a turn-end message. The profile uses shared ACP
  dispatch, selected as harness `acp` with `acp_agent: "grok"`; see
  `docs/grok-adapter.md` for the current integration boundary and live evidence.

- One Daemon Core per data dir (single instance, enforced by lock; §9.4).
  Optional `--profile <name>` runs an additional isolated daemon + data dir.
- Local Pi harness runs stage their extension in an attempt-private directory.
  `FORMLESS_PI_MCP_CONFIG` carries per-attempt MCP command metadata and
  allow-listed environment-variable names; values of those named variables
  remain in the child environment and are not serialized into this value.
  `FORMLESS_TOPIC_CAPABILITY` is the per-attempt MCP capability token; it is
  passed through the selected child environment, never command arguments.
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

<a id="4-provider-v1-openai-compatible-only"></a>

## 4. Provider

### 4.1 Scope

- `provider.api_style = "chat"` uses **OpenAI-compatible Chat Completions**
  by default. It sends `POST {base_url}/chat/completions` with Bearer auth
  and SSE streaming, and uses `GET {base_url}/models` for discovery.
- Configure `base_url` with the endpoint's path prefix. The daemon appends
  the path for the selected API style without adding `/v1`. Extra headers (e.g.
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
  recent correlated local outcomes (§4.6): a quality gate followed by
  complete USD per success, or tokens plus latency when unpriced. The
  smoothed success/latency score retains cold-start exploration; optional
  `routing_fast_models` / `routing_strong_models` subsets constrain
  simple vs complex tasks (`routing_complex_chars`, converted to a token
  threshold at four characters per token). Context size uses the shared,
  model-calibrated prompt estimator, including Tool schemas and images; an
  unresolved auto choice conservatively uses the largest candidate estimate. No vendor ranking
  is hard-coded. Stats are visible in the Metrics view and `doctor`.
  An unmatched subset entry appears in configuration diagnostics. If the
  entire subset is unmatched, auto-routing uses the first configured
  candidate and explains the fallback in `/routing`.
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
  Chat Completions) or an image. Merely mentioning an image does not
  resend a request with no image left to remove. Each recovery kind is
  applied at most once per request, and exhausting the total resend cap
  reports the final HTTP status and error body. Tools are native, or with
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
  (native Messages API), OpenRouter, Google Gemini
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
    These URL-or-preset inputs accept case-insensitive HTTP(S) schemes;
    only the scheme is lowercased. The remaining URL preserves its case
    and escaping, apart from existing outer-whitespace and trailing-slash trimming.
- Onboarding collects `base_url` + API key, then fetches `/models`; the user
  picks a **model** and a **reasoning effort**. If `/models` fails, manual
  model-name entry is offered. Model + effort are a daemon default,
  overridable per Session, changeable mid-Session, effective on the **next
  Run** (never mid-Run).
- **Model picker.** The wizard, `/model` and the web client share one
  picker. Typing filters the list by fuzzy (subsequence) match on the id:
  word starts and runs of consecutive matches rank higher, matched
  characters are highlighted, and models reported by Sessions are prioritized
  as recent. Roles
  show up as `role:<name>` rows with the provider and model they map to.
  `ListModels` returns `{models, roles, recent, current, error}`: `models`
  comes from the default provider's `/models`. `recent` deduplicates exact
  nonempty stored Session model IDs, including archived Sessions, after sorting
  stored `updated_at` descending and Session ID ascending for ties. This is
  Session-update recency, not measured model-use history. When the list can't be fetched, the typed text is
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
  model across all three API styles.
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
- Structured-output validation foundation (#618): immutable call options default
  to legacy text, or hold an unchanged JSON Schema and an offline validator.
  `complete_result_with_options`, `complete_with_usage_with_options` and
  `chat_stream_with_options` accept these options; existing signatures keep
  their text behavior. `complete_json` also returns the parsed value with its
  usage/served-model metadata. Schemas are limited to 64 KiB serialized,
  4096 JSON nodes and depth 48; local JSON-pointer references to recognized schema
  locations are supported with reference depth 64 and expanded work 16384 nodes.
  External references, resource IDs, dynamic/anchor and recursive references
  fail before any request. References inside `const`, `default`, `enum` and
  `examples` remain ordinary unchanged data. Validation uses bounded regex
  compilation/backtracking and accepts exactly one whole JSON value (no prose
  extraction/coercion), at most 1 MiB, 16384 nodes and depth 64. Errors do not echo
  schema/output values. These limits are not a wall-clock deadline or OS sandbox.
  Existing Tool input/output schema handling is unchanged.
  Chat sends `response_format: {type: json_schema, json_schema: ...}`; Responses
  sends `text.format: {type: json_schema, ...}`. The exact schema is sent with
  `strict: false` so arbitrary valid schema data is not rewritten into a vendor's
  restricted strict-schema subset; the local validator remains authoritative.
  The call owns this format field after vendor extras/native repairs, before
  request policy, admission and accounting. A 400 explicitly rejecting the mode
  retries once without that mode; invalid-schema/unrelated errors do not qualify.
  This state persists across transient retries, model fallback and stream restart,
  without changing another call or the configured provider. Anthropic uses no
  invented format field and is validated locally. Schema calls include one fixed
  JSON-format instruction and the original schema as explicitly framed user data;
  schema descriptions never become system instructions. This stable guidance
  remains when the endpoint rejects its API format mode, and is absent for legacy
  text calls. Schema-aware replies refuse
  incomplete/refused answers; partial stream deltas remain text until final
  validation. Every known physical-attempt charge is committed before validation.
  `llm.complete(prompt, {schema, system, model, role})` returns a parsed JSON value;
  omitting `schema` still returns text. Invalid schemas fail before model work;
  smoke stubs schema results without making a request, and replay denies model
  calls. Both optimiser candidate and guidance-proposal generation use schemas,
  while syntax, capability/resource checks and promotion approvals still apply.
- API key storage: OS keychain (macOS Keychain, Windows Credential Manager,
  Linux Secret Service) with a `0600`-permission file fallback. Keys are
  never written to `config.toml` in plaintext, never logged, never sent
  anywhere except the configured endpoint.
- OAuth is CLI-led because authorization happens in a browser:
  configure `[provider.auth]` (`mode`, `authorize_url`, `token_url`,
  public `client_id`, optional `scopes`, localhost `redirect_port`, and
  provider-specific extra authorize/token parameters), then run
  `formless auth login [--provider NAME]` (default: `[provider]`; named
  entries come from `[providers.NAME]`). `auth status` and `auth logout`
  accept the same selector; unknown names fail explicitly. Login binds only
  `http://127.0.0.1:<port>/callback`, uses a fresh `state` and S256 PKCE
  verifier/challenge, exchanges the code at the configured token
  endpoint, and stores tokens via keychain + a `0600`
  `oauth_tokens-<id>.json` fallback, one set per provider (`<id>` is
  derived from its `token_url` and `client_id`, so two OAuth providers
  never overwrite each other). Endpoint identity canonicalizes scheme/host
  case, default ports and trailing slashes, preserving path case and query.
  Slot derivation, recorded issuer checks and refresh locks share this identity.
  Credentials are used only with a proven matching recorded issuer and exact
  token endpoint, including unexpired access tokens. Versioned credentials
  retain the actual token endpoint separately from canonical lookup identity;
  refresh uses that captured path/query, never a newly configured spelling,
  and token requests refuse redirects. Rotation preserves this endpoint.
  Token-request transport errors omit the URL, and remote error details mask
  submitted query values in decoded/wire forms before truncation. Standard
  OAuth failure categories are classified before masking diagnostic text.
  Every non-versioned legacy record requires re-login: historical migration
  could derive both its issuer label and raw slot from the current config,
  so even a matching noncanonical issuer/hash is not transport proof.
  Canonical-only records also cannot prove whether the original endpoint had
  a trailing slash. These records remain untouched and produce a re-login
  warning, never a guessed transport destination.
  Read-only lookup does not rewrite stores; login/refresh saves the canonical
  slot, and logout removes matching discoverable aliases too. Arbitrary old
  keyring-only hashes cannot be enumerated after an endpoint spelling edit;
  a finite lookup covers the configured raw spelling and its slash variants
  for diagnostics/logout and independently versioned records,
  but arbitrary unknown historical spellings still require re-login.
  Both copies carry the time
  they were saved and the newer one wins; when only one write succeeds,
  the other store's older copy is removed (or the save fails). `formless
  auth status|logout` inspect or delete those tokens without printing
  them; logout first revokes the refresh token (RFC 7009) when
  `provider.auth.revocation_url` is set, reports any copy it could not
  delete, and says when an API key or `FORMLESS_OAUTH_ACCESS_TOKEN` stays
  in force. `doctor` reports whether the keychain works or only the
  files hold secrets. `auth status` and `doctor` report credential presence
  and issuer warnings for every configured OAuth provider without refreshing
  or printing tokens. Full reset deletes them too.
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
  skew) forces at most one refresh for a logical authenticated request:
  chat/completion across model fallbacks and outer stream-drop resends
  (including native Messages streams), all pages of a model listing,
  compaction, and calls made by a managed operational probe. A failed forced
  refresh retains the original 401 and refresh-error context even when a
  later fallback returns a hard 400/403 or credential resolution fails, and
  attempts configured fallback models; it does not replace the error or reset
  the refresh allowance per model. HTTP rejection and OAuth token error
  bodies used for diagnostics are masked against exact
  submitted credentials (including opaque access/refresh tokens, authorization
  codes/verifiers and confidential client secrets) before truncation or
  preservation in context. Status and OAuth error codes remain available.
  Retry classification and request repair inspect the raw response separately;
  masked text never drives protocol recovery, and probes receive typed rejection
  classifications without retaining or exposing the raw credential-bearing body.
  API-key/fixed-token and explicit environment overrides do not redeem
  a stored refresh token. `doctor` remains read-only, including its fixed-token
  diagnostic probes; operational probes can use a managed Provider.
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
  Built-in matches are anchored to family patterns and exclude known
  text-only and embedding variants. Partial `supported_parameters` listings
  add positive tool/reasoning facts only; absence does not downgrade a known
  capability. Each model in the provider listing has a `source` (`endpoint`,
  `builtin` or `mixed`) and an `inferred` list for guessed capability fields;
  both pickers prefix guessed facts with `~`. `provider.model_meta` keyed by model id can
  explicitly correct `vision`, `tools` or `efforts` after listing/table data.
  The TUI and web pickers show it on each row (`200k ctx · $3/$15 per M ·
  vision · tools`). TUI model prices retain the metadata's thousandths of
  USD per million tokens (0.075/0.007/0.001), trimming unused trailing zeros;
  an actual zero and whole-dollar prices remain distinct and unpadded.
  Context labels retain exact token counts: decimal multiples
  keep k/M units (200000 → 200k, 1500000 → 1.5M); other exact binary multiples
  use explicit Ki/Mi (131072 → 128Ki, 65536 → 64Ki). Smaller or irregular
  windows use exact decimal fractions only when no longer than the whole count,
  otherwise the whole count, never a rounded-down zero or integer. Missing or
  invalid context metadata is omitted (web numbers must be safe integers).
  `efforts` limits the effort choices to what the model
  accepts (`[]` hides the picker: effort has no effect; absent offers all).
- **Aliases.** `[provider.aliases] fast = "…"` names models; an alias is
  accepted wherever a model id is (`/model fast`, `--model`, Roles,
  candidates, fallbacks) and resolved at use, so changing it moves
  everything that names it. The pickers list aliases first.
- **Missing models.** The daemon checks each provider's configured default,
  fallbacks, routing models and Role candidates against `/models` once every
  24 hours, saving the last check and its evidence per profile. A model is
  flagged after two consecutive complete listings omit it. Known aliases and
  dated snapshots count as matches. Empty, partial, failed and Azure
  deployment listings cannot add a warning. `ListModels.missing` and
  `Status.missing_models` describe the provider, model and configuration
  locations. Status also shows the check time and reasons for skipped checks.
  `formless providers dismiss-warning PROVIDER MODEL` hides one warning.
- **Which model answered.** When another model than the requested one
  produced an answer (a fallback, or auto routing's pick), the message is
  stored with `meta.model {name, via, requested}` and clients show "via
  gpt-4.1-mini (fallback)" under its header. `via` is `fallback` when the
  provider used a fallback attempt, or `routing:<picked-model>` when auto
  routing selected the intended model. The pick is captured from the
  resolved route, not inferred from model-name equality. Tool-call and
  pause-turn assistant messages retain the same metadata.
  Sub-agent assistant messages retain it in their transcript; completed
  sub-agent nodes carry optional structured `served_model` attribution
  separately from the role/model label. `llm.complete` keeps its text/value
  result contract and records any served-model attribution on the enclosing
  Tool Invocation; the invocation detail displays it.
  Session exports preserve assistant message metadata. Headless `run` prints
  a `via <name> (<via>)` line for a served-different final answer, and its
  JSON result includes `served_model`. `ComparisonReady` includes the served
  model for each fork when it differs from the compared model.
- **Gateways and Azure.** `provider.query_params` are added to every
  request, `provider.api_key_header` sends the key in that header instead
  of `Authorization: Bearer`, and `extra_headers` as before. The provider
  form (TUI Settings → Providers) has these as advanced fields and
  `formless providers add` takes `--header K=V`, `--query k=v`,
  `--key-header H`. `--azure-resource R --azure-deployment D` accepts
  a resource name, an Azure host, or an HTTPS endpoint. It configures
  `/openai/v1` with the deployment as the model and the `api-key` header.
  Supplying `--api-version V` selects the dated deployment-scoped API.
- **Proxy.** `provider.proxy` sends the daemon's HTTP traffic (provider,
  relay, fetch) through that proxy, from daemon start; otherwise
  `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` apply as usual. `formless
  doctor` and `Status.proxy` show which.
- **Ollama.** `/pull MODEL` (`OllamaPull`) downloads a model through the
  Ollama server behind `provider.base_url`, streaming `ModelPullProgress
  {status, completed, total, done, error}` to the status bar.

### 4.3 Endpoint probe (onboarding gate)

Before the first Session, the wizard checks connectivity, streaming, and a
**test tool-call**. The original v1 scope required native tool calling.
The current probe accepts either native or [prompt-based tool calling](#41-scope).
When the prompt fallback succeeds, the wizard saves `provider.tool_mode = "prompt"`.
A result with neither mode demonstrated blocks Session start. Probe errors
also block completion and show the reported cause.
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
  work (natively, or only through the prompt-based fallback). A test fails
  when a comparable response model identifies a different model. The result
  records the reported identity in `served_model`. Results are
  kept per (endpoint, model) in `<data>/model_tests.json` and shown in the
  pickers (`✓ tested` / `✗ test failed`; temporary failures name the manual
  retry action). Results include a backward-compatible `transient` flag for
  transport, rate/server and logical-deadline failures. Choosing a model does not start a
  paid test by default. `provider.auto_test_models = true` opts into testing
  previously untested selected models in the background; explicit Test actions
  stay available without this setting. Within this opt-in path, a later
  selection may retry a transient failure after five minutes or another
  failure after 24 hours, based on `tested_at`. Successes stay cached.
  Unknown records or unreadable timestamps retain manual-Test-only behavior;
  future timestamps wait rather than causing repeated background probes. Background tests deduplicate concurrent
  picks by endpoint/model and release their reservation on completion or abort.
  Result merges are serialized and written privately through a synced temporary
  file and atomic replacement; save failures are reported, never silently lost.
  Every logical operational probe (explicit Test, optional background Test,
  connectivity/tool checks and the two-request cache check) uses one shared
  30-second deadline, at most eight physical model-endpoint POSTs, and a
  128-output-token ceiling and one returned choice on every attempt. Probes
  override Chat `n` and drop known vendor multi-sample/beam controls at the root
  and in `options`; ordinary Runs retain their configured sampling. The deadline includes credential
  refresh, admission waits, headers, body/stream parsing, backoff and resends.
  Vendor fields, reasoning settings and typed repairs cannot remove or increase
  the final cap; probes may switch a rejected Chat cap spelling once. The
  scoped probe disables unrelated model failover, redirects and transport-level
  automatic retries, while retaining the normal provider's credentials, request
  admission and usage hooks. Ordinary Run requests keep their existing behavior.
  Probe transport uses the configured proxy and normal factory TLS trust.
  `TestModel.probe_usage` reports physical sends and only provider-reported
  counters/costs; missing counters remain unknown, with explicit partial-usage
  and partial-cost flags. A deadline or a stream without its protocol terminal
  marks even interim reported counters/costs as partial, retaining known values
  and exposing `deadline_exceeded` / `incomplete_transport`. HTTP EOF alone is
  not final cost evidence. Chat needs a finish reason or `[DONE]`, Responses a
  terminal response event, and Messages a complete `message_stop` sequence.
  Its observer does not commit a second usage charge.
  Timeout/cancellation drops the current logical request future rather than
  detaching retries. An already-issued server request may continue or be billed;
  these are client-side request bounds, not a promise of server cancellation.

### 4.4 Provider failure handling

Persistent request errors (such as unknown parameters or an API-style mismatch
reported as HTTP400/422) point to `/details`, `/model` and `formless doctor`
rather than retrying the identical request. Transient failures retain retry
guidance. If the same provider-error category recurs after Resume, the Run
banner names likely configuration keys to check (for example
`provider.api_style`, `provider.reasoning_effort`, `provider.base_url` and
`provider.extra_body`). This compares the immediately preceding resumed Run
in that Session; a fresh user message resets the escalation. Raw details stay
available, and budget/correction decisions keep their existing guidance.

Model requests made by a daemon share budget admission, including Run turns,
sub-agents, nested `llm.complete` and internal model work. With a positive dollar
limit configured, one request at a time reserves the available budget headroom;
queued requests recheck committed usage and reload current effective limits and
prices after acquiring admission, so live settings changes apply before dispatch. This conservative lane
avoids guessing provider output costs: at most one in-flight request can cross a
limit, even across concurrent Runs sharing the UTC daily limit. It is not a hard
provider billing cap; an individual response, failed remote attempt or cancelled
remote request can still incur charges. Reported usage is committed before the
lane is released; errors and cancellation release it too. If committing usage
fails, subsequent model requests fail closed until storage is repaired and the
daemon restarted. The lane never spans Tool execution. Unpriced aggregate usage retains the existing opt-in limit
semantics, and an explicit budget-resume waiver remains effective.
Known reported usage counts even when a response is rejected as unusable or a
stream fails, is cancelled, or is retried. Each attempt commits its latest known
cumulative usage once before its admission lease is released; a retry/fallback
must acquire fresh admission. A rejected answer remains an error. Unknown remote
charges cannot be inferred from a transport failure.
Nested completion and sub-agent usage, prices and parent Tool model-use costs
use the final serving model, including fallback, consistently with Run turns.


Circuit breakers are scoped to the normalized endpoint (host case, default
port and trailing slash), configured provider name and a non-reversible
credential digest, plus the model actually serving the request. Missing models
open immediately; transport failures, rate limits, server errors and failed
streams count toward the threshold. User-caused request rejections do not count.
Only a completed response closes a breaker; receiving HTTP headers does not.
Breaker notices use a dedicated global hook, independent of a Run's Session
warning hook. Image-attachment warnings stay with the originating Session;
overriding that warning hook cannot hide global breaker transitions.
After cooldown exactly one request owns the half-open probe through response
completion, while concurrent callers skip it. A failed probe immediately
reopens with doubled cooldown; cancellation releases probe ownership without
counting a user failure. If every configured model is open, the request fails
clearly until another probe is eligible. Breaker changes are global provider
notes, including internal calls, and never attributed to an unrelated Session.
The process-wide breaker cache holds at most 1024 identities, evicting the
least recently used inactive entry; active requests cannot be evicted. If all
1024 entries have active requests, a new identity is refused temporarily rather
than growing the map or losing an in-flight probe.

- 429 / 5xx / dropped stream: automatic retry with exponential backoff,
  bounded (default 5 attempts). Exhaustion ⇒ the Run **pauses** and the error
  is surfaced in the TUI; a Run never silently fails.
  Before each wait the daemon sends `ProviderRetrying {reason, wait_secs,
  attempt, max_attempts}`, and the TUI and web activity line count it down
  (`rate limited · retrying in 8s (2/5) · Esc to stop`); Esc interrupts
  the Run, including a wait in progress.
  Re-sending a dropped stream starts fresh assembly state for that attempt:
  content, reasoning, tool calls and their fragment modes, Responses call
  indexes, partial SSE fields, usage, truncation and stop reasons are discarded
  together. A Restart event clears any visible output from the lost attempt.
- Hung request: after 300s the agent is told the request is still pending
  (same philosophy as Pending Invocations, §6.4); nothing is killed.
- Malformed tool-calls from the model are fed back to the model as a
  correction prompt and counted in a **separate** malformed-call counter —
  they never pollute Tool Success Rate. The daemon pauses a Run resumably
  after **three consecutive identical corrections**, keyed by Tool name, raw
  arguments and the typed daemon correction class (invalid JSON, truncated
  arguments or unknown Tool). Changed names, arguments or classes reset the
  streak. An ordered batch containing only daemon corrections is also hashed
  and paused after three identical turns; changed arguments, order or classes
  reset that batch streak. Successful calls and ordinary Tool errors reset both
  guards, and any such outcome makes a batch ineligible. Repeated
  successful pending polls remain unrestricted. This is not a global step
  limit. Each sub-agent model loop has the same independent guard and returns
  a stuck error to its caller. All call results in an already-returned batch
  are saved before the guard stops further provider requests, preserving
  complete call/result pairs for Resume. The stored pause reason and attached
  `RunPaused` explanation describe the repetition and suggest revising the
  request or switching model before Resume. The guards retain only hashes and
  counters, never raw argument history, and direct Tool calls do not pause Runs.

- **Stream parsing (#614).** A last event without its trailing newline
  is still parsed (often the usage / finish chunk); a JSON field split
  over several `data:` lines is joined; a line over 16 MB fails clearly.
  Chat tool-call fragments are matched by `index`, else by `id`, else
  continue the last call; a server that resends the whole name or
  arguments so far replaces instead of doubling them; a call already
  streamed as deltas isn't added again from `message.tool_calls`.
  Incremental-versus-cumulative fragment modes are tracked independently for
  each call's name and arguments; appending to one cannot change how the other
  is assembled. Empty fragments do not select an incremental mode.
- **Stops that aren't answers (#615).** A refusal (`delta.refusal`,
  `response.refusal.delta`, refusal content parts) is the answer text and
  `stop_reason = "refusal"`; a `content_filter` finish sets `stop_reason`.
  Neither is ever stored as an empty answer: the note says why, and an
  empty one becomes "(The provider's content filter stopped this
  answer.)". A Responses answer stopped by `max_output_tokens` keeps its
  partial text with `truncated`, like Chat's `length`. Final output-limit
  and context-window partials persist `meta.truncated: true`; output-limit
  partials also persist `stop_reason: "max_tokens"`. When building later
  provider context, append a `[harness note]` explaining the cut-off, or
  substitute that note when the partial is empty. TUI and web history show
  a muted stop note for stored `stop_reason`/`truncated` metadata, using
  the same wording as live stop notes; ordinary completions show none.
- **Rate limits.** The `x-ratelimit-*` (and `anthropic-ratelimit-*`)
  headers are normalized to `limit/remaining/reset-{requests,tokens,input-tokens,output-tokens}`;
  OpenRouter's unqualified fields describe requests. Observations are scoped by
  provider name, normalized base URL and credential fingerprint (never exposed).
  `RateLimits` and `Status.rate_limits` return opaque entry IDs, canonical fields,
  provider/endpoint/observation time and the same `line` shown by TUI `/usage`:
  "41/50 requests left, resets in 40s". Duration, epoch-second/millisecond and
  RFC3339 resets are relative; unknown reset formats are labelled unparsed.
  Each quota kind loses its limit/remaining/reset fields after its own parsed
  reset; other kinds remain visible. Entries expire when no kind remains or
  after five minutes. Bare numbers predating 2001 as epoch seconds/milliseconds
  are ambiguous and labelled unparsed. Headerless responses do not refresh
  observations; at most 64 entries are retained.
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
    is never in the config file or a listing. Named providers also support
    OAuth through their own `[providers.NAME.auth]` and
    `formless auth login --provider NAME`; daemon routing, Role calls,
    provider tests and price listing use that provider's managed credentials.
    Missing-login errors include the provider and exact login command.
  - Requests: `ListProviders` (with `has_key`), `UpsertProvider` (the key
    goes straight to the keychain), `TestProvider` (lists `/models`) and
    `RemoveProvider` (refused while a role uses it).
- **Roles.** `[roles.<name>]` holds a `description` (when to use it,
  written for the agent), a `provider` (default `default`), a `model`
  (empty = that provider's default), an optional `reasoning_effort`, and
  an optional `system` addendum for sub-agents in the role. Roles change
  through `UpsertRole` and `DeleteRole`.
  Deletion through `DeleteRole` or harness `roles.delete` refuses while
  file or daemon-effective `provider.model`, any Session model (including archived or soft-deleted
  Sessions), or a Session template names `role:<name>`. The error names the
  references to repoint first; after they are changed, deletion succeeds.
  `doctor` reports file-edited roles that name an unknown provider, with
  guidance to select or configure that provider.
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

Financial completeness is independent of response success. A typed Responses
`response.failed` envelope with `status: failed` and final usage can retain
complete spend for a later successful decision; it never proves model quality.
Quality evidence belongs to the exact returned physical response, not another
same-model attempt. Chat finish-only evidence requires the final wire body to
request one choice and all observed chunks to retain that shape; unknown or
multiple choices need `[DONE]`. Missing protocol termination keeps reported
figures but marks routing and probe cost incomplete.

Cost-aware routing uses recent, correlated observations, not the historical
all-time Metrics totals. `provider.routing_window_days` defaults to 30 and
applies to actual dispatch, forecasts, context information and explanations.
The main auto-model quality gate requires `provider.routing_min_samples` (5)
and `provider.routing_quality_bar` (0.8); roles retain their separate
`tools.task_min_samples` and `tools.task_quality_bar` thresholds. Existing
fast/strong subsets and configured-order ties remain effective.
Model aliases resolve once at raw selection. A forecast/consent's resolved
provider/role/model is immutable through admission, media projection and HTTP
serialization; it is never treated as another alias. Scoped resolved dispatch
clones retain all request/usage hooks and resolve configured fallback aliases
independently once. Raw public Provider alias inputs keep their existing API.

Among qualified models with a complete cost basis, choose the lowest observed
USD per success. Provider-reported USD takes precedence; otherwise both token
counters and configured prices must be known (including cache pricing).
Unknown or partial usage is not zero cost. When no qualified model has a
complete USD basis, use relative tokens plus latency; if any qualifier lacks
complete token counters, use a shared latency-only comparison until they are
known, rather than discounting missing tokens as zero. An unpriced candidate
is not silently treated as free in a mixed priced/unpriced pool. Cold-start
and exploration fallbacks remain explicit. Legacy observations without
provider/endpoint or correlated cost evidence remain historical metrics;
no endpoint identity or cost is invented by joining timestamps. Schema 64 stores
immutable correlated call receipts, exact call-to-task links, and separately
closed/final task receipts. A task enters the routing sample only when its
final receipt and all expected call receipts exist. Persistence retries are
idempotent; a delayed closed receipt cannot overwrite final evidence. User
ratings and respawns join the exact task identifier, not a model/time guess.
A role receipt without its exact durable task metadata stays unscored.
Primary role completion loads its exact Run identity, independently of the
newest-Runs page; concurrent newer Sessions cannot displace that task metadata. The
Run's durable rating also applies when feedback races ahead of task insertion.
Logical producer clocks stop at completion/cancellation, while pending sandbox
and handle owners retain the receipt until late accounting finishes. Only the
responsible agent steps choose the task identity; incidental Tool-LLM and child
resources contribute cost without relabeling it. Multiple responsible serving
identities make a whole task unscored. A final neutral/incomplete step cannot
borrow an earlier successful step; cancellation and producer unwind remain
neutral. Generic transport, auth, rate, context, configuration and resource
failures make a neutral whole-call sample whose cost is excluded from the
main selector's quality-ranking numerator; billing remains independent. An
eligible call includes all its physical failed attempts before a successful
fallback. A role task includes all its responsible and incidental resources,
including context compaction and continuations. The main sample is one logical
Provider dispatch (transport/auth/body/schema/fallback retries included), not
total Run USD; a changed prompt after context compaction or pause continuation
starts a new dispatch. Only a compiled-schema
InvalidJson/SchemaViolation from an actual complete HTTP200 returned response
is an eligible model-contract failure.


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
- **Selection** (`routing_policy::select_task` over windowed schema 64 receipts):
  - A candidate qualifies once it has `tools.task_min_samples` (5) tasks
    and at least `tools.task_quality_bar` (0.8) of them completed and not
    re-run.
  - Among qualifiers with complete cost evidence, lowest USD per success
    wins. If no qualifier has a complete cost basis, time plus tokens are
    compared relative to the best qualifier.
  - With no qualifier, the role's first candidate. With probability
    `tools.task_explore_rate` (0.1), the least-sampled under-sampled
    candidate instead, so the others earn samples.
- **The primary Session counts too.** A Run on a role (for example the
  migrated `primary`) is recorded as one task with the model it resolved
  to. When the user edits and re-runs it, or corrects it ("that's
  wrong", §8.7), the task becomes bad, so per-role `auto` has a quality
  signal for the primary role as well.
- Historical per-call Metrics remain available independently of the
  recent, provider-scoped routing evidence. Transport failover is unchanged.
- Pinned roles (a concrete `model`) are unaffected.
- **Explained.** Every routing decision (per-role candidates, and
  `provider.model = "auto"`) is kept (the last 50, in memory) with each
  candidate's samples, good rate, average time, score or relative cost,
  whether it passed the quality gate, and the rule that decided
  (exploring, nothing qualified yet, or the cheapest qualified).
  `RoutingDecisions`; CLI `formless routing [--json]`, TUI `/routing` (pager), web `/routing` (dialog). The CLI preserves report order and returned figures; this wrapper does not change attribution or the report limitations tracked in #1052.

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
  Fork creation atomically records `forked_from` (the direct parent ID) and
  `fork_point_seq` (the last source message actually copied, or 0 for an empty
  prefix). Copied message sequence numbers remain unchanged, including gaps,
  so checkpoint and fork-point boundaries retain their source meaning.
  New and imported Sessions are roots; profiles upgraded from before lineage
  have null lineage rather than guessed parents. Parent deletion never deletes
  children or erases their recorded parent ID. Forks copy transcript data and
  settings, not the parent's Runs or usage charges.
  New Runs record the current transcript-tail message ID/sequence when their row is
  created, and the closing message ID/sequence on their first transition out
  of `running`. Resuming creates a new Run; relabelling an old Run never moves
  its closing boundary. Historical and crash-orphan boundaries remain unknown.
  These are whole-Run transcript anchors, not per-request billing boundaries;
  a fork inside a Run cannot split that Run's usage exactly.
  `CompareSessions` and `formless sessions compare A B --json` compare stored
  messages locally, without a model request or a Run. Related branches start
  at their recorded common fork point; missing parents keep their identity.
  If the common prefix has since changed, the diff includes full histories
  with an explicit warning. Unknown ancestry also compares full histories.
  Usage counts only physical rows owned by each Session, never copied
  transcript charges. Whole Runs after the cut can be attributed using new
  anchors; legacy, straddling or untracked work is shown separately as
  unallocated, with incomplete totals left unknown. Each physical row uses
  reported cost first, otherwise current configured provider/model prices.
  Unpriced rows leave dollar totals unknown. A known-records allocation range
  excludes missing provider usage and is never an upper bound on the bill.
  Offline comparison reads a private snapshot and file-only prices; it never
  starts a daemon, authenticates, runs jobs or writes the source SQLite files.
  Message alignment hashes payloads once, compares fixed-size keys within a
  one-million-pair budget, and verifies aligned messages exactly. Larger
  comparisons show both complete tails without claiming unchanged matches.
  RPC details have an explicit 8 MiB safety budget (below the 32 MiB transport
  cap), checked before loading oversized bodies and again after encoding;
  overflow returns a typed error instead of a partial reply or dropped frame.
  Full offline CLI output remains available without this framing limit.
  Human comparison output renders terminal and bidi controls as visible
  escapes; content linefeeds remain diff-prefixed lines. Metadata is a quoted
  JSON string labelled as data, including metadata-only changes. Display
  escaping never rewrites stored strings or changes machine `--json` output.
  `formless sessions fork ID [--name NAME] [--up-to-seq N] --json` uses the same
  atomic fork operation as the TUI and other callers; it starts no Run.
  Branch-summary insertion is an explicit three-step operation: inspect an
  ordered source/target basis, pay for generation against those exact revisions,
  then confirm the exact server-owned candidate hash. Revisions hash actual
  message identities, sequence numbers, roles, content and metadata plus meaningful
  Session/model selection settings; usage and display counters do not invalidate
  the generation's own basis. Both Sessions must be live and idle, with unchanged
  revisions after generation and again inside the atomic append transaction.
  The source projection contains at most 128 recent whole messages in a 64 KiB
  data frame. Metadata and attachment bodies are excluded; whole-message and
  original content-byte omissions are reported, with no claim to summarize an
  omitted prefix. Known secrets are redacted before inclusion. Source role labels
  are bounded display data, never provider roles or instructions.
  Generation uses the target's ordinary provider/role/model selection, admission,
  credential, breaker and physical-usage hooks, without tools or copied Session
  instructions. Usage is read from its uniquely tagged physical ledger rows;
  missing reporting or prices leave total cost unknown. Confirmation appends one
  attributed `system-note`, marked as untrusted data for every future provider
  projection, and an atomic durable receipt. It copies no transcript, grants or
  pending handles and starts no Run. Repeated confirmation returns the same note,
  including after reconnect/restart, provided that note still matches the receipt.
  Unconfirmed previews are memory-only (16 previews, 30 minutes, at most two
  generating operations); restart/expiry requires a new explicit generation.
  Candidate text is limited to 16 KiB and encoded preview details to 256 KiB.
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
  Whitespace-free identifiers joined with `_`, `.`, `-` or `/` rank adjacent
  prefix tokens before scattered matches, retaining BM25, newest timestamp and
  recording order within each tier. Every segment remains a prefix; indexed
  adjacency does not imply literal punctuation or identifier equality.
  Search uses an FTS5 index over message content (schema v24, with
  `unicode61` tokens and diacritics folded). Triggers keep the index
  current through inserts, edits and deletes. Every query word must match
  as a word prefix. Within each tier, results rank by bm25, then newest
  timestamp and recording order. A query
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
  (arguments and results over 4000 characters are cut, with an `… (cut)`
  marker). The HTML page is
  self-contained (inline styles, no scripts or external resources), and
  any HTML in messages is shown as text; Markdown images become their
  alt text and address as text (nothing is fetched when the page is
  opened), and links keep only http(s), mailto and relative targets. TUI `/export [json|md|html]`
  (writes to the exports directory), the web "Export…" dialog and
  `/export`, and `formless sessions export ID [--format json|markdown|html]
  [-o FILE]` (`md` aliases `markdown`). The CLI infers omitted formats from
  `.md`/`.markdown`, `.html`/`.htm`, or `.json` extensions, case-insensitively;
  stdout and unknown extensions still default to JSON. Explicit conflicts with
  recognized extensions are usage errors before profile setup, Session lookup
  or daemon connection. On Unix, ordinary file output is replaced atomically,
  preserving existing write-access checks and staging/final mode bits (not a
  claim of extended ACL/xattr preservation); pre-commit failures
  leave prior exports intact. The Windows implementation verifies protected
  user-only staging ACLs before writing, publishes new files without clobbering
  competing creators, and uses DACL-preserving replacement with a private
  backup. Partial OS replacement failures use no-clobber restoration or retain
  recovery files identified in the error. Native Windows acceptance remains
  pending; no complete Windows validation is claimed. Leaf symlinks (including dangling links) keep
  write-through semantics
  with bounded link traversal. Descriptor/device streams use direct writes,
  never pathname replacement.

### 5.2 Run lifecycle

The singleton Main Chat answers everyday questions in place. For an explicit
request to organize work, the Main or Topic Chat assistant can call
`read_topic_context` for recent persisted user messages, the working directory,
and the topic hierarchy. Work can be in any domain. A proposal can name a
workspace or use scratch space. An outline reflects only stated or standing
preferences, can assign any supported harness to any step, and is advisory:
the Formless model in Topic Chat makes the final decisions.
`propose_topic` records a `pending_approval` topic for the same consent UI
used by manual proposals. Independent features can reference the same
original message, and each proposed feature is visible in the shared topic
inventory. When the user has not specified a parent, a proposal from a Topic
Chat defaults under that topic; a proposal from Main defaults directly under
Main. Attempt sessions and nested agents do not get suggestion tools.
Topic Chat suggestions see the current topic, its ancestors and descendants,
unless the user enables external context. Main sees the whole tree. The
Topic Chat assistant may answer a harness question only with verified exact
evidence from an approved request or typed same-attempt human answer receipt;
it never answers user-only questions or permissions.
Approval, rejection, cancellation, permission rules, limits and external
context access are user actions, not assistant tools.
Suggesting or declining never moves a message or starts a
harness; only an explicit approval in the client can authorize that. A chat
reply, including “yes”, is not consent. Repeating a pending suggestion for
the same source chat, messages, workspace, and parent returns the existing
proposal; declining it does not cause the assistant to propose it again
automatically.

- Each user message in a Session starts a **Run**: the agent loops
  (think → tool call(s) → observe) autonomously until it produces a final
  answer, is interrupted, or pauses on provider failure/limit-free errors.
- **No limits by default:** there is no max-step, token-budget, or
  wall-clock cap on a Run unless the user sets one. The user is the limit.
- **Opt-in spending limits.** `[limits]` `daily_usd` (all Sessions, per
  UTC day), `run_usd` and `run_steps` (per Run), all unset by default and
  settable from Settings. Full dollar totals remain unknown when usage has no
  reported cost or `provider.prices` entry. Known nonnegative finite reported USD
  and priced usage still form a spending floor: an unpriced group cannot erase
  it, and that floor triggers the same 80% note and 100% pre-dispatch refusal.
  Notes identify an incomplete total as "at least" that amount; usage displays
  still show unknown totals and unpriced models. Unknown-only or below-limit
  floors keep the existing unpriced compatibility behavior, not a promise to
  bound the unknown charge. Native-media unknown-cost refusal and explicit
  Resume waivers remain unchanged. A Run on an unpriced model says so when
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
  Calls skipped in step mode are omitted from this pre-pass; every call left
  to execute still goes through its approval checks.
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
  `GetSession` lists them (`queued` texts, and `queued_items` with ids and revisions).
  When idle, Enter simply starts a new Run. Steering extends the Run only
  when its message sequence is newer than the transcript boundary included
  in the actual provider request. A steer already included after a Tool
  result does not trigger another final-answer request. Closing the Run and
  accepting steering are serialized: messages arriving after it closes or
  during terminal publication are queued before transcript append, with an
  honest queued acknowledgement and no invented future Run ID. Explicit
  interruption or a paused/failed Run retains accepted steering in the
  transcript for Resume; it does not automatically replay it.
- **The queue is visible and editable.** `QueueChanged {session_id,
  queued}` is broadcast whenever it changes (queued, sent, edited,
  removed, reordered). The TUI lists queued messages at the end of the
  chat with `/queue edit|rm|up|down N`; `/queue edit N` loads one into
  the input (`queued ›`), Enter saves, Esc cancels. The web lists them
  above the composer with edit, ↑/↓ and remove buttons. Reordering updates
  both positions in one transaction and preserves each message ID and text.
  Editors send the revision they opened with `EditQueuedIfCurrent`.
  A reorder, edit or removal that intervenes causes a conflict. Removing a
  message uses `EditQueued` without content. Clients without revisions can
  still remove messages. `MoveQueued` returns the same message ID when it moves.
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
- **Live profile limits.** `limits.run_usd`, `limits.daily_usd` and
  `limits.run_steps` are re-read before each model step, including checks
  in nested completions and sub-agents. A setting edit applies to an
  in-flight Run at its next step boundary; already-sent requests may finish
  and their usage is still recorded. Clearing a profile limit also takes
  effect at the next check. Explicit per-Run `/limit`/`--max-steps` and
  `--max-usd` retain precedence over the corresponding profile limit;
  the daily profile limit remains live with `--max-usd`.
- **Steering is marked.** A message sent while a Run is active is stored
  with `meta.steer.run_id`; when the first provider request context includes
  it, the daemon records that 1-based provider turn in `meta.steer.step` and
  sets `meta.steer.context_included`. Clients show `↳ steer sent during a
  Run` when inclusion is unrecorded, and `↳ steer joined at step N` when it is
  recorded. Forks preserve this mark as source-transcript provenance; it
  refers to the source Run.

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
  question. An acknowledged answer wins over a later interruption and is
  returned exactly once; an answer that loses resolution to interruption
  receives `answered: false`.
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
  The exact leaf template names `.env.example`, `.env.sample`,
  `.env.template` and `.env.dist` are ordinary for regular files and
  prospective new files. Existing directories, symbolic links and special
  files retain credential classification. This convention does not certify their contents;
  credential ancestors, resolved secret targets, protected profile/key sources
  and configured sensitive-path rules still take precedence.
- **Image capability guess.** `provider.model_meta[model].vision` overrides
  `provider.vision`; if both are unset, image-part preparation and the TUI
  attachment warning share the model-name heuristic.
  Full o1/o1-pro are included; o1-mini/preview, o3-mini, Gemma 3 270M/1B
  (including Ollama size tags), and Claude 1/2/Instant names are text-only
  exceptions before broad family matches. Explicit true/false overrides remain
  available for custom endpoints or renamed models. Prompt-tool image policy
  is unchanged; a name guess does not prove endpoint availability or support.
- **Provider file authority.** Stored or imported attachment metadata is not
  permission to read a host path. Native image projection requires an artifact
  ID and path/size matching this profile's artifact registry, a regular file
  beneath its artifact directory, and no symbolic links or reparse points.
  Available hashes and claimed raster MIME are checked before transmission;
  undecodable bytes never become an image part. Foreign imports/restores keep
  their artifact-reference text and report that the image could not be attached.
  The artifact owner must match the actual stored message's Session, or a
  recorded fork ancestor whose cut includes that message. This evidence is
  bounded and cycle checked; missing historical edges require re-attachment.
  New imports retain inert attachment descriptions and strip local authority,
  including reserved markers. Projection never stats an imported path. Forks
  may retain verified artifacts owned by their original Session. Public
  key-only providers have no filesystem authority. Offline forecasts resolve
  only owned records from their private snapshot; metadata-only RPC descriptors
  never cause arbitrary host-path reads.
  Preparation shares one request budget across all messages: at most 16
  attempted native references, 32 MiB source bytes, 64 MiB header-declared decoded
  image bytes, 16384 pixels per axis, and 24 MiB final encoded image URLs. Failed
  attempts retain reserved work; cached and cold requests select the same
  prefix. Omitted images keep artifact-reference text and one bounded warning
  per affected message. The image cache holds at most 24 MiB of URLs. New
  metadata-only previews share the budget with history and conservatively
  reserve unknown decoded formats and resized output; their resource estimate
  may change after upload verifies the owned bytes.
  Forecast descriptor enrichment is a separate bounded pass: at most 16 held
  non-reference candidates and 32 MiB source bytes. Combined with native
  preparation, a quote reads at most 64 MiB plus one EOF lookahead byte per
  attempted read. Snapshot registry/lineage resolution considers only the same
  bounded positional prefix and caches missing IDs; omitted/unverified held
  hashes and dimensions remain explicitly unknown.
- **Native audio and PDFs.** Owned WAV/MP3 sources can become Chat
  `input_audio`; PDFs become Chat `file`, Responses `input_file`, or native
  Messages `document` parts. These are inline bytes, without Files API uploads,
  transcription calls, or capability probes. Native audio/PDF parts have an
  8 MiB decoded source limit and share the attachment count/source/encoded
  request budgets. Unsupported API/model/format combinations retain artifact
  reference text and a bounded omission note. Provider and resolved-model
  capability overrides are explicit; unset defaults match only documented exact
  models on known first-party transports. Gemini compatibility defaults to WAV
  only; its native API's PDF/MP3 capabilities are not inferred for this adapter.
  Audio/PDF payloads never count as ordinary base64 text tokens. Their local
  media-token/cost contribution is unknown, and they do not train text-only
  token calibration. Source preparation uses a private frozen-byte map rather
  than introducing file authority into provider JSON.
  Canonical messages and verified frozen bytes survive the first-model gate;
  each physical attempt projects for its actual model/API. Content repairs are
  typed and persist across stream retries, touching only real API content parts,
  never Tool schemas, defaults/examples, structured arguments or vendor data.
  Final wire bodies reach input/accounting hooks after optional-body repairs.
  Provider diagnostics that echo submitted attachment bytes or recognizable
  encoded prefixes/data URLs are suppressed before display or truncation,
  including later errors after a content repair. Only actual attempted content
  parts supply these bounded patterns; schemas and untransmitted sources do not.
  Raw protocol responses still drive usage parsing and recovery classification.
  Strict dollar limits refuse unknown native input costs before HTTP; existing
  explicit budget waivers retain their meaning. Schema 62 records unknown native
  cost durably: generic text prices cannot replace an unreported media cost,
  including after restart or pricing changes. Provider-reported USD wins;
  reported and estimated rows remain separate cost bases before aggregation.
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
    Exact literal files take precedence; otherwise a single shell-quoted word
    is decoded (including escaped metacharacters, quotes and backslashes).
    Native Windows separators are retained when decoding the legacy
    backslash-space form, after exact-file checks.
    No variables, globs or commands are expanded or executed; relative,
    incomplete, multiple-path or multiline inputs remain composer text. A
    shell-style trailing comment also keeps the complete paste as text; no
    instructions are discarded to recognize one file. Quoted/escaped hashes
    and hashes within a filename remain literal, with exact-file precedence.
    File URLs preserve native drive/UNC paths and percent-encode reserved
    characters; dropping the URL resolves back to the same local file.
  - Attaching an image when the current model doesn't look vision-capable
    (`provider.model_meta[model].vision`, `provider.vision`, else a guess
    from the name) warns that it will get a reference instead.
  The web composer has a 📎 file picker and accepts pasted images.
- **`@` mentions:** typing `@` in the input opens a completion popup of
  files under the client's working directory (TUI) or the daemon's (web),
  fuzzy-matched on the path. Ranking prepares the full and basename queries
  once, folds each path once, and scores its basename from a rebased folded
  suffix without highlight allocation. Bounded selection keeps score-descending,
  UTF-8-byte-length-ascending, path-ascending order and stable input order for
  duplicate ties; directory completion and the Unicode fold stay unchanged.
  `ListFiles {root?, query, limit?}` walks the
  root once and caches it for 30 s. The walk respects `.gitignore`, skips
  hidden directories and `target`, `node_modules`, `dist`, `build`,
  `__pycache__` and `venv`, and stops at 200,000 entries. Directory
  symlinks are followed only when their canonical target stays inside
  the canonical walk root and does not enter hidden/build directories;
  loops are skipped. Linked directories have size zero and their
  in-root children can be completed through the link's path. An empty query
  lists the top level, and `dir/` lists that directory, so paths complete
  step by step. Tab or Enter inserts `@path`, or `@"my notes/report.md"`
  when a path needs delimiters. The `@"..."` form is reserved for quoted
  paths: whitespace inside is literal, and only `\"` (quote) and `\\`
  (backslash) are escapes. A complete token ends at its closing quote,
  followed by whitespace, ordinary trailing mention punctuation, or end
  of input. Invalid escapes, missing closing quotes and attached word
  suffixes remain plain text on submission; they never attach a first-word
  fragment. Directory completion retains the existing `/` marker and places
  the cursor before a quoted directory's closing quote, so child completion
  replaces the whole token. Exact indexed leading-space directory spellings
  take priority before hand-typed query trimming. Extraction, chips, forecast
  input and submission use the same decoded original paths without case or
  filesystem normalization. On send, each complete mention that names an
  existing file or directory goes through
  `AttachPath {session_id, path, root?}` before the message is sent:
  - text files up to 256 KB and images, WAV/MP3 audio and PDFs up to 8 MB are attached as below;
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
- **Accessible dialogs (web).** Every modal (including output viewers,
  model pickers, Session switchers, help and detail dialogs) uses shared
  focus management: Tab and Shift+Tab stay in the top dialog and its feedback
  actions. Background content is inert, and closing restores its invoking element or a visible
  control in the active view if the original element disappeared. Nested
  dialogs restore the dialog underneath before returning to the page.
  Live feedback stays non-inert and above every dialog; its actions support
  pointer and keyboard activation. Interacting with a live notification,
  including dismissing it, leaves an open mobile Session drawer open.
  Recovery after removed or state-hidden
  controls is coalesced to a frame, without layout probes on normal streamed
  paints. Hover-only hiding and deliberate pointer click-away do not move focus.
  Escape and backdrop clicks use the dialog's existing cancellation or
  deferral action where dismissal is safe; they never choose a decision.
  The approval and question dialogs additionally:
  - are modal (`aria-modal`), labelled by their title and described by
    their body;
  - list the requested Capabilities under their own label;
  - expose key shortcuts as `aria-keyshortcuts` (the visible key hints
    are hidden from screen readers);
  - keep their existing approval and answer semantics.
- **Accessible menus (web).** Session and message popovers expose menuitems;
  ArrowUp/ArrowDown, Home and End move focus within them. Escape and Tab
  close the menu and restore its trigger (or the active view's fallback),
  clearing `aria-expanded`. Closing on an outside click keeps pointer
  navigation usable. Menus inside a dialog stay within that dialog's focus
  boundary.
- **Reading controls (web).** Settings → This browser:
  - text size A− / A+ (85–200% in 10% steps, clamped at the endpoints)
    scales the root font relative to the browser's default font size;
    typography and layout use relative units, without CSS zoom;
  - density is comfortable or compact (tighter turns, line height and
    bubbles).

  Both are remembered per device.
- **Stable transcript reading (web).** Incoming live display updates are batched
  to animation frames. Streaming tails and elapsed timers retain their text
  nodes; unchanged message wrappers, turns and Tool disclosures stay mounted.
  Reading history pauses following. Explicit block/message anchors preserve
  pixel offsets across persisted output, disclosure changes and delayed diagram
  layout without native scroll anchoring. View and Session navigation retain
  paused reading state and discard stale paint callbacks.
  Streamed paragraphs retain their offsets when their step becomes persisted
  history, including Tool-call steps and saved Session bookmarks. Missing
  semantic blocks never resolve to an empty live root; bounded text/sequence
  signatures identify the matching persisted step after an offscreen commit.
  The new-output pill tracks new content below the viewport; timer ticks and layout changes alone
  do not create it. Following resumes on an explicit user scroll to the bottom
  or pill action, rather than an approximate distance threshold or a
  programmatic scroll.
- **Mobile input and viewport (web).** Visual-viewport resize and offset changes
  keep the composer and its slash/@ suggestions, menus and new-output control
  within the visible screen. Following keeps the transcript tail above the
  composer; paused reading retains its anchor. Pinch zoom remains enabled and
  does not inverse-scale text or reflow the main transcript to the zoomed width.
  On a primary coarse pointer, text fields and editor layers have a 16
  CSS-pixel font floor, including at an 85% manual reading size. Plain Enter
  defaults to a native newline on coarse pointers and sending on fine pointers.
  This browser's Settings can override or restore that device default; the
  visible compose hint and `enterkeyhint` follow the effective preference.
  Shift+Enter inserts a newline and Ctrl/Command+Enter queues. IME composition
  keys never trigger palette, mention or send actions. Shared prompts classify
  prose, machine values, URLs and secrets to reset correction, capitalization,
  spelling and autocomplete metadata on each use; closing clears hidden input.
- **Device display and printing (web).** Auto theme follows OS changes;
  the browser theme/status-bar color follows the effective Auto, Light or Dark
  theme, with media-specific initial colors before JavaScript loads. Shell
  controls, drawers, overlays, notifications and the jump-to-output button
  respect all four safe-area insets in portrait and landscape. Printing or
  saving a PDF renders the current Session on light pages with its title,
  without app chrome or the composer. Tool chips retain a readable summary;
  code wraps, diagrams fit the page, and long transcripts paginate between
  turns without clipping long turns.
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
  anchored. Completed-line split state is reused within the current answer;
  unchanged source does not repeat split work. A commit, retry, new Run or
  Session/snapshot opening resets the live text caches. Verified event appends
  reuse checkpoints; other source replacements are checked or rebuilt.
  String operations, unfinished-block formatting and DOM/layout work remain
  separate costs; this is not a guarantee of linear total rendering.
  With thinking visible, the web shows the latest six logical reasoning
  lines by default. **Expand reasoning** opens all currently retained live
  text in a bounded keyboard-accessible scrollport; appends preserve its
  reading position. **Collapse reasoning** returns to the current tail.
  Hiding thinking hides both text and control. Expansion resets with the
  current answer and does not add reasoning persistence.
- **Web `@` mentions.** `@` in the web composer completes Tool names
  (labelled "Tool"; inserted as `@name` for the agent to see) and files
  from the Session's working directory, through the daemon. A chosen file
  is attached at once and shown as a chip; it isn't attached again on send,
  even if its chip was removed.
- **What the model sees:** each attachment adds a reference line to the
  message, `[attached: name (mime, bytes), artifact ID; read it with
  read_artifact]`. Images also go inline as `image_url` parts (8 MB max)
  when the model accepts images: `provider.model_meta[model].vision`, then
  `provider.vision`, or a guess from the model name when unset. This works on
  Chat Completions with native tool calling; elsewhere the reference line
  alone is sent.

### 5.7 What a Run changed

- **Files.** Before a Tool's live `fs.write` / `fs.append` / `fs.remove` /
  `fs.rename` / `fs.mkdir` first touches a path in a Run, the daemon records
  its original kind and bounded recovery image (`run_files`, schema v51 with
  schema-66 kind and final-state fields). Later changes keep the first image.
  A newly created directory is tracked as an original missing path with a
  directory final state. Final-entry symlinks, aliases without verifiable
  restoration authority, recursive directory removal and unknown state remain
  paths-only with specific reasons. Files changed by launched programs and MCP
  tools are not captured. A durable `untracked_writes_possible` marker is set
  for process-capable Tools, `process.run` and MCP calls, even if no paths were
  captured; the same caveat appears in the recap and file views.
- At the end of a Run with tracked paths or possible untracked writes,
  `RunFilesChanged` reports tracked paths (`created`, `modified`, `removed`, or
  `unchanged` when changed back) and the coverage caveat. Clients qualify the
  count as tracked paths and warn that untracked writes are possible from
  launched programs or MCP tools when appropriate, including an empty tracked
  list. The marker is conservative: a process-capable Tool need not actually
  launch a program or write anything. `RunFiles {session_id, run_id?}` adds a
  bounded context diff for text files. Common prefix/suffix lines are preserved
  before aligning the differing middle, so a local edit in a large file keeps
  only nearby context. The LCS matrix is capped at four million cells;
  human context output labels any remaining unaligned middle explicitly.
  Raw diff lines retain their original reconstruction contract and kinds.
  The TUI's `/files` opens them in the pager; the
  web shows a dialog with diffs and Revert buttons. Explicit Run IDs must belong
  to the requested live Session, including every recorded path row.
- **Rewind.** `RewindRun {session_id, run_id?, path?}` (`/rewind [PATH]`)
  restores one or all eligible tracked paths from the original image. A
  schema-66 after-state token binds the successful tracked operation's kind,
  content hash, size and available security/identity metadata; the immediate
  postimage must remain unchanged at Run completion before it can become proof.
  Missing legacy tokens, pending/background mutations and unverifiable states
  remain read-only. Changed content is skipped with "changed since the Run";
  type changes are skipped. Rewind offers no conflict override. A positively
  recorded directory creation can be removed only with matching identity and
  an empty-directory removal; rewind never recursively removes a directory.
  Active or pending work anywhere in the profile blocks rewind, including
  background work, under the existing admission and Store lock. A system note
  names successfully restored paths. On Windows, fresh whole-file namespace
  restoration reuses the anchored retained-object protocol: restoring a deleted
  file requires its retained original object; removing a created file requires
  its private first-absence marker and the live post-operation object witness.
  Existing-file rewind likewise requires that live witness. Positively created
  directories require a live creation proof and must still be empty. Serialized
  hashes, FileIDs and database flags cannot recreate these process-local proofs
  after a restart. Missing, lost or expired proof, unsupported namespace operations
  and changed objects remain read-only with a specific skip reason. No recovery
  path repairs ACLs or enables privileges to make an operation succeed. Owned
  interactive review remains available only where its own authority checks pass.
  Files are loaded one bounded image at a time (2 MiB); text diff work is capped
  at 256 KiB per image and 1 MiB aggregate, with omissions stated explicitly.
  These checks do not provide a filesystem transaction against an unrelated
  external writer racing the final comparison and write.
- **Recovery storage (schema 66).** Every live capture is admitted atomically
  against `run_files.max_files_per_run` (128) and `max_bytes_per_run`
  (16 MiB, original bytes plus descriptor metadata). A kept creation counts as
  one file even when it has no original bytes. Zero keeps no new recovery
  payload. Overflow still records the path, with an explicit file-count or
  byte-limit reason; repeated changes never replace the first capture.
  Original kind, final after-state and a paths-only reason are separate fields;
  historical missing kind/after-state remains unverifiable, never backfilled
  from the current filesystem. Owned reads require a live Session, its Run,
  and matching ownership on every snapshot row.
  Existing GC applies `run_files.keep_runs_per_session` (10), `max_age_days`
  (30) and `max_total_mb` (256 MiB) to recovery payload, oldest first. Zero
  expires eligible payload immediately; it never disables a limit. Active Runs
  and the latest Run with recorded paths per Session remain protected. Their
  bytes count toward the total, and GC reports any excess it cannot free.
  Expiry clears original bytes and security metadata, keeps the paths and final
  tokens, and reports "Snapshot expired by retention policy". Successful rewind
  likewise leaves an "Already rewound" path tombstone, so later `/files` or
  `/rewind` never silently falls back to an older Run. Coverage-only Runs remain
  selectable even with no tracked paths. This path history establishes the
  tombstone needed before any future policy can expire the latest recovery
  payload safely. Hard Session deletion, including expiry of its soft-delete
  undo window, removes its snapshot rows. Storage and doctor report the path
  row count, retained file count, original bytes, metadata and final-token bytes.
- **Run before-metadata (schema 63).** Before a live Tool first changes a
  path, capture its bounded bytes (up to 2 MiB) and security metadata from one
  anchored no-follow descriptor, subject to protected-profile/credential read
  policy. Store both in the same immutable first snapshot; later mutations never
  fill historical NULL metadata. Only NotFound records known absence; unsupported
  or unreadable state remains unknown. Versioned before-metadata binds the exact
  bytes and metadata with an integrity checksum (not a MAC). Newly captured
  deleted ordinary files can be rejected back to their original bytes, mode and
  ownership, then explicitly undone back to absence. Exact `user.*` maps can be
  restored, and macOS `com.apple.provenance` is compared but never written,
  removed, or adopted. Other ACLs/xattrs, nonzero BSD flags, hidden attribute
  lists, unverifiable Windows security, and legacy missing metadata remain
  read-only.
  Whole-file byte-only snapshots remain available for authorized regular files
  with hard links or unsupported metadata, including a legitimate symlink-parent
  alias: both the requested name and canonical target must pass path policy,
  the final entry cannot be a symlink, and one anchored no-follow descriptor
  supplies bounded bytes and metadata. The original route's regular-file identity
  and canonical destination are rechecked before and after the read (Unix device/
  inode; Windows volume/file index through a metadata-only alias handle). Alias,
  hard-link and unsupported-metadata captures are marked incomplete and confer no
  hunk-write authority. Retargeting, unknown identity or read failure records
  unknown state, never invented absence. This protects snapshot reads; it does
  not claim atomicity with a subsequent Tool operation or strengthen legacy
  whole-file restore semantics against an external alias retarget.
- **Hunk review engine.** Interactive review uses content-bound hunk identities
  and exact byte ranges, retaining original line endings and EOF newline state.
  Inverse edits preserve every byte outside the selected hunk. Missing and empty
  files have different identities; a rejected creation can restore absence.
  Binary, generated and oversized files remain listed with explicit reasons,
  without hunk edits. Text review is bounded to 256 KiB per image and one million
  line-alignment pairs. Larger alignment workloads become one explicitly labelled
  replacement hunk, rather than invented unchanged lines. Word spans preserve
  Unicode and whitespace; excessively large word alignments are labelled and
  shown as whole changed lines. This pure engine does not read or write paths;
  filesystem authorization and stale checks belong to the owning review RPC.
  `InspectRunReview` resolves the actual latest owned Run (or an explicit owned
  Run ID); `PrepareToolReview` pins a Tool's current version and an uploaded
  code draft. Decisions use only server review/file/hunk IDs and the expected
  current image hash, never client paths or inverse text. Accept keeps the
  current bytes; a rejected hunk must be undone before it can be accepted.
  Rejections preserve exact line endings, EOF state, existence and supported
  permission metadata. Source Sessions with active Runs or unfinished background
  Invocations are refused. File decisions and Undo additionally refuse any
  active Run or live pending/background Invocation anywhere in the same daemon
  profile, including other Sessions. Read-only inspection and Tool draft review
  do not gain this profile-wide exclusion. Completed results waiting for
  await/push delivery in any Session are retained rather than consumed by
  review. Run/Invocation admission and the final file commit share the Store
  boundary, held through the final busy check and filesystem commit. This does
  not claim a filesystem transaction against another daemon or a noncooperating
  external writer; exact image checks still refuse observed external changes.
  Before changing a Run file, a prepared private receipt retains both exact
  images and permission metadata. Receipt and per-image checksums detect
  accidental corruption, including valid-base64 corruption of the pre-image;
  they are not authentication against a local profile owner. Receipt files are
  owner-only (0600/0700 on Unix; protected owner/SYSTEM DACL on Windows), with
  opaque UUID names, a 1 MiB per-record limit and a 32 MiB/128-entry journal
  limit including staged updates. Capacity exhaustion retains old recovery
  records and refuses new changes. Audit entries contain only identifiers,
  hashes, status and path labels. Exact pre/post/existence/permission comparison
  reconciles interrupted replies and status updates; an unknown state refuses
  automatic overwrite. A replacement followed by a failed durability/status
  update is reported as changed or uncertain, never falsely unchanged.
  Reopening a Run review recovers durable undo receipts, ordered by a persistent
  sequence rather than wall-clock time. Closing a pane does not cancel an
  already-dispatched action and never automatically replays or undoes it.
  Tool rejections and undo alter only the matching server-owned draft; they do
  not publish a registry version or bypass the normal Ctrl+S save/smoke gate.
  Draft reviews expire after 30 minutes; at most 16 reviews, 8 MiB per review,
  and 32 MiB of retained review data are allowed.
  Symlinks/reparse points, multiple hard links, protected profile/credential
  paths, read-only/set-ID files, and permission metadata that cannot be preserved
  remain noneditable. Legacy deleted-file snapshots have no original metadata and
  are therefore read-only. Unix UID/GID and mode are preserved. Exact `user.*`
  attributes are restored on a private stage; `com.apple.provenance` is only
  compared. Any other extended attribute, nonempty ACL, nonzero BSD flags, or
  hidden Darwin attribute list is refused rather than discarded. Absence is not
  a wildcard for an automatically stamped provenance. Windows special
  attributes, nondefault streams and unsupported access-control entries are
  refused. On Windows, ordinary existing-file inverse edits and Undo retain the
  same file object with an attribute-only HANDLE for the active review's existing
  30-minute lifetime (at most 128 retained files per review). A volume ID and
  128-bit FileID bind each checksum-protected image, but are comparison values,
  never authority to reopen an object. A single exclusive read/write HANDLE
  performs final byte/identity/metadata checks, write, truncate, flush and readback;
  no security descriptor setter runs. Opaque audit/SACL state remains on that
  object and `security_complete` remains false when it cannot be read. Byte-only
  in-place writes are recoverable, not atomic: partial or uncertain writes retain
  prepared recovery images and refuse automatic overwrite of unknown states.
  Only changes applied by the same still-active review can attach its live object
  witness to Undo receipts. Closing/reopening or restarting without that witness
  leaves the receipt as private recovery data, with explicit Undo unavailability.
  Windows created/deleted-file rejection requires durable retained-object recovery;
  readable metadata alone cannot recreate the original object identity. An already-
  deleted historical object with an uncaptured SACL cannot be reconstructed from bytes and a DACL; it stays read-only.
  A Windows retained-object store additionally supports namespace recovery when
  the original object was successfully retained before a tracked Tool change, or
  a created object was retained before explicit rejection. Its bounded private
  manifests bind the exact Session, Run, recorded path digest, original object
  identity, immutable before-bytes and seven-day retention deadline. Internal
  object names are independently random and never sent in review responses or
  audit labels. Retained objects preserve their original security descriptor:
  **the original object's data is not newly owner-only**. Existing authorized
  readers retain their original rights, including access to a retained name if
  they learn it; retention extends that object's addressability after unlink.
  Neither opaque names nor the private parent directory are a confidentiality or
  revocation guarantee. Only newly written manifest/receipt JSON is owner-only.
  A private checksum-bound missing-state marker is written before a tracked Tool
  creates a file; mutable database existence flags alone cannot authorize removal.
  Created-file inverse preparation upgrades that marker's reserved slot and keeps
  its original deadline, so a full store can still perform its promised inverse.
  Recovery eligibility expires after seven days unless a live Run/review lease
  already retains it. Physical cleanup is opportunistic at Run completion and
  later capture/inverse actions; errors retain uncertain state and never enable
  privileges or change a user's file permissions. Completed Run capture leases
  are released independently of future Tool callbacks. Active review leases keep
  their full advertised 30-minute lifetime.
  The pre-change capture hook remains best-effort. Missing retention, capacity,
  unsupported volume/identity or an interrupted capture leaves the affected
  creation/deletion read-only, never falsely recoverable. Immutable before-bytes
  remain separate from the retained hardlink's mutable body. Namespace recovery
  uses anchored HANDLE operations without replacing an occupied target and
  accepts additional hardlinks only after proving every allowed internal peer;
  unrelated aliases remain refused. Historical already-deleted opaque security
  state is not invented. Review never enables privileges, rewrites a DACL to
  retain an object, or changes OS policy.
  Windows and macOS native metadata paths require separate platform evidence;
  Linux tests do not establish their execution.
  `CommentReviewHunk` accepts at most 4096 UTF-8 bytes and derives quoted
  `path:line` context from the exact current decision set. It appends the user's
  comment with a data-framed context and steers an existing Run. When idle it
  saves the comment for a later explicit Run; it never starts a model request.
- **What I did.** After an eligible terminal Run of 4 or more steps,
  `RunRecap {status, steps, duration_ms, total_calls?, unfinished_calls, used,
  built, files, steered, untracked_writes_possible}` summarizes its own trace
  without a model call. Read the exact owned terminal `runs` row, including its
  valid stored start/end timestamps; a missing, foreign, nonterminal or invalid
  row produces no fabricated recap. Never look up a Run through a recent-results
  page or substitute an empty timestamp. Count steering from parsed steering
  metadata within that Run's timestamp window and its recorded message-sequence
  bounds when available, without loading all Session message bodies.
- `used` includes nested and top-level terminal Invocations plus built-in calls,
  with counts and failures. `total_calls` counts all actual calls, including the
  `unfinished_calls` subset; it is not the number of distinct Tool names. Running
  and pending Invocations do not appear in `used` or in successful/failed outcome
  totals. Their separate unfinished count describes the terminal snapshot time. Status is explicit: paused, interrupted and failed recaps describe
  partial work rather than implying that the Run completed successfully. Legacy
  empty status and absent totals mean unknown, not an invented outcome.
- Capture the actual Tool changes and tracked path descriptions from that
  execution, and save the bounded exact terminal recap before broadcasting it.
  Save failures are reported; no event pretends that an unsaved recap is durable.
  The snapshot survives restart/reconnect and is never reconstructed from current
  file contents or a later mutable Run row. Resume creates a **new Run ID**:
  its metrics, steering and builds do not implicitly inherit the old Run's work.
  The old paused recap remains its original historical snapshot even after that
  row is marked resumed. Legacy/crash gaps remain unavailable; timestamp guesses
  never attribute unrelated Tool versions to a Run. A latest ineligible or legacy
  Run with no snapshot does not silently select an older recap.
- Folded and expanded recaps include the possible-untracked-writes caveat even
  when their tracked-path list is empty. The TUI shows a folded "▸ What I did"
  line under the answer (`/recap` expands it); the web shows a collapsible block.

### 5.8 The agent's plan

- `update_plan(items)` is a built-in for tasks of 3 or more steps: a
  checklist of up to 20 `{content, status}` items (`pending`,
  `in_progress`, `completed`; at most 200 characters each). Each call
  replaces the whole list; an empty list clears it. At most one step may
  be `in_progress`. Only the Session's current, owned, active main-agent Run
  may write it; sub-agents return suggested steps to the main agent, and
  stale or background calls cannot replace the checklist.
- Schema 67 binds new plan revisions to their Run, update time and actual
  message boundary. Legacy rows retain their items without invented ownership
  or timestamps. `GetSession` retains legacy `plan` and adds typed `plan_state`;
  `PlanUpdated` retains `items` and includes the same metadata. The owning
  Run's durable status exposes paused, interrupted and failed work without
  silently changing item statuses. A new unrelated Run preserves the old plan
  as a previous task; explicit Resume creates a new Run and hands its plan
  forward without losing the steps.
- Every nonempty plan is bounded and framed as data at a fixed position in
  the single leading system message. This modifies only a projected context
  clone, preserving the stored prompt and cache base. Context budgeting,
  compaction and forecasts include these bytes. The first request remains the
  frozen admission snapshot described by its quote; its source Run metadata
  describes that recorded checklist, and subsequent turns read current state.
- Historical forks copy the latest known revision at the actual copied
  message boundary, including clear revisions; later plans never leak into an
  earlier fork. Portable Session/ExportAll bundles include plan data and known
  history. Copied/imported plans retain honest detached provenance rather than
  acquiring authority from a foreign Run ID. Editing/retrying a message removes
  plan revisions derived from the rewritten branch.
- Each stored plan/revision and metadata image is bounded to 64 KiB.
  Updates at the same committed message boundary coalesce; unchanged owned
  items do not add revisions. Explicit clears are revisions too. History stays
  linear in retained message boundaries and is not silently expired. Message
  deletion removes anchored revisions; Session purge removes plan revisions and
  recorded recaps. Portable bundles include `plan`, `plan_state`, `plan_history`
  and explicit message sequences. Import maps boundaries to the new transcript,
  strips foreign Run ownership and timestamps, and marks the state imported;
  it cannot overwrite an existing local plan or Run. A full legacy fork can
  retain its current checklist as copied and unowned, while an earlier cut
  never guesses where an unversioned legacy plan belonged.
- The TUI pins it at the end of the chat, just above the input (`✓` done,
  `▸` in progress, `○` pending) and the web above the composer. Lifecycle and
  previous-task labels distinguish stopped or older checklists from active
  work. Once every step is done and no Run is active it folds to
  "✓ Plan done (n/n)".

### 5.9 Step mode

- `/step` (`SetStepMode {session_id, on}`; `GetSession.step_mode`) makes
  the Session's Runs stop before each Tool call. The daemon broadcasts
  `StepPending {step_id, tool_name, arguments, index, count}` and waits
  for `StepDecision {step_id, action, arguments?}`: `continue` runs it,
  `edit` runs it with new arguments (a JSON object, also stored on the
  call) after they pass the Tool's input schema. An invalid edit is returned
  to the deciding client and leaves the step waiting, without persisting or
  dispatching the edit. `skip` answers the model that the user chose not to
  run it, and `all` runs it and the rest of the Run without stopping (step
  mode off).
  `StepResolved` tells every client the step was decided. Esc interrupts
  the Run as usual; with no client connected the Run does not wait.
- The TUI shows the call and its arguments at the end of the chat: Enter
  runs it, `e` puts the arguments in the input to edit (Enter runs, Esc
  cancels), `s` skips, `a` runs the rest; the keys work while the input is
  empty. The web shows a banner with Run, Edit, Skip and Run the rest.
  If a restored prompt only has a bounded arguments preview, the web disables
  Edit with an explanation while keeping Run and Skip available.

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
- The daemon tracks elapsed time and a **last-work heartbeat**. Ordinary host
  calls/output retain their activity behavior. `progress()` bypasses the runtime's
  generic heartbeat: only a finite fractional value above that execution's prior
  high-water mark refreshes work activity, in the daemon. Repeated fractions,
  regressions, unknown fractions, label changes, and repeated 0→1 cycles cannot
  indefinitely suppress a stall warning. Genuine nested Tool work retains the
  parent's existing activity propagation.
- After a configurable silence period (default 5 min), an unfinished Invocation
  is visually flagged `possibly stalled`, including when idle-close is disabled.
  The flag is advisory only — **nothing is ever auto-killed**. The public Invocation
  list and stall events use the same heartbeat and bounded last-progress snapshot.
  `InvocationStalled {handle, invocation_id?, tool_name, silent_secs,
  last_fraction?, last_label?}` sets the flag; `InvocationActivity {handle,
  invocation_id?}` clears it after actual activity resumes. The user may stop just
  that Invocation with `CancelInvocation {handle}` (`/cancel`, or the chip's Cancel
  button): it is recorded user-cancelled and the Run carries on. Cancellation
  handles retain their established meaning; the additive execution ID does not
  replace them.

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

- Pending-handle references copied by a Session fork are transcript data only;
  they are not rebound to the fork. Agent `await_invocation`, `check_invocation`
  and `abandon_invocation` accept only handles owned by their calling Session.
  Foreign handles answer the same unknown-handle error as nonexistent handles,
  including persisted interrupted handles after restart; the lookup neither
  reveals the owner's result nor consumes, abandons or deletes its handle.
  The original Session keeps its own single delivery. User control requests
  `CancelInvocation`/`AbandonInvocation` remain intentionally cross-Session (§6.3).

An Invocation ends only by: completion (success/failure by §8), sandbox fault
(including memory-cap fault), or user cancellation. User-cancelled
Invocations are tagged distinctly so the optimiser does not “fix” a Tool the
user merely interrupted. Provider-request stalls follow the same pending
pattern (§4.4).

## 7. The Tool system

### 7.1 `create_tool` — the Bootstrap Tool

- Signature (conceptual): `create_tool(name, description, input_schema,
  output_schema, implementation_js, capabilities[], memory_mb?, allow_breaking=false)`.
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
- **Schema compatibility.** Every new Tool version is structurally classified
  against the currently selected immutable version before persistence. Input
  changes must continue accepting the old contract: removed/renamed declared
  properties, new required fields and narrowed types/enums are breaking. Output
  changes must continue fitting the old output contract (the reverse variance).
  Nested objects, arrays, enums and optionals follow the same rules. Changed
  unsupported constraints, unresolved semantics or classifier bounds produce
  `inconclusive`, never a false compatible verdict; annotation-only edits do not
  change the contract. Classification is bounded to 4096 schema nodes, depth 32
  and 128 reported changes.
  `create_tool` responses, `read_tool`, Tool details and explicit version diffs
  expose `schema_compatibility` with the comparison base, structural changes and
  current dependency inventory. Textual schema diffs include the verdict.
  Default historical reads compare the adjacent earlier version; explicit
  `diff_from` compares that requested immutable base. Creation compares actual
  current selection, including after a rollback, not merely version-number order.
  Dependencies distinguish active caller Tools, declared running scripts,
  reusable successful script sources, immutable historical executions and prompt
  definition pins. Nested calls always resolve the current Tool: a Session's
  pinned definition or caller's own version does not pin the callee's execution.
  Enabled approved future/active jobs have dynamic prompts and no authoritative
  Tool binding; they are explicitly **possible** consumers, not claimed exact
  call edges. A literal code reference is identified when found, but absence of
  such a spelling never proves arbitrary JavaScript unrelated: other active
  caller Tools and invocation snapshots are possible dynamic consumers. Bounded
  or truncated inventories are also reported conservatively. Each inventory class is bounded to 256 entries;
  unknown remaining consumers require acknowledgement when compatibility cannot
  be proved. Running script ownership is recorded before execution and its final
  outcome updates only the same correlated source/uses/Session/Run record.
  An agent-authored breaking or inconclusive change with live/possible consumers
  requires explicit `allow_breaking: true` before a version can be saved. The
  default is false. The live inventory and verdict are recomputed under the same
  SQLite write transaction as version insertion. Caller-supplied `author` or
  `actor` fields cannot select trusted-user policy; only server call sites do so.
  Explicit user saves and externally supplied MCP definitions remain classified;
  the agent gate never replaces syntax, schema, smoke, Capability or approval
  checks. Optimiser attempts retain their actual-base report and flag schema
  breaks even when the candidate claims to fix validation-tag failures. Their
  trusted user acknowledgement binds candidate content, base and fresh dependency
  context and composes with existing widening/replay-limited approval gates.
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
  characters after recursive sensitive-key and shared-scanner redaction).
  Persisted errors and returned tool results use the same redacted detail.
  Nested failures retain the inner detail verbatim under an `outer → inner`
  call chain; each Tool has its own source excerpt and redacted input.
  Only frames explicitly named as that Tool's source are mapped: runner,
  host and dynamically required-module frames are omitted from Tool excerpts.
  Carets use UTF-8 byte positions converted to display columns, with tabs
  expanded to four-column stops and Unicode grapheme widths respected.
  Without a Tool-source position the redacted QuickJS text is retained.

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
    `formless tools tag NAME TAG…` or `formless tools tag NAME --clear`,
    `/tooltag NAME [TAG…]` in the TUI, or the Tags field in the web Tool
    detail. `formless tools tag NAME` reads the existing tags.
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
| `fs:read` | path list | Canonicalised; symlink escapes denied (§14); credential-read gate below |
| `fs:read-sensitive` | one exact absolute identity | Operation-time credential consent; grants no filesystem authority |
| `fs:write` | path list | Same containment rules |
| `env` | named keys | Only listed keys visible |
| `process` | named commands | Executed without a shell; unscoped flagged “broad” |
| `clipboard:read` / `clipboard:write` (`clipboard` = both) | — | v2; OS clipboard |
| `notify` | — | v2; desktop notifications |
| `llm` | optionally per-model | v2; LLM completions from inside Tools (§7.5) |
| `agent` | optionally per-model | v2; sub-agents from inside Tools (§7.6) |
| `store` / `store:<ns>` | own / named shared namespace | Key-value state between Invocations; not sensitive (no approval) |
| `harness:<scope>` | `read`, `mcp`, `skills`, `models`, `providers`, `roles`, `config` | Read or change the harness itself (§7.13); every change is approved with a diff |

- **Credential reads (#708).** Ordinary project reads, including input-bound
  reads, stay quiet. Builtin credential directories (`.ssh`, `.aws`, `.gnupg`,
  `.pki`, `.kube`, `.docker`, `.password-store`), `.env*`, `*.pem`, `*.key`,
  shell histories and native/browser
  credential stores require separate exact-path consent. Only the four exact
  regular-file env-template names listed in §5.6 are exempt from the builtin
  `.env*` filename rule; template-named symlinks, directories and their descendants
  remain sensitive, and user rules/protected paths still apply. This gate applies
  even with `security.secure = false`; broad read grants and trusted folders
  cannot cover it. Its synthetic Capability grants no file, network or process
  authority. The prompt uses the actual Invocation's Tool/version, Run/source
  context and computed concrete path. Refusal/cancellation prevents access.
  The daemon's active data/config roots and key fallback are hard denied
  before grants, checking both configured namespaces and current/captured
  canonical identities. Path-valued `FORMLESS_API_KEY_FILE` and
  `FORMLESS_API_KEY_<NAME>_FILE` sources are also hard denied, including
  ordinary filenames outside the profile. Their namespaces and startup
  destinations are retained; policy construction inspects paths, not key bytes. Reads, chunks, metadata, detailed listings, implicit
  prior-content reads and source relocation use the same boundary. Moving or
  deleting a directory containing the profile is also denied. Renaming a
  configured sensitive subtree, its ancestor, or contents outside that subtree
  is refused: a read consent cannot erase the namespace classification.
  Credential-sensitive directories cannot be renamed, including across
  sensitive namespaces: exact directory consent does not cover descendant
  contents. Ancestors forming a prefix of a native-store sequence (for
  example `Library` in `Library/Keychains`) cannot be renamed either; this
  conservative rule applies even if empty, without a recursive absence check.
  Default hosts
  without interactive consent, smoke tests and optimiser replay fail closed
  for credential reads; dry-run reads still require consent.
  `security.sensitive_paths` adds typed `{kind, value}` rules: `subtree`
  (absolute or `~/`), `basename_prefix` and `basename_suffix` (nonempty literal
  strings without separators or wildcards). It cannot remove builtins or the
  profile boundary and is configured through the owner's config file.
  Explicit process path operands and working directories are checked before
  launch, including relative `cat` operands and file URIs; this is not OS
  confinement of arbitrary programs or interpretation of arbitrary scripts.
  Paths are rechecked after approval and before access, without claiming an
  atomic check/open, hard-link tracking or information-flow sandbox.

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
  [`special_purpose_registry_addresses_are_not_public`](https://github.com/athiwatc/formless/blob/main/crates/formless-runtime/src/lib.rs)
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
- **Scope validation.** Empty `net:` and `process:` scopes are rejected.
  Static `net:` hosts are canonicalized to lowercase (IDNA/IP spelling
  follows URL host parsing); schemes, paths and whitespace are rejected
  with the expected bare-host form. Input-bound hosts use the same form.
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
  single-file plain JavaScript. QuickJS provides ECMAScript built-ins such
  as `JSON`. The runtime installs the host APIs listed here. Tools can
  implement helpers such as URL parsing, hashing and format conversion in
  shared JavaScript modules.
  - `run(input)` may be `async`; the returned Promise is settled before the
    Invocation ends. A Tool reports failure by **throwing**. A returned
    object whose `error` field is truthy is recorded as a failure too (§8.1)
    unless the output schema declares an `error` property.
  - Data options take `encoding: "utf8"` (default) or `"base64"`, so any
    binary data crosses the boundary as a string. Text chunks never split a
    UTF-8 character.
  - `fetch(url, {method, headers, body, bodyEncoding, encoding})` →
    `{status, ok, url, headers, body, text(), json()}` and
    `fetchChunks(url, opts, size, cb)` (a truly streamed body in bounded
    memory) require `net`. They support HTTP(S) and check the scope on each
    redirect. The configured connection and total deadlines apply to both
    APIs, with one total budget for redirects and body reads (§13).
    For fetchChunks, size is a maximum. The callback receives available data
    as each chunk is read. At the end of a stream, incomplete UTF-8 is decoded
    with replacement characters. A terminal read error is thrown after any
    available data has been delivered.
  - **Native caps (#604).** Outside the JS heap limit, whole-value reads
    stop at `tools.max_read_bytes` (`fs.read`: checked before reading;
    `fetch`: by `Content-Length` and while reading), logs at
    `tools.max_log_bytes`, and `emit` at `tools.max_emit_bytes`.
    Whole-value reads over their cap fail with a `resource` error naming
    the setting and the streaming alternative.
  - `fs.read` (whole-value), `fs.exists`, `fs.stat` (`{type,
    size, modified_ms, readonly?}` or null), `fs.list(dir, {details})`, and
    `fs.readChunks(path, size, cb, {encoding})` / `fs.readLines` (streaming)
    — gated by `fs:read`. `fs.write/append/mkdir/rename` and
    `fs.remove(path, {recursive})` require `fs:write` (both paths for rename).
    `fs.stat` includes `readonly` for on-disk entries. Writes create missing
    parent dirs. Removal is recursive when `recursive` is true.
  - `env.get(key)` — gated by `env`. `process.run(cmd, args, {cwd, env,
    stdin, stdinEncoding, encoding, timeout_ms})` → `{code, signal, stdout,
    stderr, timed_out, killed, truncated, stdout_cap_truncated,
    stderr_cap_truncated}` — gated by `process`, no shell,
    stdin closed unless given. An optional integer `timeout_ms` from 0 to
    86400000 terminates the owned process group/job on expiry and returns
    `timed_out: true`; omission preserves cancellation-only execution.
    `timeoutMs` is an alias used when `timeout_ms` is absent or null.
  - `process.spawn(cmd, args, opts)` takes the same options and checks as
    `process.run`, returning an Invocation-local handle. `handle.read(size)`
    blocks for the next available `{stream: "stdout"|"stderr", data, eof:
    false}` chunk, or `{stream: null, data: "", eof: true}`. The size defaults
    to 65536 and is clamped to 4–1048576 bytes; UTF-8 chunks do not split a
    character. The stdout encoding follows `opts.encoding`; stderr is UTF-8,
    as with `run`. Arrival order between streams is not guaranteed.
    `handle.wait()` consumes/discards any unread output and returns
    `{code, signal, timed_out, killed, truncated}`; discards are reported as
    `truncated: true`, so waiting before reading cannot deadlock full pipes.
    `handle.kill()` terminates the group/job and waits for cleanup;
    `handle.close()` does the same without requiring a result. Finished JS
    handles cache their status and subsequent reads return EOF.
  - Streamed process output is backpressured: at most 256 KiB is queued per
    stream, plus a 64 KiB pipe read and at most three UTF-8 carry bytes. There
    are at most 16 open process handles per Invocation; waiting/closing frees
    a slot. `run` captures up to `tools.max_process_output_bytes` raw bytes
    from each stream and continues draining after reaching either cap. For
    text output, it omits a valid UTF-8 character that straddles the cap.
    Malformed bytes in the captured prefix are decoded with replacement
    characters, which can expand the encoded text past the raw-byte cap.
    Base64 stdout encodes the exact captured byte prefix. Stderr is always
    text. `stdout_cap_truncated` and `stderr_cap_truncated` report whether
    the respective cap omitted bytes. `truncated` also covers any unread
    output discarded by the process layer. Timeouts and cancellation are
    supervised independently of JS reads or sleeps.
    Processes cannot outlive their Invocation: normal return, thrown errors,
    abandoned handles, and cancellation close all owned handles and reap
    children/pipe workers. A direct child that exits cannot leave background
    group members holding its pipes indefinitely. Unix uses a private process
    group, retaining the waitable leader until group termination before
    reaping it. A Unix parent policy that auto-reaps (`SIGCHLD` ignored or
    `SA_NOCLDWAIT`) is rejected before spawn, without changing any handler;
    application code must not independently reap runtime-owned children.
    Windows assigns a suspended child to a private kill-on-close Job before
    resuming it, failing closed if Job setup fails. This is process
    ownership/cleanup, not an OS sandbox for arbitrary granted programs.
    Before either process API executes, its program is resolved from the exact
    filtered child environment and working directory. The effective executable
    receives credential/profile checks, and that same canonical absolute path
    is passed to the OS; a later PATH change cannot redirect execution after
    consent. Relative PATH entries are based on the child cwd. Unix PATH removal
    uses the native default search path; Windows retains Rust's documented
    application/system/Windows/parent-PATH fallback and implicit `.exe` lookup.
    PATHEXT spelling for explicit batch-file grants and native Windows argument
    quoting are preserved; the runtime introduces no shell command parsing.
  - `progress(fraction, label)` — no Capability needed: `fraction` 0..1
    (or null when unknown) and a short label. The daemon independently clamps
    finite fractions to 0..1, treats nonfinite values as unknown, strips control
    characters from labels, and keeps at most 120 Unicode scalar values, even
    for direct host-bridge calls that bypass the JS shim. Its invocation-local
    state records the latest sanitized tuple independently of emission.
    `ToolProgress {invocation_id?, tool_name, depth, fraction, label}` is emitted
    at most once per 200 ms per execution, with no label-change or completion
    exception. The daemon publishes the latest changed tuple when the interval
    ends if the execution is still active. Unchanged tuples are suppressed.
    Only strict fractional advancement counts as work for stall detection (§6.1).
    Tool start/progress/finish and pending/completed/stalled/activity events share
    one additive execution ID. Registry IDs are allocated before pending handoff
    and match the stored Invocation, even with a zero-second pending threshold.
    Built-in scripts and draft tests share their outer lifecycle ID and have
    independent progress state. State is owned by the execution, not an unbounded
    global throttle table. Clients match IDs exactly; legacy ID-less updates may
    match only one unambiguous running call. Progress is ignored in smoke tests,
    replays and dry runs.
  - `tools.call(name, input)` uses the nested Tool checks in §7.6 or
    `mcp:<server>` for MCP calls (§7.9). It returns a Promise of
    the nested Tool's output, so several run in parallel under
    `Promise.all` (§7.12). Every call must settle before `run` is done: a
    Tool that returns while a call is still pending fails ("await every
    call"), because a forgotten `await` would otherwise pass a Promise off
    as data.
  - `clipboard.read()` and `clipboard.write(text)` require `clipboard:read`
    and `clipboard:write` respectively. `notify(title, body)` and its alias
    `notifications.send(title, body)` require `notify` (§7.4).
  - `harness.read()` and `harness.call("read", {}, "")` require `harness:read`.
    Mutations through `harness.call(op, args, reason)` require the applicable
    `harness:<scope>` and follow the approval flow in §7.13.
  - `llm.complete(prompt, {system, model, role, schema})` uses the `llm`
    Capability. It makes a non-streaming completion and returns text by
    default or a validated JSON value with `schema` (§4.2). `agents.run(task,
    {system, model, role})` uses `agent` and returns the sub-agent's final text
    (§7.6). In both calls a nonempty `role` takes precedence over `model`.
    A Role selects its configured provider and model. An explicit model or
    Role must fit any scope on the Capability (§7.4). When both options are
    omitted, the first entry of a scoped grant is used. An unscoped grant
    inherits the Session's selection.
  - `requireModule(name)` and its alias `modules.load(name)` load shared
    JavaScript modules. `modules.list()` lists names available to the
    Invocation. These helpers use the supplied module registry (§7.6) and
    require no Capability.
  - `emit(chunk)` appends to the Invocation's output artifact, subject to
    `tools.max_emit_bytes` (§7.12). It requires no Capability.
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
  - Rejected Promises still unhandled when `run` completes fail the
    Invocation with the rejection message, including floating nested calls
    and async timer callbacks. A rejection handled before completion is
    allowed.
  - Console formatting preserves `undefined`, BigInt (`n` suffix) and Error
    names/messages, and replaces circular references with `[Circular]`.
    A return value that cannot be JSON-serialized fails the Invocation with
    `return value is not JSON-serializable`, for both sync and async `run`.
    Root functions, symbols and `toJSON` methods returning `undefined` are
    rejected rather than silently becoming null; an absent `run` return
    deliberately becomes null. A result's `toJSON` is invoked only once.
  - `console.log/info/warn/error` write capped Invocation logs. `sleep(ms)`,
    `setTimeout`, `clearTimeout`, `setInterval`, `clearInterval`,
    `cryptoRandom()` (UUID) and `randomBytes(n)` (base64) require no Capability.

    `sleep(ms)` and `randomBytes(n)` require finite non-negative safe-integer
    numbers. Invalid inputs throw; requests over `tools.max_sleep_secs` or
    `tools.max_random_bytes` throw with that setting named, instead of silently
    coercing, wrapping or shortening. This changes the older permissive API:
    generated Tools must supply numeric integer values and handle limit errors.
    Successful `randomBytes(n)` returns base64 decoding to exactly `n` bytes
    (including zero), subject to supported allocation bounds. Valid sleep keeps
    cancellation behavior; its wall time still depends on scheduling.
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
  permissions. Each Tool runs under its own declared Capabilities and the
  called Tool goes through its own approval checks. Nesting depth is bounded by
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
  `agent` Capability calls `agents.run(task, {system, model, role})` to run a
  sub-agent. Its provider loop runs over the same Tools and ends on a final
  answer, a fault, cancellation or its turn limit (§4.6). The Tool receives
  the sub-agent's final text. There is no sub-agent built-in, so
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

Local tools/skills/Sessions imports, TUI import and URL bundle previews share
a UTF-8 JSON reader. A leading UTF-8 BOM is accepted. Local files and direct
URL fetches reject UTF-16/UTF-32 BOMs with the source filename/URL and an
instruction to re-save as UTF-8 JSON; their other I/O, encoding and JSON
errors also identify the source. Embedded gist file candidates that cannot
be parsed or validated are skipped; if none qualify, the gist reports that
no file is a bundle rather than a candidate-specific encoding error. Only
the leading BOM is ignored for parsing: original source text remains intact
for existing preview fingerprints. Bundle validation and import decisions
are unchanged.

- Single Tools or the whole registry export as bundle files (JS + schemas +
  Capability declarations + `memory_mb`; **no secrets, no Invocation
  history, no metrics**).
- Imports are **never auto-trusted**: Capabilities displayed, first-run
  approval applies (§10.3), metrics start at zero, author recorded as
  `import`. A name collision imports as a new Tool, never a silent overwrite.
- **From a URL:** `formless tools import <https-url>` and the web's
  "Import URL" take a link instead of a file.
  - **What counts as a link:** an https URL to a bundle, a GitHub file page
    (`github.com/…/blob/…`, read from its raw file), or a gist. Raw gist
    links fetch the linked file directly. Gist pages use the GitHub API
    for the linked revision and use the file selected by a `#file-…`
    anchor when supplied. Without a file anchor the preview selects a
    bundle from the requested revision. Anything not
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

`formless mcp prompts [--json]` reports prompts from reachable enabled servers,
in daemon order. Discovery can start configured servers and silently skips
connection/list failures, so results may be incomplete. `formless mcp prompt
SERVER NAME --arg key=value [--arg empty=] [--json]` sends named string values
to the selected server. Values preserve spaces, additional equals and empty
strings; missing separators, empty keys and duplicate keys are usage errors
before daemon contact. Human output is the returned text verbatim, including
its trailing-newline state. The daemon projects `content.text` strings joined
with blank lines; roles and non-text parts are absent. JSON preserves the full
discovery array or daemon text report, not raw MCP messages. These commands do
not submit to a Session or change server configuration; server/daemon errors
retain their ordinary exit behavior, without promised local required/unknown
argument checks.

```sh
formless mcp prompts --json
formless mcp prompt docs summarize --arg 'topic=Unicode notes' --arg suffix=
```

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
  `<data>/mcp/<name>.log`. Before a write would exceed 2 MB, the daemon
  keeps up to the last 1 MB from a line boundary. Hourly maintenance compacts
  older oversized logs, which `formless storage` also reports. Log views read up
  to the final 1 MB. Servers start on first use, stop after
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
- **Catalog safety.** Every packaged stdio entry pins an exact npm/PyPI version.
  Catalog lists and Add review show its command/URL and warn that stdio entries
  download and run registry code on first use; Add requires explicit execution
  consent. The pinned spec is retained in config and the installation audit.
  Secret and consent prompts remain usable above an open catalog/import list;
  cancelling a prompt returns to that list without adding a server.
  Parameters distinguish paths, URLs and secrets. Secrets use masked inputs;
  URL passwords, configured env/header values and known secrets are redacted
  from server targets, security overviews, approval details, errors and
  stderr-log views. The PostgreSQL catalog
  entry removes userinfo/query passwords from its URL argument and passes the
  decoded effective password in its private `env.PGPASSWORD` configuration.
  Query-password precedence matches node-postgres; a passwordless URL may use
  the daemon's explicitly passed `PGPASSWORD` instead. Literal env values remain
  in the private plaintext config file, but never in process arguments or lists.
  Path parameters expand `~`, resolve symlinks and require an existing directory.
  Filesystem roots, home and credential folders/ancestors require a second
  confirmation that names the canonical folder and exposure.
  Credential recognition shares the attachment guard's policy (including
  Docker, Google Cloud, keychains and password stores), covers nested native
  credential locations and their ancestors, and inspects credential descendants
  within 1,024 entries and 16 directory levels. Incomplete inspection, unreadable
  descendants and unresolved/external links require explicit exposure review.
  A relative, unresolvable, or parent-traversing native home never supplies a
  permissive fallback (Windows may normalize `missing/..` before checking it):
  `~` expansion fails, and an absolute folder requires explicit exposure
  confirmation when its relationship to home cannot be verified.
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
  Secret parameters use a masked terminal/form input or CLI `--param-env
  name=ENV`; they are rejected as literal CLI `--param` values. Noninteractive
  catalog adds require `--yes`; broad path exposure has a separate
  `--confirm-exposure` confirmation. The TUI review form requires `ADD`, or
  `EXPOSE <canonical folder>` for broad folder exposure.
- **Import.** `McpImportCandidates {cwd?}` reads Claude Desktop's
  `claude_desktop_config.json`, Cursor's `~/.cursor/mcp.json` and the
  project's `.cursor/mcp.json` and `.vscode/mcp.json` (comments allowed),
  and returns each server with its source, command or URL, env names and
  any `${…}` placeholder values it could not import, flagged when a
  server of that name exists. Candidate JSON never contains env/header values.
  Adding uses a ten-minute opaque daemon-side candidate reference, re-reading
  its source file and original name and rejecting changes since review.
  Clients disclose each env/header key with a value and explain plaintext
  `config.toml` storage; skipping values is the default and leaves env names
  available for manual setup. CLI `formless mcp import [--yes] [--copy-secrets]`
  shows this notice even with `--yes`; TUI `/mcp import NAME [--copy-secrets]`
  and the web Import dialog offer the same choice. Imported servers start
  disabled and untrusted, including import-all bundles; review their commands
  before enabling them in config. Import-all reports server review targets
  and successful imported Tool capability counts by family.
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
  chars), a Markdown body, and optional extra files. Header comments outside
  scalar content are ignored; whitespace-separated inline `#` comments leave
  quoted hashes and URL fragments intact. Only a complete quoted scalar is
  decoded (JSON-compatible double-quote escapes or doubled single quotes);
  literal quote fragments and block content retain their characters. Existing
  skills install as-is; historical installed metadata is not rewritten.
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
- **Source selection:** existing local folders always copy working files,
  even when their name ends in `.git`; use an explicit `file://` URL to clone
  a local repository. Existing local ZIP files are extracted. Git URL schemes
  `http`, `https`, `ssh`, `git` and `file` are recognized case-insensitively,
  lowercasing only the scheme for transport lookup; other URL bytes and recorded
  source stay as supplied. Existing `git@` and `.git` source spellings remain
  delegated to native Git validation and transport policy.
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
  enabled; `/skills enable NAME` and `/skills disable NAME` change its
  enabled state instead of previewing a name beginning with the verb.
  The web "Review" does the same preview. `/skill:NAME [args]`
  (`LoadSkill {session_id, name, args}`) loads an enabled skill into the
  Session. Once its note is saved, the manual load counts toward skill usage.
  Run completion metrics count skill loads associated with a Run.
- **Retired skills say why.** An auto-retired skill keeps
  `retire_reason` ("unused: …" or "unhelpful: N of M Runs that loaded it
  completed"), shown by `/skills` and the web. Enabling the skill or
  installing a new version clears the reason.

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
  - explicit `password/token/api_key :|= …` assignments, quoted `is` values,
    and unquoted `is` values with a secret shape
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
    input)`, which also returns a Promise. `tools.call` is always the
    dispatcher, even when `uses` contains a Tool named `call`: invoke that
    Tool as `tools.call("call", input)`.
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
    Nested return values retain JSON data semantics, including own `__proto__`
    properties through callee and caller serialization. Settlement parses the
    result envelope as JSON and calls the context's settlement function; parse,
    lookup and call failures use the existing execution-error path.

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
    code. When a slash follows `++`, `--` or `}`, the lightweight lexer
    cannot reliably distinguish division from a regex or block context:
    it hashes the exact source with a separate marker instead. This may
    miss recurrence across literal/formatting changes in those scripts,
    rather than merge scripts that execute different code. Unambiguous
    fingerprints keep their existing encoding; old stored counts are not
    rewritten. Recurrence is keyed on the fingerprint plus the sorted
    `uses` list.
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

- Before contacting the candidate-generation provider, freeze a **server-owned
  replay manifest** from recent terminal success/failure Invocations. Selection
  is independent of outcome: there is no balancing or oversampling of failures.
  `optimiser.replay_pool_limit` defaults to 200 unique inputs (2–512). Scan
  four times that many recent records, then continue if the pool is short of
  distinct inputs. Stop at 8,192 terminal records or 32 MiB of additional input,
  with at most 256 KiB per raw input and an 8 MiB manifest. Invalid or oversized
  inputs and samples outside the bounded pool are counted as exclusions and never claimed
  as tested. The snapshot records source Invocation IDs, canonical input
  digests, selection bounds and partitions.
- Group semantically identical JSON inputs before splitting, ignoring object
  key order and normalizing numbers as JavaScript receives them. Repeated
  inputs, even with conflicting recorded outcomes, count as **one** statistical
  sample and can occur in only one partition. A group retains the known-success
  regression guard if **any** represented source Invocation succeeded, even
  when its newest representative failed. Frozen provenance separately records
  that representative's latest outcome: current failure examples, validation
  warnings and capability-only generation refusal use the latest context, while
  regression checks retain the any-known-success guard. Rank groups deterministically by
  a digest of Tool identity, base version and canonical input, independently of
  recorded outcomes. `optimiser.replay_holdout_percent` defaults to 50 (10–90).
  Only the generation partition may appear in the candidate prompt; held-out
  inputs, their errors and source contents are never included. Generation and
  promotion do not resample the snapshot.
- Schema v65 stores the immutable manifest and an immutable one-to-one attempt
  binding separately from candidate JSON and attempt presentation. A binding
  verifies the exact Tool and base version; arbitrary detail/provenance fields
  cannot mint proof. Raw held-out inputs stay in the private database; attempt
  detail exposes hashes/counts/scores, not those inputs. Replay and approval
  retries, including after restart, use this same frozen evidence. Legacy or
  missing/unverifiable evidence cannot auto-promote and requires explicit
  manual approval, with its lack of holdout proof clearly shown.
- Replay evidence storage is bounded to 64 manifests and 64 MiB total. Evidence
  older than seven days is reclaimed only when unbound or its attempt is
  terminal (promoted/rejected/discarded/failed/cancelled); generated/replaying/approval-
  pending evidence is preserved. Full capacity refuses new generation rather
  than discarding active evidence. Missing expired proof never becomes a
  fresh purported holdout. Each database upgrade backs up the old schema and
  preserves legacy attempts without inventing evidence.
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

- **Automatic bar:** evaluate Candidate and baseline on the **same held-out
  unique inputs**. Require at least `optimiser.replay_min_holdout` samples
  (default 20, range 2–512), Candidate success ≥ `promotion_margin_points`
  (default 10 points) above baseline, zero regressions on previously-successful
  replays, and a two-sided exact paired McNemar test with p ≤
  `optimiser.replay_significance` (default .05, finite range (0, .05]). Invalid
  statistical settings fail closed. Attempt detail records held-out sample and
  pass counts, discordant-pair counts, effect size, confidence intervals and
  paired significance. A positive small-sample or statistically inconclusive
  win goes to `awaiting_approval`, never automatic promotion. Existing
  capability-widening, schema-breaking and replay-limited approval gates still
  compose with this decision; no evidence label grants approval authority.
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
  - throwing on failure; a returned `{error}` counts as failure unless
    the output schema declares an `error` property for domain data. Declarations
    may be supplied through local JSON Pointer references, `allOf`, or matching
    `anyOf`/`oneOf` and conditional branches; nested properties and unused
    definitions do not declare the output field. Inspection is bounded to
    64 levels and 4096 schema values, with cyclic or unresolved branches
    conservatively retaining the failure heuristic; no external schema is fetched
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
  connection); an auth error or bad request fails at once. With no HTTP status,
  an extra measurement retry or temporary-failure explanation requires a known
  transport diagnostic at the start of an error clause (e.g. request timed out,
  connection reset/refused, stream dropped or explicit server overload).
  Generic words such as stream, connection, timeout or reset inside model,
  schema, authentication or parameter errors do not establish transience;
  unknown prose gets no extra retry. Explicit probe/deadline metadata retains
  its existing meaning. A proposal
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
  code tokens, ignoring comments and layout whitespace while retaining literal
  spellings, token boundaries and restricted-production/postfix line breaks.
  Strings, regexes and whole templates (including interpolations) remain
  value-sensitive. Numeric literals stop before property dots and binary
  operators (including hex digits e/E followed by + or -), keeping their
  original spelling; legacy leading-zero forms use raw-source comparison.
  Uncertain slash goals (e.g. after a closing parenthesis or
  brace, or after a line break that may end a statement through automatic
  semicolon insertion), unsupported syntax and unfinished literals compare
  raw source; they can miss a duplicate rather than guess an identical-code
  verdict. This exact key is separate from lossy script recurrence fingerprints.
  Otherwise the score is 0.4 × the name-token
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
  file and commands without creating data/config directories, loading
  config/credentials, starting a daemon or executing a service manager.
  Reinstalling a launchd agent unloads the old definition
  before replacing and loading its plist; a manager-reported load/unload
  failure is surfaced even if launchctl exits zero. Service profile names
  must be 1–64 ASCII letters, digits, `-` or `_`, starting with a letter or
  digit. Paths are encoded for each manager: XML entities for launchd,
  quoted argv/environment, literal `%%` specifiers and a `:` command prefix
  disabling environment substitution for systemd (so `$` stays literal), and
  doubled `%` plus disabled delayed expansion for Windows batch files.
  Control characters and invalid Windows filename characters are refused
  before writing. Systemd executable paths containing quotes or backslashes
  are also refused because its command parser cannot represent them; this
  restriction does not apply to data, config or log paths.
  The service runs `formless daemon serve --stay`, which prevents idle exit
  while the daemon is running. On Linux, installation and dry-run output
  report whether systemd user lingering is enabled. To start before login
  and continue after logout on a headless host, run
  `loginctl enable-linger "$(id -u)"` separately. Scheduled delivery still
  depends on host, daemon and network availability. `uninstall-service`
  unloads and removes it. `formless doctor`
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
  every 2 s, starting a daemon itself after 6 s. Enter keeps all typed
  input, including slash commands, with a not-sent/offline note; no
  side-effecting command is queued or replayed. Old approvals, promotions,
  questions and their decision state are discarded with a count. Once
  connected it reloads the Session's authoritative running/queue/plan
  state and global pending prompts before clearing the reconnecting banner.
  Events arriving during that read are reconciled without replaying live
  stream resets. Failed recovery reads keep the banner and retry reads
  only. If the previous Session was deleted, its text/files stay in that
  Session's draft and never move into a replacement Session. A newer
  RunPaused event retains its explanation and Resume banner over a stale
  snapshot. An interrupted or timed-out pre-send attachment upload restores the original
  Session's text/files for review without automatically sending them; a
  newer draft remains untouched until its input is empty. It notes how long
  it was away. It clears the old daemon's
  approvals mode on disconnect and requests `Status` on every new
  connection, so Settings reports the attached daemon's mode. Until the
  reply arrives the mode is shown as unknown. The web already reconnects
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
  before it started"). Successful `SendOutboxMessage.client_msg_id` receipts
  are stored durably per Session, so a persisted outbox retry returns the
  first outcome across reconnects, daemon restarts and outages of any duration.
  A different Session using the same key performs its own send. A key is
  claimed before execution; concurrent retries wait for the in-flight outcome,
  including errors. Losing a
  response/waiter does not cancel the claimed send or lose its outcome.
  Outbox sends compare the complete pending attachment inventory at admission;
  queued outbox actions do not consume attachments added after they were queued.
  Browser drafts and outbox entries are stored per relay pairing or local page
  origin. Local origin includes the port: reopen the UI at the same host and
  port to recover local data (for example, consistently run `formless web
  --port N`). A different port is a different browser origin; cross-origin
  storage migration is not provided.
  Cached in-flight/failure outcomes are process-local for 10 minutes after
  completion; successful durable receipts remain until their Session is deleted.
  Cached failures preserve the same machine-readable error code and message.
  TUI, plain mode and headless sends carry a fresh UUID per user action.
  Until a response arrives, retrying the same text in that Session reuses
  the UUID and original send mode, including after a TUI timeout or
  reconnect. A response clears the action, so a later intentional send
  of the same text gets a new UUID. The TUI restores an unacknowledged
  message to an empty draft on timeout/disconnect without overwriting
  a newer draft. A late success retracts the untouched restored retry
  draft, so Enter cannot accidentally send it again as a new action.
  Buffered replies are consumed before disconnect cleanup: an accepted
  send acknowledged immediately before EOF is never restored for retry.
- A client that falls behind loses live updates first; all six droppable kinds
  (`MessageDelta`, `ReasoningDelta`, `ToolProgress`, `RunUsage`,
  `InvocationActivity`, `ModelPullProgress`) are observable with `DeltasDropped`.
  Its original `session_id`/`run_id` fields remain; additive `kinds` lists the
  skipped kinds, and optional `handle`/`model` identifies updates without a Run.
  An absent Session or Run is represented by an empty original field. Gaps are
  coalesced per Session/Run (or Session/Invocation, or model), so simultaneous
  Runs never overwrite each other's warning. Each client keeps at most 4096
  pending scopes; if another distinct scope cannot be remembered it disconnects
  instead of silently losing the warning. Once the writer drains its queue it
  schedules the pending warnings, even if no further event is broadcast.
  The answer's `MessageComplete` carries the whole text; plain/TUI/web clients
  warn about skipped text or progress. When a non-droppable event cannot be
  queued the client disconnects and all its connection's tasks end.
- Each connection may `Subscribe {sessions, deltas}`: only the listed
  Sessions' transcript and live events (and token deltas only if
  `deltas`) reach it. Tool/module/proposal events, approvals, the Session
  list and Run start/end stay global. A new connection receives everything;
  the TUI, web and headless clients subscribe to the Session they show.
- Optional capability `future_transcript_attempts_v1` adds per-connection
  `SubscribeTranscriptAttempts`, `GetTranscriptBarrier`, and correlated
  answer-only `TranscriptAttempt` start/delta/close frames. It admits complete
  future attempts after a later producer barrier; it is not mid-attempt replay
  or a canonical commit watermark. Canonical events cannot be discarded by
  that cursor. Existing Subscribe restores legacy mode. The native desktop
  keeps this path disabled pending full transport acceptance. See
  [the exact contract and acceptance gates](docs/desktop/future-attempt-protocol-20261115.md).
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
  `formless daemon restart`. Restart uses protocol shutdown. If an incompatible
  daemon cannot authenticate that request, it directs the user to its service
  manager or original binary and refuses to signal a PID marker that may be stale.
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

- Single instance per data dir (kernel-held ownership lock); starting a second daemon against
  the same dir is refused and points at the running one. `--profile <name>`
  gives an isolated daemon + data/config dirs (intended for testing
  dangerous Tools). Named profiles live in sub-directories of the default
  profile's data and config dirs.
  Profile names use 1–64 ASCII letters, digits, `-` or `_`, beginning with a
  letter or digit. The lowercase `default` names the parent profile; other
  spellings of it and Windows device names are refused on every platform.
  Names reserved by the default-profile namespace are refused case-insensitively:
  `api_key`, `artifacts`, `backups`, `commands`, `exports`, `last_run_session`,
  `mcp`, `models`, `profiles`, `relay_secret`, `review-objects`, `review-receipts`
  and `skills`. Add new owned top-level entries to the shared reserved list.
  CLI parsing and direct path resolution reject unsafe names before creating
  directories; checked named paths also reject case-insensitive sibling
  collisions, linked/reparse profile directories and paths that equal or contain
  another configured default root, including canonical aliases. Configured
  parent-root aliases and legitimate noncolliding profiles remain supported.
  Profile discovery returns `default` once and lists only valid profiles. It
  reports ambiguous legacy directories and marker/namespace inspection errors
  without modifying them. A normal `commands/*.md` folder is never a profile;
  a legacy `commands` directory containing profile config/data is preserved
  with an actionable warning. `profiles rm` and lower-level reset independently
  validate the namespace before service or credential effects. Parent reset or
  uninstall refuses while ambiguous/unreadable legacy entries remain, rather
  than recursively erasing hidden profile state. Back up the reported paths
  outside Formless roots and recover them manually into a valid profile before
  retrying cleanup. No automatic migration, rename or legacy cleanup occurs;
  clone/rename features are separate work.
  Ownership is acquired before opening/migrating the store or performing startup
  recovery. It holds private per-user locks for both the configured absolute
  lexical path (without resolving symlinks) and the current canonical data
  directory, acquired in sorted unique order. These locks live outside removable
  profile directories and are never unlinked. Removing or retargeting a configured
  symlink cannot split ownership while maintenance still changes its config. `daemon.lock`
  is an owner-written PID marker only. Auto-start and service installation share a
  separate launcher gate; installation stops a running daemon and waits for its
  ownership to release before loading the service. A second daemon cannot alter
  the owner's PID marker, socket, uploads or recovery state.
  Profile owners, launchers, directory creation and ordinary CLI lifetimes hold
  shared hierarchy gates for data/config paths and their raw/canonical ancestors.
  Destructive preparation retains its launch gate while releasing its own shared
  lease before freezing affected roots exclusively (shared ancestors, sorted and
  deduplicated); overlapping roots cannot self-deadlock. New descendant profiles
  cannot be created, started or written by a current CLI during that freeze.
  Reset, profile removal and full uninstall hold this freeze plus ownership
  through destructive changes. Full uninstall asks
  the service manager to disable registration even if the definition is missing;
  an unknown manager/ownership state preserves the profile and binary. Full
  uninstall prepares every known profile before deleting any profile files,
  freezes the base roots, rechecks the inventory, and erases named children before
  the default parent. Failed child preparation preserves every profile; failed
  child erasure prevents recursive parent erasure. An inventory change refuses
  deletion and asks for a retry.
  `formless profiles list [--json]` shows
  each profile (default first), whether its daemon runs, when it was last
  used and its data dir, and marks the current one. `formless profiles rm
  NAME [--yes]` asks for the name to be typed, stops that profile's daemon
  and deletes its data, config, API key, OAuth tokens and relay secret. The
  default profile cannot be removed this way (`formless reset`).
- Data dir (OS-appropriate): `~/Library/Application Support/formless`
  (macOS), `$XDG_DATA_HOME/formless` (Linux), `%APPDATA%\formless`
  (Windows); config in the matching config dir (`config.toml` + keychain/
  fallback key file). Contents: SQLite DB, Tool version files, artifacts,
  rotated daemon logs, audit log. With `log_keep > 0`, once `daemon.log`
  exceeds `daemon.log_max_bytes`, the daemon shifts numbered history, renames
  the live file to `.1`, then reopens stdout/stderr on a fresh `daemon.log`.
  This rename-and-reopen behavior also works on Windows, so new output does
  not remain in `.1`. With `log_keep = 0`, no history is kept: when the live
  log exceeds its size cap, Unix unlinks it and reopens stdio, while Windows
  truncates a regular live log in place. Symlink paths are unlinked rather
  than followed. Setting `daemon.log_max_bytes = 0` disables size-triggered
  rotation.
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

Upgrade rehearsals build genuine historical schema prefixes 1, 3, 17, 23,
38 and 46 from pinned migration SQL, seed only columns present at that
checkpoint, and upgrade to the build's current schema. They check retained
rows, memory/search/Sessions-summary/usage backfills, and the old-version
recovery snapshot. A whole-profile rehearsal also preserves an old-format
config, private fallback key and registered artifact while the upgraded
daemon answers Status. Reopening an upgraded profile needs no new snapshot.

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
  data folder's total. The report reuses measurements where path and category
  rules agree. Concurrent changes can affect counts within one report.
- **Clean-up.** `Gc {dry_run}` (`formless gc [--dry-run]`, TUI `/gc` then
  `/gc run`, web Clean up… which shows the dry run before "Free"):
  artifacts past their retention, `/rewind` payload beyond `run_files.*`
  limits (paths remain listed; active/latest Runs are protected), MCP logs cut to their last 1 MB, all but the two newest
  pre-migration copies by their recorded timestamp (not schema version),
  and the database's free pages (VACUUM). Each row
  says how many items and bytes; a real run is audit-logged.
- **Backup verification and drills.** `formless backup list` lists daily,
  migration and pre-restore database snapshots with sizes and verification age;
  `backup list --json` returns stable machine-readable rows.
  `backup verify FILE` checks an immutable private copy: SQLite integrity,
  foreign keys, table/column inventory for its stamped schema, and row counts.
  A private atomic receipt binds verification to the exact source bytes; changed
  files are never shown as verified. Verification is structural, not proof of
  profile identity or that external files were recovered. Snapshots contain only
  the database, not artifacts, skills, configuration, credentials or keyring data.
  `backup restore FILE --dry-run` prints the exclusive-ownership, safety-copy,
  staged migration and atomic replacement plan without touching the profile.
  `backup restore FILE` uses the existing stopped-daemon restore path and its
  seven bounded `backups/formless.db.pre-restore-*` recovery points.
  Existing `before-restore-*.db` copies remain recognized and share that bound;
  no duplicate safety copy is made.
  `doctor --restore-drill` (or `backup drill FILE`) restores a private throwaway
  copy, migrates it, constructs an offline daemon and lists Sessions. It never
  serves, loads live configuration/credentials, starts providers, scheduled jobs,
  hooks, MCP or background tasks. `daemon.backup_restore_drill = true` opts into
  a daily drill after a successful automatic backup; it is off by default.
  Doctor reports newest-backup age and exact-byte verification status per profile.
  Pre-migration retention keeps two newest copies and any receipt-pinned recovery
  point, and prunes only after successfully verifying the newest copy; actual
  removals are logged and audited. Failed verification preserves older copies.
- **Backups.** The daemon copies the database (`VACUUM INTO`, consistent
  under WAL) to `<data>/backups/formless-<UTC time>.db` when the newest is
  a day old (checked hourly), keeping `daemon.backups_keep` (7; 0 = none).
  `formless backup` / `Backup` / TUI `/backup` take one now. These are
  **database-only** snapshots: copy `artifacts/` and `skills/` separately if they
  are needed. `config.toml`, credential files and OS keyring entries are excluded.
  The database can still contain sensitive conversation and Tool data. A separate
  live folder copy is not a coordinated database/filesystem snapshot. Recorded
  paths are not rebased: moving the files to a different profile or machine does
  not automatically repair absolute references. `formless
  restore FILE` (daemon stopped) checks the file (SQLite, integrity
  check, has Formless tables, schema not newer than this build), saves
  the current database as `backups/formless.db.pre-restore-<stamp>`, stages and migrates
  the selected snapshot in the same directory, fsyncs it, and atomically replaces
  the canonical database while holding exclusive daemon ownership. Current WAL
  data is checkpointed before old sidecars are removed. Failed staging or migration
  leaves the canonical database unchanged. Restore warns that open TUIs/login
  services may restart the daemon after the operation. Pre-restore safety copies
  have their own retention of seven and are listed by storage/doctor independently
  of daily backups. Backup and pre-restore filenames include fixed-width
  fractional UTC seconds and a unique suffix, preserving distinct restore
  points even within the same second; existing backup destinations are
  never replaced. A failed staged migration identifies any safety copy and leaves
  the old canonical database available; the safety copy remains independently usable.
  After replacement, while ownership is still held, restore checks artifact rows
  and **all** skill versions (including old, disabled and retired versions).
  It reports missing/invalid files or skill directories/`SKILL.md`, unreadable
  local references, and foreign/unresolved paths outside the destination's
  `artifacts/` or `skills/` roots separately; counts can overlap. A skill version's
  directory and `SKILL.md` are resolved independently: either escape marks that
  version foreign once. Foreign file contents are not opened. Nonzero counts produce explicit warnings; a failed
  diagnostic says recovery is unverified without misreporting a committed restore
  as rolled back. Rows are preserved: the check never runs deleting artifact GC,
  rewrites paths, copies files or certifies their contents/coherence.
  `formless doctor` shows the last backup's age.
- **Opening a stored output:** the transcript note reads `[full output (N
  bytes) stored as artifact ID; read it with read_artifact]`, with no
  filesystem path, because transcripts also reach remote web clients.
  `GetArtifact {id, offset?, length?}` returns a text slice (200 000
  characters by default) with `total_chars` and `end`, never the path.
  Both client and agent readers open only regular files beneath this profile's
  artifact directory using rooted no-follow handles, reject changed recorded
  sizes, and read at most 32 MiB plus one growth-detection byte before slicing
  or grep. Larger artifacts return an explicit text-reader limit error.
  Client ID access remains profile-wide; agent `read_artifact` still requires
  exact ownership by its current Session. A restored registry path alone cannot
  authorize a host-file read.
  - TUI: a tool result that references an artifact shows `/open <id>`,
    and `/open` (the newest) or `/open <id-prefix>` shows a full-screen
    pager (↑↓, PgUp/PgDn, g/G, Esc). In text pagers, `/` or Ctrl+F edits a
    search query and Enter jumps to the first matching line. `n` and `N`
    cycle through matching lines. Enter and Shift+Enter also cycle while
    the submitted query is shown. Esc while editing restores the previous
    query. Esc after editing closes the pager.
  - Web: the result gets an "Open full output" button that opens a viewer
    with "Load more" and Download, which fetches the rest and saves it as
    a file.

### 9.7 Scheduled jobs

A job is a prompt that runs later or periodically: re-check a build,
refresh a cache, nightly cleanup.

- **Creating:**
  - The agent uses `schedule_run({prompt, cron? | at?, session_id?})`.
    `cron` is a 5-field expression in local time (`*`, lists, ranges,
    `/step`; month/weekday fields also accept case-insensitive three-letter
    names such as `JAN-MAR` and `MON,WED,FRI`, including in ranges/lists/steps;
    Sunday is `SUN`, 0 or 7). Aliases are `@hourly`, `@daily`/`@midnight`,
    `@weekly`, `@monthly`, and `@yearly`/`@annually` (January 1 at midnight).
    Day fields follow Cronie: if either starts with `*` (including `*/N`
    or `*,…`), both parsed day sets must match; otherwise either may match.
    A later wildcard (`1,*`) or an explicit full range does not set that
    leading-wildcard flag. For example, `0 9 */2 * 1` runs on Mondays
    with odd day-of-month numbers, rather than every 48 hours. Invalid field
    values and range endpoints identify their field in both entry points.
    A repeated local minute during a clock change can match at both actual
    instants. Cron skips local minutes that the clock passes over.
    `at` is RFC 3339, `YYYY-MM-DD HH:MM` or `in 30m|2h|1d`.
  - The user uses `AddJob` or `formless jobs add PROMPT --every … |
    --cron … | --at …`. A user's schedule may be plain English: "every
    weekday at 9am", "every 2 hours", "every 15 minutes", "daily at
    18:30", "hourly", "every monday and friday at noon", "every weekend
    at 8pm", "every monday morning" (09:00), and
    "yearly"/"annually"/"every year at 9am" (January 1).
    Clock suffixes accept `am`/`pm`, `a.m.`/`p.m.` and `am.`/`pm.`,
    case-insensitively; a dotted minute separator such as `9.30am` remains
    unsupported. Natural schedules are stored as cron. Minute and hour
    intervals must divide 60 minutes or 24 hours evenly. Use explicit cron
    to schedule uneven gaps at particular clock times.
    `PreviewSchedule {schedule}` returns the cron and up to three projected
    recurring times from the daemon's current local time. `jobs add` prints
    them after creation. `formless jobs preview SCHEDULE [--json]` requests
    the same preview without creating or updating a Job. An empty preview
    means no upcoming times were returned. Adding a Job performs its own
    validation.
  - Both entry points require a nonblank prompt and a one-shot time strictly
    after now. Relative offsets use checked arithmetic and may be at most
    366 days; zero, past and overflowing times return errors without saving
    a job. Explicit immediate firing uses `RunJobNow` / `jobs run ID`.
  - A pinned `session_id` must name an existing, non-deleted Session. The
    agent may use `current`; that resolves to its current Session and is
    validated again when the job is saved after any approval wait.
- **Trust:** one-shot jobs from the agent are scheduled at once.
  Recurring ones need the user's approval: asked immediately, or, with
  nobody connected, saved as "awaiting approval" until the user enables
  them. Enabling a job approves it. Enabling/approving a disabled recurring
  job recomputes its next future cron slot; time spent disabled is not missed
  downtime. An expired or completed disabled one-shot cannot be enabled:
  the error names its expired state and the explicit run-now alternative.
  Re-enabling an already enabled/approved job is idempotent, preserving
  ordinary daemon-downtime catch-up. Cron creation uses the time after any
  approval wait when choosing its first slot.
- **Running:** the daemon checks every 20 s. A due job sends its prompt to
  its Session (queued if that Session is busy), or to a new Session named
  `Job: …` only when no target was pinned. A deleted/missing pinned target
  pauses the job, preserves its due slot and target, and records a failed
  history row, `job.target_missing` audit and client note; firing it again
  never silently creates a replacement Session. A one-shot job then ends
  after successful prompt delivery; a recurring one moves to its next
  match after now. Firings missed while the daemon was down or the
  machine slept run once, and its history says "ran late, N firings
  coalesced". An invalid stored cron or an uncomputable next occurrence
  pauses before prompt delivery with an explicit `paused:` last status,
  failed history row, `job.reschedule_failed` audit and client note. A prompt
  delivery error similarly pauses with `job.delivery_failed`, retaining the
  original due slot even for a one-shot; it is not recorded as completed.
  Repair the cause, then explicitly run it or re-enable a recurring job.
  Run completion updates history but cannot erase a paused scheduler error.
  Everything is audit-logged and broadcasts `JobsChanged`.
- **Overlap:** each job has `overlap`: `skip` (default) skips a firing
  while the previous one is still queued or running; `queue` queues it.
  Either way a Session never holds more than 5 queued messages: further
  firings are skipped. Skips are recorded in the history and audit-logged
  `job.skip`.
- **History:** every firing is a `job_runs` row: trigger (`schedule` or
  `manual`), outcome (the Run's final status, or `skipped`), duration,
  cost, Session and details. `JobRuns {id}`, `formless jobs history ID`,
  `/jobs history ID` and the web "History" button show it. The CLI shows the
  newest 20 firings by default. `--limit N` accepts 1–500, and the human
  output warns when that many rows were returned. JSON remains an array of
  firings in newest-first order. A firing whose Run doesn't complete posts
  a note to every client.
- **Run now:** `RunJobNow {id}` (`formless jobs run ID`, `/jobs run ID`,
  web "Run now") fires a job at once under the same overlap rules; its
  schedule is unchanged. A successful manual retry records its outcome in
  history; it does not resume a paused schedule or clear its pause reason.
- **Delivery:** `deliver` (`AddJob`, `SetJobOptions`; `jobs add --notify
  --to-file PATH --webhook URL`) sends the answer when the firing's Run
  ends: a desktop notification, a Markdown entry appended to a file,
  and/or a JSON POST (`job_id, prompt, status, answer, session_id`).
  Desktop delivery runs off the daemon executor and gives the notifier
  five seconds to submit the message. On Windows, Jobs and Tool notifications
  use the BurntToast module in Windows PowerShell under an interactive login.
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
capability = "net"              # optional: a Capability family or scope
run_tool = "network_policy"     # a registry Tool, or…
# command = ["/usr/local/bin/check"]   # …a command (no shell)
# trusted = true                # run the command without asking
# timeout_secs = 30
```

- **Input:** a hook gets the event as JSON: event, tool, arguments, the
  Tool's Capabilities, Session and Run. post_tool adds the result
  (clipped to 4000 characters) and `ok`; run_end has the Run's `status`.
  A Tool hook gets it as input, and a command gets it on stdin.
  For Tool calls, the `capabilities` payload and filter use scopes bound to the
  call arguments. A binding failure keeps the declared list for the hook.
  A family such as `net` matches the whole family. A scoped filter uses the
  same host, port, wildcard and path-component rules as a Capability grant.
  After a command exits, Formless gives its output pipes a short grace
  period to drain and then stops any remaining processes in the hook's
  process group. A hook that reaches its timeout also stops the group.
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
  reconnects as on first launch. At the provider step, Esc cancels a
  re-opened setup and returns to the Session without changing config,
  credentials or the daemon connection. An empty key preserves a stored
  credential only when the endpoint is unchanged; changing endpoints
  without a new key requires confirmation before clearing the previous
  stored credential. Enter at an empty key step can reuse the stored
  credential for fetching/probing only when the endpoint matches the
  on-disk provider; it is never reused for a changed endpoint.
- **Accessible first-time setup.** `formless setup` is a numbered, line-mode
  alternative: choose a provider preset, enter an API key without echo, pick a
  listed model or type its ID, and see the endpoint probe verdict. It saves the
  provider and entered key only after a successful probe. Plain mode, `chat`,
  and headless `run` name this command when setup is missing; their guidance
  also names `FORMLESS_BASE_URL`, `FORMLESS_MODEL`, and `FORMLESS_API_KEY` as
  the environment-based path for headless Runs without setup.
- **Moving around.** Esc goes back one step at every step (effort →
  model → key → URL → provider), keeping what was typed; the header shows
  "step N of 5". Going back invalidates outstanding model-list/probe
  results. Only the current operation at its expected step can advance
  setup. Changing provider or custom endpoint clears the preceding
  provider's key and model selection.
- **The API key** shows as `••••abcd` (only the last 4 characters);
  Ctrl+R shows it in full for 10 seconds at the key step only; Enter and
  step changes hide it. Short keys show at most half their characters
  as a verifiable tail. Surrounding Unicode whitespace is trimmed before
  requests and saving; internal whitespace produces a warning.
  Pasting works (bracketed paste). Ctrl-key presses never type letters.
- **Resuming.** After 300 ms without an edit the wizard atomically keeps
  a private (`0600` on Unix) draft in
  `<config>/setup-draft.json`: the step, preset, base URL, model and
  effort, never the key. URLs with userinfo, queries or fragments are not
  drafted. Drafts expire after 24 hours and are discarded when config is
  newer. The next launch resumes there (at the key step for providers
  that need one); successful config saves, including CLI setup, delete it.
- **Local discovery.** With no saved setup or provider key in the
  environment, a background worker checks the local model-list APIs on
  `127.0.0.1` and `::1`. First paint does not wait for network I/O. Ollama's
  `/api/tags` and the OpenAI-compatible `/v1/models` shape identify a model
  endpoint; an open TCP port alone makes no product claim. Discovery uses
  at most four bounded GETs (350ms each, 1MiB body cap), without credentials,
  proxies, redirects, model loads or downloads. The offer arrives only while
  the untouched provider picker remains current, and selection keeps the
  detected address, including IPv6. All detected servers remain available;
  when several answer, the status names each with its model count and each
  matching preset row is marked found. The initial highlight prefers an
  endpoint with models, while choosing another detected preset uses its own
  discovered address. Wrapped status text is included in the picker layout.
  Empty Ollama offers say to run
  `ollama pull MODEL`; other empty model APIs say to load a model first.
  `OLLAMA_HOST` selects Ollama's local discovery address/port/path instead of
  the default Ollama candidates. Schemeless addresses default to 11434;
  explicit HTTP/HTTPS default to 80/443, matching Ollama. `localhost` maps to
  literal loopbacks; unspecified bind addresses map to their loopback family.
  Remote hosts, credentials, queries/fragments and invalid forms are not
  probed; setup gives manual-provider guidance without echoing the value.
  LM-compatible default candidates remain within the same four-GET budget.
- **The first approval** (TUI and web) adds a short explainer: Tools have
  no access by default, what the Capabilities are, Always vs Once vs this
  Session vs Deny, and `/approvals` / `/revoke` to undo. It is shown until
  a client confirms the first ordinary approval answer. The daemon persists
  the acknowledgement per profile and shares it with connected clients.
  Promotion answers do not dismiss this help.
- **A guided first task.** Once setup finishes, a note offers `/tour`
  (and `/tour skip`, which hides the offer for good); it is offered once.
  `/tour` opens a labelled Guided tour Session and puts the sample task in
  its composer. Previous Session drafts stay available. Progress follows the
  exact Run started by sending the sample task and resumes when the user
  returns to the tour Session. When the agent successfully uses the sample
  Tool, the tour points to the Tools view and `/help`. Other outcomes have
  separate notes. Repeat tours reuse the labelled sample Tool. `/tour cleanup`
  archives guided Sessions and retires the unmodified, unpinned sample.

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
  (`tui.hyperlinks`). HTTP(S) schemes are matched case-insensitively;
  targets lowercase only the scheme while visible text and the remaining
  URL keep their original spelling. A URL ends where its brackets balance
  (`…/f_(x))` keeps one `)`), and a long URL hard-wrapped across rows is
  one link on every fragment, never a link to its first piece. In the
  Markdown, an image inside a link shows both addresses, and inline HTML
  shows its text: `<b>`, `<i>`, `<s>`, `<code>`, `<kbd>` and `<a href>`
  style it, other tags and comments are dropped. Anchor attribute names
  are ASCII case-insensitive, allow whitespace around `=`, and retain
  quoted or unquoted destinations. `/open link [N]` opens the Nth link in the last
  answer with the system opener (`open`, `xdg-open`, `start`).
  File paths in the chat (`src/main.rs`, `./a/b.txt:42`, `Cargo.toml`)
  that exist under the TUI's working directory are linked the same way,
  to `file://`. Existence checks are cached. Native Windows drive colons
  are preserved when parsing a trailing `:LINE[:COLUMN]` suffix.
  `/open path FILE[:LINE]`
  opens one in `$VISUAL`/`$EDITOR` at that line: `+LINE` for terminal
  editors (which get the terminal until they exit), `FILE:LINE` for
  Helix/Sublime/Zed, `-g FILE:LINE` for VS Code and its forks,
  `-l LINE FILE` for TextMate, `+LINE FILE` for gedit, and
  `--line LINE FILE` for Kate.
- **Inline images (TUI).** Image attachments the TUI can read get an
  8-row thumbnail under their message. After each draw, thumbnails whose
  rows are all on screen are sent with the terminal's own protocol, which
  scales the image into the box: iTerm2's inline images (iTerm2,
  WezTerm) or the kitty graphics protocol (kitty, Ghostty), which takes
  PNG. Both protocols decode PNG, JPEG, GIF or WebP and downscale oversized
  images to at most 800 px on the long side, preserving aspect ratio and
  PNG transparency. Oversized previews and non-PNG kitty inputs are encoded
  as PNG; small supported iTerm images and small PNGs keep their original
  bytes. Resized animated images show a still preview. Rows are reserved
  only for a supported image header;
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
  labelled self-loop shows `↺ label`. Leading YAML frontmatter is removed
  before parsing any supported diagram; its root scalar or literal/folded
  block title is a heading (frontmatter takes precedence over an inline pie
  or sequence title). Configuration/style metadata, tags and aliases are not
  evaluated by the native renderer. Unclosed frontmatter and overwide titles
  fall back to source. Decoded title controls are shown as visible escapes
  before width measurement. Sequence participant `box` groups close silently;
  nested loop/alt/opt/par/critical/break/rect blocks retain their own dividers.
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
  languages are plain text in the default colour. In both clients, Python
  hash comments need no preceding whitespace; YAML/TOML/env values retain
  semicolons, while INI/conf/properties keep semicolon comments.
  Each block's top border names its language and its index in the
  message (`╭─ rust [2]`), which is the number `/copy code 2` and
  `/pager code 2` use. `tui.code_line_numbers` adds dim line numbers.
  Code copy and paging use the same parser options and source order as chat,
  including tilde fences, indented blocks, longer and unclosed fences, and
  Mermaid among other code blocks. Commands target the last nonblank
  assistant answer, default to its last block, and use one-based indices.
  The body has Markdown containers/indentation interpreted by the parser;
  copying removes exactly one final parser newline, as chat presentation
  does, and preserves other whitespace. Diagram-command numbering is
  separate and unchanged.
- **Tables in narrow terminals (TUI).** Markdown tables wider than the
  pane shrink: columns narrower than an equal share keep their width, and
  the rest share the room in proportion. Cell text wraps inside its
  column instead of being cut. When the pane can't give every column 4
  characters, the table is shown as one `key: value` record per row,
  each value wrapping under itself with a hanging indent. Hard-wrapped
  text keeps grapheme clusters whole; a cluster wider than its row budget
  is represented by `…`, and a zero-column hard-wrap budget emits no rows.
  `/table [N]` opens the last answer's Nth table at full width in the
  pager, where ←/→ scroll sideways.
  Selection uses parsed table order within the last nonblank assistant
  answer, defaults to table 1, accepts optional outer pipes, and excludes
  pipe-like prose or code. Parsed cells, alignment and inline formatting
  survive enclosing lists/quotes; the full-width pager omits those containers.
- **Tool calls in the TUI chat.** Persisted calls start collapsed to one
  line: `▸ ⚙ name ✓`, an input summary, and the first line of the result
  (✗ on failure, … while there is none). Their separate result lines are
  folded in. `[`/`]` on an empty input focus the previous/next call and
  scroll it into view. `[`/`]` also stop on messages with text (yours
  and the answers). While a call is focused, Enter expands or collapses
  its preview, F6 opens its full stored input and available result, and
  `z`/`Z` expand or collapse all calls. Esc closes the pager and returns
  to the same call. On a focused message or
  call, `c` copies its text and `C` copies it as Markdown (a call as its
  name, input and result in fences). Esc or typing leaves the focus.
  `/copy N` copies the Nth message from the bottom.
  On a focused message of yours, `e` loads it into the input to edit and
  re-run from there (`EditMessage`). If later user turns would be
  discarded, the first Enter asks and a second confirms, and Esc cancels
  the edit. `f` forks a new Session with everything before that message
  (`ForkSession up_to_seq`) and puts the message in its input. An expanded call shows its input and its result or error,
  pretty-printed and highlighted when JSON (input and ordinary result
  previews are capped). The full call pager preserves the stored text and
  shows control characters as visible escapes.
- **Vim editing (TUI, #626).** `tui.vim_mode = true` (default false), or
  `/vim [on|off]`, enables modal editing in the composer and internal Tool,
  module, schema, description and run-input editors. The setting persists;
  list-view keys and default readline editing are unchanged. Buffers start in
  Insert, with `INSERT` / `NORMAL` shown in their border and `?` help in Normal.
  Insert Esc only enters Normal, preserving text and any active Run. Normal
  composer Esc retains Run-stop/cancel behavior; editor Esc always keeps text,
  with Ctrl+S save and explicit Ctrl+Q leave shown in the footer. Leaving a
  modified editor asks once; y discards and closes, n or Esc keeps the full
  buffer and undo history. Non-Vim editor Esc uses the same confirmation.
  Supported Normal commands: h/j/k/l, w/b/e and W/B/E, 0/$, gg/G; d/c/y with
  those motions, dd/cc/yy, iw/aw/iW/aW, single/double/backtick quotes and nested
  ()/[]/{}/<> objects; i/a/I/A/o/O, x, bare u for undo, p/P and dot repeat.
  Word/character and operator counts are bounded to 999. Bare G targets the
  last line; explicit G/gg counts select a one-based absolute line, clamped to
  the last line. A $ count reaches the end of that many lines including the
  current line. Counts also apply to i/a/I/A/o/O,
  p/P and dot: insert edits repeat on Esc (open-line commands also repeat
  their new lines), puts repeat the register, and an explicit dot count
  replaces the prior change's count. Counted changes insert replacement text
  once after changing their counted range. Linewise deletes reaching the
  final line remove its preceding separator, leave the cursor on the first
  nonblank of the new last line, and retain only the deleted line text in
  the linewise yank register. Change and yank ranges keep their separators.
  Yank stays in an internal register;
  no OS clipboard is read or written. Dot repeats the last editing command
  and its unmodified Insert keys/paste; movement and yanks do not replace it.
  Readline control shortcuts in Insert break the current repeat group.
  Large collapsed pastes expand before modal editing to preserve their actual
  contents. No Visual mode, Ex commands, macros, syntax-aware objects or full
  Vim emulation is promised. Semantics follow the official
  [motion](https://vimhelp.org/motion.txt.html),
  [change](https://vimhelp.org/change.txt.html) and
  [repeat](https://vimhelp.org/repeat.txt.html) help (including cw's trailing
  whitespace exception and grapheme-safe editing).
- **Input editor (TUI).** The message box is a multi-line editor with
  readline keys:
  - ←/→ move by character and Alt or Ctrl+←/→ by word. Home/End and
    Ctrl+A/E go to the line start and end; End on an empty input jumps
    the chat to the bottom.
  - Ctrl+W (Alt or Ctrl+Backspace) deletes a word back and Alt+D a word
    forward. Ctrl+U and Ctrl+K delete to the start and end of the line.
  - Shift+Enter, where the terminal reports it, or `\` + Enter inserts a
    newline. The box grows to 8 rows.
  - In the non-Vim composer, ↑/↓ move between visual rows, including soft
    wraps and explicit newlines, keeping a preferred display column across
    shorter rows. Shift+↑/↓ extend selection across the same rows. Width comes
    from the drawn composer after its prompt/border; resizing resets the
    preferred-column basis. Vim and shared Tool-editor movement retain their
    existing logical-line behavior. On the first or last visual row, ordinary
    non-Vim arrows walk the
    current Session's earlier messages, and Down past the newest returns
    the draft. Ctrl+R searches that history. History entries are borrowed from
    the selected Session; typing/searching does not copy every message's text
    or allocate a lowercased copy of each entry. The search preview reads one
    matched entry and bounds its text before flattening and measuring it.
    Reloads, reconnects and stashed-draft restoration invalidate old history
    indices while preserving the draft, cursor and search query. An invalidated
    match is blank until another explicit search step; Enter cancels that search
    without submitting the draft or recalling a different shifted message.
    Out-of-range Up/Down/search indices cancel safely instead of indexing blindly.
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
  (macOS file names are NFD). Native fuzzy matching, snippets and TUI Find
  also equate German `ß`/`ẞ` with `ss`, preserving each source glyph once
  in highlights. Query and candidate use the same folding. Fuzzy match positions count original
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
  Each bypassed approval decision is audit-logged as `tool.run_unasked`, and so
  is a hook command's allow decision (`hook.run_unasked`). These records name
  the Session and available Run; they do not assert that execution started or
  succeeded. Remembered denials (below) apply in
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
- Interrupting an approval pre-pass closes its open prompt and stops the
  batch before any call or pre-tool hook is dispatched. It neither denies
  the remaining calls nor remembers a denial. Answer resolution and
  interruption have one winner: an acknowledged answer is applied exactly
  once (including lasting grants), while a losing answer receives
  `resolved: false` and emits no answered event. `ApprovalClosed {approval_id}`
  dismisses a cancelled prompt without attributing a denial to the user.
  An already-dispatched memory confirmation's acknowledged approve-to-keep
  decision completes its bounded atomic memory write; interruption stops
  subsequent batch work. Clients distinguish submission from confirmation
  and do not claim that an unacknowledged decision was applied.
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
  - **Matching:** `api.example.com` matches only that host;
    `*.github.com` matches proper subdomains and excludes `github.com` itself.
    Explicit port restrictions are preserved. Look-alikes (`evilgithub.com`)
    don't match. Settings and `/security` show offline PSL/wildcard warnings
    for configured trust, including PRIVATE public suffixes such as `github.io`.
  - **Never trusted:** bare `net` (any host), a grant wider than the
    trusted host (`net:example.com` against `api.example.com`), `*` and a
    bare TLD. Like trusted folders, it is set in `config.toml` only.
- **Path scope.** Each new `fs:read:`/`fs:write:` path on a prompt can
  be granted wider: the path itself (default), its folder, or the
  repository it lies in (the nearest folder holding `.git` (a folder or
  a file, so worktrees count), `.hg`, `.jj`, `.svn`, `.pijul`, `_darcs`,
  `.fslckout` or `_FOSSIL_`), else the project (`Cargo.toml`, `package.json`,
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
    never offered or accepted as wider scopes. Home comes from the OS
    directory API, with canonical forms compared on both sides, including
    on Windows without `HOME`. Parent-traversing home paths are unverifiable,
    even if native canonicalisation normalizes them to an existing directory.
    If home cannot be resolved safely, wider
    choices are omitted and widening is refused. Anything else is refused: the exact path
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
  they cover (a denied `net:example.com` covers that exact host,
  `net:*.example.com` its proper subdomains, and bare `net` all hosts) fails without a prompt, logged `tool.deny_remembered`,
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
  - **What's covered:** ordinary reads stay quiet; credential reads require
    separate exact-path consent (§7.4). A call whose only
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
- **Naming cache.** `formless naming status` lists each cached model and
  its file sizes, marking the configured model. `formless naming remove
  [MODEL]` removes weights, the tokenizer and partial downloads for one
  model. `formless naming remove --all` removes those files for every
  supported naming model. Removal respects the same lock as downloads and
  model loading. If `naming.auto_download` is enabled, a future naming attempt
  can fetch the configured model again.
- **Title cleanup.** Model and fallback heuristic titles strip balanced
  quote/backtick/emphasis wrappers and a leading Markdown heading marker only
  when followed by whitespace. Unpaired punctuation is retained: `C#`,
  `#1234`, an inline code span at the end, and `'til` remain ordinary text.
  Existing first-line, word/grapheme budgets and trailing sentence-punctuation
  cleanup still apply.
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
- **Token and cost chart.** `UsageByDay {since, by, utc_offset_minutes}` sums
  provider token use (prompt, completion, cached, cache write) per UTC day when
  the offset is omitted, or shifted by the supplied caller UTC offset in minutes
  (`-1439..=1439`; invalid values are rejected), or with `by` = "model",
  "role" or "session" per that key over the range, priced with
  `provider.prices` (no cost when a model in the bucket has no price). Session
  rows carry the stable `session_id` and display the Session name in `key`.
  They use the ID as the label when the Session record is missing. The
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
  invocations and success rate. Its table includes all Tools independently of
  the Tools filter, ordered by invocation count in the current range (stable
  daemon order for ties). Selection follows Tool identity across range/order
  changes, clamps when removed and scrolls into view. Enter loads the highlighted
  Tool's existing all-time detail; unrelated cached detail stays hidden behind
  an activation hint. Mouse rows use the same identity/order. General paging
  keys and visible-capacity movement follow the list-key rules below.
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
  views, `/` filters identifiers fuzzily (Tool/Session name, Tool title
  and tags, or Proposal kind). Descriptions and Proposal content/rationale
  require a contiguous match under the shared accent-insensitive fold.
  Query edges are trimmed; inner whitespace is literal in prose, while
  identifier fuzzy matching ignores query whitespace. Primary names/kind
  rank above secondary fields, then prose; contiguous identifiers rank
  above scattered matches, and earlier prose matches rank first. The selected
  sort breaks equal relevance inside existing Tool groups and pinned Session
  ordering. Empty queries and queries beginning with `#` keep their selected
  sort; the Tools/Session `#tag` rules are unchanged. Quick filters, collapsed groups and parent-before-child Session
  trees still apply. Enter keeps the filter and Esc clears it. `s` cycles
  the tie-break sort and `v` the quick filters:
  - Tools sort by name, success rate, last used or calls; show all,
    active, retired, failing (<80%) or needing attention.
  - Sessions sort by last activity, created or name; show all or running.
  - Proposals sort newest first or by score; show all, pending, applied
    or rejected.
  The panel title shows the match count and the active sort, filter and
  query (`Tools · 12/240 · sort: relevance (ties: calls) · failing · /csv`), and the
  selection follows the visible rows. The web Tools and Proposals views
  have the same filter box, sort and show menus; web Sessions use the
  sidebar search.
  The web Tools table headers (Tool, Ver, Calls, Success, Last used)
  sort by that column on click or Enter; a second click reverses it. Each
  column's natural order is names A→Z and numbers or dates biggest first.
  The headers carry `aria-sort`, and the Sort menu follows.
  Tool usage time is the start of the latest retained Invocation, regardless
  of outcome. Web Tools and Metrics show that time as of the latest Tool
  refresh, and the TUI shows it in Tool details. An absent time is unknown,
  including when all call history was pruned or the daemon predates the field.
  The last-used sort puts unknown times after known ones and breaks ties by
  name and Tool ID.
- **Settings view:** edits file-backed config (§13) and per-Session
  model/effort. Safely hot-reloadable values apply immediately; data-dir /
  transport-class changes state “restart required”. Includes the full-reset
  action (§15).
  - Reasoning-effort choices in terminal Settings and the web Role editor
    include default/unset, minimal, low, medium and high. Editing only a Role's
    description preserves its stored effort; explicitly selecting default clears it.
  - No settable key needs a daemon restart: the daemon re-reads the file,
    and running TUIs reload their `tui.*` display settings on
    `SettingsChanged` (theme, colours, line numbers, images, panes,
    timestamps, hyperlinks).
  - Only `config::ON_TUI_START` (`tui.on_start`, `tui.screen_reader`)
    apply when the TUI next starts. `GetSettings` lists them as
    `on_tui_start`, and `SetSetting` answers `applies: "now" |
    "on_tui_start" | "env_override"`. The last means the file was saved but
    the daemon's environment still controls that key; `override_source`
    names the variable and `effective_value` gives the runtime value.
    The web marks TUI-start rows and, after saving one, says
    to restart any running TUI.
  - Web configuration rows are generated from documented current values,
    grouped by their key prefix. The daemon projects only existing SETTABLE
    keys; credentials and file-only controls remain excluded or read-only.
    Booleans toggle, choices use a select, and numeric/text/list/JSON edits
    use SetSetting with typed values. Empty input restores the default;
    numeric lists and maps use JSON. The payload's values, choices and
    read_only inventories are additive; older summaries remain readable but
    need a daemon update for the full editable inventory.
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
  - the selected row wraps its SPEC §13 description, units and default,
    and states whether the typed loaded configuration matches that default
    (including configuration environment overrides, but independent of the
    running daemon's approval-mode display override). `?` opens the complete entry
    in the scrollable text viewer for long descriptions or small terminals;
  - enabling `naming.auto_download` requires a supported catalogue model.
    Enter/Space arms consent and a separate `y` confirms it; Escape cancels.
    Repeated Enter/Space presses do not confirm. Without a supported model,
    the Web and TUI explain that downloads are unavailable and do not save
    the setting as enabled;
  - every settable key has an editor or a read-only pointer to config.toml,
    including keys in whole settable sections; optional keys remain visible
    when unset. File-only hooks, security and MCP areas show compact counts,
    never hook bodies, credentials or custom secret patterns;
  - read-only rows (endpoint, auth, relay, paths) say where they change.
  Changes go through `SetSetting`, so they are audited and other clients
  get `SettingsChanged`. A daemon refusal also shows under the row.
  Providers and Roles sections list them with add/edit forms (the API key
  is masked and goes to the keychain through `UpsertProvider`) and `d`
  twice to remove. "config.toml" opens the file in `$VISUAL`/`$EDITOR`.
  Editing a Role preserves its advanced candidates, system prompt, reasoning
  effort and turn cap. The form shows a bounded read-only summary of those
  fields and points to config.toml for editing them; system content is summarized
  by presence/length. Visible-field edits round-trip the existing Role object.

The setup wizard identifies its active profile in the header; the normal TUI
status line identifies non-default profiles. If the active profile is
unconfigured, setup, headless Run errors and doctor diagnostics point to another
profile with saved provider configuration and show its exact launch command plus
`formless profiles list`. Alternate discovery reads configuration files only,
without loading credentials or probing providers. Wizard hints are captured once
when opened, rather than rescanning profiles during rendering.

### External composition in the TUI

Ctrl+G opens the current composer, Session `/instructions` buffer or built-in
Tool/module editor in `$VISUAL`, then `$EDITOR` (default vi, or Notepad on
Windows). `/editor` opens the current editing surface too. In history search,
Ctrl+G retains its existing cancel-search meaning. Other modals keep their keys.
Collapsed pastes expand in the editor copy without changing the original buffer.
Code uses `.js`, structured Tool inputs/schemas `.json`, other drafts `.md`.

Editor settings are argument vectors with quoted arguments, not implicit shell
programs: Unix shell-style quoting, Windows double-quote/backslash command-line
quoting. No variable, tilde, wildcard or pipeline expansion occurs. An explicit
configured shell is the owner's command. Known GUI editors use a wait option for
composition (Code-family --wait, Sublime/Mate/Zed/Gedit --wait, Kate --block,
GVim/MacVim --nofork). The captured 1-based cursor line uses the same
editor-specific location arguments as `/open path`, independently of the wait
option. With no line or an unknown editor, the file path is passed unchanged.
Other GUI commands must themselves wait until this file
closes; detaching file-open launchers are refused. Ordinary `/open path` keeps
its existing nonblocking GUI behavior.

The editor gets a private random temporary directory and a new file with Unix
mode 0600 (directory 0700); Windows creates protected current-user/SYSTEM
DACLs using the daemon's existing owner-only descriptor, also tightened on returned
regular files only after checking the link count is one and the owner matches
the originally created private file. Both are removed on success, cancellation and error,
including editor backup files. Reads are bounded to 4 MiB and reject symlink,
reparse-point, directory and other nonregular replacements; an editor's atomic
regular-file replacement is supported. Only a successful editor exit can load
UTF-8 text back (line endings normalize to the TUI newline convention). A nonzero exit, failed start/read or oversized result leaves the
original buffer, cursor, collapsed pastes, Vim mode and attachments untouched,
with a Note. An unchanged result preserves them too. A changed result replaces
only that editing buffer and places its cursor at the end; it does not send a
message, save a Tool, or submit instructions. Existing Enter/Ctrl+S validation
and consent still apply. Stale Tool previews are invalidated; captured review
base versions are retained.

An empty result for a nonempty buffer shows a dedicated confirmation: y clears,
n/Esc/Ctrl+C keep the original. Enter alone does not clear. Confirmation binds
the edit generation, Session/Tool identity and exact original buffer/cursor;
changed targets cannot be overwritten. Temporary files are already removed
while confirmation waits. During a synchronous handoff only the parent catches
INT/QUIT (Windows Ctrl+C/Break); child exec keeps its ordinary interrupt behavior.
The exact prior parent signal policy is restored after terminal restoration,
including failed spawn or unwind. Other signals retain their normal behavior.
The handoff is synchronous: daemon messages queue while the editor owns the
terminal. It invalidates pending forecast consent before waiting, so a queued
reply cannot send the unchanged old draft on return. Superseded Tool-input
opening replies are ignored. Changed Tool buffers remove pending old preview
requests before the event pump resumes. Queued Session deletion stashes the
edited draft with its original Session; an empty-confirmation target is checked
again after queued events have been processed. No live-state freshness is
claimed while the editor is open.
Both config-file and temporary-buffer handoffs reuse
the same terminal suspension/restoration and force-redraw-from-scratch path.

### 10.5 Input and keys

The composer and internal Tool editor retain bounded local undo snapshots of
text, cursor, selection and collapsed-paste backing text. Consecutive typing
coalesces until navigation or another command. Undo/redo retain at most 128
snapshots and 16 MiB of payload; the live draft is never truncated. A snapshot
larger than that bound creates an explicit history boundary. The private kill
ring retains at most 16 entries and 1 MiB, without splitting Unicode text.
These buffers belong to the editing surface, never the daemon or transcript.
`Ctrl+_` (also `Ctrl+/`) undoes; `Ctrl+Shift+_` or `Alt+/` redoes.
`Ctrl+U/K/W` and `Alt+D` save deleted text privately; consecutive kills join
in original text order. `Ctrl+Y` yanks and `Alt+Y` cycles only immediately
following a yank. Shift+arrows/Home/End select grapheme-safe text, expanding
paste placeholders first; selections render reversed. `Ctrl+Shift+C/X`
explicitly copy/cut that exact selection through the existing OSC 52 path.
Mouse click places the draft cursor and drag selects visible grapheme cells in
both editors; scrolling remains separate. Selection never copies implicitly.
Tool editor replacement through Ctrl+G or hunk review is one undo step;
undo, redo and replacements invalidate prior asynchronous editor previews.

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
- **Search-hit navigation (TUI).** Enter on a cross-Session search hit opens
  its stored message after the matching Session load, placing it in view once.
  Navigation is owned by that Session, message ID and load request; an older
  response cannot jump to a different selected hit. An absent ID may fall back
  only to a unique sequence/role match within that loaded Session. A grouped
  Tool result shows its call header as nearest visible context with a note.
  A missing/unrenderable target or failed/timed-out load follows the bottom
  with a note. Session/view changes and manual scrolling abandon a pending
  jump; End resumes bottom-follow as usual. Chat-find highlighting does not
  choose the destination.
- **Local prompt commands.** Plain Markdown templates in the selected profile's
  `<config>/commands/NAME.md` and selected Session working directory's
  `.formless/commands/NAME.md` define `/NAME [ARGUMENTS]`. Discovery records the
  highest required single-digit position and whether `$ARGUMENTS` occurs, using
  the same token rules as expansion. TUI/plain help, the TUI picker and CLI
  metadata listings show a supplied `argument-hint` or derive
  `<arg1> … <argN>` and `[arguments…]` from the placeholders.
  `commands list --json` exposes `argument_hint`, `max_position`, and
  `uses_arguments`. Names are 1–64 lowercase
  ASCII letters/digits/underscores/hyphens, starting with a letter. Optional
  frontmatter requires a nonempty `description` string (plain, quoted or
  block text) and accepts an optional plain or quoted `argument-hint` string.
  Each is limited to 512 bytes and screened for single-line display.
  Other well-formed string keys produce a warning and have no effect on
  template expansion or permissions. Keys may have leading spaces or tabs.
  Block content must be
  indented more deeply than its key, so sibling keys remain subject to validation.
  Malformed templates are excluded with a stable diagnostic, never executed.
  Valid project templates override global templates. Built-in slash roots always win, including their subcommands; `/help` groups custom commands
  under **Yours** and identifies shadowed files.
  The shared Core loader supplies the TUI palette/help, plain-client expansion/help and
  `formless commands list [--json]`. JSON contains command metadata, diagnostics
  and shadowed-file metadata, never template bodies or expanded arguments.
  CLI discovery resolves paths read-only before profile/daemon setup and never
  creates a profile, starts a daemon, invokes a provider or reads `@path` targets.
  Human-readable file labels escape terminal controls and directional formatting;
  JSON retains the original metadata path strings.
  Plain mode uses the current Session working directory and preserves built-in
  precedence, including its `/exit` and `/switch` aliases. Expanded bodies are
  sent as literal message text and never redispatched as slash commands; `//`
  remains the raw slash-text escape. Invalid-template invocations report the
  current filename and reason even after a diagnostic note was dismissed.
  Suggestions include loaded and shadowed template names, with shadowing reasons.
  A diagnostic key is forgotten when absent from a refreshed snapshot, allowing
  a newly broken file to be announced again after repair.
  TUI discovery loads once when the profile/Session/working-directory binding
  changes. Ordinary idle frames do not read template files or poll their metadata.
  While a non-literal slash palette is open, it checks bounded directory/file
  metadata at most every 250 ms and reloads only when that fingerprint changes
  or cannot be obtained. Help and explicit expansion always force a fresh Core
  load, regardless of the preview-cache hint. Diagnostic notes are deduplicated
  by the current snapshot's bounded path/reason keys. Roots may resolve trusted aliases;
  command-directory descendants and files use native no-follow reads and refuse
  reparse points, symlinks, hardlinked files and unsupported namespaces. Each
  directory is bounded to 256 entries and 128 Markdown templates; files are at
  most 64 KiB, combined template bytes 1 MiB, and diagnostics at most 64.
  Description frontmatter permits indented comment lines and trailing
  whitespace-separated `#` comments outside quoted values. Literal hashes in
  quoted or block scalar content and URL fragments remain description text;
  malformed quotes, unsupported fields and duplicate descriptions still fail.
  Expansion is a single literal pass: `$ARGUMENTS` preserves the text after the
  invocation's first separator only when no ASCII letter, digit or underscore
  immediately follows its name; `$ARGUMENTS_PATH` and `$ARGUMENTS2` remain
  literal, while `$ARGUMENTS.` still expands. `$1` through `$9` select Unicode-whitespace
  separated positions. `$$` emits one literal `$`, so `$$1`, `$$5` and
  `$$9.99` preserve shell examples or prices. Escaped dollars and inserted
  arguments are not re-expanded. Missing referenced positions stop expansion
  before send with the template filename, position and `$$N` escape guidance;
  they never silently become empty strings. `$0` and multi-digit `$10`/`$100`
  remain literal text. Quotes, environment variables, shell syntax, backticks
  and substituted text are never evaluated or recursively expanded. Arguments
  are bounded to 64 KiB and expanded text to 1 MiB. `@path` remains ordinary
  text until the existing attachment intake and protected-path checks on send.
  Expanded input carries literal-message intent before forecast confirmation,
  queueing, steering or dispatch: even a body beginning `/quit`, `/run` or an MCP
  slash command cannot invoke that command. Draft/history persistence, reconnect,
  Session switching and editing undo retain that intent. Templates cannot execute
  anything on discovery or expansion; explicit send follows normal message and
  attachment handling.
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
  TUI model/wizard queries, list filters and the Session switcher share
  Ctrl+U clear and Ctrl+W/Alt+Backspace previous-word deletion. Only unmodified
  or Shift-modified characters enter queries; other Ctrl/Alt character chords
  are ignored except for the picker's explicit actions. Ctrl+S/Ctrl+V do not
  cycle list filters. Model, switcher and rollback result pickers support
  PgUp/PgDn and Home/End, clamped to matches, using their rendered row count
  (ten rows before the first draw). Rollback navigation disarms confirmation
  and keeps its selected version visible. Main-list paging follows the
  per-view capacity rules below.
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
- **List keys (TUI).** Sessions, Tools, Metrics, Proposals and Search take
  ↑/↓ and j/k, g/G or Home/End for the first/last row, PgUp/PgDn for a
  page and Ctrl+D/Ctrl+U for half a page. A page uses the latest rendered
  list/table body's selectable capacity, excluding borders, column headers,
  Tool group headers and other split panels. Each view retains its own
  capacity; resize is reflected after redraw. Before the first draw the
  fallback is twenty items (ten for a half-page), clamped to the list.
  Nonempty lists move by at least one item. While a list filter is being
  edited, PgUp/PgDn are consumed without moving results, Ctrl+D is ignored,
  Ctrl+U clears the query and printable characters remain query text. Other
  modal/editor/pager handlers retain priority. Search Enter still opens the
  selected message and Esc returns to Chat. In Proposals, `n` generates a
  new proposal (it was `g`).
- **Mouse (TUI).** While mouse capture is on: clicking a tab switches
  views, clicking a row in the Sessions, Tools or Proposals list selects
  it and a double-click opens it (as Enter), and clicking a tool call's
  header in the chat expands or collapses it. Each frame records its
  clickable regions, list rows from the widgets' own scroll offsets.
  The wheel over an active Sessions, Tools or Proposals list moves the
  selection by up to three filtered items without activating a row.

- **Plain mode for screen readers.** `formless --plain` (or
  `tui.screen_reader = true`) runs a line-oriented client instead of the
  full-screen TUI: no alternate screen, redraws, spinners or box drawing.
  When no provider is configured, it exits non-zero and points to
  `formless setup` (or the active profile's equivalent) and the documented
  environment variables for headless Runs.
  - Each event prints once, as a sentence, in order: the streamed answer
    after "Formless:", "Tool started: name.", "Run paused: why. Next: what
    to do.", and notes.
  - An approval becomes a prompt that lists each capability with its
    plain-language meaning; the next line answers it (y, o or n).
    Questions list numbered options and are answered the same way, one at
    a time.
  - Commands: `/new`, `/sessions`, `/switch N`, `/stop`, `/files [RUN]`,
    `/rewind [PATH]`, `/recap`, `/docs [topic]`, `/help`, `/quit` (alias
    `/exit`). `/files` shows tracked changes, `/rewind` attempts restoration,
    `/recap` shows the recorded recap, and `/docs` prints guide addresses.
    `/quit` and `/exit` leave Runs going in the daemon. Ordinary text is a
    message (a steer while a Run is active); unknown slash commands are
    reported locally. Begin with `//` to send a literal leading slash.
    The client opens the most recent Session. See [the plain-client
    reference](docs/plain.md) for launch forms and current command usage.
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
    $0.021`. The TUI uses one compact count rule for prompt/completion/cache
    writes in that status and both `/usage` notes: k, M, then B, with promotion
    at 999,950 and 999,950,000 so the lower one-decimal unit never reaches
    1000.0. B remains the highest unit for larger totals; counts below 1,000,
    including zero/negative inputs, retain their existing exact spelling.
    Locale-aware full counts and recorded accounting values stay separate.
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
    imported prices older than 90 days. Import compares all four rates,
    ignoring a timestamp-only change, and shows old/new values for updates.
    A changed record replaces the full listed record: an absent cache rate
    is shown as unset and uses the input rate locally, rather than zero.
    `--dry-run` reports the same changes without saving price configuration.
    Usage rows record their provider,
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
  estimates the current rebuilt prompt for a Session on demand using the
  persisted calibration. It reports `measured: false`: a previous measured
  prompt is not substituted for changed history, schemas or images. `/context` (or clicking the segment on
  the web) adds an estimated breakdown: system prompt, Tool schemas,
  history, and the number of attachments.
  All preflight counts share one Unicode/JSON estimator (including Tool-call
  metadata). Images use their dimensions and detail, with a nonzero fallback
  when dimensions are unknown; encoded image bytes are never counted as text.
  Counts are approximate, not a tokenizer or a forecast. Each configured
  provider/endpoint and serving model has a persisted calibration ratio:
  observed prompt tokens divided by the estimate of that exact dispatched
  request, exponentially smoothed at 0.2 and clamped to 0.25–8. Missing/zero
  prompt usage and rejected requests do not train it. Repeated cumulative
  stream snapshots train once per HTTP attempt, including fallback models.
  Compaction, context reports and routing apply that same ratio; calibration
  does not require a provider request or credential refresh.
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
  In tmux, enable `set -g focus-events on` so the TUI receives focus
  reports from a compatible outer terminal. For the desktop escape
  sequence, also enable `set -g allow-passthrough on` on tmux versions
  with that option. The `on` setting forwards sequences from visible
  panes. Reattach tmux clients after changing focus reporting.

  The web tab title also counts unseen alerts (`(2) Formless`) until you
  come back. Interrupted Runs never notify.

### 10.8 Language

- Web dates and numbers use the active UI language through one formatter:
  English uses `en-US`, Thai uses `th-TH`, with the Gregorian calendar and
  Latin digits pinned explicitly to match TUI/ISO years. Currency remains USD
  and uses that locale's currency conventions. The document's `lang` follows
  the effective UI language, including Auto and live settings changes. Machine
  values (JSON, identifiers, form values and CSS coordinates) remain unlocalized.

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
  Capabilities in plain words, task-oriented MCP/Skills/Jobs how-to guides,
  the main keys and every command, troubleshooting, and Docker. The `mcp`,
  `skills` and `jobs` topics open `docs/mcp.md`, `docs/skills.md` and
  `docs/jobs.md`; each links to its full specification section. `/docs [topic]` (TUI and web) and `formless docs [topic]
  [--print|--text]` access one (`gallery`, `capabilities`, `approvals`, `keys`,
  `troubleshooting`, `docker`, `mcp`, `skills`, `jobs`, `spec`; a unique prefix works;
  none opens the gallery). `formless doctor` prints the troubleshooting link, and
  `/help` points to `/docs`.
- `docs/keys.md` is generated from the same key and command tables `/help`
  shows (`formless docs --keys`); a test fails when it is stale, and when
  a web key it lists is gone from the web UI.

Terminal docs commands default to the gallery. Exact topic names win;
otherwise a unique prefix is required. Unknown names and ambiguous prefixes
are reported explicitly (CLI usage exit 2 with available topics; TUI/plain
notes name the error and candidates). CLI help examples are shell-tokenized
and checked against the real parser without dispatch. `formless docs TOPIC
--text` prints the bundled Markdown exactly, without config/profile, daemon,
network or browser access. All topics have embedded content; `spec` is the
full bundled SPEC and anchored topics select a top-level Markdown heading's
subtree through the next same-or-higher-level heading. A Markdown parser
supplies byte offsets and rendered heading labels, ignoring code/quoted
examples; missing or duplicate bundled anchors are explicit errors.

Desktop defaults still open the current online copy. A failed or unavailable
opener (including SSH/headless hosts) falls back to bundled text in the CLI
or a TUI pager, retaining a separately labeled current online URL that may
differ from the installed build. An opener must exit successfully within two
seconds and is reaped; success cannot prove browser navigation. Terminal
`/docs --text TOPIC` (or `TOPIC --text`) explicitly reads the bundle in the
TUI pager or plain output; plain mode's default retains its online address.
The web topic table includes Docker and continues to open online pages.
`--print` keeps bare online-URL output and conflicts with `--text`; `--keys`
conflicts with topic, `--print` and `--text`. These CLI modes and topic errors
need no profile/config or daemon. `--text` is version-matched to the compiled
source; online URLs remain on `main`.

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

Release metadata fetched by `formless update` has a 5-second connect and
15-second total deadline, including the response body. Installer curl attempts
have a 10-second connect and 120-second total transfer deadline, retaining
three transient-error retries. Timeout errors suggest retrying, pinning
`FORMLESS_VERSION` for the installer lookup, or a trusted installer mirror;
failed metadata bodies are never treated as successful release lookups.

The macOS/Linux installer reports the next commands (`formless` for setup,
`formless doctor` for verification) and the need for a provider API key or a
local model endpoint. After either a source or prebuilt install, it checks
whether the install directory is a complete PATH entry and, when missing,
prints a quoted, shell-appropriate PATH command and its startup-file location.
It does not edit shell startup files. Unsupported prebuilt platforms name
the source-install alternative and its Rust/cargo and C compiler prerequisites.

The Windows executable reserves 8 MiB of virtual address space for its main
thread, committed on demand, to accommodate the finite generated CLI command
builder and normal command work. It keeps TUI/clipboard operations on the
original main thread. Verification hash buffers live on the heap; correctness
checks run through actual CLI child processes without test-stack overrides.


### Session selectors

CLI Session lookup resolves an exact ID first, then a unique ID prefix.
If no ID prefix matches, it looks for a unique ASCII-case-insensitive
Session name. Unknown or ambiguous selectors return an error;
`formless sessions list` shows the stored Sessions.

### Root command inventory

The following table is synchronized with the visible Clap command definitions
in `main.rs`, `cli_attach.rs`, and `cli_commands.rs`. The hidden packaging
command `man` is described separately below. Omitting the root command starts
the TUI. Command-specific arguments and defaults are available through
`formless COMMAND --help` or `formless help COMMAND SUBCOMMAND`.

| Command | Actions / useful options | Purpose |
|---|---|---|
| `formless approvals` | `pending`, `approve`, `deny`, `list`, `revoke`, `denials` | Inspect pending requests, answer one, or inspect and manage existing grants/denials. |
| `formless artifacts` | `show ID [--offset CHARS] [--length CHARS] [--json]` | Read decoded artifact text in Unicode scalar slices; not a byte-exact download. |
| `formless auth` | `login`, `status`, `logout` | Manage provider credentials (API key or OAuth). |
| `formless backup` | No action: create; `list`, `verify`, `restore`, `drill` | Create, inspect, verify, restore, or rehearse database snapshots. |
| `formless bug-report` | `-o FILE` / `--output FILE` | Write a redacted Markdown diagnostic report. |
| `formless chat` | `--session ID` or `--new` | Line-mode chat with streamed answers and inline questions. |
| `formless commands` | `list` | List local user-defined slash-command metadata without starting a daemon. |
| `formless completions` | `bash`, `zsh`, `fish`, `powershell`, `elvish` | Print a shell completion script. |
| `formless config` | `path`, `list`, `get`, `set`, `edit` | Locate, inspect, validate, or edit configuration. |
| `formless daemon` | `start`, `stop`, `restart`, `status`, `serve`, `install-service`, `uninstall-service` | Manage the daemon and its optional login service. |
| `formless docs` | Optional topic; `--print`, `--keys` | Open a guide (gallery by default), print its URL, or print the embedded keyboard reference. |
| `formless doctor` | `--json`, `--fix`, `--restore-drill` | Diagnose setup, apply safe fixes, or rehearse the newest backup. |
| `formless export-all` | `--sessions`, `-o FILE` / `--output FILE`, `--overwrite` | Write a portable setup bundle, optionally including Session transcripts. To replace a file, pass `--overwrite` with `--output`. |
| `formless gc` | `--dry-run`, `--json` | Free reclaimable storage or preview what would be freed. |
| `formless help` | Optional command path, such as `sessions show` | Print help for the CLI or a nested command. |
| `formless import-all` | `FILE` | Add a portable setup bundle without overwriting existing entries. |
| `formless jobs` | `list`, `add`, `run`, `history ID [--limit N]`, `enable`, `disable`, `rm` | Manage scheduled prompts and inspect their history. |
| `formless logs` | `--follow`, `--level`, `--since`, `--lines`, `--jsonl` | Read or follow the daemon log and rotated files. |
| `formless mcp` | `add`, `remove`, `list`, `test`, `login`, `logout`, `catalog`, `import`, `log`, `tools`, `tool`, `prompts`, `prompt` | Manage external MCP servers and inspect their tools/logs. |
| `formless metrics` | `--since DURATION`, `--json` | Read recorded model, Tool and built-in statistics through independent reports at one cutoff. |
| `formless memory` | `list`, `add`, `rm`, `edit`, `used` | Inspect or change saved user-memory notes. |
| `formless models` | `list [--json]` | Fetch the default-provider model catalog and existing picker report; a non-null listing error prints a partial report and exits 1. |
| `formless modules` | `list`, `show`; `show --version N` | Read all modules, inspect metadata/history/dependents, or emit an exact stored source version. |
| `formless naming` | `status`, `pull`, `test` | Inspect, install, or preview the embedded Session-naming model. |
| `formless prices` | `import`, `list` | Import a model-price catalog or inspect prices and configured model references offline. |
| `formless profiles` | `list`, `rm` | List or remove isolated profiles. |
| `formless prompts` | `list`, `diff`, `generate`, `apply`, `reject`, `rollback` | Inspect existing proposal comparisons and manage measured prompt/memory proposals. |
| `formless providers` | `list`, `add`, `remove`, `test` | Manage named model providers and test a connection. |
| `formless relay` | `enable`, `disable`, `status` | Manage encrypted browser access through the relay. |
| `formless reset` | `--force` | Wipe configuration, credentials, and the current profile's data. |
| `formless restore` | `FILE` | Replace the database from a backup, preserving the current database. |
| `formless roles` | `list`, `add`, `rm` | Map named purposes to a provider and model. |
| `formless run` | `TASK`; `--estimate`, `--session`, `--output`, `--attach` and other Run options | Run one headless task, or print its local forecast without starting it. |
| `formless routing` | `--json` | Read currently available daemon routing decisions and returned candidate figures without starting a model request. |
| `formless runs` | No action: list; `show`, `cancel`, `running` | List recent Runs, inspect/cancel an exact Run, or read a live Invocation snapshot. |
| `formless security` | `status` | Inspect the daemon's grants, denials, trusted resources, and exposed interfaces. |
| `formless sessions` | `subagents`, `subagent`, `fork`, `compare`, `list`, `show`, `usage`, `files`, `rewind`, `rename`, `rm`, `restore`, `import`, `search`, `templates list/show`, `archive`, `unarchive`, `pin`, `unpin`, `tag`, `export` | Manage stored Sessions, follow a Run with `show --follow`, or inspect tracked files. `list --pinned --tag TAG` filters the fetched reply; selectors combine with `--archived`, preserving reply order. |
| `formless setup` | No arguments | Configure a first provider in line mode for screen readers; choose a preset and model, enter a hidden API key, then verify the endpoint. |
| `formless skills` | `reviews`, `add`, `remove`, `list`, `enable`, `disable`, `show`, `export`, `import` | Inspect and manage Skills and their retained description reviews. |
| `formless storage` | `--json` | Report disk usage by data category. |
| `formless tools` | `reviews`, `list`, `disable`, `enable`, `undo`, `pin`, `unpin`, `tag`, `show`, `diff`, `retire`, `restore`, `rollback`, `purge`, `run`, `export`, `import` | Inspect, manage, run, or transfer registry Tools. |
| `formless tui` | `--plain`, `-c` / `--continue`, `--new` | Launch the full-screen terminal client; also the default with no command. |
| `formless uninstall` | `--keep-data`, `--keep-binary`, `--yes` (alias `--force`) | Remove Formless, optionally retaining data or the binary. |
| `formless update` | No action, `check`, or `--check`; `--json` | Show newer release notes and the applicable upgrade command; never rewrite the binary. |
| `formless usage` | `--by GROUP`, `--since DURATION`, `--json` | Read full-range grouped tokens and currently computed cost estimates. Human daily rows show the applied UTC offset and mark a truncated cutoff day. |
| `formless web` | `--port`, `--no-open`, `--stop` | Open, print a link for, or stop the local browser UI. |

`providers test NAME [--model MODEL] [--json]` lists the available model IDs
or reports a model probe. JSON emits one object with the known listing or
probe fields, including recorded usage when present. A completed model probe
prints its report before returning exit 0 for success or 1 for a negative
verdict. An empty model list still exits 0. Usage errors exit 2 and other
request failures exit 1.

`formless prices list [--provider NAME] [--json]` reads the configuration file
and defaults. Rates are USD per million tokens. It shows stored entries and
the effective stored-price match for each configured provider model, fallback
model, and explicit Role model. It skips automatic routing and leaves model
references as configured. Environment overrides and historical usage are
outside this view. A provider's reported cost takes precedence over the
stored fallback estimate. Use `--provider default` to select `[provider]`.

`mcp test NAME [--json]` restarts the server and prints the full report before
returning its verdict: exit 0 for a successful reply (including zero tools),
exit 1 for an embedded restart error. JSON preserves the reply as one value
without an extra generic error line for that reported failure. Request failures
keep their existing error mapping; unexpected replies are errors. `--json` does
not make the restart read-only. `mcp add` retains its existing behavior.

`gc --json` prints the complete single daemon/offline result, including its
`dry_run`, integer `total_bytes`, `kinds` and any extra details. Use
`gc --dry-run --json` to preview without cleanup. Counts are core-reported values,
not a promise of exact physical space reclaimed. JSON changes only presentation;
retention, side effects, errors and human output remain unchanged.

`sessions pin ID` and `sessions unpin ID` display the stored pin state returned
by the daemon. `sessions tag ID` only reads tags; adding positional tags replaces
the **entire** stored tag list, and `--clear` explicitly replaces it with an empty
list (conflicting with positional tags). Tags reach storage unchanged for its
existing normalization; confirmation shows returned stored values. These three
actions accept `--json` for the selected/returned Session object.

Ambiguous Session prefixes/names remain usage errors and display up to ten
matching full IDs, bounded names and archive labels. Extra matches are counted,
with commands for both archive states. Prefix errors suggest extending the ID;
name errors suggest a displayed ID. Exact-ID priority, prefix-before-name
resolution and trimmed ASCII-case-insensitive full-name matching are unchanged.

Human Session lists label the last recorded Run status and show stored tags,
retaining the pin star and existing message preview/count. Empty replies are
reported separately from an empty archive selection, which names the alternate
list command and count. Static human `sessions show` includes stored archived,
pinned, tags and last recorded Run status metadata; absent status is "not
recorded". JSON and `--follow` retain their existing contracts.

`formless sessions show ID --last N` displays the newest N stored messages
in their original order, including blank entries in the selection before
human formatting. N must be positive. When earlier messages are omitted,
the human display reports the selected and total counts and `--json` adds
`total_messages` next to the shortened `messages` array. The bound applies
to display after fetching the full snapshot and conflicts with `--follow`.

`formless metrics --since 7d --json` reads the three existing metrics reports
with one checked UTC cutoff. Omit `--since` for all history. Tool rows use stored
`tool_id` values; reports retain their own row order and are independent reads,
not a transactional snapshot. Runs and daily usage remain separate surfaces.

`formless sessions list --pinned --tag WORK --json` selects active pinned Sessions
whose stored tags contain exactly `work`. Only the tag argument is lowercased;
a leading `#` is not stripped. Add `--archived` to select the archived partition.
These selectors filter the existing reply on the client.

`formless memory used (--session SESSION | --run RUN_ID) [--json]` reports
current note fields associated with the selected Run, in daemon order.
Session selectors use normal exact-id/prefix/name resolution and select its
latest Run, including one still running; Run ids are sent unchanged without
existence validation. Retained associations may outlive a Run, while backend
lookup/association failures can produce empty or partial reports. These are
reported current notes, not verified historical prompt contents. Disabled
notes remain present; JSON is the full returned array. Other daemon errors
retain ordinary exit behavior without a partial report.

```sh
formless memory used --session SESSION --json
formless memory used --run exact-run-id
```

`formless sessions subagents SESSION [--json]` inventories stored sub-agents
using normal Session exact-id/prefix/name resolution. Full sub-agent ids can
be passed unchanged to `formless sessions subagent SUBAGENT_ID [--json]`,
which uses exact ids with no prefix lookup. Inventory order and null versus
empty results are retained. Human detail labels the record and pretty-prints
the entire returned provider-shaped messages array; JSON preserves the full
daemon report. This is the existing unpaged transcript projection, whose
handler skips malformed stored JSON rows, with no extra integrity check,
Run start or Session mutation. Request failures produce no partial report.

```sh
formless sessions subagents SESSION --json
formless sessions subagent exact-subagent-id
```

`update --json`, `update check --json`, and `update --check --json` emit one
object: `{installed, status, newer_releases: [{tag, url, notes}]}`. Notes retain
the existing trimmed body and oldest-first order; `installed` has no `v` prefix.
The lookup follows GitHub release pages within one 15-second timeout and
excludes drafts and prereleases. `update_available` means at least one newer
stable release was found; `up_to_date` means a completed lookup found none.
`release_check_unavailable` maps to an actual HTTP 404 response,
not verified absence of releases. Error text alone never selects this status.
Completed checks exit 0; other fetch or JSON parse failures remain errors with
no success JSON. The human output format stays unchanged, and the JSON form
never runs an updater.

### Root and global flags

| Flag | Short | Availability | Meaning |
|---|---|---|---|
| `--profile` | — | Global | Select isolated profile `NAME`; separate daemon and data directory. |
| `--secure` | — | Global | Ask for approval before sensitive Tool calls; also `security.secure = true`. |
| `--plain` | — | Root/TUI startup | Use line-oriented, screen-reader-friendly output without redraws, spinners, or borders. |
| `--continue` | `-c` | Root/TUI startup | Open the most recently active Session; conflicts with `--new`. |
| `--new` | — | Root/TUI startup | Start a fresh Session; conflicts with `--continue`. |
| `--help` | `-h` | Root; also available on subcommands | Print help for the selected command. |
| `--version` | `-V` | Root | Print the executable version. |

Only `--profile` and `--secure` are inherited global options. The root-only
`--plain`, `--continue`/`-c`, and `--new` options apply to TUI startup; place
them before `tui`, or omit the command.
`run --continue`/`run -c` is a distinct command option that selects the
previous headless Run's Session.

### Command behavior


- `formless` (no args) launches the TUI (auto-starting the daemon).
- CLI lists (`tools`, `sessions`, `providers`, `roles`, `jobs`, `mcp`,
  `skills`, `prompts`) share one table renderer: aligned columns with a
  dim header, secondary fields dimmed, status in colour, and the widest
  column cut with "…" to fit the terminal. Colour and fitting happen only
  when stdout is a terminal, and `NO_COLOR` turns colour off. Piped output
  is plain aligned text. Headers and cells collapse whitespace to one space
  before width measurement; other C0/C1 controls and bidi direction marks/overrides/
  isolates are shown as Unicode escapes in both terminal and piped tables.
  A cell cannot add rows or terminal styles; the renderer's own colour styles
  are applied afterward. Printable Unicode (including emoji joiners) is retained.
  Structured `--json` values are unchanged by table formatting.
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
  Fish completions also suggest `docs` topics while the optional topic
  argument is unfilled; a supplied topic stops those suggestions. `--print`
  and `--text` may accompany a topic, but `--keys` may not.
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
  that JSON, and the last 200 daemon log lines across live and numeric
  rotations, for attaching to an issue. The log tail reads at most 1 MiB;
  the leading boundary line is conservatively omitted and a notice explains
  byte-budget or line truncation.
- Both are redacted: bearer tokens, `key`/`token`/`secret`/`password`
  values, `#k=`/`#local=` fragments, URL credentials, `sk-`/`ghp_`-style
  keys, long opaque tokens, and the home path (shown as `~`). The file says
  to read it before attaching it.
- `formless logs [-f | --jsonl] [--level warn] [--since 10m] [-n 200]` shows the
  daemon log: rotated files oldest first, then `daemon.log`. Entries are
  grouped (continuation lines stay with their entry), filtered by level
  (that level and worse) and age, and show the last `-n` (0 = all). `-f`
  keeps printing appended entries and restarts from byte zero when it observes
  the live file shrink after rotation. Numeric rotations
  are discovered from the directory (including old suffixes above 64),
  rather than a fixed filename range. Bounded tails read newest first until
  enough matching entries are found; `-n 0` streams all history oldest first.
  Lines and grouped entries are capped at 64 KiB with visible truncation
  notices; damaged UTF-8 is replaced rather than dropping an entire file.
  Read failures are reported instead of silently hiding history. The daemon logs
  `info` and above by default (other commands only errors), `RUST_LOG`
  overrides it, and colour codes are only used on a terminal, never in the
  file.
  `--jsonl` is snapshot-only and conflicts with `--follow`. It emits one JSON
  object per selected entry with `timestamp` (RFC 3339 or null),
  `filter_level` (the lowercase effective filter level, defaulting to
  `info` for missing or unknown levels), and `text` (the same grouped,
  normalized and possibly truncated entry as in raw output). `--since`
  excludes undated entries. No matches leave stdout empty and print the
  usual diagnostic on stderr.
- `formless config`:
  - `path` prints the config file.
  - `list [--filter TEXT] [--json]` reads documented settings from the local
    profile and environment, with descriptions, defaults and settable status.
    The optional filter matches dotted key names. The result marks changed
    values and distinguishes null, empty and missing values from defaults
    explained only in documentation. Credentials and other sensitive values
    are excluded or masked. The command works before a daemon starts.
  - `get KEY` prints the effective value (after `FORMLESS_*` overrides);
    an unknown key exits 2.
  - `set KEY VALUE` goes through the settings API, so it's validated against
    the allow-list, audited and hot. VALUE is JSON when it parses (`5`,
    `true`, `["a"]`), else text. The six exposed maps (`provider.prices`,
    `provider.model_context_tokens`, `provider.model_audio_input`,
    `provider.model_pdf_input`, `provider.model_meta`, `tui.colors`) use `MergeSetting` by default:
    supplied entries replace their whole value, null entries are removed,
    and siblings survive. `--replace` uses `SetSetting` for whole-map
    replacement; `--replace '{}'` clears. Lists remain replace-only.
  - `edit` opens a copy in `VISUAL` or `EDITOR`. On Unix the shell parses
    the command. On Windows the same argument parser as the TUI handles
    double-quoted executable paths and arguments. It replaces config.toml only when the copy
    parses. Otherwise it shows the error and, on a terminal, offers to edit
    again; the file is never left half-written.
- `formless sessions` manages Sessions with the same protocol requests as
  the TUI and web: `list [--archived] [--json]`, `show ID [--json]`, `rename
  ID NAME`, `rm ID` (restorable for `daemon.delete_undo_secs`; it prints the
  restore command), `restore ID`, `archive`/`unarchive ID`, `export ID
  [--format json|markdown|html] [-o FILE]` (`md` aliases `markdown`),
  `import FILE`, and `search QUERY
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
  - `json`: one `{session_id, status, answer, result, error}` object at the end;
    `answer` retains its text form. `result` carries the validated JSON value
    on a successful schema Run and is null otherwise;
  - `jsonl-events`: every event, one per line; `--json` is the same for a Run.
  - With `--estimate`, `text` prints the human forecast and `json` prints one
    forecast object. `--json` is an alias for the forecast object in this
    mode. `final` and `jsonl-events` are usage errors for estimates.

  Pipeline options are per Run, never edits to the profile configuration or
  Session's standing instructions. `--system TEXT` appends exact bounded
  instructions to this Run's projected system prompt, including after context
  rebuilds. `--max-usd N` accepts a finite nonnegative USD limit: it replaces
  the global Run limit for this Run only (including an intentional increase),
  while the daily limit remains in force. Zero authorizes no paid model
  dispatch. Every physical model request, including retries, nested/background
  calls, compaction and output repair, uses the same Run's spending ledger;
  an in-flight request can exceed the remaining headroom before its actual
  usage is reported. Run-initiated compaction, approval summaries and automatic
  skill distillation retain the originating Run's policy through their physical
  request/accounting lifetime, including after the main Run finishes. Spawned
  maintenance also retains the existing routing Task as resource-only cost:
  it cannot change the responsible model, quality result or logical response
  clock, and final Task cost waits for those owners to settle. Ordinary Run
  maintenance retains its existing usage-ledger attribution. An explicit limit
  refuses unknown/unpriced work rather than silently claiming it was bounded. A budget stop exits 1 and reports
  observed spend, with unknown usage/pricing distinguished, on stderr.

  Existing `--model`/`--role` and `--effort` flags on a pipeline Run are
  carried in the same immutable options. The raw selector resolves once during
  admission; dispatch keeps that resolved model/Role and quoted effort. This
  path never changes/restores the Session's model or triggers selection-time
  background model tests, including when `provider.auto_test_models` is on.

  `--schema FILE|-` reads a bounded JSON Schema and compiles it offline before
  admission. Both sources accept one leading UTF-8 BOM; the 64 KiB limit counts
  the original bytes, including the BOM. With `-`, stdin is the schema rather than a task or stdin.txt;
  combining it with task `-` is a usage error. The final answer is validated
  before assistant storage or delivery. Candidate text is not streamed for a
  schema Run. A JSON/schema mismatch allows exactly one paid, no-tools logical
  repair attempt; its physical requests remain charged to this Run. A second
  mismatch exits 3 and never prints the invalid candidate on stdout in final
  or json mode. Refused, incomplete or oversized output is not accepted as a
  valid JSON answer. These options are part of the immutable forecast input
  and consent fingerprint, so a quote for different options cannot authorize
  the Run.

  `--output-file PATH` leaves stdout empty and writes the selected completed
  output through a bounded private temporary file adjacent to PATH, followed
  by an atomic replacement. After writing and flushing the complete result,
  the temporary file takes the destination's Unix access bits and group or
  Windows DACL when a destination exists. New result files stay owner-only.
  Progress and errors remain on stderr. Invalid
  schema output, a failed/paused/interrupted Run, timeout, output-size overflow
  or a write failure never replaces an existing destination with partial
  output. Selected output is limited to 8 MiB. No output destination is opened
  or truncated before completion. Run-only instructions and schemas are retained
  in memory by bounded execution policies and short-lived forecast previews;
  durable policy markers contain only hashes and the numeric cap. Resume
  refuses a stopped pipeline Run before status/message/waiver changes; submit a
  new pipeline Run with its desired options instead of reconstructing them from
  Session defaults.

  A send queued while the previous Run finishes has no future Run identity
  or foreground answer. The CLI reports that it was queued on stderr even
  with `--quiet`, exits 2, and leaves result output files unchanged. JSON
  reports `status: "queued"` with null answer and error; text/final output
  stays empty and JSONL emits no invented Run events. Inspect the Session
  queue or follow its next active Run to observe delivery.

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
  usage/config error, `3` Run paused because schema validation failed (see `--schema` above), `124` Run timed out (see `--timeout` above), `130` interrupted.
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
    offline · retry in Ns"). Select **Retry now** to retry at once. After 3 failed
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
- **Exit codes**, common to commands: 0 success; 1 a failure (the daemon,
  the provider, the Tool); 2 a usage mistake (an unknown id, a missing or
  conflicting flag, a bad value, malformed `--input`, a refused
  confirmation), checked before a daemon is started where the client can
  tell; 130 interrupted. Run-specific extensions: 3 Run paused because schema
  validation failed (see `run --schema` above); 124 Run timed out (see
  `run --timeout` above). Errors print as `error: …`
  on stderr. Flags that would silently override each other are refused by
  the parser (`run --json --output`, `export NAME --all`, `jobs add --cron
  --at`, `web --stop --port`, `approvals revoke … --all`), and the TUI's
  `--plain`/`-c`/`--new` only go with starting the TUI. `relay enable`
  keeps the configured `relay.url` unless `--url` is given. `tools run`
  exits from the daemon's `ok` when present. Supply an inline
  JSON object with `--input JSON` or read one from a UTF-8 regular file or
  stdin with `--input-file FILE|-` (1 MiB maximum). The flags are mutually
  exclusive and the default input is `{}`. Invalid input exits 2 before
  profile setup or daemon dispatch. `tools run --json` prints one complete
  daemon result, including dry-run effects and logs. Request errors go
  to stderr. `formless man` documents every subcommand's options, two levels deep.
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
immediately and only the current last answer has controls. Web Markdown
recognizes HTTP(S) schemes case-insensitively in bare and angle autolinks,
and MAILTO in angle autolinks. URL text and mail-address case are retained;
existing safe-target filtering and trailing-punctuation behavior still apply.
Markdown formatting is limited to 32 nesting levels, 4096 characters per inline parse, and 128 KiB
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

When a Relay browser returns to a visible page or comes back online, it probes
the authenticated encrypted connection immediately and refreshes the selected
Session (or all Sessions) from daemon state after success. While visible, it
sends an encrypted health probe every 10 seconds and replaces an unresponsive
socket after 15 seconds without a response or valid Relay transport progress
(a quiet failure is detected within one probe interval plus that deadline),
without waiting for a close event. The daemon retires a browser channel after
five minutes without browser transport input; the Relay closes that channel
with code 4002. Mobile platforms may suspend a hidden page and its probes long
enough to reach this deadline, so on return the browser rechecks or reopens the
connection and reloads state rather than assuming the subscription survived.
An installed PWA uses the Badging API, when available,
to show the count of pending approvals and clears the badge when none remain.
While watching a running Session, a user may opt in to Keep awake; the screen
wake lock is released when the page is hidden or the Run is no longer active
and reacquired on return if the user opted in. Unsupported or rejected
platform APIs leave the feature inactive and do not retry automatically.

- `formless relay enable|disable|status` controls the hosted Relay: the
  daemon dials out to `relay.url` (default
  `wss://formless.athiwat.xyz`) and browsers meet it there — no inbound
  ports. The Relay (a Cloudflare Worker + Durable Object room in
  `relay/`) forwards only opaque AES-256-GCM ciphertext: the pairing
  secret (32 random bytes, keychain + `0600` file, never in config) is
  the E2E key. Room identity v3 is the first 16 bytes of SHA-256 of a
  separate 32-byte HKDF-SHA256 claim (empty salt, info
  `formless-relay/3 daemon-claim`). The daemon sends this claim as canonical
  lowercase hex in an `Authorization: Bearer` Upgrade header, never the URL.
  The room checks its hash commitment before upgrading/admitting a daemon;
  there is no first-writer verifier registration or unauthenticated fallback.
  The relay sees this separate daemon-slot capability, not the pairing key or
  plaintext; the capability cannot decrypt E2E traffic. A valid restart
  preempts the old slot (close 4003) without rotating the pairing secret.
  Superseded socket attachments lose control/routing authority before close,
  including delayed frames and hibernation; browsers receive offline/online
  in order to start fresh channels. Unauthenticated requests return 401 and
  cannot claim a slot, push, or disconnect clients. The daemon connects with
  `?mux=1`: the Relay gives every browser a client id, announces
  `client_open`/`client_close`, and routes `{cid, f}` frames, so each
  browser has its own daemon connection. Room v3 requires matching Worker,
  browser and daemon code: existing pairing fragments/secrets remain valid,
  but derive a new room. Reload the browser after updating; existing opt-in
  push subscriptions re-register only after a current authenticated handshake,
  never by copying old-room state or requesting new notification permission.
  Hosted deployment is a separate operation. Frames remain version 2: each
  browser connection opens with a handshake (`{"v":2,"hello":<16 random
  bytes>}` from each side) and derives a session key with HKDF-SHA256
  over the pairing key, salted with both nonces. Each side proves it holds
  the key before anything is trusted: the daemon's hello carries
  `confirm` = HKDF(key, salt = both nonces, info `formless-relay/2
  confirm d2b`), which the browser checks before answering with its own
  `{"v":2,"confirm":…}` (info `… b2d`). Only then does the browser count
  as a subscriber or appear in the device list. While a handshake is pending,
  the same hello gets the same answer without resetting its absolute 30 s
  deadline. After readiness, a known hello or confirm is ignored without
  rekeying, replying, or adding another subscriber. A different hello closes
  only that client with 4002 so a fresh WebSocket generation can handshake;
  an established channel is never replaced in place. The browser shows "securing the connection…"
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
  `--rotate` invalidates old links. The matching bounded transport requires
  `transport=1`: opaque 64 KiB fragments preserve the full 32 MiB plaintext
  IPC ceiling without decrypting application payloads at the Relay.
  Before daemon-authorized E2E readiness only 2048-byte handshake/control bodies
  are accepted, with an absolute 30 s handshake deadline. The room admits at
  most 16 CIDs including closing clients and sent-UP orphan receipts. Each
  direction has eight pending permits/deliveries, a per-CID progress reserve,
  and a shared 1 MiB excess-wire pool. Random receiver-only delivery receipts
  are bound to the actual socket and generation; sender permits cannot serve
  as pre-ACKs. Ready controls must be consumed and acknowledged before bulk
  grants; the final fragment is acknowledged only after its consumer releases
  the completed frame. Closing alone does not refund native buffering. The
  compact SQLite metadata ledger and alarms preserve charges, deadlines and
  lifecycle controls through hibernation; ciphertext is never persisted there.
  A daemon `client_close` lifecycle control carries both `cid` and its exact
  generation `g`; stale closes cannot retire a replacement connection. Logical
  cancellation does not free a still-live native socket's slot or queued bytes.
  Browser and daemon endpoint byte leases bound retained serialized producers,
  reassembly and crypto work, with independent small lanes and bounded RPC
  queues/control concurrency. Before allocating an incoming encrypted envelope
  of `E` bytes, Rust reserves `3E + 2 min(32 MiB, 3 ceil(E/4)) + 16 KiB`:
  this covers ciphertext and overlapping old/new escaped-JSON scratch buffers,
  followed by decoding/plaintext handoff. Cipher-envelope validation constructs
  constant errors without formatting attacker field names or wrong-type strings.
  These are application-controlled transport bounds,
  not OS RSS, arbitrary Store/business-query heaps, or undocumented platform
  native-buffer guarantees. Exact limits and residual scope are recorded in
  `relay/transport-design.md`; matching source versions and local end-to-end
  validation are required, and no hosted deployment is implied.
  The hosted Web frontend is a full
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
  words (hourly schedules name the minute without a bare colon, midnight
  aliases match daily labels, and January 1 yearly schedules name the date;
  unsupported shapes retain the cron text), and a target Session; a
  top-bar theme switch, Auto (the system's)
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
  per socket whether it closed or errored. Mobile platforms may suspend a
  hidden page before its application probes run; the browser therefore
  rechecks or reopens its authenticated connection and reloads state when it
  becomes visible or online, rather than promising that a hidden subscription
  stays alive.
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
  Installed browsers that implement Web Share Target can share a title,
  text, URL, image, text file or PDF to Formless. The service worker stores
  the incoming content in the browser's IndexedDB until the client connects.
  Share POSTs reject explicit foreign origins and `Sec-Fetch-Site` values of
  `cross-site` or `same-site`. Same-origin requests and directly user-initiated
  `Sec-Fetch-Site: none` launches can be applied immediately. Ambiguous
  opaque/missing-origin ingress without trusted Fetch Metadata is stored in
  the local inbox and requires the user to choose Add to draft or discard
  before any draft or attachment changes. Native Android share-sheet delivery
  has not been verified; legacy browsers may also require this confirmation.
  The share uses the app's existing launch selection: linked Session, current
  Session, saved per-scope Session, Main Chat, then the first Session. If there
  is no writable selection, it opens or creates Main Chat through the existing
  `get_main_chat` path. The saved selection is the existing `sessionStorageKey`
  preference, not a share-specific choice. A share to a Topic Chat attempt
  opens its writable parent Topic Chat, or Main Chat if no parent is writable.
  An unfinished share records its storage scope and destination Session so a retry cannot redirect
  text or files after the user switches chats. Formless adds text to that
  Session's draft and stages files through the pending-attachment flow; the
  pending attachment inventory restores chips when the Session is reopened.
  Existing draft text is preserved, and nothing is sent until the user submits
  it. The Main Chat shortcut opens Main Chat through the existing route, not a
  newly created regular Session. The manifest also offers an Approvals shortcut
  to the existing tool-grants view, with the platform-supported browser features. These manifest
  features are supported by installed Chromium-based Android browsers;
  Safari on iOS does not implement Web Share Target or manifest shortcuts,
  and unsupported browsers ignore them. The relay's pairing key remains
  governed by the existing "Stay paired after reloading this tab" choice;
  sharing does not persist a new key.
  **Notifications (Web Push, opt-in per device):**
  - Browser permission is requested only by an explicit Settings → This
    browser action, never by sending a chat message; existing granted and
    denied browser decisions are respected.
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
    you". A generic push click focuses an approval from the app-wide approval
    queue, or the app-wide pending question if there is no approval; this
    pending work is not filtered by the selected Session. If the app is closed,
    it opens the saved Session and asks it to focus pending work. Pushes carry
    no Session identifier, so they cannot select a waiting item in another
    Session. Notifications raised by the connected page carry the relevant
    Session locally and open it on click. No Session content reaches the relay
    or the push service.

## 13. Configuration (complete settings table)

- File: `config.toml` in the OS config dir; sections `[daemon] [provider]
  [optimiser] [metrics] [tools]`. The exact config environment overrides are
  `FORMLESS_BASE_URL` → `provider.base_url`, `FORMLESS_MODEL` → `provider.model`,
  `FORMLESS_IDLE_GRACE_SECS` → `daemon.idle_grace_secs`, and `FORMLESS_SECURE`
  → `security.secure`. There is no general `FORMLESS_<SECTION>_<KEY>` rule.
  Endpoint/model values are trimmed; blank values leave the file value intact.
  Idle seconds must parse as a nonnegative whole number in the u64 range;
  whitespace around the number is allowed, but empty, invalid and non-text
  values produce errors naming the variable and expected form. Secure mode
  uses the yes/no grammar documented in §10.3. These overrides apply only to
  the loading process and are never written back by a setting edit.
  Per-Session model/effort live in SQLite, not the file. Nothing configurable
  is a hidden constant: this table is exhaustive for v1.
- **Saving keeps your file (#610).** Every save (a setting, an MCP
  server, a migration) merges into the existing `config.toml`: changed
  values are replaced in place with their comments, new non-default keys
  are added, removed settings are deleted, and keys this build doesn't
  read (a newer build's, your own notes as keys) stay. Writes are atomic:
  a temporary file beside it, flushed to disk, renamed over it, so a crash
  leaves the old file or the new one. The daemon logs unknown configuration
  keys at startup, `doctor` reports them, and `config edit` warns before
  saving; close matches include a suggestion from this section's key set.
- **Changing settings from a client:** clients never write the file.
  `SetSetting {key, value}` replaces one value. `MergeSetting {key, value}`
  accepts a JSON object for one of the six exposed maps listed above,
  merging or deleting entries atomically within the serialized edit.
  Merge is a separate request so an older daemon rejects it instead of
  ignoring an unfamiliar mode field and destructively replacing a map.
  Existing full-map editors continue to use replacement. After an exposed
  map root, the complete dotted suffix is one literal entry name (including
  dots and slashes), e.g. `provider.model_meta.vendor/qwen2.5`.
  `SetSetting` with null removes that one entry; get, harness snapshot/undo,
  and setters use this same grammar. `models.set` admits entries only for
  its allow-listed model maps; prices remain `config.set`. The
  daemon checks the key against an allow-list: provider model, effort,
  routing and timing, a few `daemon.*` keys, and the whole `tui`, `tools`,
  `metrics`, `optimiser`, `compaction`, `naming`, `skills` and `artifacts`
  sections. It validates the value through the config types, so a wrong
  type or a mistyped key is refused. It writes the file without
  environment overrides, audit-logs the change and broadcasts
  `SettingsChanged`. Changes are hot and apply from the next read.
  Daemon edits use one serialized load/edit/save operation over the file-only
  configuration. Effective settings and cached runtime limits refresh before
  that operation releases its lock; environment overrides remain runtime-only.
  Failed mutations or saves do not publish a changed configuration, and an
  import that adds no configuration entries does not rewrite the file.
  A successful setting write reports `applies: "env_override"` when the
  daemon's environment supplies its model or idle timeout, with the variable
  in `override_source` and runtime value in `effective_value`. `value` remains
  the requested file mutation; unshadowed writes keep `now`/`on_tui_start` and
  a null `override_source`. Both setting operations return `effective_value`.
  The CLI names the saved and effective values and source. Its own environment
  is independent of the daemon's; `config get` uses the client's environment.
  `config get KEY --json` emits the same locally loaded effective value as one
  compact JSON value followed by a newline, preserving string quoting and
  other JSON types. Human output and unknown-key errors are unchanged. This
  is neither the daemon's current snapshot nor a whole-config backup; Set's
  supported-key validation, optional/null and map merge/replace rules remain.
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
| `daemon.log_max_bytes` / `log_keep` | 10 MB / 5 | Size-triggered rotation; `log_keep` is 0–64. Each retention check removes numeric rotations above the current keep count, including all rotations at 0; leading-zero aliases share one slot (canonical regular file wins, otherwise the shortest alias is normalized before duplicates are removed). Alias promotion refuses occupied/non-regular canonical destinations and preserves aliases on failure; max bytes 0 disables live-file rotation (§9.4) |
| `daemon.keep_alive_for_jobs` | false | Stay up while scheduled jobs wait (§9.7) |
| `daemon.delete_undo_secs` | 30 | Undo window for Session deletes (§5.1) |
| `daemon.backups_keep` | 7 | Daily database backups kept in `<data>/backups` (`formless backup` / `restore`); 0 = no automatic backups (§9.6) |
| `daemon.backup_restore_drill` | false | After a daily backup, rehearse restoration in a private throwaway profile and read its Sessions with an offline daemon; never start providers, MCP, hooks or jobs (§9.6) |
| `artifacts.max_total_mb` / `max_age_days` | 1024 / 30 | Artifact retention (§9.6); 0 disables a limit. Attachments count too: a removed one shows as expired in its message (§5.6) |
| `run_files.max_files_per_run` / `max_bytes_per_run` | 128 / 16777216 | Recovery admission caps per Run, counting original bytes plus descriptor metadata; overflow keeps paths with a reason; 0 keeps no new recovery payload (§5.7) |
| `run_files.keep_runs_per_session` / `max_age_days` / `max_total_mb` | 10 / 30 / 256 | Recovery retention per Session, age and profile MiB; 0 expires eligible payload, active/latest Runs remain protected, expired paths stay listed (§5.7) |
| `provider.base_url` | — (wizard) | Verbatim endpoint base (§4.1) |
| `provider.preset` | — (endpoint inferred) | Preset ID selected by setup; an explicit ID takes precedence over matching the endpoint (§4.2) |
| `relay.enabled` | `false` | Dial out to the hosted Relay on daemon start (§12) |
| `relay.url` | `wss://formless.athiwat.xyz` | Relay websocket base URL |
| `provider.extra_headers` | {} | Free-form headers per request (§4.1) |
| `provider.model` / `reasoning_effort` | — (wizard) | Daemon-wide defaults (§4.2) |
| `provider.context_tokens` (per model) | 128000 | Compaction trigger basis (§4.2, §11) |
| `provider.model_context_tokens` | {} | Context size per model id, overriding the endpoint-reported size and `context_tokens` for that model (§4.2, §11) |
| `provider.model_meta` | {} | Per-model corrections to reported/inferred vision, tools and reasoning efforts (§4.2) |
| `provider.auto_test_models` | false | Opt into background testing of untested or retry-eligible failed selected models; tests can spend paid tokens. Explicit Test actions remain available when off (§4.3) |
| `provider.max_retries` | 5 | Bounded backoff attempts (§4.4) |
| `provider.cache_control` | `auto` | Prompt-cache breakpoints (`cache_control`) on the system prompt and last stable message: `auto` for Claude models, `on`, `off`. Decided per model tried, so a fallback that isn't Claude gets plain content; applied to one-shot completions (compaction, `llm.complete`) too; a 400 naming `cache_control` resends once without them (§4.2) |
| `provider.cache_key` | `session` | Send a per-Session `prompt_cache_key` for better cache routing (`off` never sends it; dropped automatically if the endpoint rejects it) |
| `provider.max_backoff_secs` | 30 | Cap for exponential backoff with full jitter; a server's `Retry-After` / `x-ratelimit-reset-*` is honoured instead (up to 5 min); reset durations accept ms/s/m/h/d units, contiguous or separated by whitespace |
| `provider.tool_use_nudge` | true | Push models to build Tools: a GPT-family system-prompt addendum, and one nudge per step when an answer refuses for lack of access before using any Tool (§5.2) |
| `provider.stall_notice_secs` | 120 | Flag an open stream that sent no bytes for this many seconds (`ProviderStalled`; `/resend` re-sends); 0 = never |
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
| `provider.routing_complex_chars` | 20000 | Character-equivalent threshold (÷4 tokens) for calibrated prompt complexity |
| `provider.routing_window_days` | 30 | Recent event window used by call and role auto-routing; positive days (§4.6) |
| `provider.routing_min_samples` / `provider.routing_quality_bar` | 5 / 0.8 | Main auto-model quality gate before USD-per-success ranking; role thresholds remain separate (§4.6) |
| `provider.vision` | (by model name) | Default vision setting when no `provider.model_meta[model].vision` override exists (§5.6) |
| `provider.audio_input` / `provider.pdf_input` | known endpoint/model only | Permit native audio/PDF attachments (§5.6); unknown adapters/models retain references. API-style gates still apply. |
| `provider.model_audio_input` / `provider.model_pdf_input` | {} | Boolean overrides by resolved model ID, before provider defaults. An explicit audio override asserts Chat WAV/MP3 support; Gemini's inferred default is WAV only. |
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
| `tools.max_sleep_secs` | 60 | Longest single `sleep(ms)` in a Tool; excess is an error (§7.5) |
| `tools.max_random_bytes` | 1048576 | Largest decoded `randomBytes(n)` result; excess is an error (§7.5) |
| `tools.fetch_connect_timeout_secs` | 10 | Tool `fetch`/`fetchChunks` connection deadline, clamped to 1–86400 seconds; applies to shared and DNS-pinned clients. DNS admission/resolution waits on each hop are bounded by the smaller of this budget and the remaining total deadline |
| `tools.fetch_timeout_secs` | 60 | Total Tool HTTP deadline across DNS waits, redirects and response-body reads, clamped to 1–86400 seconds; trickling data does not reset it. Cancellation returns promptly and HTTP workers retain this deadline. OS DNS uses one fixed pool of four workers with at most sixteen queued lookups: expired queued work is skipped and late results discarded. OS lookups cannot be forcibly cancelled; a stalled lookup may retain one of those four fixed workers, while fetch callers still stop waiting at their deadlines. Timeout errors say `fetch timed out`, distinct from connection failures |
| `tools.max_read_bytes` | 67108864 | Largest `fs.read` or `fetch` body (64 MB): larger fails as a resource error pointing to `fs.readChunks` / `fetchChunks` (§7.5) |
| `tools.max_process_output_bytes` | 16777216 | `process.run` keeps this much of stdout and of stderr (16 MB); the rest is read and dropped, and the result says `truncated: true` |
| `tools.max_log_bytes` | 1048576 | All `log` lines of one Invocation (1 MB); later lines are dropped after one marker line |
| `tools.max_emit_bytes` | 1073741824 | Everything one Invocation `emit`s to its artifact (1 GB) |
| `tools.find_tools_max` | 25 | Most results one `find_tools` call returns |
| `security.trusted_hosts` | [] | Hosts whose `net:` grants need no approval (`api.example.com`, `*.github.com`); config.toml only (§10.3) |
| `security.secure` | false | Ask before ordinary sensitive Tool calls; credential reads require separate exact consent even when off. Other calls run unasked (logged). `--secure` / `FORMLESS_SECURE` turn it on at daemon start (§10.3) |
| `security.secret_patterns` | [] | Extra secret shapes (regular expressions) blanked wherever Formless stores or quotes text, beside the built-in ones; config.toml only (§15) |
| `security.sensitive_paths` | [] | Additive credential-read rules; owner config.toml only, never protocol settings; builtins and Formless credential hard denies cannot be removed (§7.4) |
| `security.sensitive_paths[].kind` | required per entry | Typed rule selector: subtree, basename_prefix or basename_suffix (§7.4) |
| `security.sensitive_paths[].value` | required per entry | Subtree uses an absolute path or ~/ prefix; basename rules use a nonempty literal prefix or suffix without separators or wildcards (§7.4) |
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
| `optimiser.promotion_margin_points` | 10 | Held-out replay win required (§8.5) |
| `optimiser.replay_pool_limit` | 200 | Unique input pool, 2–512 (§8.4) |
| `optimiser.replay_holdout_percent` | 50 | Held-out share, 10–90 percent (§8.4) |
| `optimiser.replay_min_holdout` | 20 | Minimum unique held-out inputs for automatic promotion (§8.5) |
| `optimiser.replay_significance` | 0.05 | Maximum exact paired-test p value, (0, .05] (§8.5) |
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
| `tui.vim_mode` | false | Normal/Insert editing in composer and internal Tool editor; /vim toggles it persistently, Insert Esc enters Normal, Normal Esc stops a Run and editor Ctrl+Q explicitly cancels (§10.2) |
| `tui.multi_pane` | false | Persistent multi-pane layout on wide terminals (§10.5) |
| `tui.timestamps` | true | Message times (`now`, `2m`, `3h`, `Tue 14:02`) on chat headers and a separator when the date changes; `/timestamps` toggles it (§10.2) |
| `tui.animations` | true | Moving indicators in the TUI and web. False shows a static TUI `●` and a steady cursor, and reduces web motion like the OS preference. Live changes apply on the next TUI frame and web settings refresh; re-enabling restores the terminal's default cursor and web motion unless an environment/OS preference still requests reduced motion. `REDUCED_MOTION` / `FORMLESS_REDUCED_MOTION` set to anything but empty, `0`, `false`, `no` or `off` force reduced motion in the TUI (§10.2). |
| `tui.language` | auto | UI language: `auto` (the system locale: `LC_ALL`, `LC_MESSAGES`, `LANG`), `en` or `th`; the web follows it, else the browser's language (§10.8) |
| `tui.hyperlinks` | auto | Clickable OSC 8 links for URLs in the TUI chat: `on`, `off`, or `auto` (on except Terminal.app and the Linux console) (§10.2) |
| `tui.theme` | auto | `dark` (pinned colours), `light` (deeper hues for white backgrounds), `high-contrast` (bright variants), `colorblind` (Okabe–Ito-derived sky blue/orange differences with yellow warnings and explicit status words), or `auto` (light when `COLORFGBG` reports a light background, else when the terminal answers an OSC 11 background query within 150 ms; dark otherwise). Every text colour of `dark` and `light` has at least 4.5:1 contrast on its background; a `[tui.colors]` override below that is used but noted at start, and a named override goes through the theme like any other colour (§10.6) |
| `tui.colors` | {} | Per-role colour overrides: `user`, `assistant`, `tool`, `note`, `error`, `accent` → a name (`light-cyan`) or `#rrggbb` (§10.6) |
| `tui.code_line_numbers` | false | Dim line numbers in the chat's code blocks (§10.2) |
| `tui.inline_images` | auto | Image thumbnails in the chat on iTerm2, WezTerm, kitty and Ghostty; `off` keeps only the reference (§10.2) |
| `tui.pane_widths` | [22, 26] | Multi-pane side panes as a percentage of the width, [left, right]; 0 hides one; `/panes wider\|narrower\|left\|right\|reset` or Ctrl+Shift+←/→ (§10.2) |
| `tui.screen_reader` | false | Start the line-oriented plain mode instead of the full-screen TUI, as `formless --plain` does (§10.5) |
| `tui.on_start` | last | What the TUI opens on launch: `last`, `new` or `picker`; `formless -c` / `--new` override it (§10.1) |
| `tui.notify` | desktop | `off`, `bell`, or `desktop` (terminal or browser notification plus the bell) while you are elsewhere (§10.6) |
| `tui.notify_min_secs` | 20 | Runs shorter than this finish without a notification (§10.6) |
| `limits.forecast_confirm_usd` | (none) | Ask before sending when a priced local forecast p90 exceeds this finite nonnegative USD amount; previews add no caps and unpriced/no-history forecasts have no invented dollar range (§5.2) |
| `limits.daily_usd` / `run_usd` / `run_steps` | (none) | Opt-in spending limits: provider spend per UTC day across Sessions, spend of one Run, and model steps of one Run; a note at 80%, a pause at 100% (§5.2). Profile edits apply to an in-flight Run before its next model step (§5.4). |
| `naming.enabled` | true | Rename "Untitled" Sessions after their first Run |
| `naming.model` / `auto_download` / `timeout_secs` | qwen2.5-0.5b / false / 30 | Embedded naming model (empty: a heuristic title), whether to allow a first-use download of a supported catalogue model (without it, titles use a heuristic), and how long one naming may take before the heuristic is used |

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
  Parent traversal is checked before native path normalization, including on
  Windows: each prefix traversed by `..` must resolve to an existing directory.
  Existing-directory parent traversal still resolves normally and remains
  subject to canonical containment.
  Session-relative input localization and process-relative path resolution
  preserve those literal traversal components before validation, even when
  Windows supplies a verbatim canonical working directory. Ordinary relative
  forward slashes are converted to native separators; rooted Windows paths
  retain the working directory's drive or UNC share. Ambiguous drive-relative
  paths (`C:notes.txt`) fail closed. A Session-relative path that cannot be
  represented in UTF-8 JSON is an input error, never a lossy substitute path.
  If a missing path or ancestor becomes a regular file or directory during
  resolution, canonicalisation is retried once at that same path before
  appending any missing tail. Permission, symlink-loop and other resolution
  errors fail closed; only genuinely missing entries permit walking up to an
  existing ancestor. A failed retry does not fall back to a broader ancestor.
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
  with fsmonitor, pager, credential helpers and ssh turned off. Scoped
  `diff`, `log` and `show` also disable external diff and textconv drivers
  (including when call arguments request them); repository hooks and other
  settings can still run, so `git`,
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
  New `*.run_unasked` details explicitly say `allow decision (approvals off)`.
  Legacy rows with those action names also represent approval-bypass decisions,
  including calls that subsequently failed to start or failed during execution;
  they must not be read as evidence of successful execution.

## 15. Privacy and data lifecycle

- **Zero telemetry.** No analytics, no automatic update checks, no crash
  reporting.
- **Network activity.** Examples include configured provider calls for
  Sessions, optimisation and Compaction, connections to a
  [user-enabled Relay](docs/usage.md#browser-access-via-the-relay-v2) and
  explicit commands such as `formless update check`. Optimiser paths redact
  secrets (§8.3). In builds with embedded model support, naming an untitled
  or provisionally named Session with suitable user text can download weights
  or a tokenizer when the selected [catalogue model](docs/architecture.md#local-naming-models)
  needs them. Automatic naming is enabled by default, but model downloads are
  opt-in. Older saved configs and hand-authored configs without the migration
  marker are conservatively migrated to disable downloads, because an
  unmarked `true` cannot be distinguished from the former default. Opt in
  with `formless config set naming.auto_download true`, or in TOML set both
  `auto_download = true` and `auto_download_migration_done = true` in
  `[naming]`. Set `naming.enabled = false` to disable automatic naming.
  Without the model, titles use a heuristic.
- **Recording:** Invocation inputs/outputs are recorded for metrics + replay,
  with secret-pattern redaction (API keys, bearer tokens, configured
  patterns), capped at `metrics.retention_per_tool` (default 200/Tool).
  One scanner (`secrets`) does this everywhere: memory notes, recorded
  inputs, outputs and errors, stored artifacts, and the code quoted to the
  provider for approval summaries. It looks for secret shapes (provider
  key prefixes, JWTs, private keys, bearer tokens, `password = …`,
  `token=…`, `cookie: …` assignments, long random-looking tokens) rather
  than key names, plus `security.secret_patterns`. For unquoted `keyword is
  value` phrases, a value needs a number, internal mixed case, or a recognized
  secret shape; ordinary prose such as "token is refreshed" stays intact.
  `:`/`=` and quoted assignments keep conservative masking. This heuristic
  cannot distinguish an unquoted plain-word secret from ordinary prose;
  masking known submitted credential values and configured patterns remains
  unconditional. A Tool result holding
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
  FILE [--overwrite]]` (`ExportAll`) writes one `formless.all/1` JSON file (mode 0600).
  The default name includes local hours, minutes, and seconds. Existing
  destinations require `--overwrite` with an explicit output path. The bundle includes
  every active registry Tool (MCP tools come back with their server), the
  shared modules, Skills, memories, recurring jobs and the config
  (providers, Roles, MCP servers, settings) with every value that can
  hold a credential blanked and listed in `blanked` (providers'
  `extra_headers` and auth secrets/tokens, MCP `env` values and
  `headers`); API keys never leave the keychain. `--sessions` adds every
  Session's transcript. `formless import-all FILE` (`ImportAll`) only
  adds: providers, Roles and MCP servers this machine lacks; modules
  before Tools; Tools as imports (renamed on collision for individual
  imports, §7.8; import-all skips existing names); Skills disabled for
  review unless their names already exist; memories (duplicates skipped);
  jobs paused; and Sessions. Re-importing the same setup skips existing
  entries, including jobs with the same prompt, optional source Session
  binding, schedule, overlap policy
  and delivery target, and Sessions with the same normalized name, model,
  reasoning effort, transcript metadata, checkpoints and portable plan history.
  An imported Session retains its source identity so re-import remains a skip
  after the local Session evolves; local Run provenance is not portable identity.
  A job's source Session ID is carried only for deduplication; imported jobs
  remain standalone when no matching local Session-bound job exists.
  Recurring job schedules use the destination machine's local time; bundles do
  not carry a separate timezone.
  The result reports added and skipped-existing counts per kind, the values
  to fill in and anything that failed.
- **Deletion:** Session delete removes transcripts + Invocation payloads but
  keeps anonymised per-Tool tallies (payloads gone, counts stay). Tool purge
  removes everything incl. tallies. **Full reset** (Settings or
  `formless reset`) stops the daemon and deletes config, keychain entry, and
  the entire data dir, with explicit confirmation. Default reset refuses while
  named profiles exist: remove them with `formless profiles rm NAME` first, or
  use full uninstall to coordinate every profile. The refusal is checked under
  the family freeze, so an in-use child or a newly created profile cannot be
  erased through its default parent.
  The same freeze rechecks discovery diagnostics: reserved, invalid, linked,
  case-ambiguous or unreadable legacy profile state is preserved for manual
  recovery (§9.4), including a former profile named `commands` mixed with
  default prompt templates.
- **Imports/exports** contain no secrets, metrics, or history (§7.8).

Network grants name an exact host unless their spec explicitly begins `*.`.
`net:example.com` no longer authorizes `child.example.com`; existing stored grants
are narrowed without being silently upgraded. `net:*.example.com` authorizes
proper subdomains (including deeper labels), excludes the apex, and preserves any
explicit port restriction. Input-bound URL values always resolve to concrete
hosts and cannot create wildcard authority. Runtime fetches, every redirect hop
and grant coverage share these rules. Wildcards never authorize private-address
exceptions: those still require an exact host and matching port.

Approval metadata identifies the actual localized registry arguments being
reviewed, with bounded redacted URLs (host/path and query keys), file paths and
non-GET body byte counts/previews where present. Missing or computed JavaScript
request details are labelled unresolved, never inferred from capability strings.
This preview adds visibility; it does not scope-check request paths/bodies or
change existing Always/Session/hour grant lifetimes. Public-suffix scope warnings
use a pinned offline full ICANN+PRIVATE PSL snapshot, including wildcard and
exception rules; no runtime download is allowed. Approvals also show sources of
untrusted framed content actually supplied to the model in this Run, including
Skill metadata and results without known physical origins. That banner is
informational, belongs to the explicit Run/Session, survives reconnect with the
approval, and cannot be cleared by an empty source list or forged payload text.
Accepted-input fallback evidence is owned by the active Run, nested execution,
and retained pending handles. A pending call may accept model input after its
Run ends: its evidence remains available until the result is delivered or the
handle is forgotten. Abandoning an already completed handle releases it
immediately; abandoning running work retains ownership until that work exits.
Session deletion uses the same flag-first completion handshake, so concurrent
completion cannot strand an abandoned handle. Providers retain only a weak lookup, so cached providers
cannot keep completed operation evidence alive. Failed evidence persistence
keeps known sources in memory; failed deferred delivery retains the handle for
retry, and never reports a clean consumer because a write failed. Ownership and
source locks are released before database access or asynchronous waits.

Third-party Tool, MCP, Skill and module descriptions share a 1,024-character
Unicode limit. Intake and legacy projection normalize newlines, terminal/control
and bidi/format characters to single-line metadata. Deterministic instruction-shaped
wording checks flag phrases such as “always”, “never”, “ignore” and “first call”
for human review; these checks do not establish that unflagged text is safe.
Model-visible descriptions are quoted and labelled as third-party data.
This projection also covers persisted active and inactive Session Tool definitions
(`session_tools.pinned_def`) before either forecast or model dispatch. It changes
only the model-visible top-level description: stored pinned bytes, selected
version, validation schemas and code-hash inputs stay unchanged. Existing framed
metadata is reused byte-for-byte only after validating its complete canonical,
bounded envelope and known warning set; a string prefix alone never establishes
that it has already been screened.
A frozen Session containing the exact historical raw Skill-index boundary
(`Skills (load one with load_skill before a task it covers):` followed by a
literal newline and `- ` bullet) cannot start, resume, retry, edit-and-run,
compare, compact, or consume queued work. Admission refuses before transcript,
Run or forecast writes; it never parses or rewrites the trusted frozen text.
Use the existing `/fork` workflow to retain the original history and start with
the current framed metadata. Current JSON-framed indices escape embedded
newlines and do not match the raw boundary; historical custom prompts without
that boundary retain their original behavior. Local forecasts still make zero
provider calls and report this blocked state explicitly.
Description warnings appear in previews and approval details. A changed Tool or
Skill description is shown with its before/after text and retained in Description
reviews, including MCP sync changes and changes made while clients disconnect.
The history is available through `ListDescriptionReviews`, web Settings →
Description reviews, TUI `/descriptionreviews`, and `formless tools reviews`
(`--kind tool|mcp|skill|module`, optional name, `--json`); `formless skills reviews
[NAME]` narrows it to Skills. Connected clients display new committed changes;
reconnection refreshes retained evidence. The description editor binds its preview
to the Tool ID, version and exact text. Local imports bind confirmation to the
same file content, and Skill bundle previews parse the actual bundled SKILL.md.
Current-version checks commit atomically with a reviewed description edit, and
client request generations preserve valid rollback edits while rejecting stale
previews. This screen applies to the entity description fields, not arbitrary
JSON Schema values; stored validation schemas and code-hash inputs retain their
existing semantics. This metadata review does not replace the separate code-hash
approval binding.

Tool data has no instruction authority: system instructions outrank user
instructions, and Tool/file/web/skill/module/sub-agent output is untrusted data.
Conflicting instructions embedded in data are surfaced to the user, never obeyed.
Every model-facing Tool result (including errors, deferred results and legacy
transcripts) carries a stable source-Tool/provenance envelope. Raw Tool.run and
direct-call values remain compatible. Actual data origins are bounded per Run
and persisted without URL userinfo, paths, query values, fragments, headers or
payload contents. HTTP/HTTPS sources identify only their canonical scheme, host
and port (the `url` origin kind); file sources retain canonical file paths.
Compacted history remains data; summaries do not become new system instructions.
Tool-supplied `llm.complete` and `agents.run` requests and system hints are also
framed as data from the calling Tool; they cannot create system authority.
User-configured Role system instructions keep their existing authority.
This boundary is defense in depth, not proof of real-model injection resistance
or an operating-system process sandbox.

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
promoted only on a sufficiently sampled, paired-significant ≥10-point zero-regression held-out replay win (demonstrated with
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
honest outcomes (throw on failure; returned `{error}` counts as failure
unless the output schema declares an `error` property for domain data)
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

The local Node relay adapter contains HTTP and Upgrade handler failures within
that request/socket. Malformed asset path escapes return 400; malformed mock
provider JSON returns an OpenAI-shaped 400 error. Unexpected HTTP failures
return a generic 500 before headers, or close the response after streaming
starts. Upgrade failures close only their socket. These dev-only boundaries
do not change Worker claim checks, transport/room rules or mock script behavior.
Loopback regressions verify subsequent health and valid requests still work.

Test waits for daemon subscriptions and pipe creation have a ten-second
absolute timeout with a diagnostic naming the missing event. Test-running
CI jobs have an explicit job timeout. The Windows transport gate parses a
successful Ping payload and inspects the created pipe DACL: only the current
user and SYSTEM receive allow ACEs. Subprocess fixtures are ignored by the
normal test run and explicitly selected by their spawning tests. The PR CI
assertion inventory reports tests without direct assertion macros for review
(including deliberate no-panic rendering checks). Historical
database rehearsals fail when their required fixture path is missing; storage
latency smoke checks enforce a generous 100 ms per-operation ceiling.

- Unit: schema validation, capability path containment, metrics math and
  trigger/promotion bars, compaction checkpoint round-trip.
- Integration: daemon against a **mock OpenAI-compatible server** (scripted
  streaming, tool-calls, 429/5xx, malformed calls, no-tool-calling probe
  failure); QuickJS sandbox escape suite (one test per Capability boundary).
- End-to-end: scripted TUI/CLI flows for M1–M4 acceptance lists; crash-kill
  daemon mid-Run and mid-Promotion recovery tests.
- No test may require a real provider key; all provider behaviour is mocked
  or pointed at a local compatible endpoint.

Credential-sensitive tests share a per-binary environment guard. It holds a
lock for the full test body (including awaited work), saves and clears inherited
`FORMLESS_API_KEY*` and OAuth credential overrides, and restores them on exit or
panic. Helpers borrow the caller's guard rather than reacquiring it; key-absence
and fallback tests use the same lock as setters. Tests pin hostile inherited
credentials, named-provider/file overrides, unwind restoration and contrasting
parallel mock keys. A source check rejects bare environment mutation outside the
shared helper.

The environment documentation inventory checks every `FORMLESS_*` reference
under `crates/*/src` against README or SPEC, including test-only references.
`FORMLESS_DAEMON_LOG` is explicitly excluded: it is internal log-routing state,
not a user override. Named key fixtures use the documented
`FORMLESS_API_KEY_<NAME>` / `FORMLESS_API_KEY_<NAME>_FILE` families.

`FORMLESS_LIVE_GROK=1` enables the ignored Grok ACP initialization smoke test.
`FORMLESS_GROK_BIN` selects its installed executable instead of `grok` on PATH.
This test only negotiates ACP and validates version metadata; it does not prove
authentication, MCP attachment, permission enforcement, or a completed turn.
`FORMLESS_GROK_MCP_SCRIPT` supplies the Python MCP fixture for the separate ignored
`live_grok_acp_turn` test. With the isolated loopback provider and dummy key, that
test exercises per-session MCP and user questions through the shared ACP driver.

`FORMLESS_LIVE_AGY=1` enables the ignored Antigravity ACP smoke test;
`FORMLESS_AGY_BIN` selects its executable. `FORMLESS_TOPIC_CAPABILITY` carries
the per-attempt bridge capability only in the selected MCP environment, never
in harness command arguments.

`FORMLESS_BIN` supplies a fallback CLI path when the desktop app starts its
daemon. Lookup tries a sibling CLI first, then this nonempty path, then PATH.

`FORMLESS_NOTIFICATION_TITLE` and `FORMLESS_NOTIFICATION_BODY` are internal
Windows desktop-notification helper inputs, not user configuration overrides.
Formless sets them on its PowerShell child to pass the notification title and
body as data rather than interpolating them into the script. The helper passes
nonempty text to BurntToast, falling back to `Formless` if both are empty.

Private subprocess fixtures also use these test-only variables (no product
configuration interface): `FORMLESS_CLIPBOARD_CHILD_FIXTURE`,
`FORMLESS_DOCS_OPENER_FIXTURE`,
`FORMLESS_FIXTURE_CURL_STATUS`, `FORMLESS_FIXTURE_INSTALL_MARKER`,
`FORMLESS_FIXTURE_INSTALL_TARGET`, `FORMLESS_FIXTURE_TOKEN`,
`FORMLESS_FORECAST_FIXTURE_DB`, `FORMLESS_FORECAST_FIXTURE_MODE`,
`FORMLESS_MOTION_TEST_CHILD`, `FORMLESS_PIPE_BUSY_CHILD`,
`FORMLESS_PIPE_BUSY_DATA`, `FORMLESS_PIPE_BUSY_CONFIG`,
`FORMLESS_PIPE_BUSY_PROFILE`, `FORMLESS_PRIVATE_ARTIFACT_PERMISSION_DATA`,
`FORMLESS_PRIVATE_EDITOR_ACK`, `FORMLESS_PRIVATE_EDITOR_FILE`,
`FORMLESS_PRIVATE_EDITOR_MODE`, `FORMLESS_PRIVATE_EDITOR_RECORD`,
`FORMLESS_PRIVATE_EDITOR_RESULT`, `FORMLESS_PRIVATE_REFRESH_LOCK_FIXTURE`,
`FORMLESS_PRIVATE_SNAPSHOT_CASE`, `FORMLESS_TEST_IMMEDIATE_RELEASE_MODE`,
`FORMLESS_TEST_MAINTENANCE_CONFIG`, `FORMLESS_TEST_MAINTENANCE_DATA`,
`FORMLESS_TEST_MAINTENANCE_ISOLATION`, `FORMLESS_TEST_MAINTENANCE_OPERATION`,
`FORMLESS_TEST_OWNERSHIP_CONFIG`, `FORMLESS_TEST_OWNERSHIP_DATA`,
`FORMLESS_TEST_PANIC_MARKER`, `FORMLESS_TEST_CONFIG_EDIT_ARGS` (JSON CLI
arguments for the test-only config-edit subprocess, which uses a fake editor to
verify unknown-key warnings), and `FORMLESS_TEST_DOCTOR_ARGS` (JSON CLI arguments
for the test-only doctor subprocess, which installs an in-memory credential
backend before read-only dispatch; the shipped binary does not read this variable).

`FORMLESS_LIVE_ACP_BIN_DIR` gates the ignored ACP initialize and availability tests.
It names a directory of installed ACP CLI executables (or symlinks). These tests
use isolated HOME/XDG directories and skip absent binaries. Initialize-only tests
send only ACP initialize; the availability test also sends session/new without
MCP servers, with an eight-second deadline. It is not a product configuration
override and does not authenticate or send prompts.

The existing ignored Antigravity bridge test uses `FORMLESS_LIVE_AGY=1` as its
opt-in gate and optional `FORMLESS_AGY_BIN` as its executable override.
`FORMLESS_TOPIC_CAPABILITY` is the per-attempt bearer capability for the private
MCP bridge. Pi topic runs include it in the Pi RPC process environment and
forward it to the MCP child through the extension's allow-listed `envVars`; it
is not placed in argv, prompts, or transcripts. Pi and descendants inheriting
the environment can access it. The Pi process also inherits the daemon's
ambient environment, so native credentials already present there are not
isolated from the harness.

`FORMLESS_EXPORT_DESCRIPTOR_FIXTURE` is a Linux-only export test hook pointing
to an owned redirected-stdout alias; it verifies that descriptor output preserves
the underlying file instead of atomically replacing the alias. The shipped
binary does not read this variable.

### Local pre-Run forecasts (§5.2, §10.6, §12)

`ForecastRun` previews the current draft, held attachments, checkpoint and frozen
Tool schemas without starting a Run or contacting a provider. The calibrated
prompt count is approximate. Completed Runs with recorded, comparable provider,
model/role, folder/Session, Tool snapshot and prompt size supply empirical p50/p90
steps, tokens, USD and duration. Legacy Runs without those inputs are excluded.
A quote binds the selected comparable Run IDs and measurements. Changes to
unrelated history leave an otherwise identical quote valid.
No history means prompt size only; missing prices mean no dollar range. Cache
fractions and compaction probabilities describe comparable history, not guarantees.
A draft already above the local compaction trigger is flagged immediately.

The optional `RunForecast.routing_pick` wire field names the model selected when
the effective forecast selector was automatically routed. It is omitted when no
automatic route occurred, rather than serialized as `null`. This records the
forecast's selection, not a provider attempt or a guarantee of the response's
served model; response attribution records the model that actually served the
response, including fallback.

TUI/web composer hints debounce and discard stale results after draft, Session,
model, attachment, persisted context, Tool or settings changes. Retry, edit and
resume use prospective inputs and preserve the existing transcript, paused status,
held files and budget waiver when consent is refused. Quiet hints use stable
local auto-selection; preparing a send selects exploration once and binds consent
to that selected model. `formless run --estimate TASK [--session ID]
[--model M] [--json]` uses the same Core engine locally, with no Session creation,
Run, upload, last-Run marker, credential refresh or provider request. It may use
read-only local IPC to an already-running daemon; it never starts one. Offline
SQLite reads use a private coherent snapshot under maintenance/native locks,
without opening the source with a SQLite pager or touching its WAL/SHM. Busy,
unsupported locking or a possible hot rollback journal fails closed. On macOS,
offline reads require writable local APFS/HFS/UFS: filesystem metadata is checked
before source open, preferred/forced proxy locking is refused, and the explicit
POSIX VFS probes reserved locks before claiming the existing exclusive gate.
The default auto-selecting VFS and its global flags are never changed. Network,
read-only and unknown filesystems fail closed; ordinary default-VFS writer and
checkpoint exclusion remain required. A missing
profile stays missing. Attachment previews carry client-supplied bytes-derived
metadata or display-only references; the RPC never reads a client-supplied local
path. Admissible uploaded attachments are rechecked at admission.

`limits.forecast_confirm_usd` is optional, finite and nonnegative. A priced p90
above it requires explicit confirmation. `ConfirmForecast` acknowledges a
short-lived connection-bound preview; the daemon rechecks current inputs before
appending or consuming attachments. The quote binds Session, history outcomes,
calibration, prices, selected model and actual held file identity. Store/config
admission serializes validation, append, held-file consumption, immutable ledger
and Run creation, capturing the exact first context/Tool snapshot and selected
configuration before later client changes. Edit/retry/resume mutations occur only
after their prospective quote is accepted. Stale consent fails closed. Consent
belongs to the live connection; reconnects require another review. A cached failed
send intent remains failed, so a newly approved action allocates a fresh intent.
Queued messages requiring confirmation stay queued and prompt for composer review.
Model comparisons build prospective forks privately; if a priced forecast requires
confirmation, the batch refuses before creating any real fork/message/Run. Batch
forecast approval is not supported: review and confirm each model in an individual
Session instead. Ordinary comparisons with no required forecast gate behave as before.
Headless sends
without `--yes` fail closed when confirmation is required. Forecasts add no step,
token or time caps. Admission persists an immutable per-Run forecast and its
non-text basis; Metrics shows the latest 200 recorded predictions beside actuals, errors and coverage, with
unfinished/unknown outcomes distinguished from completed observations. Each
snapshot retains at most 50 newest comparable Run ids (from the last 2000
recorded Runs) and measured non-text observations,
without duplicating prompts. A completed Run counts as a complete observation only
when every admitted physical request has explicit input and output usage;
missing/partial usage is unknown, never a fabricated zero-token success. Metrics
also names the served main provider/models and marks a mismatch with the quoted
basis; known actual-vs-quote errors remain visible, while fallback/mismatched or
zero-main-prompt outcomes are excluded from per-model calibration. Small N
ranges are observed quantiles, not confidence bounds. A cached endpoint context
window is used locally when available; otherwise the window source and uncertainty
are explicit.


### CLI observation and pending approval decisions

`sessions show ID --follow` attaches to the existing daemon, replays stored
messages and follows that Session's current/latest Run until its terminal state.
With no Run yet it waits for the first Run. `--jsonl` (or `--follow --json`)
emits the same tagged Event objects as `run --output jsonl-events`. Snapshot messages use
MessageAppended. Follow never starts, answers, or interrupts work: Ctrl+C detaches
with 130. Completed, paused/failed and interrupted outcomes exit 0, 1 and 130.
An already terminal Run exits after replay. Historical terminal Runs without
stored message boundaries display a Note and the initial Session snapshot only;
follow never invents a boundary or refreshes that legacy transcript without one.
Connection loss is an error.

Human `runs show` uses labeled stored identifiers, status/error, timestamps,
message boundaries, counters and recorded token sums. Available spend is
labeled as a report of estimates, with known subtotal, optional total and
returned completeness flags; unavailable amounts are not invented as zero.
Human `runs cancel` reports a cancellation request for a matching accepting
controller, or that none was found; neither acknowledgment proves a terminal
outcome. There is no polling or additional cancellation request. JSON and the
existing status-based show exits remain unchanged.

`tools list --filter FILTER` selects one stored-state predicate: `not-retired`
(including disabled Tools), `retired`, `failing` (known success rate below 0.8),
`needs-attention`, `off` (disabled) or `pinned`. It combines by AND with
`--tag TAG`, retaining exact stored-tag matching after the existing argument
lowercasing/leading-# removal. Unknown rates do not count as failing; states
may overlap. JSON uses the same selection and existing ToolInfo schema.
Human output distinguishes an empty registry from no matching rows and names
the flags to clear. `--show-pinned` adds a human PINNED yes/no column; default
piped cells and row order remain unchanged. These flags never mutate a Tool.

`prompts diff ID [--json]` reads the existing daemon proposal comparison:
target, already-applied state and ordered operation/line pairs. Human output
does not fit or clip lines to terminal width; JSON preserves the complete
response. This is a hypothetical without/with comparison, not an application
dry run or guarantee of permission/success. It performs no proposal evaluation
or decision; unknown IDs retain the standard not-found usage error.

Tool selectors use an exact name or full ID. After a miss, up to three
ranked partial-name suggestions can help you retry with the original exact
spelling; these use folded subsequence matching, not edit-distance spelling
correction. A suggestion never selects or runs a Tool automatically.

`modules list [--json]` reads all ModuleInfo rows, including retired modules.
`modules show NAME_OR_ID [--json] [--version N]` uses the daemon's existing
name/full-ID resolution. Default human detail shows metadata, version summaries
and sorted active Tool dependents without code bodies. `--version N` emits
exactly that stored source, preserving its existing trailing newline; missing
versions are usage errors. `--json` returns the complete existing ModuleDetail
and conflicts with `--version`. Invalid versions/conflicts fail before contact.
GetModule still fetches every version's code even for metadata-only output;
client projection does not reduce backend work (see #889). No module mutation
is exposed by these commands.

`usage [--by day|model|provider|purpose|role|session] [--since DURATION] [--json]`
reads the existing full-range UsageByDay aggregation once, independently of
the bounded recent Run list. Default/explicit day sends no group key and the
caller's current local UTC offset in minutes; its oldest-first buckets and JSON
keys are local dates. The daemon advertises this support as
usage_by_day_utc_offset_v1; if absent, the CLI refuses to display UTC buckets
as local. Other groups omit the offset and keep their descending
total-token order. Requests without an offset, including existing TUI clients,
keep UTC day grouping. The offset is fixed for the entire query, including
history across daylight-saving changes. No since means all history; durations
use the shared checked parser and one UTC RFC3339 cutoff matching the existing
stored TEXT representation. Human tables show the local UTC offset, cache writes,
total tokens (prompt plus completion), and a final total row. Cached and
cache-write tokens are included in prompt tokens.
Unknown costs and unavailable cache-write counts display as `-`, including in
totals that contain an unknown value. JSON preserves the complete row array,
including `cache_write_tokens` and null costs. Cost is the daemon's current estimate, not a
billing guarantee. Session groups use the ID for attribution and display the
current name when available. JSON includes the stable `session_id` (§10.4).
Unknown grouping keys and invalid/overflowing/unrepresentable durations fail
before contact. Aggregation, pricing and recording remain unchanged.

`runs --session SESSION --since DURATION [--limit N] [--json]` sends the
existing Session/time filters to RunMetrics before its newest-N limit. Each
selector is optional; Session lookup keeps the existing exact-ID, unambiguous
prefix and name rules. Durations use checked arithmetic and one UTC RFC3339
cutoff in the existing stored TEXT representation. The returned runs and their
summary describe that filtered, limited subset; JSON forecasts remain
independently scoped and unchanged. With neither selector, request/output
behavior is unchanged. Filters with any Runs subaction are usage errors before
connection; unknown/ambiguous Sessions necessarily fail after ListSessions.

`sessions usage SESSION [--json]` uses the existing Session selector and
buffers SessionUsage plus ContextUsage before output. Human records label
total/latest-Run counters, estimated USD cost, unpriced models and the current
prompt/context estimates. Attachment entries count stored metadata-array
entries, not tokens, unique files or pending attachments. A null latest report
means usage unavailable, not no prior Runs or a completed-Run guarantee.
JSON preserves both complete independent replies under `session_id`, `usage`
and `context`; this is not a common-snapshot guarantee. Existing context-window
discovery may contact the configured provider, so the command is not promised
offline. Reported Run count retains the backend's newest-100,000 global-window
limitation (#1458); aggregation/estimation/accounting behavior is unchanged.

`tools show NAME_OR_ID` includes stored pin state, the newest ten version
summaries (explicit numeric order) and up to five of the returned recent-call
metadata rows. Omitted counts point to `--json`, which retains the existing
complete detail contract. No new call inputs/outputs are added to these
summaries; the existing schema/example sections remain. `--code` conflicts
with JSON and writes only the exact current version source, preserving its
trailing newline state; a missing current version is an error. This intentional
human-detail layout change leaves fetching, metrics and security fields intact.
The existing GetTool all-version payload remains until protocol selectors
are addressed separately (#889).

Run inspection includes recorded cache-read, cache-write and reasoning token
counts. The human `runs show` report derives duration from the stored timestamps,
shows estimated USD amounts with six decimal places, and formats spend flags
and unpriced model names for reading.

`runs show ID [--json]` inspects any stored Run; `runs cancel ID [--json]`
cancels only that exact active Run, never a later Run in the same Session. IDs
accept unambiguous prefixes. Show uses the same terminal exit codes as follow;
an active Run query exits 0, and cancel exits 0 when its request succeeds.
`runs --limit N --json` lists stored Runs (default limit 20). An explicit
`--limit` with any `runs` subcommand is a usage error before connection.
`runs running [--json]` reads `ListInvocations` once through the existing
CLI connection. It reports live Invocations, in daemon order, with full handles,
Tool/Session identifiers, optional Invocation ID, compact elapsed/silent
durations for a minute or longer, returned stalled/abandoned flags and
optional progress as a clamped whole percentage with its label.
Abandoned means no longer awaited and may still finish. Human metadata is
terminal-escaped; JSON retains the original array and millisecond values.
Outer `runs --json running` is also supported.
There is no polling or Invocation control action.
`approvals pending [--json]` lists outstanding Tool capability requests.
`approvals approve ID [--once|--session] [--yes] [--json]` defaults to Once;
`approvals deny ID [--json]` declines without remembering a permanent denial.
Decisions use the client label CLI in the existing approval audit. A stale
request fails rather than reporting a grant. The decision receipt reports
`audit_recorded`; if the decision applied but the audit write failed, the CLI
warns explicitly without suggesting a retry. New broad Session grants require
two explicit confirmations on a TTY, and --yes off-TTY. Declining confirmation
leaves the request pending. These commands connect only to an existing daemon.


Artifact cleanup treats every SQLite or held-attachment path as untrusted data
(#1262). Retention, Session deletion, detach/expiry and orphan reconciliation
can unlink only with authority from a pinned profile artifacts directory and
ordinary relative components, using the platform boundaries below. Foreign, missing, symlinked or unverifiable
artifact references confirmed foreign, missing or nonregular lose only their
database rows; they never authorize an outside file operation or contribute
forged sizes to the retention budget. Permission, unsupported-volume, handle
verification and other uncertain I/O failures preserve rows and attachment
expiry state and return an error; preview does not invent a zero-byte estimate.
A final-path query's NotFound result is verification uncertainty, not proof
that a file is missing. Windows named lookup failures before a verified final
handle likewise retain records unless complete enumeration of the pinned parent
HANDLE proves the child absent, considering long names, short aliases and case
ambiguity. Directory enumeration uses the handle rather than reopening its name. Public detach rolls back its held entry if cleanup is
uncertain; background expiry retains the artifact row and surfaces a warning.
Owned retention uses observed regular-file sizes. Unix unlinks relative to
pinned parent descriptors (a final symlink swap never follows its target);
Windows captures the original pinned root's physical GUID-volume path once,
then validates every opened leaf's normalized final-handle path with exact
case-sensitive component containment before size accounting or handle deletion.
Each positive open must also have the exact physical parent captured before
opening its name. Registered files are protected during reconciliation by the
validated handle's volume/file identity, including case, short-name and hardlink
aliases, rather than solely by the registry's lexical path. Delete sharing is
denied so the validated leaf cannot be renamed afterward.
Attribute-only reparse changes can transiently expose names/attributes during
named opens; enumeration itself stays on pinned directory handles. No file
payload is read, and no outside leaf is authorized.
This is a final-handle containment boundary, not strict Windows no-follow
traversal. Unknown namespaces, volumes, casing or path-query errors fail closed.
Configured profile-directory aliases resolve before pinning. Unix traversal
never follows the artifacts entry or descendants through symlinks; Windows
static reparse entries are rejected and physical handle containment protects
deletion when an attribute-only reparse mutation races enumeration.

Windows tracked-file review (#628) retains original file objects in a separate
profile-private `review-objects` store on the same volume. A private, bounded
manifest containing the immutable original bytes, metadata, owner Run/path digest,
file identity, operation and expiry is flushed before creating a retained link or
moving a created file into the store. An incomplete intent never authorizes
recreating a file with guessed security metadata. Existing file objects keep
their exact security descriptors; no security-descriptor setter or privilege
change is used. Original authorized readers keep their original rights to the
retained payload: neither its locator nor the payload is claimed secret or
owner-only. New manifest JSON and its directory have explicit owner-only ACLs.
Retained hard links share later content writes, so the separate immutable bytes,
not the retained object's current contents, define the before-image.

Store namespace creation/open/link/rename is relative to pinned directory
handles. Cross-volume, unknown identity/security, untracked aliases, unsupported
filesystems or uncertain I/O refuse mutations. Every native hard link waived by
review is accounted for by a verified store manifest and exact object identity;
an extra public alias is never treated as an internal retention link. The store
admits at most 64 logical records and 32 MiB of serialized receipts and
retained object bytes. A private missing-state receipt reserves one slot and
4 KiB before the Tool runs. Reject can use that slot when the immutable
created-image receipt and retained object fit the remaining byte budget.
Existing-file hard links reserve their full bounded object allowance.
Seven-day cleanup skips live review leases and uses verified original handles.
The 32 MiB limit is an admission budget for serialized immutable images and
reserved retained bytes, not an instantaneous OS disk quota. Later writes to a
shared object and already-open handles can retain additional bytes. Inventory
charges the actual observed size without reading an oversized body; admission
refuses excess use. Cleanup can reclaim an inactive verified retained alias as
soon as it becomes oversized, without waiting seven days or touching its public
name. Active Run/review leases still protect their owned objects.
Receipts publish after a private staging file has been flushed. Recovery can
remove verified private staging files and legacy truncated receipts with
absent objects. Other corrupt token entries remain charged to the store for
manual recovery. Affected tokens cannot authorize review or deletion, and
verified tokens can still expire. Unknown filenames fail closed.
Already-deleted legacy files without a captured original object remain read-only.


### Colour-independent meaning (§10.6, #635)

`tui.theme = "colorblind"` is available through Settings and `config set`.
The TUI and CLI select a shared status palette for the detected terminal
background. On dark backgrounds it uses sky blue `#56B4E9`, yellow
`#F0E442`, and orange `#E69F00` for good, warning, and bad. On light
backgrounds it uses blue `#0064A0`, amber `#7B5300`, and vermilion
`#AA370A`. Diff additions use blue and removals orange.
Every added/removed diff retains a bold `+`/`-` gutter
by default, including when colour is disabled. Hunk line numbers, changed-word
underlining and newline labels remain intact.

When colour is disabled or the colorblind theme is selected, semantic status
cells include `ok`, `warn` or `fail`; unknown measurements stay unknown rather
than acquiring a success/failure label. Tool recent-call histories retain all
20 distinct tall/short glyphs with an explicit `▆ok ▂fail` legend, and trend
arrows retain their direction. Broader approval grants have an explicit warning;
buttons retain their named actions and keyboard shortcuts. `NO_COLOR` suppresses
foreground/background colour while keeping textual statuses, signs, values and
non-colour emphasis. Search matches use reverse/bold or underline without colour.
Pairing links remain available when a coloured QR representation is omitted.

The default remains `auto`; named and explicit role colour overrides retain
their existing precedence. The complete semantic-colour audit is recorded in
`docs/colorblind-inventory.md`.

The Metrics token-usage chart distinguishes cached, fresh and completion tokens
with `░`, `▒` and `█` patterns and a legend. Colorblind and NO_COLOR views also
show the component counts, so coincident palette colours carry no unique data.

## Topic orchestration contract v2

The daemon advertises `topic_orchestration_v2` in Ping. A human proposes selected
messages with an optional workspace and an advisory outline (zero to twenty
steps, each with a nonblank title, optional harness/model and ReadOnly or Write
access). Features may decompose a single original request into independent work
items. Approval moves each selected message once, preserves attachment
provenance, and creates the canonical Topic Chat. Attempt conversations are
read-only to chat clients; Topic Chat remains sendable.

A clean Git workspace records its base commit. A non-Git directory uses the
existing directory; an omitted workspace creates a private
`<profile-data>/topic-workspaces/<topic-id>` directory at approval, not proposal.
Each attempt has a child conversation and a private
`<profile-data>/topic-attempts/<attempt-id>/outputs` directory. Results contain
GitRevision, File (path and SHA-256), or Text artifacts, a summary and a
succeeded/failed/needs_user status. The Formless orchestrator, not the outline or
an individual harness, decides when a feature is complete. There are at most
twelve attempts per work item.

The scoped harness operations are ListConversations, ReadConversation,
AppendConversation (own attempt only), ReadArtifact (input index), and
ReportResult. Tokens are authenticated against the active attempt and topic;
no approval, rejection, start, merge or push operation is exposed. File input
reads return base64 content after scope, size and SHA-256 checks.

CancelTopic cancels approved, running or needs-user work and releases its
workspace lock. Pending proposals use RejectTopic instead. RetryTopicFeature
resets only the selected feature to Pending and the topic to Approved; it does
not start anything or move messages again. Start on a Running topic errors
without changing it.

StartTopicWorkflow claims the topic and sends a kickoff to a normal Formless
model Run in Topic Chat. Its tool snapshot includes topic_run_step,
topic_complete_feature and topic_fail_feature only for Running/NeedsUser topics.
The outline is advisory; the model chooses each independent step. A normal
Run end with open work yields NeedsUser; a Run error fails the topic. User
messages resume normal model Runs. No merge or push is performed. Start
failures persist detail, fail features and release the workspace lock.

Git steps use private worktrees under profile data: Write branches from the
latest feature revision (or the base); ReadOnly detaches at the exact selected
input revision. A successful Write must produce a clean new descendant HEAD.
Each attempt has a private staging directory, including Git attempts.
Directory/Scratch steps use private directories with copied read-only file
inputs. File/text outputs are reported through scoped MCP. A failed step is
returned to the model without automatically failing the feature; it can choose
another harness. Pi uses its integrated RPC adapter; Antigravity, named ACP
profiles and OpenCode use their ACP adapters. Muse Code uses its interactive
session protocol and requires one-run workspace trust from the user. ACP
requires a valid acp_agent.

CancelTopic interrupts the Topic Chat Run and kills live CLI process groups
on Unix (the direct child on Windows). The last launch check is serialized
with cancellation. On restart live attempts become Abandoned and Running
topics become NeedsUser, with durable detail and no automatic restart.

The private stdio bridge is `formless --profile <profile> topic-mcp --attempt <id>`.
It receives `FORMLESS_TOPIC_CAPABILITY` only through the environment. Daemon
CLI overrides `FORMLESS_TOPIC_CLAUDE_EXE`, `FORMLESS_TOPIC_CODEX_EXE`,
`FORMLESS_TOPIC_ANTIGRAVITY_EXE`, `FORMLESS_TOPIC_PI_EXE`,
`FORMLESS_TOPIC_MUSE_EXE`, and `FORMLESS_TOPIC_MCP_EXE` must be absolute
paths. Harnesses also receive the
profile's `FORMLESS_DATA_DIR`, `FORMLESS_CONFIG_DIR`,
`FORMLESS_TOPIC_ATTEMPT_ID` and `FORMLESS_TOPIC_OUTPUTS_DIR`. Capabilities
must never be included in prompts, argv or transcripts. The bridge exposes
AskUser and RequestPermission in addition to the scoped conversation/artifact
operations. RequestPermission only raises a user request; it cannot decide one.
An undecided or cancelled permission is denied. The bridge uses the existing
five-minute IPC request deadline; cancellation closes pending requests.

`FORMLESS_TOPIC_OPENCODE_EXE` must be an absolute executable path; when set, it
selects the executable for both readiness probes and topic launches. When unset,
Formless resolves `opencode` from an absolute PATH entry. The private-cwd
readiness probe runs only `--version`; sign-in stays unknown because
credential-list commands can write state and access the network.
OpenCode can still use its free default model without credentials.

The Pi adapter's availability probe accepts an absolute `FORMLESS_TOPIC_PI_EXE`
override before searching absolute PATH directories for `pi`; it skips relative
or empty PATH entries and does not fall back to a bare `pi`. It only runs
`--version`; sign-in remains unknown because Pi has no read-only, any-provider
CLI auth probe. It does not write Pi configuration or isolate the user's agent
directory.

Developer-only live-test controls are `FORMLESS_AGY_BIN`,
`FORMLESS_LIVE_AGY`, `FORMLESS_OPENCODE_BIN` and
`FORMLESS_OPENCODE_SMOKE_DIR`; they do not configure runtime harness behavior.
Claude ReadOnly uses plan permission mode and Read/Glob/Grep tools; Codex uses
its read-only sandbox. On every harness the isolated worktree/private outputs
directory is the real boundary. Pi ReadOnly is tool-restricted (`--tools`
allowlist); OpenCode ReadOnly is permission-restricted (agent rules). Neither
Pi nor OpenCode has an OS sandbox. Harness-specific permissions are not an
OS-level boundary, and permission denials must be retained in attempt output.

Topics form an arbitrary hierarchy. An explicit `parent_topic_id` must name an
existing topic that has been approved. Otherwise a proposal from an existing
Topic Chat defaults to that topic as parent; other sources create roots.
ListTopics includes all levels and their parent IDs. Cyclic parent chains are
rejected. Permanent deletion/purge removes descendant topics and their canonical
conversations; cancellation affects only the selected topic, not its children.
Purging a source removes pending/rejected proposals, not approved topics.
Source-owned attachments referenced by surviving moved topic messages are retained.

Harness and topic-model conversation tools default to the topic branch: the
topic and all ancestor Topic Chats and their attempt transcripts, not Main
Chat, siblings or unrelated sessions. The user-only SetTopicContextAccess
sets persisted allow_external_context (default false); opting in permits all
non-deleted conversations. List/read/search share this scope. Search is a
case-insensitive substring match returning at most 40 bounded snippets.

The wire also includes ACP (acp) and Muse (muse). ACP dispatches through the
registered named-agent adapter and requires acp_agent to be nonempty and
present only for ACP; selectors are validated against the registry and retained
in advisory steps and attempt metadata.

Harness questions reuse UserQuestion, AnswerQuestion and UserQuestionClosed on
the canonical attempt conversation. Topic MCP AskUser and InteractionHandler
ask_question accept 1–4 existing Question objects (labeled options and
multi_select) and return an ordered optional list of QuestionAnswer; cancellation
returns None. Pending questions and permissions pause their attempt and feature
in NeedsUser and resume them when answered, without reviving terminal work.

HarnessPermissionRequested/Closed and the user-only HarnessPermissionDecision
request handle permissions. Only offered option IDs are accepted; None rejects.
PendingApprovals and subscription attention replay include open permissions.
In flattened ServerMessage::Event messages, HarnessPermissionRequested
serializes its permission kind as `permission_kind` to avoid the outer `kind`
discriminator. Nested PendingApprovals event lists retain `kind` for older
clients, and readers accept either name. Old full envelopes with duplicate
`kind` keys remain ambiguous to JSON clients.
No model tool or scoped MCP operation can decide a permission. Cancellation,
attempt termination, shutdown and dropped interaction futures close undecided
requests as rejected. AllowAlways/RejectAlways decisions are held only in daemon
memory, scoped to the current attempt and the exact kind/title/input, never
persisted. Adapters consume the adapter-agnostic local_harness InteractionHandler
permission/question/update hooks; updates append structured canonical messages.

The topic orchestrator may answer a pending harness question using
`topic_answer_question {question_id, answers, basis, evidence:{message_id,quote}}`.
The nonempty `basis` is explanatory only; authority comes from storage-verified
evidence and an exact whole-message quote. Deterministic request grounding is
limited to one non-multi-select question ending in `?`, with a stem and unique,
nonempty offered labels containing only ASCII letters, digits, spaces, hyphens,
or underscores, and no nonempty option descriptions. The approved original
request must contain only `<same case-sensitive stem>: <one offered label>`;
the source may have one terminal period, but punctuation or quotes are not
allowed within either atom. The complete quote must match the approved source snapshot;
the exact feature source must still be live, ungenerated and moved into this
topic. Distinct approved source requests, conflicting or ambiguous context,
changed human context, paraphrases, conditional/prose requests, multiple
questions and free-text/multi-select choices remain pending for the user. The
narrow form rejects conditional, negation and conjunction tokens, as well as
`maybe`, `perhaps`, `might`, `could`, `possibly` and `probably`, in either
atom. These deterministic token and character checks are conservative syntax
filters, not semantic interpretation.

A second deterministic path accepts a `topic_human_answer` receipt only for
the same attempt and feature when the full question list (including headers,
options and multi-select flags) and normalized answers exactly match. Receipts
are appended as visible system notes only after a successful explicit user
answer to a pending harness question; they are not created for permissions or
user-only decisions. Both paths reject changed/deleted evidence and newer
in-scope human messages or answer receipts. Topic approval records a generated
`topic_grounding_context` fingerprint over canonical human messages in the
topic and ancestor contexts; attempt transcripts and generated nonhuman
messages are excluded, and external-context permission does not expand evidence
scope. Old topics without this approval receipt cannot use original-request
grounding. This is deterministic identity and explicit-choice matching, not
general NLP semantic proof; when the form does not settle the choice, the model
must leave the existing harness question pending for direct user response and
must not issue a duplicate `ask_user` prompt. Every model answer is visible in Topic
Chat with the harness, attempt, questions, answers, basis, resolved source ID,
quote and scope. User-only questions cannot be answered by this tool. First
answer wins.
UserQuestionClosed.answered_by is "user" or "model" for delivered answers and
absent/null for cancellation or expiry.

MessageInfo.generated_by is an optional typed canonical marker (serde default).
Topic kickoff messages retain role=user for normal SendMessage admission, but
are inserted atomically with generated_by="topic_kickoff". Transcript reads,
events and forks preserve the marker. Both proposal and approval reject source
messages with any generated_by value; generated instructions are not new user
requests and cannot be moved as topic requests.
Attempt instruction messages are inserted with
generated_by="topic_attempt_instructions" without changing the exact prompt
sent to the harness. This marker applies only to new messages; historical null
values remain unknown and are not backfilled.

Topic approval records a generated `topic_grounding_context` system-note with
a fingerprint of the in-scope canonical human context. Successful explicit
human answers to pending harness questions create visible `topic_human_answer`
system-note receipts containing the exact question, normalized answer, scope
and context fingerprint. Neither marker is inferred or backfilled for history.

Pending harness permissions and user-only questions also send a best-effort
desktop notification through spawn_blocking. Notifications include the topic
name, harness and redacted prompt title, never raw_input. Errors are ignored
after logging. FORMLESS_TOPIC_NOTIFICATIONS=0 disables them; unit fixtures use
an injected capture sink and never invoke OS notifications.

topic_run_step launches asynchronously and returns {attempt_id,
conversation_session_id}. Independent features can run concurrently; a feature
still permits only one active step. topic_wait {attempt_ids?, timeout_secs?}
waits for a finished attempt, a question or an informational permission notice.
timeout_secs defaults to 600 and must be in 1..=600. An empty selection or no
active work returns a snapshot immediately. Attempt IDs are scoped to this
topic; completion and user-only informational notices are consumed once per
Topic Chat daemon lifetime. Ordinary harness questions remain pending until
answered. Cancellation and daemon shutdown unblock waits. There is no child
model run. Normal model-run completion with open work marks NeedsUser and
appends a durable "send a message to continue" note; harnesses may continue,
but no model follow-up is started automatically. Run interruption/error
cancels those steps.

Topic permission rules are persisted and edited only by the user/client
AddTopicPermissionRule and RemoveTopicPermissionRule requests, never MCP or
model tools. Optional harness, ACP-agent and permission-kind filters are ANDed.
Patterns are case-sensitive full-string globs (* any characters, ? one Unicode
character); either the title or a string raw_input.command may match. Reject
rules take precedence. Rules prefer the offered AllowOnce/RejectOnce option,
falling back to the same polarity's Always option; an unavailable polarity
falls through to the user. Each automatic decision emits requested and closed
events (by="rule:<id>") and an attempt-transcript audit message. Unmatched
permissions retain the normal prompt and away notification.
The core rule-ineligible-kind denylist starts with trust_workspace: it always
requires a fresh user decision, even with wildcard Reject rules or an earlier
Always answer. Rule patterns contain 1..=1024 characters; a topic allows at
most 256 saved rules.

SetTopicLimits is a user/client-only persisted control. Defaults allow three
retries after the first attempt of a feature+trimmed step title, and twenty
attempts across a topic. Both values accept the full u32 range; zero retries
still allows the first attempt, while zero total attempts blocks all launches.
Caps are checked atomically at launch, including concurrent launches. Hitting
a cap refuses launch, marks the topic and feature NeedsUser, and raises a
user_only question: "Raise limit and continue" or "Stop". Model answers are
rejected. Raising increments the hit cap by its default (3 or 20, saturating)
and returns the topic to Running; Stop leaves NeedsUser. Neither answer starts
a model or harness automatically. SetTopicLimits itself changes limits only.

ListHarnesses refreshes all seven kinds concurrently, expanding ACP into one
entry per ACP registry id with acp_agent set. Every probe is capped at
10s overall (ProbeContext defaults to 10s); a timeout reports unknown auth with
detail "probe timed out". Legacy non-ACP readiness requires installed and
signed_in is not Some(false). ACP requires signed_in=true and a resolved
executable; unknown ACP authentication is unavailable. Cursor, Cline, Junie,
Auggie, and Devin remain unavailable because Formless has not integrated a
supported non-model sign-in readiness check for them, even if session/new
succeeds. Signing in alone does not
unblock them. Planning receives fresh availability,
and each step refreshes its selected harness before launching. Claude uses
--version and auth status; Codex uses --version and login status. Muse Code
checks its version before using usage --json and only confirms sign-in when
it receives a Meta account allowance. These probes do not send model turns
or copy raw auth output into detail or logs.

Readiness resolves absolute `FORMLESS_TOPIC_<KIND>_EXE` overrides or each
kind's command on PATH. This includes `FORMLESS_TOPIC_OPENCODE_EXE`,
`FORMLESS_TOPIC_PI_EXE`, `FORMLESS_TOPIC_MUSE_EXE`, and
`FORMLESS_TOPIC_ANTIGRAVITY_EXE`. Pi has adapter-specific availability and an
attempt-private RPC runner. OpenCode has a bounded `--version` probe in a
private cwd with kill-on-drop and process-group cleanup; it requires V2 >= 2.3.2
and leaves credential status unknown because checking auth initializes
providers and makes network requests. Muse Code checks its version before
`usage --json` and only confirms sign-in when it receives a Meta account
allowance. Antigravity uses a bounded adapter-specific probe: ACP initialization
checks installation/version and `agy models` checks existing sign-in. Topic
steps carry the selected Antigravity executable identity from readiness through
launch and fail closed if it is unavailable.

ACP applies `FORMLESS_TOPIC_ACP_EXE` to each named-agent preparation; otherwise
the agent's command is resolved from the daemon's PATH. Readiness canonicalizes
the selected absolute path and probes that path on a bounded blocking worker,
including initialize version (`agentInfo.version`, then
`_meta.agentVersion`) and `session/new` sign-in checks. It retains the
pre-canonical selected absolute path as `executable_argv0` alongside the
canonical executable. Each topic step carries both internally; public
`HarnessAvailability` and ListHarnesses shapes remain unchanged. The topic
launch executes the canonical path with the retained invocation path as argv0
on Unix, without rereading PATH or the ACP override and without executing the
symlink path. Workspace-local PATH entries therefore cannot substitute a
different ACP executable. If preparation does not produce an executable
identity, the step stops before attempt launch with the existing
retry/substitute guidance. Unknown ACP authentication is unavailable. If the selected path
disappears before spawn, launch fails without a PATH fallback. Resolution is
fresh after each retry or approved substitute; this pins path selection, not
executable contents.

The shared ACP spawn also anchors relative and empty child `PATH` entries to
the daemon's working directory before setting the topic workspace as cwd. This
applies to named ACP agents, OpenCode, and Antigravity. Workspace-local `./bin`
is not implicitly searched for child tools or shebang interpreters, so
workflows relying on workspace-relative `PATH` entries may resolve differently.

The reusable ACP probe initializes and creates a session with no MCP servers in
a private temporary cwd, never prompts, maps configured auth errors, and kills
its process group on success, failure or timeout. ACP's inner probe deadline is
eight seconds; the blocking worker is bounded at ten seconds. Antigravity
readiness resolves `agy`, verifies ACP v1 and the minimum supported version,
then runs `agy models` to determine sign-in. An explicit sign-in prompt means
not signed in; inconclusive failures remain unknown. Adapter blocking probes
must enforce their own internal deadline and kill child processes on drop:
core's outer timeout cannot abort spawn_blocking. Core does not own the ACP
agent registry and omits the bare ACP entry from ListHarnesses; the ACP
adapter's registry supplies per-agent entries. The per-kind probe dispatch
receives the selected acp_agent id.

Grok's registry probe reads `_meta.agentVersion` (minimum 1.0.92), reports
signed_in true on session creation and false on -32000, and forces
`GROK_DISABLE_AUTOUPDATER=1` through the shared launcher. It never places
`XAI_API_KEY` in arguments or sends a model turn.

topic_request_harness_substitute raises a user_only question with "Sign in and
retry", "Use <substitute>", and "Skip step". Model answers are rejected.
Only a user's Use answer records an approval for that topic, feature and
trimmed step title; the next successful launch consumes it. A request to run
the wanted harness then uses the explicitly approved substitute, which must
itself be ready. Formless never substitutes without that approval. Other
answers and cancellation record no approval and start no work.
Only the suggestion's optional model and acp_agent are used for a substitute;
an omitted model selects the substitute's default, never the original model.

Terminal topic transitions close pending limit/substitute decisions with
UserQuestionClosed answered_by=None. Finalizing one feature also closes its
own decisions while leaving sibling work alone. Closed decisions no longer
keep the daemon busy or leave stale user prompts.

Every step owns a private attempt area. Git Write uses a worktree on
formless/topic/<topic>/<attempt>, starting at the first selected GitRevision
or otherwise its feature's latest revision/base. ReadOnly Git is detached.
Every selected revision is also exposed through a detached input worktree
whose path and exact revision are in the harness prompt. Cleanup removes
worktrees on completion, failure and cancel; attempt branches retain commits.
No operation changes the source checkout or moves the user's branch.

Directory Write and ReadOnly steps recursively copy the source into their
private workspace; a selected Directory input becomes the starting copy.
Copies skip .git and reject special files, absolute symlink targets and
targets escaping the source tree. Containment follows symlinks in the longest
existing target prefix before resolving the remaining nonexistent components;
existing prefixes reached again through .. are resolved as well.
Unresolvable existing links are rejected. Relative internal links are
preserved as-is, including dangling links. File data copying
runs off the async executor and checks cancellation. The attempt area must
be outside the source tree to avoid recursive/self-copying. Scratch steps
get an empty private workspace. Directory Write automatically reports a
Directory artifact pointing at its retained attempt workspace.

Inputs may reference exact finalized artifacts from any feature of this
topic. Every Directory input is copied to a distinct private input path;
every File is copied and SHA-256 checked, and text inputs remain inline.
Harness-reported directories must be inside that attempt's private outputs.
Concurrent workflows and Write steps may share a source workspace because
the old exclusive source-workspace lock is no longer acquired. Per-attempt
leases/cancel tokens remain live through validation and are released on stop.
Harness-process and adapter errors after a successful report are logged and
ignored at the adapter boundary. Daemon validation always runs afterward:
dirty worktrees, HEAD/reported-revision mismatches, changed file hashes and
invalid Directory outputs fail the attempt and discard its provisional outputs.
Only validated outputs are published to the feature. Cancelled and Abandoned
attempts are never overwritten. CLI exit still releases the
active-attempt gate before explicit feature completion is allowed. A feature's
successful terminal state is Ready, reached through topic_complete_feature,
not automatically through report_result.
Combining outputs is explicit work in a later isolated step, never an
automatic merge or push into the source workspace.
