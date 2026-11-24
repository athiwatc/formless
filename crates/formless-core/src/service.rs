//! Starting the daemon at login (SPEC §9.1): a launchd agent on macOS, a
//! systemd user unit on Linux, a logon task on Windows. The service runs
//! `formless daemon serve --stay` to prevent idle exit while the user manager
//! is running.

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
    crate::paths::validate_profile_name(profile)
        .map_err(|error| anyhow!("invalid service profile {profile:?}: {error}"))?;
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
            enable: vec![vec!["launchctl".into(), "load".into(), "-w".into(), f]],
            disable: vec![vec!["launchctl".into(), "remove".into(), label]],
        })
    } else if os == ServiceOs::Windows {
        // Batch percent expansion happens even inside quotes. Disable delayed
        // expansion too, so literal exclamation marks survive inherited /V:ON.
        let data = data.replace('%', "%%");
        let config = config.replace('%', "%%");
        let exe_s = exe_s.replace('%', "%%");
        let file = paths.data_dir.join(format!("{name}-service.cmd"));
        // The foreground daemon owns its append handles and opens them with
        // delete sharing. cmd.exe must not open or hold the live log.
        let contents = format!(
            "@echo off\r\nsetlocal EnableExtensions DisableDelayedExpansion\r\nset \"FORMLESS_DATA_DIR={data}\"\r\nset \"FORMLESS_CONFIG_DIR={config}\"\r\n\"{exe_s}\" daemon serve --stay --own-log\r\n"
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
    if let Some(guidance) = systemd_lingering_guidance(plan) {
        output.push_str("\n# ");
        output.push_str(guidance);
    }
    output
}

#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
enum Lingering {
    Enabled,
    Disabled,
    Unknown,
}

#[cfg(target_os = "linux")]
fn systemd_lingering(root: &Path, username: &std::ffi::OsStr) -> Lingering {
    let name = Path::new(username);
    let mut components = name.components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Lingering::Unknown;
    }
    match std::fs::symlink_metadata(root.join(name)) {
        Ok(metadata) if metadata.is_file() => Lingering::Enabled,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Lingering::Disabled,
        _ => Lingering::Unknown,
    }
}

#[cfg(target_os = "linux")]
fn login_name() -> Option<std::ffi::OsString> {
    use std::os::unix::ffi::OsStringExt as _;

    let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buffer = [0u8; 16 * 1024];
    let mut found = std::ptr::null_mut();
    // The reentrant call stores the username in this call's scratch buffer.
    unsafe {
        let status = libc::getpwuid_r(
            libc::getuid(),
            record.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut found,
        );
        if status != 0 || found.is_null() || (*found).pw_name.is_null() {
            return None;
        }
        let name = std::ffi::CStr::from_ptr((*found).pw_name).to_bytes();
        Some(std::ffi::OsString::from_vec(name.to_vec()))
    }
}

/// Report the Linux user's linger marker using only local reads.
pub fn systemd_lingering_guidance(plan: &Plan) -> Option<&'static str> {
    #[cfg(target_os = "linux")]
    if plan.manager == "systemd" {
        let status = login_name().map_or(Lingering::Unknown, |name| {
            systemd_lingering(Path::new("/var/lib/systemd/linger"), &name)
        });
        return Some(match status {
            Lingering::Enabled => "Systemd user lingering is enabled, allowing startup before login and operation after logout.",
            Lingering::Disabled => "Systemd user lingering is disabled. To keep scheduled jobs running after logout or before login, run `loginctl enable-linger \"$(id -u)\"` separately.",
            Lingering::Unknown => "Could not confirm systemd user lingering. To keep scheduled jobs running after logout or before login, check `loginctl show-user \"$(id -u)\" -p Linger` and enable lingering if needed.",
        });
    }
    let _ = plan;
    None
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
            && matches!(
                c.get(1).map(String::as_str),
                Some("load" | "unload" | "remove")
            )
            && (stderr.to_ascii_lowercase().contains("failed")
                || stderr.to_ascii_lowercase().contains("error"));
        if !success || launchctl_failure {
            return Err(anyhow!("`{}` failed: {}", c.join(" "), stderr));
        }
    }
    Ok(())
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
/// Every manager is addressed by stable registration identity even when its file is gone.
pub fn uninstall(plan: &Plan) -> Result<()> {
    uninstall_with(plan, &mut run_command)
}

fn uninstall_with(
    plan: &Plan,
    run: &mut impl FnMut(&[String]) -> Result<(bool, String)>,
) -> Result<()> {
    disable_with(plan, run)?;
    remove_definition(plan)
}

/// Stop registration without deleting its definition. Multi-profile uninstall
/// uses this preparation phase before freezing and deleting any profile files.
pub fn disable(plan: &Plan) -> Result<()> {
    disable_with(plan, &mut run_command)
}

fn disable_with(
    plan: &Plan,
    run: &mut impl FnMut(&[String]) -> Result<(bool, String)>,
) -> Result<()> {
    run_all_with(&plan.disable, run)
        .context("could not disable the registered service; any definition is kept")
}

/// The manager must already have been disabled successfully (or proven absent).
pub fn remove_definition(plan: &Plan) -> Result<()> {
    match std::fs::remove_file(&plan.file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Serialize automatic launchers during a deliberate stop, release, and load.
pub fn handover(
    paths: &Paths,
    timeout: std::time::Duration,
    stop: impl FnMut() -> Result<()>,
    start: impl FnOnce() -> Result<()>,
) -> Result<()> {
    handover_with(paths, timeout, stop, start, || {
        Ok((
            crate::daemon::endpoint_occupied(paths)?,
            crate::profile_lock::owner_held(paths)?,
        ))
    })
}

fn handover_with(
    paths: &Paths,
    timeout: std::time::Duration,
    mut stop: impl FnMut() -> Result<()>,
    start: impl FnOnce() -> Result<()>,
    mut state: impl FnMut() -> Result<(bool, bool)>,
) -> Result<()> {
    let _gate = crate::profile_lock::ProfileLock::launcher(paths, timeout)?;
    let deadline = std::time::Instant::now() + timeout;
    let mut stopped = false;
    loop {
        let (reachable, owned) = state()?;
        if !reachable && !owned {
            break;
        }
        if reachable && !stopped {
            stop()?;
            stopped = true;
            continue;
        }
        if std::time::Instant::now() >= deadline {
            return Err(anyhow!(
                "existing daemon did not release ownership; service was not loaded"
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    start()?;
    loop {
        let (reachable, owned) = state()?;
        if reachable || owned {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(anyhow!(
                "service loaded but did not claim the profile; check the service manager"
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

#[derive(Debug, PartialEq)]
pub enum Registration {
    Registered,
    NotRegistered,
    Unknown(String),
}

pub struct ServiceStatus {
    pub manager: &'static str,
    pub file: PathBuf,
    pub file_present: bool,
    pub registration: Registration,
}

fn registration_with(
    plan: &Plan,
    mut query: impl FnMut(&[String]) -> Result<(bool, String)>,
) -> Registration {
    let command = match plan.manager {
        "launchd" => vec![
            "launchctl".into(),
            "list".into(),
            plan.file
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ],
        "systemd" => vec![
            "systemctl".into(),
            "--user".into(),
            "show".into(),
            plan.file
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            "--property=LoadState".into(),
            "--property=ActiveState".into(),
        ],
        "Task Scheduler" => {
            let name = plan
                .disable
                .iter()
                .flat_map(|c| c.windows(2))
                .find(|p| p[0] == "/TN")
                .map(|p| p[1].clone());
            let Some(name) = name else {
                return Registration::Unknown("no task name".into());
            };
            vec!["schtasks".into(), "/Query".into(), "/TN".into(), name]
        }
        _ => return Registration::Unknown("unsupported manager".into()),
    };
    match query(&command) {
        Ok((true, text)) if text.lines().any(|l| l == "LoadState=not-found") => {
            Registration::NotRegistered
        }
        Ok((true, _)) => Registration::Registered,
        Ok((false, text)) => Registration::Unknown(text.trim().into()),
        Err(e) => Registration::Unknown(format!("{e:#}")),
    }
}

/// Query registration independently of whether the definition still exists.
pub fn status(paths: &Paths) -> Result<ServiceStatus> {
    let plan = plan_here(paths)?;
    let registration = registration_with(&plan, |c| {
        let out = std::process::Command::new(&c[0]).args(&c[1..]).output()?;
        let text = if out.status.success() {
            &out.stdout
        } else {
            &out.stderr
        };
        Ok((
            out.status.success(),
            String::from_utf8_lossy(text).into_owned(),
        ))
    });
    Ok(ServiceStatus {
        manager: plan.manager,
        file_present: plan.file.exists(),
        file: plan.file,
        registration,
    })
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

    #[cfg(target_os = "linux")]
    #[test]
    fn systemd_linger_marker_distinguishes_missing_and_enabled_users() {
        let root = std::env::temp_dir().join(format!("formless-linger-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let user = std::ffi::OsStr::new("fixture-user");
        assert_eq!(
            super::systemd_lingering(&root, user),
            super::Lingering::Disabled
        );
        std::fs::write(root.join(user), b"").unwrap();
        assert_eq!(
            super::systemd_lingering(&root, user),
            super::Lingering::Enabled
        );
        assert_eq!(
            super::systemd_lingering(&root, std::ffi::OsStr::new("../fixture-user")),
            super::Lingering::Unknown
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    use super::*;

    fn fixture(data: &str, config: &str, profile: &str) -> Paths {
        Paths {
            data_dir: data.into(),
            config_dir: config.into(),
            profile: profile.into(),
        }
    }

    #[test]
    fn successful_preparation_keeps_definition_until_destructive_phase() {
        let root = std::env::temp_dir().join(format!("fl-disable-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("service");
        std::fs::write(&file, "definition sentinel").unwrap();
        let plan = Plan {
            manager: "fixture",
            file: file.clone(),
            contents: String::new(),
            enable: Vec::new(),
            disable: vec![vec!["private-fake-manager".into(), "disable".into()]],
        };
        let mut calls = 0;
        disable_with(&plan, &mut |_| {
            calls += 1;
            Ok((true, String::new()))
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "definition sentinel"
        );
        remove_definition(&plan).unwrap();
        assert!(!file.exists());
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn missing_definition_still_disables_registration_and_queries_remain_truthful() {
        let base = std::env::temp_dir().join(format!("fl-svc-{}", uuid::Uuid::new_v4()));
        let paths = fixture(
            base.join("d").to_str().unwrap(),
            base.join("c").to_str().unwrap(),
            "default",
        );
        for os in [ServiceOs::MacOs, ServiceOs::Systemd, ServiceOs::Windows] {
            // This matrix checks registration/removal, not cross-platform
            // executable syntax. A Windows temp path contains backslashes that
            // systemd must reject; give each target a representable executable.
            let exe = if os == ServiceOs::Windows {
                base.join("formless.exe")
            } else {
                PathBuf::from("/opt/formless")
            };
            let p = plan_for(&paths, &exe, &base, os).unwrap();
            assert!(!p.file.exists());
            let mut called = Vec::new();
            uninstall_with(&p, &mut |args| {
                called.push(args.to_vec());
                Ok((true, String::new()))
            })
            .unwrap();
            assert_eq!(called, p.disable);
            assert_eq!(
                registration_with(&p, |_| Ok((true, String::new()))),
                Registration::Registered
            );
            assert!(matches!(
                registration_with(&p, |_| Ok((false, "manager unavailable".into()))),
                Registration::Unknown(_)
            ));
            assert!(matches!(
                registration_with(&p, |_| Err(anyhow!("permission denied"))),
                Registration::Unknown(_)
            ));
            if os == ServiceOs::MacOs {
                assert_eq!(p.disable[0][1], "remove");
                assert!(!p.disable[0].contains(&p.file.display().to_string()));
            }
            if os == ServiceOs::Systemd {
                assert_eq!(
                    registration_with(&p, |_| Ok((
                        true,
                        "LoadState=not-found\nActiveState=inactive\n".into()
                    ))),
                    Registration::NotRegistered
                );
            }
        }
        assert!(
            !base.exists(),
            "queries and missing-file removal create no profile state"
        );
    }

    #[test]
    fn handover_serializes_stop_release_and_service_start_and_fails_closed() {
        use std::cell::{Cell, RefCell};
        let base = std::env::temp_dir().join(format!("fl-handover-{}", uuid::Uuid::new_v4()));
        let paths = Paths {
            data_dir: base.join("d"),
            config_dir: base.join("c"),
            profile: "default".into(),
        };
        let state = Cell::new((true, true));
        let calls = RefCell::new(Vec::new());
        handover_with(
            &paths,
            std::time::Duration::from_secs(1),
            || {
                calls.borrow_mut().push("stop");
                state.set((false, false));
                Ok(())
            },
            || {
                assert!(
                    crate::profile_lock::ProfileLock::launcher(&paths, std::time::Duration::ZERO)
                        .is_err(),
                    "automatic launchers remain serialized"
                );
                assert_eq!(state.get(), (false, false));
                calls.borrow_mut().push("load");
                state.set((false, true));
                Ok(())
            },
            || Ok(state.get()),
        )
        .unwrap();
        assert_eq!(*calls.borrow(), ["stop", "load"]);
        let started = Cell::new(false);
        assert!(handover_with(
            &paths,
            std::time::Duration::ZERO,
            || Err(anyhow!("shutdown rejected")),
            || {
                started.set(true);
                Ok(())
            },
            || Ok((true, true))
        )
        .is_err());
        assert!(!started.get());
        assert!(handover_with(
            &paths,
            std::time::Duration::ZERO,
            || Ok(()),
            || {
                started.set(true);
                Ok(())
            },
            || Err(anyhow!("unknown owner"))
        )
        .is_err());
        assert!(!started.get());
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
        // Paths::log_file derives its final component using the host's path
        // separator. The Unix formatter must preserve that exact supplied path
        // literally, including a Windows-host backslash, while doubling %.
        // Keep independent literal escaping oracles; don't call the formatter's
        // escape helpers to construct an expected answer.
        assert!(linux.contents.contains(&format!(
            "StandardOutput=append:/home/a space \"quotes\" %%n & <node> $TOKEN/data{}daemon.log\n",
            std::path::MAIN_SEPARATOR
        )));
        assert!(linux.contents.contains(&format!(
            "StandardError=append:/home/a space \"quotes\" %%n & <node> $TOKEN/data{}daemon.log\n",
            std::path::MAIN_SEPARATOR
        )));
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
        assert!(p.contents.contains(
            r#""C:\Program Files\A & %%NAME%%!\formless.exe" daemon serve --stay --own-log"#
        ));
        assert!(!p.contents.contains(">>"));
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
        // Ancestor names deliberately contain the unit prefix: only the exact
        // service basename distinguishes the default and named registrations.
        let base = std::env::temp_dir().join(format!("formless-parent-{}", uuid::Uuid::new_v4()));
        let home = base.join("formless-home-parent");
        let exe = if cfg!(windows) {
            base.join("bin/formless.exe")
        } else {
            PathBuf::from("/usr/local/bin/formless")
        };
        let mut paths = Paths::from_dirs(base.join("data/work"), base.join("conf/work")).unwrap();
        paths.profile = "work".into();
        let p = plan(&paths, &exe, &home).unwrap();
        assert!(p.contents.contains("serve"), "{}", p.contents);
        assert!(p.contents.contains("--stay"));
        assert!(p.contents.contains(&paths.data_dir.display().to_string()));
        assert!(p.contents.contains(&paths.config_dir.display().to_string()));
        let (named_basename, default_basename, parent) = match p.manager {
            "launchd" => (
                "xyz.athiwat.formless-work.plist",
                "xyz.athiwat.formless.plist",
                home.join("Library/LaunchAgents"),
            ),
            "systemd" => (
                "formless-work.service",
                "formless.service",
                home.join(".config/systemd/user"),
            ),
            "Task Scheduler" => (
                "formless-work-service.cmd",
                "formless-service.cmd",
                paths.data_dir.clone(),
            ),
            manager => panic!("unexpected service manager {manager}"),
        };
        assert_eq!(p.file, parent.join(named_basename));
        assert!(!p.file.exists(), "planning writes no service definition");
        assert!(!p.enable.is_empty() && !p.disable.is_empty());
        let default = Paths::from_dirs(base.join("data"), base.join("conf")).unwrap();
        let d = plan(&default, &exe, &home).unwrap();
        assert_eq!(d.manager, p.manager);
        assert_eq!(
            d.file.file_name().unwrap().to_str().unwrap(),
            default_basename
        );
        let default_parent = if d.manager == "Task Scheduler" {
            &default.data_dir
        } else {
            &parent
        };
        assert_eq!(d.file, default_parent.join(default_basename));
        assert!(!d.file.exists(), "planning writes no default definition");
        let _ = std::fs::remove_dir_all(&base);
    }
}
