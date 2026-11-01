//! Skills (SPEC §7.10): SKILL.md parsing, installs from a folder, a zip and
//! git, versioning, trust (installed disabled) and safe file reads.

use formless_core::skills::{has_scripts, install, parse_skill_md, read_file};
use formless_core::{Paths, Store};

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_paths(tag: &str) -> (Fixture, Paths) {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::env::temp_dir().join(format!("fl-{tag}-{short}"));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&base).unwrap();
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-{tag}-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    (Fixture(base), paths)
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
    let (_fixture, paths) = temp_paths("skills");
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
    let report = paths.data_dir.join("src/report");
    let mut entries = Vec::new();
    for rel in formless_core::skills::files(&report).unwrap() {
        entries.push((
            format!("report/{rel}"),
            std::fs::read(report.join(&rel)).unwrap(),
        ));
    }
    write_zip(&zip, &entries);
    let v2 = install(&paths, &store, zip.to_str().unwrap(), None, None, "user").unwrap();
    assert_eq!((v2.version, v2.enabled), (2, false));
    assert_eq!(
        read_file(&store, "report-writer", "SKILL.md").unwrap(),
        std::fs::read_to_string(src.join("SKILL.md")).unwrap(),
        "zip install must contain the updated version's actual files"
    );

    // git (a local bare repo stands in for a URL), with a subdir.
    let repo = paths.data_dir.join("work");
    write_skill(&repo.join("skills/git-skill"), "git-skill", "From git.");
    let git = |args: &[&str], dir: &std::path::Path| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} in {}: {}\nstdout: {}\nstderr: {}",
            dir.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
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
    assert_eq!((g.version, g.enabled, g.has_scripts), (1, false, true));
    assert_eq!(
        read_file(&store, "git-skill", "reference.md").unwrap(),
        "extra notes"
    );
    assert_eq!(
        read_file(&store, "git-skill", "scripts/fix.py").unwrap(),
        "print('hi')"
    );
    assert_eq!(store.list_skills().unwrap().len(), 2);
    // Removing deletes rows; files are the caller's to delete.
    assert!(!store.remove_skill("git-skill").unwrap().is_empty());
    assert!(store.get_skill("git-skill").unwrap().is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn skills_are_managed_through_the_daemon() {
    use formless_protocol::Request;
    use std::sync::Arc;
    let (_fixture, paths) = temp_paths("skillsd");
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
    let (_fixture, paths) = temp_paths("loadskill");
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
    // The user loads it by hand: a note in the chat, counted as a load.
    daemon
        .handle(formless_protocol::Request::LoadSkill {
            session_id: session.clone(),
            name: "report-writer".into(),
            args: Some("Q3 sheet".into()),
        })
        .await
        .unwrap();
    let note = store
        .list_messages(&session)
        .unwrap()
        .into_iter()
        .rev()
        .find(|m| m.role == "system-note")
        .unwrap();
    assert!(
        note.content.contains("Step 1: read the sheet.")
            && note.content.contains("Arguments: Q3 sheet")
    );
    assert_eq!(store.get_skill("report-writer").unwrap().unwrap().loads, 2);
}

#[test]
fn skills_export_and_import_bundles() {
    use formless_core::skills::{commit, export, prepare_bundle};
    let (_fixture, paths) = temp_paths("skbundle");
    let store = Store::open(&paths.db_file()).unwrap();
    let src = paths.data_dir.join("src/report");
    write_skill(&src, "report-writer", "Step 1: read the sheet.");
    install(&paths, &store, src.to_str().unwrap(), None, None, "user").unwrap();
    store.set_skill_enabled("report-writer", true).unwrap();

    let bundle = export(&store, "report-writer").unwrap();
    assert_eq!(bundle["format"], "formless.skill/1");
    assert!(bundle.get("loads").is_none(), "no metrics in a bundle");

    // Into a fresh install: author import, disabled, same files.
    let (_other_fixture, other) = temp_paths("skbundle2");
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

#[test]
fn distilled_skills_parse_and_install_as_auto() {
    use formless_core::skills::{distil_input, install_text, parse_distilled};
    assert!(parse_distilled("NONE").is_none());
    assert!(parse_distilled("Sure! here is a skill").is_none());
    let reply = "```markdown\n---\nname: csv-cleanup\ndescription: Clean a CSV before analysis.\n---\n# CSV cleanup\n## Steps\n1. Read it.\n```";
    let (meta, text) = parse_distilled(reply).unwrap();
    assert_eq!(meta.name, "csv-cleanup");
    assert!(text.starts_with("---\nname: csv-cleanup") && !text.contains("```"));

    let calls = vec![(
        "read_file".to_string(),
        "{\"path\":\"x\"}".to_string(),
        "success".to_string(),
    )];
    let input = distil_input("tidy the sheet", &calls, "done", &[]);
    assert!(input.contains("1. read_file [success]") && input.contains("(none)"));

    let (_fixture, paths) = temp_paths("autoskill");
    let store = Store::open(&paths.db_file()).unwrap();
    let info = install_text(&paths, &store, &text, "auto:run r1", "auto").unwrap();
    assert_eq!((info.author.as_str(), info.version), ("auto", 1));
    let again = install_text(&paths, &store, &text, "auto:run r2", "auto").unwrap();
    assert_eq!(again.version, 2, "a repeat becomes a new version");
}

#[test]
fn auto_skills_are_measured_and_retired() {
    use formless_core::skills::install_text;
    let (_fixture, paths) = temp_paths("retire");
    let store = Store::open(&paths.db_file()).unwrap();
    let md = |n: &str| format!("---\nname: {n}\ndescription: d\n---\nbody\n");
    install_text(&paths, &store, &md("idle-skill"), "auto", "auto").unwrap();
    install_text(&paths, &store, &md("bad-skill"), "auto", "auto").unwrap();
    install_text(&paths, &store, &md("user-skill"), "x", "user").unwrap();
    for n in ["idle-skill", "bad-skill", "user-skill"] {
        store.set_skill_enabled(n, true).unwrap();
    }
    // bad-skill: loaded in 2 Runs, one completed, one interrupted.
    let session = store.create_session(None).unwrap().id;
    for status in ["completed", "interrupted"] {
        let run = store.create_run(&session).unwrap();
        store.set_run_status(&run, status).unwrap();
        store
            .record_skill_load("bad-skill", &session, &run)
            .unwrap();
        assert!(store.run_loaded_skill(&run).unwrap());
    }
    let bad = store.get_skill("bad-skill").unwrap().unwrap();
    assert_eq!((bad.loads, bad.runs, bad.runs_completed), (2, 2, 1));

    // Nothing is idle yet against an old cutoff; 50% < 60% retires bad-skill.
    let retired = store
        .retire_auto_skills(Some("2000-01-01T00:00:00+00:00"), 0.6, 2)
        .unwrap();
    assert_eq!(retired, vec!["bad-skill".to_string()]);
    let bad = store.get_skill("bad-skill").unwrap().unwrap();
    assert_eq!(
        bad.retire_reason.as_deref(),
        Some("unhelpful: 1 of 2 Runs that loaded it completed")
    );
    // A cutoff in the future: idle-skill was never loaded; user skills are
    // never auto-retired.
    let retired = store
        .retire_auto_skills(Some("2999-01-01T00:00:00+00:00"), 0.0, 2)
        .unwrap();
    assert_eq!(retired, vec!["idle-skill".to_string()]);
    let idle = store.get_skill("idle-skill").unwrap().unwrap();
    assert!(idle.retired && !idle.enabled);
    assert!(idle.retire_reason.as_deref().unwrap().starts_with("unused"));
    // Retired skills are restorable.
    store.set_skill_enabled("idle-skill", true).unwrap();
    let restored = store.get_skill("idle-skill").unwrap().unwrap();
    assert!(!restored.retired && restored.retire_reason.is_none());
}

#[test]
fn run_tool_sequences_are_compared_across_sessions() {
    let (_fixture, paths) = temp_paths("toolseq");
    let store = Store::open(&paths.db_file()).unwrap();
    let caps = formless_runtime::Capabilities::parse(&[]).unwrap();
    let mut ids = Vec::new();
    for n in ["fetch_page", "summarize"] {
        let (t, v) = store
            .create_tool_version(
                n,
                "d",
                "{}",
                "{}",
                "function run(i){return i;}",
                &caps,
                64,
                "agent",
            )
            .unwrap();
        ids.push((t.id, v));
    }
    let run_in = |session: &str| {
        let run = store.create_run(session).unwrap();
        for (id, v) in &ids {
            store
                .start_invocation(id, *v, Some(session), Some(&run), None, "{}")
                .unwrap();
        }
        store.set_run_status(&run, "completed").unwrap();
        run
    };
    let (a, b) = (
        store.create_session(None).unwrap().id,
        store.create_session(None).unwrap().id,
    );
    let run_b = run_in(&b);
    let seq: Vec<String> = store
        .run_invocations(&run_b)
        .unwrap()
        .into_iter()
        .map(|(n, _, _)| n)
        .collect();
    assert_eq!(seq, ["fetch_page", "summarize"]);
    assert!(!store.tool_sequence_seen_elsewhere(&b, &seq).unwrap());
    run_in(&a);
    assert!(store.tool_sequence_seen_elsewhere(&b, &seq).unwrap());
}

/// A zip of `(name, bytes)` entries, deflated, written in-process.
fn write_zip(path: &std::path::Path, entries: &[(String, Vec<u8>)]) {
    use std::io::Write;
    let mut w = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in entries {
        w.start_file(name.as_str(), opts).unwrap();
        w.write_all(bytes).unwrap();
    }
    w.finish().unwrap();
}

/// Archives are extracted in-process with caps enforced while writing:
/// a zip bomb, too many entries, escaping paths and links are refused by
/// name and leave nothing behind (#856).
#[test]
fn hostile_skill_zips_are_refused_and_leave_nothing() {
    use std::io::Write;
    let (_fixture, paths) = temp_paths("zipbomb");
    let store = Store::open(&paths.db_file()).unwrap();
    let skill_md = b"---\nname: bomb\ndescription: d\n---\nbody".to_vec();
    let staging = paths.data_dir.join("skills/.staging");
    let refused = |zip: &std::path::Path, want: &str| {
        let err = install(&paths, &store, zip.to_str().unwrap(), None, None, "user")
            .err()
            .unwrap_or_else(|| panic!("{} installed", zip.display()));
        assert!(format!("{err:#}").contains(want), "{err:#}");
        let left = std::fs::read_dir(&staging).map(|d| d.count()).unwrap_or(0);
        assert_eq!(left, 0, "staging removed");
        assert!(store
            .list_skills()
            .unwrap()
            .iter()
            .all(|s| s.name != "bomb"));
    };

    // 11 MB of zeros compresses to a few KB.
    let bomb = paths.data_dir.join("bomb.zip");
    write_zip(
        &bomb,
        &[
            ("SKILL.md".into(), skill_md.clone()),
            ("zeros.bin".into(), vec![0u8; 11_000_000]),
        ],
    );
    assert!(std::fs::metadata(&bomb).unwrap().len() < 100_000);
    refused(&bomb, "more than 10 MB");

    let many = paths.data_dir.join("many.zip");
    let mut entries = vec![("SKILL.md".to_string(), skill_md.clone())];
    entries.extend((0..2001).map(|i| (format!("f{i}.txt"), Vec::new())));
    write_zip(&many, &entries);
    refused(&many, "at most 2000");

    let dots = paths.data_dir.join("dots.zip");
    write_zip(
        &dots,
        &[
            ("SKILL.md".into(), skill_md.clone()),
            ("../escape.txt".into(), b"x".to_vec()),
        ],
    );
    refused(&dots, "'../escape.txt' would be written outside");
    assert!(!paths.data_dir.join("skills/escape.txt").exists());

    let abs = paths.data_dir.join("abs.zip");
    write_zip(
        &abs,
        &[
            ("SKILL.md".into(), skill_md.clone()),
            ("/tmp/fl-abs-escape.txt".into(), b"x".to_vec()),
        ],
    );
    refused(&abs, "would be written outside");

    let link = paths.data_dir.join("link.zip");
    let mut w = zip::ZipWriter::new(std::fs::File::create(&link).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    w.start_file("SKILL.md", opts).unwrap();
    w.write_all(&skill_md).unwrap();
    w.add_symlink("hosts", "/etc/hosts", opts).unwrap();
    w.finish().unwrap();
    refused(&link, "'hosts' is a symbolic link");
}
