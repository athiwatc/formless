//! Operation-boundary regressions. All files and policy roots are private fixtures.
use super::*;
use sensitive_paths::PathPolicy;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("fl-read-guard-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }
    fn request(&self, code: &str, path: &Path) -> ExecRequest {
        let input = json!({"path": path, "to": self.0.join("public.txt")});
        ExecRequest {
            output_schema: None,
            code: code.into(),
            input: input.clone(),
            caps: Capabilities::parse(&[
                "fs:read:$input.path".into(),
                format!("fs:write:{}", self.0.display()),
            ])
            .unwrap()
            .bind(&input)
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
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
struct ConsentHost {
    policy: PathPolicy,
    allow: bool,
    paths: Mutex<Vec<PathBuf>>,
}
impl HostOps for ConsentHost {
    fn path_policy(&self) -> Result<PathPolicy, String> {
        Ok(self.policy.clone())
    }
    fn approve_sensitive_read(&self, path: &Path, _: &str, _: &str) -> Result<(), String> {
        self.paths.locked().push(path.into());
        if self.allow {
            Ok(())
        } else {
            Err("fixture declined credential read".into())
        }
    }
    // Deliberately retain the no-op file_changing default: rename safety
    // must not depend on snapshots, their size limit or backup configuration.
}
fn host(policy: PathPolicy, allow: bool) -> Arc<ConsentHost> {
    Arc::new(ConsentHost {
        policy,
        allow,
        paths: Mutex::new(Vec::new()),
    })
}
#[test]
fn input_bound_project_read_is_quiet_but_credentials_need_exact_consent() {
    let f = Fixture::new();
    let normal = f.0.join("notes.txt");
    let key = f.0.join("private.key");
    std::fs::write(&normal, "project").unwrap();
    std::fs::write(&key, "fixture-secret").unwrap();
    let ops = host(PathPolicy::default(), true);
    let code = "function run(input) { return [fs.read(input.path), fs.read(input.path)]; }";
    assert_eq!(
        execute(&f.request(code, &normal), ops.clone())
            .unwrap()
            .output,
        json!(["project", "project"])
    );
    assert!(ops.paths.locked().is_empty());
    assert_eq!(
        execute(&f.request(code, &key), ops.clone()).unwrap().output,
        json!(["fixture-secret", "fixture-secret"])
    );
    assert_eq!(*ops.paths.locked(), vec![key.canonicalize().unwrap()]);
    assert!(execute(&f.request(code, &key), Arc::new(NoHostOps)).is_err());
}
#[test]
fn profile_denial_precedes_consent_and_broad_grants() {
    let f = Fixture::new();
    let profile = f.0.join("profile");
    std::fs::create_dir(&profile).unwrap();
    let key = profile.join("api_key");
    std::fs::write(&key, "fixture-secret").unwrap();
    let ops = host(PathPolicy::new(vec![profile], vec![]).unwrap(), true);
    let mut req = f.request("function run(input) { return fs.read(input.path); }", &key);
    req.caps.fs_read = vec![f.0.to_str().unwrap().into()];
    assert!(execute(&req, ops.clone())
        .unwrap_err()
        .to_string()
        .contains("profile"));
    assert!(ops.paths.locked().is_empty());
    assert_eq!(std::fs::read_to_string(key).unwrap(), "fixture-secret");
}
#[test]
fn rename_cannot_launder_a_credential_when_snapshot_callback_is_noop() {
    let f = Fixture::new();
    let key = f.0.join("private.key");
    std::fs::write(&key, "fixture-secret").unwrap();
    let ops = host(PathPolicy::default(), false);
    let req = f.request(
        "function run(input) { fs.rename(input.path, input.to); return fs.read(input.to); }",
        &key,
    );
    assert!(execute(&req, ops.clone()).is_err());
    assert_eq!(*ops.paths.locked(), vec![key.canonicalize().unwrap()]);
    assert_eq!(std::fs::read_to_string(&key).unwrap(), "fixture-secret");
    assert!(!f.0.join("public.txt").exists());
}
#[test]
fn smoke_and_replay_never_request_or_return_credential_bytes() {
    let f = Fixture::new();
    let key = f.0.join("private.key");
    std::fs::write(&key, "fixture-secret").unwrap();
    for mode in [HostMode::Smoke, HostMode::Replay] {
        let ops = host(PathPolicy::default(), true);
        let mut req = f.request("function run(input) { return fs.read(input.path); }", &key);
        req.mode = mode;
        assert!(execute(&req, ops.clone()).is_err());
        assert!(ops.paths.locked().is_empty());
    }
}
