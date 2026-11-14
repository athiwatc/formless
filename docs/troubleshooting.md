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

## Still stuck?

Run `formless bug-report` and open an issue with the file it writes.
