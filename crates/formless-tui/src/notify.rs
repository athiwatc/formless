//! Terminal notifications (SPEC §10.6): a bell and, in `desktop` mode, the
//! OSC notification the terminal understands, sent while the TUI isn't
//! focused when a Run finishes, fails or stalls, or needs the user.

/// Escape sequences for one notification. `mode` is `tui.notify`;
/// `term` is `$TERM_PROGRAM` (or a known terminal's marker) and `tmux`
/// says whether to wrap the sequence for tmux passthrough.
pub fn sequence(mode: &str, term: &str, tmux: bool, title: &str, body: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .filter(|c| !c.is_control())
            .collect::<String>()
            .replace(';', ",")
    };
    let (title, body) = (clean(title), clean(body));
    let osc = match mode {
        "desktop" => match term {
            // kitty: OSC 99, title then body.
            "kitty" => format!("\x1b]99;i=1:d=0;{title}\x1b\\\x1b]99;i=1:d=1:p=body;{body}\x1b\\"),
            // urxvt, foot and others: OSC 777.
            "" | "rxvt" | "foot" => format!("\x1b]777;notify;{title};{body}\x1b\\"),
            // iTerm2, WezTerm, Ghostty, Windows Terminal: OSC 9.
            _ => format!("\x1b]9;{title}: {body}\x07"),
        },
        "bell" => String::new(),
        _ => return String::new(),
    };
    let osc = if tmux && !osc.is_empty() {
        // tmux passthrough: ESC P tmux; <seq with ESC doubled> ESC \
        format!("\x1bPtmux;{}\x1b\\", osc.replace('\x1b', "\x1b\x1b"))
    } else {
        osc
    };
    format!("{osc}\x07")
}

/// The terminal, from the environment.
pub fn terminal() -> String {
    if std::env::var_os("WT_SESSION").is_some() {
        return "WindowsTerminal".into();
    }
    if std::env::var_os("KITTY_WINDOW_ID").is_some() {
        return "kitty".into();
    }
    std::env::var("TERM_PROGRAM").unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_terminal_protocol() {
        assert_eq!(sequence("off", "iTerm.app", false, "t", "b"), "");
        assert_eq!(sequence("bell", "iTerm.app", false, "t", "b"), "\x07");
        assert_eq!(
            sequence("desktop", "iTerm.app", false, "Run done", "fix; build"),
            "\x1b]9;Run done: fix, build\x07\x07"
        );
        assert!(sequence("desktop", "kitty", false, "t", "b").starts_with("\x1b]99;"));
        assert!(sequence("desktop", "", false, "t", "b").starts_with("\x1b]777;notify;t;b"));
        let wrapped = sequence("desktop", "WezTerm", true, "t", "b");
        assert!(wrapped.starts_with("\x1bPtmux;\x1b\x1b]9;"), "{wrapped:?}");
        // Control characters can't break out of the sequence.
        assert!(!sequence("desktop", "WezTerm", false, "a\x1b]0;x\x07", "b").contains("\x1b]0;"));
    }
}
