//! Human presentation of stored Session metadata; JSON remains in the caller.
use super::*;

fn status(session: &SessionInfo) -> &str {
    session
        .last_run_status
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("not recorded")
}

pub(super) fn empty(list: &[SessionInfo], archived: bool) -> String {
    if list.is_empty() {
        return "No Sessions in this response.".into();
    }
    let other = list.iter().filter(|s| s.archived != archived).count();
    let selected = if archived { "archived" } else { "active" };
    let alternate = if archived { "active" } else { "archived" };
    let flag = if archived { "" } else { " --archived" };
    format!(
        "No {selected} Sessions. {other} {alternate} Session{}; use formless sessions list{flag}.",
        if other == 1 { "" } else { "s" }
    )
}

pub(super) fn list_table(shown: &[&SessionInfo]) -> Table {
    let mut table = Table::new(&[
        "ID",
        "UPDATED",
        "MSGS",
        "NAME",
        "LAST RECORDED RUN",
        "TAGS",
        "LAST MESSAGE",
    ]);
    for s in shown {
        table.row(vec![
            t(s.id.chars().take(8).collect::<String>(), Tone::Dim),
            c(s.updated_at.get(..16).unwrap_or(&s.updated_at)),
            c(s.message_count.to_string()),
            t(
                format!("{}{}", if s.pinned { "★ " } else { "" }, s.name),
                Tone::Strong,
            ),
            c(status(s)),
            c(s.tags.join(", ")),
            t(s.preview.clone().unwrap_or_default(), Tone::Dim),
        ]);
    }
    table
}

pub(super) fn metadata(session: &SessionInfo) -> String {
    format!(
        "Archived: {}\nPinned: {}\nTags: {}\nLast recorded Run status: {}\n",
        if session.archived { "yes" } else { "no" },
        if session.pinned { "yes" } else { "no" },
        if session.tags.is_empty() {
            "none".into()
        } else {
            comparison_terminal_text(&session.tags.join(", "))
        },
        comparison_terminal_text(status(session)),
    )
}
