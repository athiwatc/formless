//! Profiles are listed from the default profile's directories (SPEC §9.4).

use formless_core::Paths;

#[test]
fn profiles_are_the_default_plus_named_dirs_with_data() {
    let base = std::env::temp_dir().join(format!("fl-prof-{}", uuid::Uuid::new_v4()));
    let default = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
    let work = default.named("work");
    std::fs::create_dir_all(&work.data_dir).unwrap();
    std::fs::write(work.db_file(), "").unwrap();
    let alt = default.named("alt");
    std::fs::create_dir_all(&alt.config_dir).unwrap();
    std::fs::write(alt.config_file(), "").unwrap();
    // Internal sub-directories are not profiles.
    std::fs::create_dir_all(default.data_dir.join("artifacts")).unwrap();

    let names =
        |p: &Paths| -> Vec<String> { p.list_profiles().into_iter().map(|p| p.profile).collect() };
    assert_eq!(names(&default), ["default", "alt", "work"]);
    // Same answer from inside a named profile, and the paths round-trip.
    assert_eq!(names(&work), ["default", "alt", "work"]);
    assert_eq!(work.named("default").data_dir, default.data_dir);
    assert_eq!(work.named("alt").config_dir, alt.config_dir);
    let _ = std::fs::remove_dir_all(base);
}
