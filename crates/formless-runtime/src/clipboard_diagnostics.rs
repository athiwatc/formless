//! Bounded Linux clipboard fallback diagnostics. Never report captured stdout.

const DETAIL_CHARS: usize = 256;

pub(super) fn failure(cmd: &str, kind: &str, detail: &str, payload: Option<&str>) -> String {
    // A write helper may echo its stdin in stderr. Remove the exact payload
    // before limiting/context flattening, including multiline payloads.
    let detail = match payload.filter(|text| !text.is_empty()) {
        Some(text) => detail.replace(text, "[clipboard text]"),
        None => detail.to_string(),
    };
    let mut chars = detail.trim().chars();
    let context: String = chars
        .by_ref()
        .take(DETAIL_CHARS)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let cut = if chars.next().is_some() { "…" } else { "" };
    if context.is_empty() {
        format!("{cmd}: {kind}")
    } else {
        format!("{cmd}: {kind}: {context}{cut}")
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
type Helper = (&'static str, &'static [&'static str]);

#[cfg(all(unix, not(target_os = "macos")))]
const READ: &[Helper] = &[
    ("wl-paste", &[]),
    ("xclip", &["-selection", "clipboard", "-o"]),
    ("xsel", &["--clipboard", "--output"]),
];
#[cfg(all(unix, not(target_os = "macos")))]
const WRITE: &[Helper] = &[
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
];

#[cfg(all(unix, not(target_os = "macos")))]
fn fallback(
    helpers: &[Helper],
    mut run: impl FnMut(&str, &[&str]) -> Result<String, String>,
) -> Result<String, String> {
    let mut failures = Vec::with_capacity(helpers.len());
    for (cmd, args) in helpers {
        match run(cmd, args) {
            Ok(output) => return Ok(output),
            Err(error) => failures.push(error),
        }
    }
    Err(format!("clipboard helpers failed: {}", failures.join("; ")))
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn read() -> Result<String, String> {
    fallback(READ, |cmd, args| {
        super::run_capture_impl(cmd, args, None, true)
    })
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(super) fn write(text: &str) -> Result<(), String> {
    fallback(WRITE, |cmd, args| {
        super::run_capture_impl(cmd, args, Some(text), true)
    })
    .map(|_| ())
}

#[cfg(all(test, unix, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn later_helper_success_preserves_output_and_stops_fallback() {
        for helpers in [READ, WRITE] {
            let mut tried = Vec::new();
            let output = fallback(helpers, |cmd, args| {
                tried.push((cmd.to_string(), args.join(" ")));
                if tried.len() == 2 {
                    Ok("clipboard stdout\n".into())
                } else {
                    Err(failure(
                        cmd,
                        "exited unsuccessfully (exit status: 1)",
                        "no display",
                        None,
                    ))
                }
            })
            .unwrap();
            assert_eq!(output, "clipboard stdout\n");
            assert_eq!(
                tried
                    .iter()
                    .map(|(cmd, _)| cmd.as_str())
                    .collect::<Vec<_>>(),
                vec![helpers[0].0, "xclip"]
            );
            assert_eq!(tried[1].1, helpers[1].1.join(" "));
        }
    }

    #[test]
    fn all_failed_keeps_order_context_and_missing_final_helper() {
        for helpers in [READ, WRITE] {
            let mut tried = 0;
            let error = fallback(helpers, |cmd, _| {
                tried += 1;
                Err(match tried {
                    1 => failure(
                        cmd,
                        "exited unsuccessfully (exit status: 1)",
                        "Wayland unavailable",
                        None,
                    ),
                    2 => failure(cmd, "exited unsuccessfully (exit status: 2)", "", None),
                    _ => failure(cmd, "could not spawn", "No such file or directory", None),
                })
            })
            .unwrap_err();
            assert!(error.contains(&format!(
                "{}: exited unsuccessfully (exit status: 1): Wayland unavailable",
                helpers[0].0
            )));
            assert!(error.contains("xclip: exited unsuccessfully (exit status: 2)"));
            assert!(error.ends_with("xsel: could not spawn: No such file or directory"));
        }
    }

    #[test]
    fn process_failures_have_status_without_stdout_or_write_payload() {
        let secret = "private clipboard\nsecond line";
        let error = super::super::run_capture_impl(
            "sh",
            &["-c", "printf 'stdout clipboard sentinel'; cat >&2; exit 7"],
            Some(secret),
            true,
        )
        .unwrap_err();
        assert!(error.contains("exit status: 7"), "{error}");
        assert!(!error.contains("stdout clipboard sentinel") && !error.contains(secret));
        assert!(error.contains("[clipboard text]"));
        let error =
            super::super::run_capture_impl("sh", &["-c", "exit 8"], None, true).unwrap_err();
        assert!(error.contains("exit status: 8"), "{error}");
        let error = super::super::run_capture_impl(
            "/formless-fixture-missing-clipboard-helper",
            &[],
            None,
            true,
        )
        .unwrap_err();
        assert!(error.contains("could not spawn"), "{error}");
    }

    #[test]
    fn diagnostic_context_is_bounded_unicode_safe_and_control_free() {
        let control = failure(
            "wl-paste",
            "exited unsuccessfully (exit status: 1)",
            "bad\u{1b}[31m\n\tline",
            None,
        );
        assert!(!control.chars().any(char::is_control));
        let detail = format!("\n{}\u{1b}[31m", "日".repeat(10_000));
        let error = fallback(READ, |cmd, _| {
            Err(failure(
                cmd,
                "exited unsuccessfully (exit status: 1)",
                &detail,
                None,
            ))
        })
        .unwrap_err();
        assert!(error.len() < 3500, "{}", error.len());
        assert_eq!(error.matches('…').count(), 3);
        assert!(!error.chars().any(char::is_control));
    }
}
