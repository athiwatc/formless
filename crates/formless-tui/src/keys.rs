//! The per-view key table (SPEC §10.5): one list drives the hint bar, so
//! every view shows its keys the same way. Lower `rank` = more important;
//! narrow terminals show only the top-ranked ones.

use crate::View;

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
];

const TOOLS: &[Hint] = &[
    h("Enter", "detail", 0),
    h("[ ]", "calls", 4),
    h("E", "edit", 2),
    h("t", "try it", 3),
    h("o", "optimise", 4),
    h("x", "retire", 3),
    h("b", "rollback", 3),
    h("e", "export", 5),
    h("i", "import", 5),
    h("p", "purge", 6),
    h("/", "filter", 1),
    h("s", "sort", 6),
    h("v", "show", 6),
];

const METRICS: &[Hint] = &[
    h("j k", "move", 0),
    h("g G", "top/bottom", 2),
    h("Tab", "views", 1),
];

const PROPOSALS: &[Hint] = &[
    h("n", "new (costs tokens)", 1),
    h("a", "apply", 0),
    h("r", "reject", 0),
    h("b", "roll back", 2),
    h("/", "filter", 3),
    h("s", "sort", 4),
    h("v", "status", 4),
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
];

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
    let all = table(view);
    let mut order: Vec<usize> = (0..all.len()).collect();
    order.sort_by_key(|&i| all[i].rank);
    let item = |i: usize| format!("{} {}", all[i].keys, all[i].action);
    let mut chosen: Vec<usize> = Vec::new();
    let mut used = 0;
    for i in order {
        let w = item(i).chars().count() + if chosen.is_empty() { 0 } else { 3 };
        if used + w > width {
            continue;
        }
        used += w;
        chosen.push(i);
    }
    chosen.sort_unstable();
    chosen.into_iter().map(item).collect::<Vec<_>>().join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_terminals_keep_the_important_hints() {
        let wide = line(View::Sessions, 200);
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
