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

/// A single existing absolute file, pasted literally, as a local file URL,
/// or with terminal shell quoting. Parsing never expands or executes text.
/// Prefer an exact literal filename before interpreting backslashes/quotes.
pub fn dropped_file(pasted: &str) -> Option<std::path::PathBuf> {
    dropped_file_with(pasted, cfg!(windows), |text| {
        let path = if text.starts_with("file://") {
            url::Url::parse(text).ok()?.to_file_path().ok()?
        } else {
            std::path::PathBuf::from(text)
        };
        (path.is_absolute() && path.is_file()).then_some(path)
    })
}

fn dropped_file_with(
    pasted: &str,
    windows: bool,
    existing: impl Fn(&str) -> Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    let source = pasted.trim_matches(['\r', '\n']);
    if source.trim().is_empty() || source.contains(['\r', '\n', '\0']) {
        return None;
    }
    if let Some(path) = existing(source).or_else(|| existing(source.trim())) {
        return Some(path);
    }
    // Native Windows paths are not POSIX words: their ordinary backslashes
    // are separators. After exact-file checks, support the legacy escaped
    // space form without consuming any other backslash.
    if windows {
        let native = source.trim();
        let native = native
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(native);
        if let Some(path) = existing(native) {
            return Some(path);
        }
        if native.contains("\\ ") {
            let mut decoded = String::with_capacity(native.len());
            let mut chars = native.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\\' && chars.peek() == Some(&' ') {
                    chars.next();
                    decoded.push(' ');
                } else {
                    decoded.push(c);
                }
            }
            if let Some(path) = existing(&decoded) {
                return Some(path);
            }
        }
    }
    let words = shlex::split(source)?;
    match words.as_slice() {
        [word] => existing(word),
        _ => None,
    }
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

    #[cfg(unix)]
    #[test]
    fn dropped_paths_decode_shell_quoting_without_rewriting_literal_files() {
        let dir = std::env::temp_dir().join(format!("fl-drop-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        for name in [
            "shot (1).png",
            "shot & '$;*#?[]`\"|<> ไทย.png",
            "slash\\ space.png",
            "tail.png ",
        ] {
            let file = dir.join(name);
            std::fs::write(&file, b"x").unwrap();
            let plain = file.to_str().unwrap();
            // Terminal/iTerm-style escaping, including backslash parity.
            let escaped: String = plain
                .chars()
                .flat_map(|c| {
                    let escape = " \\()&'\"$;*#?[]`|<>".contains(c);
                    if escape {
                        vec!['\\', c]
                    } else {
                        vec![c]
                    }
                })
                .collect();
            let single = format!("'{}'", plain.replace('\'', "'\\''"));
            let double = format!(
                "\"{}\"",
                plain
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('$', "\\$")
                    .replace('`', "\\`")
            );
            for input in [plain.to_owned(), escaped, single, double] {
                assert_eq!(dropped_file(&input), Some(file.clone()), "{input:?}");
            }
        }
        let file = dir.join("literal\\ space.png");
        std::fs::write(&file, b"literal").unwrap();
        std::fs::write(dir.join("literal space.png"), b"different").unwrap();
        assert_eq!(dropped_file(file.to_str().unwrap()), Some(file));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn dropped_paths_reject_incomplete_or_multiple_shell_words() {
        let dir = std::env::temp_dir().join(format!("fl-drop-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let first = dir.join("first.png");
        let second = dir.join("second.png");
        std::fs::write(&first, b"x").unwrap();
        std::fs::write(&second, b"x").unwrap();
        let quoted = format!("'{}'", first.display());
        for input in [
            format!("{quoted} '{}'", second.display()),
            format!("'{}", first.display()),
            format!("{}\\", first.display()),
            format!("{quoted}\n{}", second.display()),
            format!("{quoted}; echo hello"),
            "relative.png".into(),
            "$HOME/first.png".into(),
            "~/first.png".into(),
            dir.display().to_string(),
        ] {
            assert!(dropped_file(&input).is_none(), "{input:?}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn windows_drop_dispatch_keeps_drive_and_unc_separators() {
        // Exercise the Windows dispatch on Linux too; filesystem validity is
        // supplied explicitly, not inferred from Linux's Path rules.
        for (input, expected) in [
            (r"C:\Temp\shot\ (1).png", r"C:\Temp\shot (1).png"),
            (
                r"\\server\share\shot\ (1).png",
                r"\\server\share\shot (1).png",
            ),
            (r#""C:\Temp\shot\ (1).png""#, r"C:\Temp\shot (1).png"),
        ] {
            let result = dropped_file_with(input, true, |candidate| {
                (candidate == expected).then(|| expected.into())
            });
            assert_eq!(result, Some(expected.into()), "{input}");
        }
        let literal = r"C:\Temp\literal\ space.png";
        let sibling = r"C:\Temp\literal space.png";
        assert_eq!(
            dropped_file_with(literal, true, |candidate| {
                [literal, sibling]
                    .contains(&candidate)
                    .then(|| candidate.into())
            }),
            Some(literal.into())
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_native_drops_keep_separators_and_exact_file_precedence() {
        let dir = std::env::temp_dir().join(format!("fl drop {}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let file = dir.join("shot (1).png");
        std::fs::write(&file, b"x").unwrap();
        let plain = file.to_str().unwrap();
        let escaped = plain.replace(' ', "\\ ");
        for input in [plain.to_owned(), escaped.clone(), format!("\"{escaped}\"")] {
            assert_eq!(dropped_file(&input), Some(file.clone()));
        }
        std::fs::create_dir(dir.join("literal")).unwrap();
        let exact = dir.join("literal").join(" space.png");
        std::fs::write(&exact, b"exact").unwrap();
        std::fs::write(dir.join("literal space.png"), b"other").unwrap();
        assert_eq!(dropped_file(exact.to_str().unwrap()), Some(exact));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recognises_dropped_paths() {
        let dir = std::env::temp_dir().join(format!("fl drop {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("shot #1 %.png");
        std::fs::write(&file, b"x").unwrap();
        let plain = file.to_string_lossy().to_string();
        assert_eq!(dropped_file(&format!("'{plain}'")).as_ref(), Some(&file));
        assert_eq!(dropped_file(&format!("\"{plain}\"")).as_ref(), Some(&file));
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
