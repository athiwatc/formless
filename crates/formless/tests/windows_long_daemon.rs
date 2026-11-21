//! Detached Windows daemons rotate a live log and report long path errors.
#![cfg(windows)]

use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Profile {
    root: PathBuf,
    data: PathBuf,
    config: PathBuf,
    started: bool,
}

impl Profile {
    fn new(long: bool) -> Self {
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
            "[daemon]\nlog_max_bytes = 4096\nlog_keep = 1\nidle_grace_secs = 600\n[provider]\nbase_url = \"http://127.0.0.1:9\"\n",
        ).unwrap();
        Self {
            root,
            data,
            config,
            started: false,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_formless"))
            .args(args)
            .env("FORMLESS_DATA_DIR", &self.data)
            .env("FORMLESS_CONFIG_DIR", &self.config)
            .env_remove("FORMLESS_API_KEY")
            .env_remove("FORMLESS_BASE_URL")
            .env_remove("FORMLESS_MODEL")
            .env("RUST_LOG", "info")
            .output()
            .unwrap()
    }

    fn start(&mut self) -> Output {
        let result = self.run(&["daemon", "start"]);
        self.started = result.status.success();
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
    let mut profile = Profile::new(false);
    let result = profile.start();
    assert!(result.status.success(), "{result:?}");
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
        if old.exists() && live_text.contains("rotated daemon log") {
            assert!(!live_text.contains("before-live-rotation"));
            assert!(std::fs::read_to_string(&old)
                .unwrap()
                .contains("before-live-rotation"));
            break;
        }
        assert!(
            Instant::now() < until,
            "live log never reopened after rotation: {live_text}"
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    assert!(profile
        .run(&["daemon", "status", "--json"])
        .status
        .success());
}

#[test]
fn long_windows_profile_works_or_doctor_names_the_path_limit() {
    let mut profile = Profile::new(true);
    assert!(profile.data.as_os_str().encode_wide().count() > 260);
    if profile.start().status.success() {
        assert!(profile.data.join("formless.db").is_file());
        assert!(profile
            .run(&["daemon", "status", "--json"])
            .status
            .success());
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
