use super::*;

#[test]
fn signed_header_like_content_inside_a_hunk_is_counted_and_colored() {
    let text = "--- a/f\n+++ b/f\n@@ -1,2 +1 @@\n----\n--- note\n+++i;\n";
    assert_eq!(diff_stat(text), (1, 2));
    let lines = diff_lines(text, true);
    for (index, color) in [
        (0, Color::White),
        (1, Color::White),
        (3, Color::Red),
        (4, Color::Red),
        (5, Color::Green),
    ] {
        assert_eq!(lines[index][0].style.fg, Some(color));
        assert_eq!(lines[index][0].content, text.lines().nth(index).unwrap());
    }
}

#[test]
fn multiple_files_hunks_empty_sides_and_no_newline_markers_have_shared_roles() {
    let text="diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -0,0 +1,2 @@ function\n+++ literal\n+second\n\\ No newline at end of file\n@@ -1,2 +0,0 @@\n--- literal\n-second\n\\ No newline at end of file\n--- a/g\n+++ b/g\n@@ -1 +1 @@\n-old\n+new\n";
    assert_eq!(diff_stat(text), (3, 3));
    use DiffKind::*;
    let kinds: Vec<_> = diff_classification::classify(text)
        .map(|(_, kind)| kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            Plain, Header, Header, Hunk, Added, Added, Plain, Hunk, Removed, Removed, Plain,
            Header, Header, Hunk, Removed, Added
        ]
    );
    let rendered = diff_lines(text, true);
    assert_eq!(
        rendered
            .iter()
            .map(|line| line[0].content.as_ref())
            .collect::<Vec<_>>(),
        text.lines().collect::<Vec<_>>()
    );
}

#[test]
fn malformed_and_incomplete_fragments_use_explicit_bounded_fallback() {
    use DiffKind::*;
    for (text, expected, stats) in [
        (
            "@@ malformed\n--- a/f\n+++ b/f\n+fragment\n-fragment",
            vec![Hunk, Header, Header, Added, Removed],
            (1, 1),
        ),
        ("@@ -1,2 +1,2 @@\n--- partial", vec![Hunk, Removed], (0, 1)),
        (
            "@@ -1 +1 @@\n unexpected context\n--- a/f\n+++ b/f",
            vec![Hunk, Plain, Header, Header],
            (0, 0),
        ),
        (
            "@@ -1,0 +1,1 @@\n+valid\n+++ ambiguous\n-extra",
            vec![Hunk, Added, Header, Removed],
            (1, 1),
        ),
        (
            "@@ -999999999999999999999999999999999999 +1 @@\n--- header",
            vec![Hunk, Header],
            (0, 0),
        ),
        (
            "@@ -1,2 +1,2 @@\n-removed\nnot a patch row\n--- header",
            vec![Hunk, Removed, Plain, Header],
            (0, 1),
        ),
    ] {
        assert_eq!(
            diff_classification::classify(text)
                .map(|(_, kind)| kind)
                .collect::<Vec<_>>(),
            expected,
            "{text}"
        );
        assert_eq!(diff_stat(text), stats, "{text}");
    }
}

#[test]
fn context_collapse_consumes_hidden_rows_before_classifying_later_headers() {
    let mut text = "--- a/f\n+++ b/f\n@@ -1,11 +1,10 @@\n".to_string();
    for n in 0..10 {
        text.push_str(&format!(" context {n}\n"));
    }
    text.push_str("--- removed\n--- a/next\n+++ b/next\n");
    assert_eq!(diff_stat(&text), (0, 1));
    let rendered = diff_lines(&text, true);
    assert!(rendered
        .iter()
        .any(|line| line[0].content == " ⋯ 4 unchanged lines"));
    for (line, color) in [
        ("--- removed", Color::Red),
        ("--- a/next", Color::White),
        ("+++ b/next", Color::White),
    ] {
        assert_eq!(
            rendered.iter().find(|row| row[0].content == line).unwrap()[0]
                .style
                .fg,
            Some(color)
        );
    }
    let _theme = crate::theme::test_theme("colorblind");
    let colored = diff_lines("@@ -1 +1 @@\n--- ไทย\n+++ 日本語", true);
    let plain = diff_lines("@@ -1 +1 @@\n--- ไทย\n+++ 日本語", false);
    for (index, tone) in [
        (1, crate::SemanticTone::Bad),
        (2, crate::SemanticTone::Good),
    ] {
        let (r, g, b) = crate::colorblind_rgb(tone);
        assert_eq!(colored[index][0].style.fg, Some(Color::Rgb(r, g, b)));
        assert_eq!(colored[index][0].content, plain[index][0].content);
        assert_eq!(plain[index][0].style.fg, None);
        assert!(plain[index][0].style.add_modifier.contains(Modifier::BOLD));
    }
}
