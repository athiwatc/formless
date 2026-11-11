//! Stream entries, or select a bounded matching tail, from shared log readers.
use super::{log_start, LogEntry};
use formless_core::{logs, Paths};
use std::collections::VecDeque;
use std::io::{self, Write};
const CAP: usize = logs::MAX_LOG_LINE_BYTES;
const MARKER: &str = " [log entry truncated]";

#[derive(Debug, Default)]
pub(super) struct OutputSummary {
    pub entries: usize,
    pub read: logs::LogReadSummary,
    pub truncated_entries: usize,
}

struct Builder {
    entry: LogEntry,
    has_line: bool,
    cut: bool,
}
impl Builder {
    fn new(at: Option<chrono::DateTime<chrono::FixedOffset>>, level: u8) -> Self {
        Self {
            entry: LogEntry {
                at,
                level,
                text: String::new(),
            },
            has_line: false,
            cut: false,
        }
    }
    fn append(&mut self, line: &str) {
        if self.cut {
            return;
        }
        if self.has_line {
            self.entry.text.push('\n');
        }
        self.entry.text.push_str(line);
        self.has_line = true;
        if self.entry.text.len() > CAP {
            self.mark_cut();
        }
    }
    fn mark_cut(&mut self) {
        if self.cut {
            return;
        }
        let mut end = self.entry.text.len().min(CAP - MARKER.len());
        while !self.entry.text.is_char_boundary(end) {
            end -= 1;
        }
        self.entry.text.truncate(end);
        self.entry.text.push_str(MARKER);
        self.cut = true;
    }
}

fn emit(
    builder: Builder,
    keep: &impl Fn(&LogEntry) -> bool,
    out: &mut impl Write,
    summary: &mut OutputSummary,
) -> io::Result<()> {
    summary.truncated_entries += usize::from(builder.cut);
    if keep(&builder.entry) {
        writeln!(out, "{}", builder.entry.text)?;
        summary.entries += 1;
    }
    Ok(())
}

pub(super) fn history(
    paths: &Paths,
    lines: usize,
    keep: &impl Fn(&LogEntry) -> bool,
    out: &mut impl Write,
) -> io::Result<OutputSummary> {
    let mut summary = OutputSummary::default();
    if lines == 0 {
        let mut current: Option<Builder> = None;
        let read = logs::visit_lines(paths, false, |line| {
            if let Some((at, level)) = log_start(&line) {
                if let Some(previous) = current.take() {
                    emit(previous, keep, out, &mut summary)?;
                }
                current = Some(Builder::new(Some(at), level));
            }
            current
                .get_or_insert_with(|| Builder::new(None, 2))
                .append(&line);
            Ok(true)
        })?;
        if let Some(last) = current {
            emit(last, keep, out, &mut summary)?;
        }
        summary.read = read;
        return Ok(summary);
    }
    let mut pending = VecDeque::<String>::new();
    let mut pending_bytes = 0;
    let mut pending_cut = false;
    let mut selected = Vec::new();
    let read = logs::visit_lines(paths, true, |line| {
        if let Some((at, level)) = log_start(&line) {
            let mut entry = Builder::new(Some(at), level);
            entry.append(&line);
            for part in pending.drain(..) {
                entry.append(&part);
            }
            if pending_cut {
                entry.mark_cut();
            }
            pending_bytes = 0;
            pending_cut = false;
            summary.truncated_entries += usize::from(entry.cut);
            if keep(&entry.entry) {
                selected.push(entry.entry);
            }
            // Filtering can require older files. Timestamps can be out of
            // order, so never terminate merely because one predates --since.
            return Ok(selected.len() < lines);
        }
        pending_bytes += line.len() + usize::from(!pending.is_empty());
        pending.push_front(line);
        while pending_bytes > CAP && pending.len() > 1 {
            let removed = pending.pop_back().unwrap();
            pending_bytes -= removed.len() + 1;
            pending_cut = true;
        }
        Ok(true)
    })?;
    if !pending.is_empty() && selected.len() < lines {
        let mut entry = Builder::new(None, 2);
        for part in pending {
            entry.append(&part);
        }
        if pending_cut {
            entry.mark_cut();
        }
        summary.truncated_entries += usize::from(entry.cut);
        if keep(&entry.entry) {
            selected.push(entry.entry);
        }
    }
    for entry in selected.iter().rev() {
        writeln!(out, "{}", entry.text)?;
    }
    summary.entries = selected.len();
    summary.read = read;
    Ok(summary)
}

pub(super) fn appended(
    path: &std::path::Path,
    start: u64,
    end: u64,
    keep: &impl Fn(&LogEntry) -> bool,
    out: &mut impl Write,
) -> io::Result<OutputSummary> {
    let mut summary = OutputSummary::default();
    let mut current: Option<Builder> = None;
    let read = logs::visit_appended_lines(path, start, end, |line| {
        if let Some((at, level)) = log_start(&line) {
            if let Some(previous) = current.take() {
                emit(previous, keep, out, &mut summary)?;
            }
            current = Some(Builder::new(Some(at), level));
        }
        current
            .get_or_insert_with(|| Builder::new(None, 2))
            .append(&line);
        Ok(true)
    })?;
    if let Some(last) = current {
        emit(last, keep, out, &mut summary)?;
    }
    summary.read = read;
    Ok(summary)
}
