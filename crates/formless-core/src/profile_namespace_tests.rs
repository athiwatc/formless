use super::*;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("fl-profile-namespace-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn base(&self) -> Paths {
        Paths::from_dirs(self.0.join("data"), self.0.join("config")).unwrap()
    }
    fn marked(&self, base: &Paths, name: &str) -> PathBuf {
        let directory = base.config_dir.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("config.toml"), "private fixture config").unwrap();
        directory
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn owned_names_are_reserved_case_insensitively_and_invalid_names_never_resolve() {
    let fixture = Fixture::new();
    let base = fixture.base();
    for reserved in RESERVED_PROFILE_NAMES {
        for name in [reserved.to_string(), reserved.to_uppercase()] {
            assert!(validate_profile_name(&name).is_err(), "{name}");
            assert!(base.named(&name).is_err(), "{name}");
            assert!(!base.data_dir.join(&name).exists());
            assert!(!base.config_dir.join(&name).exists());
        }
    }
    for name in [
        "",
        ".",
        "..",
        "../outside",
        "a/b",
        "a\\b",
        ".hidden",
        "a b",
        "a%",
        "é",
        "Default",
        "CON",
        "aux",
        "com1",
        "LPT9",
    ] {
        assert!(base.named(name).is_err(), "{name:?}");
    }
    assert!(validate_profile_name(&"a".repeat(65)).is_err());
    for name in ["default", "work", "Work-2", "a_b", "9"] {
        assert!(base.named(name).is_ok(), "{name}");
    }
}

#[test]
fn template_directory_is_never_a_profile_and_legacy_markers_are_preserved() {
    let fixture = Fixture::new();
    let base = fixture.base();
    let commands = base.config_dir.join("commands");
    std::fs::create_dir(&commands).unwrap();
    std::fs::write(commands.join("review.md"), "private template").unwrap();
    assert!(
        base.profile_inventory().diagnostics.is_empty(),
        "ordinary templates are not legacy profiles"
    );
    assert!(base.validate_cleanup_namespace().is_ok());
    fixture.marked(&base, "commands");
    std::fs::write(commands.join("api_key"), "private fixture key").unwrap();
    let work = fixture.marked(&base, "work");
    let inventory = base.profile_inventory();
    assert_eq!(
        inventory
            .profiles
            .iter()
            .map(|p| p.profile.as_str())
            .collect::<Vec<_>>(),
        ["default", "work"]
    );
    assert!(inventory
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("commands")
            && diagnostic.contains("preserved")
            && diagnostic.contains("Back up")));
    assert!(
        base.validate_cleanup_namespace().is_err(),
        "a parent cleanup cannot erase skipped legacy state"
    );
    assert!(base.named("commands").is_err());
    assert_eq!(
        std::fs::read_to_string(commands.join("review.md")).unwrap(),
        "private template"
    );
    assert_eq!(
        std::fs::read_to_string(commands.join("api_key")).unwrap(),
        "private fixture key"
    );
    assert!(work.join("config.toml").exists());
}

#[test]
fn discovery_never_duplicates_default_or_selects_case_ambiguous_legacy_profiles() {
    let fixture = Fixture::new();
    let base = fixture.base();
    fixture.marked(&base, "default");
    fixture.marked(&base, ".legacy");
    let upper = fixture.marked(&base, "Work");
    // On case-insensitive filesystems a distinct second directory cannot exist;
    // selecting the differently cased spelling must still fail everywhere.
    assert!(base.named("work").is_err());
    #[cfg(not(windows))]
    {
        let lower = base.config_dir.join("work");
        if !lower.exists() {
            fixture.marked(&base, "work");
        }
    }
    let inventory = base.profile_inventory();
    assert_eq!(
        inventory
            .profiles
            .iter()
            .filter(|p| p.profile == "default")
            .count(),
        1
    );
    assert!(!inventory.profiles.iter().any(|p| p.profile == ".legacy"));
    assert!(inventory
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("default")));
    assert!(upper.join("config.toml").exists());
}

#[test]
fn nested_and_shared_default_roots_cannot_be_selected_or_contained_by_named_cleanup() {
    let fixture = Fixture::new();
    for (data, config, name) in [
        (
            fixture.0.join("one"),
            fixture.0.join("one").join("custom"),
            "custom",
        ),
        (
            fixture.0.join("two").join("custom"),
            fixture.0.join("two"),
            "custom",
        ),
        (
            fixture.0.join("three"),
            fixture.0.join("three").join("custom").join("nested"),
            "custom",
        ),
    ] {
        let base = Paths::from_dirs(data, config).unwrap();
        std::fs::write(base.config_file(), "private default config").unwrap();
        assert!(base
            .named(name)
            .unwrap_err()
            .to_string()
            .contains("overlaps"));
        assert!(base.named("work").is_ok());
        if base.config_dir.parent() == Some(base.data_dir.as_path()) {
            assert!(
                base.profile_inventory().diagnostics.is_empty(),
                "a real nested default root isn't an accidental profile"
            );
        }
        assert_eq!(
            std::fs::read_to_string(base.config_file()).unwrap(),
            "private default config"
        );
    }
    let root = fixture.0.join("shared");
    let shared = Paths::from_dirs(root.clone(), root).unwrap();
    fixture.marked(&shared, "work");
    assert_eq!(
        shared
            .list_profiles()
            .iter()
            .map(|p| p.profile.as_str())
            .collect::<Vec<_>>(),
        ["default", "work"]
    );
    assert!(shared.named("commands").is_err());
}

#[test]
fn marker_probe_errors_are_not_confused_with_absent_legacy_state() {
    use std::io::{Error, ErrorKind};
    for denied in [ErrorKind::PermissionDenied, ErrorKind::Other] {
        let mut calls = 0;
        let error = profile_marker_with(Path::new("private-directory"), |_| {
            calls += 1;
            Err(Error::from(if calls == 1 {
                ErrorKind::NotFound
            } else {
                denied
            }))
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("config.toml"));
        assert_eq!(calls, 2);
    }
    assert!(
        !profile_marker_with(Path::new("private-directory"), |_| Err(
            std::io::Error::from(std::io::ErrorKind::NotFound)
        ))
        .unwrap()
    );
}

#[test]
fn forged_reserved_paths_cannot_reset_even_under_a_valid_family_freeze() {
    let fixture = Fixture::new();
    let base = fixture.base();
    let commands = fixture.marked(&base, "commands");
    std::fs::write(commands.join("review.md"), "private default template").unwrap();
    std::fs::write(base.key_fallback_file(), "private default key").unwrap();
    let forged = Paths {
        data_dir: base.data_dir.join("commands"),
        config_dir: commands.clone(),
        profile: "commands".into(),
    };
    let gate =
        crate::profile_lock::ProfileLock::maintenance(&base, std::time::Duration::ZERO).unwrap();
    let guard = gate.freeze().unwrap();
    let error = crate::config::reset_all_guarded(&forged, &guard).unwrap_err();
    assert!(error.to_string().contains("reserved profile"));
    assert!(
        crate::config::reset_all_guarded(&base, &guard).is_err(),
        "default cleanup preserves an ambiguous legacy profile"
    );
    assert_eq!(
        std::fs::read_to_string(commands.join("review.md")).unwrap(),
        "private default template"
    );
    assert_eq!(
        std::fs::read_to_string(base.key_fallback_file()).unwrap(),
        "private default key"
    );
}

#[cfg(unix)]
#[test]
fn discovery_and_resolution_preserve_linked_profiles_and_canonical_root_aliases() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let base = fixture.base();
    let outside = fixture.0.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("config.toml"), "outside sentinel").unwrap();
    symlink(&outside, base.config_dir.join("legacy")).unwrap();
    let inventory = base.profile_inventory();
    assert_eq!(inventory.profiles.len(), 1);
    assert!(inventory
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.contains("linked")));
    assert!(base.named("legacy").is_err());
    assert!(base.validate_cleanup_namespace().is_err());
    assert_eq!(
        std::fs::read_to_string(outside.join("config.toml")).unwrap(),
        "outside sentinel"
    );
    let alias = fixture.0.join("alias");
    symlink(&base.data_dir, &alias).unwrap();
    let nested = Paths {
        data_dir: alias,
        config_dir: base.data_dir.join("custom"),
        profile: "default".into(),
    };
    assert!(
        nested.named("custom").is_err(),
        "canonical root aliases cannot hide a collision"
    );
}
