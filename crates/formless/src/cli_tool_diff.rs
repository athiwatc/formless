//! Existing sectioned Tool-version comparison; no client GetTool or mutation.
use super::*;

pub(super) fn run(
    paths: &Paths,
    name: &str,
    from: Option<i64>,
    to: Option<i64>,
    json: bool,
) -> Result<()> {
    let tool = find_tool(paths, name)?; // ListTools, preserving the existing resolver.
    let (from, to) = versions(tool.current_version, from, to)?;
    let value = formless_tui::client::request_once(
        paths,
        Request::DiffToolVersions {
            id: tool.id,
            from,
            to,
        },
    )?;
    render(&value, json, &mut std::io::stdout().lock())
}
fn versions(current: i64, from: Option<i64>, to: Option<i64>) -> Result<(i64, i64)> {
    let to = to.unwrap_or(current);
    if to < 1 {
        return Err(usage("target version must be positive"));
    }
    let from = from
        .or_else(|| to.checked_sub(1))
        .filter(|version| *version >= 1)
        .ok_or_else(|| usage("no preceding version below v1; give --from and --to explicitly"))?;
    Ok((from, to))
}
fn render(value: &Value, json: bool, out: &mut impl Write) -> Result<()> {
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(value)?)?;
    } else {
        out.write_all(
            value["diff"]
                .as_str()
                .context("Tool diff response has no diff text")?
                .as_bytes(),
        )?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diff_defaults_follow_target_not_history_and_use_checked_predecessors() {
        assert_eq!(versions(3, None, None).unwrap(), (2, 3));
        assert_eq!(versions(3, Some(1), Some(2)).unwrap(), (1, 2));
        assert_eq!(versions(3, None, Some(2)).unwrap(), (1, 2));
        assert!(versions(1, None, None).is_err());
        assert!(versions(i64::MIN, None, None).is_err());
        assert_eq!(
            versions(i64::MAX, None, None).unwrap(),
            (i64::MAX - 1, i64::MAX)
        );
        assert_eq!(versions(1, Some(1), Some(3)).unwrap(), (1, 3));
        assert_eq!(versions(1, Some(3), None).unwrap(), (3, 1));
    }
    #[test]
    fn diff_output_keeps_exact_returned_text_and_complete_json() {
        for text in ["ไทย 日本語\n--- code\n+changed", "No changes."] {
            let value = serde_json::json!({"diff":text,"schema_compatibility":{"future":[null,true]},"extra":"kept"});
            let mut out = Vec::new();
            render(&value, false, &mut out).unwrap();
            assert_eq!(out, text.as_bytes());
            let mut out = Vec::new();
            render(&value, true, &mut out).unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), value);
        }
    }
    #[test]
    fn diff_cli_requires_positive_representable_versions() {
        for flag in ["--from=0", "--to=-1", "--from=99999999999999999999999"] {
            assert!(Cli::try_parse_from(["formless", "tools", "diff", "fixture", flag]).is_err());
        }
        assert!(Cli::try_parse_from([
            "formless", "tools", "diff", "fixture", "--from=1", "--to=2", "--json"
        ])
        .is_ok());
    }
}
