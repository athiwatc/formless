use serde::{Deserialize, Serialize};

/// Capability set for one Tool version (SPEC §7.4). Serialised to/from the
/// spec-string form the agent passes to `create_tool`, e.g.
/// `["net", "net:example.com", "fs:read:/tmp/in", "env:HOME", "process:ls"]`.
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
}

impl Capabilities {
    pub fn parse(specs: &[String]) -> Result<Self, String> {
        let mut c = Capabilities::default();
        for s in specs {
            let s = s.trim();
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
            } else if s == "llm" {
                c.llm = Some(Vec::new());
            } else if let Some(m) = s.strip_prefix("llm:") {
                if m.is_empty() {
                    return Err("llm needs a model name".into());
                }
                c.llm.get_or_insert_with(Vec::new).push(m.to_string());
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
        match &self.llm {
            Some(v) if v.is_empty() => out.push("llm".into()),
            Some(v) => out.extend(v.iter().map(|m| format!("llm:{m}"))),
            None => {}
        }
        out
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
    }
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
