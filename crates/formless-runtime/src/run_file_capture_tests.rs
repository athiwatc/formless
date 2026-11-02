use super::*;

#[derive(Default)]
struct Capture {
    before: Mutex<Vec<(PathBuf, PathBuf, String)>>,
    after: Mutex<Vec<(PathBuf, PathBuf, String)>>,
    directories: Mutex<Vec<PathBuf>>,
    untracked: AtomicU64,
}
impl HostOps for Capture {
    fn file_changing_resolved(&self, requested: &Path, resolved: &Path, operation: &str) {
        self.before
            .locked()
            .push((requested.into(), resolved.into(), operation.into()));
    }
    fn file_changed(&self, requested: &Path, resolved: &Path, operation: &str) {
        self.after
            .locked()
            .push((requested.into(), resolved.into(), operation.into()));
    }
    fn directory_created(&self, path: &Path) {
        self.directories.locked().push(path.into());
    }
    fn untracked_writes_possible(&self) {
        self.untracked.fetch_add(1, Ordering::SeqCst);
    }
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("fl-runtime-capture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn request(&self, code: &str, mode: HostMode) -> ExecRequest {
        ExecRequest {
            code: code.into(),
            input: json!({"root":self.0}),
            caps: Capabilities::parse(&[
                format!("fs:write:{}", self.0.display()),
                format!("fs:read:{}", self.0.display()),
            ])
            .unwrap(),
            memory_mb: 256,
            mode,
            output_schema: None,
            overlay: None,
            activity: None,
            modules: Default::default(),
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn successful_mutations_report_post_images_and_only_new_directory_ancestors() {
    let f = Fixture::new();
    let ops = Arc::new(Capture::default());
    let req = f.request("function run(i){ fs.mkdir(i.root+'/new/nested'); fs.mkdir(i.root+'/new/nested'); fs.write(i.root+'/other/deep/file','body'); fs.rename(i.root+'/other/deep/file',i.root+'/renamed/file'); fs.remove(i.root+'/renamed/file'); return true; }",HostMode::Live);
    execute(&req, ops.clone()).unwrap();
    let dirs = ops.directories.locked();
    assert_eq!(dirs.len(), 5, "{dirs:?}");
    for relative in ["new", "new/nested", "other", "other/deep", "renamed"] {
        assert_eq!(
            dirs.iter()
                .filter(|path| **path == f.0.join(relative))
                .count(),
            1
        );
    }
    assert_eq!(ops.before.locked().len(), 4); // write, both rename endpoints, remove
    assert_eq!(ops.after.locked().len(), 4);
}

#[test]
fn failed_write_and_overlay_execution_cannot_claim_a_successful_live_change() {
    let f = Fixture::new();
    let ops = Arc::new(Capture::default());
    std::fs::create_dir(f.0.join("directory")).unwrap();
    let req = f.request(
        "function run(i){ fs.write(i.root+'/directory','cannot replace directory'); }",
        HostMode::Live,
    );
    assert!(execute(&req, ops.clone()).is_err());
    assert_eq!(ops.before.locked().len(), 1);
    assert!(ops.after.locked().is_empty());
    std::fs::write(f.0.join("regular-file"), b"keep file").unwrap();
    let mkdir_file = f.request(
        "function run(i){ fs.mkdir(i.root+'/regular-file'); }",
        HostMode::Live,
    );
    assert!(execute(&mkdir_file, ops.clone()).is_err());
    assert_eq!(
        std::fs::read(f.0.join("regular-file")).unwrap(),
        b"keep file"
    );
    let req = f.request("function run(i){ fs.mkdir(i.root+'/overlay'); fs.write(i.root+'/overlay-file','overlay only'); }",HostMode::Replay);
    execute(&req, ops.clone()).unwrap();
    assert!(ops.directories.locked().is_empty());
    assert!(ops.after.locked().is_empty());
    assert!(!f.0.join("overlay").exists());
    assert!(!f.0.join("overlay-file").exists());
}

#[cfg(unix)]
#[test]
fn write_through_symlink_reports_both_the_requested_route_and_actual_target() {
    let f = Fixture::new();
    let target = f.0.join("target");
    let alias = f.0.join("alias");
    std::fs::write(&target, b"before").unwrap();
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    let ops = Arc::new(Capture::default());
    let req = f.request(
        "function run(i){ fs.write(i.root+'/alias','after'); }",
        HostMode::Live,
    );
    execute(&req, ops.clone()).unwrap();
    assert_eq!(
        ops.before.locked()[0],
        (alias.clone(), target.clone(), "write".into())
    );
    assert_eq!(
        ops.after.locked()[0],
        (alias, target.clone(), "write".into())
    );
    assert_eq!(std::fs::read(target).unwrap(), b"after");
}

#[cfg(unix)]
#[test]
fn actual_program_launch_marks_untracked_writes_but_overlay_does_not() {
    let f = Fixture::new();
    let ops = Arc::new(Capture::default());
    let mut req = f.request(
        "function run(){ return process.run('echo',['private fixture']).code; }",
        HostMode::Smoke,
    );
    req.caps = Capabilities::parse(&["process:echo".into()]).unwrap();
    execute(&req, ops.clone()).unwrap();
    assert_eq!(ops.untracked.load(Ordering::SeqCst), 0);
    req.mode = HostMode::Live;
    assert_eq!(execute(&req, ops.clone()).unwrap().output, json!(0));
    assert_eq!(ops.untracked.load(Ordering::SeqCst), 1);
}
