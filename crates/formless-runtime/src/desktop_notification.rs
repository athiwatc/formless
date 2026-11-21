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
        let status = run(
            std::process::Command::new(program).args(args),
            std::time::Duration::from_secs(5),
        )?;
        if !status.success() {
            return Err(format!("{program} exited with {status}"));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let powershell = crate::process_executable::native_windows_system_directory()?
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        let status = run(
            &mut windows_notification_command(&powershell, title, body),
            std::time::Duration::from_secs(5),
        )?;
        match status.code() {
            Some(0) => Ok(()),
            Some(20) => Err("Windows desktop notifications require an interactive login".into()),
            Some(21) => Err(
                "Windows desktop notifications require the BurntToast module in Windows PowerShell"
                    .into(),
            ),
            _ => Err(format!(
                "Windows desktop notification helper exited with {status}"
            )),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (title, body);
        Err("notifications unsupported on this platform".into())
    }
}

#[cfg(any(windows, test))]
const WINDOWS_NOTIFICATION_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
if (-not [Environment]::UserInteractive) { exit 20 }
if (-not (Get-Module -ListAvailable -Name BurntToast)) { exit 21 }
Import-Module BurntToast
$text = @($env:FORMLESS_NOTIFICATION_TITLE, $env:FORMLESS_NOTIFICATION_BODY |
    Where-Object { -not [string]::IsNullOrEmpty($_) })
if ($text.Count -eq 0) { $text = @('Formless') }
New-BurntToastNotification -Text $text | Out-Null
"#;

#[cfg(any(windows, test))]
fn windows_notification_command(
    powershell: &std::path::Path,
    title: &str,
    body: &str,
) -> std::process::Command {
    let mut command = std::process::Command::new(powershell);
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            WINDOWS_NOTIFICATION_SCRIPT,
        ])
        .env("FORMLESS_NOTIFICATION_TITLE", title)
        .env("FORMLESS_NOTIFICATION_BODY", body);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn notify_send_args(title: &str, body: &str) -> Vec<String> {
    let body = body
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    vec!["--".into(), title.into(), body]
}

#[cfg(any(unix, windows))]
fn run(
    command: &mut std::process::Command,
    timeout: std::time::Duration,
) -> Result<std::process::ExitStatus, String> {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let name = command.get_program().to_string_lossy().into_owned();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("{name}: {error}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        let error = match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
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

    #[test]
    fn windows_helper_passes_unicode_and_multiline_text_as_environment_values() {
        let title = "-報告 $header";
        let body = "line one\r\nline two '<tag>'";
        let powershell =
            std::path::Path::new("C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe");
        let command = windows_notification_command(powershell, title, body);
        assert_eq!(command.get_program(), powershell.as_os_str());
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                WINDOWS_NOTIFICATION_SCRIPT
            ]
        );
        assert_eq!(
            command
                .get_envs()
                .map(|(key, value)| (key.to_str().unwrap(), value.unwrap().to_str().unwrap()))
                .collect::<std::collections::BTreeMap<_, _>>(),
            [
                ("FORMLESS_NOTIFICATION_TITLE", title),
                ("FORMLESS_NOTIFICATION_BODY", body)
            ]
            .into_iter()
            .collect()
        );
        assert!(WINDOWS_NOTIFICATION_SCRIPT.contains("New-BurntToastNotification -Text"));
        assert!(
            !WINDOWS_NOTIFICATION_SCRIPT.contains(title)
                && !WINDOWS_NOTIFICATION_SCRIPT.contains(body)
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
            std::process::Command::new(&notifier).args(notify_send_args("Title", "- markdown")),
            Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.join("args")).unwrap(),
            "--\nTitle\n- markdown\n"
        );

        fs::write(&notifier, "#!/bin/sh\nexec sleep 30\n").unwrap();
        let started = Instant::now();
        assert!(run(
            &mut std::process::Command::new(&notifier),
            Duration::from_millis(120)
        )
        .unwrap_err()
        .contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
        fs::remove_dir_all(root).unwrap();
    }
}
