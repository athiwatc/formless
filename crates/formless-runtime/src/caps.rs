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
                if d.is_empty() {
                    return Err("net needs a host name".into());
                }
                let host = if d.starts_with(INPUT_PREFIX) {
                    d.to_string()
                } else {
                    canonical_host_scope(d)?
                };
                c.net.get_or_insert_with(Vec::new).push(host);
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
                if cmd.is_empty() {
                    return Err("process needs a command name".into());
                }
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

    /// Make relative paths absolute against `base` in the input fields that
    /// input-bound `fs:` scopes name, before [`Self::bind`]: the Tool then
    /// gets, and is granted, the path the user meant (SPEC §5.1).
    pub fn absolutize_inputs(&self, input: &mut serde_json::Value, base: &std::path::Path) {
        let fields = self
            .fs_read
            .iter()
            .chain(&self.fs_write)
            .filter_map(|s| s.strip_prefix(INPUT_PREFIX));
        for field in fields {
            let Some(v) = field
                .split('.')
                .try_fold(&mut *input, |v, part| v.get_mut(part))
            else {
                continue;
            };
            let fix = |s: &mut serde_json::Value| {
                if let Some(p) = s.as_str().filter(|p| !p.is_empty()) {
                    let path = std::path::Path::new(p);
                    if path.is_relative() && !p.starts_with('~') {
                        *s = serde_json::Value::String(
                            base.join(path).to_string_lossy().into_owned(),
                        );
                    }
                }
            };
            match v {
                serde_json::Value::Array(items) => items.iter_mut().for_each(fix),
                other => fix(other),
            }
        }
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
            let (ah, ap) = crate::split_grant(a);
            let (bh, bp) = crate::split_grant(b);
            formless_protocol::net_scope::host_covers(ah, bh) && ap.is_none_or(|p| Some(p) == bp)
        }) && path_covers(&self.fs_read, &other.fs_read)
            && path_covers(&self.fs_write, &other.fs_write)
            && other.env.iter().all(|k| self.env.contains(k))
            // `process:git` covers `process:git:status`; a scoped grant
            // covers only the sub-commands it lists.
            && scoped_covers(&self.process, &other.process, |a, b| {
                if a == b {
                    return true;
                }
                let (ac, asub) = crate::process_grant(a);
                let (bc, bsub) = crate::process_grant(b);
                ac == bc
                    && match (asub, bsub) {
                        (None, _) => true,
                        (Some(_), None) => false,
                        (Some(x), Some(y)) => y.iter().all(|s| x.contains(s)),
                    }
            })
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
    canonical_path_with(raw, |p| p.canonicalize(), |p| p.symlink_metadata())
}

// Separate filesystem operations let tests place a creation exactly between
// canonicalize's NotFound and symlink_metadata, without timing or global hooks.
fn canonical_path_with(
    raw: &str,
    mut canonicalize: impl FnMut(&std::path::Path) -> std::io::Result<std::path::PathBuf>,
    mut metadata: impl FnMut(&std::path::Path) -> std::io::Result<std::fs::Metadata>,
) -> Result<std::path::PathBuf, String> {
    use std::io::ErrorKind;
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
        let unresolved = |e| format!("cannot resolve path {}: {e}", base.display());
        match canonicalize(base) {
            Ok(c) => return Ok(tail.iter().rev().fold(c, |acc, t| acc.join(t))),
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(unresolved(e)),
        }
        match metadata(base) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(format!("dangling symlink: {}", base.display()));
            }
            Ok(_) => {
                // Another writer created this entry after canonicalize saw it
                // missing. Resolve it rather than mistaking a regular file or
                // directory for a dangling symlink. A failed retry stays closed.
                let c = canonicalize(base).map_err(unresolved)?;
                return Ok(tail.iter().rev().fold(c, |acc, t| acc.join(t)));
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(unresolved(e)),
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

/// Static net scopes name hosts (optionally with a port), never URLs.
/// Use the HTTP URL parser's host canonicalization so static and input-bound
/// scopes agree on case, IDNA and IP literals without dropping an explicit port.
fn canonical_host_scope(raw: &str) -> Result<String, String> {
    let (wildcard, raw) = raw
        .strip_prefix("*.")
        .map_or((false, raw), |base| (true, base));
    let bad = || {
        format!("bad net scope '{raw}': expected a bare host or host:port (for example net:api.example.com), without a scheme or path")
    };
    if raw.contains(['/', '\\', '?', '#', '@', '*']) || raw.contains(char::is_whitespace) {
        return Err(bad());
    }
    if let Ok(ip) = raw.parse::<std::net::IpAddr>() {
        if wildcard {
            return Err(bad());
        }
        return Ok(match ip {
            std::net::IpAddr::V4(ip) => ip.to_string(),
            std::net::IpAddr::V6(ip) => format!("[{ip}]"),
        });
    }
    if raw.ends_with(':') {
        return Err(bad());
    }
    let url = reqwest::Url::parse(&format!("http://{raw}/")).map_err(|_| bad())?;
    let host = url.host_str().ok_or_else(bad)?;
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.is_empty() || host.split('.').any(str::is_empty) {
        return Err(bad());
    }
    if wildcard
        && host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok()
    {
        return Err(bad());
    }
    let host = if wildcard {
        format!("*.{host}")
    } else {
        host.to_string()
    };
    let (_, port) = crate::split_grant(raw);
    Ok(match port {
        Some(p) => format!("{host}:{p}"),
        None => host.to_string(),
    })
}

/// Host of a bound `net` value: a URL's host, or the value as a bare host.
/// A non-default port is kept (`host:8443`), so the grant shown for
/// approval is exactly that port, never every port (#603).
fn bound_host(raw: &str) -> Result<String, String> {
    if raw.contains('*') {
        return Err("a concrete net binding cannot contain a wildcard".into());
    }
    if raw.contains("://") {
        let u = reqwest::Url::parse(raw).map_err(|e| format!("bad URL for net binding: {e}"))?;
        let host = u
            .host_str()
            .map(str::to_string)
            .ok_or_else(|| format!("URL has no host: {raw}"))?;
        return Ok(match u.port() {
            Some(p) => format!("{host}:{p}"),
            None => host,
        });
    }
    canonical_host_scope(raw).map_err(|e| format!("bad host for net binding: {e}"))
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

    struct PathFixture(std::path::PathBuf);
    impl PathFixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("fl-canonical-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            Self(root.canonicalize().unwrap())
        }
    }
    impl Drop for PathFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn canonical_path_resolves_entries_created_after_the_missing_probe() {
        let root = PathFixture::new();
        for directory in [false, true] {
            let created = root.0.join(if directory { "parent" } else { "out.txt" });
            let target = if directory {
                created.join("out.txt")
            } else {
                created.clone()
            };
            let mut resolutions = Vec::new();
            let mut created_between_probes = false;
            let result = canonical_path_with(
                target.to_str().unwrap(),
                |p| {
                    resolutions.push(p.to_path_buf());
                    p.canonicalize()
                },
                |p| {
                    if p == created && !created_between_probes {
                        // The preceding real canonicalize returned NotFound.
                        if directory {
                            std::fs::create_dir(p).unwrap();
                        } else {
                            std::fs::write(p, "created by another writer").unwrap();
                        }
                        created_between_probes = true;
                    }
                    p.symlink_metadata()
                },
            )
            .unwrap();
            assert!(created_between_probes);
            assert_eq!(result, target);
            let expected = if directory {
                vec![target, created.clone(), created]
            } else {
                vec![created.clone(), created]
            };
            assert_eq!(
                resolutions, expected,
                "retry the same entry, not its parent"
            );
        }
    }

    #[test]
    fn canonical_path_never_ascends_after_resolution_or_metadata_errors() {
        use std::io::{Error, ErrorKind};
        let root = PathFixture::new();
        let target = root.0.join("out.txt");
        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::NotADirectory,
            ErrorKind::Other,
        ] {
            let mut resolutions = 0;
            let error = canonical_path_with(
                target.to_str().unwrap(),
                |_| {
                    resolutions += 1;
                    Err(Error::from(kind))
                },
                |_| panic!("non-missing resolution errors must stop immediately"),
            )
            .unwrap_err();
            assert!(error.starts_with("cannot resolve path"), "{error}");
            assert_eq!(resolutions, 1);
            let mut metadata_calls = 0;
            let error = canonical_path_with(
                target.to_str().unwrap(),
                |_| Err(Error::from(ErrorKind::NotFound)),
                |_| {
                    metadata_calls += 1;
                    Err(Error::from(kind))
                },
            )
            .unwrap_err();
            assert!(error.starts_with("cannot resolve path"), "{error}");
            assert_eq!(metadata_calls, 1);
        }
    }

    #[test]
    fn canonical_path_does_not_fall_back_when_the_creation_retry_fails() {
        use std::io::{Error, ErrorKind};
        let root = PathFixture::new();
        let target = root.0.join("out.txt");
        std::fs::write(&target, "exists by the metadata probe").unwrap();
        for kind in [ErrorKind::NotFound, ErrorKind::PermissionDenied] {
            let mut resolutions = 0;
            let error = canonical_path_with(
                target.to_str().unwrap(),
                |_| {
                    resolutions += 1;
                    Err(Error::from(if resolutions == 1 {
                        ErrorKind::NotFound
                    } else {
                        kind
                    }))
                },
                |p| p.symlink_metadata(),
            )
            .unwrap_err();
            assert!(error.starts_with("cannot resolve path"), "{error}");
            assert_eq!(resolutions, 2, "only one retry, no ancestor fallback");
        }
    }

    #[test]
    fn canonical_path_preserves_missing_tail_and_refuses_parent_traversal() {
        let root = PathFixture::new();
        let target = root.0.join("missing").join("out.txt");
        assert_eq!(canonical_path(target.to_str().unwrap()).unwrap(), target);
        let escape = root.0.join("missing").join("..").join("escape.txt");
        assert!(canonical_path(escape.to_str().unwrap()).is_err());
        assert!(canonical_path("").is_err());
        let file = root.0.join("file");
        std::fs::write(&file, "regular file").unwrap();
        assert!(canonical_path(file.join("child").to_str().unwrap()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn canonical_path_refuses_real_dangling_and_looping_symlinks() {
        let root = PathFixture::new();
        let dangling = root.0.join("dangling");
        std::os::unix::fs::symlink(root.0.join("absent"), &dangling).unwrap();
        for target in [dangling.clone(), dangling.join("child")] {
            let error = canonical_path(target.to_str().unwrap()).unwrap_err();
            assert!(error.starts_with("dangling symlink:"), "{error}");
        }
        let looping = root.0.join("loop");
        std::os::unix::fs::symlink(&looping, &looping).unwrap();
        let error = canonical_path(looping.to_str().unwrap()).unwrap_err();
        assert!(error.starts_with("cannot resolve path"), "{error}");
        let actual = root.0.join("actual");
        std::fs::create_dir(&actual).unwrap();
        let link = root.0.join("link");
        std::os::unix::fs::symlink(&actual, &link).unwrap();
        assert_eq!(
            canonical_path(link.join("new").to_str().unwrap()).unwrap(),
            actual.join("new")
        );
    }

    #[test]
    fn net_scopes_are_canonical_and_cover_mixed_case_bindings() {
        let static_scope = caps(&["net:API.Example.COM"]);
        assert_eq!(static_scope.to_specs(), ["net:api.example.com"]);
        let template = caps(&["net:$input.url"]);
        for raw in [
            "https://aPI.ExAmPlE.com/path",
            "Api.Example.Com",
            "https://API.example.com:8443/path",
        ] {
            let bound = template.bind(&json!({"url":raw})).unwrap();
            assert!(static_scope.covers(&bound), "{raw}");
        }
        let port_scope = caps(&["net:API.Example.COM:8443"]);
        assert!(static_scope.covers(&port_scope));
        assert!(!port_scope.covers(&static_scope));
        assert!(port_scope.covers(
            &template
                .bind(&json!({"url":"https://API.EXAMPLE.COM:8443/x"}))
                .unwrap()
        ));
        assert!(!port_scope.covers(&caps(&["net:api.example.com:443"])));
        assert_eq!(caps(&["net:LOCALHOST:80"]).to_specs(), ["net:localhost:80"]);
        assert!(caps(&["net:::1"]).covers(&caps(&["net:[::1]:8080"])));
    }

    #[test]
    fn explicit_wildcards_narrow_existing_grants_without_widening_input_bindings() {
        let exact = caps(&["net:example.com"]);
        let wildcard = caps(&["net:*.EXAMPLE.com:8443"]);
        assert_eq!(wildcard.to_specs(), ["net:*.example.com:8443"]);
        assert!(!exact.covers(&caps(&["net:api.example.com"])));
        assert!(!exact.covers(&wildcard));
        assert!(wildcard.covers(&caps(&["net:a.b.example.com:8443"])));
        assert!(wildcard.covers(&caps(&["net:*.sub.example.com:8443"])));
        assert!(!wildcard.covers(&caps(&["net:example.com:8443"])));
        assert!(!wildcard.covers(&caps(&["net:a.example.com:443"])));
        assert!(!wildcard.covers(&caps(&["net:evil-example.com:8443"])));
        for spec in [
            "net:foo.*.example.com",
            "net:*",
            "net:*.127.0.0.1",
            "net:*.[::1]",
            "net:*.*.example.com",
            "net:.foo",
            "net:.a.foo",
            "net:a..foo",
            "net:a.foo..",
        ] {
            assert!(Capabilities::parse(&[spec.into()]).is_err(), "{spec}");
        }
        let binding = caps(&["net:$input.url"]);
        for input in ["*.example.com", "https://*.example.com/x"] {
            assert!(
                binding.bind(&json!({"url":input})).is_err(),
                "concrete input cannot synthesize a wildcard grant: {input}"
            );
        }
        let unicode = caps(&["net:*.bücher.example"]);
        assert_eq!(unicode.to_specs(), ["net:*.xn--bcher-kva.example"]);
    }

    #[test]
    fn malformed_net_and_empty_process_scopes_are_rejected() {
        for spec in ["net:", "process:"] {
            let error = Capabilities::parse(&[spec.into()]).unwrap_err();
            assert!(error.contains("needs a"), "{error}");
        }
        for spec in [
            "net:https://api.example.com",
            "net:api.example.com:443/v1",
            "net:host:notaport",
            "net:host:",
            "net:bad host",
        ] {
            let error = Capabilities::parse(&[spec.into()]).unwrap_err();
            assert!(error.contains("bare host"), "{error}");
        }
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
        // The input URL's port stays in the grant (#603).
        assert_eq!(b.net, Some(vec!["api.example.com:8443".to_string()]));
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

#[cfg(test)]
mod scope_tests {
    use super::*;

    #[test]
    fn bound_urls_keep_their_port_and_process_grants_cover_sub_commands() {
        assert_eq!(
            bound_host("https://api.example.com:8443/x").unwrap(),
            "api.example.com:8443"
        );
        assert_eq!(
            bound_host("https://api.example.com/x").unwrap(),
            "api.example.com"
        );
        assert_eq!(
            bound_host("Host.example:9000").unwrap(),
            "host.example:9000"
        );
        assert!(bound_host("host:notaport").is_err());
        // IPv6: one bracketed host:port form, from a URL or a bare value (#1094).
        assert_eq!(bound_host("http://[::1]:8080/x").unwrap(), "[::1]:8080");
        assert_eq!(bound_host("[::1]:8080").unwrap(), "[::1]:8080");
        assert_eq!(bound_host("http://[::1]/x").unwrap(), "[::1]");
        let wide = Capabilities::parse(&["process:git".to_string()]).unwrap();
        let narrow = Capabilities::parse(&["process:git:status".to_string()]).unwrap();
        assert!(wide.covers(&narrow));
        assert!(!narrow.covers(&wide));
    }

    /// docs/capabilities.md says exactly which Capabilities are asked
    /// about under `--secure` (#1084).
    #[test]
    fn the_docs_list_what_is_asked() {
        let doc = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/capabilities.md"),
        )
        .unwrap();
        let listed = |prefix: &str| -> Vec<String> {
            let line = doc
                .lines()
                .find(|l| l.starts_with(prefix))
                .unwrap_or_else(|| panic!("no '{prefix}' line"));
            let items = line.rsplit_once(": ").map_or(line, |(_, rest)| rest);
            items
                .split('`')
                .skip(1)
                .step_by(2)
                .map(str::to_string)
                .collect()
        };
        let asked = listed("- Asked before first use:");
        let never = listed("- Never asked");
        assert!(!asked.is_empty() && !never.is_empty());
        for spec in &asked {
            let c = Capabilities::parse(std::slice::from_ref(spec)).unwrap();
            assert!(c.is_sensitive(), "{spec} is listed as asked but never is");
        }
        for spec in &never {
            let c = Capabilities::parse(std::slice::from_ref(spec)).unwrap();
            assert!(!c.is_sensitive(), "{spec} is listed as never asked but is");
        }
    }
}
