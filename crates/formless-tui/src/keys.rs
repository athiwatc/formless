//! The per-view key table (SPEC §10.5): one list drives the hint bar, so
//! every view shows its keys the same way. Lower `rank` = more important;
//! narrow terminals show only the top-ranked ones.

use crate::{text, View};

pub struct Hint {
    pub keys: &'static str,
    pub action: &'static str,
    pub rank: u8,
}

const fn h(keys: &'static str, action: &'static str, rank: u8) -> Hint {
    Hint { keys, action, rank }
}

const CHAT: &[Hint] = &[
    h("Enter", "send", 0),
    h("/", "commands", 1),
    h("@", "file", 3),
    h("[ ]", "focus", 4),
    h("Ctrl+F", "find", 5),
    h("Ctrl+G", "editor", 5),
    h("Ctrl+K", "switch", 5),
    h("Tab", "views", 2),
    h("?", "help", 2),
];

const SESSIONS: &[Hint] = &[
    h("Enter", "open", 0),
    h("n", "new", 1),
    h("f", "fork", 3),
    h("r", "rename", 3),
    h("d", "delete", 2),
    h("P", "pin", 4),
    h("a", "archive", 5),
    h("u", "undo delete", 5),
    h("Space", "select", 5),
    h("e", "export", 5),
    h("i", "import", 5),
    h("/", "filter", 4),
    h("s", "sort", 6),
    h("v", "running/archived", 6),
    h("T", "session tree", 5),
    h("t", "tag marked Sessions", 6),
    h("PgUp/PgDn", "page", 4),
];

const TOOLS: &[Hint] = &[
    h("Enter", "detail", 0),
    h("[ ]", "calls", 4),
    h("E", "edit", 2),
    h("t", "try it", 3),
    h("D S", "describe/schemas", 5),
    h("o", "optimise", 4),
    h("x", "retire/restore", 3),
    h("b", "rollback", 3),
    h("e", "export", 5),
    h("i", "import", 5),
    h("p", "purge", 6),
    h("/", "filter", 1),
    h("s", "sort", 6),
    h("v", "show", 6),
    h("c", "group", 6),
    h("z/Z", "fold selected group / unfold all", 6),
    h("J", "jump to the call’s Session", 6),
    h("PgUp/PgDn", "page", 4),
];

const METRICS: &[Hint] = &[
    h("j k", "move", 0),
    h("Enter", "Tool detail", 0),
    h("g G", "top/bottom", 2),
    h("r", "range", 1),
    h("b", "tokens by", 2),
    h("f", "forecast versus actual", 5),
    h("m", "model metrics (when available)", 6),
    h("Tab", "views", 1),
    h("PgUp/PgDn", "page", 4),
];

const PROPOSALS: &[Hint] = &[
    h("Enter", "review", 0),
    h("n", "new (costs tokens)", 1),
    h("a", "apply", 0),
    h("r", "reject", 0),
    h("b", "roll back", 2),
    h("/", "filter", 3),
    h("s", "sort", 4),
    h("v", "status", 4),
    h("PgUp/PgDn", "page", 4),
];

const SETTINGS: &[Hint] = &[
    h("↑↓", "select", 0),
    h("Enter", "change", 0),
    h("d", "remove", 2),
    h("Esc", "back", 1),
];

const SEARCH: &[Hint] = &[
    h("↑↓", "move", 0),
    h("Enter", "open", 0),
    h("Esc", "back", 1),
    h("PgUp/PgDn", "page", 4),
    h("j k", "move", 2),
    h("g G", "top/bottom", 3),
];

pub(crate) const LIST_MORE: &str = "? more";

pub(crate) fn list_key_title(view: View) -> Option<&'static str> {
    match view {
        View::Sessions => Some("Sessions keys"),
        View::Tools => Some("Tools keys"),
        View::Metrics => Some("Metrics keys"),
        View::Proposals => Some("Proposals keys"),
        View::Search => Some("Search keys"),
        View::Chat | View::Settings => None,
    }
}

pub(crate) fn contextual_help(view: View) -> Option<(&'static str, Vec<String>)> {
    let title = list_key_title(view)?;
    let mut lines = table(view)
        .iter()
        .map(|hint| format!("{}  {}", hint.keys, hint.action))
        .collect::<Vec<_>>();
    if view == View::Tools {
        lines.push("Enter with a call selected  open that call".into());
    }
    if view != View::Search {
        lines.push("↑ / ↓  move up/down".into());
    }
    if !matches!(view, View::Search | View::Metrics) {
        lines.push("j / k  move down/up".into());
        lines.push("g / G  first/last".into());
    }
    lines.push("Home / End  first/last".into());
    lines.push("Ctrl+D / Ctrl+U  move half a page".into());
    match view {
        View::Metrics => lines.push("Shift+Tab  previous view".into()),
        View::Search => lines.push("Tab / Shift+Tab  return to Chat".into()),
        _ => lines.push("Tab / Shift+Tab  switch views".into()),
    }
    match view {
        View::Sessions => lines.push("Esc  clear filter or selection, or return to Chat".into()),
        View::Tools | View::Proposals => lines.push("Esc  clear filter or return to Chat".into()),
        _ => {}
    }
    lines.push("?  show these keys (outside filter editing)".into());
    lines.push("Esc / q  close this help".into());
    Some((title, lines))
}

pub(crate) fn list_footer_truncated(view: View, width: usize) -> bool {
    if list_key_title(view).is_none() {
        return false;
    }
    let hints = table(view);
    let items = hints
        .iter()
        .map(|hint| text::width(hint.keys) + 1 + text::width(hint.action));
    items.sum::<usize>() + 3 * hints.len().saturating_sub(1) > width
}

pub fn table(view: View) -> &'static [Hint] {
    match view {
        View::Chat => CHAT,
        View::Sessions => SESSIONS,
        View::Tools => TOOLS,
        View::Metrics => METRICS,
        View::Proposals => PROPOSALS,
        View::Settings => SETTINGS,
        View::Search => SEARCH,
    }
}

/// The hints that fit in `width` columns, highest-ranked first chosen,
/// shown in table order: `Enter open · n new · d delete`.
pub fn line(view: View, width: usize) -> String {
    line_with_chat_focus(view, width, None)
}

#[derive(Clone, Copy)]
pub(crate) enum ChatFocus {
    Message,
    Call,
    Unavailable,
}

pub(crate) fn line_with_chat_focus(view: View, width: usize, focus: Option<ChatFocus>) -> String {
    let all = table(view);
    let mut order: Vec<usize> = (0..all.len())
        .filter(|&i| {
            !(view == View::Chat
                && all[i].keys == "Enter"
                && matches!(focus, Some(ChatFocus::Unavailable)))
        })
        .collect();
    order.sort_by_key(|&i| all[i].rank);
    let item = |i: usize| {
        let action = match (view, all[i].keys, focus) {
            (View::Chat, "Enter", Some(ChatFocus::Call)) => "toggle call",
            (View::Chat, "Enter", Some(ChatFocus::Message)) => "show focus",
            _ => all[i].action,
        };
        format!("{} {}", all[i].keys, action)
    };
    let mut chosen: Vec<usize> = Vec::new();
    let mut used = 0;
    let show_more = list_footer_truncated(view, width);
    let marker = if width >= text::width(LIST_MORE) {
        LIST_MORE
    } else if width > 0 {
        "?"
    } else {
        ""
    };
    let available = if show_more {
        width.saturating_sub(text::width(marker) + 3)
    } else {
        width
    };
    for i in order {
        let w = text::width(&item(i)) + if chosen.is_empty() { 0 } else { 3 };
        if used + w > available {
            continue;
        }
        used += w;
        chosen.push(i);
    }
    chosen.sort_unstable();
    let mut hints = chosen.into_iter().map(item).collect::<Vec<_>>();
    if show_more && !marker.is_empty() {
        hints.push(marker.into());
    }
    hints.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_footers_reserve_a_help_marker_and_keep_full_wide_hints() {
        for view in [
            View::Sessions,
            View::Tools,
            View::Metrics,
            View::Proposals,
            View::Search,
        ] {
            let wide = line(view, 512);
            assert!(!wide.contains(LIST_MORE));
            assert_eq!(line(view, text::width(&wide)), wide);
            for width in 0..=80 {
                let narrow = line(view, width);
                assert!(text::width(&narrow) <= width, "{view:?}: {narrow}");
                if width >= 6 && text::width(&wide) > width {
                    assert!(narrow.ends_with(LIST_MORE), "{view:?}: {narrow}");
                }
                if (1..6).contains(&width) {
                    assert_eq!(narrow, "?");
                }
            }
        }
    }

    #[test]
    fn narrow_terminals_keep_the_important_hints() {
        let wide = line(View::Sessions, 512);
        assert!(wide.starts_with("Enter open · n new · f fork"), "{wide}");
        assert!(wide.contains("v running"));
        let narrow = line(View::Sessions, 30);
        assert!(narrow.chars().count() <= 30, "{narrow}");
        assert!(
            narrow.contains("Enter open") && narrow.contains("n new"),
            "{narrow}"
        );
        assert!(!narrow.contains("running"));
        assert_eq!(line(View::Settings, 0), "");
    }
}
