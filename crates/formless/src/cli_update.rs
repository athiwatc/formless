//! Release reporting only; the network lookup and upgrade text stay in main.
use super::*;

fn json_report(
    out: &mut impl Write,
    current: &str,
    status: &str,
    newer: &[(String, String, String)],
) -> Result<()> {
    let releases: Vec<_> = newer
        .iter()
        .map(|(tag, notes, url)| serde_json::json!({"tag":tag,"url":url,"notes":notes}))
        .collect();
    writeln!(
        out,
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"installed":current,"status":status,"newer_releases":releases})
        )?
    )?;
    Ok(())
}

pub(super) fn report(
    out: &mut impl Write,
    check: bool,
    json: bool,
    current: &str,
    upgrade: &str,
    fetch: impl FnOnce() -> Result<String>,
) -> Result<()> {
    let body = match fetch() {
        Ok(body) => body,
        Err(error) if update_http::is_not_found(&error) => {
            if json {
                return json_report(out, current, "release_check_unavailable", &[]);
            }
            writeln!(out, "installed: v{current}")?;
            writeln!(out, "latest:    (no published releases yet)")?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let releases: Value = serde_json::from_str(&body).context("parse releases")?;
    let newer = newer_releases(&releases, current);
    if json {
        return json_report(
            out,
            current,
            if newer.is_empty() {
                "up_to_date"
            } else {
                "update_available"
            },
            &newer,
        );
    }
    writeln!(out, "installed: v{current}")?;
    let Some((latest, _, url)) = newer.last() else {
        writeln!(out, "up to date")?;
        return Ok(());
    };
    writeln!(out, "latest:    {latest}  ({url})")?;
    writeln!(out)?;
    for (tag, notes, _) in &newer {
        writeln!(out, "── {tag} ──")?;
        writeln!(
            out,
            "{}",
            if notes.is_empty() {
                "(no release notes)"
            } else {
                notes
            }
        )?;
        writeln!(out)?;
    }
    if check {
        writeln!(
            out,
            "{} newer release(s); `formless update` shows how to upgrade.",
            newer.len()
        )?;
    } else {
        writeln!(out, "To upgrade to {latest}:\n  {upgrade}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn releases() -> String {
        serde_json::json!([
            {"tag_name":"v1.2.0","body":"  newest ไทย\ninstalled: decorative notes\n ","html_url":"url-two"},
            {"tag_name":"v1.1.0","body":" 日本語\n\nFirst notes ","html_url":"url-one"},
            {"tag_name":"v1.3.0","body":"skip draft","draft":true},
            {"tag_name":"v1.4.0","body":"skip pre","prerelease":true},
            {"tag_name":"not-version","body":"skip invalid"},
            {"tag_name":"v1.0.0","body":"current"}
        ]).to_string()
    }
    #[test]
    fn every_update_json_placement_routes_to_the_same_single_report() {
        let expected = serde_json::json!({"installed":"1.0.0","status":"update_available","newer_releases":[
            {"tag":"v1.1.0","url":"url-one","notes":"日本語\n\nFirst notes"},
            {"tag":"v1.2.0","url":"url-two","notes":"newest ไทย\ninstalled: decorative notes"}
        ]});
        for args in [
            vec!["formless", "update", "--json"],
            vec!["formless", "update", "check", "--json"],
            vec!["formless", "update", "--json", "check"],
            vec!["formless", "update", "--check", "--json"],
            vec!["formless", "update", "--json", "--check"],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            let Some(Cmd::Update {
                action,
                check,
                json,
            }) = cli.cmd
            else {
                panic!()
            };
            assert!(json);
            let mut out = Vec::new();
            report(
                &mut out,
                check || matches!(action, Some(UpdateAction::Check)),
                json,
                "1.0.0",
                "never executed",
                || Ok(releases()),
            )
            .unwrap();
            let value: Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(value, expected);
            assert!(value.get("latest").is_none() && value.get("upgrade_command").is_none());
        }
    }
    #[test]
    fn unavailable_page_scope_and_failures_preserve_output_and_exit_semantics() {
        for (body,status) in [(Ok("[]".into()),"up_to_date"),(Ok(serde_json::json!([{"tag_name":"v0.9.0"},{"tag_name":"v9.0.0","prerelease":true}]).to_string()),"up_to_date"),(Err(update_http::fixture_not_found_error()),"release_check_unavailable")] {
            let mut out=Vec::new();
            report(&mut out,true,true,"1.0.0","never executed",||body).unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(),serde_json::json!({"installed":"1.0.0","status":status,"newer_releases":[]}));
        }
        for body in [
            Err(anyhow::anyhow!("transport failed")),
            Ok("not release JSON".into()),
        ] {
            let mut out = Vec::new();
            let error =
                report(&mut out, true, true, "1.0.0", "never executed", || body).unwrap_err();
            assert_eq!(exit_code(&error), 1);
            assert!(out.is_empty());
        }
    }
    #[test]
    fn unrelated_404_text_fails_without_output_in_human_and_json_reports() {
        for check in [false, true] {
            for json in [false, true] {
                let mut out = Vec::new();
                let error = report(&mut out, check, json, "1.0.0", "never executed", || {
                    Err(anyhow::anyhow!("unrelated transport error mentioning 404"))
                })
                .unwrap_err();
                assert_eq!(exit_code(&error), 1);
                assert_eq!(
                    error.to_string(),
                    "unrelated transport error mentioning 404"
                );
                assert!(out.is_empty(), "check={check}, json={json}: {out:?}");
            }
        }
    }

    #[test]
    fn contextual_typed_404_preserves_the_unavailable_report_in_both_formats() {
        for check in [false, true] {
            for json in [false, true] {
                let mut out = Vec::new();
                report(&mut out, check, json, "1.0.0", "never executed", || {
                    Err(update_http::fixture_not_found_error().context("release lookup failed"))
                })
                .unwrap();
                if json {
                    assert_eq!(
                        serde_json::from_slice::<Value>(&out).unwrap(),
                        serde_json::json!({"installed":"1.0.0","status":"release_check_unavailable","newer_releases":[]})
                    );
                } else {
                    assert_eq!(
                        out,
                        b"installed: v1.0.0\nlatest:    (no published releases yet)\n"
                    );
                }
            }
        }
    }

    #[test]
    fn human_reports_keep_all_pre_change_bytes_including_full_notes_and_footers() {
        let notes="latest:    v1.2.0  (url-two)\n\n── v1.1.0 ──\n日本語\n\nFirst notes\n\n── v1.2.0 ──\nnewest ไทย\ninstalled: decorative notes\n\n";
        for check in [false, true] {
            let mut out = Vec::new();
            report(&mut out, check, false, "1.0.0", "fixture upgrade", || {
                Ok(releases())
            })
            .unwrap();
            let footer = if check {
                "2 newer release(s); `formless update` shows how to upgrade.\n"
            } else {
                "To upgrade to v1.2.0:\n  fixture upgrade\n"
            };
            assert_eq!(
                String::from_utf8(out).unwrap(),
                format!("installed: v1.0.0\n{notes}{footer}")
            );
        }
        for (body, expected) in [
            (Ok("[]".into()), "installed: v1.0.0\nup to date\n"),
            (
                Err(update_http::fixture_not_found_error()),
                "installed: v1.0.0\nlatest:    (no published releases yet)\n",
            ),
        ] {
            let mut out = Vec::new();
            report(&mut out, false, false, "1.0.0", "fixture upgrade", || body).unwrap();
            assert_eq!(out, expected.as_bytes());
        }
    }
}
