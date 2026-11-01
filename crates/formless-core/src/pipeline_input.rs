//! Explicit client-local schema input. RPC callers send a Value, never a path.
use anyhow::{ensure, Context, Result};
use std::io::Read;
use std::path::Path;
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
    ensure!(
        bytes.len() <= super::MAX_SCHEMA_BYTES,
        "schema file exceeds the 64 KiB limit"
    );
    Ok(serde_json::from_slice(&bytes).context("--schema must contain one JSON Schema")?)
}
