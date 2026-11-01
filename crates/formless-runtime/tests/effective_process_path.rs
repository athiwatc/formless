//! Effective executable guards use private copies of this test binary.
use formless_runtime::sensitive_paths::PathPolicy;
use formless_runtime::{execute, Capabilities, ExecErrorKind, ExecRequest, HostMode, HostOps};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

#[test]
#[ignore = "private executable helper"]
fn effective_path_helper() {
    if let Ok(probe) = std::env::var("FL_EFFECTIVE_PATH_PROBE") {
        let f = Fixture::new();
        let allowed = f.root.join("allowed");
        std::fs::create_dir(&allowed).unwrap();
        std::fs::copy(std::env::current_exe().unwrap(), allowed.join(&f.program)).unwrap();
        // This runs only inside a private subprocess, never the parallel test
        // runner. It supplies a deliberately different parent lookup identity.
        std::env::set_var("PATH", &allowed);
        for api in ["run", "spawn"] {
            let mut req = f.request(api, false);
            if probe == "scoped" {
                req.caps = Capabilities::parse(&[
                    format!("process:{}", allowed.join(&f.program).display()),
                    "env:PATH".into(),
                    format!("fs:read:{}", f.root.display()),
                ])
                .unwrap();
                let error = execute(&req, Arc::new(formless_runtime::NoHostOps)).unwrap_err();
                assert_eq!(error.kind, ExecErrorKind::Denied);
                assert!(error.message.contains("not in scope"));
            } else {
                req.input["env"]["PATH"] = serde_json::Value::Null;
                let error = execute(&req, Arc::new(formless_runtime::NoHostOps)).unwrap_err();
                assert!(error.message.contains("not found"));
            }
            assert!(!f.root.join("started").exists());
        }
        println!("PRIVATE_PARENT_PATH_PROBE_PASSED");
        std::process::exit(0);
    }
    let Some(marker) = std::env::var_os("FL_EFFECTIVE_PATH_MARKER") else {
        return;
    };
    std::fs::write(
        marker,
        std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .as_bytes(),
    )
    .unwrap();
    println!("EFFECTIVE_PATH_HELPER_RAN");
    std::process::exit(0);
}
struct Fixture {
    root: PathBuf,
    project: PathBuf,
    private: PathBuf,
    program: String,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("fl-effective-path-{}", uuid::Uuid::new_v4()));
        let project = root.join("project");
        let private = project.join("profile");
        std::fs::create_dir_all(&private).unwrap();
        let root = root.canonicalize().unwrap();
        let project = project.canonicalize().unwrap();
        let private = private.canonicalize().unwrap();
        let program = format!(
            "fl-exec-{}{}",
            uuid::Uuid::new_v4(),
            if cfg!(windows) { ".exe" } else { "" }
        );
        std::fs::copy(std::env::current_exe().unwrap(), private.join(&program)).unwrap();
        Self {
            root,
            project,
            private,
            program,
        }
    }
    fn request(&self, api: &str, relative: bool) -> ExecRequest {
        let path = if relative {
            "profile".into()
        } else {
            self.private.to_string_lossy().into_owned()
        };
        let call = if api == "run" {
            "process.run(i.cmd,i.args,o)"
        } else {
            "process.spawn(i.cmd,i.args,o).wait()"
        };
        ExecRequest {
            code: format!(
                "function run(i){{const o={{cwd:i.cwd,env:i.env,timeout_ms:5000}};return {call};}}"
            ),
            input: json!({"cmd":self.program,"args":["--exact","effective_path_helper","--ignored","--nocapture"],"cwd":self.project,
              "env":{"PATH":path,"FL_EFFECTIVE_PATH_MARKER":self.root.join("started")}}),
            output_schema: None,
            caps: Capabilities::parse(&[
                format!("process:{}", self.program),
                "env:PATH".into(),
                format!("fs:read:{}", self.root.display()),
            ])
            .unwrap(),
            memory_mb: 64,
            mode: HostMode::Live,
            cancel: Arc::new(AtomicBool::new(false)),
            overlay: None,
            activity: None,
            modules: Arc::new(Vec::new()),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
struct Protected(PathBuf);
impl HostOps for Protected {
    fn path_policy(&self) -> Result<PathPolicy, String> {
        PathPolicy::new(vec![self.0.clone()], vec![])
    }
}

#[test]
fn child_path_override_cannot_execute_a_profile_program_even_with_an_env_grant() {
    for api in ["run", "spawn"] {
        for relative in [false, true] {
            let f = Fixture::new();
            let result = execute(
                &f.request(api, relative),
                Arc::new(Protected(f.private.clone())),
            );
            assert!(
                result.is_err(),
                "the preexisting parent-PATH guard allowed the child-PATH executable: {result:?}"
            );
            let error = result.unwrap_err();
            assert_eq!(error.kind, ExecErrorKind::Denied);
            assert!(error.message.contains("profile"));
            assert!(
                !f.root.join("started").exists(),
                "a denied executable ran before its actual path was guarded"
            );
        }
    }
}

#[test]
fn ordinary_child_path_controls_work_for_absolute_and_relative_entries() {
    for api in ["run", "spawn"] {
        for relative in [false, true] {
            let f = Fixture::new();
            let value = execute(
                &f.request(api, relative),
                Arc::new(formless_runtime::NoHostOps),
            )
            .unwrap()
            .output;
            assert_eq!(value["code"], 0);
            let actual = PathBuf::from(std::fs::read_to_string(f.root.join("started")).unwrap());
            assert_eq!(
                actual.canonicalize().unwrap(),
                f.private.join(&f.program).canonicalize().unwrap()
            );
        }
    }
}
struct AddEarlierProgram {
    sensitive: PathBuf,
    source: PathBuf,
    earlier: PathBuf,
    count: AtomicUsize,
}
impl HostOps for AddEarlierProgram {
    fn path_policy(&self) -> Result<PathPolicy, String> {
        PathPolicy::new(
            vec![],
            vec![
                formless_runtime::sensitive_paths::SensitivePathRule::Subtree(
                    self.sensitive.to_string_lossy().into_owned(),
                ),
            ],
        )
    }
    fn approve_sensitive_read(
        &self,
        path: &std::path::Path,
        _: &str,
        _: &str,
    ) -> Result<(), String> {
        assert!(path.starts_with(&self.sensitive));
        self.count.fetch_add(1, Ordering::SeqCst);
        std::fs::copy(&self.source, &self.earlier).map_err(|e| e.to_string())?;
        Ok(())
    }
}
#[test]
fn consent_cannot_be_redirected_by_inserting_an_earlier_path_candidate() {
    for api in ["run", "spawn"] {
        let f = Fixture::new();
        let first = f.project.join("first");
        std::fs::create_dir(&first).unwrap();
        let path = std::env::join_paths([&first, &f.private]).unwrap();
        let mut req = f.request(api, false);
        req.input["env"]["PATH"] = json!(path);
        let host = Arc::new(AddEarlierProgram {
            sensitive: f.private.clone(),
            source: std::env::current_exe().unwrap(),
            earlier: first.join(&f.program),
            count: AtomicUsize::new(0),
        });
        let value = execute(&req, host.clone()).unwrap().output;
        assert_eq!(value["code"], 0);
        assert_eq!(
            host.count.load(Ordering::SeqCst),
            1,
            "the actual executable did not receive exact consent"
        );
        assert!(
            first.join(&f.program).is_file(),
            "the consent callback did not alter PATH selection"
        );
        let actual = PathBuf::from(std::fs::read_to_string(f.root.join("started")).unwrap());
        assert_eq!(
            actual.canonicalize().unwrap(),
            f.private.join(&f.program).canonicalize().unwrap(),
            "the OS searched PATH again after consent and executed an unguarded different file"
        );
    }
}

#[cfg(unix)]
#[test]
fn inaccessible_earlier_candidate_does_not_hide_later_executable() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let first = f.project.join("first");
    std::fs::create_dir(&first).unwrap();
    let inaccessible = first.join(&f.program);
    std::fs::write(&inaccessible, "not executable").unwrap();
    std::fs::set_permissions(&inaccessible, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut req = f.request("run", false);
    req.input["env"]["PATH"] = json!(std::env::join_paths([&first, &f.private]).unwrap());
    let value = execute(&req, Arc::new(formless_runtime::NoHostOps))
        .unwrap()
        .output;
    assert_eq!(value["code"], 0);
    let actual = PathBuf::from(std::fs::read_to_string(f.root.join("started")).unwrap());
    assert_eq!(
        actual.canonicalize().unwrap(),
        f.private.join(&f.program).canonicalize().unwrap()
    );
}

#[test]
fn removed_path_does_not_use_the_supplied_child_override() {
    // An unknown UUID cannot be found by the platform default lookup. No
    // global PATH mutation is needed to prove removal cannot use this override.
    let f = Fixture::new();
    let mut req = f.request("run", false);
    req.input["env"]["PATH"] = serde_json::Value::Null;
    let error = execute(&req, Arc::new(formless_runtime::NoHostOps)).unwrap_err();
    assert!(error.message.contains("not found"));
    assert!(!f.root.join("started").exists());
}

#[test]
fn explicit_child_relative_programs_are_guarded_and_bound_without_path_lookup() {
    for api in ["run", "spawn"] {
        let f = Fixture::new();
        let mut req = f.request(api, false);
        let requested = std::path::Path::new("profile")
            .join(&f.program)
            .to_string_lossy()
            .into_owned();
        req.input["cmd"] = json!(requested);
        req.input["env"]["PATH"] = json!("missing-directory");
        req.caps = Capabilities::parse(&[
            format!("process:{}", f.private.join(&f.program).display()),
            "env:PATH".into(),
            format!("fs:read:{}", f.root.display()),
        ])
        .unwrap();
        assert_eq!(
            execute(&req, Arc::new(Protected(f.private.clone())))
                .unwrap_err()
                .kind,
            ExecErrorKind::Denied
        );
        assert!(!f.root.join("started").exists());
        let value = execute(&req, Arc::new(formless_runtime::NoHostOps))
            .unwrap()
            .output;
        assert_eq!(value["code"], 0);
    }
}

fn parent_path_probe(probe: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "effective_path_helper",
            "--ignored",
            "--nocapture",
        ])
        .env("FL_EFFECTIVE_PATH_PROBE", probe)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "private parent-PATH fixture failed: {:?}",
        output.status
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("PRIVATE_PARENT_PATH_PROBE_PASSED"));
}
#[test]
fn path_scoped_grant_cannot_authorize_a_different_child_path_program() {
    parent_path_probe("scoped");
}
#[cfg(unix)]
#[test]
fn removed_child_path_uses_native_default_instead_of_the_parent_path() {
    parent_path_probe("removed");
}
