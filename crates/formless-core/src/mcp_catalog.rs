//! A small curated catalog of popular MCP servers, and importing servers
//! already configured in other apps (SPEC §7.9).

use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::McpServerCfg;

/// One catalog entry. `{name}` placeholders in `args` / `url` are filled
/// from `params`; `env_keys` are environment variables the server needs.
pub struct Entry {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// stdio: program and arguments; empty for http.
    pub command: &'static str,
    pub args: &'static [&'static str],
    /// http: the endpoint (OAuth via `formless mcp login`).
    pub url: &'static str,
    pub params: &'static [(&'static str, &'static str)],
    pub env_keys: &'static [&'static str],
}

pub const CATALOG: &[Entry] = &[
    Entry {
        id: "filesystem",
        title: "Filesystem",
        description: "Read, write and search files in one folder",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-filesystem", "{path}"],
        url: "",
        params: &[("path", "The folder the server may use")],
        env_keys: &[],
    },
    Entry {
        id: "github",
        title: "GitHub",
        description: "Repositories, issues and pull requests (GitHub's remote server; log in with OAuth)",
        command: "",
        args: &[],
        url: "https://api.githubcopilot.com/mcp/",
        params: &[],
        env_keys: &[],
    },
    Entry {
        id: "git",
        title: "Git",
        description: "Read and search a local git repository",
        command: "uvx",
        args: &["mcp-server-git", "--repository", "{path}"],
        url: "",
        params: &[("path", "The repository folder")],
        env_keys: &[],
    },
    Entry {
        id: "fetch",
        title: "Fetch",
        description: "Fetch web pages as Markdown",
        command: "uvx",
        args: &["mcp-server-fetch"],
        url: "",
        params: &[],
        env_keys: &[],
    },
    Entry {
        id: "playwright",
        title: "Browser (Playwright)",
        description: "Drive a real browser: open pages, click, fill forms, take snapshots",
        command: "npx",
        args: &["-y", "@playwright/mcp@latest"],
        url: "",
        params: &[],
        env_keys: &[],
    },
    Entry {
        id: "postgres",
        title: "PostgreSQL",
        description: "Read-only queries and schema inspection",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-postgres", "{url}"],
        url: "",
        params: &[("url", "A connection URL, e.g. postgresql://localhost/mydb")],
        env_keys: &[],
    },
    Entry {
        id: "slack",
        title: "Slack",
        description: "Read channels and post messages with a bot token",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-slack"],
        url: "",
        params: &[],
        env_keys: &["SLACK_BOT_TOKEN", "SLACK_TEAM_ID"],
    },
    Entry {
        id: "memory",
        title: "Knowledge graph memory",
        description: "A persistent knowledge graph the agent can store and query",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-memory"],
        url: "",
        params: &[],
        env_keys: &[],
    },
    Entry {
        id: "time",
        title: "Time",
        description: "Current time and time-zone conversions",
        command: "uvx",
        args: &["mcp-server-time"],
        url: "",
        params: &[],
        env_keys: &[],
    },
    Entry {
        id: "sentry",
        title: "Sentry",
        description: "Issues and errors from Sentry (remote server; log in with OAuth)",
        command: "",
        args: &[],
        url: "https://mcp.sentry.dev/mcp",
        params: &[],
        env_keys: &[],
    },
    Entry {
        id: "linear",
        title: "Linear",
        description: "Issues and projects in Linear (remote server; log in with OAuth)",
        command: "",
        args: &[],
        url: "https://mcp.linear.app/mcp",
        params: &[],
        env_keys: &[],
    },
    Entry {
        id: "notion",
        title: "Notion",
        description: "Pages and databases in Notion (remote server; log in with OAuth)",
        command: "",
        args: &[],
        url: "https://mcp.notion.com/mcp",
        params: &[],
        env_keys: &[],
    },
];

pub fn find(id: &str) -> Option<&'static Entry> {
    CATALOG.iter().find(|e| e.id.eq_ignore_ascii_case(id))
}

/// The catalog as JSON for clients.
pub fn catalog_json() -> Value {
    json!(CATALOG
        .iter()
        .map(|e| json!({
            "id": e.id,
            "title": e.title,
            "description": e.description,
            "transport": if e.url.is_empty() { "stdio" } else { "http" },
            "target": target(e),
            "params": e.params.iter().map(|(n, d)| json!({"name": n, "description": d})).collect::<Vec<_>>(),
            "env_keys": e.env_keys,
        }))
        .collect::<Vec<_>>())
}

fn target(e: &Entry) -> String {
    if e.url.is_empty() {
        std::iter::once(e.command)
            .chain(e.args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        e.url.to_string()
    }
}

/// The server config for a catalog entry, with its `{param}`s filled.
pub fn server_config(e: &Entry, params: &Map<String, Value>) -> Result<McpServerCfg> {
    let fill = |s: &str| -> Result<String> {
        let mut out = s.to_string();
        for (name, _) in e.params {
            let key = format!("{{{name}}}");
            if out.contains(&key) {
                let v = params
                    .get(*name)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| anyhow!("{} needs {name}: {}", e.id, param_hint(e, name)))?;
                out = out.replace(&key, v);
            }
        }
        Ok(out)
    };
    let mut cfg = McpServerCfg::default();
    if e.url.is_empty() {
        cfg.transport = "stdio".into();
        cfg.command = e.command.to_string();
        cfg.args = e.args.iter().map(|a| fill(a)).collect::<Result<_>>()?;
        cfg.env_keys = e.env_keys.iter().map(|k| k.to_string()).collect();
    } else {
        cfg.transport = "http".into();
        cfg.url = fill(e.url)?;
    }
    Ok(cfg)
}

fn param_hint(e: &Entry, name: &str) -> &'static str {
    e.params
        .iter()
        .find(|(n, _)| *n == name)
        .map_or("", |(_, d)| d)
}

/// A server found in another app's config, ready to add.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// "Claude Desktop", "Cursor", "Cursor (project)", "VS Code (project)".
    pub source: String,
    pub file: PathBuf,
    pub name: String,
    pub server: McpServerCfg,
    /// Environment values that could not be imported (`${input:…}`
    /// placeholders): set them before use.
    pub missing_env: Vec<String>,
}

impl Candidate {
    pub fn to_json(&self) -> Value {
        json!({
            "source": self.source,
            "file": self.file.display().to_string(),
            "name": self.name,
            "transport": self.server.transport,
            "target": if self.server.transport == "http" {
                self.server.url.clone()
            } else {
                std::iter::once(self.server.command.clone())
                    .chain(self.server.args.clone())
                    .collect::<Vec<_>>()
                    .join(" ")
            },
            "env": self.server.env.keys().collect::<Vec<_>>(),
            "missing_env": self.missing_env,
            "server": serde_json::to_value(&self.server).unwrap_or_default(),
        })
    }
}

/// Config files other apps keep MCP servers in: Claude Desktop, Cursor
/// (global and this project's) and VS Code (this project's).
pub fn sources(home: &Path, project: Option<&Path>) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let claude = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Claude/claude_desktop_config.json")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join("Claude/claude_desktop_config.json")
    } else {
        home.join(".config/Claude/claude_desktop_config.json")
    };
    out.push(("Claude Desktop".to_string(), claude));
    out.push(("Cursor".to_string(), home.join(".cursor/mcp.json")));
    if let Some(p) = project {
        out.push(("Cursor (project)".to_string(), p.join(".cursor/mcp.json")));
        out.push(("VS Code (project)".to_string(), p.join(".vscode/mcp.json")));
    }
    out
}

/// Every server in the given config files. Unreadable files are skipped.
pub fn candidates(files: &[(String, PathBuf)]) -> Vec<Candidate> {
    let mut out = Vec::new();
    for (source, file) in files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&strip_json_comments(&text)) else {
            continue;
        };
        // Claude Desktop and Cursor: `mcpServers`; VS Code: `servers`.
        let servers = v
            .get("mcpServers")
            .or_else(|| v.get("servers"))
            .and_then(Value::as_object);
        for (name, s) in servers.into_iter().flatten() {
            if let Some((server, missing_env)) = parse_server(s) {
                out.push(Candidate {
                    source: source.clone(),
                    file: file.clone(),
                    name: sanitize_name(name),
                    server,
                    missing_env,
                });
            }
        }
    }
    out
}

/// One server entry in the common JSON shape: `{command, args, env, cwd}`
/// or `{url | serverUrl, headers}` (with an optional `type`).
fn parse_server(s: &Value) -> Option<(McpServerCfg, Vec<String>)> {
    let mut cfg = McpServerCfg::default();
    let mut missing = Vec::new();
    let url = s["url"].as_str().or_else(|| s["serverUrl"].as_str());
    if let Some(url) = url {
        cfg.transport = "http".into();
        cfg.url = url.to_string();
        for (k, v) in s["headers"].as_object().into_iter().flatten() {
            match v.as_str() {
                Some(v) if !v.contains("${") => {
                    cfg.headers.insert(k.clone(), v.to_string());
                }
                _ => missing.push(k.clone()),
            }
        }
    } else {
        cfg.transport = "stdio".into();
        cfg.command = s["command"].as_str()?.to_string();
        cfg.args = s["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str().map(str::to_string))
            .collect();
        cfg.cwd = s["cwd"].as_str().map(str::to_string);
        let mut env = BTreeMap::new();
        for (k, v) in s["env"].as_object().into_iter().flatten() {
            match v.as_str() {
                Some(v) if !v.contains("${") => {
                    env.insert(k.clone(), v.to_string());
                }
                _ => missing.push(k.clone()),
            }
        }
        cfg.env = env;
    }
    cfg.disabled = s["disabled"].as_bool().unwrap_or(false);
    Some((cfg, missing))
}

/// Config names Formless accepts: letters, digits, `-` and `_`.
fn sanitize_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "server".into()
    } else {
        s
    }
}

/// VS Code's JSON allows `//` and `/* */` comments; drop them (outside
/// strings) so serde can parse it.
fn strip_json_comments(text: &str) -> String {
    let (mut out, mut chars) = (String::with_capacity(text.len()), text.chars().peekable());
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_str = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_entries_fill_their_params() {
        let fs = find("filesystem").unwrap();
        let err = server_config(fs, &Map::new()).unwrap_err().to_string();
        assert!(err.contains("needs path"), "{err}");
        let mut p = Map::new();
        p.insert("path".into(), json!("/work"));
        let cfg = server_config(fs, &p).unwrap();
        assert_eq!(cfg.command, "npx");
        assert_eq!(cfg.args.last().unwrap(), "/work");
        let gh = server_config(find("GitHub").unwrap(), &Map::new()).unwrap();
        assert_eq!(gh.transport, "http");
        let slack = server_config(find("slack").unwrap(), &Map::new()).unwrap();
        assert_eq!(slack.env_keys, ["SLACK_BOT_TOKEN", "SLACK_TEAM_ID"]);
        // Ids are unique.
        let mut ids: Vec<_> = CATALOG.iter().map(|e| e.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), CATALOG.len());
    }

    #[test]
    fn servers_import_from_claude_cursor_and_vscode_shapes() {
        let dir = std::env::temp_dir().join(format!("fl-mcpimp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let claude = dir.join("claude.json");
        std::fs::write(
            &claude,
            r#"{"mcpServers": {
                "files": {"command": "npx", "args": ["-y", "@x/fs", "/tmp"], "env": {"A": "1"}},
                "remote api": {"url": "https://example.com/mcp", "headers": {"Authorization": "${input:tok}"}}
            }}"#,
        )
        .unwrap();
        let vscode = dir.join("mcp.json");
        std::fs::write(
            &vscode,
            "// VS Code\n{\"servers\": {\"git\": {\"type\": \"stdio\", \"command\": \"uvx\", /* x */ \"args\": [\"mcp-server-git\"], \"env\": {\"T\": \"${input:t}\"}}}}",
        )
        .unwrap();
        let got = candidates(&[
            ("Claude Desktop".into(), claude),
            ("VS Code (project)".into(), vscode),
            ("Cursor".into(), dir.join("missing.json")),
        ]);
        assert_eq!(got.len(), 3);
        let files = got.iter().find(|c| c.name == "files").unwrap();
        assert_eq!(files.server.args, ["-y", "@x/fs", "/tmp"]);
        assert_eq!(files.server.env.get("A").map(String::as_str), Some("1"));
        let remote = got.iter().find(|c| c.name == "remote-api").unwrap();
        assert_eq!(remote.server.transport, "http");
        assert_eq!(remote.missing_env, ["Authorization"]);
        let git = got.iter().find(|c| c.name == "git").unwrap();
        assert_eq!(git.source, "VS Code (project)");
        assert_eq!(git.missing_env, ["T"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
