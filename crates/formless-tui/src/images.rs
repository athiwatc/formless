//! Inline image thumbnails in the chat (SPEC §10.2). Image attachments get
//! a few reserved rows; after each draw, the images whose rows are fully
//! visible are sent to the terminal with its own protocol, which scales
//! them into the box: iTerm2's (iTerm2, WezTerm) or kitty's graphics
//! protocol (kitty, Ghostty). Other terminals keep the reference line.

use std::io::Write;

/// Rows reserved for a thumbnail.
pub const ROWS: u16 = 8;
/// Widest a thumbnail gets, in columns.
pub const MAX_COLS: u16 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    ITerm,
    Kitty,
}

/// The protocol for `tui.inline_images` ("auto" or "off") in this
/// terminal; none inside tmux (it doesn't pass images through by
/// default) or on terminals without one.
pub fn detect(setting: &str) -> Option<Protocol> {
    if setting == "off" || std::env::var_os("TMUX").is_some() {
        return None;
    }
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let term = std::env::var("TERM").unwrap_or_default();
    if std::env::var_os("KITTY_WINDOW_ID").is_some()
        || term == "xterm-kitty"
        || program == "ghostty"
    {
        return Some(Protocol::Kitty);
    }
    match program.as_str() {
        "iTerm.app" | "WezTerm" => Some(Protocol::ITerm),
        _ => None,
    }
}

/// A thumbnail on screen: where, how big, and which file.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub x: u16,
    pub y: u16,
    pub cols: u16,
    pub path: String,
}

/// Longest side of a decoded terminal preview: a thumbnail, so a phone
/// photo doesn't become megabytes of escape sequence.
const THUMB_PX: u32 = 800;

/// Both protocols use bounded decoded previews. Small iTerm images keep
/// their original format (including animations); kitty requires PNG.
fn preview_bytes(proto: Protocol, data: &[u8]) -> Option<std::borrow::Cow<'_, [u8]>> {
    let img = image::load_from_memory(data).ok()?;
    let oversized = img.width().max(img.height()) > THUMB_PX;
    if !oversized && (proto == Protocol::ITerm || data.starts_with(b"\x89PNG\r\n\x1a\n")) {
        return Some(std::borrow::Cow::Borrowed(data));
    }
    let img = if oversized {
        img.thumbnail(THUMB_PX, THUMB_PX)
    } else {
        img
    };
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(std::borrow::Cow::Owned(png))
}

/// Whether the image at `path` can be drawn with `proto`, so its rows are
/// worth reserving (#881). Both previews need a supported decoder format;
/// only the header is checked, once per file.
pub fn drawable(proto: Protocol, path: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashMap<(String, bool), bool>>> = OnceLock::new();
    let key = (path.to_string(), proto == Protocol::Kitty);
    let seen = SEEN.get_or_init(Default::default);
    if let Some(&known) = seen.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return known;
    }
    let ok = image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .is_some();
    seen.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, ok);
    ok
}

/// The escape sequence that draws `data` (the file's bytes) into a box of
/// `cols` × `ROWS` cells at the cursor. Oversized images are downscaled
/// before encoding; None when the supported decoder can't read the image.
pub fn sequence(proto: Protocol, data: &[u8], cols: u16) -> Option<String> {
    use base64::Engine;
    let data = preview_bytes(proto, data)?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
    match proto {
        Protocol::ITerm => Some(format!(
            "\x1b]1337;File=inline=1;size={};width={cols};height={ROWS};preserveAspectRatio=1:{b64}\x07",
            data.len()
        )),
        Protocol::Kitty => {
            // Chunks of 4096 base64 bytes; `m=1` until the last.
            let chunks: Vec<&str> = b64
                .as_bytes()
                .chunks(4096)
                .map(|c| std::str::from_utf8(c).unwrap_or_default())
                .collect();
            let mut out = String::new();
            for (i, chunk) in chunks.iter().enumerate() {
                let more = u8::from(i + 1 < chunks.len());
                if i == 0 {
                    out.push_str(&format!(
                        "\x1b_Ga=T,f=100,q=2,C=1,c={cols},r={ROWS},m={more};{chunk}\x1b\\"
                    ));
                } else {
                    out.push_str(&format!("\x1b_Gm={more};{chunk}\x1b\\"));
                }
            }
            Some(out)
        }
    }
}

/// Draw `placed` (replacing what was drawn before, for kitty, whose
/// images outlive the text under them).
pub fn paint(out: &mut impl Write, proto: Protocol, placed: &[Placed]) -> std::io::Result<()> {
    use crossterm::{cursor, style::Print, QueueableCommand};
    out.queue(cursor::SavePosition)?;
    if proto == Protocol::Kitty {
        out.queue(Print("\x1b_Ga=d,d=a,q=2\x1b\\"))?;
    }
    for p in placed {
        let Ok(data) = std::fs::read(&p.path) else {
            continue;
        };
        if let Some(seq) = sequence(proto, &data, p.cols) {
            out.queue(cursor::MoveTo(p.x, p.y))?;
            out.queue(Print(seq))?;
        }
    }
    out.queue(cursor::RestorePosition)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_png(width: u32, height: u32) -> Vec<u8> {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(width, height, image::Rgba([200, 30, 30, 100]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    }

    fn payload(proto: Protocol, seq: &str) -> Vec<u8> {
        use base64::Engine;
        let encoded = match proto {
            Protocol::Kitty => seq
                .split("\x1b\\")
                .filter_map(|chunk| chunk.split_once(';').map(|(_, data)| data))
                .collect::<String>(),
            Protocol::ITerm => seq
                .split_once(':')
                .unwrap()
                .1
                .strip_suffix('\x07')
                .unwrap()
                .to_string(),
        };
        let data = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        if proto == Protocol::ITerm {
            assert!(seq.contains(&format!(";size={};", data.len())));
        }
        data
    }

    fn assert_png_preview_bound(proto: Protocol) {
        for (width, height, expected) in [(1200, 900, (800, 600)), (900, 1200, (600, 800))] {
            let png = fixture_png(width, height);
            let seq = sequence(proto, &png, 30).unwrap();
            let thumb = image::load_from_memory(&payload(proto, &seq))
                .unwrap()
                .to_rgba8();
            assert_eq!(thumb.dimensions(), expected, "{proto:?}");
            assert_eq!(
                thumb.get_pixel(0, 0).0[3],
                100,
                "PNG transparency is retained"
            );
        }
        let small = fixture_png(20, 10);
        let seq = sequence(proto, &small, 30).unwrap();
        assert_eq!(payload(proto, &seq), small, "small PNG needs no transcode");
    }

    #[test]
    fn kitty_png_previews_are_bounded() {
        assert_png_preview_bound(Protocol::Kitty);
    }

    #[test]
    fn iterm_png_previews_are_bounded() {
        assert_png_preview_bound(Protocol::ITerm);
    }

    #[test]
    fn sequences_per_protocol() {
        let png = fixture_png(20, 10);
        let iterm = sequence(Protocol::ITerm, &png, 30).unwrap();
        assert!(iterm.starts_with(&format!(
            "\x1b]1337;File=inline=1;size={};width=30;height=8;",
            png.len()
        )));
        let kitty = sequence(Protocol::Kitty, &png, 30).unwrap();
        assert!(kitty.starts_with("\x1b_Ga=T,f=100,q=2,C=1,c=30,r=8,m=0;"));
        assert!(
            sequence(Protocol::Kitty, b"\xff\xd8not really a jpeg", 30).is_none(),
            "kitty: unreadable bytes draw nothing"
        );
        // Big images go in 4096-byte chunks.
        let mut state = 0x12345678u32;
        let noisy = image::RgbImage::from_fn(128, 128, |_, _| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            image::Rgb([state as u8, (state >> 8) as u8, (state >> 16) as u8])
        });
        let mut big = Vec::new();
        noisy
            .write_to(&mut std::io::Cursor::new(&mut big), image::ImageFormat::Png)
            .unwrap();
        let k = sequence(Protocol::Kitty, &big, 30).unwrap();
        assert!(k.matches("\x1b_G").count() > 1);
        for chunk in k.split("\x1b\\").filter(|s| !s.is_empty()) {
            assert!(chunk.split_once(';').unwrap().1.len() <= 4096);
        }
        assert_eq!(payload(Protocol::Kitty, &k), big);
        for proto in [Protocol::Kitty, Protocol::ITerm] {
            assert!(sequence(proto, b"\x89PNG\r\n\x1a\nrest", 30).is_none());
        }
        assert!(k.contains(",m=1;") && k.contains("\x1b_Gm=0;"));
        assert_eq!(detect("off"), None);
    }

    /// A JPEG is transcoded to PNG for kitty, so it actually appears, and
    /// a file nothing can draw reserves no rows (#881).
    #[test]
    fn kitty_draws_jpeg_by_transcoding() {
        use base64::Engine;
        let mut jpeg = Vec::new();
        image::RgbImage::from_pixel(1200, 900, image::Rgb([200, 30, 30]))
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let seq = sequence(Protocol::Kitty, &jpeg, 30).expect("drawable");
        assert!(seq.starts_with("\x1b_Ga=T,f=100,"));
        // The payload is a PNG thumbnail.
        let encoded: String = seq
            .split("\x1b\\")
            .filter_map(|c| c.split_once(';').map(|(_, d)| d))
            .collect();
        let png = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        let thumb = image::load_from_memory(&png).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (800, 600));

        let iterm = sequence(Protocol::ITerm, &jpeg, 30).unwrap();
        let thumb = image::load_from_memory(&payload(Protocol::ITerm, &iterm)).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (800, 600));
        let mut small = Vec::new();
        image::RgbImage::from_pixel(20, 10, image::Rgb([200, 30, 30]))
            .write_to(
                &mut std::io::Cursor::new(&mut small),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let seq = sequence(Protocol::ITerm, &small, 30).unwrap();
        assert_eq!(payload(Protocol::ITerm, &seq), small);

        let dir = std::env::temp_dir().join(format!("fl-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (good, bad) = (dir.join("a.jpg"), dir.join("b.jpg"));
        std::fs::write(&good, &jpeg).unwrap();
        std::fs::write(&bad, b"not an image").unwrap();
        assert!(drawable(Protocol::Kitty, good.to_str().unwrap()));
        assert!(!drawable(Protocol::Kitty, bad.to_str().unwrap()));
        assert!(drawable(Protocol::ITerm, good.to_str().unwrap()));
        assert!(!drawable(Protocol::ITerm, bad.to_str().unwrap()));
        assert!(!drawable(
            Protocol::ITerm,
            dir.join("missing.png").to_str().unwrap()
        ));
        let _ = std::fs::remove_dir_all(dir);
    }
}
