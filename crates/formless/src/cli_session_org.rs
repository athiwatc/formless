//! Explicit Session organization through the existing normalizing endpoints.
use super::*;

pub(super) fn pin(paths: &Paths, id: &str, pinned: bool, json: bool) -> Result<()> {
    let session = find_session(paths, id)?;
    pin_resolved(
        session,
        pinned,
        json,
        |request| formless_tui::client::request_once(paths, request),
        &mut std::io::stdout().lock(),
    )
}

fn pin_resolved(
    session: SessionInfo,
    pinned: bool,
    json: bool,
    mut request: impl FnMut(Request) -> Result<Value>,
    out: &mut impl Write,
) -> Result<()> {
    let result = request(Request::SetSessionPinned {
        id: session.id,
        pinned,
    })?;
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(&result)?)?;
    } else {
        let stored: SessionInfo = serde_json::from_value(result)?;
        writeln!(
            out,
            "Session ID: {}\nStored pinned: {}",
            comparison_terminal_text(&stored.id),
            if stored.pinned { "yes" } else { "no" }
        )?;
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
    tag_resolved(
        session,
        tags,
        clear,
        json,
        |request| formless_tui::client::request_once(paths, request),
        &mut std::io::stdout().lock(),
    )
}

fn tag_resolved(
    session: SessionInfo,
    tags: Vec<String>,
    clear: bool,
    json: bool,
    mut request: impl FnMut(Request) -> Result<Value>,
    out: &mut impl Write,
) -> Result<()> {
    let replace = clear || !tags.is_empty();
    let result = if replace {
        request(Request::SetSessionTags {
            id: session.id,
            tags,
        })?
    } else {
        serde_json::to_value(session)?
    };
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(&result)?)?;
    } else {
        let stored: SessionInfo = serde_json::from_value(result)?;
        writeln!(out, "Session ID: {}", comparison_terminal_text(&stored.id))?;
        let label = if replace {
            "Stored tags (replacement result)"
        } else {
            "Stored tags"
        };
        writeln!(
            out,
            "{label}: {}",
            if stored.tags.is_empty() {
                "none".into()
            } else {
                comparison_terminal_text(&stored.tags.join(", "))
            }
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn session() -> SessionInfo {
        serde_json::from_value(json!({"id":"full-session-id","name":"ไทย","tags":["existing"],"created_at":"","updated_at":""})).unwrap()
    }

    #[test]
    fn pins_send_the_resolved_id_and_display_returned_state_and_complete_json() {
        for requested in [false, true] {
            let returned = json!({"id":"full-session-id","pinned":!requested,"tags":[],"name":"ไทย","created_at":"","updated_at":"","future":[null,9_007_199_254_740_999_u64]});
            for json_mode in [false, true] {
                let mut requests = Vec::new();
                let mut out = Vec::new();
                pin_resolved(
                    session(),
                    requested,
                    json_mode,
                    |request| {
                        requests.push(request);
                        Ok(returned.clone())
                    },
                    &mut out,
                )
                .unwrap();
                assert!(
                    matches!(&requests[..], [Request::SetSessionPinned { id, pinned }] if id == "full-session-id" && *pinned == requested)
                );
                if json_mode {
                    assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), returned);
                } else {
                    assert_eq!(
                        String::from_utf8(out).unwrap(),
                        format!(
                            "Session ID: full-session-id\nStored pinned: {}\n",
                            if requested { "no" } else { "yes" }
                        )
                    );
                }
            }
        }
    }

    #[test]
    fn tags_distinguish_reads_replacement_and_explicit_clear() {
        let mut out = Vec::new();
        tag_resolved(
            session(),
            Vec::new(),
            false,
            true,
            |_| panic!("read-only tags must not send a mutation"),
            &mut out,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&out).unwrap(),
            serde_json::to_value(session()).unwrap()
        );
        let raw = vec![
            " WORK ".to_string(),
            "WORK".into(),
            "#Tag".into(),
            "ไทย".into(),
        ];
        for clear in [false, true] {
            let tags = if clear { Vec::new() } else { raw.clone() };
            let stored = json!({"id":"full-session-id","name":"ไทย","tags":if clear {json!([])} else {json!(["normalized","ไทย\u{1b}[31m"])},"created_at":"","updated_at":"","future":{"kept":true}});
            for json_mode in [false, true] {
                let mut requests = Vec::new();
                let mut out = Vec::new();
                tag_resolved(
                    session(),
                    tags.clone(),
                    clear,
                    json_mode,
                    |request| {
                        requests.push(request);
                        Ok(stored.clone())
                    },
                    &mut out,
                )
                .unwrap();
                assert!(
                    matches!(&requests[..], [Request::SetSessionTags { id, tags: sent }] if id == "full-session-id" && sent == &tags)
                );
                if json_mode {
                    assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), stored);
                } else {
                    let text = String::from_utf8(out).unwrap();
                    assert!(text.contains("Stored tags (replacement result):"));
                    assert!(!text.contains('\u{1b}'));
                    assert!(text.contains(if clear { "none" } else { "normalized" }));
                }
            }
        }
    }

    #[test]
    fn failed_mutations_have_no_partial_output_and_clear_conflicts_are_parser_errors() {
        let mut out = Vec::new();
        assert_eq!(
            pin_resolved(session(), true, true, |_| bail!("pin failed"), &mut out)
                .unwrap_err()
                .to_string(),
            "pin failed"
        );
        assert!(out.is_empty());
        assert_eq!(
            tag_resolved(
                session(),
                vec!["replacement".into()],
                false,
                false,
                |_| bail!("tag failed"),
                &mut out
            )
            .unwrap_err()
            .to_string(),
            "tag failed"
        );
        assert!(out.is_empty());
        assert!(
            Cli::try_parse_from(["formless", "sessions", "tag", "session", "work", "--clear"])
                .is_err()
        );
        assert!(Cli::try_parse_from([
            "formless", "sessions", "tag", "session", "--clear", "--json"
        ])
        .is_ok());
    }
}
