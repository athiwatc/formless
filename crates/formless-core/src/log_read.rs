//! Bounded physical-line readers; entry grouping belongs to the consumer.
use super::log_files;
use crate::Paths;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

pub const MAX_LOG_LINE_BYTES: usize = 64 * 1024;
const BLOCK: usize = 8192;
const LINE_MARKER: &str = " [log line truncated]";

#[derive(Debug, Default)]
pub struct LogReadSummary {
    pub bytes_read: u64,
    pub files_read: usize,
    pub truncated_lines: usize,
}

fn line_text(mut bytes: Vec<u8>, truncated: bool, newline: bool) -> (String, bool) {
    if newline && !truncated && bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    let cut = truncated || text.len() > MAX_LOG_LINE_BYTES;
    if cut {
        let mut end = text.len().min(MAX_LOG_LINE_BYTES - LINE_MARKER.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(LINE_MARKER);
    }
    (text, cut)
}

fn open(path: &Path) -> io::Result<Option<File>> {
    match File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io::Error::new(
            e.kind(),
            format!("read {}: {e}", path.display()),
        )),
    }
}

fn forward(
    mut reader: impl Read,
    summary: &mut LogReadSummary,
    visit: &mut impl FnMut(String) -> io::Result<bool>,
) -> io::Result<bool> {
    let mut block = [0; BLOCK];
    let mut line = Vec::new();
    let mut cut = false;
    let mut pending = false;
    loop {
        let n = reader.read(&mut block)?;
        summary.bytes_read += n as u64;
        if n == 0 {
            break;
        }
        for &b in &block[..n] {
            if b == b'\n' {
                let (text, truncated) = line_text(std::mem::take(&mut line), cut, true);
                summary.truncated_lines += usize::from(truncated);
                if !visit(text)? {
                    return Ok(false);
                }
                cut = false;
                pending = false;
            } else {
                pending = true;
                if line.len() < MAX_LOG_LINE_BYTES {
                    line.push(b);
                } else {
                    cut = true;
                }
            }
        }
    }
    if pending {
        let (text, truncated) = line_text(line, cut, false);
        summary.truncated_lines += usize::from(truncated);
        return visit(text);
    }
    Ok(true)
}

struct ReverseLines {
    file: File,
    len: u64,
    pos: u64,
    at: usize,
    block: Vec<u8>,
    line_end: u64,
    done: bool,
}

impl ReverseLines {
    fn new(file: File) -> io::Result<Self> {
        let len = file.metadata()?.len();
        Ok(Self {
            file,
            len,
            pos: len,
            at: 0,
            block: Vec::new(),
            line_end: len,
            done: len == 0,
        })
    }
    fn range(&mut self, start: u64, end: u64, summary: &mut LogReadSummary) -> io::Result<String> {
        let len = (end - start).min(MAX_LOG_LINE_BYTES as u64) as usize;
        let mut bytes = vec![0; len];
        self.file.seek(SeekFrom::Start(start))?;
        self.file.read_exact(&mut bytes)?;
        summary.bytes_read += len as u64;
        let (text, truncated) = line_text(bytes, end - start > len as u64, end < self.len);
        summary.truncated_lines += usize::from(truncated);
        Ok(text)
    }
    fn next(&mut self, summary: &mut LogReadSummary) -> io::Result<Option<String>> {
        if self.done {
            return Ok(None);
        }
        loop {
            if self.at == 0 {
                if self.pos == 0 {
                    self.done = true;
                    return self.range(0, self.line_end, summary).map(Some);
                }
                let start = self.pos.saturating_sub(BLOCK as u64);
                self.block.resize((self.pos - start) as usize, 0);
                self.file.seek(SeekFrom::Start(start))?;
                self.file.read_exact(&mut self.block)?;
                summary.bytes_read += self.block.len() as u64;
                self.pos = start;
                self.at = self.block.len();
            }
            self.at -= 1;
            let offset = self.pos + self.at as u64;
            if self.block[self.at] == b'\n' {
                // str::lines does not invent an empty line after a final LF.
                if offset + 1 == self.len {
                    self.line_end = offset;
                    continue;
                }
                let end = self.line_end;
                self.line_end = offset;
                return self.range(offset + 1, end, summary).map(Some);
            }
        }
    }
}

/// Visit physical lines without retaining the history. A false callback stops
/// immediately; newest-first reads only the reverse blocks needed for a tail.
pub fn visit_lines(
    paths: &Paths,
    newest_first: bool,
    mut visit: impl FnMut(String) -> io::Result<bool>,
) -> io::Result<LogReadSummary> {
    let mut files = log_files(paths)?;
    if !newest_first {
        files.reverse();
    }
    let mut summary = LogReadSummary::default();
    for path in files {
        let Some(file) = open(&path)? else { continue };
        summary.files_read += 1;
        if newest_first {
            let mut reader = ReverseLines::new(file)?;
            while let Some(line) = reader.next(&mut summary)? {
                if !visit(line)? {
                    return Ok(summary);
                }
            }
        } else if !forward(file, &mut summary, &mut visit)? {
            break;
        }
    }
    Ok(summary)
}

/// Stream a follow poll's byte range without a whole-append String allocation.
pub fn visit_appended_lines(
    path: &Path,
    start: u64,
    end: u64,
    mut visit: impl FnMut(String) -> io::Result<bool>,
) -> io::Result<LogReadSummary> {
    let mut summary = LogReadSummary::default();
    if end <= start {
        return Ok(summary);
    }
    let Some(mut file) = open(path)? else {
        return Ok(summary);
    };
    summary.files_read = 1;
    file.seek(SeekFrom::Start(start))?;
    forward(file.take(end - start), &mut summary, &mut visit)?;
    Ok(summary)
}

#[derive(Debug, Default)]
pub struct LogTail {
    pub lines: Vec<String>,
    pub bytes_read: usize,
    pub byte_limit_hit: bool,
    pub truncated_lines: usize,
}

/// Raw-line tail for bug reports, with a strict aggregate byte-read budget.
/// A leading fragment at the byte boundary is omitted, never presented as a
/// complete line (which could also lose its original redaction context).
pub fn tail_lines(paths: &Paths, count: usize, max_bytes: usize) -> io::Result<LogTail> {
    let mut tail = LogTail::default();
    if count == 0 {
        return Ok(tail);
    }
    for path in log_files(paths)? {
        if tail.bytes_read == max_bytes {
            tail.byte_limit_hit = true;
            break;
        }
        let Some(mut file) = open(&path)? else {
            continue;
        };
        let len = file.metadata()?.len();
        let n = len.min((max_bytes - tail.bytes_read) as u64) as usize;
        let start = len - n as u64;
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = vec![0; n];
        file.read_exact(&mut bytes)?;
        tail.bytes_read += n;
        let text = String::from_utf8_lossy(&bytes);
        let complete = if start > 0 {
            text.split_once('\n').map_or("", |(_, rest)| rest)
        } else {
            &text
        };
        for line in complete.lines().rev() {
            let (text, cut) = line_text(line.as_bytes().to_vec(), false, false);
            tail.truncated_lines += usize::from(cut);
            tail.lines.push(text);
            if tail.lines.len() == count {
                break;
            }
        }
        if tail.lines.len() == count {
            break;
        }
        if start > 0 {
            tail.byte_limit_hit = true;
            break;
        }
    }
    tail.lines.reverse();
    Ok(tail)
}
