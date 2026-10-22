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

/// The escape sequence that draws `data` (the file's bytes) into a box of
/// `cols` × `ROWS` cells at the cursor. Kitty takes PNG only.
pub fn sequence(proto: Protocol, data: &[u8], cols: u16) -> Option<String> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(data);
    match proto {
        Protocol::ITerm => Some(format!(
            "\x1b]1337;File=inline=1;size={};width={cols};height={ROWS};preserveAspectRatio=1:{b64}\x07",
            data.len()
        )),
        Protocol::Kitty => {
            if !data.starts_with(b"\x89PNG") {
                return None;
            }
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

    #[test]
    fn sequences_per_protocol() {
        let png = b"\x89PNG\r\n\x1a\nrest".to_vec();
        let iterm = sequence(Protocol::ITerm, &png, 30).unwrap();
        assert!(iterm.starts_with("\x1b]1337;File=inline=1;size=12;width=30;height=8;"));
        let kitty = sequence(Protocol::Kitty, &png, 30).unwrap();
        assert!(kitty.starts_with("\x1b_Ga=T,f=100,q=2,C=1,c=30,r=8,m=0;"));
        assert!(
            sequence(Protocol::Kitty, b"\xff\xd8jpeg", 30).is_none(),
            "kitty: PNG only"
        );
        // Big images go in 4096-byte chunks.
        let big = [png.clone(), vec![0u8; 9000]].concat();
        let k = sequence(Protocol::Kitty, &big, 30).unwrap();
        assert_eq!(k.matches("\x1b_G").count(), 3);
        assert!(k.contains(",m=1;") && k.contains("\x1b_Gm=0;"));
        assert_eq!(detect("off"), None);
    }
}
