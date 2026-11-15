//! Text-only access to the existing GetArtifact slice envelope.
use super::*;

#[derive(Subcommand)]
pub(super) enum Action {
    /// Read decoded text, not a byte-exact download; scalar slices may divide a grapheme
    Show {
        /// Stored artifact ID
        id: String,
        /// Starting Unicode scalar offset (default 0), not a byte offset
        #[arg(long)]
        offset: Option<usize>,
        /// Unicode scalars to read (daemon default 200000, cap 1000000)
        #[arg(long)]
        length: Option<usize>,
        /// Print the existing complete response envelope as JSON
        #[arg(long)]
        json: bool,
    },
}

pub(super) fn run(paths: &Paths, action: Action) -> Result<()> {
    let Action::Show {
        id,
        offset,
        length,
        json,
    } = action;
    formless_core::daemon::ensure_daemon(paths)?;
    let value =
        formless_tui::client::request_once(paths, Request::GetArtifact { id, offset, length })?;
    render(
        &value,
        json,
        length,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
}

fn render(
    value: &Value,
    json: bool,
    length: Option<usize>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<()> {
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(value)?)?;
        return Ok(());
    }
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .context("artifact response has no text")?;
    out.write_all(text.as_bytes())?;
    let offset = value["offset"].as_u64();
    let end = value["end"].as_u64();
    let total = value["total_chars"].as_u64();
    if let (Some(offset), Some(end), Some(total), Some(id)) =
        (offset, end, total, value["id"].as_str())
    {
        if end > offset && end < total {
            // Stored ids are UUIDs. Omit the optional shell hint if a future
            // response uses an id that cannot be pasted as one literal word.
            if !id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
                || !id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            {
                return Ok(());
            }
            let mut hint = format!("Next slice: formless artifacts show {id} --offset {end}");
            if let Some(length) = length {
                hint.push_str(&format!(" --length {length}"));
            }
            writeln!(err, "{}", comparison_terminal_text(&hint))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn artifact_text_rendering_preserves_exact_text_json_and_progress_hints() {
        let value = serde_json::json!({"id":"fixture","session_id":"session","bytes":999,"total_chars":10,"offset":2,"end":4,"text":"ไทย\n","future":{"kept":true}});
        let (mut out, mut err) = (Vec::new(), Vec::new());
        render(&value, false, Some(2), &mut out, &mut err).unwrap();
        assert_eq!(out, "ไทย\n".as_bytes());
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "Next slice: formless artifacts show fixture --offset 4 --length 2\n"
        );
        let (mut out, mut err) = (Vec::new(), Vec::new());
        render(&value, true, None, &mut out, &mut err).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), value);
        assert!(err.is_empty());
        for (offset, end, total) in [
            (0, 0, 10),
            (10, 10, 10),
            (20, 20, 10),
            (0, 0, 0),
            (0, 10, 10),
        ] {
            let value = serde_json::json!({"id":"fixture","offset":offset,"end":end,"total_chars":total,"text":""});
            let (mut out, mut err) = (Vec::new(), Vec::new());
            render(&value, false, Some(0), &mut out, &mut err).unwrap();
            assert!(out.is_empty() && err.is_empty());
        }
    }
}
