use serde::{Deserialize, Serialize};

/// Capability set for one Tool version (SPEC §7.4). Serialised to/from the
/// spec-string form the agent passes to `create_tool`, e.g.
/// `["net", "net:example.com", "fs:read:/tmp/in", "env:HOME", "process:ls"]`.
///
/// `fs:read`, `fs:write` and `net` scopes may instead be bound to an input
/// field, e.g. `fs:read:$input.path` or `net:$input.url`. Such a template is
/// resolved per Invocation by [`Capabilities::bind`], so one generic Tool
/// still gets least privilege: exactly the paths/hosts it was called with.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    /// None = no network. Some(empty) = unscoped. Some(list) = those domains.
    pub net: Option<Vec<String>>,
    pub fs_read: Vec<String>,
    pub fs_write: Vec<String>,
    pub env: Vec<String>,
    /// None = no process. Some(empty) = unscoped. Some(list) = those commands.
    pub process: Option<Vec<String>>,
    pub clipboard_read: bool,
    pub clipboard_write: bool,
    pub notify: bool,
    /// None = no LLM calls. Some(empty) = any configured model. Some(list) = those models.
    pub llm: Option<Vec<String>>,
    /// None = no sub-agents. Some(empty) = any configured model. Some(list) = those models.
    pub agent: Option<Vec<String>>,
    /// MCP servers whose tools this entry calls (`mcp:<server>`); set only
    /// on the registry rows that stand for MCP tools.
    #[serde(default)]
    pub mcp: Vec<String>,
    /// `store`: the Tool's own key-value namespace (SPEC §7.4).
    #[serde(default)]
    pub store: bool,
    /// `store:<ns>`: shared key-value namespaces.
    #[serde(default)]
    pub store_shared: Vec<String>,
    /// `harness:<scope>`: read or change the harness itself (SPEC §7.13).
    #[serde(default)]
    pub harness: Vec<String>,
}

/// Harness scopes (SPEC §7.13). `read` needs no approval; every other one
/// changes the harness behind a per-change approval.
pub const HARNESS_SCOPES: &[&str] = &[
    "read",
    "mcp",
    "skills",
    "models",
    "providers",
    "roles",
    "config",
];

impl Capabilities {
    pub fn parse(specs: &[String]) -> Result<Self, String> {
        let mut c = Capabilities::default();
        for s in specs {
            let s = s.trim();
            let bindable =
                s.starts_with("fs:read:") || s.starts_with("fs:write:") || s.starts_with("net:");
            if !bindable && s.contains(INPUT_PREFIX) {
                return Err(format!(
                    "input-bound scopes are only supported for fs:read, fs:write and net: {s}"
                ));
            }
            if let Some(field) = s
                .rsplit(':')
                .next()
                .and_then(|t| t.strip_prefix(INPUT_PREFIX))
            {
                if field.is_empty() || field.split('.').any(str::is_empty) {
                    return Err(format!("bad input field in capability: {s}"));
                }
            }
            if s == "net" {
                c.net = Some(Vec::new());
            } else if let Some(d) = s.strip_prefix("net:") {
                c.net.get_or_insert_with(Vec::new).push(d.to_string());
            } else if let Some(p) = s.strip_prefix("fs:read:") {
                if p.is_empty() {
                    return Err("fs:read needs a path".into());
                }
                c.fs_read.push(p.to_string());
            } else if let Some(p) = s.strip_prefix("fs:write:") {
                if p.is_empty() {
                    return Err("fs:write needs a path".into());
                }
                c.fs_write.push(p.to_string());
            } else if let Some(k) = s.strip_prefix("env:") {
                if k.is_empty() {
                    return Err("env needs a key name".into());
                }
                c.env.push(k.to_string());
            } else if s == "process" {
                c.process = Some(Vec::new());
            } else if let Some(cmd) = s.strip_prefix("process:") {
                c.process.get_or_insert_with(Vec::new).push(cmd.to_string());
            } else if s == "clipboard" {
                c.clipboard_read = true;
                c.clipboard_write = true;
            } else if s == "clipboard:read" {
                c.clipboard_read = true;
            } else if s == "clipboard:write" {
                c.clipboard_write = true;
            } else if s == "notify" {
                c.notify = true;
            } else if let Some(scope) = s.strip_prefix("harness:") {
                if !HARNESS_SCOPES.contains(&scope) {
                    return Err(format!(
                        "unknown harness scope '{scope}' (one of {})",
                        HARNESS_SCOPES.join(", ")
                    ));
                }
                if !c.harness.iter().any(|h| h == scope) {
                    c.harness.push(scope.to_string());
                }
            } else if s == "store" {
                c.store = true;
            } else if let Some(ns) = s.strip_prefix("store:") {
                if ns.is_empty()
                    || !ns
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || "-_.".contains(ch))
                {
                    return Err(format!(
                        "store namespace must be letters, digits, '-', '_' or '.': {s}"
                    ));
                }
                c.store_shared.push(ns.to_string());
            } else if s == "llm" {
                c.llm = Some(Vec::new());
            } else if let Some(m) = s.strip_prefix("llm:") {
                if m.is_empty() {
                    return Err("llm needs a model name".into());
                }
                c.llm.get_or_insert_with(Vec::new).push(m.to_string());
            } else if let Some(server) = s.strip_prefix("mcp:") {
                if server.is_empty() {
                    return Err("mcp needs a server name".into());
                }
                c.mcp.push(server.to_string());
            } else if s == "agent" {
                c.agent = Some(Vec::new());
            } else if let Some(m) = s.strip_prefix("agent:") {
                if m.is_empty() {
                    return Err("agent needs a model name".into());
                }
                c.agent.get_or_insert_with(Vec::new).push(m.to_string());
            } else {
                return Err(format!("unknown capability: {s}"));
            }
        }
        Ok(c)
    }

    pub fn to_specs(&self) -> Vec<String> {
        let mut out = Vec::new();
        match &self.net {
            Some(v) if v.is_empty() => out.push("net".into()),
            Some(v) => out.extend(v.iter().map(|d| format!("net:{d}"))),
            None => {}
        }
        out.extend(self.fs_read.iter().map(|p| format!("fs:read:{p}")));
        out.extend(self.fs_write.iter().map(|p| format!("fs:write:{p}")));
        out.extend(self.env.iter().map(|k| format!("env:{k}")));
        match &self.process {
            Some(v) if v.is_empty() => out.push("process".into()),
            Some(v) => out.extend(v.iter().map(|c| format!("process:{c}"))),
            None => {}
        }
        if self.clipboard_read && self.clipboard_write {
            out.push("clipboard".into());
        } else {
            if self.clipboard_read {
                out.push("clipboard:read".into());
            }
            if self.clipboard_write {
                out.push("clipboard:write".into());
            }
        }
        if self.notify {
            out.push("notify".into());
        }
        if self.store {
            out.push("store".into());
        }
        out.extend(self.store_shared.iter().map(|n| format!("store:{n}")));
        out.extend(self.harness.iter().map(|h| format!("harness:{h}")));
        match &self.llm {
            Some(v) if v.is_empty() => out.push("llm".into()),
            Some(v) => out.extend(v.iter().map(|m| format!("llm:{m}"))),
            None => {}
        }
        match &self.agent {
            Some(v) if v.is_empty() => out.push("agent".into()),
            Some(v) => out.extend(v.iter().map(|m| format!("agent:{m}"))),
            None => {}
        }
        out.extend(self.mcp.iter().map(|s| format!("mcp:{s}")));
        out
    }

    /// True if any scope is an unresolved `$input.<field>` template.
    pub fn has_bindings(&self) -> bool {
        self.templates().next().is_some()
    }

    fn templates(&self) -> impl Iterator<Item = &String> {
        self.net
            .iter()
            .flatten()
            .chain(&self.fs_read)
            .chain(&self.fs_write)
            .filter(|s| s.starts_with(INPUT_PREFIX))
    }

    /// Resolve `$input.<field>` scopes against one Invocation's input.
    /// A field may hold a string or an array of strings. Paths are made
    /// absolute and canonicalised (like the runtime's containment check);
    /// `net` values may be URLs or bare hosts. A missing/non-string field
    /// is an error: the Tool would otherwise run with less than it asked
    /// for and fail confusingly.
    pub fn bind(&self, input: &serde_json::Value) -> Result<Self, String> {
        let mut out = self.clone();
        let resolve = |list: &[String], f: &dyn Fn(&str) -> Result<String, String>| {
            let mut res = Vec::new();
            for s in list {
                match s.strip_prefix(INPUT_PREFIX) {
                    None => res.push(s.clone()),
                    Some(field) => {
                        for v in input_strings(input, field)? {
                            res.push(f(&v)?);
                        }
                    }
                }
            }
            Ok::<_, String>(res)
        };
        out.fs_read = resolve(&self.fs_read, &|p| bound_path(p))?;
        out.fs_write = resolve(&self.fs_write, &|p| bound_path(p))?;
        if let Some(net) = &self.net {
            if !net.is_empty() {
                out.net = Some(resolve(net, &|v| bound_host(v))?);
            }
        }
        Ok(out)
    }

    /// Drop every input-bound scope (used where no real input exists, e.g.
    /// the create-time smoke test, which grants nothing it can't resolve).
    pub fn unbound(&self) -> Self {
        let keep = |l: &[String]| -> Vec<String> {
            l.iter()
                .filter(|s| !s.starts_with(INPUT_PREFIX))
                .cloned()
                .collect()
        };
        let mut out = self.clone();
        out.fs_read = keep(&self.fs_read);
        out.fs_write = keep(&self.fs_write);
        out.net = match &self.net {
            Some(n) if !n.is_empty() => {
                let k = keep(n);
                if k.is_empty() {
                    None
                } else {
                    Some(k)
                }
            }
            other => other.clone(),
        };
        out
    }

    /// Union of two grants (accumulating per-path approvals for one Tool).
    pub fn union(&self, other: &Capabilities) -> Capabilities {
        fn merge(a: &[String], b: &[String]) -> Vec<String> {
            let mut out = a.to_vec();
            out.extend(b.iter().filter(|x| !a.contains(x)).cloned());
            out
        }
        fn merge_scoped(a: &Option<Vec<String>>, b: &Option<Vec<String>>) -> Option<Vec<String>> {
            match (a, b) {
                (None, x) | (x, None) => x.clone(),
                (Some(x), _) | (_, Some(x)) if x.is_empty() => Some(Vec::new()),
                (Some(x), Some(y)) => Some(merge(x, y)),
            }
        }
        Capabilities {
            net: merge_scoped(&self.net, &other.net),
            fs_read: merge(&self.fs_read, &other.fs_read),
            fs_write: merge(&self.fs_write, &other.fs_write),
            env: merge(&self.env, &other.env),
            process: merge_scoped(&self.process, &other.process),
            clipboard_read: self.clipboard_read || other.clipboard_read,
            clipboard_write: self.clipboard_write || other.clipboard_write,
            notify: self.notify || other.notify,
            llm: merge_scoped(&self.llm, &other.llm),
            agent: merge_scoped(&self.agent, &other.agent),
            mcp: merge(&self.mcp, &other.mcp),
            store: self.store || other.store,
            store_shared: merge(&self.store_shared, &other.store_shared),
            harness: merge(&self.harness, &other.harness),
        }
    }

    /// Sensitive capabilities need first-run approval (SPEC §10.3).
    pub fn is_sensitive(&self) -> bool {
        self.net.is_some()
            || self.process.is_some()
            || !self.fs_write.is_empty()
            || !self.env.is_empty()
            || self.clipboard_read
            || self.clipboard_write
            || self.notify
            || self.llm.is_some()
            || self.agent.is_some()
            || !self.mcp.is_empty()
            || self.harness.iter().any(|h| h != "read")
    }

    /// Does `self` (approved) fully cover `other` (requested)?
    pub fn covers(&self, other: &Capabilities) -> bool {
        scoped_covers(&self.net, &other.net, |a, b| {
            a == b || b.ends_with(&format!(".{a}"))
        }) && path_covers(&self.fs_read, &other.fs_read)
            && path_covers(&self.fs_write, &other.fs_write)
            && other.env.iter().all(|k| self.env.contains(k))
            && scoped_covers(&self.process, &other.process, |a, b| a == b)
            && (!other.clipboard_read || self.clipboard_read)
            && (!other.clipboard_write || self.clipboard_write)
            && (!other.notify || self.notify)
            && scoped_covers(&self.llm, &other.llm, |a, b| a == b)
            && scoped_covers(&self.agent, &other.agent, |a, b| a == b)
            && other.mcp.iter().all(|s| self.mcp.contains(s))
            && (!other.store || self.store)
            && other
                .store_shared
                .iter()
                .all(|n| self.store_shared.contains(n))
            && other.harness.iter().all(|h| self.harness.contains(h))
    }
}

const INPUT_PREFIX: &str = "$input.";

fn input_strings(input: &serde_json::Value, field: &str) -> Result<Vec<String>, String> {
    let mut v = input;
    for part in field.split('.') {
        v = v
            .get(part)
            .ok_or_else(|| format!("capability bound to input.{field}, but it is missing"))?;
    }
    let bad =
        || format!("capability bound to input.{field}, which must be a string or array of strings");
    match v {
        serde_json::Value::String(s) if !s.is_empty() => Ok(vec![s.clone()]),
        serde_json::Value::Array(a) => a
            .iter()
            .map(|x| {
                x.as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .ok_or_else(bad)
            })
            .collect(),
        _ => Err(bad()),
    }
}

/// Absolute, canonical form of a bound path.
fn bound_path(raw: &str) -> Result<String, String> {
    Ok(canonical_path(raw)?.to_string_lossy().into_owned())
}

/// Absolute, canonical form of a path that may not exist yet (SPEC §14):
/// the deepest existing ancestor is canonicalised (symlinks resolved) and
/// the missing tail appended. A dangling symlink is refused, since writing
/// through it would land wherever it points; so is `..` in the missing
/// tail, which could otherwise climb out after canonicalisation.
pub fn canonical_path(raw: &str) -> Result<std::path::PathBuf, String> {
    use std::path::{Component, Path};
    if raw.is_empty() {
        return Err("empty path".into());
    }
    let p = Path::new(raw);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().map_err(|e| e.to_string())?.join(p)
    };
    let mut base = abs.as_path();
    let mut tail = Vec::new();
    loop {
        if let Ok(c) = base.canonicalize() {
            return Ok(tail.iter().rev().fold(c, |acc, t| acc.join(t)));
        }
        if base.symlink_metadata().is_ok() {
            return Err(format!("dangling symlink: {}", base.display()));
        }
        match base.components().next_back() {
            Some(Component::Normal(name)) => tail.push(name.to_os_string()),
            Some(Component::CurDir) => {}
            _ => return Err(format!("cannot resolve path: {raw}")),
        }
        base = base
            .parent()
            .ok_or_else(|| format!("cannot resolve path: {raw}"))?;
    }
}

/// Host of a bound `net` value: a URL's host, or the value as a bare host.
fn bound_host(raw: &str) -> Result<String, String> {
    if raw.contains("://") {
        let u = reqwest::Url::parse(raw).map_err(|e| format!("bad URL for net binding: {e}"))?;
        return u
            .host_str()
            .map(str::to_string)
            .ok_or_else(|| format!("URL has no host: {raw}"));
    }
    let host = raw.split(['/', ':']).next().unwrap_or_default();
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Err(format!("bad host for net binding: {raw}"));
    }
    Ok(host.to_lowercase())
}

fn scoped_covers(
    approved: &Option<Vec<String>>,
    requested: &Option<Vec<String>>,
    one: impl Fn(&str, &str) -> bool,
) -> bool {
    match (approved, requested) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(a), Some(_)) if a.is_empty() => true, // unscoped approved covers all
        (Some(_), Some(r)) if r.is_empty() => false, // unscoped requested needs unscoped approved
        (Some(a), Some(r)) => r.iter().all(|want| a.iter().any(|have| one(have, want))),
    }
}

fn path_covers(approved: &[String], requested: &[String]) -> bool {
    requested.iter().all(|want| {
        approved.iter().any(|have| {
            let want_p = std::path::Path::new(want);
            let have_p = std::path::Path::new(have);
            want_p == have_p || want_p.starts_with(have_p)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn caps(specs: &[&str]) -> Capabilities {
        Capabilities::parse(&specs.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn bind_resolves_input_fields() {
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let c = caps(&[
            "fs:read:$input.src",
            "fs:write:$input.out.dir",
            "net:$input.url",
            "fs:read:/etc",
        ]);
        assert!(c.has_bindings());
        let b = c
            .bind(&json!({
                "src": [dir.to_str().unwrap()],
                "out": {"dir": dir.join("new.txt").to_str().unwrap()},
                "url": "https://API.example.com:8443/x?y"
            }))
            .unwrap();
        assert!(!b.has_bindings());
        assert_eq!(
            b.fs_read,
            vec![dir.to_string_lossy().to_string(), "/etc".into()]
        );
        assert_eq!(
            b.fs_write,
            vec![dir.join("new.txt").to_string_lossy().to_string()]
        );
        assert_eq!(b.net, Some(vec!["api.example.com".to_string()]));
    }

    #[test]
    fn bind_rejects_missing_or_bad_fields() {
        let c = caps(&["fs:read:$input.path"]);
        assert!(c.bind(&json!({})).is_err());
        assert!(c.bind(&json!({"path": 3})).is_err());
        assert!(c.bind(&json!({"path": ""})).is_err());
    }

    #[test]
    fn bindings_only_for_fs_and_net() {
        for bad in [
            "env:$input.k",
            "process:$input.cmd",
            "llm:$input.m",
            "fs:read:$input.",
            "net:$input.a..b",
        ] {
            assert!(Capabilities::parse(&[bad.to_string()]).is_err(), "{bad}");
        }
    }

    #[test]
    fn unbound_drops_templates() {
        let c = caps(&["net:$input.url", "fs:read:$input.p", "fs:read:/tmp"]).unbound();
        assert_eq!(c.net, None);
        assert_eq!(c.fs_read, vec!["/tmp".to_string()]);
        assert_eq!(caps(&["net"]).unbound().net, Some(vec![]));
    }

    #[test]
    fn union_accumulates_grants() {
        let u = caps(&["fs:write:/a", "net:x.com"]).union(&caps(&["fs:write:/b", "net:y.com"]));
        assert!(u.covers(&caps(&["fs:write:/a/f", "fs:write:/b", "net:y.com"])));
        assert!(caps(&["net:x.com"])
            .union(&caps(&["net"]))
            .covers(&caps(&["net"])));
    }
}
