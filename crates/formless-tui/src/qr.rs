//! Pairing QR codes for the terminal (SPEC §12): two pixel rows per text
//! line with half blocks. Light modules are the drawn blocks, so callers
//! print them white on black (`ansi`), which scans on any terminal theme.

use qrcode::{Color, EcLevel, QrCode};

/// The code for `text` as lines of `█`, `▀`, `▄` and spaces (light
/// modules print as blocks, so it reads the same on any background).
pub fn lines(text: &str) -> Option<Vec<String>> {
    // Level M (~15% damage tolerance) costs the pairing link about one
    // version over L: still terminal-sized, and it scans off a glossy or
    // scaled screen (#882).
    let code = QrCode::with_error_correction_level(text, EcLevel::M).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    // The 4-module quiet zone ISO/IEC 18004 asks for, drawn light, since
    // what lies beyond the code on a dark terminal is not (#882).
    let quiet = 4i64;
    let size = width as i64 + 2 * quiet;
    let light = |x: i64, y: i64| {
        let (cx, cy) = (x - quiet, y - quiet);
        if cx < 0 || cy < 0 || cx >= width as i64 || cy >= width as i64 {
            return true;
        }
        colors[cy as usize * width + cx as usize] == Color::Light
    };
    let mut out = Vec::new();
    let mut y = 0;
    while y < size {
        let line: String = (0..size)
            .map(|x| match (light(x, y), y + 1 >= size || light(x, y + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            })
            .collect();
        out.push(line);
        y += 2;
    }
    Some(out)
}

/// `lines`, coloured white on black for a real terminal.
pub fn ansi(text: &str) -> Option<String> {
    Some(
        lines(text)?
            .iter()
            .map(|l| format!("\x1b[97;40m{l}\x1b[0m\n"))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_becomes_a_square_of_half_blocks() {
        let l = lines("https://formless.athiwat.xyz/#k=abc").unwrap();
        let w = l[0].chars().count();
        // Square: twice as many pixel rows as text lines (rounded up).
        assert!(
            l.len() * 2 >= w && l.len() * 2 <= w + 1,
            "{} x {}",
            w,
            l.len()
        );
        assert!(l.iter().all(|r| r.chars().count() == w));
        // The quiet zone is light: the top line is all full blocks.
        assert!(l[0].chars().all(|c| c == '█'));
        assert!(l.iter().any(|r| r.contains(' ')), "dark modules show");
    }

    #[test]
    fn the_pairing_link_has_a_full_quiet_zone_and_fits_a_terminal() {
        // The default relay URL with a 43-character secret.
        let url = format!("https://formless.athiwat.xyz/#k={}", "A".repeat(43));
        let l = lines(&url).unwrap();
        let w = l[0].chars().count();
        // Four light modules on every side: two text lines on top, four
        // full blocks at both ends of every line.
        assert!(l[..2].iter().all(|r| r.chars().all(|c| c == '█')));
        for row in &l {
            let chars: Vec<char> = row.chars().collect();
            assert!(
                chars[..4].iter().chain(&chars[w - 4..]).all(|c| *c == '█'),
                "{row}"
            );
        }
        // Level M: version 5 (37 modules) plus the quiet zone.
        assert_eq!(w, 37 + 8);
        assert!(l.len() <= 23, "{} lines", l.len());
    }
}
