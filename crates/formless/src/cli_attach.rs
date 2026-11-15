//! Observation and headless decisions share one labeled connection. In
//! particular, request_once would lose the CLI identity on an approval reply.
use super::*;
use formless_tui::client::{DaemonClient, DaemonError};
use std::{
    collections::{HashSet, VecDeque},
    io::{BufRead, IsTerminal},
    sync::mpsc,
    time::{Duration, Instant},
};

#[derive(Subcommand)]
pub(super) enum RunsAction {
    /// Read one live Invocation snapshot (not stored Run metrics)
    Running {
        /// Print the original Invocation array as JSON
        #[arg(long)]
        json: bool,
    },
    /// Inspect a stored Run (full id or unambiguous prefix)
    Show {
        /// Stored Run ID or unambiguous prefix to inspect
        id: String,
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// Cancel only this Run; a later Run in its Session is unaffected
    Cancel {
        /// Run ID to cancel
        id: String,
        /// Print machine-readable JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
}

struct Connection {
    client: DaemonClient,
    rx: mpsc::Receiver<ServerMessage>,
    events: VecDeque<Event>,
    event_bytes: usize,
    cancel: Option<Arc<std::sync::atomic::AtomicUsize>>,
}
impl Connection {
    fn connect(paths: &Paths) -> Result<Self> {
        Self::connect_with_cancel(paths, None)
    }
    fn connect_with_cancel(
        paths: &Paths,
        cancel: Option<Arc<std::sync::atomic::AtomicUsize>>,
    ) -> Result<Self> {
        // A saturated local socket backlog can block the OS connect itself.
        // Keep follow's detach path responsive even before the first RPC.
        let connected = if let Some(counter) = &cancel {
            let paths = paths.clone();
            let (tx, rx) = mpsc::sync_channel(1);
            std::thread::spawn(move || {
                let _ = tx.send(DaemonClient::connect(&paths));
            });
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if counter.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                    return Err(Detached.into());
                }
                let left = deadline.saturating_duration_since(Instant::now());
                anyhow::ensure!(
                    !left.is_zero(),
                    "connect to the running daemon timed out after 30s"
                );
                match rx.recv_timeout(left.min(Duration::from_millis(100))) {
                    Ok(result) => break result,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => bail!("daemon connection worker ended unexpectedly"),
                }
            }
        } else {
            DaemonClient::connect(paths)
        };
        let (client, rx) = connected.context("connect to the running daemon")?;
        let mut this = Self {
            client,
            rx,
            events: VecDeque::new(),
            event_bytes: 0,
            cancel,
        };
        this.subscribe(Vec::new(), false)?;
        Ok(this)
    }
    fn subscribe(&mut self, sessions: Vec<String>, deltas: bool) -> Result<()> {
        self.request(Request::Subscribe {
            sessions: Some(sessions),
            deltas,
            client: Some("CLI".into()),
        })?;
        Ok(())
    }
    fn request(&mut self, request: Request) -> Result<Value> {
        let id = self.client.send(request);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if self
                .cancel
                .as_ref()
                .is_some_and(|count| count.load(std::sync::atomic::Ordering::SeqCst) > 0)
            {
                return Err(Detached.into());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            anyhow::ensure!(!left.is_zero(), "daemon did not answer within 30s");
            let msg = match self.rx.recv_timeout(left.min(Duration::from_millis(100))) {
                Ok(msg) => msg,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => bail!("daemon disconnected while awaiting response"),
            };
            match msg {
                msg @ ServerMessage::Response { id: rid, .. } if rid == id => {
                    let code = msg.error_code().map(str::to_owned);
                    let ServerMessage::Response {
                        ok, result, error, ..
                    } = msg
                    else {
                        unreachable!()
                    };
                    return if ok {
                        Ok(result.unwrap_or(Value::Null))
                    } else {
                        Err(DaemonError {
                            code,
                            message: error.unwrap_or_else(|| "daemon error".into()),
                        }
                        .into())
                    };
                }
                ServerMessage::Event(event) => {
                    self.event_bytes += serde_json::to_vec(&event)?.len();
                    anyhow::ensure!(
                        self.event_bytes <= 4 * 1024 * 1024 && self.events.len() < 4096,
                        "live event backlog exceeded the CLI limit; attach again to resynchronize"
                    );
                    self.events.push_back(event);
                }
                _ => {}
            }
        }
    }
    fn next(&mut self) -> Result<Option<Event>> {
        if let Some(ev) = self.events.pop_front() {
            self.event_bytes = self
                .event_bytes
                .saturating_sub(serde_json::to_vec(&ev)?.len());
            return Ok(Some(ev));
        }
        match self.rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ServerMessage::Event(ev)) => Ok(Some(ev)),
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(_) => bail!("daemon disconnected while following"),
        }
    }
}

pub(super) fn runs(paths: &Paths, action: RunsAction, outer_json: bool) -> Result<()> {
    let mut conn = Connection::connect(paths)?;
    let inspect = matches!(&action, RunsAction::Show { .. });
    let running = matches!(&action, RunsAction::Running { .. });
    let (value, json) = match action {
        RunsAction::Running { json } => (conn.request(Request::ListInvocations)?, json),
        RunsAction::Show { id, json } => (conn.request(Request::GetRun { run_id: id })?, json),
        RunsAction::Cancel { id, json } => {
            (conn.request(Request::CancelRunId { run_id: id })?, json)
        }
    };
    if json || outer_json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else if running {
        print!("{}", invocation_snapshot(&value)?);
    } else {
        print!(
            "{}",
            if inspect {
                super::cli_run_report::show(&value)
            } else {
                super::cli_run_report::cancel(&value)
            }
        );
    }
    if inspect {
        if let Some(code) = value["status"]
            .as_str()
            .and_then(terminal)
            .filter(|code| *code != 0)
        {
            std::process::exit(code);
        }
    }
    Ok(())
}

pub(super) fn validate_runs_limit(cli: &Cli) -> Result<()> {
    if matches!(
        &cli.cmd,
        Some(Cmd::Runs {
            action: Some(RunsAction::Running { .. }),
            limit: Some(_),
            ..
        })
    ) {
        return Err(super::usage(
            "--limit applies to the stored Run list, not the live Invocation snapshot",
        ));
    }
    Ok(())
}

fn invocation_snapshot(value: &Value) -> Result<String> {
    use std::fmt::Write;
    let entries = value
        .as_array()
        .context("live Invocation response was not an array")?;
    let mut out = String::from("Live Invocations (one snapshot)\n");
    if entries.is_empty() {
        out.push_str("No live Invocations.\n");
    }
    for entry in entries {
        writeln!(
            out,
            "\nHandle: {}\n  Tool: {}\n  Session: {}",
            text(entry, "handle"),
            text(entry, "tool_name"),
            text(entry, "session_id")
        )?;
        if entry["invocation_id"].is_string() {
            writeln!(out, "  Invocation ID: {}", text(entry, "invocation_id"))?;
        }
        for (label, key) in [("Elapsed", "elapsed_ms"), ("Silent", "silent_ms")] {
            let milliseconds = entry[key]
                .as_u64()
                .context("Invocation duration was not an unsigned millisecond value")?;
            writeln!(
                out,
                "  {label}: {}.{:03}s",
                milliseconds / 1000,
                milliseconds % 1000
            )?;
        }
        writeln!(
            out,
            "  Stalled: {}",
            if entry["stalled"]
                .as_bool()
                .context("Invocation stalled flag was not a boolean")?
            {
                "yes"
            } else {
                "no"
            }
        )?;
        writeln!(
            out,
            "  Abandoned: {}",
            if entry["abandoned"]
                .as_bool()
                .context("Invocation abandoned flag was not a boolean")?
            {
                "yes — no longer awaited; may still finish"
            } else {
                "no"
            }
        )?;
        if entry["last_fraction"].is_number() {
            writeln!(out, "  Progress fraction: {}", entry["last_fraction"])?;
        }
        if entry["last_label"].is_string() {
            writeln!(out, "  Progress label: {}", text(entry, "last_label"))?;
        }
    }
    Ok(out)
}
pub(super) fn pending(paths: &Paths, json: bool) -> Result<()> {
    let mut conn = Connection::connect(paths)?;
    let value = conn.request(Request::PendingApprovals)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        for request in value.as_array().into_iter().flatten() {
            println!(
                "{}  {}  Session {}\n  {}",
                text(request, "approval_id"),
                text(request, "tool_name"),
                text(request, "session_id"),
                super::comparison_terminal_text(&request["capabilities"].to_string())
            );
        }
    }
    Ok(())
}
fn text(value: &Value, key: &str) -> String {
    super::comparison_terminal_text(value[key].as_str().unwrap_or_default())
}
fn select_pending(value: Value, query: &str) -> Result<Event> {
    let list: Vec<Event> = serde_json::from_value(value)?;
    if let Some(event) = list
        .iter()
        .find(|ev| matches!(ev,Event::ApprovalRequested{approval_id,..} if approval_id==query))
    {
        return Ok(event.clone());
    }
    let mut matches = list.into_iter().filter(|ev| matches!(ev, Event::ApprovalRequested { approval_id,.. } if approval_id.starts_with(query)));
    let first = matches
        .next()
        .ok_or_else(|| super::usage("pending approval not found"))?;
    anyhow::ensure!(
        !query.is_empty() && matches.next().is_none(),
        super::usage("ambiguous approval id")
    );
    Ok(first)
}
fn confirm_broad(
    input: &mut impl BufRead,
    output: &mut impl Write,
    tty: bool,
    yes: bool,
) -> Result<()> {
    if !tty {
        anyhow::ensure!(
            yes,
            super::usage("a broad Session grant requires --yes off-TTY")
        );
        return Ok(());
    }
    for prompt in [
        "Grant these broad capabilities for this Session? [y/N] ",
        "Confirm broad Session grant again? [y/N] ",
    ] {
        write!(output, "{prompt}")?;
        output.flush()?;
        let mut line = String::new();
        std::io::Read::take(&mut *input, 1024).read_line(&mut line)?;
        anyhow::ensure!(
            matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
            "approval left pending"
        );
    }
    Ok(())
}
pub(super) fn decide(
    paths: &Paths,
    id: &str,
    approved: bool,
    session: bool,
    yes: bool,
    json: bool,
) -> Result<()> {
    let mut conn = Connection::connect(paths)?;
    let event = select_pending(conn.request(Request::PendingApprovals)?, id)?;
    let Event::ApprovalRequested {
        approval_id,
        capabilities,
        already_allowed,
        ..
    } = &event
    else {
        unreachable!()
    };
    let broad = capabilities.iter().any(|cap| {
        !already_allowed.contains(cap) && formless_protocol::broad_capability(cap).is_some()
    });
    if approved && session && broad {
        eprintln!(
            "{}",
            super::comparison_terminal_text(&serde_json::to_string_pretty(&event)?)
        );
        confirm_broad(
            &mut std::io::stdin().lock(),
            &mut std::io::stderr().lock(),
            std::io::stdin().is_terminal(),
            yes,
        )?;
    }
    let result = conn.request(Request::ApprovalDecision {
        approval_id: approval_id.clone(),
        approved,
        scope: if session {
            ApprovalScope::Session
        } else {
            ApprovalScope::Once
        },
        paths: Default::default(),
        remember_denial: false,
    })?;
    anyhow::ensure!(
        result["resolved"] == true,
        "approval is no longer pending; no decision was applied"
    );
    if result["audit_recorded"] == false {
        eprintln!("decision applied, but the daemon could not write its audit record; do not retry this decision");
    }
    if json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        println!(
            "{} {}",
            if approved { "approved" } else { "denied" },
            super::comparison_terminal_text(approval_id)
        );
    }
    Ok(())
}

fn print_terminal_reason(run: &Value) {
    if let Some(notice) = formless_tui::plain::unfinished_run_text(run) {
        eprintln!("{notice}");
    } else if let Some(error) = run["error"].as_str().filter(|error| !error.is_empty()) {
        let bounded = error.chars().take(2048).collect::<String>();
        eprintln!("{}", super::comparison_terminal_text(&bounded));
    }
}

fn terminal(status: &str) -> Option<i32> {
    match status {
        "completed" => Some(0),
        "interrupted" => Some(130),
        "paused" | "failed" | "abandoned" => Some(1),
        _ => None,
    }
}
struct Observer {
    session: String,
    seen: HashSet<String>,
    approvals: HashSet<String>,
    json: bool,
}
impl Observer {
    fn emit(&mut self, event: Event) -> Result<()> {
        self.emit_to(event, &mut std::io::stdout().lock())
    }
    fn emit_to(&mut self, event: Event, output: &mut impl Write) -> Result<()> {
        let relevant = match &event {
            Event::ApprovalAnswered { approval_id, .. }
            | Event::ApprovalSummary { approval_id, .. }
            | Event::ApprovalClosed { approval_id } => self.approvals.contains(approval_id),
            _ => event.session_id() == Some(self.session.as_str()),
        };
        if !relevant {
            return Ok(());
        }
        match &event {
            Event::MessageAppended { message, .. } | Event::MessageComplete { message, .. } => {
                if !self.seen.insert(message.id.clone()) {
                    return Ok(());
                }
            }
            Event::ApprovalRequested { approval_id, .. }
                if !self.approvals.insert(approval_id.clone()) =>
            {
                return Ok(())
            }
            Event::ApprovalClosed { approval_id } => {
                self.approvals.remove(approval_id);
            }
            _ => {}
        }
        if self.json {
            writeln!(output, "{}", serde_json::to_string(&event)?)?;
        } else {
            match &event {
                Event::MessageDelta { delta, .. } => {
                    write!(output, "{}", super::comparison_terminal_text(delta))?
                }
                Event::MessageAppended { message, .. } | Event::MessageComplete { message, .. } => {
                    writeln!(
                        output,
                        "\n{}: {}",
                        super::comparison_terminal_text(&message.role),
                        super::comparison_terminal_text(&message.content)
                    )?
                }
                _ => writeln!(
                    output,
                    "{}",
                    super::comparison_terminal_text(&serde_json::to_string(&event)?)
                )?,
            }
        }
        output.flush()?;
        Ok(())
    }
    fn snapshot(&mut self, value: &Value, end: Option<i64>, attention: bool) -> Result<()> {
        let messages: Vec<formless_protocol::MessageInfo> =
            serde_json::from_value(value["messages"].clone())?;
        for message in messages {
            if end.is_none_or(|end| message.seq <= end) {
                self.emit(Event::MessageAppended {
                    session_id: self.session.clone(),
                    message,
                })?;
            }
        }
        if attention {
            for event in value["pending_events"].as_array().into_iter().flatten() {
                self.emit(serde_json::from_value(event.clone())?)?;
            }
        }
        Ok(())
    }
}
#[derive(Debug)]
struct Detached;
impl std::fmt::Display for Detached {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "observer detached")
    }
}
impl std::error::Error for Detached {}
pub(super) fn follow(paths: &Paths, query: &str, json: bool) -> Result<i32> {
    match follow_inner(paths, query, json) {
        Err(e) if e.downcast_ref::<Detached>().is_some() => Ok(130),
        other => other,
    }
}
fn follow_inner(paths: &Paths, query: &str, json: bool) -> Result<i32> {
    let sigints = super::count_sigints();
    let mut conn = Connection::connect_with_cancel(paths, Some(sigints.clone()))?;
    let sessions: Vec<SessionInfo> = serde_json::from_value(conn.request(Request::ListSessions)?)?;
    let session = super::find_session_in(&sessions, query)?;
    conn.subscribe(vec![session.id.clone()], true)?;
    let snapshot = conn.request(Request::GetSession {
        id: session.id.clone(),
    })?;
    let mut run = snapshot["latest_run"].clone();
    let mut observer = Observer {
        session: session.id.clone(),
        seen: HashSet::new(),
        approvals: HashSet::new(),
        json,
    };
    observer.snapshot(
        &snapshot,
        run["end_message_seq"].as_i64(),
        run["status"].as_str().and_then(terminal).is_none(),
    )?;
    // Buffered deltas can predate the stored snapshot, with no message identity
    // to dedupe them. Drop only these ephemeral chunks; complete messages persist.
    conn.events.retain(|ev| {
        !matches!(
            ev,
            Event::MessageDelta { .. } | Event::ReasoningDelta { .. }
        )
    });
    conn.event_bytes = 0;
    if let Some(code) = run["status"]
        .as_str()
        .and_then(terminal)
        .filter(|_| run["end_message_seq"].is_null())
    {
        // Historical Runs predate stored message anchors. The initial Session
        // snapshot is honest; a later unbounded refresh could include a new Run.
        observer.emit(Event::Note {session_id:Some(session.id.clone()),text:"Historical Run has no stored message boundary; displayed the initial Session snapshot only.".into()})?;
        print_terminal_reason(&run);
        return Ok(code);
    }
    loop {
        if sigints.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            return Ok(130);
        }
        if let Some(code) = run["status"].as_str().and_then(terminal) {
            let end=run["end_message_seq"].as_i64().context("finished Run has no durable message boundary; cannot safely refresh its transcript")?;
            let snapshot = conn.request(Request::GetSession {
                id: session.id.clone(),
            })?;
            observer.snapshot(&snapshot, Some(end), false)?;
            print_terminal_reason(&run);
            return Ok(code);
        }
        let Some(ev) = conn.next()? else { continue };
        if ev.session_id() != Some(session.id.as_str()) {
            observer.emit(ev)?;
            continue;
        }
        if let Event::MessageAppended { message, .. } = &ev {
            if !observer.seen.contains(&message.id) {
                if let Some(target) = run["run_id"].as_str() {
                    let exact = conn.request(Request::GetRun {
                        run_id: target.into(),
                    })?;
                    if exact["status"].as_str().and_then(terminal).is_some() {
                        // MessageAppended has no Run id. A new user message
                        // can precede B's RunStarted after A is already stored
                        // terminal. Replay A's bounded durable tail instead.
                        anyhow::ensure!(exact["end_message_seq"].as_i64().is_some(),
                            "the finished Run has no durable message boundary; cannot safely attach its new messages");
                        run = exact;
                        continue;
                    }
                }
            }
        }
        // Another client can start B after A leaves the active map but before
        // A's tail broadcasts RunFinished. Keep A's stream pinned to A.
        let identity = serde_json::to_value(&ev)?;
        if let (Some(target), Some(event_run)) =
            (run["run_id"].as_str(), identity["run_id"].as_str())
        {
            if !event_run.is_empty() && event_run != target {
                continue;
            }
        }
        if let Event::RunStarted { run_id, .. } = &ev {
            if run.is_null() {
                run = conn.request(Request::GetRun {
                    run_id: run_id.clone(),
                })?;
            }
        }
        let ending = matches!(&ev,Event::RunFinished {run_id,..}|Event::RunPaused {run_id,..} if run["run_id"].as_str()==Some(run_id.as_str()));
        let resync = matches!(&ev, Event::DeltasDropped { .. });
        observer.emit(ev)?;
        if ending {
            run = conn.request(Request::GetRun {
                run_id: run["run_id"].as_str().unwrap().into(),
            })?;
            anyhow::ensure!(run["status"].as_str().and_then(terminal).is_some(),
                "daemon reported that the Run ended but its stored status is still nonterminal; cannot confirm the durable outcome");
        }
        if resync {
            if let Some(run_id) = run["run_id"].as_str() {
                run = conn.request(Request::GetRun {
                    run_id: run_id.into(),
                })?;
            }
            let snap = conn.request(Request::GetSession {
                id: session.id.clone(),
            })?;
            observer.snapshot(
                &snap,
                run["end_message_seq"].as_i64(),
                run["status"].as_str().and_then(terminal).is_none(),
            )?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_snapshot_json_placements_and_explicit_limit_validation() {
        for args in [
            vec!["formless", "runs", "running"],
            vec!["formless", "runs", "running", "--json"],
            vec!["formless", "runs", "--json", "running"],
        ] {
            let cli = Cli::try_parse_from(&args).unwrap();
            validate_runs_limit(&cli).unwrap();
            let Some(Cmd::Runs {
                action: Some(RunsAction::Running { json }),
                json: outer,
                limit,
                ..
            }) = cli.cmd
            else {
                panic!("wrong action")
            };
            assert_eq!(json || outer, args.contains(&"--json"));
            assert!(limit.is_none());
        }
        for limit in ["0", "20", "100"] {
            let cli =
                Cli::try_parse_from(["formless", "runs", "--limit", limit, "running"]).unwrap();
            let error = validate_runs_limit(&cli).unwrap_err();
            assert_eq!(super::super::exit_code(&error), 2);
        }
        let cli = Cli::try_parse_from(["formless", "runs", "--limit", "7", "--json"]).unwrap();
        validate_runs_limit(&cli).unwrap();
        assert!(matches!(
            cli.cmd,
            Some(Cmd::Runs {
                limit: Some(7),
                action: None,
                ..
            })
        ));
        assert!(Cli::try_parse_from(["formless", "runs", "running", "--limit", "20"]).is_err());
    }

    #[test]
    fn malformed_snapshot_never_yields_a_partial_report() {
        assert!(invocation_snapshot(&serde_json::json!({})).is_err());
        assert!(invocation_snapshot(&serde_json::json!([{"handle":"first","elapsed_ms":1,"silent_ms":2,"stalled":false,"abandoned":false},{"handle":"broken"}])).is_err());
    }

    #[test]
    fn cli_parsing_preserves_listing_and_new_commands() {
        for args in [
            vec!["formless", "runs", "--json"],
            vec!["formless", "runs", "show", "abc", "--json"],
            vec!["formless", "runs", "cancel", "abc"],
            vec!["formless", "sessions", "show", "abc", "--follow", "--jsonl"],
            vec![
                "formless",
                "approvals",
                "approve",
                "abc",
                "--session",
                "--yes",
                "--json",
            ],
            vec!["formless", "approvals", "deny", "abc", "--json"],
            vec!["formless", "approvals", "pending", "--json"],
        ] {
            Cli::try_parse_from(args).unwrap();
        }
        assert!(Cli::try_parse_from(["formless", "sessions", "show", "abc", "--jsonl"]).is_err());
        assert!(Cli::try_parse_from([
            "formless",
            "approvals",
            "approve",
            "abc",
            "--session",
            "--once"
        ])
        .is_err());
    }
    #[test]
    fn broad_persistent_consent_requires_two_tty_answers_even_with_yes() {
        let mut out = Vec::new();
        assert!(confirm_broad(&mut &b"y\nn\n"[..], &mut out, true, true).is_err());
        assert!(confirm_broad(&mut &b"y\n"[..], &mut Vec::new(), true, false).is_err());
        confirm_broad(&mut &b"y\nyes\n"[..], &mut Vec::new(), true, false).unwrap();
        assert!(confirm_broad(&mut &b""[..], &mut Vec::new(), false, false).is_err());
        confirm_broad(&mut &b""[..], &mut Vec::new(), false, true).unwrap();
        assert_eq!(String::from_utf8(out).unwrap().matches("[y/N]").count(), 2);
    }
}

#[cfg(test)]
#[path = "cli_observer_close_tests.rs"]
mod observer_close_tests;
