//! Profiles are listed from the default profile's directories (SPEC §9.4).

use formless_core::Paths;

#[test]
fn profiles_are_the_default_plus_named_dirs_with_data() {
    let base = std::env::temp_dir().join(format!("fl-prof-{}", uuid::Uuid::new_v4()));
    let default = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
    let work = default.named("work").unwrap();
    std::fs::create_dir_all(&work.data_dir).unwrap();
    std::fs::write(work.db_file(), "").unwrap();
    let alt = default.named("alt").unwrap();
    std::fs::create_dir_all(&alt.config_dir).unwrap();
    std::fs::write(alt.config_file(), "").unwrap();
    // Internal sub-directories are not profiles.
    std::fs::create_dir_all(default.data_dir.join("artifacts")).unwrap();

    let names =
        |p: &Paths| -> Vec<String> { p.list_profiles().into_iter().map(|p| p.profile).collect() };
    assert_eq!(names(&default), ["default", "alt", "work"]);
    // Same answer from inside a named profile, and the paths round-trip.
    assert_eq!(names(&work), ["default", "alt", "work"]);
    assert_eq!(work.named("default").unwrap().data_dir, default.data_dir);
    assert_eq!(work.named("alt").unwrap().config_dir, alt.config_dir);
    let _ = std::fs::remove_dir_all(base);
}

#[path = "support/env.rs"]
mod credential_env;

#[test]
fn profile_resolution_refuses_collisions_before_creating_any_selected_directories() {
    let environment = credential_env::EnvGuard::isolated();
    let root = std::env::temp_dir().join(format!("fl-profile-resolve-{}", uuid::Uuid::new_v4()));
    let data = root.join("data");
    let config = root.join("config");
    environment.set("FORMLESS_DATA_DIR", &data);
    environment.set("FORMLESS_CONFIG_DIR", &config);
    for name in [
        "commands",
        "Commands",
        "../outside",
        "api_key",
        "review-objects",
    ] {
        assert!(Paths::resolve_read_only(Some(name)).is_err());
        assert!(Paths::resolve(Some(name)).is_err());
        assert!(!root.exists(), "invalid profile resolution created state");
    }
    // The single-override layout shares config and data, so both namespaces apply.
    environment.remove("FORMLESS_CONFIG_DIR");
    assert!(Paths::resolve(Some("skills")).is_err());
    assert!(!root.exists());
    let paths = Paths::resolve_read_only(Some("work")).unwrap();
    assert_eq!(paths.data_dir, data.join("work"));
    assert_eq!(paths.config_dir, data.join("work"));
    assert!(!root.exists(), "read-only resolution must remain read-only");
}
