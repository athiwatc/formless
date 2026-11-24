//! Detached Windows daemons rotate a live log and report long path errors.
#![cfg(windows)]

use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Profile {
    root: PathBuf,
    data: PathBuf,
    config: PathBuf,
    started: bool,
}

impl Profile {
    fn new(long: bool, keep: usize) -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("fl-win-log-{}-{stamp}", std::process::id()));
        let config = root.join("config");
        let mut data = root.join("data");
        if long {
            while data.as_os_str().encode_wide().count() < 280 {
                data.push("nested-data-directory-0123456789");
            }
        }
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join("config.toml"),
            format!("[daemon]\nlog_max_bytes = 4096\nlog_keep = {keep}\nidle_grace_secs = 600\n[provider]\nbase_url = \"http://127.0.0.1:9\"\n"),
        ).unwrap();
        Self {
            root,
            data,
            config,
            started: false,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_formless"));
        command
            .args(args)
            .env("FORMLESS_DATA_DIR", &self.data)
            .env("FORMLESS_CONFIG_DIR", &self.config)
            .env_remove("FORMLESS_API_KEY")
            .env_remove("FORMLESS_BASE_URL")
            .env_remove("FORMLESS_MODEL")
            .env("RUST_LOG", "info");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn start(&mut self) -> ExitStatus {
        // Avoid capture pipes that a detached child may inherit and hold open.
        let result = self
            .command(&["daemon", "start"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        self.started = result.success();
        result
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        if self.started {
            let _ = self.run(&["daemon", "stop"]);
            let until = Instant::now() + Duration::from_secs(8);
            while self.data.join("daemon.lock").exists() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn detached_daemon_reopens_its_log_after_rotation() {
    assert_detached_daemon_reopens_its_log(1);
}

#[test]
fn detached_daemon_reopens_its_log_without_retaining_history() {
    assert_detached_daemon_reopens_its_log(0);
}

fn assert_detached_daemon_reopens_its_log(keep: usize) {
    let mut profile = Profile::new(false, keep);
    let result = profile.start();
    assert!(result.success(), "{result:?}");
    let live = profile.data.join("daemon.log");
    let old = profile.data.join("daemon.log.1");
    assert!(!old.exists(), "rotation must happen after daemon start");
    {
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(&live)
            .unwrap();
        log.write_all(b"before-live-rotation\n".repeat(480).as_slice())
            .unwrap();
        log.flush().unwrap();
    }
    let until = Instant::now() + Duration::from_secs(85);
    loop {
        let live_text = std::fs::read_to_string(&live).unwrap_or_default();
        if live_text.contains("rotated daemon log") {
            assert!(!live_text.contains("before-live-rotation"));
            if keep == 0 {
                assert!(!old.exists(), "zero retention must not keep a rotated log");
            } else {
                assert!(std::fs::read_to_string(&old)
                    .unwrap()
                    .contains("before-live-rotation"));
            }
            break;
        }
        assert!(
            Instant::now() < until,
            "live log never reopened after rotation: {live_text}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    let status = profile.run(&["daemon", "status", "--check"]);
    assert!(status.status.success(), "{status:?}");
}

#[test]
fn long_windows_profile_works_or_doctor_names_the_path_limit() {
    let mut profile = Profile::new(true, 1);
    assert!(profile.data.as_os_str().encode_wide().count() > 260);
    if profile.start().success() {
        assert!(profile.data.join("formless.db").is_file());
        let status = profile.run(&["daemon", "status", "--check"]);
        assert!(status.status.success(), "{status:?}");
    } else {
        let doctor = profile.run(&["doctor", "--json"]);
        let output = format!(
            "{}\n{}",
            String::from_utf8_lossy(&doctor.stdout),
            String::from_utf8_lossy(&doctor.stderr)
        );
        assert!(!doctor.status.success(), "{output}");
        assert!(
            output.contains("Windows data directory path") && output.contains("FORMLESS_DATA_DIR"),
            "{output}"
        );
    }
}

#[test]
fn generated_windows_service_launcher_owns_and_reopens_the_live_log() {
    use formless_core::{service, Paths};
    use formless_runtime::process_group::SupervisedProcess;
    use std::path::Path;

    fn wait(process: &mut SupervisedProcess, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            if process.has_exited().unwrap() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn cli(paths: &Paths, args: &[&str]) -> bool {
        let mut command = Command::new(env!("CARGO_BIN_EXE_formless"));
        command
            .args(args)
            .env("FORMLESS_DATA_DIR", &paths.data_dir)
            .env("FORMLESS_CONFIG_DIR", &paths.config_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut process = SupervisedProcess::spawn(command).unwrap();
        wait(&mut process, Duration::from_secs(5)) && process.finish().unwrap().success()
    }

    // Drop order: the supervised Job terminates the shell and every child
    // (even if the shell exited) before TempDir removes isolated profile data.
    let root = tempfile::Builder::new()
        .prefix("fl-task-launch-")
        .tempdir()
        .unwrap();
    let paths = Paths {
        data_dir: root.path().join("data %TEMP% & spaces!"),
        config_dir: root.path().join("config %TEMP% & spaces!"),
        profile: "default".into(),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(
        paths.config_dir.join("config.toml"),
        "[daemon]\nlog_max_bytes = 4096\nlog_keep = 0\n[provider]\nbase_url = \"http://127.0.0.1:9\"\n",
    )
    .unwrap();
    let plan = service::plan(
        &paths,
        Path::new(env!("CARGO_BIN_EXE_formless")),
        root.path(),
    )
    .unwrap();
    assert_eq!(plan.manager, "Task Scheduler");
    std::fs::write(&plan.file, &plan.contents).unwrap();

    // Run the exact installed definition without registering a persistent
    // task or inheriting a capture pipe. The shell must stay alive for the daemon.
    let mut command = Command::new("cmd.exe");
    command
        .args(["/D", "/C"])
        .arg(plan.file.file_name().unwrap())
        .current_dir(&paths.data_dir)
        .env("RUST_LOG", "info")
        .env_remove("FORMLESS_DAEMON_LOG")
        .env_remove("FORMLESS_API_KEY")
        .env_remove("FORMLESS_BASE_URL")
        .env_remove("FORMLESS_MODEL")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut launcher = SupervisedProcess::spawn(command).unwrap();
    let live = paths.log_file();
    let startup_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            !launcher.has_exited().unwrap(),
            "service launcher exited before the daemon was ready"
        );
        if cli(&paths, &["daemon", "status", "--check"]) {
            break;
        }
        assert!(Instant::now() < startup_deadline, "daemon did not start");
        std::thread::sleep(Duration::from_millis(100));
    }

    // Force a live rotation, not a startup cleanup. Retain a second name to
    // verify the zero-retention path did not truncate the prior log.
    let old = root.path().join("linked-old.log");
    std::fs::hard_link(&live, &old).unwrap();
    let marker = "before-scheduled-launcher-rotation";
    {
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(&live)
            .unwrap();
        log.write_all(format!("{marker}\n").repeat(240).as_bytes())
            .unwrap();
        log.flush().unwrap();
    }
    let rotation_deadline = Instant::now() + Duration::from_secs(85);
    loop {
        assert!(
            !launcher.has_exited().unwrap(),
            "service launcher exited before rotation completed"
        );
        let text = std::fs::read_to_string(&live).unwrap_or_default();
        if text.contains("rotated daemon log") {
            assert!(!text.contains(marker));
            assert!(std::fs::read_to_string(&old).unwrap().contains(marker));
            assert!(!live.with_extension("log.1").exists());
            break;
        }
        assert!(
            Instant::now() < rotation_deadline,
            "live log never reopened after rotation: {text}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }

    assert!(cli(&paths, &["daemon", "status", "--check"]));
    assert!(cli(&paths, &["daemon", "stop"]));
    assert!(
        wait(&mut launcher, Duration::from_secs(15)),
        "service launcher did not stop"
    );
    assert!(launcher.finish().unwrap().success());
}
