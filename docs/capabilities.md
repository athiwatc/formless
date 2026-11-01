# What Tools can do: Capabilities

Every Tool the agent builds runs in a sandbox with **no access to anything**:
no files, no network, no programs, no environment. A Tool gets exactly the
access its **Capabilities** list, and nothing else. This page explains each
one in plain words, what can go wrong, and how much to trust.

The full technical definition is in [SPEC §7.4](../SPEC.md#74-capabilities-closed-taxonomy).

## Are you asked?

By default ordinary capability approvals are **off**. Credential-shaped
file reads still require exact-path consent; the active Formless profile
is inaccessible to Tools. Other sensitive calls are recorded in the audit log (`formless approvals list`,
`/audit`). Start with `formless --secure` (or set `security.secure = true`)
to be asked before a Tool first uses a sensitive Capability. Not every
Capability is asked about:

<!-- Checked against the code by caps.rs `the_docs_list_what_is_asked`. -->
- Asked before first use: `net`, `process`, `fs:write:~`, `env:HOME`, `clipboard:read`, `clipboard:write`, `notify`, `llm`, `agent`, `mcp:github`, `harness:mcp`
- Never asked as ordinary grants: `fs:read:~`, `store`, `harness:read`

An ordinary project-file read stays quiet, including an input-bound read.
Reading credential paths such as `.ssh/config`, `.aws/credentials`, `.env*`,
`*.pem`, `*.key`, shell histories and native credential stores asks with the
computed concrete path, even when `security.secure = false`. Broad filesystem
grants and trusted folders do not cover this consent. The synthetic
`fs:read-sensitive:<absolute-path>` consent covers exactly that identity and
confers no filesystem, process or network access of its own.

The active daemon's data and config directories, including its key fallback,
are denied regardless of grants. Configured namespaces and resolved targets
are checked, including aliases and moves that could expose their contents.
Smoke tests and optimiser replay cannot approve credential reads. Reads in a
dry run still need consent. Explicit process operands (including a `cat`
filename relative to its `cwd`) receive the same checks; arbitrary approved
programs are not confined by an OS filesystem sandbox.

Add rules in `config.toml`; these supplement the builtins and cannot remove
the profile boundary:

```toml
[security]
sensitive_paths = [
  { kind = "subtree", value = "~/private" },
  { kind = "basename_prefix", value = "private-" },
  { kind = "basename_suffix", value = ".p12" },
]
```

Subtrees must be absolute (or start with `~/`); basename prefixes and suffixes
are literal, nonempty strings without separators or wildcards. Checks repeat
after approval and before access. They do not make filesystem checks and
opens atomic, track renamed hard links, or confine an external program's
internal file accesses.

Either way:

- **Remembered denials** always apply (`N` in the TUI, "Always deny" on the
  web).
- `/security` shows everything you have allowed in one screen;
  `/approvals` lists grants and `/revoke` takes them back.
- **Trusted folders** (`security.trusted_paths`) and **trusted hosts**
  (`security.trusted_hosts`) let writes and requests inside them go through
  without a prompt, and each is logged.

## The Capabilities

| Capability | What it allows | Example | Risk |
|---|---|---|---|
| `fs:read:<path>` | Read files under that path | `fs:read:~/notes` reads your notes | Low for a project folder; high for `~` or `/` (it could read keys and passwords) |
| `fs:write:<path>` | Create, change, rename and delete files under that path | `fs:write:./build` writes build output | Medium: changes can be undone with `/rewind` (files up to 2 MB); high for your home folder |
| `net:<host>` | HTTP(S) requests to that exact host; explicit `*.` covers proper subdomains | `net:api.github.com` | Low for a known API; bare `net` (any host) is flagged **broad**: it could send your data anywhere |
| `process:<command>` | Run that program (no shell) | `process:git` | Medium: the program can do anything you can; bare `process` (any program) is **broad** |
| `env:<KEY>` | Read one environment variable | `env:GITHUB_TOKEN` | Depends on the variable: a token is as sensitive as a password |
| `clipboard:read` / `clipboard:write` | Read or set the clipboard | | Low to medium: the clipboard may hold a password you just copied |
| `notify` | Show a desktop notification | | Low |
| `llm` | Ask a model (`llm.complete`) from inside the Tool | | Costs tokens; low otherwise |
| `agent` | Start a sub-agent from inside the Tool | | Costs tokens; the sub-agent's own Tool calls are checked as usual |
| `store` / `store:<ns>` | Keep small state between runs (own, or a shared namespace) | | None: never asked about |
| `harness:<scope>` | Read or change Formless itself: MCP servers, skills, models, providers, Roles, settings | `harness:mcp` | High: every change is shown to you with a diff and needs your OK |
| `mcp:<server>` | Use an MCP server's tools | `mcp:github` | Whatever that server can do; each server is approved once |

Static network scopes use a bare host, optionally with a port (for example
`net:api.example.com:8443`). Hostnames are stored in lowercase. `net:example.com` is exact;
`net:*.example.com` explicitly includes proper subdomains and excludes the apex.
Stored grants and configured trusted hosts follow the same rule. Public-suffix
wildcards (including `github.io` and `workers.dev`) show a warning because they
span independent tenants. Schemes,
paths and empty `net:` or `process:` scopes are rejected when the Tool is
created. Use an input-bound scope when the input contains a full URL.

### Scoped to the input

`fs:read`, `fs:write` and `net` can be tied to a field of the Tool's input,
for example `fs:read:$input.path`. The Tool then gets exactly the file or
host it was called with, nothing more. This is how one general Tool (say,
"count words in a file") stays least-privilege.

## Recommended settings

- **Trying Formless out:** defaults (approvals off, everything logged) are
  fine on a machine without secrets you care about. Run `/security` now and
  then.
- **Your everyday machine:** turn approvals on (`formless --secure`), and
  add your project folders to `security.trusted_paths` so routine writes
  there don't ask. Ordinary reads stay quiet; check the `fs:read` paths as well as the
  separate credential-read consent.
- **Shared or work machines:** approvals on, no trusted folders outside
  projects, deny bare `net` and bare `process`, and deny any Tool whose
  `fs:read` covers your home folder (ordinary reads remain quiet).
- **Headless (`formless run`):** approvals fail closed unless you pass
  `--yes`, which approves for that Run only.

## Warning signs in an approval

- **broad access** in red: bare `net`, bare `process`, `fs:write` on your
  home folder or the whole disk, `env` for something that looks like a
  secret.
- **⚠ outside the working folder**, or a secrets folder such as `.ssh`.
- A Tool whose Capabilities **widened** since you approved it (you are
  asked again).
