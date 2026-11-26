# Using Formless

### Requirements

| Need | Why |
|---|---|
| A Rust stable toolchain (`rustup` recommended) — **source builds only** | the app is one Rust workspace, edition 2021 |
| A C compiler (`cc` / `clang` / `gcc`) — **source builds only** | the QuickJS sandbox (`rquickjs`) is compiled from source |
| A model endpoint that **supports tool calling** | Formless supports OpenAI-compatible Chat Completions or Responses, and native Anthropic Messages. The Anthropic preset selects Messages. Other built-in presets and custom OpenAI-compatible URLs start with Chat Completions. |
| A real terminal | the default client is a full-screen TUI |

Prebuilt binaries need none of the toolchain requirements. On Linux a Secret Service provider (GNOME Keyring, KWallet) keeps the API key in the OS keychain; if none is available, Formless falls back to a `0600` file in the config dir, so headless machines still work.

### Install

**Prebuilt binary (macOS, Linux).** The installer resolves the latest GitHub release for your platform, downloads it, verifies the `.sha256` (with `sha256sum` or `shasum`) and puts `formless` in `$PREFIX/bin` (default `~/.local`). Each installer curl attempt has a 10-second connection and 120-second transfer limit, with three retries for transient errors; a timed-out lookup suggests pinning `FORMLESS_VERSION` or using a trusted mirror (`FORMLESS_LATEST_URL` / `FORMLESS_RELEASES_URL`). It checks that `$PREFIX/bin` is writable before downloading, and replaces the binary atomically (a temp file renamed into place), so an interrupted or failed install leaves the previous version working:

```bash
curl -fsSL https://raw.githubusercontent.com/athiwatc/formless/main/scripts/install.sh | sh

# overrides: PREFIX=/usr/local, FORMLESS_VERSION=0.1.0 (or v0.1.0),
# FORMLESS_FROM_SOURCE=1 (builds this checkout when run from one, else the
# GitHub repository at FORMLESS_VERSION)
```

The installer prints the next setup and verification commands and checks
whether its `bin` directory is on `PATH`. If needed, it prints a quoted command
for your `$SHELL` (bash, zsh, fish, or a POSIX shell) and names the startup file
where you can keep it. It does not edit shell configuration files. Run
`formless` to start setup, or `formless doctor` to check your configuration;
you need a provider API key or a local model endpoint.

**From source** (any platform with a toolchain):

```bash
git clone https://github.com/athiwatc/formless && cd formless
cargo build --release          # binary at ./target/release/formless
```

`cargo run` (debug) and `cargo test --workspace` work the same way if you are working on the code. You can also `cargo install --git https://github.com/athiwatc/formless --locked formless-bin`.

**Linux packages.** The installer supports checksum-verified x86_64 and ARM64 archives. Each release also has a `.deb` (`sudo apt install ./formless_<version>_amd64.deb`) and an `.rpm` (`sudo dnf install ./formless-<version>.x86_64.rpm`); Arch users can install `formless-bin` from the AUR once it is published.

**Docker.** `ghcr.io/athiwatc/formless` runs the daemon headless on a server, reachable through the relay; volumes, the API key as a secret and profiles are in [docs/docker.md](docker.md).

**Homebrew / winget.** Releases render a Homebrew formula and winget manifests and publish them once the maintainer's tap and tokens are set up ([packaging/README.md](../packaging/README.md)): then `brew install athiwatc/tap/formless` and `winget install athiwatc.formless`. Until then, use the installer on macOS/Linux or build from source. **Windows:** follow the [manual ZIP walkthrough](windows-install.md) for checksum verification, the nested executable directory and optional user PATH setup, or build from source.

**Uninstall.** `formless uninstall` stops the daemon, removes the login service, deletes keychain entries and every profile's data, and removes the binary (or names the package-manager command). `--keep-data` keeps Sessions, Tools and config; `formless export-all` keeps a copy first.

> [!NOTE]
> Release artifacts are checksummed but unsigned. On macOS you may need `xattr -d com.apple.quarantine formless` after installing by hand. `formless update check` reports newer releases but never rewrites the installed binary — updates stay with the installer and checksum verification ([§3.12](#updating)).

### First run: the onboarding wizard

```bash
./target/release/formless
```

With no arguments `formless` launches the **TUI**, which auto-starts the daemon on first use (`formless daemon serve`, detached, logging to `<data dir>/daemon.log`) and then walks the wizard:

1. **Provider** — pick a preset (OpenAI, Anthropic, OpenRouter, Gemini, Groq, Ollama, LM Studio) or enter any OpenAI-compatible base URL. Local presets skip the key step, and a key in `OPENAI_API_KEY` (etc.) is offered automatically. Local setup discovery honors `OLLAMA_HOST` and `LM_STUDIO_HOST`, including custom loopback ports such as `LM_STUDIO_HOST=127.0.0.1:1235`. Configure remote hosts manually. Add more later with `formless providers add NAME --preset openrouter`.
2. **API key** — stored in the OS keychain plus a `0600` fallback file (`api_key`) in the config dir, so rebuilding a dev binary never loses it.
3. **Model** — picked from the endpoint's model list, or typed by hand.
4. **Reasoning effort** — for models that accept it.
5. **Probe** — checks connectivity, streaming and **tool calling**. The probe tries native calls first, then tries prompt-based calls if needed. If the prompt fallback succeeds, the wizard saves `provider.tool_mode = "prompt"` ([§3.8](#provider-options-v2)). A result with neither mode demonstrated blocks setup. Probe errors such as authentication failures show their own diagnostics.

The result is written to `config.toml` in the config dir ([§4](#data-config-and-profiles)); you never need to edit it by hand. Re-run the wizard at any time with `formless doctor` for a diagnosis, or delete the config to go through it again.

Setup names the active profile, and the TUI status line identifies a non-default
profile. If this profile needs setup while another profile has saved provider
configuration, the wizard, `formless run`, and `formless doctor` show the exact
command to open it. `formless profiles list` lists the available profiles.

### Using the TUI

Type a task at the bottom and press **Enter**. Formless starts with the built-in Tools ([§5.1](architecture.md#the-built-in-tools)) and builds whatever else the task needs with `create_tool`.

Essential keys to get started are below. For the generated main-keys and
command reference, see [Keys and commands](keys.md), run
`formless docs --keys`, or open `/help` in the TUI. Views, lists and dialogs
also show their own keys in the footer.

| Key | Action |
|---|---|
| `Enter` | send the message, or steer the Run that is in flight |
| `/copy`, `/copy code [n]` | copy the last answer or one of its code blocks (OSC 52, works over SSH) |
| `/mouse` | release the mouse so you can select text with the terminal (again: wheel scrolling back) |
| `Ctrl+J` (or `Ctrl+Enter` / `Alt+Enter`) | queue the message for after the current Run; `Ctrl+Enter` needs a terminal with the kitty keyboard protocol (kitty, WezTerm, Ghostty, recent iTerm2) |
| `Esc` | stop the current Run / leave a dialog |
| `Tab` / `Shift+Tab` | cycle views (Chat, Sessions, Tools, Metrics, Proposals, Settings) — the bar at the top shows where you are |
| `Shift+E` (Tools view) | edit the selected Tool's JS (open it with `Enter` first); `Ctrl+S` saves it as a new version through the smoke gate |
| `PgUp` / `PgDn` | scroll the transcript |
| `↑` / `↓` | move through wrapped draft rows; earlier messages of this Session at the first/last row (non-Vim); `Ctrl+R` searches them |
| `Shift+Enter` or `\` then `Enter` | new line in the message |
| `←` `→` `Home` `End`, `Ctrl+A/E/W/U/K`, `Alt+B/F/D` | edit the message (readline keys) |
| `Ctrl+C` | clear the typed message; press again within 2 s to detach (the daemon keeps running, nothing is lost) |

Common slash commands: `/help /new /sessions /main /topics /topic /fork /search /edit /tools /metrics /proposals /propose /panes /settings /model [ID] /thinking /stop /cancel /resend /resume /abandon /agents /quit`. Type `/` for a command palette (fuzzy filter, Tab completes, Enter runs); `/help` or `?` on an empty input lists every command grouped by area. Ctrl+V or `/paste` attaches a screenshot from the clipboard, and dragging a file into the terminal attaches it. Type `@` to reference files: a popup completes paths under the current directory (respecting `.gitignore`), and on send small files are attached while large files and directories go as a reference the agent reads with its Tools. `/model` opens a picker: type to filter (fuzzy, so `son4` finds `claude-sonnet-4`), Tab changes reasoning effort, Enter switches the current Session, Ctrl+Enter / Alt+Enter / Ctrl+D sets the global default. `/agents` lists the Session's sub-agents (started by `agents.run`) and `/agents <n>` shows one's stored transcript; their steps also show live as activity. `/cancel` stops just the running Tool the daemon flagged as pending or stalled, without stopping the Run. `/resend` aborts and re-sends a stalled provider request (clients flag a stream that has sent nothing for `provider.stall_notice_secs`, default 120). `/resume` continues a Session whose last Run was interrupted (crash, Esc) or paused (provider error) from its transcript; `/abandon` drops it. On terminals at least 110 columns wide, `/panes` switches to a dashboard with Sessions and Tools beside the chat and Metrics on the right; the choice is remembered (`tui` settings).

#### Main Chat and Topic Chats

On a daemon with topic orchestration, `/main` opens the canonical Main Chat and
`/topics` browses it and nested Topic Chats, including their harness
conversations. Terminals at least 110 columns wide show a conversation rail;
narrower terminals use a picker and breadcrumb.

Use `/main backend` in the terminal, **Main Chat backend** in the desktop or
relay web Main Chat, or `formless main backend` to inspect the saved choice and
available backends. **Inherit** keeps the current Session model behavior.
You can select a configured provider with optional model and reasoning-effort
overrides, or a daemon-supported local harness. A harness that is not ready
rejects sends without substituting another backend. Harnesses accept new text
only (no attachments or steering), receive only the Main Chat transcript,
and relay questions and permission requests back to you. Topic Chat routing
does not change.

Use `/topic` to propose moving selected genuine user requests from Main Chat
into a Topic Chat. A proposal moves nothing until you approve it; work starts
only after a separate confirmation unless you explicitly approve and start
together. The Main Chat assistant may suggest a proposal, but that suggestion
is not consent. Topic Chats accept messages; harness transcripts are
read-only, and ready features are never merged automatically. `/topic ID`
reviews a proposal or shows a topic's features, attempts and revisions. Use
`/topic start`, `/topic cancel`, `/topic recover`, `/topic retry TOPIC FEATURE`
or `/topic reject TOPIC REASON` to manage the topic.

The orchestrator can answer an ordinary harness question only with
storage-verified evidence: a complete quote of the active feature's approved
request in the narrow single-choice `stem: option` form, or a trusted prior
human-answer receipt from the same attempt for identical questions and
answers. It does not paraphrase or infer a general semantic answer. If evidence
is missing, changed or ambiguous, the question stays pending for your direct
answer. The model cannot answer user-only questions or permissions; permission
decisions remain under your control.

You can add your own prompt commands as Markdown files in the current profile's
`<config>/commands/` directory or the current project's `.formless/commands/`.
For example, `workflow.md` defines `/workflow`:

```markdown
---
description: Review the proposed change
---
Review $ARGUMENTS. Focus first on $1, and include @notes/review-checklist.md.
```

Invoke `/workflow src/parser.rs` to supply its first argument. Use `$$` for a
literal dollar: `$$5 per run`, `$$9.99`, and `echo $$1` become `$5 per run`,
`$9.99`, and `echo $1`. A referenced `$1`–`$9` without enough arguments stops
expansion with the filename, missing position and escape guidance before send;
the message is not sent with text silently removed.

New files and edits appear without restarting the TUI or plain client. `/help` lists them under
**Yours**; project files override global files with the same name. Builtin
commands always win, and shadowed files and malformed templates are reported.
Expansion substitutes arguments into prompt text; the full TUI then uses its
existing `@path` attachment flow. Template text does not execute shell commands
or Tools by itself.
Plain mode (`--plain` or `formless chat`) loads the current Session's templates
and sends expanded bodies as literal messages, including bodies beginning with
`/`. Its `//` escape still sends raw slash text. Invalid-template invocations
name the file and reason; typo suggestions include local and shadowed templates.
Diagnostics are announced once while present and can be announced again after
a repaired file becomes invalid again.

TUI file completion quotes spaced or delimited names, for example
`@"my notes/report.md"`. Inside that reserved form, only `\"` and `\\`
are escapes; whitespace is literal. Unclosed or invalid quoted mentions stay
plain text and attach no first-word fragment. Directory completion keeps the
cursor before its closing quote for child completion. Chips, forecast and
submission all decode the same path; existing file eligibility rules apply.

`formless commands list` shows the available metadata, diagnostics and shadowed
files; `formless commands list --json` returns the same shared metadata projection
used by the TUI. Listing is read-only: it does not start a daemon, create a
profile, expand a template or open its referenced attachments.
TUI/plain `/help`, the TUI picker and `commands list` show required positions
(for example `/workflow <arg1>`) and `[arguments…]` when `$ARGUMENTS` is used.
Positions count whitespace-separated words; `$ARGUMENTS` may be empty. JSON
metadata includes `max_position` and `uses_arguments`. Escaped `$$5` and literal
multi-digit `$10` do not add required arguments.

For red/green colour blindness, set `formless config set tui.theme colorblind`
(or choose **colorblind** in Settings). The TUI and CLI share an Okabe–Ito-derived
blue/orange status palette that adapts to the terminal background. Diffs keep bold `+`/`-` gutters; CLI status cells add
`ok`, `warn`, and `fail` markers when this theme is active or colour is disabled.
`NO_COLOR` suppresses ANSI styling without removing status information; printable
Unicode is preserved. JSON output retains its original machine-readable values.

For interface language, open **Settings → Interface → language** in the TUI,
or run `formless config set tui.language th` for Thai. Use
`formless config set tui.language en` for English or
`formless config set tui.language auto` to follow the locale. Auto uses the
first nonempty `LC_ALL`, `LC_MESSAGES`, then `LANG` value in the TUI, and the
browser language on the web; Thai locales select Thai, otherwise English.
Supported interface text and date/number formatting follow that choice, but
catalog coverage is partial: untranslated strings and some daemon messages
remain English. Web dates retain the Gregorian calendar and Latin digits.
This setting does not translate agent answers; asking for answers in Thai is
a separate preference. See [SPEC §10.8](../SPEC.md#108-language) for details.



Approvals for broad Capabilities appear as a modal — the request names the exact host, path or binary, and the agent waits. Purging a Tool's code and history is user-only and lives in the Tools view ([§5.3](architecture.md#lifecycle-and-versioning)).

#### Plain terminals and screen readers

After first-run setup, `formless --plain` opens a line-oriented client with
no alternate screen, redraws or spinners. `formless chat` opens the same
client as its own command; use `formless chat --new` for a fresh Session or
`formless chat --session ID` to select one. This suits screen readers, dumb
terminals and SSH sessions with limited terminal support. Events print in
order, and approvals and questions are answered on the next line. See the
[plain-client commands](plain.md), or type `/help` in that client.

### Headless runs (no TUI)

`formless run` is the scriptable entry point: it starts the daemon if needed, streams the answer to stdout, and exits.

```bash
formless run "summarise the CHANGELOG of this repo"        # ephemeral Session
formless run "now group those by author" --session 3f9c    # continue a Session
formless run -c "and sort them by date"                     # continue the last `formless run` Session
formless run "audit my Downloads folder" --json            # one JSON event per line
formless run "one-line summary of README.md" --output final -q   # just the answer on stdout
formless run "triage this" --output json                   # {session_id, status, answer, result, error}
formless run "clean up the temp files" --yes               # auto-approve sensitive Tools
formless run "classify these titles" --model small-model --effort low
formless run "plan the migration" --role planner           # a configured Role's provider and model
formless run --estimate "plan the migration" --json       # local preview; no Run or provider call
formless run "fix the failing test" --cwd ~/src/app        # act on that folder (default: where you run it)
formless run "what's wrong here?" --attach shot.png --attach app.log   # files and images
git diff | formless run "review this"                      # piped input goes along as stdin.txt
cat task.md | formless run -                               # `-` reads the task itself from stdin
formless run "classify these titles" --schema result.schema.json --system "Use concise labels" --max-usd 0.10 --output final --output-file result.json
formless run "return an object" --schema object.schema.json --output json | jq '.result'
```

`--schema` validates the final answer and allows one paid repair; a second
mismatch exits 3 without emitting invalid JSON in final/json mode. `--schema -`
uses stdin for the schema instead of a task or attachment. `--system` applies
only to this Run. `--max-usd` replaces its Run cap, including an intentional
increase, while the profile's daily cap still applies; zero admits no model
request. Limits are checked before each physical call, so an in-flight response
can exceed the remaining amount. Unknown spend or pricing pauses further calls.
A budget stop exits 1 and reports observed usage cost on stderr. Stopped pipeline
Runs must be resubmitted with their options; `/resume` cannot recover ephemeral
instructions. `--output-file` leaves stdout empty and atomically replaces the
file only after successful output (at most 8 MiB). An existing output keeps
its Unix access bits and group or its Windows DACL. New files are private.
A write failure leaves the previous file intact.

Schema files and `--schema -` accept one leading UTF-8 BOM; the 64 KiB input
limit includes those bytes. With `--schema --output json`, the envelope keeps
`answer` as the original validated text and adds `result` as the parsed JSON
value, for example `"result": {"n": 2}`. Consumers can read `.result` directly
without parsing the answer twice. The same envelope is written by
`--output-file`. Without a schema, or when schema validation fails, `result`
is null; a schema mismatch also keeps `answer` null.

Before sending, the TUI and web composer show a local forecast for the current
draft: prompt tokens, comparable model/role Runs, likely cost and steps, and
compaction risk. No comparable history means prompt size only; unpriced models
have no dollar range. `run --estimate` prints p50/p90 steps, tokens, cost and
duration without creating a Session or Run, uploading files, refreshing credentials
or calling a provider. It may read an already-running daemon over local IPC; it
never starts one. Offline snapshots fail closed when the profile is busy.

Set `limits.forecast_confirm_usd` to require confirmation when a priced p90 exceeds
your threshold. Headless Runs need `--yes` for that confirmation. Forecasts add no
step, token or time caps. Metrics records the immutable forecast beside actual
usage, errors and coverage; interrupted or incomplete usage stays unknown.


Piped stdin is read only when stdin isn't a terminal; from a script that leaves stdin open without writing to it, add `< /dev/null`.

Approvals are **opt-in**: by default (`security.secure = false`) Tools run without asking, and each sensitive call is recorded in the audit log (`tool.run_unasked`). With approvals on (`formless --secure`, or `security.secure = true` in `config.toml`), they are not skipped by scripting: without `--yes` a sensitive Invocation **fails closed** with a clear error instead of being silently granted. `--timeout 10m` (also `90s`, `1h30m`) interrupts a Run that takes too long. Exit codes: `0` answered, `1` Run errored/paused, `2` usage/config error, `3` Run paused because schema validation failed (see `--schema` above), `124` Run timed out (see `--timeout` above), `130` interrupted.

### Sessions from the command line

```bash
formless sessions list                     # --archived, --json
formless sessions show 3f9c                # the transcript (--json for scripts)
formless sessions search "invoice"         # across every Session
formless sessions rename 3f9c "Q3 report"
formless sessions export 3f9c -o q3.md       # infer Markdown from the filename
formless sessions import bundle.json
formless sessions rm 3f9c                  # restorable for a short while: sessions restore <id>
```

`--format` accepts `json`, `markdown` (alias `md`), or `html`. When omitted,
`.md`/`.markdown`, `.html`/`.htm`, and `.json` output extensions select their
matching format, ignoring extension case. Standard output and other extensions
still default to JSON. An explicit format conflicting with a recognized output
extension is a usage error before any Session lookup or daemon connection.
On Unix, ordinary file output uses atomic replacement with existing mode bits
applied before staging content; failed pre-commit writes leave the prior export
intact. This does not claim extended ACL or xattr preservation.
The Windows implementation uses verified user-private staging and preserves an
existing file's DACL through replacement, without ignoring ACL merge failures.
It remains pending native Windows acceptance. If an OS replacement failure
retains private recovery files, the error identifies their directory for
inspection before retrying; no destructive replacement fallback is used.
Existing symbolic links keep writing through to their targets,
and descriptor/device streams retain their existing direct-write behavior.

### Reporting a bug

```bash
formless bug-report      # writes formless-bug-report-<time>.md (secrets redacted) to attach to an issue
formless doctor --json   # the same checks, machine-readable
```

### Daemon log

```bash
formless logs --level warn --since 1h      # recent warnings and errors
formless logs --jsonl --lines 20           # one JSON object per selected entry
formless logs -f                           # follow as it's written
```

`--jsonl` is for snapshots and conflicts with `--follow`. Each record contains
an RFC 3339 `timestamp` (or `null`), the effective `filter_level` (`info`
for unknown or missing levels), and the same grouped `text` as raw output.

### Shell completions

The installer and the Homebrew formula install bash, zsh and fish completions. Otherwise:

```bash
formless completions zsh > ~/.zfunc/_formless      # also bash, fish, powershell, elvish
```

### Settings from the command line

```bash
formless config path                       # where config.toml is
formless config get tools.inline_limit     # effective value (env overrides applied)
formless config get provider.model --json # one JSON value, with its type preserved
formless config set tui.notify_min_secs 30 # validated, audited, applied at once
formless config edit                       # $EDITOR; saved only if it still parses
```

In tmux, `tui.notify` needs `set -g focus-events on` for the TUI to detect
focus loss. Desktop alerts also need `set -g allow-passthrough on` on tmux
versions with that option. See [TUI notifications in tmux](troubleshooting.md#tui-notifications-in-tmux).

`config get KEY` reads locally loaded effective configuration, including this
client's applicable environment overrides. Add `--json` for one compact JSON
value followed by a newline; strings remain quoted, including text such as
`true`, quotes and newlines. The default human output is unchanged. This is
not a snapshot of the running daemon or a whole-configuration backup. JSON
encoding preserves the selected value's type; Set still applies its supported
key validation, optional/null behavior and map merge or `--replace` rules.

Map settings (`provider.prices`, `provider.model_context_tokens`,
`provider.model_audio_input`, `provider.model_pdf_input`, `tui.colors`) merge
entries by default. Each supplied entry replaces that entry's whole value;
`null` removes that entry. Use `--replace` for whole-map replacement or clearing:

```bash
formless config set provider.prices '{"claude-sonnet-4.5":{"input":1,"output":2}}'
formless config get provider.prices.claude-sonnet-4.5
formless config set provider.model_context_tokens '{"vendor/qwen2.5":32768}'
formless config set provider.model_context_tokens.vendor/qwen2.5 null
formless config set provider.prices '{}' --replace
```

After a listed map's root, the entire suffix is one literal entry name,
including dots and slashes. Quote the key in your shell if it contains spaces.
Lists still replace their complete value. A merge needs an updated daemon;
older daemons reject it without changing the file. Restart an older daemon
after updating the binary.

### Daemon, diagnostics and reset

```bash
formless daemon start     # start it if it is not already up
formless daemon status    # pid, version, protocol, sessions, running Runs
formless daemon stop
formless daemon restart   # waits for running Runs (--force interrupts them)
formless daemon serve     # foreground, for watching logs
formless doctor           # config, dirs, keychain, endpoint + tool-calling probe
formless tools list       # registry inspection (also export / import)
formless tools show fetch_page                       # description, schemas, versions, stats
formless tools reviews                               # retained description warnings and diffs
formless tools run fetch_page --input '{"url":"https://example.com"}' --yes
formless tools run fetch_page --input-file request.json
printf '%s' '{"url":"https://example.com"}' | formless tools run fetch_page --input-file -
formless reset --force    # destructive: config, key, whole data dir
```

There is one daemon per profile, reachable over a unix socket (`<data dir>/daemon.sock`) or a named pipe on Windows. By default it exits after `daemon.idle_grace_secs` (60 s) with no clients and no pending Invocations; the next client starts it again when needed.

#### Optional per-user autostart

Preview the login entry and manager commands without installing them:

```sh
formless daemon install-service --dry-run
```

Install only when you want per-user startup at login:

```sh
formless daemon install-service
```

For a named profile, preview and install with the same profile name:

```sh
formless --profile work daemon install-service --dry-run
```

```sh
formless --profile work daemon install-service
```

Remove that profile's login entry with:

```sh
formless --profile work daemon uninstall-service
```

For the default profile, use `formless daemon uninstall-service`. These are
per-user login entries (launchd agent, systemd user unit or Windows logon task).
`formless doctor` reports installed-state diagnostics; an installed entry alone
does not establish service health. See [SPEC §9.1](../SPEC.md#91-lifecycle).

The entry uses `daemon serve --stay`, which prevents idle exit while the daemon
is running. It does not guarantee scheduled delivery through logout, sleep,
shutdown, network failure or reboot without login. Linux session/logout
lifecycle guidance is tracked separately in [#1406](https://github.com/athiwatc/formless/issues/1406).

### Browser access via the relay (v2)

```bash
formless relay enable     # prints https://…#k=… — open that URL
formless relay status     # show it again
formless relay disable --forget
```

The daemon dials out to the relay, so nothing needs an inbound port; the browser holds the pairing key in the URL fragment and the relay only ever forwards AES-256-GCM ciphertext. The web app matches the TUI feature for feature (Sessions, chat with steer/queue/stop/edit, approvals, Tools with a code editor, Metrics, Proposals, Settings) and works on phones.

The default endpoint is the Worker in [`relay/`](../relay/README.md) — self-host it and point `relay.url` at your own deployment; `node relay/dev.mjs --mock-provider` runs the whole thing locally without a provider or an account.

### Provider options (v2)

Set these options in `[provider]` to select the wire format, Tool calling mode and model routing.

| Setting | Effect |
|---|---|
| `api_style = "chat"` | use OpenAI-compatible Chat Completions (`POST {base_url}/chat/completions`), the default |
| `api_style = "responses"` | use the Responses API (`POST {base_url}/responses`) |
| `api_style = "anthropic"` | use native Anthropic Messages (`POST {base_url}/messages`) with `x-api-key` by default. The Anthropic preset selects this style. See [SPEC §4.1](../SPEC.md#41-scope) for request conversion and caching. |
| `tool_mode = "prompt"` | describe Tools in the prompt and parse a JSON tool call from the reply, for models without native tool calling (onboarding detects this) |
| `fallback_models = [...]` | models to fail over to, in order, when the selected one errors |
| `model = "auto"` + `routing_models = [...]` | pick a model per Run from **measured** local success and latency; `routing_fast_models` / `routing_strong_models` split simple from complex tasks |

Subscription providers sign in with OAuth2 + PKCE instead of an API key:

```toml
[provider.auth]
mode = "oauth"
authorize_url = "https://provider.example/oauth/authorize"
token_url = "https://provider.example/oauth/token"
client_id = "your-public-client-id"
```

```bash
formless auth login     # opens the browser, listens on 127.0.0.1 for the callback
formless auth status    # mode and token expiry, never the token itself
formless auth logout
```

Tokens are kept in the keychain (plus a `0600` fallback file) and refreshed automatically before each provider call.

### Profiles and environment variables

`--profile <name>` is a global flag: it gives an isolated daemon, data dir and config dir, which is the safe way to try dangerous Tools.

Names use 1–64 ASCII letters, digits, `-` or `_`, starting with a letter or digit.
Default-owned names such as `commands`, `artifacts`, `backups`, `mcp`, `models`,
`skills`, `api_key` and `relay_secret` are reserved regardless of case; the full
list is in SPEC §9.4. Case collisions and linked or overlapping profile paths are
refused before setup or removal. `profiles list` warns about unsafe legacy
folders and preserves them. Back up the reported paths outside the Formless
roots and recover them manually before reset/uninstall; no legacy folder is
automatically moved or deleted. Your ordinary `commands/*.md` templates remain
part of the default profile, never a removable profile named `commands`.

```bash
formless --profile sandbox
formless profiles list           # every profile, whether its daemon runs, where its data is
formless profiles rm sandbox     # stops its daemon, deletes its data, config and secrets
```

| Variable | Effect |
|---|---|
| `FORMLESS_API_KEY` | provider key; wins over the keychain and fallback file |
| `FORMLESS_API_KEY_<NAME>` | key for a named provider; uppercase the name and replace non-alphanumeric characters with `_` |
| `FORMLESS_API_KEY_FILE`, `FORMLESS_API_KEY_<NAME>_FILE` | paths to files containing the default/named provider key |
| `FORMLESS_OAUTH_ACCESS_TOKEN` | OAuth mode: use this access token instead of stored tokens |
| `FORMLESS_OAUTH_CLIENT_SECRET` | OAuth mode: client secret for providers that need one (never stored in config) |
| `FORMLESS_BASE_URL`, `FORMLESS_MODEL` | override the configured endpoint/model for this process; trimmed, empty/whitespace-only values ignored |
| `FORMLESS_DATA_DIR`, `FORMLESS_CONFIG_DIR` | relocate state (also how the auto-started daemon inherits them) |
| `FORMLESS_BIN` | path to the `formless` CLI the desktop app uses to start the daemon; checked after the CLI next to the app and before PATH |
| `FORMLESS_IDLE_GRACE_SECS` | idle daemon timeout as a nonnegative whole number of seconds; invalid or empty values are errors |
| `FORMLESS_SECURE` | process override for `security.secure`: `1/true/yes/on` or `0/false/no/off`, case insensitive; blank is ignored, invalid values are errors |
| `FORMLESS_DAEMON_STAY` | keep the daemon alive as with `daemon serve --stay`; presence enables it |
| `FORMLESS_RELAY_SECRET` | pairing secret override for the relay |
| `RUST_LOG` | log filter for the daemon, e.g. `RUST_LOG=debug` |

Only the four config overrides listed in SPEC §13 are supported; setting an
arbitrary `FORMLESS_*` name does not override a config key. A setting saved
under the daemon's `FORMLESS_MODEL` or `FORMLESS_IDLE_GRACE_SECS` override is
persisted for later use. `config set` reports the saved value, the daemon's
effective value and the overriding variable; `config get` reads the file with
the client's own environment applied.

### Troubleshooting

Start with `formless doctor`, inspect the daemon log with `formless logs`,
and use `formless bug-report` to collect a redacted report for an issue.
The maintained [Troubleshooting guide](troubleshooting.md) covers startup,
keychain, provider and recovery problems by symptom. Open that topic from the
CLI with `formless docs troubleshooting`.

| Symptom | Fix |
|---|---|
| `not configured; run formless for the wizard` | headless runs need a configured provider first — launch the TUI once |
| Onboarding reports neither native nor prompt-based tool calling | Press Enter to pick another model in the wizard. For a saved provider, check a candidate with `/test` or `formless providers test default --model MODEL` and [set `provider.tool_mode = "prompt"`](#provider-options-v2) if the model works through the fallback. |
| TUI renders garbage or does not respond | it needs a real TTY; run it interactively rather than through a pipe |

### Updating

`formless update` bounds its release lookup to 15 seconds overall (5 seconds to connect), including the response body, and reports an actionable timeout error. It lists every GitHub release newer than the running binary, oldest first, with its release notes, then prints the command that upgrades it the way it was installed (`brew upgrade formless`, `cargo install … --force`, or the install script). `formless update --check` (or `update check`) stops after the report. Neither downloads or replaces anything. Re-run `scripts/install.sh` (it fetches the latest release, or `FORMLESS_VERSION`, and verifies the checksum), or rebuild from source. Your data dir and Tools are untouched by an update — config is migrated automatically, with a backup taken first ([§4](#data-config-and-profiles)).

---

## Data, config and profiles

* **Data dir:** `~/Library/Application Support/formless` (macOS), `$XDG_DATA_HOME/formless` (Linux), `%APPDATA%\formless` (Windows). Config lives in the matching config dir (`config.toml` plus the OS keychain for secrets).
* Contents: the SQLite DB (Sessions, Runs, Tool metadata and version index, Invocations, metric tallies, compaction checkpoints, approvals, audit events), Tool version files, artifacts, rotated daemon logs and the audit log.
* The schema is versioned; the daemon auto-migrates on start and **takes a backup before migrating**. On corruption it refuses to guess: it stops, reports, and points at the backup.
* `formless backup` saves the **database only**. Copy `artifacts/` and `skills/` separately if you need their files; `config.toml`, credential files and OS keyring entries are excluded. The database itself may contain sensitive conversation or Tool data. Separate live folder copies are not a coordinated snapshot.
* `formless restore FILE` preserves a safety copy and replaces the database with the daemon stopped. It warns about missing, unreadable or foreign artifact/skill references without deleting their rows. Recorded paths are not rewritten: copying companion folders to another profile or machine does not automatically repair absolute paths. The warning checks availability, not file contents or whole-profile consistency.

Backup inspection is available with `formless backup list` and
`formless backup verify FILE`. Use `formless backup restore FILE --dry-run`
to inspect the stopped-daemon restore plan, and `formless doctor --restore-drill`
for an isolated offline rehearsal of the newest snapshot. Snapshots contain the
database only: copy artifact/skill files separately; configuration, credential
files and OS keyring data are excluded. Recorded paths are not relocated.

* `FORMLESS_DATA_DIR` / `FORMLESS_CONFIG_DIR` relocate both dirs; `--profile <name>` gives an isolated daemon plus data/config dirs — intended for testing dangerous Tools.

---
