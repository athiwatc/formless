//! Skills (SPEC §7.10): SKILL.md parsing, installs from a folder, a zip and
//! git, versioning, trust (installed disabled) and safe file reads.

use formless_core::skills::{has_scripts, install, parse_skill_md, read_file, validate_name};
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
    assert!(parse_skill_md("---anything\nname: ok\ndescription: x\n---\n").is_err());
    assert!(parse_skill_md("---\nname: ok\ndescription: x\n---anything\n").is_err());
    assert!(
        parse_skill_md("---\nname: ok\n---\nbody").is_err(),
        "description required"
    );
    let literal = parse_skill_md(
        "---\nname: pdf-tools\ndescription: |\n  before\n  ---\n  after\n---\n# Body",
    )
    .unwrap();
    assert_eq!(literal.description, "before\n---\nafter\n");
    assert_eq!(literal.body, "# Body");
    assert!(has_scripts(&["scripts/run.sh".into()]) && !has_scripts(&["SKILL.md".into()]));
}

#[test]
fn skill_scalar_quotes_follow_yaml_semantics() {
    for (value, expected) in [
        (r#"say "hello""#, r#"say "hello""#),
        (r#"hi" there"#, r#"hi" there"#),
        ("users'", "users'"),
        (r#""Work with PDFs""#, "Work with PDFs"),
        (r#""say \"hi\"""#, r#"say "hi""#),
        (r#""C:\\work\\file""#, r"C:\work\file"),
        ("'It''s useful'", "It's useful"),
        ("'\"hello\"'", "\"hello\""),
    ] {
        let source = format!("---\nname: pdf-tools\ndescription: {value}\n---\n# Body\n");
        let meta = parse_skill_md(&source).unwrap();
        assert_eq!(meta.description, expected, "{value}");
        assert_eq!(meta.body, "# Body\n");
    }
    assert!(parse_skill_md("---\nname: pdf-tools\ndescription: \"hi\" there\n---\n").is_err());
    let meta = parse_skill_md(
        "---\nname: pdf-tools\ndescription: |-\n  \"literal quoted block\"\n---\nBody",
    )
    .unwrap();
    assert_eq!(meta.description, "\"literal quoted block\"");
}

#[test]
fn skill_frontmatter_follows_yaml_scalar_semantics() {
    for (indicator, expected) in [
        ("|", "First\nSecond\n"),
        ("|-", "First\nSecond"),
        ("|+", "First\nSecond\n"),
        ("|2", "First\nSecond\n"),
        ("|2-", "First\nSecond"),
        ("|-2", "First\nSecond"),
        ("|+2", "First\nSecond\n"),
        ("|2+", "First\nSecond\n"),
        (">", "First Second\n"),
        (">-", "First Second"),
        (">+", "First Second\n"),
        ("> # note", "First Second\n"),
        (">2-", "First Second"),
        (">-2", "First Second"),
        (">+2", "First Second\n"),
        (">2+", "First Second\n"),
    ] {
        let source =
            format!("---\nname: pdf-tools\ndescription: {indicator}\n  First\n  Second\n---\nBody");
        assert_eq!(
            parse_skill_md(&source).unwrap().description,
            expected,
            "{indicator}"
        );
    }
    let kept =
        parse_skill_md("---\nname: pdf-tools\ndescription: |+\n  First\n\n---\nBody").unwrap();
    assert_eq!(kept.description, "First\n\n");
    let folded_kept =
        parse_skill_md("---\nname: pdf-tools\ndescription: >+\n  First\n\n---\nBody").unwrap();
    assert_eq!(folded_kept.description, "First\n\n");

    let plain = parse_skill_md(
        "---\nname: pdf-tools\ndescription: Work with PDFs\n  across multiple lines.\n---\nBody",
    )
    .unwrap();
    assert_eq!(plain.description, "Work with PDFs across multiple lines.");

    let escaped =
        parse_skill_md("---\nname: pdf-tools\ndescription: \"Say: \\\"hi\\\"\\nnext\"\n---\nBody")
            .unwrap();
    assert_eq!(escaped.description, "Say: \"hi\"\nnext");
    let yaml_escape =
        parse_skill_md("---\nname: pdf-tools\ndescription: \"Letter: \\x41\"\n---\nBody").unwrap();
    assert_eq!(yaml_escape.description, "Letter: A");

    let crlf =
        parse_skill_md("---\r\nname: pdf-tools\r\ndescription: CRLF fences\r\n---\r\n# Body\r\n")
            .unwrap();
    assert_eq!(crlf.description, "CRLF fences");
    assert_eq!(crlf.body, "# Body\r\n");

    let example = parse_skill_md(
        "---\nname: expense-report\ndescription: File and validate reports.\nlicense: Apache-2.0\ncompatibility: Requires python3\nmetadata:\n  author: example-org\n  version: \"2.1\"\nallowed-tools: Bash(git status:*)\n---\n# Expense reports",
    )
    .unwrap();
    assert_eq!(example.name, "expense-report");
    assert_eq!(example.description, "File and validate reports.");
}

#[test]
fn skill_frontmatter_errors_name_duplicate_keys_and_source_lines() {
    for (key, line, source) in [
        (
            "name",
            3,
            "---\nname: first\nname: second\ndescription: d\n---\nBody",
        ),
        (
            "description",
            4,
            "---\nname: skill\ndescription: first\ndescription: second\n---\nBody",
        ),
        (
            "license",
            5,
            "---\nname: skill\ndescription: d\nlicense: MIT\nlicense: Apache-2.0\n---\nBody",
        ),
    ] {
        let error = parse_skill_md(source).unwrap_err().to_string();
        assert!(
            error.contains(&format!("duplicate frontmatter key '{key}'")),
            "{error}"
        );
        assert!(error.contains(&format!("on line {line}")), "{error}");
    }

    let error = parse_skill_md("---\nname: skill\ndescription: [unterminated\n---\nBody")
        .unwrap_err()
        .to_string();
    assert!(error.contains("on line 3"), "{error}");
}

#[test]
fn skill_frontmatter_field_errors_include_source_lines() {
    for (source, line) in [
        ("---\nname: Bad Name\ndescription: d\n---\nBody", 2),
        ("---\nname: skill\ndescription: \"\"\n---\nBody", 3),
        ("---\ndescription: d\n---\nBody", 3),
        ("---\nname: skill\n---\nBody", 3),
        ("---\nname: 42\ndescription: d\n---\nBody", 2),
        ("---\nname: true\ndescription: d\n---\nBody", 2),
        ("---\nname: null\ndescription: d\n---\nBody", 2),
        ("---\nname: skill\ndescription: 42\n---\nBody", 3),
        ("---\nname: skill\ndescription: false\n---\nBody", 3),
        ("---\nname: skill\ndescription: null\n---\nBody", 3),
    ] {
        let error = parse_skill_md(source).unwrap_err().to_string();
        assert!(error.contains(&format!("line {line}")), "{error}");
    }

    let too_long = format!(
        "---\nname: skill\ndescription: {}\n---\nBody",
        "d".repeat(1025)
    );
    let error = parse_skill_md(&too_long).unwrap_err().to_string();
    assert!(error.contains("line 3"), "{error}");
}

#[test]
fn skill_names_follow_agent_skills_rules_and_match_source_directory() {
    for name in ["-leading", "trailing-", "double--hyphen"] {
        assert!(validate_name(name).is_err(), "{name}");
    }
    for name in ["pdf-tools", "a", "a-1"] {
        validate_name(name).unwrap();
    }
    let too_long = "d".repeat(1025);
    assert!(parse_skill_md(&format!(
        "---\nname: skill\ndescription: {too_long}\n---\nBody"
    ))
    .is_err());

    let (_fixture, paths) = temp_paths("skill-name-directory");
    let store = Store::open(&paths.db_file()).unwrap();
    let src = paths.data_dir.join("src/directory-name");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("SKILL.md"),
        "---\nname: metadata-name\ndescription: d\n---\nBody",
    )
    .unwrap();
    let error = install(&paths, &store, src.to_str().unwrap(), None, None, "user")
        .unwrap_err()
        .to_string();
    assert!(error.contains("metadata-name"), "{error}");
    assert!(error.contains("directory-name"), "{error}");
    assert!(store.list_skills().unwrap().is_empty());
}

#[test]
fn skill_scalar_comments_leave_quoted_hashes_and_block_content_intact() {
    for (value, expected) in [
        ("Work with PDFs # annotation", "Work with PDFs"),
        (r#""Work with PDFs" # annotation"#, "Work with PDFs"),
        ("'It''s # useful' # annotation", "It's # useful"),
        (r#""Say \"hi\" # text" # annotation"#, "Say \"hi\" # text"),
        (
            "https://example.test/#anchor # annotation",
            "https://example.test/#anchor",
        ),
        ("C# helpers # annotation", "C# helpers"),
        ("Bob's notes # annotation", "Bob's notes"),
        (
            "|- # header annotation\n  literal # content",
            "literal # content",
        ),
    ] {
        let source=format!("---\n# name: ignored\nname: 'pdf-tools' # primary\ndescription: {value}\n---\n# Body # unchanged\n");
        let meta = parse_skill_md(&source).unwrap();
        assert_eq!(meta.name, "pdf-tools");
        assert_eq!(meta.description, expected, "{value}");
        assert_eq!(meta.body, "# Body # unchanged\n");
    }
}

#[test]
fn skill_scalar_metadata_survives_disabled_installation() {
    let (_fixture, paths) = temp_paths("skill-scalars");
    let store = Store::open(&paths.db_file()).unwrap();
    let src = paths.data_dir.join("src/pdf-tools");
    std::fs::create_dir_all(&src).unwrap();
    let source="---\nname: pdf-tools # primary\ndescription: \"say \\\"hi\\\"\" # annotation\n---\n# Body\n";
    std::fs::write(src.join("SKILL.md"), source).unwrap();
    let installed = install(&paths, &store, src.to_str().unwrap(), None, None, "user").unwrap();
    assert_eq!(installed.description, "say \"hi\"");
    assert!(!installed.enabled);
    let saved = store.get_skill("pdf-tools").unwrap().unwrap();
    assert_eq!(saved.description, "say \"hi\"");
    assert!(!saved.enabled);
}

#[test]
fn skills_install_from_folder_zip_and_git_and_version() {
    let (_fixture, paths) = temp_paths("skills");
    let store = Store::open(&paths.db_file()).unwrap();
    let src = paths.data_dir.join("src/report-writer");
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
    let report = paths.data_dir.join("src/report-writer");
    let mut entries = Vec::new();
    for rel in formless_core::skills::files(&report).unwrap() {
        entries.push((
            format!("report-writer/{rel}"),
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
    // A local path always copies working files, including .git directories.
    // An explicit file URL requests Git cloning; uppercase also exercises the
    // real normalized transport lookup without a remote/network dependency.
    let source = reqwest::Url::from_directory_path(&bare)
        .unwrap()
        .to_string()
        .replacen("file:", "FILE:", 1);
    let g = install(
        &paths,
        &store,
        &source,
        Some("skills/git-skill"),
        None,
        "user",
    )
    .unwrap();
    assert_eq!(g.name, "git-skill");
    assert_eq!(g.source, source);
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
    let src = paths.data_dir.join("src/note-taker");
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
    std::fs::write(
        paths.data_dir.join("skills/note-taker/v1/SKILL.md"),
        "---\nname: note-taker\ndescription: \"hi\" there\n---\nWrite notes.\n",
    )
    .unwrap();
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
    let src = paths.data_dir.join("src/report-writer");
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
    std::fs::write(
        paths.data_dir.join("skills/report-writer/v1/SKILL.md"),
        "---\nname: report-writer\ndescription: \"hi\" there\n---\nStep 1: read the sheet.\n",
    )
    .unwrap();
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
    let before_manual = store.get_skill("report-writer").unwrap().unwrap();
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
    let after_manual = store.get_skill("report-writer").unwrap().unwrap();
    assert_eq!(after_manual.loads, 2);
    assert_eq!(after_manual.runs, before_manual.runs);
    let original = formless_protocol::Request::LoadSkill {
        session_id: "missing-session".into(),
        name: "report-writer".into(),
        args: None,
    };
    assert!(daemon
        .handle(original)
        .await
        .unwrap_err()
        .to_string()
        .contains("session not found"));
    assert_eq!(store.get_skill("report-writer").unwrap().unwrap().loads, 2);
    store.set_skill_enabled("report-writer", false).unwrap();
    for (name, message) in [
        ("report-writer", "disabled"),
        ("missing-skill", "no skill named"),
    ] {
        let error = daemon
            .handle(formless_protocol::Request::LoadSkill {
                session_id: session.clone(),
                name: name.into(),
                args: None,
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains(message));
    }
}

#[test]
fn skills_export_and_import_bundles() {
    use formless_core::skills::{commit, export, prepare_bundle};
    let (_fixture, paths) = temp_paths("skbundle");
    let store = Store::open(&paths.db_file()).unwrap();
    let src = paths.data_dir.join("src/report-writer");
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
    let updated = install_text(&paths, &store, &md("bad-skill"), "auto", "auto").unwrap();
    assert_eq!(updated.version, 2);
    assert!(!updated.retired && updated.retire_reason.is_none());
}

#[tokio::test]
async fn manual_loads_and_legacy_blank_run_ids_do_not_trigger_unhelpful_retirement() {
    use formless_core::skills::install_text;
    let (_fixture, paths) = temp_paths("manual-retire");
    let store = Store::open(&paths.db_file()).unwrap();
    let name = "manual-skill";
    install_text(
        &paths,
        &store,
        "---\nname: manual-skill\ndescription: d\n---\nbody\n",
        "auto",
        "auto",
    )
    .unwrap();
    store.set_skill_enabled(name, true).unwrap();
    let session = store.create_session(None).unwrap().id;
    let raw = rusqlite::Connection::open(paths.db_file()).unwrap();
    raw.execute(
        "INSERT INTO skill_loads(skill_id,session_id,run_id,created_at) SELECT id,?1,'','2000-01-01T00:00:00Z' FROM skills WHERE name=?2",
        rusqlite::params![session, name],
    ).unwrap();
    let daemon = std::sync::Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let load = || formless_protocol::Request::LoadSkill {
        session_id: session.clone(),
        name: name.into(),
        args: None,
    };
    assert_eq!(daemon.handle(load()).await.unwrap()["loaded"], name);
    let skill = store.get_skill(name).unwrap().unwrap();
    assert_eq!((skill.loads, skill.runs, skill.runs_completed), (2, 0, 0));
    assert!(store.retire_auto_skills(None, 0.9, 1).unwrap().is_empty());
    assert_eq!(
        raw.query_row(
            "SELECT COUNT(*) FROM skill_loads WHERE run_id IS NULL",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(
        store
            .retire_auto_skills(Some("2999-01-01T00:00:00Z"), 0.0, 1)
            .unwrap(),
        vec![name]
    );
    assert!(daemon
        .handle(load())
        .await
        .unwrap_err()
        .to_string()
        .contains("is retired"));
    assert_eq!(store.get_skill(name).unwrap().unwrap().loads, 2);
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

#[test]
fn local_skill_directory_with_git_suffix_is_not_treated_as_a_git_url() {
    let (_fixture, paths) = temp_paths("skill-git-name");
    let store = Store::open(&paths.db_file()).unwrap();
    let src = paths.data_dir.join("vendor/notes.git");
    write_skill(&src, "git-named-folder", "Local working files.");
    // The directory is intentionally not a Git repository. Both spellings
    // must read its actual files rather than invoking clone.
    for source in [
        src.to_string_lossy().into_owned(),
        format!("{}/", src.display()),
    ] {
        let error = install(&paths, &store, &source, None, None, "user")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("does not match source directory 'notes.git'"),
            "{error}"
        );
    }
}
