use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::Paths;

pub(crate) const KEEP_BYTES: u64 = 1_000_000;
pub(crate) const MAX_BYTES: u64 = 2 * KEEP_BYTES;

pub(crate) fn path(paths: &Paths, name: &str) -> PathBuf {
    paths.data_dir.join("mcp").join(format!("{name}.log"))
}

pub(crate) fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// Keep the log live while the MCP server runs. File locking coordinates
/// writes and compaction with other daemon tasks and offline garbage collection.
pub(crate) async fn capture(mut stderr: impl AsyncRead + Unpin, mut file: File) -> io::Result<()> {
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let n = stderr.read(&mut buffer).await?;
        if n == 0 {
            return Ok(());
        }
        let (next_file, next_buffer, result) = tokio::task::spawn_blocking(move || {
            let result = append(&mut file, &buffer[..n]);
            (file, buffer, result)
        })
        .await
        .map_err(io::Error::other)?;
        file = next_file;
        buffer = next_buffer;
        if let Err(error) = result {
            // A disk error must not leave the child waiting on a full pipe.
            let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
            return Err(error);
        }
    }
}

fn append(file: &mut File, bytes: &[u8]) -> io::Result<()> {
    file.lock()?;
    let result = (|| {
        let len = file.seek(SeekFrom::End(0))?;
        if len.saturating_add(bytes.len() as u64) > MAX_BYTES {
            trim(file)?;
            file.seek(SeekFrom::End(0))?;
        }
        file.write_all(bytes)
    })();
    let unlocked = file.unlock();
    result.and(unlocked)
}

/// Read only the bounded suffix, skipping a partial line when a newline exists.
fn tail(file: &mut File) -> io::Result<Vec<u8>> {
    let len = file.metadata()?.len();
    let start = len.saturating_sub(KEEP_BYTES);
    let mut begins_on_line = start == 0;
    if start > 0 {
        file.seek(SeekFrom::Start(start - 1))?;
        let mut preceding = [0];
        file.read_exact(&mut preceding)?;
        begins_on_line = preceding == [b'\n'];
    }
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.take(len - start).read_to_end(&mut bytes)?;
    if !begins_on_line {
        let first_line = bytes.iter().position(|&byte| byte == b'\n');
        let discard = first_line.map_or(0, |index| index + 1);
        bytes.drain(..discard);
    }
    Ok(bytes)
}

fn trim(file: &mut File) -> io::Result<()> {
    let bytes = tail(file)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&bytes)?;
    file.set_len(bytes.len() as u64)
}

pub(crate) fn read_lines(path: &Path, lines: usize) -> io::Result<Vec<String>> {
    let mut file = File::open(path)?;
    file.lock_shared()?;
    let bytes = tail(&mut file)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<_> = text.lines().rev().take(lines).map(str::to_owned).collect();
    lines.reverse();
    Ok(lines)
}

fn logs(paths: &Paths) -> impl Iterator<Item = (PathBuf, u64)> {
    fs::read_dir(paths.data_dir.join("mcp"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "log" || !entry.file_type().ok()?.is_file() {
                return None;
            }
            Some((path, entry.metadata().ok()?.len()))
        })
}

pub(crate) fn over_limit(paths: &Paths) -> usize {
    logs(paths).filter(|(_, len)| *len > MAX_BYTES).count()
}

pub(crate) fn compact_logs(paths: &Paths, limit: u64, dry_run: bool) -> io::Result<(usize, u64)> {
    let mut count = 0;
    let mut bytes = 0;
    for (path, len) in logs(paths).filter(|(_, len)| *len > limit) {
        if dry_run {
            count += 1;
            bytes += len - KEEP_BYTES;
            continue;
        }
        let mut file = match OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        file.lock()?;
        let len = file.metadata()?.len();
        if len > limit {
            trim(&mut file)?;
            count += 1;
            bytes += len - file.metadata()?.len();
        }
    }
    Ok((count, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn fixture() -> (PathBuf, Paths, PathBuf) {
        let base = std::env::temp_dir().join(format!("fl-mcp-log-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(base.join("data"), base.join("config")).unwrap();
        fs::create_dir_all(paths.data_dir.join("mcp")).unwrap();
        let log = path(&paths, "fixture");
        (base, paths, log)
    }

    #[tokio::test]
    async fn live_stderr_remains_bounded_across_restarts_and_keeps_the_latest_lines() {
        let (base, paths, log) = fixture();
        for pass in 0..2 {
            let (mut writer, reader) = tokio::io::duplex(32 * 1024);
            let log_file = open(&log).unwrap();
            let send = async move {
                let block = "fixture stderr payload\r\n".repeat(2200);
                for _ in 0..50 {
                    writer.write_all(block.as_bytes()).await.unwrap();
                }
                writer
                    .write_all(format!("previous {pass}\r\nlatest {pass}").as_bytes())
                    .await
                    .unwrap();
            };
            let (_, result) = tokio::join!(send, capture(reader, log_file));
            result.unwrap();
            assert!(fs::metadata(&log).unwrap().len() <= MAX_BYTES);
            assert_eq!(
                read_lines(&log, 2).unwrap(),
                [format!("previous {pass}"), format!("latest {pass}")]
            );
        }
        assert_eq!(over_limit(&paths), 0);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn newline_free_logs_retain_the_bounded_suffix() {
        let (base, paths, log) = fixture();
        let mut file = open(&log).unwrap();
        assert!(tail(&mut file).unwrap().is_empty());
        let bytes = vec![b'x'; KEEP_BYTES as usize + 1234];
        file.write_all(&bytes).unwrap();
        assert_eq!(compact_logs(&paths, KEEP_BYTES, true).unwrap(), (1, 1234));
        assert_eq!(fs::read(&log).unwrap(), bytes);
        assert_eq!(compact_logs(&paths, KEEP_BYTES, false).unwrap(), (1, 1234));
        assert_eq!(fs::read(&log).unwrap(), vec![b'x'; KEEP_BYTES as usize]);
        drop(file);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn legacy_log_tail_and_maintenance_skip_a_partial_first_line() {
        let (base, paths, log) = fixture();
        let mut file = open(&log).unwrap();
        file.seek(SeekFrom::Start(128 * MAX_BYTES)).unwrap();
        file.write_all(b"oversized leading line\r\nprevious\r\nlatest\n")
            .unwrap();
        assert_eq!(over_limit(&paths), 1);
        assert_eq!(read_lines(&log, 10).unwrap(), ["previous", "latest"]);
        let initial_size = file.metadata().unwrap().len();
        assert_eq!(
            compact_logs(&paths, MAX_BYTES, true).unwrap(),
            (1, initial_size - KEEP_BYTES)
        );
        assert_eq!(file.metadata().unwrap().len(), initial_size);
        assert_eq!(
            compact_logs(&paths, MAX_BYTES, false).unwrap(),
            (1, initial_size - b"previous\r\nlatest\n".len() as u64)
        );
        assert_eq!(fs::read(&log).unwrap(), b"previous\r\nlatest\n");
        assert_eq!(over_limit(&paths), 0);
        drop(file);
        fs::remove_dir_all(base).unwrap();
    }
}
