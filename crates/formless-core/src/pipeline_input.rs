//! Explicit client-local schema input. RPC callers send a Value, never a path.
use anyhow::{ensure, Context, Result};
use std::io::Read;
use std::path::Path;

/// Parse explicit schema input identically from a file or stdin. Count the
/// original bytes before accepting one optional UTF-8 byte-order mark.
pub fn parse_schema_bytes(bytes: &[u8]) -> Result<serde_json::Value> {
    ensure!(
        bytes.len() <= super::MAX_SCHEMA_BYTES,
        "schema file exceeds the 64 KiB limit"
    );
    let json = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    Ok(serde_json::from_slice(json).context("--schema must contain one JSON Schema")?)
}

pub fn read_schema_file(path: &Path) -> Result<serde_json::Value> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A named pipe or a racing replacement must not block schema loading.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path).context("open --schema file")?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "--schema must name a regular file");
    ensure!(
        metadata.len() <= super::MAX_SCHEMA_BYTES as u64,
        "schema file exceeds the 64 KiB limit"
    );
    let mut bytes = Vec::new();
    file.take((super::MAX_SCHEMA_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    parse_schema_bytes(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_file_accepts_one_bom_and_rejects_invalid_or_oversized_input() {
        let path =
            std::env::temp_dir().join(format!("formless-schema-{}.json", uuid::Uuid::new_v4()));
        struct Owned(std::path::PathBuf);
        impl Drop for Owned {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let owned = Owned(path);
        let plain = br#"{"type":"object"}"#;
        std::fs::write(&owned.0, plain).unwrap();
        let expected = read_schema_file(&owned.0).unwrap();
        std::fs::write(&owned.0, [b"\xef\xbb\xbf".as_slice(), plain].concat()).unwrap();
        assert_eq!(read_schema_file(&owned.0).unwrap(), expected);
        for invalid in [
            b"\xef\xbb\xbfnot JSON".to_vec(),
            b"\xef\xbb\xbf\xef\xbb\xbf{}".to_vec(),
            [
                b"\xef\xbb\xbf".as_slice(),
                &vec![b' '; super::super::MAX_SCHEMA_BYTES],
            ]
            .concat(),
        ] {
            std::fs::write(&owned.0, &invalid).unwrap();
            assert!(read_schema_file(&owned.0).is_err());
            assert!(parse_schema_bytes(&invalid).is_err());
        }
        let mut boundary = [b"\xef\xbb\xbf".as_slice(), plain].concat();
        boundary.resize(super::super::MAX_SCHEMA_BYTES, b' ');
        std::fs::write(&owned.0, &boundary).unwrap();
        assert_eq!(read_schema_file(&owned.0).unwrap(), expected);
        boundary.push(b' ');
        std::fs::write(&owned.0, &boundary).unwrap();
        assert!(read_schema_file(&owned.0).is_err());
    }
}
