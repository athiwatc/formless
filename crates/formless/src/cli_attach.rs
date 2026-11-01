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
    /// Inspect a stored Run (full id or unambiguous prefix)
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Cancel only this Run; a later Run in its Session is unaffected
    Cancel {
        id: String,
        #[arg(long)]
        json: bool,
    },
}

struct Connection {
    client: DaemonClient,
    rx: mpsc::Receiver<ServerMessage>,
    events: VecDeque<Event>,
    event_bytes: usize,
}
impl Connection {
    fn connect(paths: &Paths) -> Result<Self> {
        let (client, rx) = DaemonClient::connect(paths).context("connect to the running daemon")?;
        let mut this = Self {
            client,
            rx,
            events: VecDeque::new(),
            event_bytes: 0,
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
            let msg = self
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .context("daemon disconnected or did not answer within 30s")?;
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
    let (value, json) = match action {
        RunsAction::Show { id, json } => (conn.request(Request::GetRun { run_id: id })?, json),
        RunsAction::Cancel { id, json } => {
            (conn.request(Request::CancelRunId { run_id: id })?, json)
        }
    };
    if json || outer_json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!(
            "{}",
            super::comparison_terminal_text(&serde_json::to_string_pretty(&value)?)
        );
    }
    Ok(())
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
        .context(super::usage("pending approval not found"))?;
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
        input.take(1024).read_line(&mut line)?;
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
        paths: Vec::new(),
        remember_denial: false,
    })?;
    anyhow::ensure!(
        result["resolved"] == true,
        "approval is no longer pending; no decision was applied"
    );
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
        let relevant = match &event {
            Event::ApprovalAnswered { approval_id, .. }
            | Event::ApprovalSummary { approval_id, .. } => self.approvals.contains(approval_id),
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
            Event::ApprovalRequested { approval_id, .. } => {
                if !self.approvals.insert(approval_id.clone()) {
                    return Ok(());
                }
            }
            _ => {}
        }
        if self.json {
            println!("{}", serde_json::to_string(&event)?);
        } else {
            match &event {
                Event::MessageDelta { delta, .. } => {
                    print!("{}", super::comparison_terminal_text(delta))
                }
                Event::MessageAppended { message, .. } | Event::MessageComplete { message, .. } => {
                    println!(
                        "\n{}: {}",
                        super::comparison_terminal_text(&message.role),
                        super::comparison_terminal_text(&message.content)
                    )
                }
                _ => println!(
                    "{}",
                    super::comparison_terminal_text(&serde_json::to_string(&event)?)
                ),
            }
        }
        std::io::stdout().flush()?;
        Ok(())
    }
    fn snapshot(&mut self, value: &Value, end: Option<i64>) -> Result<()> {
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
        for event in value["pending_events"].as_array().into_iter().flatten() {
            self.emit(serde_json::from_value(event.clone())?)?;
        }
        Ok(())
    }
}
pub(super) fn follow(paths: &Paths, query: &str, json: bool) -> Result<i32> {
    let sigints = super::count_sigints();
    let mut conn = Connection::connect(paths)?;
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
    observer.snapshot(&snapshot, run["end_message_seq"].as_i64())?;
    // Buffered deltas can predate the stored snapshot, with no message identity
    // to dedupe them. Drop only these ephemeral chunks; complete messages persist.
    conn.events.retain(|ev| {
        !matches!(
            ev,
            Event::MessageDelta { .. } | Event::ReasoningDelta { .. }
        )
    });
    conn.event_bytes = 0;
    loop {
        if sigints.load(std::sync::atomic::Ordering::SeqCst) > 0 {
            return Ok(130);
        }
        if let Some(code) = run["status"].as_str().and_then(terminal) {
            let snapshot = conn.request(Request::GetSession {
                id: session.id.clone(),
            })?;
            observer.snapshot(&snapshot, run["end_message_seq"].as_i64())?;
            if let Some(error) = run["error"].as_str() {
                eprintln!("{}", super::comparison_terminal_text(error));
            }
            return Ok(code);
        }
        let Some(ev) = conn.next()? else { continue };
        if ev.session_id() != Some(session.id.as_str()) {
            observer.emit(ev)?;
            continue;
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
        }
        if resync {
            let snap = conn.request(Request::GetSession {
                id: session.id.clone(),
            })?;
            observer.snapshot(&snap, None)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    #[test]
    fn terminal_codes_distinguish_paused_error_and_interrupted() {
        assert_eq!(terminal("running"), None);
        assert_eq!(terminal("completed"), Some(0));
        assert_eq!(terminal("paused"), Some(1));
        assert_eq!(terminal("failed"), Some(1));
        assert_eq!(terminal("interrupted"), Some(130));
    }
}
