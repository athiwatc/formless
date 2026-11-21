/// AppleScript source for a notification. Notification text stays on one
/// source line and cannot end its quoted literal.
#[cfg(any(target_os = "macos", test))]
pub fn macos_notification_script(title: &str, body: &str) -> String {
    let escape = |text: &str| {
        let single_line: String = text
            .replace("\r\n", "\n")
            .chars()
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .collect();
        single_line.replace('\\', "\\\\").replace('"', "\\\"")
    };
    format!(
        "display notification \"{}\" with title \"{}\"",
        escape(body),
        escape(title)
    )
}

/// Send a desktop notification from a blocking thread. The notifier gets
/// five seconds to submit it to the desktop, then the child is reaped.
pub fn desktop_notify(title: &str, body: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        #[cfg(target_os = "macos")]
        let (program, args) = (
            "osascript",
            vec!["-e".into(), macos_notification_script(title, body)],
        );
        #[cfg(not(target_os = "macos"))]
        let (program, args) = ("notify-send", notify_send_args(title, body));
        run(
            std::path::Path::new(program),
            &args,
            std::time::Duration::from_secs(5),
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (title, body);
        Err("notifications unsupported on this platform".into())
    }
}

#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn notify_send_args(title: &str, body: &str) -> Vec<String> {
    let body = body
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    vec!["--".into(), title.into(), body]
}

#[cfg(unix)]
fn run(
    program: &std::path::Path,
    args: &[String],
    timeout: std::time::Duration,
) -> Result<(), String> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let name = program.display();
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("{name}: {error}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        let error = match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("{name} exited with {status}")),
            Ok(None) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if !remaining.is_zero() {
                    std::thread::sleep(remaining.min(Duration::from_millis(25)));
                    continue;
                }
                format!("{name} timed out")
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => format!("{name}: {error}"),
        };
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_send_separates_options_and_escapes_body_markup() {
        assert_eq!(
            notify_send_args("-title", "- bullet with a < b & c > d"),
            ["--", "-title", "- bullet with a &lt; b &amp; c &gt; d"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn notifier_gets_literal_arguments_and_a_stuck_child_times_out() {
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            time::{Duration, Instant},
        };
        let root = std::env::temp_dir().join(format!("formless-notify-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let notifier = root.join("fixture-notifier");
        fs::write(
            &notifier,
            r#"#!/bin/sh
printf '%s\n' "$@" > "$(dirname -- "$0")/args"
"#,
        )
        .unwrap();
        fs::set_permissions(&notifier, fs::Permissions::from_mode(0o700)).unwrap();
        run(
            &notifier,
            &notify_send_args("Title", "- markdown"),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("args")).unwrap(),
            "--\nTitle\n- markdown\n"
        );

        fs::write(&notifier, "#!/bin/sh\nexec sleep 30\n").unwrap();
        let started = Instant::now();
        assert!(run(&notifier, &[], Duration::from_millis(120))
            .unwrap_err()
            .contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
        fs::remove_dir_all(root).unwrap();
    }
}
