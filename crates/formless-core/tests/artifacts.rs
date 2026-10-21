//! Artifact retention (SPEC §9.6, §15).

use formless_core::artifacts::{gc, remove_for_session, usage, Collected};
use formless_core::config::ArtifactsCfg;
use formless_core::Store;

fn file(dir: &std::path::Path, name: &str, bytes: usize) -> String {
    let p = dir.join(name);
    std::fs::write(&p, vec![b'x'; bytes]).unwrap();
    p.to_string_lossy().into_owned()
}

#[test]
fn retention_removes_old_then_oldest_over_the_cap_and_session_deletes_its_own() {
    let dir = std::env::temp_dir().join(format!("fl-art-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Store::open(&dir.join("db.sqlite")).unwrap();
    let s = store.create_session(None).unwrap();

    let a = file(&dir, "a", 600_000);
    let b = file(&dir, "b", 600_000);
    let c = file(&dir, "c", 600_000);
    let mine = file(&dir, "mine", 10);
    for p in [&a, &b, &c] {
        store.save_artifact(None, p, "", 600_000).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    store.save_artifact(Some(&s.id), &mine, "", 10).unwrap();

    // Nothing is old; 1.8 MB over a 1 MB cap: the two oldest go.
    let cfg = ArtifactsCfg {
        max_total_mb: 1,
        max_age_days: 30,
    };
    assert_eq!(
        gc(&store, &cfg).unwrap(),
        Collected {
            files: 2,
            bytes: 1_200_000
        }
    );
    assert!(!std::path::Path::new(&a).exists() && !std::path::Path::new(&b).exists());
    assert!(std::path::Path::new(&c).exists());
    assert_eq!(usage(&store).unwrap(), (2, 600_010));
    assert_eq!(
        gc(&store, &cfg).unwrap(),
        Collected::default(),
        "now within caps"
    );

    // Deleting a Session takes its artifacts with it.
    remove_for_session(&store, &s.id).unwrap();
    assert!(!std::path::Path::new(&mine).exists());
    assert_eq!(usage(&store).unwrap(), (1, 600_000));

    // An artifact past max_age_days goes even when under the size cap.
    rusqlite::Connection::open(dir.join("db.sqlite"))
        .unwrap()
        .execute(
            "UPDATE artifacts SET created_at='2000-01-01T00:00:00+00:00' WHERE path=?1",
            [&c],
        )
        .unwrap();
    let roomy = ArtifactsCfg {
        max_total_mb: 1024,
        max_age_days: 30,
    };
    assert_eq!(gc(&store, &roomy).unwrap().files, 1);
    assert!(!std::path::Path::new(&c).exists());

    // max_age_days = 0 and max_total_mb = 0 disable the limits.
    let off = ArtifactsCfg {
        max_total_mb: 0,
        max_age_days: 0,
    };
    assert_eq!(gc(&store, &off).unwrap(), Collected::default());
}
