//! Doctor dispatch/output coverage in owned test subprocesses. The child runs
//! the real Cli parser and run_cli dispatcher with an in-memory keyring, so
//! availability probes never read the user's native credential store. This is
//! not native-keychain persistence coverage; ordinary Run end-to-end tests
//! remain in tests/run_pipeline.rs against the shipped binary.
use super::{Cli, Cmd};
use clap::Parser;
use formless_core::{Config, Paths};
use serde_json::Value;
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
const BEGIN: &str = "PRIVATE_DOCTOR_STDOUT_BEGIN\n";
const END: &str = "PRIVATE_DOCTOR_STDOUT_END\n";

#[test]
fn naming_status_reports_auto_download_and_model_size() {
    let f = Fixture::new();
    let mut cfg = Config::load(&f.paths).unwrap();
    cfg.naming.enabled = true;
    let status = super::naming_model_status(&f.paths, &cfg);
    assert!(status.contains("not installed"), "{status}");
    assert!(status.contains("auto-download"), "{status}");
    assert!(!status.contains("auto-download on first use"), "{status}");
    assert!(status.contains("409 MiB"), "{status}");
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[derive(Clone)]
struct SharedLogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for SharedLogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Fixture {
    root: PathBuf,
    paths: Paths,
    provider: TcpListener,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "fl-doctor-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&root).unwrap();
        let paths = Paths::from_dirs(root.join("d"), root.join("c"))
            .unwrap()
            .named("p")
            .unwrap();
        let provider = TcpListener::bind("127.0.0.1:0").unwrap();
        provider.set_nonblocking(true).unwrap();
        let fixture = Self {
            root,
            paths,
            provider,
        };
        let mut cfg = Config::default();
        cfg.provider.base_url = fixture.provider_url();
        cfg.provider.model = "model".into();
        cfg.naming.enabled = false;
        cfg.save(&fixture.paths).unwrap();
        fixture
    }
    fn provider_url(&self) -> String {
        format!("http://{}", self.provider.local_addr().unwrap())
    }
    fn assert_no_provider_calls(&self) {
        assert!(
            matches!(self.provider.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
            "doctor must inspect configuration without contacting a provider"
        );
    }
    fn run(&self, profile: &str, args: &[&str]) -> Output {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "cli_doctor_tests::doctor_child_fixture",
            "--ignored",
            "--nocapture",
        ]);
        for (key, _) in std::env::vars_os() {
            let key_text = key.to_string_lossy().to_ascii_uppercase();
            if [
                "FORMLESS_",
                "OPENAI_",
                "ANTHROPIC_",
                "GROK_",
                "GOOGLE_",
                "GEMINI_",
            ]
            .iter()
            .any(|prefix| key_text.starts_with(prefix))
                || key_text.contains("API_KEY")
                || key_text.contains("OAUTH")
                || key_text == "FL_TEST_REFRESH_CHILD"
            {
                command.env_remove(key);
            }
        }
        let argv: Vec<_> = ["formless", "--profile", profile]
            .into_iter()
            .chain(args.iter().copied())
            .collect();
        command
            .current_dir(&self.root)
            .env("FORMLESS_DATA_DIR", self.root.join("d"))
            .env("FORMLESS_CONFIG_DIR", self.root.join("c"))
            .env(
                "FORMLESS_TEST_DOCTOR_ARGS",
                serde_json::to_string(&argv).unwrap(),
            )
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("RUST_LOG", "error")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = OwnedChild(command.spawn().unwrap());
        let mut stdout = child.0.stdout.take().unwrap();
        let mut stderr = child.0.stderr.take().unwrap();
        let out = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let err = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).unwrap();
            bytes
        });
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.0.kill().unwrap();
                child.0.wait().unwrap();
                panic!(
                    "owned doctor child timed out; stdout={:?}; stderr={:?}",
                    out.join().unwrap(),
                    err.join().unwrap()
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let raw = out.join().unwrap();
        let stderr = err.join().unwrap();
        assert!(
            matches!(status.code(), Some(0 | 1)),
            "doctor child failed: {}",
            String::from_utf8_lossy(&stderr)
        );
        // Strip only our libtest wrapper. Every byte run_cli writes between
        // these markers remains in the exact stdout/JSON assertions below.
        let starts: Vec<_> = raw
            .windows(BEGIN.len())
            .enumerate()
            .filter_map(|(i, w)| (w == BEGIN.as_bytes()).then_some(i + BEGIN.len()))
            .collect();
        let ends: Vec<_> = raw
            .windows(END.len())
            .enumerate()
            .filter_map(|(i, w)| (w == END.as_bytes()).then_some(i))
            .collect();
        assert_eq!(
            starts.len(),
            1,
            "one begin marker: {}",
            String::from_utf8_lossy(&raw)
        );
        assert_eq!(
            ends.len(),
            1,
            "one end marker: {}",
            String::from_utf8_lossy(&raw)
        );
        assert!(starts[0] <= ends[0]);
        Output {
            status,
            stdout: raw[starts[0]..ends[0]].to_vec(),
            stderr,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(unix)]
#[test]
fn doctor_fixes_open_config_backup_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let backup = fixture.paths.config_file().with_extension("toml.bak");
    std::fs::write(&backup, "fixture backup").unwrap();
    std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o644)).unwrap();
    let problem = super::doctor_local_problems(&fixture.paths)
        .into_iter()
        .find(|problem| matches!(&problem.fix, Some(super::Fix::Chmod600(files)) if files.contains(&backup)))
        .expect("doctor should report an open config backup");
    super::apply_fix(&fixture.paths, &problem.fix.unwrap()).unwrap();
    let mode = std::fs::metadata(&backup).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
#[ignore = "owned subprocess fixture; invoked by doctor dispatch/output tests"]
fn doctor_child_fixture() {
    let env = super::oauth_credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-doctor-fixture-key");
    keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
    let argv: Vec<String> = serde_json::from_str(
        &std::env::var("FORMLESS_TEST_DOCTOR_ARGS").expect("owned doctor parent arguments"),
    )
    .unwrap();
    let cli = Cli::try_parse_from(argv).unwrap();
    assert!(
        matches!(
            cli.cmd,
            Some(Cmd::Doctor {
                fix: false,
                restore_drill: false,
                ..
            })
        ),
        "only read-only doctor dispatch is allowed in this fixture"
    );
    print!("{BEGIN}");
    std::io::stdout().flush().unwrap();
    let status = match super::run_cli(cli) {
        Ok(()) => 0,
        Err(error) if error.is::<super::DoctorProblems>() => super::exit_code(&error),
        Err(error) => panic!("doctor child failed: {error:#}"),
    };
    println!("{}", END.trim_end());
    std::io::stdout().flush().unwrap();
    if status != 0 {
        std::process::exit(status);
    }
}

#[cfg(unix)]
#[test]
fn config_edit_warns_about_unknown_config_keys() {
    use std::os::unix::fs::PermissionsExt;

    let f = Fixture::new();
    let editor = f.root.join("editor.sh");
    std::fs::write(
        &editor,
        "#!/bin/sh\nprintf '%s\\n' '[provider]' 'model = \"fixture\"' 'modle = \"fixture\"' > \"$1\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&editor, std::fs::Permissions::from_mode(0o700)).unwrap();

    let argv = ["formless", "--profile", "p", "config", "edit"];
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        "cli_doctor_tests::config_edit_child_fixture",
        "--ignored",
        "--nocapture",
    ]);
    for (key, _) in std::env::vars_os() {
        let key_text = key.to_string_lossy().to_ascii_uppercase();
        if [
            "FORMLESS_",
            "OPENAI_",
            "ANTHROPIC_",
            "GROK_",
            "GOOGLE_",
            "GEMINI_",
        ]
        .iter()
        .any(|prefix| key_text.starts_with(prefix))
            || key_text.contains("API_KEY")
            || key_text.contains("OAUTH")
        {
            command.env_remove(key);
        }
    }
    let output = command
        .current_dir(&f.root)
        .env("FORMLESS_DATA_DIR", f.root.join("d"))
        .env("FORMLESS_CONFIG_DIR", f.root.join("c"))
        .env(
            "FORMLESS_TEST_CONFIG_EDIT_ARGS",
            serde_json::to_string(&argv).unwrap(),
        )
        .env("EDITOR", &editor)
        .env_remove("VISUAL")
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr
            .contains("unknown configuration key 'provider.modle'; did you mean 'provider.model'?"),
        "{stderr}"
    );
    assert!(std::fs::read_to_string(f.paths.config_file())
        .unwrap()
        .contains("modle = \"fixture\""));
}

#[test]
fn daemon_startup_logs_unknown_config_keys() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "cli_doctor_tests::daemon_startup_log_child_fixture",
            "--ignored",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "owned config-edit subprocess fixture"]
fn config_edit_child_fixture() {
    let _env = super::oauth_credential_env::EnvGuard::isolated();
    let argv: Vec<String> = serde_json::from_str(
        &std::env::var("FORMLESS_TEST_CONFIG_EDIT_ARGS")
            .expect("owned config-edit parent arguments"),
    )
    .unwrap();
    let cli = Cli::try_parse_from(argv).unwrap();
    super::run_cli(cli).unwrap();
}

#[tokio::test]
#[ignore = "owned daemon-startup subprocess fixture"]
async fn daemon_startup_log_child_fixture() {
    use tracing_subscriber::util::SubscriberInitExt;

    let env = super::oauth_credential_env::EnvGuard::isolated();
    for key in [
        "FORMLESS_BASE_URL",
        "FORMLESS_MODEL",
        "FORMLESS_IDLE_GRACE_SECS",
        "FORMLESS_SECURE",
    ] {
        env.remove(key);
    }
    let f = Fixture::new();
    let mut config = Config::load_file_only(&f.paths).unwrap();
    config.provider.model.clear();
    config.save(&f.paths).unwrap();
    let file = f.paths.config_file();
    let original = std::fs::read_to_string(&file).unwrap();
    let edited = original.replacen("[provider]\n", "[provider]\nmodle = \"gpt-5\"\n", 1);
    assert_ne!(original, edited);
    std::fs::write(&file, edited).unwrap();

    let logs = Arc::new(Mutex::new(Vec::new()));
    let writer = logs.clone();
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing_subscriber::filter::LevelFilter::WARN)
        .with_writer(move || SharedLogWriter(writer.clone()))
        .finish()
        .try_init()
        .unwrap();
    let daemon = formless_core::daemon::Daemon::new(f.paths.clone()).unwrap();
    let logs = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("unknown configuration key 'provider.modle'; did you mean 'provider.model'?"),
        "{logs}"
    );
    drop(daemon);
    f.assert_no_provider_calls();
}

#[test]
fn doctor_reports_unknown_role_providers_without_changing_configuration() {
    use formless_core::config::{ProviderCfg, RoleCfg};
    let f = Fixture::new();
    let mut cfg = Config::load_file_only(&f.paths).unwrap();
    cfg.provider.model.clear(); // Diagnostics need no provider request.
    cfg.providers.insert(
        "local".into(),
        ProviderCfg {
            base_url: f.provider_url(),
            model: "model".into(),
            ..Default::default()
        },
    );
    for (name, provider) in [
        ("broken", Some("missing-provider")),
        ("valid", Some("local")),
        ("default", Some("default")),
        ("empty", Some("")),
        ("implicit", None),
    ] {
        cfg.roles.insert(
            name.into(),
            RoleCfg {
                provider: provider.map(str::to_string),
                model: "model".into(),
                ..Default::default()
            },
        );
    }
    cfg.save(&f.paths).unwrap();
    let before = std::fs::read(f.paths.config_file()).unwrap();
    let human = f.run("p", &["doctor"]);
    assert_eq!(
        human.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(
        text.contains("role \"broken\"") && text.contains("unknown provider \"missing-provider\""),
        "{text}"
    );
    let json = f.run("p", &["doctor", "--json"]);
    assert_eq!(
        json.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    let value: Value = serde_json::from_slice(&json.stdout).unwrap();
    let role_problems: Vec<_> = value["problems"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["what"].as_str().is_some_and(|s| s.starts_with("role ")))
        .collect();
    assert_eq!(role_problems.len(), 1, "{value}");
    assert!(role_problems[0]["next"]
        .as_str()
        .unwrap()
        .contains("formless config edit"));
    assert_eq!(std::fs::read(f.paths.config_file()).unwrap(), before);
    assert!(!formless_core::daemon::endpoint_occupied(&f.paths).unwrap());
    #[cfg(unix)]
    assert!(!f.paths.socket_path().exists());
    assert!(
        !f.paths.lock_file().exists(),
        "doctor must not publish a PID"
    );
    f.assert_no_provider_calls();
}

#[test]
fn doctor_reports_unknown_config_keys_and_suggestions() {
    let f = Fixture::new();
    let mut config = Config::load_file_only(&f.paths).unwrap();
    config.provider.model.clear();
    config.save(&f.paths).unwrap();
    let file = f.paths.config_file();
    let original = std::fs::read_to_string(&file).unwrap();
    let edited = original.replacen("[provider]\n", "[provider]\nmodle = \"gpt-5\"\n", 1);
    assert_ne!(original, edited);
    std::fs::write(&file, &edited).unwrap();

    let json = f.run("p", &["doctor", "--json"]);
    assert_eq!(
        json.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&json.stderr)
    );
    let value: Value = serde_json::from_slice(&json.stdout).unwrap();
    let problems: Vec<_> = value["problems"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|problem| {
            problem["what"]
                .as_str()
                .is_some_and(|what| what.contains("unknown configuration key"))
        })
        .collect();
    assert_eq!(problems.len(), 1, "{value}");
    assert!(problems[0]["what"]
        .as_str()
        .unwrap()
        .contains("did you mean 'provider.model'?"));
    let next = problems[0]["next"].as_str().unwrap();
    assert!(
        next.starts_with("review ") && next.ends_with(".toml"),
        "{next}"
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), edited);
    f.assert_no_provider_calls();
}

#[test]
fn unconfigured_doctor_names_the_profile_and_offers_saved_setup_elsewhere() {
    let f = Fixture::new();
    let before = std::fs::read(f.paths.config_file()).unwrap();
    for active in ["default", "scratch"] {
        let doctor = f.run(active, &["doctor"]);
        assert_eq!(
            doctor.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&doctor.stderr)
        );
        let text = String::from_utf8(doctor.stdout).unwrap();
        assert!(text.contains(&format!("profile:    {active}")), "{text}");
        assert!(
            text.contains("formless --profile p") && text.contains("formless profiles list"),
            "{text}"
        );
        let json = f.run(active, &["doctor", "--json"]);
        assert_eq!(
            json.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&json.stderr)
        );
        let value: Value = serde_json::from_slice(&json.stdout).unwrap();
        assert_eq!(value["profile"], active);
        assert!(value["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|problem| problem["next"]
                .as_str()
                .is_some_and(|next| next.contains("formless --profile p"))));
    }
    f.assert_no_provider_calls();
    assert!(!formless_core::daemon::daemon_reachable(&f.paths));
    assert_eq!(std::fs::read(f.paths.config_file()).unwrap(), before);
}
