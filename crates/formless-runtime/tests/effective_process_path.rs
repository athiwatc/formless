//! Effective executable guards use private copies of this test binary.
use formless_runtime::sensitive_paths::PathPolicy;
use formless_runtime::{execute, Capabilities, ExecErrorKind, ExecRequest, HostMode, HostOps};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Arc};

#[test]
#[ignore = "private executable helper"]
fn effective_path_helper() {
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
