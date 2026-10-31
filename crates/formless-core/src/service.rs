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
fn unit_name(paths: &Paths) -> String {
    if paths.profile == "default" || paths.profile.is_empty() {
        "formless".into()
    } else {
        format!("formless-{}", paths.profile)
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The service for this OS, the binary `exe` and the user's `home`.
pub fn plan(paths: &Paths, exe: &Path, home: &Path) -> Result<Plan> {
    let name = unit_name(paths);
    let data = paths.data_dir.display().to_string();
    let config = paths.config_dir.display().to_string();
    let log = paths.log_file().display().to_string();
    let exe_s = exe.display().to_string();
    if cfg!(target_os = "macos") {
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
    } else if cfg!(windows) {
        let file = paths.data_dir.join(format!("{name}-service.cmd"));
        let contents = format!(
            "@echo off\r\nset \"FORMLESS_DATA_DIR={data}\"\r\nset \"FORMLESS_CONFIG_DIR={config}\"\r\nset \"{}=1\"\r\n\"{exe_s}\" daemon serve --stay >> \"{log}\" 2>&1\r\n",
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
        let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
        let contents = format!(
            "[Unit]\nDescription=Formless daemon\n\n[Service]\nExecStart={} daemon serve --stay\nEnvironment={}\nEnvironment={}\nEnvironment={}=1\nStandardOutput=append:{log}\nStandardError=append:{log}\nRestart=on-failure\n\n[Install]\nWantedBy=default.target\n",
            q(&exe_s),
            q(&format!("FORMLESS_DATA_DIR={data}")),
            q(&format!("FORMLESS_CONFIG_DIR={config}")),
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
