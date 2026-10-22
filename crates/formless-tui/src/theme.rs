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

/// A role's override (`[tui.colors]`), if any.
pub fn role(name: &str) -> Option<Color> {
    THEME.with(|t| t.borrow().as_ref()?.roles.get(name).copied())
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
    if let Some(r) = role {
        return *r;
    }
    match t.kind {
        Kind::Dark => c,
        // Deeper hues that stay readable on a white background.
        Kind::Light => match c {
            Color::Yellow | Color::LightYellow => Color::Rgb(150, 100, 0),
            Color::Cyan | Color::LightCyan => Color::Rgb(0, 110, 140),
            Color::Green | Color::LightGreen => Color::Rgb(0, 125, 50),
            Color::Magenta | Color::LightMagenta => Color::Rgb(140, 0, 140),
            Color::Blue | Color::LightBlue => Color::Rgb(0, 80, 180),
            Color::Red | Color::LightRed => Color::Rgb(185, 0, 0),
            Color::Gray => Color::Rgb(90, 90, 90),
            Color::DarkGray => Color::Rgb(120, 120, 120),
            Color::White => Color::Black,
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

#[cfg(test)]
pub(crate) fn reset() {
    THEME.with(|t| *t.borrow_mut() = None);
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
        assert_eq!(role("user"), Some(Color::Magenta));
        set(from_config("high-contrast", &HashMap::new()));
        assert_eq!(remap(Color::DarkGray), Color::Gray);
        reset();
        assert_eq!(remap(Color::Yellow), Color::Yellow);
    }
}
