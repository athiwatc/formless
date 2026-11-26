//! Plain, line-oriented mode for screen readers (SPEC §10.5): no
//! alternate screen, redraws, spinners or borders. New messages and events
//! are printed once, in order, and approvals and questions become plain
//! prompts answered on the next line.

use crate::client::DaemonClient;
use crate::text::escape_approval_controls;
use anyhow::Result;
use formless_core::{Config, Paths};
use formless_protocol::*;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[path = "plain_commands.rs"]
mod templates;

fn docs_lines(input: &str) -> Vec<String> {
    let (topic, text) = formless_core::docs::text_option(input);
    let mut lines = match formless_core::docs::resolve(Some(&topic)) {
        Ok(url) if text => match formless_core::docs::text(Some(&topic)) {
            Ok(body) => {
                let mut out = vec![format!(
                    "Bundled guide (Formless {}).",
                    env!("CARGO_PKG_VERSION")
                )];
                out.extend(body.lines().map(str::to_owned));
                out.push(format!(
                    "Current online copy (may differ from this build): {url}"
                ));
                out
            }
            Err(error) => vec![error.to_string()],
        },
        Ok(url) => vec![format!(
            "Current online copy (may differ from this build): {url}"
        )],
        Err(error) => vec![error.to_string()],
    };
    let topics = formless_core::docs::TOPICS
        .iter()
        .map(|(name, _, what)| format!("{name} ({what})"))
        .collect::<Vec<_>>();
    lines.push(format!("Topics: {}.", topics.join(", ")));
    lines.push("Read bundled Markdown with /docs --text [topic].".into());
    lines
}

pub(super) fn display_text(text: &str) -> String {
    text.chars().flat_map(|c| {
        if c.is_control() || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
            c.escape_default().collect::<Vec<_>>()
        } else { vec![c] }
    }).collect()
}

fn invocation_label(id: Option<&str>) -> String {
    id.map(|id| format!(" [{}]", display_text(id)))
        .unwrap_or_default()
}

pub(super) fn progress_diagnostic(fraction: Option<f64>, label: Option<&str>) -> String {
    if fraction.is_none() && label.is_none() {
        return String::new();
    }
    format!(
        " Last progress: {}{}.",
        fraction
            .map(|f| format!("{}%", (f * 100.0).round()))
            .unwrap_or_else(|| "unknown".into()),
        label
            .map(|l| format!(" ({})", display_text(l)))
            .unwrap_or_default()
    )
}

/// Stateful, bounded feedback shared with headless stderr. Legacy progress
/// without an invocation ID cannot safely distinguish concurrent calls.
#[derive(Default)]
pub struct LiveFeedback {
    progress: HashMap<(String, String), (Instant, Option<u8>, String)>,
}
impl LiveFeedback {
    pub fn event(&mut self, ev: &Event) -> Option<String> {
        self.event_with_suffix(ev, None)
    }

    fn event_with_suffix(&mut self, ev: &Event, suffix: Option<&str>) -> Option<String> {
        match ev {
            Event::ToolCallFinished {
                run_id,
                invocation_id: Some(id),
                ..
            } => {
                self.progress.remove(&(run_id.clone(), id.clone()));
                None
            }
            Event::InvocationCompleted {
                invocation_id: Some(id),
                ..
            } => {
                self.progress.retain(|(_, invocation), _| invocation != id);
                None
            }
            Event::ToolProgress {
                run_id,
                invocation_id: Some(id),
                tool_name,
                fraction,
                label,
                depth,
                ..
            } if *depth == 0 => {
                let key = (run_id.clone(), id.clone());
                let percent = fraction
                    .filter(|f| f.is_finite())
                    .map(|f| (f.clamp(0.0, 1.0) * 100.0).round() as u8);
                let label = display_text(&label.chars().take(160).collect::<String>());
                let now = Instant::now();
                if let Some((at, old, old_label)) = self.progress.get(&key) {
                    let milestone = percent.is_some_and(|p| {
                        p == 100 && *old != Some(100) || old.is_some_and(|o| p / 25 > o / 25)
                    });
                    if (*old == percent && *old_label == label)
                        || (!milestone && now.duration_since(*at) < Duration::from_secs(2))
                    {
                        return None;
                    }
                } else if self.progress.len() >= 1024 {
                    // Keep existing identities intact; never attribute an unknown
                    // invocation's progress to another invocation with the same name.
                    return None;
                }
                self.progress.insert(key, (now, percent, label.clone()));
                Some(format!(
                    "Tool progress: {}{}: {}{}.",
                    display_text(tool_name),
                    suffix
                        .map(std::borrow::Cow::Borrowed)
                        .unwrap_or_else(|| { std::borrow::Cow::Owned(invocation_label(Some(id))) }),
                    percent.map_or_else(|| "working".into(), |p| format!("{p}%")),
                    if label.is_empty() {
                        String::new()
                    } else {
                        format!(" ({label})")
                    }
                ))
            }
            Event::PlanUpdated {
                items, plan_state, ..
            } => Some(plan_text(items, plan_state.as_ref())),
            Event::RunRecap { recap, .. } => Some(recap_text(recap)),
            _ => None,
        }
    }
}

/// The plain client needs an audible distinction only for calls that
/// overlap. Preserve the distinction until both lifecycles are announced.
#[derive(Default)]
struct PlainFeedback {
    live: LiveFeedback,
    calls: HashMap<String, PlainCall>,
    next: usize,
}

struct PlainCall {
    run: Option<String>,
    name: String,
    number: usize,
    numbered: bool,
    pending: bool,
}

impl PlainFeedback {
    fn notice(&mut self, ev: &Event, streaming: &mut bool) -> Option<String> {
        let label = self
            .call_number(ev)
            .map_or_else(String::new, |n| format!(" #{n}"));
        let live = self.live.event_with_suffix(ev, Some(&label));
        if matches!(ev, Event::ToolProgress { .. }) {
            live.map(|text| {
                if std::mem::take(streaming) {
                    format!("\n{text}")
                } else {
                    text
                }
            })
        } else {
            format_event(ev, streaming, &label)
        }
    }

    fn discover(
        &mut self,
        id: &str,
        name: &str,
        run: Option<&str>,
        pending: bool,
    ) -> Option<usize> {
        if let Some(call) = self.calls.get_mut(id) {
            call.pending |= pending;
            return call.numbered.then_some(call.number);
        }
        if self.calls.len() >= 1024 {
            return None;
        }
        self.next = self.next.saturating_add(1);
        let mut numbered = false;
        for call in self.calls.values_mut().filter(|call| call.name == name) {
            call.numbered = true;
            numbered = true;
        }
        self.calls.insert(
            id.to_owned(),
            PlainCall {
                run: run.map(str::to_owned),
                name: name.to_owned(),
                number: self.next,
                numbered,
                pending,
            },
        );
        numbered.then_some(self.next)
    }

    fn call_number(&mut self, ev: &Event) -> Option<usize> {
        match ev {
            Event::ToolCallStarted {
                invocation_id: Some(id),
                tool_name,
                run_id,
                depth: 0,
                ..
            }
            | Event::ToolProgress {
                invocation_id: Some(id),
                tool_name,
                run_id,
                depth: 0,
                ..
            } => self.discover(id, tool_name, Some(run_id), false),
            Event::InvocationPending {
                invocation_id: Some(id),
                tool_name,
                run_id,
                ..
            } => self.discover(id, tool_name, Some(run_id), true),
            Event::InvocationStalled {
                invocation_id: Some(id),
                tool_name,
                ..
            } => self.discover(id, tool_name, None, true),
            Event::ToolCallFinished {
                invocation_id: Some(id),
                ..
            } => {
                let call = self.calls.get(id)?;
                let label = call.numbered.then_some(call.number);
                if !call.pending {
                    self.calls.remove(id);
                }
                label
            }
            Event::InvocationCompleted {
                invocation_id: Some(id),
                ..
            } => self
                .calls
                .remove(id)
                .and_then(|call| call.numbered.then_some(call.number)),
            Event::RunFinished { run_id, .. } | Event::RunPaused { run_id, .. } => {
                self.calls
                    .retain(|_, call| call.pending || call.run.as_deref() != Some(run_id));
                None
            }
            _ => None,
        }
    }
}

pub fn plan_text(items: &[PlanItem], state: Option<&PlanState>) -> String {
    let done = items.iter().filter(|i| i.status == "completed").count();
    let current = items
        .iter()
        .find(|i| i.status == "in_progress")
        .map(|i| format!(" Current: {}.", display_text(&i.content)))
        .unwrap_or_default();
    let lifecycle = state
        .map(|s| {
            format!(
                "{}{}{}",
                s.run_status
                    .as_deref()
                    .map(|v| format!("; {}", display_text(v)))
                    .unwrap_or_default(),
                if s.previous_task {
                    "; previous task"
                } else {
                    ""
                },
                if s.origin.is_empty() {
                    String::new()
                } else {
                    format!("; {}", display_text(&s.origin))
                }
            )
        })
        .unwrap_or_default();
    format!("Plan: {done}/{} done{lifecycle}.{current}", items.len())
}

pub fn recap_text(recap: &RunRecap) -> String {
    let finished = recap
        .total_calls
        .and_then(|total| total.checked_sub(recap.unfinished_calls))
        .filter(|count| *count >= 0)
        .map_or_else(|| "unknown".into(), |count| count.to_string());
    let mut text = format!("What I did: status {}; {} steps; {} ms; {} total calls; {finished} finished calls; {} unfinished calls; {} steering message(s).", if recap.status.is_empty() { "unknown (legacy)".into() } else { display_text(&recap.status) }, recap.steps, recap.duration_ms, recap.total_calls.map_or_else(|| "unknown".into(), |n| n.to_string()), recap.unfinished_calls, recap.steered);
    for (title, values) in [
        ("Used", &recap.used),
        ("Built", &recap.built),
        ("Files", &recap.files),
    ] {
        text.push_str(&format!(
            "\n{title}: {}",
            if values.is_empty() {
                "none recorded".into()
            } else {
                values
                    .iter()
                    .map(|v| display_text(v))
                    .collect::<Vec<_>>()
                    .join("; ")
            }
        ));
    }
    text.push_str("\nCoverage: tracked paths only.");
    if recap.untracked_writes_possible {
        text.push(' ');
        text.push_str(super::RUN_FILES_CAVEAT);
    }
    text
}

/// Full path identities and diffs, rather than the abbreviated live notice.
pub fn files_result_text(result: &serde_json::Value) -> Result<String> {
    let files: Vec<RunFileInfo> = serde_json::from_value(result["files"].clone())?;
    let mut text = format!(
        "Run {}. Tracked paths only.",
        display_text(result["run_id"].as_str().unwrap_or("unknown"))
    );
    for file in files {
        text.push_str(&format!(
            "\n{}: {} (restorable: {})",
            display_text(&file.path),
            display_text(&file.status),
            file.revertible
        ));
        if let Some(reason) = file.revert_reason {
            text.push_str(&format!("; {}", display_text(&reason)));
        }
        if let Some(diff) = file.diff {
            text.push_str(&format!(
                "\n{}",
                diff.lines()
                    .map(display_text)
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
    }
    if result["untracked_writes_possible"]
        .as_bool()
        .unwrap_or(false)
    {
        text.push(' ');
        text.push_str(super::RUN_FILES_CAVEAT);
    }
    Ok(text)
}

pub fn rewind_result_text(result: &serde_json::Value) -> String {
    let mut text = String::from("Rewind result:");
    for path in result["restored"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str())
    {
        text.push_str(&format!("\nRestored: {}", display_text(path)));
    }
    for item in result["skipped"].as_array().into_iter().flatten() {
        text.push_str(&format!(
            "\nNot restored: {} ({})",
            display_text(item["path"].as_str().unwrap_or("unknown")),
            display_text(item["reason"].as_str().unwrap_or("unknown"))
        ));
    }
    text
}

#[derive(Default)]
struct EventBacklog {
    messages: VecDeque<ServerMessage>,
    bytes: usize,
}
impl EventBacklog {
    fn push(&mut self, message: ServerMessage) -> Result<()> {
        let bytes = serde_json::to_vec(&message)?.len();
        anyhow::ensure!(self.messages.len() < 4096 && self.bytes.saturating_add(bytes) <= 4 * 1024 * 1024, "plain event backlog exceeded 4096 messages or 4 MiB; reconnect to resynchronize (events were not silently discarded)");
        self.bytes += bytes;
        self.messages.push_back(message);
        Ok(())
    }
    fn pop(&mut self) -> Option<ServerMessage> {
        let message = self.messages.pop_front()?;
        self.bytes = self
            .bytes
            .saturating_sub(serde_json::to_vec(&message).map_or(0, |v| v.len()));
        Some(message)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct UnfinishedRun {
    run_id: String,
    status: String,
    error: String,
}
impl UnfinishedRun {
    fn from_value(value: &serde_json::Value) -> Option<Self> {
        let status = value["status"].as_str()?;
        if !matches!(status, "paused" | "interrupted") {
            return None;
        }
        let raw = value["error"].as_str().unwrap_or_default();
        let mut error = display_text(&raw.chars().take(2048).collect::<String>());
        if raw.chars().count() > 2048 {
            error.push_str("… [reason truncated]");
        }
        Some(Self {
            run_id: value["run_id"].as_str().unwrap_or_default().into(),
            status: status.into(),
            error,
        })
    }
    fn text(&self) -> String {
        let reason = if self.error.is_empty() {
            String::new()
        } else {
            format!(": {}", self.error)
        };
        format!(
            "Run {}{reason}\nResume this Session in the TUI or web after reviewing the cause.",
            self.status
        )
    }
}

/// Persisted pause reason for human attach views. It does not invent a live
/// event, a current provider request or an explanation missing from storage.
pub fn unfinished_run_text(value: &serde_json::Value) -> Option<String> {
    UnfinishedRun::from_value(value).map(|run| run.text())
}

/// Snapshot replies are accepted only if no live Run/plan event has arrived
/// since the request. An older terminal event cannot replace a newer Run.
#[derive(Default)]
struct RunState {
    generation: u64,
    run_id: Option<String>,
    recap: Option<RecordedRunRecap>,
    plan: Vec<PlanItem>,
    plan_state: Option<PlanState>,
    unfinished: Option<UnfinishedRun>,
}
impl RunState {
    fn accept(&mut self, event: &Event) -> bool {
        match event {
            Event::RunStarted { run_id, .. } => {
                self.generation = self.generation.wrapping_add(1);
                self.run_id = Some(run_id.clone());
                self.recap = None;
                self.unfinished = None;
            }
            Event::RunRecap { run_id, recap, .. } => {
                if self.run_id.as_ref().is_some_and(|id| id != run_id)
                    || self.recap.as_ref().is_some_and(|r| r.run_id == *run_id)
                {
                    return false;
                }
                self.generation = self.generation.wrapping_add(1);
                self.recap = Some(RecordedRunRecap {
                    run_id: run_id.clone(),
                    recap: recap.clone(),
                });
            }
            Event::PlanUpdated {
                items, plan_state, ..
            } => {
                if plan_state.as_ref().is_some_and(|s| {
                    !s.previous_task
                        && s.run_id
                            .as_ref()
                            .zip(self.run_id.as_ref())
                            .is_some_and(|(a, b)| a != b)
                }) {
                    return false;
                }
                self.generation = self.generation.wrapping_add(1);
                self.plan = items.clone();
                self.plan_state = plan_state.clone();
            }
            Event::RunPaused { run_id, error, .. } => {
                if self.run_id.as_ref().is_some_and(|id| id != run_id) {
                    return false;
                }
                self.generation = self.generation.wrapping_add(1);
                self.unfinished = UnfinishedRun::from_value(
                    &serde_json::json!({"run_id":run_id,"status":"paused","error":error}),
                );
            }
            Event::RunFinished { run_id, .. } => {
                if self.run_id.as_ref().is_some_and(|id| id != run_id) {
                    return false;
                }
                self.generation = self.generation.wrapping_add(1);
            }
            _ => {}
        }
        true
    }
    fn snapshot(
        &mut self,
        generation: u64,
        value: &serde_json::Value,
        explicit: bool,
    ) -> Vec<String> {
        if self.generation != generation {
            return if explicit {
                self.recap
                    .as_ref()
                    .map(|r| vec![recap_text(&r.recap)])
                    .unwrap_or_else(|| vec!["No recorded recap for the latest Run.".into()])
            } else {
                Vec::new()
            };
        }
        let mut lines = Vec::new();
        self.run_id = value["latest_run"]["run_id"].as_str().map(str::to_owned);
        if !explicit {
            let unfinished = if value["running"] == true {
                None
            } else {
                UnfinishedRun::from_value(&value["unfinished_run"])
            };
            if unfinished != self.unfinished {
                if let Some(run) = &unfinished {
                    lines.push(run.text());
                }
            }
            self.unfinished = unfinished;
        }
        if let Ok(items) = serde_json::from_value::<Vec<PlanItem>>(value["plan"].clone()) {
            let state = serde_json::from_value::<PlanState>(value["plan_state"].clone()).ok();
            if !explicit && !items.is_empty() && (self.plan != items || self.plan_state != state) {
                lines.push(plan_text(&items, state.as_ref()));
            }
            self.plan = items;
            self.plan_state = state;
        }
        let recap = serde_json::from_value::<RecordedRunRecap>(value["last_recap"].clone()).ok();
        if let Some(recorded) = &recap {
            if explicit || self.recap.as_ref() != Some(recorded) {
                lines.push(recap_text(&recorded.recap));
            }
        } else if explicit {
            lines.push("No recorded recap for the latest Run.".into());
        }
        self.recap = recap;
        lines
    }
}

fn wait_response(
    rx: &mpsc::Receiver<ServerMessage>,
    sends: &std::cell::RefCell<crate::client::SendTracker>,
    backlog: &std::cell::RefCell<EventBacklog>,
    id: u64,
) -> Result<serde_json::Value> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let message = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))?;
        if let ServerMessage::Response {
            id: rid,
            ok,
            result,
            error,
        } = &message
        {
            sends.borrow_mut().answered(*rid);
            if *rid == id {
                return if *ok {
                    Ok(result.clone().unwrap_or_default())
                } else {
                    Err(anyhow::anyhow!(error.clone().unwrap_or_default()))
                };
            }
        }
        backlog.borrow_mut().push(message)?;
    }
}

/// Something waiting for the user's next line.
enum Prompt {
    Approval {
        id: String,
        promotion: bool,
    },
    Question {
        id: String,
        questions: Vec<Question>,
        answers: Vec<QuestionAnswer>,
    },
}

/// Submission and application are distinct: an open prompt may have been
/// cancelled or answered by another client before this request is processed.
#[derive(Clone, Copy)]
enum PendingReply {
    Approval(bool),
    Promotion,
    Question,
}
#[derive(Default)]
struct ReplyAcks(HashMap<u64, PendingReply>);
impl ReplyAcks {
    fn has_room(&self) -> bool {
        self.0.len() < 128
    }
    fn insert(&mut self, request: u64, reply: PendingReply) {
        self.0.insert(request, reply);
    }
    fn response(
        &mut self,
        request: u64,
        ok: bool,
        result: Option<&serde_json::Value>,
        error: Option<&str>,
    ) -> Option<String> {
        let reply = self.0.remove(&request)?;
        let (kind, field) = match reply {
            PendingReply::Approval(_) => ("Approval decision", "resolved"),
            PendingReply::Promotion => ("Promotion decision", "resolved"),
            PendingReply::Question => ("Answer", "answered"),
        };
        Some(if !ok {
            format!(
                "{kind} was not applied: {}",
                display_text(error.unwrap_or("request failed"))
            )
        } else if result.and_then(|v| v[field].as_bool()) == Some(true) {
            match reply {
                PendingReply::Approval(true) => "Approved.".into(),
                PendingReply::Approval(false) => "Denied.".into(),
                PendingReply::Promotion => "Promotion decision accepted.".into(),
                PendingReply::Question => "Answered.".into(),
            }
        } else {
            format!("{kind} was not applied: the prompt was already closed, cancelled, or resolved elsewhere.")
        })
    }
}

fn remove_resolved_prompt(prompts: &mut VecDeque<Prompt>, ev: &Event) {
    prompts.retain(|prompt| !match (prompt, ev) {
        (
            Prompt::Approval {
                id,
                promotion: true,
            },
            Event::PromotionApprovalResolved { promotion_id },
        ) => id == promotion_id,
        (
            Prompt::Approval {
                id,
                promotion: false,
            },
            Event::ApprovalAnswered { approval_id, .. },
        ) => id == approval_id,
        (
            Prompt::Approval {
                id,
                promotion: false,
            },
            Event::ApprovalClosed { approval_id },
        ) => id == approval_id,
        (Prompt::Question { id, .. }, Event::UserQuestionClosed { question_id, .. }) => {
            id == question_id
        }
        _ => false,
    });
}

#[derive(Clone, Copy, PartialEq)]
enum GapRefresh {
    Usage,
    Invocations,
}

fn gap_requests(ev: &Event) -> Vec<(GapRefresh, Request)> {
    let mut requests = Vec::new();
    if let Event::DeltasDropped {
        session_id, kinds, ..
    } = ev
    {
        if kinds.contains(&DroppedEventKind::RunUsage) && !session_id.is_empty() {
            requests.push((
                GapRefresh::Usage,
                Request::SessionUsage {
                    session_id: session_id.clone(),
                },
            ));
        }
        if kinds.contains(&DroppedEventKind::InvocationActivity) {
            requests.push((GapRefresh::Invocations, Request::ListInvocations));
        }
    }
    requests
}

fn gap_result(kind: GapRefresh, result: &serde_json::Value) -> String {
    match kind {
        GapRefresh::Usage => {
            match serde_json::from_value::<UsageInfo>(result["last_run"].clone()) {
                Ok(usage) => format!("Recorded usage: {}.", crate::usage_text(&usage)),
                Err(_) => "No recorded Run usage is available yet.".into(),
            }
        }
        GapRefresh::Invocations => {
            match serde_json::from_value::<Vec<InvocationInfo>>(result.clone()) {
                Ok(list) => format!(
                    "Recorded tools: {} running, {} stalled.",
                    list.len(),
                    list.iter().filter(|i| i.stalled).count()
                ),
                Err(_) => "Recorded tool activity could not be refreshed.".into(),
            }
        }
    }
}

/// The printed form of an event, if it has one. `streaming` says whether
/// the answer is mid-stream (deltas are printed as they come).
pub(crate) fn format_event(ev: &Event, streaming: &mut bool, call_label: &str) -> Option<String> {
    let end_stream = |streaming: &mut bool| {
        if std::mem::take(streaming) {
            "\n"
        } else {
            ""
        }
    };
    Some(match ev {
        Event::RunStarted { .. } => "Working…".into(),
        Event::MessageDelta { delta, .. } => {
            let lead = if *streaming { "" } else { "Formless: " };
            *streaming = true;
            return Some(format!("{lead}{delta}"));
        }
        Event::MessageComplete { .. } => {
            return (*streaming).then(|| end_stream(streaming).to_string());
        }
        Event::MessageAppended { message, .. }
            if message.role == "assistant"
                && message.meta.as_deref().and_then(|meta| serde_json::from_str::<serde_json::Value>(meta).ok())
                    .is_some_and(|meta| meta["harness"].is_string()) =>
        {
            format!("{}Formless: {}", end_stream(streaming), message.content)
        }
        // Scripted output is never silently wrong (#1096).
        Event::DeltasDropped { .. } => {
            eprintln!("\n[display fell behind: live text, reasoning or progress updates were skipped; final answers remain in the transcript; recorded usage and tool activity are refreshed when affected]");
            return None;
        }
        Event::ToolCallStarted {
            tool_name, depth, ..
        } if *depth == 0 => format!("{}Tool started: {tool_name}{}.", end_stream(streaming), call_label),
        Event::ToolCallFinished { tool_name, ok, .. } => format!(
            "Tool {}: {tool_name}{}.",
            if *ok { "finished" } else { "failed" }, call_label
        ),
        // A paused Run was already reported by RunPaused.
        Event::RunFinished { status, .. } if status == "paused" => return None,
        Event::RunFinished { status, .. } => {
            format!("{}Run {status}.", end_stream(streaming))
        }
        Event::RunPaused {
            error, explained, ..
        } => {
            let msg = explained
                .as_ref()
                .map_or(error.as_str(), |x| x.message.as_str());
            let next = explained
                .as_ref()
                .and_then(|x| x.action.as_ref())
                .map(|a| format!(" Next: {a}."))
                .unwrap_or_default();
            format!("{}Run paused: {msg}{next}", end_stream(streaming))
        }
        Event::Note { text, .. } => format!("Note: {text}"),
        Event::RunFilesChanged {
            files,
            untracked_writes_possible,
            ..
        } => format!(
            "{}{}",
            end_stream(streaming),
            super::run_files_text(files, *untracked_writes_possible)
        ),
        Event::RunRecap { recap, .. } => format!("{}{}", end_stream(streaming), recap_text(recap)),
        Event::PlanUpdated { items, plan_state, .. } => format!("{}{}", end_stream(streaming), plan_text(items, plan_state.as_ref())),
        Event::RunToolChanges { changes, .. } => format!("{}.", super::tool_changes_text(changes)),
        Event::CompactionNotice { .. } => "Note: the conversation was compacted.".into(),
        Event::ApprovalRequested {
            context,
            tool_name,
            capabilities,
            detail,
            ..
        } => {
            let what = match detail {
                Some(d) => d.clone(),
                None => format!(
                    "Tool {tool_name} wants: {}",
                    if capabilities.is_empty() {
                        "no capabilities".to_string()
                    } else {
                        capabilities
                            .iter()
                            .map(|c| format!("{c} ({})", crate::ui::explain_capability(c)))
                            .collect::<Vec<_>>()
                            .join("; ")
                    }
                ),
            };
            let mut evidence = context
                .as_ref()
                .map(formless_protocol::approval_context_lines)
                .unwrap_or_default();
            for capability in capabilities {
                if let Some(warning) = formless_protocol::sensitive_read_warning(capability) {
                    evidence.push(format!("⚠ {capability}: {warning}"));
                }
            }
            let evidence = evidence.join("\n");
            let what = if evidence.is_empty() {
                what
            } else {
                format!("{what}\n{evidence}")
            };
            let what = escape_approval_controls(&what);
            format!(
                "{}Approval needed. {what}. Type y to always allow, o for this once, s for this Session, h for an hour, or n to deny.",
                end_stream(streaming)
            )
        }
        Event::PromotionApprovalRequested {
            tool_name, reason, ..
        } => format!(
            "{}Promotion of {} needs approval: {reason}. Type y to approve or n to deny.",
            end_stream(streaming), escape_approval_controls(tool_name)
        ),
        Event::ApprovalClosed { .. } => format!("{}Approval prompt closed.", end_stream(streaming)),
        Event::PromotionApprovalResolved { .. } => {
            "The promotion approval prompt was resolved.".into()
        }
        // The partial answer printed so far is void: say so on its own
        // line, so the re-sent answer never reads as its continuation (#890).
        Event::StreamRestarted { .. } => format!(
            "{}[The provider connection dropped; the answer starts again below.]",
            end_stream(streaming)
        ),
        Event::ProviderStalled { idle_secs, .. } => format!(
            "{}Note: the model has sent nothing for {idle_secs} s. /stop stops the Run.",
            end_stream(streaming)
        ),
        Event::InvocationStalled {
            tool_name,
            last_fraction,
            last_label,
            silent_secs,
            ..
        } => format!(
            "{}Note: Tool {tool_name}{} has shown no advancing progress or activity for {silent_secs} s; it keeps running.{}",
            end_stream(streaming), call_label, progress_diagnostic(*last_fraction, last_label.as_deref())
        ),
        Event::InvocationCompleted { tool_name, ok, .. } => format!(
            "{}Background Tool {tool_name}{} {}.",
            end_stream(streaming), call_label,
            if *ok { "finished" } else { "failed" }
        ),
        Event::InvocationPending { tool_name, handle, .. } => format!("{}Tool {tool_name}{} continues in the background ({}).", end_stream(streaming), call_label, display_text(handle)),
        // Stateful progress is printed by LiveFeedback. Reasoning, activity,
        // usage, settings and subscription chatter stay out of line mode.
        // A new event lands here by default: decide whether it belongs.
        _ => return None,
    })
}

/// A question as plain text: its number, text and numbered options.
pub(crate) fn question_text(q: &Question, n: usize, total: usize) -> String {
    let mut s = String::from("Question");
    if total > 1 {
        s.push_str(&format!(" {n} of {total}"));
    }
    s.push_str(&format!(": {}", q.question));
    for (i, o) in q.options.iter().enumerate() {
        s.push_str(&format!("\n  {}. {}", i + 1, o.label));
        if let Some(d) = &o.description {
            s.push_str(&format!(", {d}"));
        }
    }
    s.push_str(match (q.options.is_empty(), q.multi_select) {
        (true, _) => "\nType your answer.",
        (false, false) => "\nType an option number or your own answer.",
        (false, true) => "\nType option numbers separated by commas, or your own answer.",
    });
    s
}

pub fn run(paths: Paths) -> Result<()> {
    run_with(paths, ChatStart::Recent, "plain mode")
}

/// Which Session a line-mode chat opens on.
pub enum ChatStart {
    /// The most recently used one (or a new one when there are none).
    Recent,
    New,
    Session(String),
}

/// The line-oriented client: the TUI's `--plain`/screen-reader mode and
/// `formless chat` (SPEC §10.5).
pub fn run_with(paths: Paths, start: ChatStart, label: &str) -> Result<()> {
    let _ = Config::persist_naming_download_migration(&paths);
    let cfg = Config::load(&paths)?;
    if !cfg.provider.is_configured() {
        return Err(anyhow::anyhow!(
            formless_core::profile_setup::missing_setup_message(&paths)
        ));
    }
    formless_core::daemon::ensure_daemon(&paths)?;
    // Told in words here too: an older or rebuilt daemon, or one without
    // the approvals this client asked for (#845).
    if let Some(warning) = crate::client::check_daemon_version(&paths)? {
        println!("Warning: {warning}");
    }
    let (client, rx) = DaemonClient::connect(&paths)?;
    let sends = std::cell::RefCell::new(crate::client::SendTracker::default());
    let backlog = std::cell::RefCell::new(EventBacklog::default());
    let wait = |id: u64| wait_response(&rx, &sends, &backlog, id);
    let create = || -> Result<SessionInfo> {
        Ok(serde_json::from_value(wait(
            client.send(Request::CreateSession { name: None }),
        )?)?)
    };
    let mut session = match start {
        // Continue the most recent Session, or start one.
        ChatStart::Recent => {
            let sessions: Vec<SessionInfo> =
                serde_json::from_value(wait(client.send(Request::ListSessions))?)
                    .unwrap_or_default();
            match sessions.into_iter().next() {
                Some(s) => s,
                None => create()?,
            }
        }
        ChatStart::New => create()?,
        ChatStart::Session(id) => {
            let v = wait(client.send(Request::GetSession { id: id.clone() }))
                .map_err(|e| anyhow::anyhow!("Session {id}: {e}"))?;
            serde_json::from_value(v["session"].clone())?
        }
    };
    let say = |s: &str| {
        println!("{s}");
        let _ = std::io::stdout().flush();
    };
    say(&format!(
        "Formless, {label}. Session: {}. Type a message and press Enter; /help lists commands.",
        session.name
    ));
    client.send(Request::Subscribe {
        sessions: Some(vec![session.id.clone()]),
        deltas: true,
        client: Some("TUI (plain)".into()),
    });

    let (line_tx, lines) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut prompts: VecDeque<Prompt> = VecDeque::new();
    let mut reply_acks = ReplyAcks::default();
    let mut streaming = false;
    let mut running = false;
    let mut feedback = PlainFeedback::default();
    let mut template_diagnostics = VecDeque::new();
    let mut run_state = RunState::default();
    let mut snapshots = HashMap::new();
    snapshots.insert(
        client.send(Request::GetSession {
            id: session.id.clone(),
        }),
        (session.id.clone(), run_state.generation, false),
    );
    let mut gap_refreshes: HashMap<u64, GapRefresh> = HashMap::new();
    loop {
        loop {
            let msg = backlog.borrow_mut().pop().or_else(|| rx.try_recv().ok());
            let Some(msg) = msg else { break };
            if let ServerMessage::Response {
                id,
                ok,
                error,
                result,
                ..
            } = &msg
            {
                sends.borrow_mut().answered(*id);
                let reply_notice = reply_acks.response(*id, *ok, result.as_ref(), error.as_deref());
                let handled_reply = reply_notice.is_some();
                if let Some(notice) = reply_notice {
                    if std::mem::take(&mut streaming) {
                        println!();
                    }
                    say(&notice);
                }
                if let Some((snapshot_session, generation, explicit)) = snapshots.remove(id) {
                    if *ok && snapshot_session == session.id {
                        if let Some(result) = result {
                            for text in run_state.snapshot(generation, result, explicit) {
                                if std::mem::take(&mut streaming) {
                                    println!();
                                }
                                say(&text);
                            }
                        }
                    }
                }
                if let Some(kind) = gap_refreshes.remove(id) {
                    if *ok {
                        if streaming {
                            println!();
                            streaming = false;
                        }
                        if let Some(result) = result {
                            say(&gap_result(kind, result));
                        }
                    }
                }
                if !ok && !handled_reply {
                    say(&format!(
                        "Daemon: {}",
                        error.as_deref().unwrap_or("request failed")
                    ));
                }
            }
            let ServerMessage::Event(ev) = msg else {
                continue;
            };
            // Approvals are global; everything else is this Session's.
            let mine = ev.session_id().is_none_or(|s| s == session.id);
            if !mine && !matches!(ev, Event::ApprovalRequested { .. }) {
                continue;
            }
            if !run_state.accept(&ev) {
                continue;
            }
            for (kind, request) in gap_requests(&ev) {
                if !gap_refreshes.values().any(|pending| *pending == kind) {
                    gap_refreshes.insert(client.send(request), kind);
                }
            }
            match &ev {
                Event::RunStarted { .. } => running = true,
                Event::RunFinished { .. } | Event::RunPaused { .. } => running = false,
                Event::SessionUpdated { session: updated } => session = updated.clone(),
                _ => {}
            }
            let text = feedback.notice(&ev, &mut streaming);
            if let Some(text) = text {
                if matches!(ev, Event::MessageDelta { .. }) || text == "\n" {
                    print!("{text}");
                    let _ = std::io::stdout().flush();
                } else {
                    say(&text);
                }
            }
            remove_resolved_prompt(&mut prompts, &ev);
            match ev {
                Event::ApprovalRequested { approval_id, .. } => {
                    prompts.push_back(Prompt::Approval {
                        id: approval_id,
                        promotion: false,
                    })
                }
                Event::PromotionApprovalRequested { promotion_id, .. } => {
                    prompts.push_back(Prompt::Approval {
                        id: promotion_id,
                        promotion: true,
                    })
                }
                Event::UserQuestion {
                    question_id,
                    question,
                    options,
                    mut questions,
                    ..
                } => {
                    if questions.is_empty() {
                        questions.push(Question::single(question, options));
                    }
                    say(&question_text(&questions[0], 1, questions.len()));
                    prompts.push_back(Prompt::Question {
                        id: question_id,
                        questions,
                        answers: Vec::new(),
                    });
                }
                _ => {}
            }
        }
        let line = match lines.recv_timeout(Duration::from_millis(50)) {
            Ok(l) => l,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let text = line.trim().to_string();
        // A waiting approval or question takes the next line.
        if let Some(p) = prompts.front_mut() {
            match p {
                Prompt::Approval { id, promotion } => {
                    let choice = text.to_lowercase();
                    let (approved, scope) = match choice.as_str() {
                        "y" | "yes" => (true, ApprovalScope::Always),
                        "o" | "once" if !*promotion => (true, ApprovalScope::Once),
                        "s" | "session" if !*promotion => (true, ApprovalScope::Session),
                        "h" | "hour" if !*promotion => (true, ApprovalScope::Hour),
                        "n" | "no" => (false, ApprovalScope::Always),
                        _ => {
                            say("Please type y, o, s, h or n.");
                            continue;
                        }
                    };
                    if !reply_acks.has_room() {
                        say("Too many prompt replies await confirmation; wait before replying again.");
                        continue;
                    }
                    let request = if *promotion {
                        client.send(Request::PromotionDecision {
                            promotion_id: id.clone(),
                            approved,
                        })
                    } else {
                        client.send(Request::ApprovalDecision {
                            remember_denial: Default::default(),
                            paths: Default::default(),
                            approval_id: id.clone(),
                            approved,
                            scope,
                        })
                    };
                    reply_acks.insert(
                        request,
                        if *promotion {
                            PendingReply::Promotion
                        } else {
                            PendingReply::Approval(approved)
                        },
                    );
                    say("Decision sent; awaiting daemon confirmation.");
                    prompts.pop_front();
                }
                Prompt::Question {
                    id,
                    questions,
                    answers,
                } => {
                    if answers.len() + 1 == questions.len() && !reply_acks.has_room() {
                        say("Too many prompt replies await confirmation; wait before replying again.");
                        continue;
                    }
                    let answer = questions[answers.len()].parse_reply(&text);
                    answers.push(answer);
                    if answers.len() < questions.len() {
                        say(&question_text(
                            &questions[answers.len()],
                            answers.len() + 1,
                            questions.len(),
                        ));
                    } else {
                        let request = client.send(Request::AnswerQuestion {
                            question_id: id.clone(),
                            answer: QuestionAnswer::summary_of(answers),
                            answers: std::mem::take(answers),
                        });
                        reply_acks.insert(request, PendingReply::Question);
                        say("Answer sent; awaiting daemon confirmation.");
                        prompts.pop_front();
                    }
                }
            }
            continue;
        }
        let templates = templates::load(&paths, &session);
        for note in crate::user_commands_ui::diagnostic_notes(&templates, &mut template_diagnostics)
        {
            if std::mem::take(&mut streaming) {
                println!();
            }
            say(&note);
        }
        match templates::expand(&templates, &text) {
            Ok(Some(expanded)) => {
                // Do not redispatch a body beginning with slash or shell syntax.
                sends.borrow_mut().send(
                    &client,
                    session.id.clone(),
                    expanded,
                    if running {
                        SendMode::Steer
                    } else {
                        SendMode::Send
                    },
                );
                continue;
            }
            Err(error) => {
                say(&error);
                continue;
            }
            Ok(None) => {}
        }
        match text.as_str() {
            "" => {}
            "/quit" | "/exit" => break,
            "/help" => {
                say("Commands: /new starts a Session, /sessions lists them, /switch N opens one, /stop stops the Run, /files [RUN] shows tracked changes, /rewind [PATH] attempts safe restoration, /recap shows the recorded recap, /docs [topic] gives the guides' addresses; /docs --text [topic] reads bundled Markdown, /quit exits (Runs keep going in the daemon). Anything else is sent as a message; to send text that starts with /, begin it with //.");
                for line in crate::user_commands_ui::help_lines(&templates) {
                    say(&line);
                }
            }
            cmd if cmd == "/docs" || cmd.starts_with("/docs ") => {
                let topic = cmd["/docs".len()..].trim();
                for line in docs_lines(topic) {
                    say(&line);
                }
            }

            cmd if cmd == "/summarise-branch" || cmd.starts_with("/summarise-branch ") => {
                say("Open the full TUI or web, switch to the target Session, then use /summarise-branch SOURCE_SESSION to generate and review a candidate before inserting one note.");
            }
            cmd if cmd == "/files" || cmd.starts_with("/files ") => {
                let run_id = cmd.strip_prefix("/files").unwrap().trim();
                match wait(client.send(Request::RunFiles {
                    session_id: session.id.clone(),
                    run_id: (!run_id.is_empty()).then(|| run_id.to_string()),
                }))
                .and_then(|v| files_result_text(&v))
                {
                    Ok(text) => say(&text),
                    Err(e) => say(&format!("Files: {e}")),
                }
            }
            cmd if cmd == "/rewind" || cmd.starts_with("/rewind ") => {
                let path = cmd.strip_prefix("/rewind").unwrap().trim();
                match wait(client.send(Request::RewindRun {
                    session_id: session.id.clone(),
                    run_id: None,
                    path: (!path.is_empty()).then(|| path.to_string()),
                })) {
                    Ok(v) => say(&rewind_result_text(&v)),
                    Err(e) => say(&format!("Rewind: {e}")),
                }
            }
            "/recap" => {
                snapshots.insert(
                    client.send(Request::GetSession {
                        id: session.id.clone(),
                    }),
                    (session.id.clone(), run_state.generation, true),
                );
            }
            "/stop" => {
                client.send(Request::InterruptRun {
                    session_id: session.id.clone(),
                });
            }
            "/new" => {
                let s: SessionInfo = serde_json::from_value(wait(
                    client.send(Request::CreateSession { name: None }),
                )?)?;
                session = s;
                run_state = RunState::default();
                feedback = PlainFeedback::default();
                client.send(Request::Subscribe {
                    sessions: Some(vec![session.id.clone()]),
                    deltas: true,
                    client: Some("TUI (plain)".into()),
                });
                snapshots.insert(
                    client.send(Request::GetSession {
                        id: session.id.clone(),
                    }),
                    (session.id.clone(), run_state.generation, false),
                );
                say(&format!("New Session: {}.", session.name));
            }
            "/sessions" => {
                let list: Vec<SessionInfo> =
                    serde_json::from_value(wait(client.send(Request::ListSessions))?)
                        .unwrap_or_default();
                for (i, s) in list.iter().enumerate().take(20) {
                    say(&format!("{}. {}", i + 1, s.name));
                }
                say("Type /switch and a number to open one.");
            }
            cmd if cmd.starts_with("/switch ") => {
                let list: Vec<SessionInfo> =
                    serde_json::from_value(wait(client.send(Request::ListSessions))?)
                        .unwrap_or_default();
                match cmd["/switch ".len()..]
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| list.get(n.wrapping_sub(1)))
                {
                    Some(s) => {
                        session = s.clone();
                        run_state = RunState::default();
                        feedback = PlainFeedback::default();
                        client.send(Request::Subscribe {
                            sessions: Some(vec![session.id.clone()]),
                            deltas: true,
                            client: Some("TUI (plain)".into()),
                        });
                        snapshots.insert(
                            client.send(Request::GetSession {
                                id: session.id.clone(),
                            }),
                            (session.id.clone(), run_state.generation, false),
                        );
                        say(&format!("Switched to {}.", session.name));
                    }
                    None => say("No Session with that number; /sessions lists them."),
                }
            }
            // An unknown command is never sent to the model by mistake.
            cmd if cmd.starts_with('/') && !cmd.starts_with("//") => {
                let name = cmd.split_whitespace().next().unwrap_or(cmd);
                let suggestion =
                    crate::user_commands_ui::suggestion(&templates, name.trim_start_matches('/'))
                        .map(|text| format!(" {text}"))
                        .unwrap_or_default();
                say(&format!("{name} is not a command here; /help lists them.{suggestion} To send it as a message, begin it with //."));
            }
            msg => {
                let msg = msg
                    .strip_prefix('/')
                    .filter(|m| m.starts_with('/'))
                    .unwrap_or(msg);
                sends.borrow_mut().send(
                    &client,
                    session.id.clone(),
                    msg.to_string(),
                    if running {
                        SendMode::Steer
                    } else {
                        SendMode::Send
                    },
                );
            }
        }
    }
    if running {
        say("A Run is still working in the daemon; it will continue.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(id: Option<&str>, fraction: f64) -> Event {
        Event::ToolProgress {
            invocation_id: id.map(str::to_owned),
            session_id: "s".into(),
            run_id: "r".into(),
            tool_name: "same".into(),
            depth: 0,
            fraction: Some(fraction),
            label: "line\n\u{1b}[31m\u{202e}".into(),
        }
    }

    #[test]
    fn progress_throttles_exact_ids_and_keeps_background_identity() {
        let mut feedback = LiveFeedback::default();
        let first = feedback.event(&progress(Some("one"), 0.01)).unwrap();
        assert!(first.contains("[one]"));
        assert!(!first.contains('\u{1b}'));
        assert!(!first.contains('\u{202e}'));
        assert!(feedback.event(&progress(Some("one"), 0.02)).is_none());
        assert!(feedback.event(&progress(Some("two"), 0.02)).is_some());
        assert!(feedback.event(&progress(Some("one"), 0.25)).is_some());
        assert!(feedback.event(&progress(Some("one"), 1.0)).is_some());
        assert!(feedback.event(&progress(Some("one"), 1.0)).is_none());
        assert!(feedback.event(&progress(None, 1.0)).is_none());
        feedback.event(&Event::RunFinished {
            session_id: "s".into(),
            run_id: "r".into(),
            status: "completed".into(),
        });
        assert!(feedback.progress.keys().any(|(_, id)| id == "two"));
        feedback.event(&Event::InvocationCompleted {
            session_id: "s".into(),
            invocation_id: Some("two".into()),
            handle: "h".into(),
            tool_name: "same".into(),
            ok: true,
        });
        assert!(!feedback.progress.keys().any(|(_, id)| id == "two"));
    }

    #[test]
    fn plain_tool_labels_appear_only_for_overlapping_calls_and_stay_stable() {
        let start = |id: &str| Event::ToolCallStarted {
            invocation_id: Some(id.into()),
            session_id: "s".into(),
            run_id: "r".into(),
            tool_name: "same".into(),
            version: 1,
            arguments: "{}".into(),
            depth: 0,
        };
        let finish = |id: &str| Event::ToolCallFinished {
            invocation_id: Some(id.into()),
            session_id: "s".into(),
            run_id: "r".into(),
            tool_name: "same".into(),
            ok: true,
            elapsed_ms: 1,
        };
        let first = "a00a00a0-1000-4000-8000-000000000001";
        let second = "a00a00a0-1000-4000-8000-000000000002";
        let mut streaming = false;
        let mut sequential = PlainFeedback::default();
        for id in [first, second] {
            assert_eq!(
                sequential.notice(&start(id), &mut streaming).as_deref(),
                Some("Tool started: same.")
            );
            assert_eq!(
                sequential.notice(&finish(id), &mut streaming).as_deref(),
                Some("Tool finished: same.")
            );
        }

        let mut parallel = PlainFeedback::default();
        assert_eq!(
            parallel.notice(&start(first), &mut streaming).as_deref(),
            Some("Tool started: same.")
        );
        assert_eq!(
            parallel.notice(&start(second), &mut streaming).as_deref(),
            Some("Tool started: same #2.")
        );
        let status = parallel
            .notice(&progress(Some(first), 0.5), &mut streaming)
            .unwrap();
        assert!(
            status.starts_with("Tool progress: same #1: 50%"),
            "{status}"
        );
        assert!(!status.contains(first));
        let pending = Event::InvocationPending {
            invocation_id: Some(first.into()),
            session_id: "s".into(),
            run_id: "r".into(),
            tool_name: "same".into(),
            handle: "cancel-handle".into(),
        };
        assert_eq!(
            parallel.notice(&pending, &mut streaming).as_deref(),
            Some("Tool same #1 continues in the background (cancel-handle).")
        );
        assert_eq!(
            parallel.notice(&finish(second), &mut streaming).as_deref(),
            Some("Tool finished: same #2.")
        );
        parallel.notice(
            &Event::RunFinished {
                session_id: "s".into(),
                run_id: "r".into(),
                status: "completed".into(),
            },
            &mut streaming,
        );
        let completed = Event::InvocationCompleted {
            invocation_id: Some(first.into()),
            session_id: "s".into(),
            tool_name: "same".into(),
            handle: "cancel-handle".into(),
            ok: true,
        };
        assert_eq!(
            parallel.notice(&completed, &mut streaming).as_deref(),
            Some("Background Tool same #1 finished.")
        );
        assert!(parallel.calls.is_empty());
    }

    #[test]
    fn rpc_wait_preserves_interleaved_critical_events_and_responses_in_order() {
        let (tx, rx) = mpsc::channel();
        tx.send(ServerMessage::Event(Event::RunStarted {
            session_id: "s".into(),
            run_id: "r".into(),
        }))
        .unwrap();
        tx.send(ServerMessage::Response {
            id: 8,
            ok: false,
            result: None,
            error: Some("other request".into()),
        })
        .unwrap();
        tx.send(ServerMessage::Event(Event::PlanUpdated {
            session_id: "s".into(),
            items: vec![],
            plan_state: None,
        }))
        .unwrap();
        tx.send(ServerMessage::Response {
            id: 9,
            ok: true,
            result: Some(serde_json::json!({"files": []})),
            error: None,
        })
        .unwrap();
        let backlog = std::cell::RefCell::new(EventBacklog::default());
        let sends = std::cell::RefCell::new(crate::client::SendTracker::default());
        assert_eq!(
            wait_response(&rx, &sends, &backlog, 9).unwrap()["files"],
            serde_json::json!([])
        );
        let mut backlog = backlog.borrow_mut();
        assert!(matches!(
            backlog.pop(),
            Some(ServerMessage::Event(Event::RunStarted { .. }))
        ));
        assert!(matches!(
            backlog.pop(),
            Some(ServerMessage::Response { id: 8, .. })
        ));
        assert!(matches!(
            backlog.pop(),
            Some(ServerMessage::Event(Event::PlanUpdated { .. }))
        ));
        assert!(backlog.pop().is_none());
        assert_eq!(backlog.bytes, 0);
        backlog.bytes = 4 * 1024 * 1024;
        assert!(backlog
            .push(ServerMessage::Event(Event::Note {
                session_id: None,
                text: "overflow".into()
            }))
            .unwrap_err()
            .to_string()
            .contains("reconnect"));
    }

    #[test]
    fn unfinished_calls_are_not_reported_as_finished() {
        let text = recap_text(&RunRecap {
            total_calls: Some(5),
            unfinished_calls: 5,
            ..Default::default()
        });
        assert!(
            text.contains("5 total calls; 0 finished calls; 5 unfinished calls"),
            "{text}"
        );
        assert!(text.contains("Used: none recorded"));
        let legacy = recap_text(&RunRecap::default());
        assert!(legacy.contains("unknown total calls; unknown finished calls"));
    }

    #[test]
    fn snapshot_restores_once_and_live_new_run_rejects_old_recap() {
        let recap = RunRecap {
            status: "paused".into(),
            total_calls: Some(5),
            unfinished_calls: 1,
            used: vec!["read ×4".into()],
            built: vec!["created helper v1".into()],
            files: vec!["modified a.rs".into()],
            steered: 2,
            untracked_writes_possible: true,
            ..Default::default()
        };
        let snapshot = serde_json::json!({"latest_run": {"run_id":"old"}, "last_recap": RecordedRunRecap { run_id: "old".into(), recap: recap.clone() }, "plan": [{"content":"finish","status":"in_progress"}], "plan_state": {"run_id":"old","run_status":"paused","origin":"live"}});
        let mut state = RunState::default();
        let lines = state.snapshot(0, &snapshot, false);
        assert_eq!(lines.len(), 2);
        for required in [
            "status paused",
            "5 total calls",
            "1 unfinished",
            "2 steering",
            "Used: read",
            "Built: created helper",
            "Files: modified",
            "Untracked writes",
        ] {
            assert!(lines[1].contains(required), "{}", lines[1]);
        }
        assert!(lines[0].contains("0/1 done; paused; live. Current: finish."));
        assert!(state.snapshot(0, &snapshot, false).is_empty());
        assert!(!state.accept(&Event::RunRecap {
            session_id: "s".into(),
            run_id: "old".into(),
            recap: recap.clone()
        }));
        assert!(state.accept(&Event::RunStarted {
            session_id: "s".into(),
            run_id: "new".into()
        }));
        assert!(state.snapshot(0, &snapshot, false).is_empty());
        assert!(!state.accept(&Event::RunRecap {
            session_id: "s".into(),
            run_id: "old".into(),
            recap
        }));
        assert!(state.recap.is_none());
    }

    #[test]
    fn gap_recovery_requests_and_prints_recorded_usage_and_activity() {
        let ev = Event::DeltasDropped {
            session_id: "s".into(),
            run_id: "r".into(),
            kinds: vec![
                DroppedEventKind::RunUsage,
                DroppedEventKind::InvocationActivity,
                DroppedEventKind::ToolProgress,
            ],
            handle: None,
            model: None,
        };
        let requests = gap_requests(&ev);
        assert_eq!(requests.len(), 2);
        assert!(
            matches!(&requests[0].1, Request::SessionUsage { session_id } if session_id == "s")
        );
        assert!(matches!(requests[1].1, Request::ListInvocations));
        let text = gap_result(
            GapRefresh::Usage,
            &serde_json::json!({"last_run": UsageInfo { steps: 3, ..UsageInfo::default() }}),
        );
        assert!(text.contains("Recorded usage: 3 steps"));
        assert_eq!(
            gap_result(GapRefresh::Invocations, &serde_json::json!([])),
            "Recorded tools: 0 running, 0 stalled."
        );
    }

    #[test]
    fn resolved_promotion_releases_the_next_plain_prompt() {
        let mut prompts = VecDeque::from([
            Prompt::Approval {
                id: "p".into(),
                promotion: true,
            },
            Prompt::Approval {
                id: "next".into(),
                promotion: false,
            },
        ]);
        remove_resolved_prompt(
            &mut prompts,
            &Event::PromotionApprovalResolved {
                promotion_id: "unrelated".into(),
            },
        );
        assert_eq!(prompts.len(), 2);
        remove_resolved_prompt(
            &mut prompts,
            &Event::PromotionApprovalResolved {
                promotion_id: "p".into(),
            },
        );
        assert!(
            matches!(prompts.front(), Some(Prompt::Approval { id, promotion: false }) if id == "next")
        );
    }

    /// A restarted stream never prints as one run-on answer (#890).
    #[test]
    fn a_restarted_stream_is_marked() {
        let mut streaming = false;
        let delta = |d: &str| Event::MessageDelta {
            session_id: "s".into(),
            run_id: "r".into(),
            delta: d.into(),
        };
        let mut out = String::new();
        for ev in [
            delta("Hello wor"),
            Event::StreamRestarted {
                session_id: "s".into(),
                run_id: "r".into(),
            },
            delta("Hello world"),
        ] {
            if let Some(t) = format_event(&ev, &mut streaming, "") {
                out.push_str(&t);
            }
        }
        assert!(!out.contains("worHello"), "{out}");
        assert!(out.contains("starts again"), "{out}");
        assert!(out.ends_with("Formless: Hello world"), "{out}");
    }

    #[test]
    fn events_print_harness_answers_without_streaming() {
        let message = serde_json::from_value(serde_json::json!({
            "id":"m", "session_id":"s", "seq":2, "role":"assistant",
            "content":"harness reply", "created_at":"", "meta":"{\"harness\":\"Claude\"}"
        }))
        .unwrap();
        let appended = Event::MessageAppended {
            session_id: "s".into(),
            message,
        };
        assert_eq!(
            format_event(&appended, &mut false, "").as_deref(),
            Some("Formless: harness reply")
        );
    }

    #[test]
    fn events_print_as_plain_sentences() {
        let mut streaming = false;
        let delta = |d: &str| Event::MessageDelta {
            session_id: "s".into(),
            run_id: "r".into(),
            delta: d.into(),
        };
        assert_eq!(
            format_event(&delta("Hel"), &mut streaming, "").unwrap(),
            "Formless: Hel"
        );
        assert_eq!(
            format_event(&delta("lo"), &mut streaming, "").unwrap(),
            "lo"
        );
        let started = Event::ToolCallStarted {
            invocation_id: None,
            session_id: "s".into(),
            run_id: "r".into(),
            tool_name: "fetch".into(),
            version: 1,
            arguments: String::new(),
            depth: 0,
        };
        // A tool after streamed text starts on its own line.
        assert_eq!(
            format_event(&started, &mut streaming, "").unwrap(),
            "\nTool started: fetch."
        );
        let approval = Event::ApprovalRequested {
            context: None,
            path_options: Default::default(),
            approval_id: "a".into(),
            session_id: "s".into(),
            tool_name: "fetch".into(),
            version: 1,
            capabilities: vec!["net:example.com".into()],
            memory_mb: 64,
            detail: None,
            already_allowed: vec![],
            code_preview: None,
            tool_id: None,
            batch: None,
        };
        let text = format_event(&approval, &mut streaming, "").unwrap();
        assert!(
            text.contains("net:example.com (can reach example.com over the network)"),
            "{text}"
        );
        assert!(text.ends_with("or n to deny."));
        let mut sensitive = approval.clone();
        if let Event::ApprovalRequested {
            capabilities,
            detail,
            ..
        } = &mut sensitive
        {
            *capabilities = vec!["fs:read-sensitive:/private/project/token\u{200b}.json".into()];
            *detail = Some("Pinned reader v2 requests credential access\u{e0061}".into());
        }
        let text = format_event(&sensitive, &mut streaming, "").unwrap();
        assert!(text.contains("Pinned reader v2 requests credential access⟨U+E0061⟩"));
        assert!(text.contains("fs:read-sensitive:/private/project/token⟨U+200B⟩.json"));
        assert!(text.contains("exact approved path identity only, not descendants"));
        assert!(text.contains("may flow to the model provider"));
        let no_box_drawing = |s: &str| !s.chars().any(|c| ('\u{2500}'..='\u{257f}').contains(&c));
        assert!(no_box_drawing(&text));
        let q = Question {
            question: "Which db?".into(),
            header: None,
            options: vec![QuestionOption {
                label: "Postgres".into(),
                description: Some("relational".into()),
            }],
            multi_select: false,
        };
        assert_eq!(
            question_text(&q, 1, 2),
            "Question 1 of 2: Which db?\n  1. Postgres, relational\nType an option number or your own answer."
        );
    }
}

#[cfg(test)]
#[path = "plain_reply_ack_tests.rs"]
mod reply_ack_tests;

#[cfg(test)]
#[path = "plain_pause_restore_tests.rs"]
mod pause_restore_tests;

#[cfg(test)]
mod docs_topic_tests {
    #[test]
    fn plain_docs_reads_bundled_text_with_either_flag_position() {
        for input in ["--text docker", "docker --text"] {
            let lines = super::docs_lines(input);
            assert!(lines[0].starts_with("Bundled guide"));
            assert!(lines
                .join("\n")
                .contains(include_str!("../../../docs/docker.md").trim_end()));
            assert!(lines
                .iter()
                .any(|line| line.contains("Current online copy")));
        }
        assert!(super::docs_lines("--text nope")[0].contains("unknown docs topic"));
        assert!(super::docs_lines("s --text")[0].contains("ambiguous docs topic"));
    }

    #[test]
    fn plain_docs_reports_unknown_ambiguous_and_the_gallery_without_launching() {
        let unknown = super::docs_lines("nope");
        assert!(unknown[0].contains("unknown docs topic \"nope\""));
        let ambiguous = super::docs_lines("s");
        assert!(ambiguous[0].contains("ambiguous docs topic \"s\": matches skills, spec"));
        for lines in [unknown, ambiguous] {
            assert!(lines[1].contains("Topics:") && lines[1].contains("troubleshooting"));
        }
        assert!(super::docs_lines("")[0].ends_with("docs/gallery.md"));
        assert!(super::docs_lines("spe")[0].ends_with("SPEC.md"));
    }
}
