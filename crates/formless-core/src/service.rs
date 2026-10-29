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

fn run_all(cmds: &[Vec<String>]) -> Result<()> {
    for c in cmds {
        let (prog, args) = c.split_first().context("empty command")?;
        let out = std::process::Command::new(prog)
            .args(args)
            .output()
            .with_context(|| format!("run {prog}"))?;
        if !out.status.success() {
            return Err(anyhow!(
                "`{}` failed: {}",
                c.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    Ok(())
}

/// Write the service file and load it. A daemon already running keeps
/// going; the service's daemon takes over once it stops.
pub fn install(plan: &Plan) -> Result<()> {
    if let Some(dir) = plan.file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&plan.file, &plan.contents)
        .with_context(|| format!("write {}", plan.file.display()))?;
    run_all(&plan.enable)
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
