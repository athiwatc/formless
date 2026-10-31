//! Credential isolation is tested in a child with deliberately hostile inherited
//! values, and with parallel callers using different mock credentials.
#[path = "support/env.rs"]
mod credential_env;

#[test]
fn inherited_credentials_are_cleared_and_restored_even_after_panic() {
    const CHILD: &str = "FL_CREDENTIAL_ENV_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "inherited_credentials_are_cleared_and_restored_even_after_panic",
            ])
            .env(CHILD, "1")
            .env("FORMLESS_API_KEY", "hostile-key")
            .env("FORMLESS_API_KEY_FILE", "/nonexistent")
            .env("FORMLESS_API_KEY_LOCAL", "hostile-provider")
            .env("FORMLESS_API_KEY_LOCAL_FILE", "/also-nonexistent")
            .env("FORMLESS_OAUTH_ACCESS_TOKEN", "hostile-token")
            .env("FORMLESS_OAUTH_CLIENT_SECRET", "hostile-secret")
            .env("FL_CREDENTIAL_TEST_OTHER", "before")
            .status()
            .unwrap();
        assert!(status.success(), "credential isolation child failed");
        return;
    }
    let names = [
        "FORMLESS_API_KEY",
        "FORMLESS_API_KEY_FILE",
        "FORMLESS_API_KEY_LOCAL",
        "FORMLESS_API_KEY_LOCAL_FILE",
        "FORMLESS_OAUTH_ACCESS_TOKEN",
        "FORMLESS_OAUTH_CLIENT_SECRET",
        "FL_CREDENTIAL_TEST_OTHER",
    ];
    let before: Vec<_> = names.iter().map(std::env::var_os).collect();
    let result = std::panic::catch_unwind(|| {
        let env = credential_env::EnvGuard::isolated();
        for name in &names[..6] {
            assert!(
                std::env::var_os(name).is_none(),
                "inherited {name} survived"
            );
        }
        env.set("FORMLESS_API_KEY", "test-only");
        env.set("FORMLESS_API_KEY_NEW_PROVIDER_FILE", "/new-test-file");
        env.set("FL_CREDENTIAL_TEST_OTHER", "changed");
        env.set("FL_CREDENTIAL_TEST_NEW", "created");
        env.remove("FL_CREDENTIAL_TEST_OTHER");
        panic!("deliberate guard unwind");
    });
    assert!(result.is_err());
    assert_eq!(
        names.iter().map(std::env::var_os).collect::<Vec<_>>(),
        before
    );
    assert!(std::env::var_os("FORMLESS_API_KEY_NEW_PROVIDER_FILE").is_none());
    assert!(std::env::var_os("FL_CREDENTIAL_TEST_NEW").is_none());
    // A sibling still acquires the lock after the panicking guard poisoned it.
    let _env = credential_env::EnvGuard::isolated();
    assert!(std::env::var_os("FORMLESS_API_KEY").is_none());
}

#[test]
fn parallel_tests_keep_different_keys_until_their_guard_drops() {
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|i| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for round in 0..8 {
                    let env = credential_env::EnvGuard::isolated();
                    assert!(std::env::var_os("FORMLESS_API_KEY").is_none());
                    assert!(std::env::var_os("FORMLESS_API_KEY_LOCAL").is_none());
                    let key = format!("mock-{i}-{round}");
                    env.set("FORMLESS_API_KEY", &key);
                    env.set("FORMLESS_API_KEY_LOCAL", &key);
                    std::thread::yield_now();
                    assert_eq!(std::env::var("FORMLESS_API_KEY").unwrap(), key);
                    assert_eq!(std::env::var("FORMLESS_API_KEY_LOCAL").unwrap(), key);
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn test_environment_mutations_use_the_shared_guard() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let markers = [
        concat!("std::env::", "set_var("),
        concat!("std::env::", "remove_var("),
    ];
    let mut violations = Vec::new();
    // Top-level integration tests and core unit-test sources must route their
    // mutations through support/env.rs. The helper itself is in a subdirectory.
    for dir in [root.join("tests"), root.join("src")] {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|s| s == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                if markers.iter().any(|m| text.contains(m)) {
                    violations.push(path);
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "unguarded environment mutations: {violations:?}"
    );
}
