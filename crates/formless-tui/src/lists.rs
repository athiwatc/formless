//! Filtering and sorting the Tools, Sessions and Proposals lists (SPEC
//! §10.4): `/` types a fuzzy filter, `s` cycles the sort, `v` cycles the
//! quick filters. Views select by position in the visible rows.

use formless_core::fuzzy::fuzzy_match;
use formless_protocol::{PromptProposalInfo, SessionInfo, ToolInfo};
use std::collections::HashSet;

#[derive(Debug, Default, Clone)]
pub struct ListFilter {
    pub query: String,
    /// `/` was pressed and keys go to `query`.
    pub editing: bool,
    pub sort: usize,
    pub quick: usize,
}

impl ListFilter {
    /// Whether `text` (several fields joined) matches the query.
    fn hit(&self, fields: &[&str]) -> bool {
        self.query.trim().is_empty() || fields.iter().any(|f| fuzzy_match(&self.query, f).is_some())
    }

    pub fn active(&self) -> bool {
        !self.query.trim().is_empty() || self.quick != 0
    }

    /// `name · 12/240 · sort: calls · failing`, for the panel title.
    pub fn title(
        &self,
        name: &str,
        shown: usize,
        total: usize,
        sorts: &[&str],
        quicks: &[&str],
    ) -> String {
        let mut t = format!(" {name} · ");
        if shown == total {
            t.push_str(&total.to_string());
        } else {
            t.push_str(&format!("{shown}/{total}"));
        }
        if self.sort != 0 {
            t.push_str(&format!(" · sort: {}", sorts[self.sort % sorts.len()]));
        }
        if self.quick != 0 {
            t.push_str(&format!(" · {}", quicks[self.quick % quicks.len()]));
        }
        if self.editing || !self.query.is_empty() {
            t.push_str(&format!(
                " · /{}{}",
                self.query,
                if self.editing { "▏" } else { "" }
            ));
        }
        t.push(' ');
        t
    }
}

pub const TOOL_SORTS: &[&str] = &["name", "success rate", "last used", "calls"];
pub const TOOL_QUICK: &[&str] = &[
    "all",
    "active",
    "retired",
    "failing (<80%)",
    "needs attention",
];
pub const SESSION_SORTS: &[&str] = &["last activity", "created", "name"];
pub const SESSION_QUICK: &[&str] = &["all", "running", "archived"];
pub const PROPOSAL_SORTS: &[&str] = &["newest", "score"];
pub const PROPOSAL_QUICK: &[&str] = &["all", "pending", "applied", "rejected"];

/// Indices into `tools`, filtered and sorted.
pub fn tools(tools: &[ToolInfo], f: &ListFilter) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..tools.len())
        .filter(|&i| {
            let t = &tools[i];
            let quick = match f.quick % TOOL_QUICK.len() {
                1 => !t.retired,
                2 => t.retired,
                3 => t.success_rate.is_some_and(|r| r < 0.8),
                4 => t.needs_attention,
                _ => true,
            };
            quick && f.hit(&[&t.name, &t.description])
        })
        .collect();
    match f.sort % TOOL_SORTS.len() {
        1 => idx.sort_by(|&a, &b| {
            let r = |i: usize| tools[i].success_rate.unwrap_or(-1.0);
            r(b).total_cmp(&r(a))
        }),
        2 => idx.sort_by(|&a, &b| tools[b].updated_at.cmp(&tools[a].updated_at)),
        3 => idx.sort_by_key(|&i| std::cmp::Reverse(tools[i].invocations)),
        _ => idx.sort_by(|&a, &b| tools[a].name.cmp(&tools[b].name)),
    }
    idx
}

/// Indices into `sessions` (newest activity first by default).
pub fn sessions(sessions: &[SessionInfo], running: &HashSet<String>, f: &ListFilter) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..sessions.len())
        .filter(|&i| {
            let s = &sessions[i];
            // Archived Sessions only show under their own filter.
            let quick = match f.quick % SESSION_QUICK.len() {
                1 => running.contains(&s.id) && !s.archived,
                2 => s.archived,
                _ => !s.archived,
            };
            // `#tag` matches a tag exactly; other queries also search tags.
            let hit = match f.query.trim().strip_prefix('#') {
                Some(tag) if !tag.is_empty() => s.tags.iter().any(|t| t == &tag.to_lowercase()),
                _ => f.hit(&[&s.name, &s.tags.join(" ")]),
            };
            quick && hit
        })
        .collect();
    match f.sort % SESSION_SORTS.len() {
        1 => idx.sort_by(|&a, &b| sessions[b].created_at.cmp(&sessions[a].created_at)),
        2 => idx.sort_by_key(|&i| sessions[i].name.to_lowercase()),
        _ => idx.sort_by(|&a, &b| sessions[b].updated_at.cmp(&sessions[a].updated_at)),
    }
    // Pinned Sessions first, whatever the sort (a stable sort keeps the
    // order within each group).
    idx.sort_by_key(|&i| !sessions[i].pinned);
    idx
}

/// Indices into `proposals` (as listed, newest first, by default).
pub fn proposals(props: &[PromptProposalInfo], f: &ListFilter) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..props.len())
        .filter(|&i| {
            let p = &props[i];
            let quick = match f.quick % PROPOSAL_QUICK.len() {
                1 => !matches!(p.status.as_str(), "applied" | "rejected" | "rolled_back"),
                2 => p.status == "applied",
                3 => p.status == "rejected",
                _ => true,
            };
            quick && f.hit(&[&p.content, &p.rationale, &p.kind])
        })
        .collect();
    if f.sort % PROPOSAL_SORTS.len() == 1 {
        idx.sort_by(|&a, &b| {
            let s = |i: usize| props[i].candidate_score.unwrap_or(-1.0);
            s(b).total_cmp(&s(a))
        });
    }
    idx
}

/// Keys for a list's filter: true when used. `/` starts typing; while
/// typing, Enter keeps the filter and Esc clears it; `s`/`v` cycle.
pub fn key(
    f: &mut ListFilter,
    code: crossterm::event::KeyCode,
    sorts: usize,
    quicks: usize,
) -> bool {
    use crossterm::event::KeyCode;
    if f.editing {
        match code {
            KeyCode::Enter => f.editing = false,
            KeyCode::Esc => {
                f.editing = false;
                f.query.clear();
            }
            KeyCode::Backspace => {
                f.query.pop();
            }
            KeyCode::Char(c) => f.query.push(c),
            _ => return false,
        }
        return true;
    }
    match code {
        KeyCode::Char('/') => f.editing = true,
        KeyCode::Char('s') => f.sort = (f.sort + 1) % sorts,
        KeyCode::Char('v') => f.quick = (f.quick + 1) % quicks,
        // Esc clears an active filter before it leaves the view.
        KeyCode::Esc if f.active() => {
            f.query.clear();
            f.quick = 0;
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, rate: Option<f64>, calls: i64, retired: bool) -> ToolInfo {
        ToolInfo {
            id: name.into(),
            name: name.into(),
            description: format!("{name} things"),
            retired,
            current_version: 1,
            capabilities: vec![],
            memory_mb: 64,
            invocations: calls,
            success_rate: rate,
            needs_attention: false,
            created_at: String::new(),
            updated_at: format!("2026-01-0{calls}"),
        }
    }

    #[test]
    fn tools_filter_sort_and_quick_filters() {
        let ts = vec![
            tool("read_file", Some(0.99), 5, false),
            tool("http_get", Some(0.5), 9, false),
            tool("old_csv", None, 1, true),
        ];
        let names = |idx: Vec<usize>| {
            idx.into_iter()
                .map(|i| ts[i].name.clone())
                .collect::<Vec<_>>()
        };
        let mut f = ListFilter::default();
        assert_eq!(names(tools(&ts, &f)), ["http_get", "old_csv", "read_file"]);
        f.query = "rdfl".into();
        assert_eq!(names(tools(&ts, &f)), ["read_file"]);
        f.query.clear();
        f.sort = 3;
        assert_eq!(names(tools(&ts, &f))[0], "http_get");
        f.sort = 1;
        assert_eq!(names(tools(&ts, &f))[0], "read_file");
        f.quick = 3;
        assert_eq!(names(tools(&ts, &f)), ["http_get"]);
        f.quick = 2;
        assert_eq!(names(tools(&ts, &f)), ["old_csv"]);
        assert_eq!(
            f.title("Tools", 1, 3, TOOL_SORTS, TOOL_QUICK),
            " Tools · 1/3 · sort: success rate · retired "
        );
    }

    #[test]
    fn filter_keys() {
        use crossterm::event::KeyCode;
        let mut f = ListFilter::default();
        assert!(key(&mut f, KeyCode::Char('/'), 2, 2));
        assert!(key(&mut f, KeyCode::Char('s'), 2, 2), "typed while editing");
        assert_eq!(f.query, "s");
        key(&mut f, KeyCode::Enter, 2, 2);
        assert!(key(&mut f, KeyCode::Char('s'), 2, 2));
        assert_eq!(f.sort, 1);
        assert!(key(&mut f, KeyCode::Esc, 2, 2), "clears the filter first");
        assert!(f.query.is_empty());
        assert!(!key(&mut f, KeyCode::Esc, 2, 2), "then Esc leaves the view");
        assert!(!key(&mut f, KeyCode::Char('x'), 2, 2));
    }
}
