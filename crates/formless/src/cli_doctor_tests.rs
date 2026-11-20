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
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
const BEGIN: &str = "PRIVATE_DOCTOR_STDOUT_BEGIN\n";
const END: &str = "PRIVATE_DOCTOR_STDOUT_END\n";

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
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
            status.success(),
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
    super::run_cli(cli).unwrap();
    std::io::stdout().flush().unwrap();
    println!("{}", END.trim_end());
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
    assert!(
        human.status.success(),
        "{}",
        String::from_utf8_lossy(&human.stderr)
    );
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(
        text.contains("role \"broken\"") && text.contains("unknown provider \"missing-provider\""),
        "{text}"
    );
    let json = f.run("p", &["doctor", "--json"]);
    assert!(
        json.status.success(),
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
fn unconfigured_doctor_names_the_profile_and_offers_saved_setup_elsewhere() {
    let f = Fixture::new();
    let before = std::fs::read(f.paths.config_file()).unwrap();
    for active in ["default", "scratch"] {
        let doctor = f.run(active, &["doctor"]);
        assert!(
            doctor.status.success(),
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
        assert!(
            json.status.success(),
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
