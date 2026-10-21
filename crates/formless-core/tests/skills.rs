//! Skills (SPEC §7.10): SKILL.md parsing, installs from a folder, a zip and
//! git, versioning, trust (installed disabled) and safe file reads.

use formless_core::skills::{has_scripts, install, parse_skill_md, read_file};
use formless_core::{Paths, Store};

fn temp_paths(tag: &str) -> Paths {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-{tag}-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-{tag}-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    paths
}

fn write_skill(dir: &std::path::Path, name: &str, body: &str) {
    std::fs::create_dir_all(dir.join("scripts")).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: >\n  Turn spreadsheets into\n  tidy reports.\n---\n\n{body}\n"),
    )
    .unwrap();
    std::fs::write(dir.join("reference.md"), "extra notes").unwrap();
    std::fs::write(dir.join("scripts/fix.py"), "print('hi')").unwrap();
}

#[test]
fn skill_md_frontmatter_is_parsed_and_validated() {
    let m = parse_skill_md(
        "---\nname: pdf-tools\ndescription: \"Work with PDFs\"\nlicense: MIT\n---\n# Body\n",
    )
    .unwrap();
    assert_eq!(
        (m.name.as_str(), m.description.as_str(), m.body.as_str()),
        ("pdf-tools", "Work with PDFs", "# Body\n")
    );
    assert!(parse_skill_md("no frontmatter").is_err());
    assert!(parse_skill_md("---\nname: Bad Name\ndescription: x\n---\n").is_err());
    assert!(
        parse_skill_md("---\nname: ok\n---\nbody").is_err(),
        "description required"
    );
    assert!(has_scripts(&["scripts/run.sh".into()]) && !has_scripts(&["SKILL.md".into()]));
}

#[test]
fn skills_install_from_folder_zip_and_git_and_version() {
    let paths = temp_paths("skills");
    let store = Store::open(&paths.db_file()).unwrap();
    let src = paths.data_dir.join("src/report");
    write_skill(&src, "report-writer", "Step 1: read the sheet.");

    let v1 = install(&paths, &store, src.to_str().unwrap(), None, None, "user").unwrap();
    assert_eq!(
        (v1.version, v1.enabled, v1.has_scripts),
        (1, false, true),
        "installed disabled"
    );
    assert_eq!(v1.description, "Turn spreadsheets into tidy reports.");
    store.set_skill_enabled("report-writer", true).unwrap();
    assert_eq!(
        read_file(&store, "report-writer", "reference.md").unwrap(),
        "extra notes"
    );
    assert!(
        read_file(&store, "report-writer", "../../../../etc/hosts").is_err(),
        "no escaping the folder"
    );

    // A zip that wraps the skill in a folder: a new version, disabled again.
    write_skill(&src, "report-writer", "Step 1: read the sheet carefully.");
    let zip = paths.data_dir.join("report.zip");
    assert!(std::process::Command::new("zip")
        .args(["-qr", zip.to_str().unwrap(), "report"])
        .current_dir(paths.data_dir.join("src"))
        .status()
        .unwrap()
        .success());
    let v2 = install(&paths, &store, zip.to_str().unwrap(), None, None, "user").unwrap();
    assert_eq!((v2.version, v2.enabled), (2, false));

    // git (a local bare repo stands in for a URL), with a subdir.
    let repo = paths.data_dir.join("work");
    write_skill(&repo.join("skills/git-skill"), "git-skill", "From git.");
    let git = |args: &[&str], dir: &std::path::Path| {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap()
                .status
                .success(),
            "git {args:?}"
        );
    };
    git(&["init", "-q"], &repo);
    git(
        &["-c", "user.email=t@t", "-c", "user.name=t", "add", "."],
        &repo,
    );
    git(
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "skill",
        ],
        &repo,
    );
    let bare = paths.data_dir.join("skills-repo.git");
    git(
        &[
            "clone",
            "-q",
            "--bare",
            repo.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
        &paths.data_dir,
    );
    let g = install(
        &paths,
        &store,
        bare.to_str().unwrap(),
        Some("skills/git-skill"),
        None,
        "user",
    )
    .unwrap();
    assert_eq!(g.name, "git-skill");
    assert_eq!(store.list_skills().unwrap().len(), 2);
    // Removing deletes rows; files are the caller's to delete.
    assert!(!store.remove_skill("git-skill").unwrap().is_empty());
    assert!(store.get_skill("git-skill").unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn skills_are_managed_through_the_daemon() {
    use formless_protocol::Request;
    use std::sync::Arc;
    let paths = temp_paths("skillsd");
    formless_core::Config::default().save(&paths).unwrap();
    let src = paths.data_dir.join("src/notes");
    write_skill(&src, "note-taker", "Write notes.");
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let info = daemon
        .handle(Request::AddSkill {
            source: src.to_string_lossy().into(),
            subdir: None,
            git_ref: None,
        })
        .await
        .unwrap();
    assert_eq!(
        (info["name"].as_str(), info["enabled"].as_bool()),
        (Some("note-taker"), Some(false))
    );
    let got = daemon
        .handle(Request::GetSkill {
            name: "note-taker".into(),
        })
        .await
        .unwrap();
    assert_eq!(got["body"], "Write notes.\n");
    assert_eq!(
        got["files"],
        serde_json::json!(["SKILL.md", "reference.md", "scripts/fix.py"])
    );
    daemon
        .handle(Request::SetSkillEnabled {
            name: "note-taker".into(),
            enabled: true,
        })
        .await
        .unwrap();
    let list = daemon.handle(Request::ListSkills).await.unwrap();
    assert_eq!(list[0]["enabled"], true);
    daemon
        .handle(Request::RemoveSkill {
            name: "note-taker".into(),
        })
        .await
        .unwrap();
    assert!(
        !paths.data_dir.join("skills/note-taker").exists(),
        "files deleted"
    );
    assert_eq!(
        daemon.handle(Request::ListSkills).await.unwrap(),
        serde_json::json!([])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_agent_loads_enabled_skills() {
    use std::sync::Arc;
    let paths = temp_paths("loadskill");
    formless_core::Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let src = paths.data_dir.join("src/report");
    write_skill(&src, "report-writer", "Step 1: read the sheet.");
    install(&paths, &store, src.to_str().unwrap(), None, None, "user").unwrap();
    let session = store.create_session(None).unwrap().id;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let call = |name: &'static str, args: serde_json::Value| {
        let (d, s) = (daemon.clone(), session.clone());
        async move { d.call_tool_direct(&s, name, args).await }
    };
    let off = call("load_skill", serde_json::json!({"name": "report-writer"})).await;
    assert!(
        off.contains("no enabled skill"),
        "disabled skills stay closed: {off}"
    );
    store.set_skill_enabled("report-writer", true).unwrap();
    let loaded: serde_json::Value = serde_json::from_str(
        &call("load_skill", serde_json::json!({"name": "report-writer"})).await,
    )
    .unwrap();
    assert_eq!(loaded["instructions"], "Step 1: read the sheet.\n");
    assert_eq!(
        loaded["files"],
        serde_json::json!(["reference.md", "scripts/fix.py"])
    );
    assert_eq!(store.get_skill("report-writer").unwrap().unwrap().loads, 1);
    // Bundled scripts are listed with how (not) to run them.
    assert_eq!(loaded["scripts"], serde_json::json!(["scripts/fix.py"]));
    assert!(loaded["scripts_note"]
        .as_str()
        .unwrap()
        .contains("never runs them on the host"));
    let file = call(
        "read_skill_file",
        serde_json::json!({"name": "report-writer", "path": "reference.md"}),
    )
    .await;
    assert_eq!(file, "extra notes");
    let found: serde_json::Value = serde_json::from_str(
        &call(
            "find_tools",
            serde_json::json!({"query": "spreadsheets reports"}),
        )
        .await,
    )
    .unwrap();
    assert_eq!(found["skills"][0]["name"], "report-writer");
}

#[test]
fn skills_export_and_import_bundles() {
    use formless_core::skills::{commit, export, prepare_bundle};
    let paths = temp_paths("skbundle");
    let store = Store::open(&paths.db_file()).unwrap();
    let src = paths.data_dir.join("src/report");
    write_skill(&src, "report-writer", "Step 1: read the sheet.");
    install(&paths, &store, src.to_str().unwrap(), None, None, "user").unwrap();
    store.set_skill_enabled("report-writer", true).unwrap();

    let bundle = export(&store, "report-writer").unwrap();
    assert_eq!(bundle["format"], "formless.skill/1");
    assert!(bundle.get("loads").is_none(), "no metrics in a bundle");

    // Into a fresh install: author import, disabled, same files.
    let other = temp_paths("skbundle2");
    let store2 = Store::open(&other.db_file()).unwrap();
    let staged = prepare_bundle(&other, &bundle).unwrap();
    let info = commit(&other, &store2, &staged, "import").unwrap();
    assert_eq!(
        (info.version, info.enabled, info.has_scripts),
        (1, false, true)
    );
    assert_eq!(
        (info.author.as_str(), info.source.as_str()),
        ("import", "import")
    );
    assert_eq!(
        read_file(&store2, "report-writer", "scripts/fix.py").unwrap(),
        "print('hi')"
    );

    // Paths that leave the folder are refused.
    let mut evil = bundle.clone();
    evil["files"][0]["path"] = "../escape.md".into();
    assert!(prepare_bundle(&other, &evil).is_err());
}
