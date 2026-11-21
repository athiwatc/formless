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

#[test]
fn apache_password_file_uses_existing_exact_read_consent() {
    let f = Fixture::new();
    let protected = f.0.join(".HTPASSWD");
    let ordinary = f.0.join(".htpasswd.bak");
    std::fs::write(&protected, "private test placeholder").unwrap();
    std::fs::write(&ordinary, "ordinary test placeholder").unwrap();
    let code = "function run(input) { return fs.read(input.path); }";
    let declined = host(PathPolicy::default(), false);
    let protected_read = f.request(code, &protected);
    assert!(execute(&protected_read, declined.clone()).is_err());
    assert_eq!(
        *declined.paths.locked(),
        vec![protected.canonicalize().unwrap()]
    );
    let allowed = host(PathPolicy::default(), true);
    assert_eq!(
        execute(&protected_read, allowed.clone()).unwrap().output,
        json!("private test placeholder")
    );
    assert_eq!(
        *allowed.paths.locked(),
        vec![protected.canonicalize().unwrap()]
    );
    assert_eq!(
        execute(&f.request(code, &ordinary), declined.clone())
            .unwrap()
            .output,
        json!("ordinary test placeholder")
    );
    assert_eq!(declined.paths.locked().len(), 1);
}

#[test]
fn configured_namespace_and_its_ancestor_cannot_be_renamed_out_after_consent() {
    let f = Fixture::new();
    let parent = f.0.join("ordinary-parent");
    let private = parent.join("private-subtree");
    std::fs::create_dir_all(&private).unwrap();
    let secret = private.join("ordinary-name.txt");
    std::fs::write(&secret, "fixture-secret").unwrap();
    let rule = sensitive_paths::SensitivePathRule::Subtree(private.to_str().unwrap().into());
    let ops = host(PathPolicy::new(vec![], vec![rule]).unwrap(), true);
    for from in [&secret, &private, &parent] {
        // Include a successful read before relocation: even an accepted exact
        // read must not wash the configured subtree's future classification.
        let mut req = f.request(
            "function run(i) { fs.read(i.secret); fs.rename(i.path, i.to); return 'laundered'; }",
            from,
        );
        req.input["secret"] = json!(secret);
        req.caps.fs_read = vec![f.0.to_str().unwrap().into()];
        assert!(execute(&req, ops.clone())
            .unwrap_err()
            .to_string()
            .contains("configured sensitive namespace"));
        assert!(from.exists());
        assert!(!f.0.join("public.txt").exists());
        assert_eq!(std::fs::read_to_string(&secret).unwrap(), "fixture-secret");
    }
    let mut within = f.request(
        "function run(i) { fs.rename(i.path,i.to); return fs.read(i.to); }",
        &secret,
    );
    let destination = private.join("still-private.txt");
    within.input["to"] = json!(destination);
    within.caps.fs_read = vec![private.to_str().unwrap().into()];
    assert_eq!(
        execute(&within, ops.clone()).unwrap().output,
        json!("fixture-secret")
    );
    assert!(ops.paths.locked().contains(&destination));
}

#[test]
fn chunks_lines_metadata_and_implicit_old_contents_share_the_gate_in_every_mode() {
    let f = Fixture::new();
    let key = f.0.join("private.key");
    std::fs::write(&key, "fixture-secret\nsecond-line").unwrap();
    for mode in [
        HostMode::Live,
        HostMode::DryRun,
        HostMode::Smoke,
        HostMode::Replay,
    ] {
        for operation in [
            "fs.read(i.path)",
            "fs.readChunks(i.path,4,()=>{})",
            "fs.readLines(i.path,()=>{})",
            "fs.exists(i.path)",
            "fs.stat(i.path)",
            "fs.append(i.path,'changed')",
            "fs.write(i.path,'changed')",
            "fs.remove(i.path)",
        ] {
            let ops = host(PathPolicy::default(), false);
            let mut req = f.request(
                &format!("function run(i) {{ {operation}; return 'escaped'; }}"),
                &key,
            );
            req.mode = mode;
            assert!(execute(&req, ops.clone()).is_err(), "{mode:?}: {operation}");
            assert_eq!(
                ops.paths.locked().len(),
                usize::from(matches!(mode, HostMode::Live | HostMode::DryRun)),
                "{mode:?}: {operation}"
            );
            assert_eq!(
                std::fs::read_to_string(&key).unwrap(),
                "fixture-secret\nsecond-line"
            );
        }
    }
    let ops = host(PathPolicy::default(), true);
    let req = f.request(
        "function run(i) { let text=''; fs.readChunks(i.path,4,s=>text+=s); return text; }",
        &key,
    );
    assert_eq!(
        execute(&req, ops.clone()).unwrap().output,
        json!("fixture-secret\nsecond-line")
    );
    assert_eq!(
        ops.paths.locked().len(),
        1,
        "chunks reuse only exact consent within this Invocation"
    );
}

#[test]
fn ordinary_listing_names_stay_quiet_but_sensitive_metadata_cannot_escape_through_overlay() {
    let f = Fixture::new();
    let key = f.0.join("private.key");
    std::fs::write(&key, "fixture-secret").unwrap();
    for mode in [
        HostMode::Live,
        HostMode::DryRun,
        HostMode::Smoke,
        HostMode::Replay,
    ] {
        let ops = host(PathPolicy::default(), false);
        let mut req = f.request("function run(i) { return fs.list(i.path); }", &f.0);
        req.mode = mode;
        assert_eq!(
            execute(&req, ops.clone()).unwrap().output,
            json!(["private.key"])
        );
        assert!(ops.paths.locked().is_empty());
        req.code = "function run(i) { return fs.list(i.path,{details:true}); }".into();
        if mode.is_overlay() {
            let overlay = new_overlay();
            overlay.locked().insert(
                key.to_str().unwrap().into(),
                Some(b"overlay-private".to_vec()),
            );
            req.overlay = Some(overlay);
        }
        assert!(execute(&req, ops.clone()).is_err(), "{mode:?}");
        assert_eq!(std::fs::read_to_string(&key).unwrap(), "fixture-secret");
    }
    let profile = f.0.join("profile");
    std::fs::create_dir(&profile).unwrap();
    std::fs::write(profile.join("api_key"), "private profile fixture").unwrap();
    let ops = host(PathPolicy::new(vec![profile], vec![]).unwrap(), false);
    let req = f.request("function run(i) { return fs.list(i.path); }", &f.0);
    assert_eq!(execute(&req, ops).unwrap().output, json!(["private.key"]));
}

#[cfg(unix)]
#[test]
fn approval_window_symlink_retarget_is_denied_before_reading_replacement() {
    use std::os::unix::fs::symlink;
    struct Retarget {
        alias: PathBuf,
        next: PathBuf,
        calls: AtomicU64,
    }
    impl HostOps for Retarget {
        fn approve_sensitive_read(&self, _: &Path, _: &str, _: &str) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::fs::remove_file(&self.alias).unwrap();
            symlink(&self.next, &self.alias).unwrap();
            Ok(())
        }
    }
    let f = Fixture::new();
    let ssh = f.0.join(".ssh");
    std::fs::create_dir(&ssh).unwrap();
    let first = f.0.join("first.txt");
    let second = f.0.join("second.txt");
    std::fs::write(&first, "first-private").unwrap();
    std::fs::write(&second, "replacement-private").unwrap();
    let alias = ssh.join("config");
    symlink(&first, &alias).unwrap();
    let ops = Arc::new(Retarget {
        alias: alias.clone(),
        next: second,
        calls: AtomicU64::new(0),
    });
    let mut req = f.request("function run(i) { return fs.read(i.path); }", &alias);
    req.caps.fs_read = vec![f.0.to_str().unwrap().into()];
    assert!(execute(&req, ops.clone())
        .unwrap_err()
        .to_string()
        .contains("path changed"));
    assert_eq!(ops.calls.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read_to_string(first).unwrap(), "first-private");
}

#[cfg(unix)]
#[test]
fn configured_profile_alias_retarget_protects_both_old_and_new_destinations() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let old = f.0.join("old-profile");
    let new = f.0.join("new-profile");
    for root in [&old, &new] {
        std::fs::create_dir(root).unwrap();
        std::fs::write(root.join("plain.txt"), "profile-private").unwrap();
    }
    let alias = f.0.join("active-profile");
    symlink(&old, &alias).unwrap();
    let policy = PathPolicy::new(vec![alias.clone()], vec![]).unwrap();
    std::fs::remove_file(&alias).unwrap();
    symlink(&new, &alias).unwrap();
    let ops = host(policy, true);
    for path in [
        old.join("plain.txt"),
        new.join("plain.txt"),
        alias.join("plain.txt"),
    ] {
        let mut req = f.request("function run(i) { return fs.read(i.path); }", &path);
        req.caps.fs_read = vec![f.0.to_str().unwrap().into()];
        assert!(execute(&req, ops.clone())
            .unwrap_err()
            .to_string()
            .contains("profile"));
    }
    assert!(ops.paths.locked().is_empty());
}

#[cfg(unix)]
#[test]
fn cat_relative_file_and_sensitive_cwd_cannot_bypass_via_process_write_scope_fallback() {
    let f = Fixture::new();
    let ssh = f.0.join(".ssh");
    std::fs::create_dir(&ssh).unwrap();
    let key = ssh.join("config");
    std::fs::write(&key, "fixture-secret").unwrap();
    let normal = f.0.join("notes.txt");
    std::fs::write(&normal, "project-control").unwrap();
    let mut req = f.request(
        "function run(i) { return process.run('cat',[i.operand],{cwd:i.cwd}).stdout; }",
        &f.0,
    );
    req.caps.process = Some(vec!["cat".into()]);
    req.input["cwd"] = json!(f.0);
    req.input["operand"] = json!("notes.txt");
    let ops = host(PathPolicy::default(), false);
    assert_eq!(
        execute(&req, ops.clone()).unwrap().output,
        json!("project-control")
    );
    assert!(ops.paths.locked().is_empty());
    for operand in [
        json!(".ssh/config"),
        json!(key),
        json!(reqwest::Url::from_file_path(&key).unwrap().as_str()),
    ] {
        req.input["operand"] = operand;
        assert!(execute(&req, ops.clone()).is_err());
    }
    let count = ops.paths.locked().len();
    req.input["cwd"] = json!(ssh);
    req.input["operand"] = json!("config");
    // Both read and write roots contain this cwd. A declined read must not
    // be retried as a write-authorized cwd and then launch the subprocess.
    assert!(execute(&req, ops.clone()).is_err());
    assert_eq!(ops.paths.locked().len(), count + 1);
    let allowed = host(PathPolicy::default(), true);
    assert_eq!(
        execute(&req, allowed.clone()).unwrap().output,
        json!("fixture-secret")
    );
    let exact_identities = vec![ssh.canonicalize().unwrap(), key.canonicalize().unwrap()];
    assert_eq!(
        *allowed.paths.locked(),
        exact_identities,
        "only the actual cwd and file identities need consent; a bare PATH command is not cwd/cat"
    );
    req.code =
        "function run(i) { return process.run(i.command,[i.operand],{cwd:i.cwd}).stdout; }".into();
    req.input["command"] =
        json!(which("cat").expect("private fixture requires the same cat executable used above"));
    assert_eq!(
        execute(&req, allowed.clone()).unwrap().output,
        json!("fixture-secret")
    );
    assert_eq!(
        *allowed.paths.locked(),
        exact_identities
            .iter()
            .chain(&exact_identities)
            .cloned()
            .collect::<Vec<_>>(),
        "an explicit executable path retains both actual scope checks"
    );
    let profile = host(PathPolicy::new(vec![ssh], vec![]).unwrap(), true);
    assert!(execute(&req, profile.clone())
        .unwrap_err()
        .to_string()
        .contains("profile"));
    assert!(profile.paths.locked().is_empty());
}

#[test]
fn accepted_directory_consent_cannot_launder_builtin_descendants_by_rename() {
    let f = Fixture::new();
    for (from_relative, sensitive_relative) in [
        (".ssh", ".ssh"),
        (".kube", ".kube"),
        (".docker", ".docker"),
        (".password-store", ".password-store"),
        (".env-directory", ".env-directory"),
        ("Library", "Library/Keychains"),
        (".local/share", ".local/share/keyrings"),
        ("Microsoft", "Microsoft/Credentials"),
        ("etc/ssl", "etc/ssl/private"),
    ] {
        let from = f.0.join(from_relative);
        let namespace = f.0.join(sensitive_relative);
        std::fs::create_dir_all(&namespace).unwrap();
        let sentinel = namespace.join("config");
        std::fs::write(&sentinel, "unexposed-private-descendant").unwrap();
        let ops = host(PathPolicy::default(), true);
        let mut req = f.request("function run(i) { fs.list(i.namespace); fs.rename(i.path,i.to); return fs.read(i.exposed); }", &from);
        req.caps.fs_read = vec![f.0.to_str().unwrap().into()];
        req.input["namespace"] = json!(namespace);
        let exposed =
            f.0.join("ordinary")
                .join(namespace.strip_prefix(&from).unwrap())
                .join("config");
        req.input["to"] = json!(f.0.join("ordinary"));
        req.input["exposed"] = json!(exposed);
        assert!(
            execute(&req, ops.clone())
                .unwrap_err()
                .to_string()
                .contains("credential-directory classification"),
            "{from_relative}"
        );
        assert_eq!(
            *ops.paths.locked(),
            vec![namespace.canonicalize().unwrap()],
            "the accepted directory consent must remain exact"
        );
        assert_eq!(
            std::fs::read_to_string(&sentinel).unwrap(),
            "unexposed-private-descendant"
        );
        assert!(!exposed.exists());
    }
    // A destination that is sensitive too does not turn directory consent
    // into a grant for all descendant contents.
    let mut cross = f.request(
        "function run(i) { fs.list(i.path); fs.rename(i.path,i.to); }",
        &f.0.join(".ssh"),
    );
    cross.input["to"] = json!(f.0.join(".aws"));
    cross.caps.fs_read = vec![f.0.to_str().unwrap().into()];
    let ops = host(PathPolicy::default(), true);
    assert!(execute(&cross, ops).is_err());
    assert!(!f.0.join(".aws").exists());

    #[cfg(unix)]
    {
        let physical = f.0.join("plain-physical-directory");
        std::fs::create_dir(&physical).unwrap();
        std::fs::write(physical.join("config"), "unexposed-alias-descendant").unwrap();
        let parent = f.0.join("alias-parent");
        std::fs::create_dir(&parent).unwrap();
        let alias = parent.join(".ssh");
        std::os::unix::fs::symlink(&physical, &alias).unwrap();
        let mut req = f.request("function run(i) { fs.list(i.path); fs.rename(i.path,i.to); return fs.read(i.exposed); }", &alias);
        req.caps.fs_read = vec![f.0.to_str().unwrap().into()];
        req.input["to"] = json!(f.0.join("exposed-physical"));
        req.input["exposed"] = json!(f.0.join("exposed-physical/config"));
        let ops = host(PathPolicy::default(), true);
        assert!(execute(&req, ops.clone()).is_err());
        assert_eq!(*ops.paths.locked(), vec![physical.canonicalize().unwrap()]);
        assert!(alias.exists());
        assert_eq!(
            std::fs::read_to_string(physical.join("config")).unwrap(),
            "unexposed-alias-descendant"
        );
        assert!(!f.0.join("exposed-physical").exists());
    }
    // The fixture host has a no-op snapshot callback. Ordinary moves still
    // work, and need neither credential consent nor snapshot configuration.
    let ordinary = f.0.join("ordinary-project");
    std::fs::create_dir(&ordinary).unwrap();
    std::fs::write(ordinary.join("notes.txt"), "ordinary-control").unwrap();
    let ops = host(PathPolicy::default(), false);
    let mut req = f.request(
        "function run(i) { fs.rename(i.path,i.to); return fs.read(i.exposed); }",
        &ordinary,
    );
    req.input["to"] = json!(f.0.join("ordinary"));
    req.input["exposed"] = json!(f.0.join("ordinary/notes.txt"));
    req.caps.fs_read = vec![f.0.to_str().unwrap().into()];
    assert_eq!(
        execute(&req, ops.clone()).unwrap().output,
        json!("ordinary-control")
    );
    assert!(ops.paths.locked().is_empty());
}
