//! Colour themes (SPEC §10.6): every coloured style goes through `fg`,
//! which maps the base colour the code names through the active theme,
//! with per-role overrides from `[tui.colors]`.

use ratatui::style::Color;
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dark,
    Light,
    HighContrast,
    Colorblind,
}

/// One status vocabulary and palette, shared by terminal tables and TUI views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticTone {
    Good,
    Warn,
    Bad,
}
pub const fn colorblind_rgb(tone: SemanticTone) -> (u8, u8, u8) {
    match tone {
        SemanticTone::Good => (86, 180, 233),
        SemanticTone::Warn => (240, 228, 66),
        SemanticTone::Bad => (230, 159, 0),
    }
}
pub const fn tone_marker(tone: SemanticTone) -> &'static str {
    match tone {
        SemanticTone::Good => "ok",
        SemanticTone::Warn => "warn",
        SemanticTone::Bad => "fail",
    }
}
pub(crate) fn redundant(color: bool) -> bool {
    !color
        || THEME.with(|theme| {
            theme
                .borrow()
                .as_ref()
                .is_some_and(|theme| theme.kind == Kind::Colorblind)
        })
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub kind: Kind,
    /// user, assistant, tool, note, error, accent.
    pub roles: HashMap<String, Color>,
}

impl Default for Theme {
    fn default() -> Self {
        Theme {
            kind: Kind::Dark,
            roles: HashMap::new(),
        }
    }
}

// The TUI draws on one thread, so the theme lives there (and tests on
// other threads keep the plain palette).
thread_local! {
    static THEME: RefCell<Option<Theme>> = const { RefCell::new(None) };
}

/// Use `theme` from now on.
pub fn set(theme: Theme) {
    THEME.with(|t| *t.borrow_mut() = Some(theme));
}

/// The theme for `tui.theme` and `[tui.colors]`. "auto" picks light or
/// dark from `COLORFGBG` (set by many terminals), else dark.
pub fn from_config(name: &str, colors: &HashMap<String, String>) -> Theme {
    let kind = match name {
        "light" => Kind::Light,
        "high-contrast" => Kind::HighContrast,
        "colorblind" => Kind::Colorblind,
        "dark" => Kind::Dark,
        _ => detect(std::env::var("COLORFGBG").ok().as_deref()),
    };
    let roles = colors
        .iter()
        .filter_map(|(k, v)| parse_color(v).map(|c| (k.to_lowercase(), c)))
        .collect();
    Theme { kind, roles }
}

/// Light when `COLORFGBG`'s background ("fg;bg") is a light colour.
pub fn detect(colorfgbg: Option<&str>) -> Kind {
    let bg = colorfgbg
        .and_then(|v| v.rsplit(';').next())
        .and_then(|b| b.parse::<u8>().ok());
    match bg {
        Some(7) | Some(15) | Some(9..=14) => Kind::Light,
        _ => Kind::Dark,
    }
}

/// `blue`, `light-cyan`, `#3a7bd5` or `#3ad`.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim().to_lowercase().replace(['_', ' '], "-");
    if let Some(hex) = s.strip_prefix('#') {
        let full = match hex.len() {
            3 => hex.chars().flat_map(|c| [c, c]).collect::<String>(),
            6 => hex.to_string(),
            _ => return None,
        };
        let n = u32::from_str_radix(&full, 16).ok()?;
        return Some(Color::Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8));
    }
    Some(match s.as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "dark-gray" | "dark-grey" => Color::DarkGray,
        "light-red" => Color::LightRed,
        "light-green" => Color::LightGreen,
        "light-yellow" => Color::LightYellow,
        "light-blue" => Color::LightBlue,
        "light-magenta" => Color::LightMagenta,
        "light-cyan" => Color::LightCyan,
        "white" => Color::White,
        _ => return None,
    })
}

/// A role's override resolved through the active palette, if any.
pub fn role(name: &str) -> Option<Color> {
    THEME.with(|slot| {
        let theme = slot.borrow();
        let t = theme.as_ref()?;
        t.roles.get(name).map(|&color| resolve_override(t, color))
    })
}

/// Resolve a named override without applying another role's override.
fn resolve_override(t: &Theme, color: Color) -> Color {
    match color {
        Color::Rgb(..) | Color::Indexed(_) => color,
        named => palette(t.kind, named),
    }
}

/// The colour to draw for `c` under the active theme. Cyan is the accent
/// and Red the error colour, so those overrides apply everywhere.
pub fn remap(c: Color) -> Color {
    THEME.with(|t| t.borrow().as_ref().map_or(c, |t| remap_with(t, c)))
}

fn remap_with(t: &Theme, c: Color) -> Color {
    let role = match c {
        Color::Cyan => t.roles.get("accent"),
        Color::Red => t.roles.get("error"),
        _ => None,
    };
    // A named override goes through the theme like any other colour, so
    // `accent = "yellow"` is deepened on the light theme too (#883).
    if let Some(&r) = role {
        return resolve_override(t, r);
    }
    palette(t.kind, c)
}

fn palette(kind: Kind, c: Color) -> Color {
    match kind {
        // Pinned, so text keeps ≥ 4.5:1 contrast on a dark background
        // whatever the terminal's palette makes of "blue" (#883).
        Kind::Dark => match c {
            Color::Yellow => Color::Rgb(229, 192, 123),
            Color::LightYellow => Color::Rgb(240, 210, 120),
            Color::Cyan => Color::Rgb(86, 182, 194),
            Color::LightCyan => Color::Rgb(120, 210, 220),
            Color::Green => Color::Rgb(152, 195, 121),
            Color::LightGreen => Color::Rgb(170, 215, 140),
            Color::Magenta => Color::Rgb(198, 120, 221),
            Color::LightMagenta => Color::Rgb(215, 150, 235),
            Color::Blue => Color::Rgb(97, 175, 239),
            Color::LightBlue => Color::Rgb(130, 190, 250),
            Color::Red => Color::Rgb(240, 100, 100),
            Color::LightRed => Color::Rgb(250, 130, 130),
            Color::Gray => Color::Rgb(171, 178, 191),
            Color::DarkGray => Color::Rgb(140, 140, 140),
            other => other,
        },
        // Deeper hues that stay readable on a white background.
        Kind::Light => match c {
            Color::Yellow | Color::LightYellow => Color::Rgb(150, 100, 0),
            Color::Cyan | Color::LightCyan => Color::Rgb(0, 110, 140),
            Color::Green | Color::LightGreen => Color::Rgb(0, 125, 50),
            Color::Magenta | Color::LightMagenta => Color::Rgb(140, 0, 140),
            Color::Blue | Color::LightBlue => Color::Rgb(0, 80, 180),
            Color::Red | Color::LightRed => Color::Rgb(185, 0, 0),
            Color::Gray => Color::Rgb(90, 90, 90),
            // 120 was 4.42:1 on white, under AA (#883).
            Color::DarkGray => Color::Rgb(105, 105, 105),
            Color::White => Color::Black,
            other => other,
        },
        Kind::Colorblind => match c {
            Color::Green
            | Color::LightGreen
            | Color::Blue
            | Color::LightBlue
            | Color::Cyan
            | Color::LightCyan => {
                let (r, g, b) = colorblind_rgb(SemanticTone::Good);
                Color::Rgb(r, g, b)
            }
            Color::Red | Color::LightRed => {
                let (r, g, b) = colorblind_rgb(SemanticTone::Bad);
                Color::Rgb(r, g, b)
            }
            Color::Yellow | Color::LightYellow => {
                let (r, g, b) = colorblind_rgb(SemanticTone::Warn);
                Color::Rgb(r, g, b)
            }
            Color::Magenta | Color::LightMagenta => Color::Rgb(204, 121, 167),
            Color::Gray => Color::Rgb(171, 178, 191),
            Color::DarkGray => Color::Rgb(140, 140, 140),
            other => other,
        },
        // Bright variants for the most contrast on a dark background.
        Kind::HighContrast => match c {
            Color::Yellow => Color::LightYellow,
            Color::Cyan => Color::LightCyan,
            Color::Green => Color::LightGreen,
            Color::Magenta => Color::LightMagenta,
            Color::Blue => Color::LightBlue,
            Color::Red => Color::LightRed,
            Color::DarkGray => Color::Gray,
            Color::Gray => Color::White,
            other => other,
        },
    }
}

/// The background each theme is designed against (the dark one a common
/// editor grey, darker than which only raises contrast).
fn background(kind: Kind) -> (u8, u8, u8) {
    match kind {
        Kind::Light => (255, 255, 255),
        Kind::Dark | Kind::HighContrast | Kind::Colorblind => (0x1e, 0x1e, 0x1e),
    }
}

/// xterm's values for the named colours, for checking contrast.
fn rgb(c: Color) -> Option<(u8, u8, u8)> {
    Some(match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::Red => (205, 0, 0),
        Color::Green => (0, 205, 0),
        Color::Yellow => (205, 205, 0),
        Color::Blue => (0, 0, 238),
        Color::Magenta => (205, 0, 205),
        Color::Cyan => (0, 205, 205),
        Color::Gray => (229, 229, 229),
        Color::DarkGray => (127, 127, 127),
        Color::LightRed => (255, 0, 0),
        Color::LightGreen => (0, 255, 0),
        Color::LightYellow => (255, 255, 0),
        Color::LightBlue => (92, 92, 255),
        Color::LightMagenta => (255, 0, 255),
        Color::LightCyan => (0, 255, 255),
        Color::White => (255, 255, 255),
        _ => return None,
    })
}

/// WCAG 2.x contrast ratio of two sRGB colours.
pub fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    fn lum((r, g, b): (u8, u8, u8)) -> f64 {
        let lin = |c: u8| {
            let c = c as f64 / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    }
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// One line per `[tui.colors]` override that would be hard to read on
/// this theme's background (under 4.5:1); it is still used (#883).
pub fn contrast_warnings(t: &Theme) -> Vec<String> {
    let bg = background(t.kind);
    let mut roles: Vec<(&String, &Color)> = t.roles.iter().collect();
    roles.sort_by_key(|(name, _)| *name);
    roles
        .into_iter()
        .filter_map(|(name, c)| {
            let drawn = resolve_override(t, *c);
            let ratio = contrast(rgb(drawn)?, bg);
            (ratio < 4.5).then(|| {
                format!("[tui.colors] {name} has {ratio:.1}:1 contrast on this theme's background (4.5:1 is the readable minimum)")
            })
        })
        .collect()
}

/// The theme `auto` should use when the terminal says its background is
/// `bg` (from an OSC 11 answer): light when the background is light.
pub fn kind_for_background(bg: (u8, u8, u8)) -> Kind {
    if contrast(bg, (0, 0, 0)) > contrast(bg, (255, 255, 255)) {
        Kind::Light
    } else {
        Kind::Dark
    }
}

/// The background colour in an OSC 11 answer
/// (`ESC ] 11 ; rgb:RRRR/GGGG/BBBB` ending in BEL or ST).
pub fn parse_osc11(answer: &[u8]) -> Option<(u8, u8, u8)> {
    let text = String::from_utf8_lossy(answer);
    let at = text.find("]11;rgb:")? + "]11;rgb:".len();
    let body: String = text[at..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit() || *c == '/')
        .collect();
    let mut parts = body.split('/').map(|h| {
        let v = u32::from_str_radix(h, 16).ok()?;
        let max = (1u32 << (4 * h.len().clamp(1, 4))) - 1;
        Some((v * 255 / max) as u8)
    });
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// Ask the terminal for its background colour (OSC 11), for `auto`
/// themes where `COLORFGBG` says nothing (kitty, Ghostty, WezTerm,
/// iTerm2…). A device-attributes query goes after it, which every
/// terminal answers, so the wait ends as soon as the terminal has said
/// all it will; it never waits past `timeout`. Call in raw mode.
#[cfg(unix)]
pub fn query_background(timeout: std::time::Duration) -> Option<(u8, u8, u8)> {
    use std::io::Write;
    // SAFETY: isatty only inspects the descriptors.
    if unsafe { libc::isatty(0) == 0 || libc::isatty(1) == 0 } {
        return None;
    }
    let mut out = std::io::stdout();
    out.write_all(b"\x1b]11;?\x1b\\\x1b[c").ok()?;
    out.flush().ok()?;
    let deadline = std::time::Instant::now() + timeout;
    let mut buf = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        let mut pfd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for the duration of the call.
        let ready = unsafe { libc::poll(&mut pfd, 1, left.as_millis().max(1) as i32) };
        if ready <= 0 {
            break;
        }
        let mut chunk = [0u8; 256];
        // SAFETY: reads at most `chunk.len()` bytes into `chunk`.
        let n = unsafe { libc::read(0, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n <= 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
        // The device-attributes answer (`ESC [ ? … c`) comes last.
        if let Some(i) = buf.windows(3).position(|w| w == b"\x1b[?") {
            if buf[i..].contains(&b'c') {
                break;
            }
        }
    }
    parse_osc11(&buf)
}

#[cfg(not(unix))]
pub fn query_background(_timeout: std::time::Duration) -> Option<(u8, u8, u8)> {
    None
}

/// Switch the active theme to `kind`, keeping its overrides.
pub fn set_kind(kind: Kind) {
    THEME.with(|t| {
        if let Some(t) = t.borrow_mut().as_mut() {
            t.kind = kind;
        }
    });
}

#[cfg(test)]
pub(crate) fn reset() {
    THEME.with(|t| *t.borrow_mut() = None);
}

#[cfg(test)]
pub(crate) struct TestThemeGuard(Option<Theme>);
#[cfg(test)]
impl Drop for TestThemeGuard {
    fn drop(&mut self) {
        THEME.with(|theme| *theme.borrow_mut() = self.0.take());
    }
}
#[cfg(test)]
pub(crate) fn test_theme(name: &str) -> TestThemeGuard {
    TestThemeGuard(THEME.with(|theme| {
        theme
            .borrow_mut()
            .replace(from_config(name, &HashMap::new()))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colours_and_detects_background() {
        assert_eq!(parse_color("Light Cyan"), Some(Color::LightCyan));
        assert_eq!(parse_color("#3a7bd5"), Some(Color::Rgb(0x3a, 0x7b, 0xd5)));
        assert_eq!(parse_color("#fff"), Some(Color::Rgb(255, 255, 255)));
        assert_eq!(parse_color("chartreuse"), None);
        assert_eq!(detect(Some("0;15")), Kind::Light);
        assert_eq!(detect(Some("15;0")), Kind::Dark);
        assert_eq!(detect(None), Kind::Dark);
    }

    #[test]
    fn message_role_resolution_matches_contrast_without_accent_error_chaining() {
        for theme in ["dark", "light", "high-contrast", "colorblind"] {
            for (name, color) in [
                ("cyan", Color::Cyan),
                ("red", Color::Red),
                ("yellow", Color::Yellow),
            ] {
                for role_name in ["user", "assistant", "tool", "note"] {
                    let colors = HashMap::from([
                        (role_name.to_string(), name.to_string()),
                        ("accent".into(), "#112233".into()),
                        ("error".into(), "#445566".into()),
                    ]);
                    let t = from_config(theme, &colors);
                    let expected = palette(t.kind, color);
                    let low_contrast = contrast(rgb(expected).unwrap(), background(t.kind)) < 4.5;
                    assert_eq!(
                        contrast_warnings(&t)
                            .iter()
                            .any(|warning| warning
                                .starts_with(&format!("[tui.colors] {role_name} "))),
                        low_contrast
                    );
                    set(t);
                    assert_eq!(
                        role(role_name),
                        Some(expected),
                        "{theme} {role_name} {name}"
                    );
                    assert_eq!(remap(Color::Cyan), Color::Rgb(0x11, 0x22, 0x33));
                    assert_eq!(remap(Color::Red), Color::Rgb(0x44, 0x55, 0x66));
                }
            }
        }
        reset();
    }

    #[test]
    fn themes_remap_base_colours_and_roles() {
        let mut colors = HashMap::new();
        colors.insert("accent".to_string(), "#112233".to_string());
        colors.insert("user".to_string(), "magenta".to_string());
        let light = from_config("light", &colors);
        assert_eq!(light.kind, Kind::Light);
        set(light);
        assert_eq!(
            remap(Color::Cyan),
            Color::Rgb(0x11, 0x22, 0x33),
            "accent override"
        );
        assert_eq!(remap(Color::Yellow), Color::Rgb(150, 100, 0));
        assert_eq!(role("user"), Some(palette(Kind::Light, Color::Magenta)));
        // A named accent override is deepened like any other colour.
        let mut colors = HashMap::new();
        colors.insert("accent".to_string(), "yellow".to_string());
        set(from_config("light", &colors));
        assert_eq!(remap(Color::Cyan), Color::Rgb(150, 100, 0));
        set(from_config("high-contrast", &HashMap::new()));
        assert_eq!(remap(Color::DarkGray), Color::Gray);
        reset();
        assert_eq!(remap(Color::Yellow), Color::Yellow);
    }

    /// Every colour the built-in themes draw text in has at least 4.5:1
    /// contrast on its background (#883).
    #[test]
    fn default_text_colours_meet_wcag_aa() {
        let text = [
            Color::Yellow,
            Color::LightYellow,
            Color::Cyan,
            Color::LightCyan,
            Color::Green,
            Color::LightGreen,
            Color::Magenta,
            Color::LightMagenta,
            Color::Blue,
            Color::LightBlue,
            Color::Red,
            Color::LightRed,
            Color::Gray,
            Color::DarkGray,
        ];
        for kind in [Kind::Dark, Kind::Light, Kind::Colorblind] {
            for c in text {
                let drawn = rgb(palette(kind, c)).unwrap();
                let ratio = contrast(drawn, background(kind));
                assert!(ratio >= 4.5, "{kind:?} {c:?} → {drawn:?}: {ratio:.2}:1");
            }
        }
        // The light theme's dim grey used to be 4.42:1.
        assert!(contrast((120, 120, 120), (255, 255, 255)) < 4.5);
    }

    #[test]
    fn low_contrast_overrides_warn_but_apply() {
        let mut colors = HashMap::new();
        colors.insert("user".to_string(), "#202020".to_string());
        colors.insert("note".to_string(), "#f0f0f0".to_string());
        let t = from_config("dark", &colors);
        let warn = contrast_warnings(&t);
        assert_eq!(warn.len(), 1, "{warn:?}");
        assert!(warn[0].contains("user") && warn[0].contains(":1"));
        assert_eq!(t.roles["user"], Color::Rgb(0x20, 0x20, 0x20), "still used");
    }

    /// `auto` reads the terminal's answer to OSC 11.
    #[test]
    fn background_answers_pick_the_theme() {
        let white = parse_osc11(b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\\x1b[?62;c").unwrap();
        assert_eq!(white, (255, 255, 255));
        assert_eq!(kind_for_background(white), Kind::Light);
        let dark = parse_osc11(b"\x1b]11;rgb:1e1e/1e1e/1e1e\x07").unwrap();
        assert_eq!(kind_for_background(dark), Kind::Dark);
        assert_eq!(parse_osc11(b"\x1b]11;rgb:f/e/d\x07"), Some((255, 238, 221)));
        assert_eq!(parse_osc11(b"\x1b[?62;c"), None, "no answer: no guess");
    }
}
