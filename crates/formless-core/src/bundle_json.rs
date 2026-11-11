//! Shared JSON text boundary for explicit file/URL bundle imports.
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::path::Path;

fn check_encoding(bytes: &[u8], source: &str) -> Result<()> {
    if bytes.starts_with(&[0xff, 0xfe, 0, 0]) || bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
        bail!("{source} is UTF-32; re-save it as UTF-8 JSON before importing");
    }
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        bail!("{source} is UTF-16; re-save it as UTF-8 JSON before importing");
    }
    Ok(())
}

fn parse_text(text: &str, source: &str) -> Result<Value> {
    serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(text))
        .map_err(|error| anyhow!("{source} is not a JSON bundle: {error}"))
}

/// Parse UTF-8 JSON with an optional leading BOM. Recognized UTF-16/32
/// BOMs receive a named conversion instruction rather than a bare parse error.
pub fn parse(bytes: &[u8], source: &str) -> Result<Value> {
    check_encoding(bytes, source)?;
    let text = std::str::from_utf8(bytes).map_err(|error| {
        anyhow!("{source} is not a JSON bundle: expected UTF-8 ({error}); re-save it as UTF-8 JSON")
    })?;
    parse_text(text, source)
}

/// Read a local bundle, retaining source context on I/O and parse failures.
pub fn read(path: impl AsRef<Path>) -> Result<Value> {
    read_with_text(path).map(|(value, _)| value)
}

/// Also return the exact original UTF-8 text, including its BOM and spacing.
/// Callers that fingerprint the source must not hash normalized JSON instead.
pub fn read_with_text(path: impl AsRef<Path>) -> Result<(Value, String)> {
    let path = path.as_ref();
    let source = path.display().to_string();
    let bytes = std::fs::read(path).with_context(|| format!("cannot read JSON bundle {source}"))?;
    check_encoding(&bytes, &source)?;
    let text = String::from_utf8(bytes).map_err(|error| {
        anyhow!(
            "{source} is not a JSON bundle: expected UTF-8 ({}); re-save it as UTF-8 JSON",
            error.utf8_error()
        )
    })?;
    let value = parse_text(&text, &source)?;
    Ok((value, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_bom_preserves_json_values_and_original_text() {
        let raw = "\u{feff} {\"format\":\"fixture\",\"name\":\"ชื่อ\",\"text\":\"inside\u{feff}value\"}\r\n";
        let expected: Value = serde_json::from_str(raw.trim_start_matches('\u{feff}')).unwrap();
        assert_eq!(parse(raw.as_bytes(), "memory.json").unwrap(), expected);
        assert_eq!(
            parse(raw.trim_start_matches('\u{feff}').as_bytes(), "plain.json").unwrap(),
            expected
        );
        let dir = std::env::temp_dir().join(format!("bundle-text-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("source with spaces.json");
        std::fs::write(&file, raw).unwrap();
        let (value, original) = read_with_text(&file).unwrap();
        assert_eq!(value, expected);
        assert_eq!(
            original, raw,
            "fingerprinting source bytes must stay unchanged"
        );
        assert_eq!(read(&file).unwrap(), expected);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn encoding_and_parse_errors_name_the_source_and_conversion() {
        for bytes in [&[0xff, 0xfe, b'{', 0][..], &[0xfe, 0xff, 0, b'{'][..]] {
            let error = parse(bytes, "Windows export.json").unwrap_err().to_string();
            assert!(
                error.contains("Windows export.json")
                    && error.contains("UTF-16")
                    && error.contains("re-save it as UTF-8"),
                "{error}"
            );
        }
        for bytes in [&[0xff, 0xfe, 0, 0][..], &[0, 0, 0xfe, 0xff][..]] {
            let error = parse(bytes, "UTF32 export.json").unwrap_err().to_string();
            assert!(
                error.contains("UTF32 export.json")
                    && error.contains("UTF-32")
                    && error.contains("re-save it as UTF-8"),
                "{error}"
            );
        }
        for bytes in [&b"\xef\xbb\xbf{bad"[..], &b"\xff"[..], &b""[..]] {
            let error = parse(bytes, "broken.json").unwrap_err().to_string();
            assert!(
                error.contains("broken.json is not a JSON bundle:"),
                "{error}"
            );
        }
        let error = read("/nonexistent-formless-bundle-fixture/file.json")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("file.json") && error.contains("JSON bundle"),
            "{error}"
        );
    }
}
