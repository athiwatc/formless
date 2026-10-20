# Tools run in embedded QuickJS with a daemon-provided API surface

Created Tools are single-file plain JavaScript executed by embedded QuickJS-NG (via `rquickjs`), with all privileged operations supplied as Capability-gated daemon host calls (`fetch`, `fs.*` incl. streaming, `env.get`, `process.run`, `tools.call`) and no ambient authority. We considered `deno_core`/V8 (TypeScript and real Web APIs, but a heavy binary), Boa (pure Rust, less battle-tested), Lua/Rhai/Starlark (agents write them worse, hurting first-try Tool success), Wasmtime (requires compilation), and external Node/Bun processes (no single binary, weaker containment). QuickJS keeps one static Rust binary, true embed-level sandboxing, and a language the agent writes well; the cost — the daemon must spec and implement the entire host API, including streaming primitives — is accepted and documented in the bootstrap prompt.

**Status:** accepted

**Considered Options:** deno_core (V8), Boa, Lua (mlua), Rhai, Starlark, Wasmtime, Node/Bun subprocess.
