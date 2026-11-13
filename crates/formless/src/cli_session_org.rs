//! Explicit Session organization through the existing normalizing endpoints.
use super::*;

pub(super) fn pin(paths: &Paths, id: &str, pinned: bool, json: bool) -> Result<()> {
    let session = find_session(paths, id)?;
    let result = formless_tui::client::request_once(
        paths,
        Request::SetSessionPinned {
            id: session.id,
            pinned,
        },
    )?;
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let stored: SessionInfo = serde_json::from_value(result)?;
        println!(
            "Session ID: {}\nStored pinned: {}",
            comparison_terminal_text(&stored.id),
            if stored.pinned { "yes" } else { "no" }
        );
    }
    Ok(())
}

pub(super) fn tag(
    paths: &Paths,
    id: &str,
    tags: Vec<String>,
    clear: bool,
    json: bool,
) -> Result<()> {
    let session = find_session(paths, id)?;
    let replace = clear || !tags.is_empty();
    let result = if replace {
        formless_tui::client::request_once(
            paths,
            Request::SetSessionTags {
                id: session.id,
                tags,
            },
        )?
    } else {
        serde_json::to_value(session)?
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let stored: SessionInfo = serde_json::from_value(result)?;
        println!("Session ID: {}", comparison_terminal_text(&stored.id));
        let label = if replace {
            "Stored tags (replacement result)"
        } else {
            "Stored tags"
        };
        println!(
            "{label}: {}",
            if stored.tags.is_empty() {
                "none".into()
            } else {
                comparison_terminal_text(&stored.tags.join(", "))
            }
        );
    }
    Ok(())
}
