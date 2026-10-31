# PostgreSQL password constructor fixtures

`server-postgres@0.6.2` requires `pg: ^8.13.0` and constructs
`new pg.Pool({connectionString: databaseUrl})` from its first argument.
The two CommonJS files here are unmodified upstream sources for that supported
minimum driver and its connection-string parser. Their package licenses are
included. `provenance.json` records registry sources and tarball integrity.

The Node regression evaluates only these constructors/parsers with mocked
defaults and DNS. A fake package Pool captures the resulting connection
parameters; no SDK, MCP server, socket, package manager, or network is started.
Rust's production catalog configuration and the Node constructor test consume
the same `cases.json`, including percent decoding, query precedence and daemon
environment fallback.
