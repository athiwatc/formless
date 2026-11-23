use super::*;

#[test]
fn every_topic_has_build_matched_bundled_content_including_docker() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (name, path, _) in TOPICS {
        let bundled = text(Some(name)).unwrap();
        assert!(!bundled.is_empty(), "{name}");
        let (file, anchor) = path
            .split_once('#')
            .map_or((*path, None), |(file, anchor)| (file, Some(anchor)));
        let source = std::fs::read_to_string(root.join(file)).unwrap();
        if anchor.is_none() {
            assert_eq!(bundled, source, "{name}");
        } else {
            assert!(source.contains(bundled));
            assert!(bundled.starts_with("## Are you asked?"));
        }
    }
    assert_eq!(
        text(None).unwrap(),
        include_str!("../../../docs/gallery.md")
    );
    assert_eq!(
        text(Some(" DOCK ")).unwrap(),
        include_str!("../../../docs/docker.md")
    );
    assert_eq!(text(Some("spe")).unwrap(), include_str!("../../../SPEC.md"));
    assert!(matches!(
        text(Some("s")),
        Err(TextError::Topic(TopicError::Ambiguous { .. }))
    ));
}

#[test]
fn extracted_guides_have_named_bundled_topics() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (name, path) in [
        ("usage", "docs/usage.md"),
        ("cli", "docs/cli.md"),
        ("architecture", "docs/architecture.md"),
    ] {
        assert!(TOPICS.iter().any(|(topic, _, _)| *topic == name), "{name}");
        assert_eq!(
            text(Some(name)).unwrap(),
            std::fs::read_to_string(root.join(path)).unwrap()
        );
    }
}

#[test]
fn sections_follow_parsed_heading_structure_and_original_utf8_offsets() {
    let source="# Intro\n```md\n## Target guide\n```\n\n## Target [guide](relative.md)\nไทย body\n### Child\ncontent\n> # quoted example\n\n    # indented example\n\n## Next\nother\n";
    let expected="## Target [guide](relative.md)\nไทย body\n### Child\ncontent\n> # quoted example\n\n    # indented example\n\n";
    assert_eq!(section(source, "target-guide").unwrap(), expected);
    assert_eq!(
        section("Title\n-----\nbody\n\nNext\n====\nother", "title").unwrap(),
        "Title\n-----\nbody\n\n"
    );
    assert_eq!(
        section("## A &amp; B\nbody", "a--b").unwrap(),
        "## A &amp; B\nbody"
    );
    assert!(matches!(
        section("# Same\na\n# Same\nb", "same"),
        Err(TextError::DuplicateAnchor(_))
    ));
    assert!(matches!(
        section("```md\n# Fake\n```\n", "fake"),
        Err(TextError::MissingAnchor(_))
    ));
}

#[test]
fn explicit_text_never_calls_an_opener_and_failed_openers_return_local_text() {
    let explicit = present_with(Some("jobs"), true, |_| {
        panic!("text must not launch an opener")
    })
    .unwrap();
    assert_eq!(explicit.text, Some(include_str!("../../../docs/jobs.md")));
    let fallback = present_with(Some("skills"), false, |_| false).unwrap();
    assert_eq!(fallback.text, Some(include_str!("../../../docs/skills.md")));
    assert!(fallback.url.ends_with("docs/skills.md"));
    assert!(present_with(None, false, |_| true).unwrap().text.is_none());
    assert!(matches!(
        present_with(Some("nope"), false, |_| panic!("bad topic must not open")),
        Err(TextError::Topic(_))
    ));
}

#[test]
fn slash_text_flag_is_exact_and_keeps_existing_topic_mistakes() {
    for (input, topic, flag) in [
        ("--text jobs", "jobs", true),
        ("jobs --text", "jobs", true),
        ("--text", "", true),
        (" jobs extra ", "jobs extra", false),
        ("jobs --textual", "jobs --textual", false),
    ] {
        assert_eq!(text_option(input), (topic.into(), flag));
    }
}
