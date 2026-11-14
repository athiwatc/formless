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
    /// Sessions only: group forks under their recorded parent.
    pub tree: bool,
    /// Tools only: grouping (`TOOL_GROUPS`) and the collapsed groups.
    pub group: usize,
    pub collapsed: HashSet<String>,
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
pub const TOOL_GROUPS: &[&str] = &["none", "capability", "author", "status"];

/// The group a Tool falls in under grouping `g` (SPEC §10.4): its
/// Capability families ("fs + net"), its current version's author, or
/// its status. `None` when not grouping.
pub fn tool_group(t: &ToolInfo, g: usize) -> Option<String> {
    Some(match g % TOOL_GROUPS.len() {
        1 => {
            let mut fams: Vec<&str> = t
                .capabilities
                .iter()
                .map(|c| c.split(':').next().unwrap_or(c))
                .collect();
            fams.sort_unstable();
            fams.dedup();
            if fams.is_empty() {
                "no capabilities".into()
            } else {
                fams.join(" + ")
            }
        }
        2 if t.author.is_empty() => "unknown".into(),
        2 => t.author.clone(),
        3 if t.retired => "retired".into(),
        3 if t.needs_attention => "needs attention".into(),
        3 => "active".into(),
        _ => return None,
    })
}

/// Status groups in a useful order: what needs attention first.
fn group_rank(group: &str, g: usize) -> (u8, String) {
    let rank = match (g % TOOL_GROUPS.len(), group) {
        (3, "needs attention") => 0,
        (3, "active") => 1,
        (3, _) => 2,
        (1, "no capabilities") => 1,
        _ => 0,
    };
    (rank, group.to_string())
}

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
            // `#git` filters by tag; anything else also matches tags.
            let text = match f.query.trim().strip_prefix('#') {
                Some(tag) => {
                    tag.is_empty() || t.tags.iter().any(|g| g.starts_with(&tag.to_lowercase()))
                }
                None => f.hit(&[&t.name, &t.title, &t.description, &t.tags.join(" ")]),
            };
            quick && text
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
    // Grouped: by group (stable, so the sort holds inside each), without
    // the collapsed groups' Tools.
    if !f.group.is_multiple_of(TOOL_GROUPS.len()) {
        idx.sort_by_cached_key(|&i| {
            group_rank(&tool_group(&tools[i], f.group).unwrap_or_default(), f.group)
        });
        idx.retain(|&i| {
            !f.collapsed
                .contains(&tool_group(&tools[i], f.group).unwrap_or_default())
        });
    }
    idx
}

/// Every group with its Tool count (after the filter, collapsed ones
/// too), in display order.
pub fn tool_groups(tools: &[ToolInfo], f: &ListFilter) -> Vec<(String, usize)> {
    let open = ListFilter {
        collapsed: HashSet::new(),
        ..f.clone()
    };
    let mut out: Vec<(String, usize)> = Vec::new();
    for i in self::tools(tools, &open) {
        let Some(g) = tool_group(&tools[i], f.group) else {
            return Vec::new();
        };
        match out.last_mut() {
            Some((last, n)) if *last == g => *n += 1,
            _ => out.push((g, 1)),
        }
    }
    out
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
    key: &crossterm::event::KeyEvent,
    sorts: usize,
    quicks: usize,
) -> bool {
    use crossterm::event::KeyCode;
    if f.editing {
        if super::query::edit(&mut f.query, key).is_some() {
            return true;
        }
        if !super::query::plain(key) {
            return false;
        }
        match key.code {
            KeyCode::Enter => f.editing = false,
            KeyCode::Esc => {
                f.editing = false;
                f.query.clear();
            }
            _ => return false,
        }
        return true;
    }
    if !super::query::plain(key) {
        return false;
    }
    match key.code {
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
    #[test]
    fn full_key_events_keep_control_and_alt_chords_out_of_filters() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut filter = ListFilter {
            query: "alpha 日本語".into(),
            editing: true,
            ..Default::default()
        };
        assert!(key(
            &mut filter,
            &KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL),
            2,
            3
        ));
        assert_eq!(filter.query, "alpha ");
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            assert!(key(
                &mut filter,
                &KeyEvent::new(KeyCode::Char('b'), modifiers),
                2,
                3
            ));
            assert_eq!(filter.query, "alpha ");
        }
        assert!(key(
            &mut filter,
            &KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
            2,
            3
        ));
        assert!(filter.query.is_empty());
        filter.editing = false;
        for code in [KeyCode::Char('s'), KeyCode::Char('v'), KeyCode::Char('/')] {
            assert!(!key(
                &mut filter,
                &KeyEvent::new(code, KeyModifiers::CONTROL),
                2,
                3
            ));
            assert_eq!((filter.sort, filter.quick, filter.editing), (0, 0, false));
        }
    }

    fn tool(name: &str, rate: Option<f64>, calls: i64, retired: bool) -> ToolInfo {
        ToolInfo {
            id: name.into(),
            name: name.into(),
            title: String::new(),
            tags: Vec::new(),
            pinned: false,
            disabled: false,
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
            recent: String::new(),
            author: "agent".into(),
        }
    }

    #[test]
    fn bare_tool_tag_prefix_keeps_untagged_tools_under_every_quick_filter() {
        let mut items = vec![
            tool("plain", Some(0.7), 1, false),
            tool("tagged", Some(0.9), 2, false),
            tool("retired", None, 3, true),
        ];
        items[0].needs_attention = true;
        items[1].tags = vec!["work".into()];
        items[2].tags = vec!["workshop".into()];
        for quick in 0..TOOL_QUICK.len() {
            for sort in 0..TOOL_SORTS.len() {
                let empty = ListFilter {
                    quick,
                    sort,
                    ..Default::default()
                };
                let bare = ListFilter {
                    query: " # ".into(),
                    ..empty.clone()
                };
                assert_eq!(tools(&items, &bare), tools(&items, &empty));
            }
        }
        assert_eq!(
            tools(
                &items,
                &ListFilter {
                    query: "#WOrK".into(),
                    ..Default::default()
                }
            ),
            vec![2, 1]
        );
        assert!(tools(
            &items,
            &ListFilter {
                query: "#missing".into(),
                ..Default::default()
            }
        )
        .is_empty());
        assert_eq!(
            tools(
                &items,
                &ListFilter {
                    query: "pl".into(),
                    ..Default::default()
                }
            ),
            vec![0]
        );
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
    fn tools_group_by_capability_author_or_status() {
        let mut ts = vec![
            tool("read_file", Some(0.99), 5, false),
            tool("http_get", Some(0.5), 9, false),
            tool("old_csv", None, 1, true),
            tool("fetch_json", Some(0.9), 2, false),
        ];
        ts[0].capabilities = vec!["fs:read:/tmp".into()];
        ts[1].capabilities = vec!["net:example.com".into(), "fs:write:/tmp".into()];
        ts[3].capabilities = vec!["net:api.example.com".into()];
        ts[1].needs_attention = true;
        ts[3].author = "user".into();
        let names = |idx: Vec<usize>| {
            idx.into_iter()
                .map(|i| ts[i].name.as_str())
                .collect::<Vec<_>>()
        };
        let mut f = ListFilter {
            group: 1,
            ..Default::default()
        };
        assert_eq!(
            tool_groups(&ts, &f),
            [
                ("fs".into(), 1),
                ("fs + net".into(), 1),
                ("net".into(), 1),
                ("no capabilities".into(), 1)
            ]
        );
        f.group = 3;
        assert_eq!(
            names(tools(&ts, &f)),
            ["http_get", "fetch_json", "read_file", "old_csv"]
        );
        // Folding a group hides its Tools but keeps its header.
        f.collapsed.insert("active".into());
        assert_eq!(names(tools(&ts, &f)), ["http_get", "old_csv"]);
        assert_eq!(tool_groups(&ts, &f).len(), 3);
        f.group = 2;
        f.collapsed.clear();
        assert_eq!(
            tool_groups(&ts, &f),
            [("agent".into(), 3), ("user".into(), 1)]
        );
    }

    #[test]
    fn filter_keys() {
        use crossterm::event::KeyCode;
        let key_event =
            |code| crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE);
        let mut f = ListFilter::default();
        assert!(key(&mut f, &key_event(KeyCode::Char('/')), 2, 2));
        assert!(
            key(&mut f, &key_event(KeyCode::Char('s')), 2, 2),
            "typed while editing"
        );
        assert_eq!(f.query, "s");
        key(&mut f, &key_event(KeyCode::Enter), 2, 2);
        assert!(key(&mut f, &key_event(KeyCode::Char('s')), 2, 2));
        assert_eq!(f.sort, 1);
        assert!(
            key(&mut f, &key_event(KeyCode::Esc), 2, 2),
            "clears the filter first"
        );
        assert!(f.query.is_empty());
        assert!(
            !key(&mut f, &key_event(KeyCode::Esc), 2, 2),
            "then Esc leaves the view"
        );
        assert!(!key(&mut f, &key_event(KeyCode::Char('x')), 2, 2));
    }
}
