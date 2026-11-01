//! Reading an image from the system clipboard (SPEC §5.6) with the
//! platform's own tools, so no image library is linked in:
//! macOS `osascript`, Wayland `wl-paste`, X11 `xclip`, Windows PowerShell.

use std::process::Command;

/// PNG bytes of the clipboard's image, or why there is none.
pub fn read_png() -> Result<Vec<u8>, String> {
    if cfg!(target_os = "macos") {
        let out = run("osascript", &["-e", "get the clipboard as «class PNGf»"])?;
        return parse_applescript_data(&String::from_utf8_lossy(&out))
            .ok_or_else(|| "no image on the clipboard".into());
    }
    if cfg!(windows) {
        let script = "Add-Type -AssemblyName System.Windows.Forms; \
            $i = [System.Windows.Forms.Clipboard]::GetImage(); \
            if ($i) { $m = New-Object IO.MemoryStream; \
            $i.Save($m, [System.Drawing.Imaging.ImageFormat]::Png); \
            [Convert]::ToBase64String($m.ToArray()) }";
        let out = run("powershell", &["-NoProfile", "-Command", script])?;
        use base64::Engine;
        let text = String::from_utf8_lossy(&out);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text.trim())
            .map_err(|_| "no image on the clipboard".to_string())?;
        return non_empty(bytes);
    }
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let out = if wayland {
        run("wl-paste", &["--no-newline", "--type", "image/png"])
    } else {
        run(
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
        )
    };
    out.map_err(|e| {
        format!(
            "{e} (install {} for image paste)",
            if wayland { "wl-clipboard" } else { "xclip" }
        )
    })
    .and_then(non_empty)
}

fn non_empty(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if bytes.starts_with(b"\x89PNG") {
        Ok(bytes)
    } else {
        Err("no image on the clipboard".into())
    }
}

fn run(cmd: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("can't run {cmd}: {e}"))?;
    if !out.status.success() {
        return Err("no image on the clipboard".into());
    }
    Ok(out.stdout)
}

/// AppleScript prints clipboard data as `«data PNGf89504E47…»`.
fn parse_applescript_data(text: &str) -> Option<Vec<u8>> {
    let start = text.find("«data PNGf")? + "«data PNGf".len();
    let hex = text[start..].split('»').next()?;
    let bytes: Option<Vec<u8>> = (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect();
    bytes.and_then(|b| non_empty(b).ok())
}

/// A pasted (dragged-in) file path, unquoted and unescaped, if it names an
/// existing file. Terminals paste drops as `'/a b.png'`, `/a\ b.png` or
/// `file:///a%20b.png`.
pub fn dropped_file(pasted: &str) -> Option<std::path::PathBuf> {
    let t = pasted.trim();
    if t.is_empty() || t.contains('\n') {
        return None;
    }
    let unquoted = t
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
        .or_else(|| t.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
        .unwrap_or(t);
    let p = if unquoted.starts_with("file://") {
        url::Url::parse(unquoted).ok()?.to_file_path().ok()?
    } else {
        std::path::PathBuf::from(unquoted.replace("\\ ", " "))
    };
    (p.is_absolute() && p.is_file()).then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_applescript_png_data() {
        assert_eq!(
            parse_applescript_data("«data PNGf89504E470D0A»\n").unwrap(),
            vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A]
        );
        assert!(parse_applescript_data("«data TIFF0000»").is_none());
        assert!(parse_applescript_data("hello").is_none());
    }

    #[test]
    fn recognises_dropped_paths() {
        let dir = std::env::temp_dir().join(format!("fl drop {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("shot #1 %.png");
        std::fs::write(&file, b"x").unwrap();
        let plain = file.to_string_lossy().to_string();
        assert_eq!(dropped_file(&format!("'{plain}'")).as_ref(), Some(&file));
        assert_eq!(
            dropped_file(&plain.replace(' ', "\\ ")).as_ref(),
            Some(&file)
        );
        assert_eq!(
            dropped_file(url::Url::from_file_path(&file).unwrap().as_str()).as_ref(),
            Some(&file)
        );
        assert!(dropped_file("just some text").is_none());
        assert!(dropped_file("relative.png").is_none());
        assert!(dropped_file(&format!("{plain}\nmore")).is_none());
    }
}
