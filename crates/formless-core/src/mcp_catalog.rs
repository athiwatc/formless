//! A small curated catalog of popular MCP servers, and importing servers
//! already configured in other apps (SPEC §7.9).

use anyhow::{anyhow, bail, Context, Result};
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
    pub params: &'static [Param],
    pub env_keys: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    Path,
    Secret,
    Url,
}
pub struct Param {
    pub name: &'static str,
    pub description: &'static str,
    pub kind: ParamKind,
}
pub const CODE_WARNING: &str = "Downloads and runs code from npm/PyPI on first use.";

pub const CATALOG: &[Entry] = &[
    Entry {
        id: "filesystem",
        title: "Filesystem",
        description: "Read, write and search files in one folder",
        command: "npx",
        args: &[
            "-y",
            "@modelcontextprotocol/server-filesystem@2.1.0",
            "{path}",
        ],
        url: "",
        params: &[Param {
            name: "path",
            description: "The folder the server may use",
            kind: ParamKind::Path,
        }],
        env_keys: &[],
    },
    Entry {
        id: "github",
        title: "GitHub",
        description:
            "Repositories, issues and pull requests (GitHub's remote server; log in with OAuth)",
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
        args: &["mcp-server-git==2026.10.28", "--repository", "{path}"],
        url: "",
        params: &[Param {
            name: "path",
            description: "The repository folder",
            kind: ParamKind::Path,
        }],
        env_keys: &[],
    },
    Entry {
        id: "fetch",
        title: "Fetch",
        description: "Fetch web pages as Markdown",
        command: "uvx",
        args: &["mcp-server-fetch==2026.10.28"],
        url: "",
        params: &[],
        env_keys: &[],
    },
    Entry {
        id: "playwright",
        title: "Browser (Playwright)",
        description: "Drive a real browser: open pages, click, fill forms, take snapshots",
        command: "npx",
        args: &["-y", "@playwright/mcp@0.0.84"],
        url: "",
        params: &[],
        env_keys: &[],
    },
    Entry {
        id: "postgres",
        title: "PostgreSQL",
        description: "Read-only queries and schema inspection",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-postgres@0.6.2", "{url}"],
        url: "",
        params: &[Param {
            name: "url",
            description: "A connection URL, e.g. postgresql://localhost/mydb",
            kind: ParamKind::Secret,
        }],
        env_keys: &["PGPASSWORD"],
    },
    Entry {
        id: "slack",
        title: "Slack",
        description: "Read channels and post messages with a bot token",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-slack@2025.4.25"],
        url: "",
        params: &[],
        env_keys: &["SLACK_BOT_TOKEN", "SLACK_TEAM_ID"],
    },
    Entry {
        id: "memory",
        title: "Knowledge graph memory",
        description: "A persistent knowledge graph the agent can store and query",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-memory@2.0.1"],
        url: "",
        params: &[],
        env_keys: &[],
    },
    Entry {
        id: "time",
        title: "Time",
        description: "Current time and time-zone conversions",
        command: "uvx",
        args: &["mcp-server-time==2026.10.28"],
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
            "execution_warning": if e.url.is_empty() { CODE_WARNING } else { "" },
            "params": e.params.iter().map(|p| json!({"name": p.name, "description": p.description, "kind": p.kind})).collect::<Vec<_>>(),
            "env_keys": e.env_keys,
        }))
        .collect::<Vec<_>>())
}

pub fn target(e: &Entry) -> String {
    if e.url.is_empty() {
        std::iter::once(e.command)
            .chain(e.args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        e.url.to_string()
    }
}

/// Path checks happen in the daemon as well as in local review forms.
pub fn normalize_directory(raw: &str) -> Result<PathBuf> {
    let home = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf());
    normalize_directory_with_home(raw, home.as_deref())
}

fn verified_home(home: Option<&Path>) -> Result<PathBuf> {
    let home = home
        .filter(|h| h.is_absolute())
        .context("native home must be an absolute existing directory")?;
    let home = home
        .canonicalize()
        .context("native home cannot be resolved canonically")?;
    if !home.is_dir() {
        bail!("native home must be a directory");
    }
    Ok(home)
}

fn normalize_directory_with_home(raw: &str, home: Option<&Path>) -> Result<PathBuf> {
    let expanded = if raw == "~" || raw.starts_with("~/") || raw.starts_with("~\\") {
        verified_home(home)?.join(raw.get(2..).unwrap_or(""))
    } else {
        PathBuf::from(raw)
    };
    let path = expanded
        .canonicalize()
        .map_err(|_| anyhow!("path must name an existing accessible directory"))?;
    if !path.is_dir() {
        bail!("path must be a directory");
    }
    Ok(path)
}

pub fn path_exposure(path: &Path) -> Option<String> {
    let home = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf());
    path_exposure_with_home(path, home.as_deref())
}

fn path_exposure_with_home(path: &Path, home: Option<&Path>) -> Option<String> {
    let warning = |reason: &str| {
        Some(format!(
            "{} {reason}; confirm this exposure explicitly",
            path.display()
        ))
    };
    let home = match verified_home(home) {
        Ok(home) => home,
        Err(_) => return warning("exposure cannot be narrowed because native home is unverified"),
    };
    let path = match path.canonicalize() {
        Ok(path) if path.is_absolute() && path.is_dir() => path,
        _ => return warning("is not a verified canonical directory"),
    };
    if path.parent().is_none() || home.starts_with(&path) {
        return warning("exposes a filesystem root or home folder");
    }
    if crate::daemon::credential_path(&path).is_some()
        || path.components().any(|part| part.as_os_str() == ".config")
    {
        return warning("exposes a credential folder");
    }
    // Home-relative locations also recognize parents such as Library before
    // reaching its nested Keychains child. Missing known locations remain
    // lexical future credential roots; other resolution errors require review.
    let mut roots: Vec<_> = crate::daemon::CREDENTIAL_DIRS
        .iter()
        .map(|(name, _)| home.join(name))
        .collect();
    roots.extend(
        [
            ".config",
            ".config/gcloud",
            "Library/Keychains",
            "AppData/Roaming/gcloud",
            "AppData/Local/gcloud",
        ]
        .map(|name| home.join(name)),
    );
    let mut canonical_roots = Vec::new();
    for known in roots {
        let known = match known.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => known,
            Err(_) => return warning("has an unverifiable relationship to credential folders"),
        };
        if path.starts_with(&known) || known.starts_with(&path) {
            return warning("exposes a credential folder or its ancestor");
        }
        canonical_roots.push(known);
    }
    let mut pending = vec![(path.clone(), 0usize)];
    let mut visited = std::collections::HashSet::from([path.clone()]);
    let mut inspected = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(_) => return warning("cannot be fully inspected for credential exposure"),
        };
        for entry in entries {
            inspected += 1;
            if inspected > 1024 {
                return warning("has too many descendants to rule out credential exposure");
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    return warning(
                        "has an unreadable descendant whose credential exposure is unknown",
                    )
                }
            };
            let entry_path = entry.path();
            if crate::daemon::credential_path(&entry_path).is_some()
                || entry.file_name() == ".config"
            {
                return warning("contains a credential path");
            }
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(_) => {
                    return warning(
                        "has an unreadable descendant whose credential exposure is unknown",
                    )
                }
            };
            let nested = if kind.is_symlink() {
                let target = match entry_path.canonicalize() {
                    Ok(target) => target,
                    Err(_) => {
                        return warning(
                            "contains an unresolved link whose credential exposure is unknown",
                        )
                    }
                };
                if crate::daemon::credential_path(&target).is_some()
                    || canonical_roots
                        .iter()
                        .any(|root| target.starts_with(root) || root.starts_with(&target))
                {
                    return warning("contains a link to a credential path");
                }
                if !target.starts_with(&path) {
                    return warning(
                        "contains an external link whose credential exposure is unknown",
                    );
                }
                target.is_dir().then_some(target)
            } else {
                kind.is_dir().then_some(entry_path)
            };
            if let Some(nested) = nested {
                if visited.insert(nested.clone()) {
                    if depth >= 16 {
                        return warning("is too deep to rule out credential exposure");
                    }
                    pending.push((nested, depth + 1));
                }
            }
        }
    }
    None
}

fn prepare(e: &Entry, params: &Map<String, Value>) -> Result<(McpServerCfg, Vec<String>)> {
    let mut values = BTreeMap::new();
    let mut warnings = Vec::new();
    for p in e.params {
        let value = params
            .get(p.name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("{} needs {}: {}", e.id, p.name, p.description))?;
        let value = match p.kind {
            ParamKind::Path => {
                let path = normalize_directory(value)?;
                if let Some(warning) = path_exposure(&path) {
                    warnings.push(warning);
                }
                path.to_string_lossy().into_owned()
            }
            ParamKind::Secret | ParamKind::Url => {
                reqwest::Url::parse(value).map_err(|_| anyhow!("{} needs a valid URL", p.name))?;
                value.to_string()
            }
        };
        values.insert(p.name, value);
    }
    let fill = |s: &str| {
        values
            .iter()
            .fold(s.to_string(), |s, (k, v)| s.replace(&format!("{{{k}}}"), v))
    };
    let mut cfg = if e.url.is_empty() {
        McpServerCfg {
            transport: "stdio".into(),
            command: e.command.into(),
            args: e.args.iter().map(|a| fill(a)).collect(),
            env_keys: e.env_keys.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    } else {
        McpServerCfg {
            transport: "http".into(),
            url: fill(e.url),
            ..Default::default()
        }
    };
    if e.id == "postgres" {
        postgres_password_env(&mut cfg)?;
    }
    Ok((cfg, warnings))
}

/// server-postgres 0.6.2 passes this URL to pg.Pool. pg's parsed password
/// falls back to PGPASSWORD when the URL has none; keep credentials out of argv.
fn postgres_password_env(cfg: &mut McpServerCfg) -> Result<()> {
    let argument = cfg.args.last_mut().context("missing PostgreSQL URL")?;
    let mut url = reqwest::Url::parse(argument).context("invalid PostgreSQL URL")?;
    // pg-connection-string keeps the last query value and falls back to
    // userinfo when that value is empty. Query pairs are already percent-decoded.
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let query_password = pairs
        .iter()
        .filter(|(k, _)| k == "password")
        .next_back()
        .map(|(_, v)| v.clone())
        .filter(|v| !v.is_empty());
    let password = match query_password {
        Some(value) => Some(value),
        None => url
            .password()
            .map(decode_password)
            .transpose()?
            .filter(|v| !v.is_empty()),
    };
    url.set_password(None)
        .map_err(|_| anyhow!("invalid PostgreSQL URL credentials"))?;
    let pairs: Vec<_> = pairs
        .into_iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("password"))
        .collect();
    if pairs.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    *argument = url.to_string();
    if let Some(password) = password {
        if password.contains('\0') {
            bail!("PostgreSQL password cannot contain NUL");
        }
        cfg.env.insert("PGPASSWORD".into(), password);
    }
    Ok(())
}

fn decode_password(raw: &str) -> Result<String> {
    let mut bytes = Vec::with_capacity(raw.len());
    let mut rest = raw.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if byte == b'%' {
            let pair = rest
                .get(1..3)
                .and_then(|b| std::str::from_utf8(b).ok())
                .and_then(|s| u8::from_str_radix(s, 16).ok())
                .context("invalid PostgreSQL password encoding")?;
            bytes.push(pair);
            rest = &rest[3..];
        } else {
            bytes.push(byte);
            rest = tail;
        }
    }
    String::from_utf8(bytes).context("invalid PostgreSQL password encoding")
}

pub fn server_config(e: &Entry, params: &Map<String, Value>) -> Result<McpServerCfg> {
    let (cfg, warnings) = prepare(e, params)?;
    if !warnings.is_empty() && params.get("confirm_exposure").and_then(Value::as_bool) != Some(true)
    {
        return Err(anyhow!(
            "{}; set confirm_exposure=true only after reviewing it",
            warnings.join("; ")
        ));
    }
    Ok(cfg)
}

pub fn catalog_preview(e: &Entry, params: &Map<String, Value>) -> Result<Value> {
    let (cfg, warnings) = prepare(e, params)?;
    Ok(json!({"target": safe_target(&cfg), "warnings": warnings,
        "execution_warning": if e.url.is_empty() { CODE_WARNING } else { "" }}))
}

fn safe_url(raw: &str) -> String {
    if let Ok(mut url) = reqwest::Url::parse(raw) {
        if url.password().is_some() {
            let _ = url.set_password(Some("REDACTED"));
        }
        let pairs: Vec<_> = url
            .query_pairs()
            .map(|(k, v)| {
                let secret = [
                    "password",
                    "token",
                    "secret",
                    "key",
                    "credential",
                    "authorization",
                ]
                .iter()
                .any(|s| k.to_ascii_lowercase().contains(s));
                (
                    k.into_owned(),
                    if secret {
                        "REDACTED".into()
                    } else {
                        v.into_owned()
                    },
                )
            })
            .collect();
        if !pairs.is_empty() {
            url.query_pairs_mut().clear().extend_pairs(pairs);
        }
        return url.to_string();
    }
    raw.to_string()
}

/// Redact known literal secrets even when too short for the general scanner.
pub fn redact_server_text(cfg: &McpServerCfg, text: &str) -> String {
    let mut out = text.to_string();
    for value in cfg.args.iter().chain(std::iter::once(&cfg.url)) {
        if let Ok(url) = reqwest::Url::parse(value) {
            out = out.replace(value, &safe_url(value));
            if let Some(password) = url.password().filter(|p| !p.is_empty()) {
                out = out.replace(password, "[REDACTED]");
                let bytes = password.as_bytes();
                let mut decoded = Vec::new();
                let mut i = 0;
                while i < bytes.len() {
                    if bytes[i] == b'%' && i + 2 < bytes.len() {
                        if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                                decoded.push(byte);
                                i += 3;
                                continue;
                            }
                        }
                    }
                    decoded.push(bytes[i]);
                    i += 1;
                }
                if let Ok(secret) = String::from_utf8(decoded) {
                    if !secret.is_empty() {
                        out = out.replace(&secret, "[REDACTED]");
                    }
                }
            }
            for (key, secret) in url.query_pairs() {
                if !secret.is_empty()
                    && [
                        "password",
                        "token",
                        "secret",
                        "key",
                        "credential",
                        "authorization",
                    ]
                    .iter()
                    .any(|s| key.to_ascii_lowercase().contains(s))
                {
                    out = out.replace(secret.as_ref(), "[REDACTED]");
                }
            }
        }
    }
    for value in cfg
        .env
        .values()
        .chain(cfg.headers.values())
        .filter(|v| !v.is_empty())
    {
        out = out.replace(value, "[REDACTED]");
    }
    for key in &cfg.env_keys {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                out = out.replace(&value, "[REDACTED]");
            }
        }
    }
    crate::secrets::redact(&out).0
}

pub fn safe_target(cfg: &McpServerCfg) -> String {
    let target = if cfg.transport == "http" {
        cfg.url.clone()
    } else {
        std::iter::once(cfg.command.clone())
            .chain(cfg.args.clone())
            .collect::<Vec<_>>()
            .join(" ")
    };
    redact_server_text(cfg, &target)
}

/// A server found in another app's config, ready to add.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// "Claude Desktop", "Cursor", "Cursor (project)", "VS Code (project)".
    pub source: String,
    pub file: PathBuf,
    pub name: String,
    pub original_name: String,
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
            "target": safe_target(&self.server),
            "original_name": self.original_name,
            "env": self.server.env.keys().collect::<Vec<_>>(),
            "headers": self.server.headers.keys().collect::<Vec<_>>(),
            "secret_storage": "Literal env/header values are stored in plaintext config.toml unless skipped.",
            "missing_env": self.missing_env,
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
                    original_name: name.clone(),
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
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
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
        let _env = crate::credential_env::EnvGuard::isolated();
        let fs = find("filesystem").unwrap();
        let err = server_config(fs, &Map::new()).unwrap_err().to_string();
        assert!(err.contains("needs path"), "{err}");
        let mut p = Map::new();
        let path = std::env::temp_dir().canonicalize().unwrap();
        p.insert("path".into(), json!(path));
        p.insert("confirm_exposure".into(), json!(true));
        let cfg = server_config(fs, &p).unwrap();
        assert_eq!(cfg.command, "npx");
        assert_eq!(cfg.args.last().unwrap().as_str(), path.to_str().unwrap());
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
        let _env = crate::credential_env::EnvGuard::isolated();
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
        let public = files.to_json();
        assert!(public.get("server").is_none());
        let remote = got.iter().find(|c| c.name == "remote-api").unwrap();
        assert_eq!(remote.server.transport, "http");
        assert_eq!(remote.missing_env, ["Authorization"]);
        let git = got.iter().find(|c| c.name == "git").unwrap();
        assert_eq!(git.source, "VS Code (project)");
        assert_eq!(git.missing_env, ["T"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_stdio_catalog_package_has_an_exact_version() {
        for entry in CATALOG.iter().filter(|e| !e.command.is_empty()) {
            assert!(!entry.args.iter().any(|a| a.contains("@latest")));
            let spec = entry.args.iter().find(|a| **a != "-y").unwrap();
            let version = if entry.command == "npx" {
                spec.rsplit_once('@').map(|(_, v)| v)
            } else {
                spec.split_once("==").map(|(_, v)| v)
            }
            .expect("package must be pinned");
            assert!(
                version.split('.').count() >= 3
                    && version.chars().all(|c| c.is_ascii_digit() || c == '.'),
                "{spec}"
            );
        }
    }

    #[test]
    fn postgres_passwords_use_private_env_and_never_process_arguments() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let cases: Vec<Value> =
            serde_json::from_str(include_str!("../tests/fixtures/postgres-env/cases.json"))
                .unwrap();
        for case in cases {
            let params = Map::from_iter([("url".into(), case["input"].clone())]);
            let cfg = server_config(find("postgres").unwrap(), &params).unwrap();
            assert_eq!(cfg.args.last().unwrap(), case["argument"].as_str().unwrap());
            assert_eq!(
                cfg.env.get("PGPASSWORD").map(String::as_str),
                case["literal_password"].as_str()
            );
            assert!(cfg.env_keys.iter().any(|key| key == "PGPASSWORD"));
            let url = reqwest::Url::parse(cfg.args.last().unwrap()).unwrap();
            assert!(url.password().is_none());
            assert!(!url
                .query_pairs()
                .any(|(key, _)| key.eq_ignore_ascii_case("password")));
        }
        for value in [
            "postgresql://u:%GG@h/db",
            "postgresql://u:%ff@h/db",
            "postgresql://u@h/db?password=%00",
        ] {
            assert!(server_config(
                find("postgres").unwrap(),
                &Map::from_iter([("url".into(), json!(value))])
            )
            .is_err());
        }
    }

    #[test]
    fn short_url_passwords_and_import_values_never_appear_in_public_review() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let postgres = server_config(
            find("postgres").unwrap(),
            &Map::from_iter([("url".into(), json!("postgresql://u:pw@h/db"))]),
        )
        .unwrap();
        assert!(!safe_target(&postgres).contains("pw"));
        assert!(
            !redact_server_text(&postgres, "password pw and postgresql://u:pw@h/db").contains("pw")
        );
        let encoded = McpServerCfg {
            args: vec!["postgresql://u:a%20b@h/db?password=xyz".into()],
            ..Default::default()
        };
        let safe = redact_server_text(
            &encoded,
            "a b a%20b xyz postgresql://u:a%20b@h/db?password=xyz",
        );
        assert!(!safe.contains("a b") && !safe.contains("a%20b") && !safe.contains("xyz"));
        let candidate = Candidate {
            source: "test".into(),
            file: PathBuf::from("config.json"),
            name: "server".into(),
            original_name: "server".into(),
            missing_env: vec![],
            server: McpServerCfg {
                env: BTreeMap::from([("K".into(), "s3cr3t".into())]),
                headers: BTreeMap::from([("Authorization".into(), "header-secret".into())]),
                ..postgres
            },
        };
        let public = candidate.to_json().to_string();
        assert!(
            !public.contains("s3cr3t")
                && !public.contains("header-secret")
                && !public.contains("pw")
        );
        assert!(public.contains("plaintext config.toml") && public.contains("Authorization"));
    }

    #[test]
    fn credential_policy_covers_docker_nested_cloud_keychains_and_ancestors() {
        let root =
            std::env::temp_dir().join(format!("fl-mcp-exposure-policy-{}", uuid::Uuid::new_v4()));
        let home = root.join("home");
        let project = root.join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        let home = home.canonicalize().unwrap();
        let project = project.canonicalize().unwrap();
        assert!(path_exposure_with_home(&project, Some(&home)).is_none());
        for relative in [
            ".docker",
            ".password-store",
            ".config/gcloud",
            "Library/Keychains",
        ] {
            let credential = home.join(relative);
            let child = credential.join("nested");
            std::fs::create_dir_all(&child).unwrap();
            assert!(crate::daemon::credential_path(&child).is_some());
            for target in [&credential, &child, credential.parent().unwrap()] {
                let warning = path_exposure_with_home(target, Some(&home))
                    .expect("credential exposure must require confirmation");
                assert!(warning.contains(target.to_str().unwrap()));
            }
        }
        // This folder is outside home; an immediate credential child still
        // makes the folder an exposure, using the same attachment policy.
        std::fs::create_dir_all(project.join(".docker/nested")).unwrap();
        assert!(path_exposure_with_home(&project, Some(&home)).is_some());
        assert!(path_exposure_with_home(&project.join(".docker/nested"), Some(&home)).is_some());
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("nested/cloud/gcloud/configurations")).unwrap();
        assert!(
            path_exposure_with_home(&elsewhere, Some(&home)).is_some(),
            "an external ancestor also exposes its nested credential tree"
        );
        let plain = root.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join(".env"), "fixture only").unwrap();
        assert!(path_exposure_with_home(&plain, Some(&home)).is_some());
        #[cfg(unix)]
        {
            let alias_root = root.join("aliases");
            std::fs::create_dir_all(&alias_root).unwrap();
            std::os::unix::fs::symlink(home.join(".docker"), alias_root.join("neutral-name"))
                .unwrap();
            assert!(path_exposure_with_home(&alias_root, Some(&home)).is_some());
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn incomplete_tree_inspection_requires_review_instead_of_assuming_safe() {
        let root =
            std::env::temp_dir().join(format!("fl-mcp-exposure-budget-{}", uuid::Uuid::new_v4()));
        let home = root.join("home");
        let deep = root.join("deep");
        std::fs::create_dir_all(&home).unwrap();
        let mut nested = deep.clone();
        for _ in 0..17 {
            nested.push("child");
        }
        std::fs::create_dir_all(nested).unwrap();
        let warning = path_exposure_with_home(&deep, Some(&home)).unwrap();
        assert!(warning.contains("too deep"));
        #[cfg(unix)]
        {
            let links = root.join("links");
            std::fs::create_dir_all(&links).unwrap();
            std::os::unix::fs::symlink(root.join("missing"), links.join("unresolved")).unwrap();
            let warning = path_exposure_with_home(&links, Some(&home)).unwrap();
            assert!(warning.contains("unresolved link"));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unverifiable_home_never_weakens_folder_confirmation_or_tilde_expansion() {
        let root =
            std::env::temp_dir().join(format!("fl-mcp-unverified-home-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("folder")).unwrap();
        let folder = root.join("folder").canonicalize().unwrap();
        let missing = root.join("missing/..");
        let file = root.join("file");
        std::fs::write(&file, "file").unwrap();
        for home in [
            None,
            Some(Path::new("relative-home")),
            Some(missing.as_path()),
            Some(file.as_path()),
        ] {
            assert!(normalize_directory_with_home("~", home).is_err());
            assert!(normalize_directory_with_home("~/folder", home).is_err());
            let warning = path_exposure_with_home(&folder, home)
                .expect("unknown native home requires explicit exposure review");
            assert!(warning.contains("unverified") && warning.contains(folder.to_str().unwrap()));
            assert_eq!(
                normalize_directory_with_home(folder.to_str().unwrap(), home).unwrap(),
                folder
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn directories_are_canonical_and_broad_exposure_requires_confirmation() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let root = std::env::temp_dir().join(format!("fl-catalog-path-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".ssh")).unwrap();
        let path = root.canonicalize().unwrap();
        #[cfg(unix)]
        {
            let link = root.join("alias");
            std::os::unix::fs::symlink(root.join(".ssh"), &link).unwrap();
            assert_eq!(
                normalize_directory(link.to_str().unwrap()).unwrap(),
                path.join(".ssh")
            );
        }
        let mut params = Map::from_iter([("path".into(), json!(path))]);
        let error = server_config(find("filesystem").unwrap(), &params)
            .unwrap_err()
            .to_string();
        assert!(error.contains("credential") && error.contains(path.to_str().unwrap()));
        params.insert("confirm_exposure".into(), json!(true));
        assert_eq!(
            server_config(find("filesystem").unwrap(), &params)
                .unwrap()
                .args
                .last()
                .unwrap(),
            path.to_str().unwrap()
        );
        assert!(normalize_directory(root.join("missing").to_str().unwrap()).is_err());
        std::fs::write(root.join("file"), "file").unwrap();
        assert!(normalize_directory(root.join("file").to_str().unwrap()).is_err());
        let home = normalize_directory("~").unwrap();
        assert!(path_exposure(&home).is_some());
        assert!(path_exposure(home.ancestors().last().unwrap()).is_some());
        let _ = std::fs::remove_dir_all(root);
    }
}
