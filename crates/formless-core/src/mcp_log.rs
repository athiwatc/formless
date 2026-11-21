//! Bounded stderr storage for local MCP servers.
use crate::{config::McpServerCfg, Paths};
use anyhow::{Context, Result};
use formless_runtime::LockExt as _;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::io::{AsyncRead, AsyncReadExt};

const FILE_LIMIT: usize = 1024 * 1024;
const TAIL_LIMIT: usize = 128 * 1024;
const LINE_LIMIT: usize = 16 * 1024;
const OMITTED_LINE: &[u8] = b"[An oversized stderr line was omitted.]\n";

pub(crate) struct LogFiles {
    pub(crate) current: PathBuf,
    previous: PathBuf,
}

fn stat(path: &Path) -> Result<Option<u64>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(Some(meta.len())),
        Ok(_) => anyhow::bail!("MCP stderr log must be a regular file: {}", path.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("read MCP stderr log metadata {}", path.display()))
        }
    }
}

fn remove_file(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("remove MCP stderr log {}", path.display()))
        }
    }
}

fn open_writer(path: &Path, append: bool) -> Result<File> {
    stat(path)?;
    let mut options = OpenOptions::new();
    options
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("write MCP stderr log {}", path.display()))
}

/// Read at most `budget` bytes, and only return full lines. A partial
/// fragment at the seek boundary can contain part of a credential.
fn suffix(path: &Path, budget: usize) -> Result<(Vec<u8>, u64, bool)> {
    let Some(_) = stat(path)? else {
        return Ok((Vec::new(), 0, false));
    };
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), 0, false)),
        Err(error) => {
            return Err(error).with_context(|| format!("open MCP stderr log {}", path.display()))
        }
    };
    let len = file.metadata()?.len();
    let start = len.saturating_sub(budget as u64);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(len - start).read_to_end(&mut bytes)?;
    if start > 0 {
        let after_first = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |index| index + 1);
        bytes.drain(..after_first);
    }
    let complete = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let partial_line = complete < bytes.len();
    bytes.truncate(complete);
    Ok((bytes, len, start > 0 || partial_line))
}

impl LogFiles {
    pub(crate) fn new(paths: &Paths, name: &str) -> Self {
        let stem = if !name.is_empty()
            && name.len() <= 120
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            name.to_string()
        } else {
            format!(".server-{}", crate::forecast::hash(&name))
        };
        let directory = paths.data_dir.join("mcp");
        Self {
            current: directory.join(format!("{stem}.log")),
            previous: directory.join(format!("{stem}.log.1")),
        }
    }

    pub(crate) fn remove(&self) -> Result<()> {
        remove_file(&self.current)?;
        remove_file(&self.previous)
    }

    fn rotate(&self, size: usize) -> Result<()> {
        if size > FILE_LIMIT {
            let (bytes, _, _) = suffix(&self.current, FILE_LIMIT)?;
            open_writer(&self.previous, false)?.write_all(&bytes)?;
            remove_file(&self.current)?;
        } else {
            remove_file(&self.previous)?;
            fs::rename(&self.current, &self.previous)?;
        }
        Ok(())
    }

    fn append(&self, records: &[Vec<u8>]) -> Result<()> {
        fs::create_dir_all(
            self.current
                .parent()
                .context("MCP log directory is missing")?,
        )?;
        let mut size = stat(&self.current)?
            .unwrap_or_default()
            .min(usize::MAX as u64) as usize;
        if size > FILE_LIMIT {
            self.rotate(size)?;
            size = 0;
        }
        let mut writer: Option<File> = None;
        for record in records {
            if size.saturating_add(record.len()) > FILE_LIMIT {
                writer.take();
                self.rotate(size)?;
                size = 0;
            }
            if writer.is_none() {
                writer = Some(open_writer(&self.current, true)?);
            }
            writer.as_mut().expect("opened above").write_all(record)?;
            size += record.len();
        }
        Ok(())
    }

    pub(crate) fn tail(
        &self,
        cfg: &McpServerCfg,
        lines: usize,
    ) -> Result<(Vec<String>, u64, bool)> {
        let (current, current_size, incomplete_current) = suffix(&self.current, TAIL_LIMIT)?;
        let available = current.iter().filter(|byte| **byte == b'\n').count();
        let (mut combined, previous_size, incomplete_previous) =
            if available < lines && !incomplete_current {
                suffix(
                    &self.previous,
                    TAIL_LIMIT.saturating_sub(current_size as usize),
                )?
            } else {
                (Vec::new(), stat(&self.previous)?.unwrap_or_default(), false)
            };
        combined.extend(current);
        let text = String::from_utf8_lossy(&combined);
        let safe = crate::mcp_catalog::redact_server_text(cfg, &text);
        let all: Vec<_> = safe.lines().collect();
        let tail = all[all.len().saturating_sub(lines)..]
            .iter()
            .map(|line| (*line).to_owned())
            .collect();
        Ok((
            tail,
            current_size.saturating_add(previous_size),
            incomplete_current || incomplete_previous,
        ))
    }
}

type Sink = Arc<Mutex<Option<LogFiles>>>;

pub(crate) struct Stderr {
    sink: Sink,
    reader: tokio::task::JoinHandle<()>,
}

impl Stderr {
    pub(crate) fn start(files: LogFiles, source: impl AsyncRead + Unpin + Send + 'static) -> Self {
        let sink = Arc::new(Mutex::new(Some(files)));
        Self {
            sink: sink.clone(),
            reader: tokio::spawn(pump(source, sink)),
        }
    }

    pub(crate) fn stop(&self) {
        self.sink.locked().take();
        self.reader.abort();
    }
}

async fn record(sink: &Sink, records: Vec<Vec<u8>>) -> Result<()> {
    let sink = sink.clone();
    tokio::task::spawn_blocking(move || {
        if let Some(files) = &*sink.locked() {
            files.append(&records)?;
        }
        Ok(())
    })
    .await?
}

async fn pump(mut source: impl AsyncRead + Unpin, sink: Sink) {
    let mut warned = false;
    let write = |result: Result<()>, warned: &mut bool| {
        if let Err(error) = result {
            if !*warned {
                tracing::warn!("cannot save MCP stderr: {error:#}");
                *warned = true;
            }
        }
    };
    write(record(&sink, Vec::new()).await, &mut warned);
    let mut buf = [0; 8192];
    let mut line = Vec::new();
    let mut oversized = false;
    while let Ok(n) = source.read(&mut buf).await {
        if n == 0 {
            break;
        }
        let mut records = Vec::new();
        for fragment in buf[..n].split_inclusive(|byte| *byte == b'\n') {
            oversized |= line.len() + fragment.len() >= LINE_LIMIT;
            if !oversized {
                line.extend(fragment);
            }
            if fragment.last() == Some(&b'\n') {
                records.push(if oversized {
                    OMITTED_LINE.to_vec()
                } else {
                    std::mem::take(&mut line)
                });
                line.clear();
                oversized = false;
            }
        }
        if !records.is_empty() {
            write(record(&sink, records).await, &mut warned);
        }
    }
    if oversized || !line.is_empty() {
        let final_line = if oversized {
            OMITTED_LINE.to_vec()
        } else {
            line.push(b'\n');
            line
        };
        write(record(&sink, vec![final_line]).await, &mut warned);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, daemon::Daemon};
    use formless_protocol::Request;
    use serde_json::json;

    fn fixture() -> (Paths, PathBuf) {
        let root = std::env::temp_dir().join(format!("fl-mcp-log-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        fs::create_dir_all(paths.data_dir.join("mcp")).unwrap();
        (paths, root)
    }

    #[test]
    fn bounded_tail_of_legacy_file_redacts_secrets_and_omits_fragments() {
        let (paths, root) = fixture();
        let files = LogFiles::new(&paths, "local");
        let mut file = File::create(&files.current).unwrap();
        file.set_len((FILE_LIMIT * 8) as u64).unwrap();
        file.seek(SeekFrom::End(0)).unwrap();
        file.write_all(b"\nfixture secret abcd\nlast complete line\ntrailing fragment")
            .unwrap();
        drop(file);
        let mut cfg = McpServerCfg::default();
        cfg.env.insert("KEY".into(), "abcd".into());
        let (lines, size, truncated) = files.tail(&cfg, 1000).unwrap();
        assert_eq!(lines, ["fixture secret [REDACTED]", "last complete line"]);
        assert!(truncated && size > FILE_LIMIT as u64 * 8);

        files.append(&[]).unwrap();
        assert!(!files.current.exists());
        assert!(stat(&files.previous).unwrap().unwrap() <= FILE_LIMIT as u64);
        files
            .append(&[b"written after restart\n".to_vec()])
            .unwrap();
        assert_eq!(
            files.tail(&cfg, 2).unwrap().0,
            ["last complete line", "written after restart"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn stderr_writer_bounds_files_and_discards_oversized_lines() {
        let (paths, root) = fixture();
        let mut input = b"normal line\n".repeat(FILE_LIMIT / 4);
        input.extend(std::iter::repeat_n(b'x', LINE_LIMIT * 5));
        input.extend(b"\nfinal record");
        let mut stderr = Stderr::start(LogFiles::new(&paths, "local"), std::io::Cursor::new(input));
        (&mut stderr.reader).await.unwrap();
        let files = LogFiles::new(&paths, "local");
        for path in [&files.current, &files.previous] {
            assert!(stat(path).unwrap().unwrap() <= FILE_LIMIT as u64);
        }
        let tail = files.tail(&McpServerCfg::default(), 2).unwrap().0;
        assert_eq!(
            tail,
            ["[An oversized stderr line was omitted.]", "final record"]
        );
        stderr.stop();
        files.remove().unwrap();
        record(&stderr.sink, vec![b"obsolete writer\n".to_vec()])
            .await
            .unwrap();
        assert!(!files.current.exists() && !files.previous.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn daemon_reports_remote_logs_and_readding_local_starts_empty() {
        let (paths, root) = fixture();
        let mut cfg = Config::default();
        let local = McpServerCfg {
            disabled: true,
            transport: "stdio".into(),
            ..Default::default()
        };
        let remote = McpServerCfg {
            disabled: true,
            transport: "http".into(),
            ..Default::default()
        };
        cfg.mcp.servers.insert("local".into(), local.clone());
        cfg.mcp.servers.insert("remote".into(), remote);
        cfg.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
        let remote = daemon
            .handle(Request::McpServerLog {
                name: "remote".into(),
                lines: None,
            })
            .await
            .unwrap();
        assert!(remote["file"].is_null());
        assert_eq!(
            remote["message"],
            "Remote MCP servers do not expose a stderr log."
        );

        let files = LogFiles::new(&paths, "local");
        fs::write(&files.current, b"old current\n").unwrap();
        fs::write(&files.previous, b"old rotated\n").unwrap();
        let local_log = || Request::McpServerLog {
            name: "local".into(),
            lines: None,
        };
        let before = daemon.handle(local_log()).await.unwrap();
        assert_eq!(before["lines"], json!(["old rotated", "old current"]));
        assert_eq!(before["bytes"], 24);
        let removed = daemon
            .handle(Request::RemoveMcpServer {
                name: "local".into(),
            })
            .await
            .unwrap();
        assert!(removed["log_error"].is_null());
        assert!(!files.current.exists() && !files.previous.exists());
        daemon
            .handle(Request::AddMcpServer {
                name: "local".into(),
                server: serde_json::to_value(local).unwrap(),
            })
            .await
            .unwrap();
        assert_eq!(
            daemon.handle(local_log()).await.unwrap()["lines"],
            json!([])
        );
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }
}
