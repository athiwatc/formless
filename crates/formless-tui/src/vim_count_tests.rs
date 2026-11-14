//! Counted edits and last-line ranges use the actual shared input buffer.
use super::*;

fn normal(text: &str) -> (Vim, InputLine) {
    let mut line = InputLine::from(text);
    line.set_modal_cursor(0);
    (
        Vim {
            mode: Mode::Normal,
            ..Vim::default()
        },
        line,
    )
}

fn keys(vim: &mut Vim, line: &mut InputLine, text: &str, surface: Surface) {
    for c in text.chars() {
        let code = if c == '\u{1b}' {
            KeyCode::Esc
        } else {
            KeyCode::Char(c)
        };
        assert_eq!(
            vim.key(line, KeyEvent::new(code, KeyModifiers::NONE), surface),
            Outcome::Handled,
            "key {c:?} on {surface:?}"
        );
    }
}

#[test]
fn counted_insert_entries_never_interpret_typed_edits_as_normal_commands() {
    for surface in [Surface::Composer, Surface::Editor] {
        for (command, expected) in [
            ('i', "dede base\n tail"),
            ('a', " dedebase\n tail"),
            ('I', " dedebase\n tail"),
            ('A', " basedede\n tail"),
            ('o', " base\nde\nde\n tail"),
            ('O', "de\nde\n base\n tail"),
        ] {
            let (mut vim, mut line) = normal(" base\n tail");
            keys(&mut vim, &mut line, &format!("2{command}"), surface);
            assert_eq!(vim.mode, Mode::Insert, "{command} {surface:?}");
            assert!(vim.pending.is_empty());
            keys(&mut vim, &mut line, "de\u{1b}", surface);
            assert_eq!(&*line, expected, "{command} {surface:?}");
            assert_eq!(vim.mode, Mode::Normal);
        }
    }
}

#[test]
fn counted_puts_repeat_character_and_linewise_registers_including_eof_yanks() {
    for surface in [Surface::Composer, Surface::Editor] {
        for (text, command, expected) in [
            ("one\ntwo", "yy2p", "one\none\none\ntwo"),
            ("one\ntwo", "Gyy2p", "one\ntwo\ntwo\ntwo"),
            ("one\ntwo", "Gyygg2P", "two\ntwo\none\ntwo"),
            ("ab cd", "yiw2p", "aababb cd"),
            ("ab cd", "yiw2P", "ababab cd"),
            ("👩‍💻x", "x2p", "x👩‍💻👩‍💻"),
        ] {
            let (mut vim, mut line) = normal(text);
            keys(&mut vim, &mut line, command, surface);
            assert_eq!(&*line, expected, "{command} {surface:?}");
            assert_eq!(vim.mode, Mode::Normal);
            assert!(vim.pending.is_empty());
        }
    }
}

#[test]
fn dot_count_replaces_last_count_and_plain_dot_keeps_it() {
    for surface in [Surface::Composer, Surface::Editor] {
        for (command, expected) in [
            ("x3.", "efghijklmno"),
            ("x3..", "hijklmno"),
            ("x3.1..", "ghijklmno"),
            ("3x.", "ghijklmno"),
        ] {
            let (mut vim, mut line) = normal("abcdefghijklmno");
            keys(&mut vim, &mut line, command, surface);
            assert_eq!(&*line, expected, "{command} {surface:?}");
        }
        let (mut vim, mut line) = normal("one\ntwo");
        keys(&mut vim, &mut line, "yy2p3.", surface);
        assert_eq!(&*line, "one\none\none\none\none\none\ntwo");
        for (command, expected) in [
            ("2ihi\u{1b}0.", "hihihihitail"),
            ("2ihi\u{1b}01.", "hihihitail"),
        ] {
            let (mut vim, mut line) = normal("tail");
            keys(&mut vim, &mut line, command, surface);
            assert_eq!(&*line, expected, "{command} {surface:?}");
            assert_eq!(vim.mode, Mode::Normal);
        }
    }
}

#[test]
fn counted_open_lines_repeat_even_without_inserted_text() {
    for surface in [Surface::Composer, Surface::Editor] {
        for (command, expected) in [
            ("2o\u{1b}", "one\n\n"),
            ("2O\u{1b}", "\n\none"),
            ("2o\u{1b}.", "one\n\n\n\n"),
        ] {
            let (mut vim, mut line) = normal("one");
            keys(&mut vim, &mut line, command, surface);
            assert_eq!(&*line, expected, "{command} {surface:?}");
            assert_eq!(vim.mode, Mode::Normal);
        }
    }
}

#[test]
fn counted_changes_select_counted_range_but_insert_replacement_once() {
    for surface in [Surface::Composer, Surface::Editor] {
        let (mut vim, mut line) = normal("one two three four");
        keys(&mut vim, &mut line, "2cwZ\u{1b}w.", surface);
        assert_eq!(&*line, "Z Z");
        assert_eq!(vim.mode, Mode::Normal);
    }
}

#[test]
fn insert_count_is_bounded_and_escape_cancels_pending_count() {
    for surface in [Surface::Composer, Surface::Editor] {
        let (mut vim, mut line) = normal("tail");
        keys(&mut vim, &mut line, "9999ix\u{1b}", surface);
        assert_eq!(&*line, format!("{}tail", "x".repeat(999)));
        let (mut vim, mut line) = normal("tail");
        keys(&mut vim, &mut line, "3\u{1b}iZ\u{1b}", surface);
        assert_eq!(&*line, "Ztail");
        assert!(vim.pending.is_empty());
        // An empty counted put is still a no-op, preserving the last edit.
        keys(&mut vim, &mut line, "2p$.", surface);
        assert_eq!(&*line, "ZtaiZl");
    }
}

#[test]
fn counted_insert_replays_unicode_paste_as_literal_edits_on_both_surfaces() {
    for surface in [Surface::Composer, Surface::Editor] {
        let (mut vim, mut line) = normal("tail");
        line.set_literal(true);
        keys(&mut vim, &mut line, "2i", surface);
        vim.paste(&mut line, "de\ne\u{301}👩‍💻");
        keys(&mut vim, &mut line, "\u{1b}", surface);
        assert_eq!(&*line, "de\ne\u{301}👩‍💻de\ne\u{301}👩‍💻tail");
        assert!(line.is_literal());
        assert_eq!(vim.mode, Mode::Normal);
    }
}

#[test]
fn final_line_deletes_preserve_yanks_and_choose_first_nonblank_cursor() {
    for surface in [Surface::Composer, Surface::Editor] {
        for (text, command, expected, register, cursor) in [
            ("one\ntwo", "Gdd", "one", "two", 0),
            ("one\ntwo\nthree", "jdG", "one", "two\nthree", 0),
            ("one\ntwo\nthree", "Gdk", "one", "two\nthree", 0),
            ("  one\ntwo\nthree", "j2dd", "  one", "two\nthree", 2),
            ("one\ntwo", "2dd", "", "one\ntwo", 0),
            ("e\u{301} 👩‍💻\ntwo", "Gdd", "e\u{301} 👩‍💻", "two", 0),
            ("  👩‍💻\nlast", "Gdd", "  👩‍💻", "last", 2),
        ] {
            let (mut vim, mut line) = normal(text);
            keys(&mut vim, &mut line, command, surface);
            assert_eq!(&*line, expected, "{command} {surface:?}");
            assert_eq!(line.cursor_byte(), cursor, "{command} {surface:?}");
            assert_eq!(vim.register, register);
            assert!(vim.register_linewise);
        }
        let (mut vim, mut line) = normal("one\ntwo");
        keys(&mut vim, &mut line, "Gddp", surface);
        assert_eq!(
            &*line, "one\ntwo",
            "delete/put restores the exact last line"
        );
    }
}

#[test]
fn ending_at_final_lf_does_not_always_mean_deleting_the_final_empty_line() {
    for surface in [Surface::Composer, Surface::Editor] {
        for (text, command, expected, register) in [
            ("one\ntwo\n", "jdd", "one\n", "two\n"),
            ("one\ntwo\n", "Gdd", "one\ntwo", "\n"),
            ("one\ntwo\n", "j2dd", "one", "two\n"),
            ("one\ntwo\n", "Gdk", "one", "two\n"),
            ("one\n\n", "Gdd", "one\n", "\n"),
            ("one\n\n", "jdd", "one\n", "\n"),
        ] {
            let (mut vim, mut line) = normal(text);
            keys(&mut vim, &mut line, command, surface);
            assert_eq!(&*line, expected, "{text:?} {command} {surface:?}");
            assert_eq!(vim.register, register);
        }
    }
}

#[test]
fn final_line_change_and_yank_keep_their_original_ranges() {
    for surface in [Surface::Composer, Surface::Editor] {
        for (command, expected) in [
            ("GccX\u{1b}", "one\nX"),
            ("jcGX\u{1b}", "one\nX"),
            ("Gyy", "one\ntwo"),
            ("jyG", "one\ntwo"),
        ] {
            let (mut vim, mut line) = normal("one\ntwo");
            keys(&mut vim, &mut line, command, surface);
            assert_eq!(&*line, expected, "{command} {surface:?}");
            assert_eq!(vim.register, "two");
            assert!(vim.register_linewise);
        }
    }
}

#[test]
fn counted_puts_keep_empty_lines_in_the_register() {
    for surface in [Surface::Composer, Surface::Editor] {
        let (mut vim, mut line) = normal("one\ntwo\n");
        keys(&mut vim, &mut line, "Gdd2p", surface);
        assert_eq!(&*line, "one\ntwo\n\n");
        assert_eq!(vim.register, "\n");
        let (mut vim, mut line) = normal("tail");
        vim.register = "a\n\n".into();
        vim.register_linewise = true;
        keys(&mut vim, &mut line, "2p", surface);
        assert_eq!(&*line, "tail\na\n\na\n");
    }
}

#[test]
fn empty_buffer_delete_preserves_private_register_contents_and_kind() {
    for surface in [Surface::Composer, Surface::Editor] {
        let (mut vim, mut line) = normal("");
        vim.register = "kept".into();
        vim.register_linewise = false;
        keys(&mut vim, &mut line, "2dd", surface);
        assert!(line.is_empty());
        assert_eq!(vim.register, "kept");
        assert!(!vim.register_linewise);
    }
}

#[test]
fn final_line_register_put_before_next_line_preserves_separators_and_undo() {
    for surface in [Surface::Composer, Surface::Editor] {
        for (source, copy, put, expected) in [
            ("x\ny\nlast", "Gyy", "ggp", "x\nlast\ny\nlast"),
            ("x\ny\nlast", "Gdd", "ggp", "x\nlast\ny"),
            ("x\ny\nlast", "Gyy", "ggP", "last\nx\ny\nlast"),
            ("x\ny\nlast", "Gyy", "Gp", "x\ny\nlast\nlast"),
            ("a\n\ny\nz", "2yy", "ggjp", "a\n\na\n\ny\nz"),
            ("a\n\n\ny\nz", "3yy", "ggjjp", "a\n\n\na\n\n\ny\nz"),
        ] {
            let (mut vim, mut line) = normal(source);
            keys(&mut vim, &mut line, copy, surface);
            let register = vim.register.clone();
            let literal = vim.register_literal;
            keys(&mut vim, &mut line, &put[..put.len() - 1], surface);
            let before = line.to_string();
            keys(&mut vim, &mut line, &put[put.len() - 1..], surface);
            assert_eq!(&*line, expected, "{copy}/{put} {surface:?}");
            assert_eq!(vim.register, register);
            assert_eq!(vim.register_literal, literal);
            assert!(vim.register_linewise);
            assert!(line.undo());
            assert_eq!(&*line, before);
            assert_eq!(vim.register, register);
        }
    }
}

#[test]
fn eof_linewise_put_retains_copied_blank_lines_without_rewriting_the_register() {
    for surface in [Surface::Composer, Surface::Editor] {
        for (source, copy, put, expected) in [
            ("a\n\nz", "2yy", "Gp", "a\n\nz\na\n"),
            ("a\n\n\nz", "3yy", "Gp", "a\n\n\nz\na\n\n"),
            ("a\nz", "yy", "Gp", "a\nz\na"),
            ("\n\nz", "2yy", "Gp", "\n\nz\n\n"),
            ("\nz", "yy", "Gp", "\nz\n"),
            ("x\ny\nlast", "Gyy", "Gp", "x\ny\nlast\nlast"),
            ("a\n\nz", "2yy", "G2p", "a\n\nz\na\n\na\n"),
            ("a\n\nz", "2yy", "ggP", "a\n\na\n\nz"),
            ("a\n\ny\nz", "2yy", "ggjp", "a\n\na\n\ny\nz"),
        ] {
            let (mut vim, mut line) = normal(source);
            keys(&mut vim, &mut line, copy, surface);
            let register = vim.register.clone();
            let literal = vim.register_literal;
            keys(&mut vim, &mut line, &put[..put.len() - 1], surface);
            let before = line.to_string();
            keys(&mut vim, &mut line, &put[put.len() - 1..], surface);
            assert_eq!(&*line, expected, "{copy}/{put} {surface:?}");
            assert_eq!(vim.register, register);
            assert_eq!(vim.register_literal, literal);
            assert!(vim.register_linewise);
            assert!(line.undo());
            assert_eq!(&*line, before);
            assert_eq!(vim.register, register);
        }
    }
}
