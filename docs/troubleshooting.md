# Troubleshooting

Start with **`formless doctor`**: it checks the config, the daemon, the
database, the provider, approvals, backups and the login service, and says
what to do for each problem it finds (`formless doctor --fix` repairs what
it can). `formless logs` shows the daemon's log, and `formless bug-report`
collects both (with secrets removed) for an issue.

## The daemon won't start

- **"daemon did not accept connections within Ns"**: the first start after
  an upgrade may migrate and back up a large database. Wait and retry, or
  run `formless config edit` and raise `start_timeout_secs` in the existing
  `[daemon]` section (or add that section if absent):

  ```toml
  [daemon]
  start_timeout_secs = 180
  ```

  `config edit` works while the daemon is down. `formless config set` needs
  the daemon and cannot change this key. If the timeout error says the
  config could not be loaded, repair it with `formless config edit`; that
  start attempt used the default 60 seconds. `formless logs` shows the
  daemon's log.
- **Another version is running**: `formless daemon restart` (it waits for
  running Runs; `--force` interrupts them). The TUI's `/restart-daemon`
  does the same.
- **"database schema vN is newer than this build"**: a newer Formless used
  this data folder. Upgrade, or restore an older backup
  (`formless restore FILE`, see `formless storage`).

## Stale socket

`formless doctor` reports a socket left by a daemon that exited. Run
`formless daemon start`; the stale socket is replaced. A socket **owned by
another user** (a daemon started with `sudo`) is reported with its owner:
stop that daemon as that user, don't start a second one.

## Keychain prompts on macOS

macOS asks once per binary for access to the stored API key. Choose
**Always Allow**. After rebuilding or upgrading, the binary changed, so it
asks again. If prompts keep coming back, re-enter the key with `/setup` in
the TUI, so the new binary owns it.

## TUI notifications in tmux

Both `tui.notify = "bell"` and `tui.notify = "desktop"` wait for the
terminal to report that the TUI lost focus. Add `set -g focus-events on`
to your tmux configuration and use an outer terminal that reports focus.
Reload the tmux configuration and reattach clients after changing this
setting.

For desktop alerts, add `set -g allow-passthrough on` on tmux versions
with that option. It forwards Formless's notification escape sequence
from a visible pane to the outer terminal. The outer terminal must
support the notification sequence.

## The model doesn't call Tools

- Test it: `/test` (TUI), the picker's **Test**, or
  `formless providers test default --model MODEL`. It reports whether
  tool calls work natively, only through the prompt-based fallback, or not
  at all.
- Pick a model with tool support (the picker shows **tools** /
  **no tools**), or set `provider.tool_mode = "prompt"` for models without
  native tool calling.

## 401 / 403 errors

The key is wrong, expired or for another endpoint. Re-enter it with
`/setup` in the TUI (named providers: Settings → Providers, or
`formless providers add NAME …` again). With OAuth:
`formless auth login`. For Azure OpenAI the key goes in the `api-key`
header: add the provider with `--azure-resource` / `--azure-deployment`.

## 429 errors and slow answers

The provider is rate-limiting. Formless waits and retries on its own (the
activity line shows "rate limited · retrying in 8s"). `/usage` shows how
many requests and tokens the endpoint says are left. Lower the pace with
`limits.*`, add `provider.fallback_models`, or use another key.

## Windows: pipe permissions

The daemon listens on a named pipe only your user can open. "Access is
denied" means a daemon from another user (or an elevated prompt) owns it:
stop it there, or start Formless from the same kind of prompt.

## The relay doesn't connect

`formless relay status` shows the pairing and the connection. Check that
`relay.url` is reachable from the daemon's machine (`formless doctor`
tests it), and that a proxy is set if you need one (`provider.proxy` or
`HTTPS_PROXY`). If an old browser link stops working, make a new one with
`formless relay enable --rotate`.

## Ctrl+Enter doesn't queue

Many terminals send Ctrl+Enter as plain Enter. Formless can distinguish
**Ctrl+Enter** only when the terminal supports the kitty keyboard protocol.
Use **Ctrl+J** or **Alt+Enter** otherwise; the TUI footer shows the binding
that works in the current terminal. `tui.queue_key` is kept only for
compatibility and does not change these bindings.

## Context pressure or failed compaction

Inspect `/context` for the Session's context window and its contents. Automatic
compaction runs near the model context limit. For on-demand compaction, wait
until the Run ends, then use `/compact` or `/compact FOCUS`. It is unavailable
while a Run is active and reports nothing to compact when no turns are new
since the last checkpoint.

For the ordinary daemon-side summarizer, an empty or output-limit-cut summary
fails before saving a checkpoint; the turns remain. Read the reported failure
and provider diagnostics before retrying. A successful summary is not lossless.
Provider-side compaction is optional and endpoint-specific. See
[README context guidance](../README.md#context-and-compaction),
[the generated commands](keys.md#session), and
[SPEC §11](../SPEC.md#11-compaction) for the existing contract.

## Still stuck?

Run `formless bug-report` and open an issue with the file it writes.
