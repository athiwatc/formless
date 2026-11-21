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

pub(super) fn selection_empty(
    list: &[SessionInfo],
    archived: bool,
    pinned: bool,
    tag: Option<&str>,
) -> String {
    if !list.is_empty() && (pinned || tag.is_some()) {
        let mut filters = Vec::new();
        if archived {
            filters.push("--archived".to_string());
        }
        if pinned {
            filters.push("--pinned".to_string());
        }
        if let Some(tag) = tag {
            filters.push(format!("--tag {}", comparison_terminal_text(tag)));
        }
        return format!(
            "no Sessions match {} (formless sessions list)",
            filters.join(" ")
        );
    }
    empty(list, archived)
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
            c(formless_tui::local_timestamp(&s.updated_at)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session(archived: bool) -> SessionInfo {
        serde_json::from_value(json!({"id":"日本語日本語日本語","name":"ไทย","created_at":"","updated_at":"2026-11-01T12:34:56Z","archived":archived,"pinned":true,"tags":["work","ไทย\u{1b}[31m"],"message_count":7,"preview":"preview 日本語","last_run_status":"paused"})).unwrap()
    }

    #[test]
    fn empty_replies_archive_partitions_and_main_filter_messages_stay_distinct() {
        assert_eq!(
            selection_empty(&[], false, false, None),
            "No Sessions in this response."
        );
        let archived = session(true);
        assert_eq!(
            selection_empty(&[archived], false, false, None),
            "No active Sessions. 1 archived Session; use formless sessions list --archived."
        );
        assert_eq!(
            selection_empty(&[session(false)], true, false, None),
            "No archived Sessions. 1 active Session; use formless sessions list."
        );
        assert_eq!(
            selection_empty(&[session(false)], true, true, Some("WORK\n")),
            "no Sessions match --archived --pinned --tag WORK\\u{a} (formless sessions list)"
        );
        assert_eq!(
            selection_empty(&[], false, true, Some("work")),
            "No Sessions in this response."
        );
    }

    #[test]
    fn stored_metadata_escapes_controls_and_marks_unrecorded_status_without_mutation() {
        let mut session = session(true);
        let before = serde_json::to_value(&session).unwrap();
        assert_eq!(metadata(&session), "Archived: yes\nPinned: yes\nTags: work, ไทย\\u{1b}[31m\nLast recorded Run status: paused\n");
        assert_eq!(serde_json::to_value(&session).unwrap(), before);
        for status in [None, Some(String::new())] {
            session.last_run_status = status;
            assert!(metadata(&session).ends_with("Last recorded Run status: not recorded\n"));
        }
        session.tags.clear();
        assert!(metadata(&session).contains("Tags: none\n"));
    }

    #[test]
    fn metadata_table_preserves_fields_unicode_and_pinned_marker_in_both_layouts() {
        let session = session(false);
        let table = list_table(&[&session]);
        let pipe = table.render(false, false, None);
        for value in [
            "LAST RECORDED RUN",
            "TAGS",
            "paused",
            "work, ไทย\\u{1b}[31m",
            "★ ไทย",
            "preview 日本語",
            "日本語日本語日本",
        ] {
            assert!(pipe.contains(value), "missing {value}: {pipe}");
        }
        assert!(!pipe.contains('\u{1b}'));
        let narrow = table.render(true, false, Some(20));
        for value in ["ID:", "TAGS:", "paused", "preview 日本語"] {
            assert!(narrow.contains(value), "missing {value}: {narrow}");
        }
        assert!(!narrow.contains('\u{1b}'));
    }
}
