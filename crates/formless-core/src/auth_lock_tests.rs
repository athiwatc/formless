//! Private cross-process lock fixture. No token contents or keychain access.
use super::*;
use std::process::{Child, Command, Stdio};
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
#[ignore = "only spawned by the private refresh-lock cancellation test"]
fn held_lock_child() {
    let path =
        std::env::var_os("FORMLESS_PRIVATE_REFRESH_LOCK_FIXTURE").expect("private fixture path");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .unwrap();
    file.lock().unwrap();
    println!("PRIVATE_REFRESH_LOCK_HELD");
    std::io::stdout().flush().unwrap();
    let mut byte = [0];
    std::io::stdin().read_exact(&mut byte).unwrap();
    drop(file);
}

#[test]
fn cancelled_refresh_lock_wait_has_no_detached_blocker_or_runtime_shutdown_wait() {
    use std::io::BufRead;
    let _env = crate::credential_env::EnvGuard::isolated();
    let root = std::env::temp_dir().join(format!("fl-refresh-cancel-{}", uuid::Uuid::new_v4()));
    let fixture = Fixture(root.clone());
    let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
    let path = paths.config_dir.join(".refresh-private.lock");
    let mut child = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "auth::refresh_lock_tests::held_lock_child",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("FORMLESS_PRIVATE_REFRESH_LOCK_FIXTURE", &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in std::io::BufReader::new(stdout).lines() {
            if line.unwrap().contains("PRIVATE_REFRESH_LOCK_HELD") {
                let _ = ready_tx.send(());
            }
        }
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let worker_paths = paths.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            assert!(tokio::time::timeout(
                Duration::from_millis(60),
                refresh_file_lock(&worker_paths, "private")
            )
            .await
            .is_err())
        });
        // A detached spawn_blocking(File::lock) would make this drop wait
        // until the still-held external lock was released.
        drop(runtime);
        done_tx.send(()).unwrap();
    });
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("cancelled lock wait retained a blocking worker at runtime drop");
    worker.join().unwrap();
    assert!(
        child.0.try_wait().unwrap().is_none(),
        "the fixture still holds the actual kernel lock"
    );
    child.0.stdin.take().unwrap().write_all(b"x").unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(5));
    }
    reader.join().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let owned = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(1), refresh_file_lock(&paths, "private"))
            .await
            .unwrap()
            .unwrap()
    });
    drop(owned);
    drop(runtime);
    drop(child);
    drop(fixture);
}
