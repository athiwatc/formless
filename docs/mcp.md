# Connect an MCP server

Want to use an existing integration? Ask Formless: “Connect a server that
can search my project notes, then show me what it can access.” An MCP server
is a program or remote service that supplies Tools. Its Tools appear as
`mcp__SERVER__TOOL` in Formless.

This guide covers connecting, checking and removing a server. Protocol and
configuration details are in [SPEC §7.9](../SPEC.md#79-external-mcp-servers).

## Choose how to connect

Start with the catalog:

```sh
formless mcp catalog
formless mcp add filesystem --param path=/absolute/path/to/notes
```

Replace the example path with the folder you want to expose. The catalog
flow shows the pinned command and exposure warnings before confirmation.
It may download and run a server; read the displayed command and folder
access. Secret catalog parameters use a masked prompt or `--param-env
KEY=ENVIRONMENT_VARIABLE`, rather than a secret in `--param`.

If you already have a local server, give its command after `--`:

```sh
formless mcp add notes -- node /absolute/path/to/server.js
```

Use the command and arguments supplied by your server. A local server is
an external process running as the daemon's user, with the environment
variables Formless passes to it. Its filesystem access depends on that
program and its arguments; it does not run in a generated Tool's sandbox.

For a remote server, replace this placeholder with its MCP endpoint:

```sh
formless mcp add handbook --url https://example.com/mcp
```

Servers that require OAuth authorization need a login:

```sh
formless mcp login handbook
```

Login opens the authorization flow. Static-header and environment options
are listed in `formless mcp add --help`; keep credentials out of shared
command examples and exported configuration.

## Check the connection and available Tools

```sh
formless mcp list
formless mcp test notes
formless mcp tools notes
```

Use the name you added: the catalog example is `filesystem`, while the
custom-command example is `notes`. `test` restarts the server and asks for
its Tools again. A successful connection does not establish that every
Tool will work with your data or account permissions.

Ask Formless to use a named server Tool for your task. Agent-proposed server
connections require approval of the proposed configuration. Calls follow
your current [Capability and approval settings](capabilities.md#are-you-asked).

To turn off an individual server Tool, copy its name from `mcp tools`:

```sh
formless mcp tool notes TOOL_NAME --off
```

## Diagnose or disconnect

For a local stdio server, inspect recent stderr:

```sh
formless mcp log notes --lines 50
```

Those logs live under the active profile's data directory in
`mcp/notes.log`. Remote HTTP servers do not produce that local process log.
Check the configured command or URL, authorization and reported error before
retrying; [troubleshooting](troubleshooting.md) has the general diagnostics.

```sh
formless mcp logout handbook
formless mcp remove notes
```

`logout` removes the stored OAuth tokens. `remove` disconnects the server
and retires its Tool entries while keeping their metrics. The TUI's `/mcp`
and the web Settings MCP section provide access to the same connections.
