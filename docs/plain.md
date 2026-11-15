# Plain terminals and screen readers

After first-run setup, use `formless --plain` for a line-oriented client
without alternate-screen redraws, spinners or box drawing. `formless chat`
opens the same client as a standalone command, useful for screen readers,
dumb terminals and SSH sessions with limited terminal support.

```sh
formless --plain
formless chat --new
formless chat --session SESSION_ID
```

The default is the most recent Session. `chat --new` starts a fresh one;
`chat --session ID` selects one. Setting `tui.screen_reader = true` also
selects the plain client when launching the TUI. Complete provider setup in
the full TUI first if the plain client reports that Formless is not set up.

Events and answers print in order. Approvals list capabilities and prompt
for a line containing `y`, `o` or `n`; questions show numbered choices and
accept the next line. Prompts are answered one at a time.

## Commands

Type `/help` in the client for its current help, including any loaded
user-defined slash commands.

| Input | Existing action |
|---|---|
| `/new` | Start a Session. |
| `/sessions` | List Sessions. |
| `/switch N` | Open Session number N from the list. |
| `/stop` | Stop the Run. |
| `/files [RUN]` | Show tracked changes. |
| `/rewind [PATH]` | Attempt restoration of tracked changes and report the result. |
| `/recap` | Show the recorded recap. |
| `/docs [topic]` | Print guide addresses. |
| `/help` | Show command help. |
| `/quit`, `/exit` | Exit this client; Runs keep going in the daemon. |
| `//text` | Send `/text` as a literal message, with one leading slash removed. |

Ordinary text sends a message, or steers a Run that is already active.
Unknown slash commands are reported locally; begin with `//` to send text
that starts with `/`. Exiting the client does not stop its Run; use `/stop`
when that is the intended action.

This reference describes the existing plain client. Full-screen keys and
commands are listed separately in [Keys and commands](keys.md); see
[SPEC §10.5](../SPEC.md#105-input-and-keys) for the interface description.
