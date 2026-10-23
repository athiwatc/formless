//! Pairing QR codes for the terminal (SPEC §12): two pixel rows per text
//! line with half blocks. Light modules are the drawn blocks, so callers
//! print them white on black (`ansi`), which scans on any terminal theme.

use qrcode::{Color, EcLevel, QrCode};

/// The code for `text` as lines of `█`, `▀`, `▄` and spaces (light
/// modules print as blocks, so it reads the same on any background).
pub fn lines(text: &str) -> Option<Vec<String>> {
    let code = QrCode::with_error_correction_level(text, EcLevel::L).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    // A 2-module quiet zone; outside the code counts as light.
    let quiet = 2i64;
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
}
