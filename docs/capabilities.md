# What Tools can do: Capabilities

Every Tool the agent builds runs in a sandbox with **no access to anything**:
no files, no network, no programs, no environment. A Tool gets exactly the
access its **Capabilities** list, and nothing else. This page explains each
one in plain words, what can go wrong, and how much to trust.

The full technical definition is in [SPEC §7.4](../SPEC.md#74-capabilities-closed-taxonomy).

## Are you asked?

By default approvals are **off**: Tools run without asking, and every
sensitive call is recorded in the audit log (`formless approvals list`,
`/audit`). Start with `formless --secure` (or set `security.secure = true`)
to be asked before a Tool first uses a Capability. Either way:

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
| `net:<host>` | HTTP(S) requests to that host (and its subdomains) | `net:api.github.com` | Low for a known API; bare `net` (any host) is flagged **broad**: it could send your data anywhere |
| `process:<command>` | Run that program (no shell) | `process:git` | Medium: the program can do anything you can; bare `process` (any program) is **broad** |
| `env:<KEY>` | Read one environment variable | `env:GITHUB_TOKEN` | Depends on the variable: a token is as sensitive as a password |
| `clipboard:read` / `clipboard:write` | Read or set the clipboard | | Low to medium: the clipboard may hold a password you just copied |
| `notify` | Show a desktop notification | | Low |
| `llm` | Ask a model (`llm.complete`) from inside the Tool | | Costs tokens; low otherwise |
| `agent` | Start a sub-agent from inside the Tool | | Costs tokens; the sub-agent's own Tool calls are checked as usual |
| `store` / `store:<ns>` | Keep small state between runs (own, or a shared namespace) | | None: never asked about |
| `harness:<scope>` | Read or change Formless itself: MCP servers, skills, models, providers, Roles, settings | `harness:mcp` | High: every change is shown to you with a diff and needs your OK |
| `mcp:<server>` | Use an MCP server's tools | `mcp:github` | Whatever that server can do; each server is approved once |

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
  there don't ask.
- **Shared or work machines:** approvals on, no trusted folders outside
  projects, deny bare `net` and bare `process`, and never allow
  `fs:read` on your home folder.
- **Headless (`formless run`):** approvals fail closed unless you pass
  `--yes`, which approves for that Run only.

## Warning signs in an approval

- **broad access** in red: bare `net`, bare `process`, `fs:write` on your
  home folder or the whole disk, `env` for something that looks like a
  secret.
- **⚠ outside the working folder**, or a secrets folder such as `.ssh`.
- A Tool whose Capabilities **widened** since you approved it (you are
  asked again).
