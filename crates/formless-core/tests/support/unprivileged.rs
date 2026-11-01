// Run real permission-denial assertions without root's DAC override. Shared by
// core unit and integration fixtures; no production policy/account is changed.
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn run(test_name: &str, body: impl FnOnce()) {
    // Use the public Unix temporary root even when root's inherited TMPDIR is
    // private. Only a fresh dummy directory becomes readable/traversable. The
    // real checkout/target can remain 0700, and no host account is looked up.
    let fixture = Fixture(
        PathBuf::from("/tmp").join(format!("fl-permission-fixture-{}", uuid::Uuid::new_v4())),
    );
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&fixture.0)
        .unwrap();
    let uid = std::fs::metadata(&fixture.0).unwrap().uid();
    if uid != 0 {
        drop(fixture);
        body();
        if let Ok(child_name) = std::env::var("FORMLESS_PERMISSION_FIXTURE_CHILD") {
            assert_eq!(child_name, test_name);
            println!(
                "{}",
                std::env::var("FORMLESS_PERMISSION_FIXTURE_COMPLETED").unwrap()
            );
        }
        return;
    }
    assert!(
        std::env::var_os("FORMLESS_PERMISSION_FIXTURE_CHILD").is_none(),
        "permission child still has uid 0; refusing recursive spawn"
    );
    let executable = fixture.0.join("permission-test");
    std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o555)).unwrap();
    std::fs::set_permissions(&fixture.0, std::fs::Permissions::from_mode(0o755)).unwrap();
    // An inherited output descriptor needs no filesystem write privilege in
    // the child. Keeping output in a file also avoids pipe-buffer deadlocks.
    let log = fixture.0.join("output");
    let output = std::fs::File::create(&log).unwrap();
    let completed = format!("permission-fixture-completed:{}", uuid::Uuid::new_v4());
    let mut child = Process(
        Command::new(executable)
            .args(["--exact", test_name, "--test-threads=1", "--nocapture"])
            .env("FORMLESS_PERMISSION_FIXTURE_CHILD", test_name)
            .env("FORMLESS_PERMISSION_FIXTURE_COMPLETED", &completed)
            .env("TMPDIR", "/tmp")
            .current_dir(&fixture.0)
            // uid() clears supplementary groups. Failure to set either ID is
            // a spawn/test failure, never a skip or privileged fallback.
            .gid(65534)
            .uid(65534)
            .stdin(Stdio::null())
            .stdout(output.try_clone().unwrap())
            .stderr(output)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            let output = std::fs::read_to_string(&log).unwrap();
            assert!(
                status.success(),
                "permission child {test_name}: {status}\n{output}"
            );
            // A typo in the exact harness selector must not pass as zero tests.
            assert!(
                output.contains(&completed),
                "fixture never completed: {output}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "permission child {test_name} exceeded 30 seconds"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
