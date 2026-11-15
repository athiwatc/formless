//! Terminal notifications (SPEC §10.6): a bell and, in `desktop` mode, the
//! OSC notification the terminal understands, sent while the TUI isn't
//! focused when a Run finishes, fails or stalls, or needs the user.

/// Escape sequences for one notification. `mode` is `tui.notify`;
/// `term` is the selected terminal identity and `tmux`
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
            // Only known OSC 9 terminals select that protocol.
            "iTerm.app" | "WezTerm" | "Ghostty" | "WindowsTerminal" => {
                format!("\x1b]9;{title}: {body}\x07")
            }
            // urxvt, foot and unknown terminals use the legacy 777 fallback.
            _ => format!("\x1b]777;notify;{title};{body}\x1b\\"),
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

/// The terminal, from environment markers. Multiplexers are not terminal
/// identities; no external command is run while delivering a notification.
pub fn terminal() -> String {
    terminal_from(
        &std::env::var("TERM_PROGRAM").unwrap_or_default(),
        &std::env::var("TERM").unwrap_or_default(),
        &std::env::var("LC_TERMINAL").unwrap_or_default(),
        std::env::var_os("WT_SESSION").is_some(),
        std::env::var_os("KITTY_WINDOW_ID").is_some(),
    )
    .to_string()
}

fn known_terminal(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "kitty" => Some("kitty"),
        "iterm.app" | "iterm2" => Some("iTerm.app"),
        "wezterm" => Some("WezTerm"),
        "ghostty" => Some("Ghostty"),
        "windowsterminal" => Some("WindowsTerminal"),
        "foot" | "footclient" => Some("foot"),
        "rxvt" | "urxvt" => Some("rxvt"),
        _ => None,
    }
}

fn terminal_from(
    program: &str,
    term: &str,
    outer: &str,
    windows: bool,
    kitty: bool,
) -> &'static str {
    if windows {
        return "WindowsTerminal";
    }
    if kitty {
        return "kitty";
    }
    if let Some(known) = known_terminal(program).or_else(|| known_terminal(outer)) {
        return known;
    }
    let term = term.to_ascii_lowercase();
    if term == "foot" || term.starts_with("foot-") {
        "foot"
    } else if term == "rxvt"
        || term.starts_with("rxvt-")
        || term == "urxvt"
        || term.starts_with("urxvt-")
    {
        "rxvt"
    } else if term == "xterm-kitty" {
        "kitty"
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_selection_reaches_existing_protocols_without_commands() {
        for (program, term, outer, windows, kitty, expected) in [
            ("tmux", "foot", "", false, false, "foot"),
            ("screen", "rxvt-unicode-256color", "", false, false, "rxvt"),
            ("vscode", "foot-direct", "", false, false, "foot"),
            (
                "tmux",
                "screen-256color",
                "iTerm2",
                false,
                false,
                "iTerm.app",
            ),
            (
                "WezTerm",
                "xterm-256color",
                "iTerm2",
                false,
                false,
                "WezTerm",
            ),
            ("tmux", "screen-256color", "", false, true, "kitty"),
            ("tmux", "screen-256color", "", true, true, "WindowsTerminal"),
            ("", "xterm-kitty", "", false, false, "kitty"),
            ("unknown", "screen-256color", "unknown", false, false, ""),
        ] {
            let selected = terminal_from(program, term, outer, windows, kitty);
            assert_eq!(selected, expected);
            let plain = sequence("desktop", selected, false, "t", "b");
            let protocol = match expected {
                "kitty" => "\x1b]99;",
                "iTerm.app" | "WezTerm" | "WindowsTerminal" => "\x1b]9;",
                _ => "\x1b]777;",
            };
            assert!(plain.starts_with(protocol), "{program}: {plain:?}");
            let wrapped = sequence("desktop", selected, true, "t", "b");
            assert_eq!(
                wrapped,
                format!(
                    "\x1bPtmux;{}\x1b\\\x07",
                    plain[..plain.len() - 1].replace('\x1b', "\x1b\x1b")
                )
            );
        }
    }

    #[test]
    fn unknown_terminal_notification_uses_legacy_777() {
        for terminal in ["tmux", "screen", "vscode", "Apple_Terminal", "unknown"] {
            assert!(
                sequence("desktop", terminal, false, "t", "b").starts_with("\x1b]777;notify;t;b"),
                "{terminal}"
            );
        }
    }

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
