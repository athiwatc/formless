//! Starting the daemon at login (SPEC §9.1): a launchd agent on macOS, a
//! systemd user unit on Linux, a logon task on Windows. The service runs
//! `formless daemon serve --stay`, which never stops for being idle, so
//! scheduled jobs fire on time.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

use crate::Paths;

/// What installing writes and runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// "launchd", "systemd" or "Task Scheduler".
    pub manager: &'static str,
    pub file: PathBuf,
    pub contents: String,
    /// Commands that load (install) or unload (uninstall) it.
    pub enable: Vec<Vec<String>>,
    pub disable: Vec<Vec<String>>,
}

/// The per-profile name: `formless`, or `formless-<profile>`.
fn unit_name(paths: &Paths) -> Result<String> {
    let profile = &paths.profile;
    if profile.is_empty()
        || profile.len() > 64
        || !profile.as_bytes()[0].is_ascii_alphanumeric()
        || !profile
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(anyhow!("invalid service profile {profile:?}: use 1–64 ASCII letters, digits, '-' or '_', starting with a letter or digit"));
    }
    Ok(if profile == "default" {
        "formless".into()
    } else {
        format!("formless-{profile}")
    })
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[derive(Clone, Copy, PartialEq)]
enum ServiceOs {
    MacOs,
    Windows,
    Systemd,
}

/// The service for this OS, the binary `exe` and the user's `home`.
pub fn plan(paths: &Paths, exe: &Path, home: &Path) -> Result<Plan> {
    let os = if cfg!(target_os = "macos") {
        ServiceOs::MacOs
    } else if cfg!(windows) {
        ServiceOs::Windows
    } else {
        ServiceOs::Systemd
    };
    plan_for(paths, exe, home, os)
}

fn service_path(path: &Path, os: ServiceOs) -> Result<String> {
    let value = path.to_str().context("service paths must be valid UTF-8")?;
    if value.chars().any(char::is_control) {
        return Err(anyhow!(
            "service path contains a control character: {value:?}"
        ));
    }
    if os == ServiceOs::Windows
        && value
            .chars()
            .any(|c| matches!(c, '"' | '<' | '>' | '|' | '?' | '*'))
    {
        return Err(anyhow!(
            "service path contains a character Windows filenames cannot represent: {value:?}"
        ));
    }
    Ok(value.to_string())
}

fn systemd_quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    )
}

fn plan_for(paths: &Paths, exe: &Path, home: &Path, os: ServiceOs) -> Result<Plan> {
    let name = unit_name(paths)?;
    let data = service_path(&paths.data_dir, os)?;
    let config = service_path(&paths.config_dir, os)?;
    let log = service_path(&paths.log_file(), os)?;
    let exe_s = service_path(exe, os)?;
    service_path(home, os)?;
    // systemd rejects these in the executable name after unquoting it, even
    // when they are escaped correctly. Environment/output paths permit them.
    if os == ServiceOs::Systemd && exe_s.contains(['\'', '"', '\\']) {
        return Err(anyhow!(
            "systemd cannot represent an executable path containing quotes or backslashes: {exe_s:?}"
        ));
    }
    if os == ServiceOs::MacOs {
        let label = format!("xyz.athiwat.{name}");
        let file = home
            .join("Library/LaunchAgents")
            .join(format!("{label}.plist"));
        let contents = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>daemon</string>
    <string>serve</string>
    <string>--stay</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>FORMLESS_DATA_DIR</key><string>{data}</string>
    <key>FORMLESS_CONFIG_DIR</key><string>{config}</string>
    <key>{log_env}</key><string>1</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
            label = xml_escape(&label),
            exe = xml_escape(&exe_s),
            data = xml_escape(&data),
            config = xml_escape(&config),
            log = xml_escape(&log),
            log_env = crate::logs::DAEMON_LOG_ENV,
        );
        let f = file.display().to_string();
        Ok(Plan {
            manager: "launchd",
            file,
            contents,
            enable: vec![vec![
                "launchctl".into(),
                "load".into(),
                "-w".into(),
                f.clone(),
            ]],
            disable: vec![vec!["launchctl".into(), "unload".into(), "-w".into(), f]],
        })
    } else if os == ServiceOs::Windows {
        // Batch percent expansion happens even inside quotes. Disable delayed
        // expansion too, so literal exclamation marks survive inherited /V:ON.
        let data = data.replace('%', "%%");
        let config = config.replace('%', "%%");
        let log = log.replace('%', "%%");
        let exe_s = exe_s.replace('%', "%%");
        let file = paths.data_dir.join(format!("{name}-service.cmd"));
        let contents = format!(
            "@echo off\r\nsetlocal EnableExtensions DisableDelayedExpansion\r\nset \"FORMLESS_DATA_DIR={data}\"\r\nset \"FORMLESS_CONFIG_DIR={config}\"\r\nset \"{}=1\"\r\n\"{exe_s}\" daemon serve --stay >> \"{log}\" 2>&1\r\n",
            crate::logs::DAEMON_LOG_ENV
        );
        let task = format!("Formless ({name})");
        Ok(Plan {
            manager: "Task Scheduler",
            file: file.clone(),
            contents,
            enable: vec![
                vec![
                    "schtasks".into(),
                    "/Create".into(),
                    "/F".into(),
                    "/SC".into(),
                    "ONLOGON".into(),
                    "/TN".into(),
                    task.clone(),
                    "/TR".into(),
                    format!("\"{}\"", file.display()),
                ],
                vec!["schtasks".into(), "/Run".into(), "/TN".into(), task.clone()],
            ],
            disable: vec![vec![
                "schtasks".into(),
                "/Delete".into(),
                "/F".into(),
                "/TN".into(),
                task,
            ]],
        })
    } else {
        let unit = format!("{name}.service");
        let file = home.join(".config/systemd/user").join(&unit);
        // StandardOutput's append: payload is a raw path, not an argv
        // word: quotes/backslashes/spaces remain literal. Only specifiers
        // are expanded (systemd load-fragment.c config_parse_exec_output).
        let output = log.replace('%', "%%");
        // Quotes do not suppress systemd's $NAME/${NAME} substitution.
        // The ':' executable prefix disables it for the whole command.
        let contents = format!(
            "[Unit]\nDescription=Formless daemon\n\n[Service]\nExecStart=:{} daemon serve --stay\nEnvironment={}\nEnvironment={}\nEnvironment={}=1\nStandardOutput=append:{output}\nStandardError=append:{output}\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n",
            systemd_quote(&exe_s),
            systemd_quote(&format!("FORMLESS_DATA_DIR={data}")),
            systemd_quote(&format!("FORMLESS_CONFIG_DIR={config}")),
            crate::logs::DAEMON_LOG_ENV,
        );
        let sc = |args: &[&str]| -> Vec<String> {
            std::iter::once("systemctl")
                .chain(std::iter::once("--user"))
                .chain(args.iter().copied())
                .map(str::to_string)
                .collect()
        };
        Ok(Plan {
            manager: "systemd",
            file,
            contents,
            enable: vec![sc(&["daemon-reload"]), sc(&["enable", "--now", &unit])],
            disable: vec![sc(&["disable", "--now", &unit])],
        })
    }
}

/// The exact definition and planned commands shown by install-service --dry-run.
pub fn dry_run(plan: &Plan) -> String {
    let mut output = format!(
        "# {} ({})\n{}",
        plan.file.display(),
        plan.manager,
        plan.contents
    );
    if plan.manager == "launchd" && plan.file.exists() {
        for command in &plan.disable {
            output.push_str(&format!("\n$ {}", command.join(" ")));
        }
    }
    for command in &plan.enable {
        output.push_str(&format!("\n$ {}", command.join(" ")));
    }
    output
}

fn run_command(c: &[String]) -> Result<(bool, String)> {
    let (prog, args) = c.split_first().context("empty command")?;
    let out = std::process::Command::new(prog)
        .args(args)
        .output()
        .with_context(|| format!("run {prog}"))?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    ))
}

fn run_all_with(
    cmds: &[Vec<String>],
    run: &mut impl FnMut(&[String]) -> Result<(bool, String)>,
) -> Result<()> {
    for c in cmds {
        let prog = c.first().context("empty command")?;
        let (success, stderr) = run(c)?;
        // Legacy launchctl load/unload can report failure while exiting 0.
        // Preserve the manager's explanation instead of claiming success.
        let launchctl_failure = Path::new(prog)
            .file_name()
            .is_some_and(|n| n == "launchctl")
            && matches!(c.get(1).map(String::as_str), Some("load" | "unload"))
            && (stderr.to_ascii_lowercase().contains("failed")
                || stderr.to_ascii_lowercase().contains("error"));
        if !success || launchctl_failure {
            return Err(anyhow!("`{}` failed: {}", c.join(" "), stderr));
        }
    }
    Ok(())
}

fn run_all(cmds: &[Vec<String>]) -> Result<()> {
    run_all_with(cmds, &mut run_command)
}

/// Write the service file and load it. Reinstalling a launchd agent unloads
/// its old definition first, while its original plist is still in place.
pub fn install(plan: &Plan) -> Result<()> {
    install_with(plan, &mut run_command)
}

fn install_with(
    plan: &Plan,
    run: &mut impl FnMut(&[String]) -> Result<(bool, String)>,
) -> Result<()> {
    if plan.manager == "launchd" && plan.file.exists() {
        run_all_with(&plan.disable, run)
            .context("could not unload the previous launchd definition; its file is kept")?;
    }
    if let Some(dir) = plan.file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&plan.file, &plan.contents)
        .with_context(|| format!("write {}", plan.file.display()))?;
    run_all_with(&plan.enable, run)
}

/// Unload the service, then remove its file. A failed unload keeps the
/// file, so the service manager never respawns a deleted binary (#1079).
/// A Windows task is deleted by name even when its script is gone.
pub fn uninstall(plan: &Plan) -> Result<()> {
    let present = plan.file.exists();
    if !present && plan.manager != "Task Scheduler" {
        return Err(anyhow!(
            "not installed ({} is missing)",
            plan.file.display()
        ));
    }
    if let Err(e) = run_all(&plan.disable) {
        if present {
            return Err(e.context("could not unload the service; its file is kept"));
        }
        return Err(anyhow!("not installed (no {} task)", plan.manager));
    }
    if present {
        std::fs::remove_file(&plan.file)?;
    }
    Ok(())
}

/// The plan for this binary and this user.
pub fn plan_here(paths: &Paths) -> Result<Plan> {
    let exe = std::env::current_exe().context("locate the formless binary")?;
    let home = directories::BaseDirs::new()
        .map(|b| b.home_dir().to_path_buf())
        .context("no home directory")?;
    plan(paths, &exe, &home)
}

/// Whether the service file is in place (for `doctor` and `daemon status`).
pub fn installed(paths: &Paths) -> Option<(&'static str, PathBuf)> {
    let p = plan_here(paths).ok()?;
    p.file.exists().then_some((p.manager, p.file))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(data: &str, config: &str, profile: &str) -> Paths {
        Paths {
            data_dir: data.into(),
            config_dir: config.into(),
            profile: profile.into(),
        }
    }

    #[test]
    fn invalid_profiles_and_control_characters_are_refused_before_rendering() {
        for os in [ServiceOs::MacOs, ServiceOs::Systemd, ServiceOs::Windows] {
            for profile in [
                "",
                "has space",
                "has\"quote",
                "%PATH%",
                "a&b",
                "</string><key>RunAtLoad</key>",
                "../escape",
                "-leading",
            ] {
                let paths = fixture("/data", "/config", profile);
                assert!(
                    plan_for(&paths, Path::new("/bin/formless"), Path::new("/home"), os)
                        .unwrap_err()
                        .to_string()
                        .contains("invalid service profile")
                );
            }
            let paths = fixture("/data\ninjected", "/config", "work");
            assert!(
                plan_for(&paths, Path::new("/bin/formless"), Path::new("/home"), os)
                    .unwrap_err()
                    .to_string()
                    .contains("control character")
            );
        }
    }

    #[test]
    fn unix_formats_preserve_hostile_path_characters_as_literal_data() {
        let paths = fixture(
            "/home/a space \"quotes\" %n & <node> $TOKEN/data",
            "/home/a space \"quotes\" %n & <node> $TOKEN/config",
            "work",
        );
        let exe = Path::new("/opt/a space \"quotes\" %n & <node> $TOKEN/${TOKEN}/formless");
        let mac = plan_for(&paths, exe, Path::new("/home"), ServiceOs::MacOs).unwrap();
        assert!(mac
            .contents
            .contains("<key>Label</key><string>xyz.athiwat.formless-work</string>"));
        assert!(mac
            .contents
            .contains("/home/a space &quot;quotes&quot; %n &amp; &lt;node&gt; $TOKEN/data"));
        assert!(mac.contents.contains(
            "/opt/a space &quot;quotes&quot; %n &amp; &lt;node&gt; $TOKEN/${TOKEN}/formless"
        ));
        let linux = plan_for(
            &paths,
            Path::new("/opt/a space %n & <node> $TOKEN/${TOKEN}/formless"),
            Path::new("/home"),
            ServiceOs::Systemd,
        )
        .unwrap();
        assert!(linux.contents.contains(
            r#"ExecStart=:"/opt/a space %%n & <node> $TOKEN/${TOKEN}/formless" daemon serve --stay"#
        ));
        assert!(linux.contents.contains(
            r#"Environment="FORMLESS_DATA_DIR=/home/a space \"quotes\" %%n & <node> $TOKEN/data""#
        ));
        assert!(linux.contents.contains(
            "StandardOutput=append:/home/a space \"quotes\" %%n & <node> $TOKEN/data/daemon.log\n"
        ));
        assert!(linux.contents.contains(
            "StandardError=append:/home/a space \"quotes\" %%n & <node> $TOKEN/data/daemon.log\n"
        ));
        let preview = dry_run(&linux);
        assert!(preview.contains(&linux.contents));
        assert!(preview.contains(
            r#"ExecStart=:"/opt/a space %%n & <node> $TOKEN/${TOKEN}/formless" daemon serve --stay"#
        ));
    }

    #[test]
    fn systemd_refuses_unrepresentable_executable_paths_before_writing() {
        let paths = fixture("/data \"quotes\"", "/config\\backslash", "work");
        for exe in [
            "/opt/a\"quote/formless",
            "/opt/a'quote/formless",
            "/opt/a\\slash/formless",
        ] {
            let error = plan_for(
                &paths,
                Path::new(exe),
                Path::new("/home"),
                ServiceOs::Systemd,
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("systemd cannot represent an executable path"),
                "{error}"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn hostile_paths_produce_a_plist_the_native_parser_accepts() {
        let base = std::env::temp_dir().join(format!("fl-svc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let paths = fixture("/a space \" & %PATH% <node>/data", "/config & <c>", "work");
        let p = plan_for(
            &paths,
            Path::new("/bin/a \" & <b>"),
            &base,
            ServiceOs::MacOs,
        )
        .unwrap();
        let file = base.join("fixture.plist");
        std::fs::write(&file, p.contents).unwrap();
        let parsed = std::process::Command::new("/usr/bin/plutil")
            .arg("-lint")
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            parsed.status.success(),
            "{}",
            String::from_utf8_lossy(&parsed.stderr)
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn windows_batch_values_disable_delayed_expansion_and_escape_every_percent() {
        let paths = fixture(
            r"C:\Users\A space & %TEMP%!\data",
            r"D:\A's %NAME% & !\config",
            "work",
        );
        let p = plan_for(
            &paths,
            Path::new(r"C:\Program Files\A & %NAME%!\formless.exe"),
            Path::new(r"C:\Users\A"),
            ServiceOs::Windows,
        )
        .unwrap();
        assert!(p
            .contents
            .starts_with("@echo off\r\nsetlocal EnableExtensions DisableDelayedExpansion\r\n"));
        assert!(p
            .contents
            .contains(r#"set "FORMLESS_DATA_DIR=C:\Users\A space & %%TEMP%%!\data""#));
        assert!(p.contents.contains(r#""C:\Program Files\A & %%NAME%%!\formless.exe" daemon serve --stay >> "C:\Users\A space & %%TEMP%%!\data/daemon.log" 2>&1"#)
            || p.contents.contains(r#""C:\Program Files\A & %%NAME%%!\formless.exe" daemon serve --stay >> "C:\Users\A space & %%TEMP%%!\data\daemon.log" 2>&1"#));
        let preview = dry_run(&p);
        assert!(preview.contains(&p.contents));
        assert!(preview.contains("$ schtasks /Create"));
        assert!(preview.contains("%%TEMP%%"));
        let mut chars = p.contents.chars();
        while let Some(c) = chars.next() {
            if c == '%' {
                assert_eq!(chars.next(), Some('%'), "unescaped batch expansion");
            }
        }
        for invalid in [r#"C:\a"b"#, r"C:\<tag>"] {
            assert!(plan_for(
                &paths,
                Path::new(invalid),
                Path::new(r"C:\Users\A"),
                ServiceOs::Windows
            )
            .unwrap_err()
            .to_string()
            .contains("Windows filenames cannot represent"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn native_cmd_preserves_literal_percent_and_exclamation_in_environment_values() {
        let base = std::env::temp_dir().join(format!("fl-svc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let paths = fixture(
            r"C:\a space & %TEMP%!\data",
            r"C:\a space & %PATH%!\config",
            "work",
        );
        let p = plan_for(
            &paths,
            Path::new(r"C:\formless.exe"),
            &base,
            ServiceOs::Windows,
        )
        .unwrap();
        // Exercise the generated environment assignments using the native
        // interpreter, without starting a daemon or registering any task.
        let mut script = p
            .contents
            .lines()
            .filter(|line| {
                line.starts_with('@') || line.starts_with("setlocal ") || line.starts_with("set ")
            })
            .collect::<Vec<_>>()
            .join("\r\n");
        script.push_str("\r\nset FORMLESS_DATA_DIR\r\nset FORMLESS_CONFIG_DIR\r\n");
        let file = base.join("fixture.cmd");
        std::fs::write(&file, script).unwrap();
        let out = std::process::Command::new("cmd.exe")
            .args(["/D", "/V:ON", "/C"])
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            text.contains(r"FORMLESS_DATA_DIR=C:\a space & %TEMP%!\data"),
            "{text}"
        );
        assert!(
            text.contains(r"FORMLESS_CONFIG_DIR=C:\a space & %PATH%!\config"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn hostile_fixture_dry_run_shows_the_escaped_definition_without_writing_it() {
        let base = std::env::temp_dir().join(format!("fl-svc-preview-{}", uuid::Uuid::new_v4()));
        let paths = fixture("/a space \" & %PATH% <node>/data", "/config & <c>", "work");
        for os in [ServiceOs::MacOs, ServiceOs::Systemd] {
            let exe = if os == ServiceOs::Systemd {
                "/bin/a $TOKEN %n & <b>"
            } else {
                "/bin/a \" & <b>"
            };
            let p = plan_for(&paths, Path::new(exe), &base, os).unwrap();
            let shown = dry_run(&p);
            assert!(shown.starts_with(&format!("# {} ({})\n", p.file.display(), p.manager)));
            assert!(shown.contains(&p.contents));
            assert!(shown.contains("$ "));
            assert!(!p.file.exists());
        }
        assert!(!base.exists(), "dry run creates no service folders");
    }

    #[test]
    fn reinstall_replaces_the_loaded_launchd_binary_path() {
        let base = std::env::temp_dir().join(format!("fl-svc-{}", uuid::Uuid::new_v4()));
        let mut p = Plan {
            manager: "launchd",
            file: base.join("agent.plist"),
            contents: "/old/formless".into(),
            enable: vec![vec!["launchctl".into(), "load".into()]],
            disable: vec![vec!["launchctl".into(), "unload".into()]],
        };
        let mut loaded = None;
        let mut observed = Vec::new();
        let file = p.file.clone();
        let mut fake_launchctl = |c: &[String]| -> Result<(bool, String)> {
            observed.push((c[1].clone(), std::fs::read_to_string(&file)?));
            match c[1].as_str() {
                "load" => loaded = Some(std::fs::read_to_string(&file)?),
                "unload" => loaded = None,
                _ => unreachable!(),
            }
            Ok((true, String::new()))
        };
        install_with(&p, &mut fake_launchctl).unwrap();
        p.contents = "/new/formless".into();
        install_with(&p, &mut fake_launchctl).unwrap();
        drop(fake_launchctl);
        assert_eq!(loaded.as_deref(), Some("/new/formless"));
        assert_eq!(
            observed,
            [
                ("load".into(), "/old/formless".into()),
                ("unload".into(), "/old/formless".into()),
                ("load".into(), "/new/formless".into()),
            ]
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn zero_exit_launchctl_failures_are_errors_and_keep_the_previous_file() {
        let base = std::env::temp_dir().join(format!("fl-svc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).unwrap();
        let file = base.join("agent.plist");
        std::fs::write(&file, "previous definition").unwrap();
        let p = Plan {
            manager: "launchd",
            file: file.clone(),
            contents: "replacement".into(),
            enable: vec![vec!["launchctl".into(), "load".into()]],
            disable: vec![vec!["launchctl".into(), "unload".into()]],
        };
        for verb in ["load", "unload"] {
            let stderr = format!("{verb} failed: 5: Input/output error");
            let error = run_all_with(&[vec!["launchctl".into(), verb.into()]], &mut |_| {
                Ok((true, stderr.clone()))
            })
            .unwrap_err();
            assert!(error.to_string().contains(&stderr));
        }
        let error = install_with(&p, &mut |_| {
            Ok((true, "Unload failed: 5: Input/output error".into()))
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("Unload failed: 5: Input/output error"));
        assert_eq!(
            std::fs::read_to_string(file).unwrap(),
            "previous definition"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn the_service_runs_serve_stay_with_this_profiles_dirs() {
        let base = std::env::temp_dir().join(format!("fl-svc-{}", uuid::Uuid::new_v4()));
        let mut paths = Paths::from_dirs(base.join("data/work"), base.join("conf/work")).unwrap();
        paths.profile = "work".into();
        let p = plan(
            &paths,
            Path::new("/usr/local/bin/formless"),
            Path::new("/home/u"),
        )
        .unwrap();
        assert!(p.contents.contains("serve"), "{}", p.contents);
        assert!(p.contents.contains("--stay"));
        assert!(p.contents.contains(&paths.data_dir.display().to_string()));
        assert!(p.contents.contains(&paths.config_dir.display().to_string()));
        assert!(
            p.file.to_string_lossy().contains("formless-work"),
            "per-profile name: {}",
            p.file.display()
        );
        assert!(!p.enable.is_empty() && !p.disable.is_empty());
        let default = Paths::from_dirs(base.join("data"), base.join("conf")).unwrap();
        let d = plan(&default, Path::new("/x/formless"), Path::new("/home/u")).unwrap();
        assert!(!d.file.to_string_lossy().contains("formless-"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
