//! Bounded Linux clipboard fallback diagnostics. Never report captured stdout.

const DETAIL_CHARS: usize = 256;

pub(super) fn failure(cmd: &str, kind: &str, detail: &str, payload: Option<&str>) -> String {
    // Write stderr is untrusted: helpers can echo any fragment or encoding of
    // stdin. Keep only the trusted program/failure status, even for empty input.
    if payload.is_some() {
        return format!("{cmd}: {kind}");
    }
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
        assert_eq!(error, "sh: exited unsuccessfully (exit status: 7)");
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
    fn write_failures_omit_partial_escaped_and_empty_payload_context() {
        let text = "private clipboard\nsecond line";
        for detail in [
            "invalid input: private clipboard".into(),
            text.to_string(),
            serde_json::to_string(text).unwrap(),
            "private%20clipboard%0Asecond%20line".into(),
            "invalid input: ไทย 日本語".into(),
        ] {
            for payload in [Some(text), Some(""), Some("ไทย 日本語")] {
                assert_eq!(
                    failure(
                        "wl-copy",
                        "exited unsuccessfully (exit status: 7)",
                        &detail,
                        payload
                    ),
                    "wl-copy: exited unsuccessfully (exit status: 7)"
                );
            }
        }
        let error = fallback(WRITE, |cmd, _| {
            Err(failure(
                cmd,
                "exited unsuccessfully (exit status: 7)",
                "invalid input: private clipboard",
                Some(text),
            ))
        })
        .unwrap_err();
        assert_eq!(error, "clipboard helpers failed: wl-copy: exited unsuccessfully (exit status: 7); xclip: exited unsuccessfully (exit status: 7); xsel: exited unsuccessfully (exit status: 7)");
    }

    #[test]
    fn subprocess_write_partial_echo_is_hidden_and_read_context_remains() {
        let args = &[
            "-c",
            "printf 'stdout sentinel'; printf 'invalid input: private clipboard' >&2; exit 7",
        ];
        let error = super::super::run_capture_impl(
            "sh",
            args,
            Some("private clipboard\nsecond line"),
            true,
        )
        .unwrap_err();
        assert_eq!(error, "sh: exited unsuccessfully (exit status: 7)");
        let error = super::super::run_capture_impl("sh", args, None, true).unwrap_err();
        assert_eq!(
            error,
            "sh: exited unsuccessfully (exit status: 7): invalid input: private clipboard"
        );
    }

    #[test]
    fn shared_write_runner_suppresses_stderr_with_detailed_mode_off() {
        let args = &[
            "-c",
            "printf 'stdout sentinel'; printf 'invalid input: private clipboard' >&2; exit 7",
        ];
        for text in ["private clipboard\nsecond line", "", "ไทย 日本語"] {
            assert_eq!(
                super::super::run_capture("sh", args, Some(text)).unwrap_err(),
                "sh: exited unsuccessfully (exit status: 7)"
            );
        }
        for echo in [
            serde_json::to_string("private clipboard\nsecond line").unwrap(),
            "ไทย\\n日本語".into(),
        ] {
            let args = &[
                "-c",
                "printf '%s' \"$1\" >&2; exit 7",
                "fixture",
                echo.as_str(),
            ];
            assert_eq!(
                super::super::run_capture("sh", args, Some("private clipboard\nsecond line"))
                    .unwrap_err(),
                "sh: exited unsuccessfully (exit status: 7)"
            );
        }
        assert_eq!(
            super::super::run_capture("sh", args, None).unwrap_err(),
            "sh failed: invalid input: private clipboard"
        );
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

#[cfg(all(test, unix, not(target_os = "macos")))]
#[test]
fn stalled_clipboard_helpers_time_out_and_fallback_continues() {
    use std::time::{Duration, Instant};
    let start = Instant::now();
    let large_input = "clipboard fixture\n".repeat(10_000);
    let output = fallback(WRITE, |cmd, _| {
        if cmd == WRITE[0].0 {
            let error = super::run_capture_until(
                "sh",
                &["-c", "exec sleep 60"],
                Some(&large_input),
                true,
                Duration::from_millis(100),
            )
            .unwrap_err();
            assert_eq!(error, "sh: timed out");
            Err(error)
        } else {
            super::run_capture_impl("sh", &["-c", "printf 'fallback'"], None, true)
        }
    })
    .unwrap();
    assert_eq!(output, "fallback");
    assert!(start.elapsed() < Duration::from_secs(3));
}

#[cfg(all(test, unix))]
#[test]
fn helper_timeout_applies_while_stdout_is_open() {
    let result = super::run_capture_until(
        "sh",
        &["-c", "printf 'partial'; exec sleep 60"],
        None,
        true,
        std::time::Duration::from_millis(100),
    );
    assert_eq!(result.unwrap_err(), "sh: timed out");
}

#[cfg(all(test, windows))]
#[test]
fn powershell_clipboard_write_reads_multiline_utf8_from_stdin() {
    let path =
        std::env::temp_dir().join(format!("formless-clipboard-{}.txt", uuid::Uuid::new_v4()));
    let quoted = path.to_string_lossy().replace('\'', "''");
    let script = format!(
        "function Set-Clipboard {{ [CmdletBinding()] param([string] $Value) \
         [IO.File]::WriteAllText('{quoted}', $Value, [Text.UTF8Encoding]::new($false)) }}; {}",
        super::POWERSHELL_CLIPBOARD_WRITE
    );
    let text = "first line\r\nไทย 日本語\nlast line";
    super::run_capture_until(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
        Some(text),
        true,
        std::time::Duration::from_secs(5),
    )
    .unwrap();
    let actual = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(path).unwrap();
    assert_eq!(actual, text);
}
