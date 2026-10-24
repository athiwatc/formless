//! Plain, line-oriented mode for screen readers (SPEC §10.7): no
//! alternate screen, redraws, spinners or borders. New messages and events
//! are printed once, in order, and approvals and questions become plain
//! prompts answered on the next line.

use crate::client::DaemonClient;
use anyhow::Result;
use formless_core::{Config, Paths};
use formless_protocol::*;
use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::time::Duration;

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

/// The printed form of an event, if it has one. `streaming` says whether
/// the answer is mid-stream (deltas are printed as they come).
pub(crate) fn format_event(ev: &Event, streaming: &mut bool) -> Option<String> {
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
        Event::ToolCallStarted {
            tool_name, depth, ..
        } if *depth == 0 => format!("{}Tool started: {tool_name}.", end_stream(streaming)),
        Event::ToolCallFinished { tool_name, ok, .. } => format!(
            "Tool {}: {tool_name}.",
            if *ok { "finished" } else { "failed" }
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
        Event::RunToolChanges { changes, .. } => format!("{}.", super::tool_changes_text(changes)),
        Event::CompactionNotice { .. } => "Note: the conversation was compacted.".into(),
        Event::ApprovalRequested {
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
            format!(
                "{}Approval needed. {what}. Type y to always allow, o for this once, s for this Session, h for an hour, or n to deny.",
                end_stream(streaming)
            )
        }
        Event::PromotionApprovalRequested {
            tool_name, reason, ..
        } => format!(
            "{}Promotion of {tool_name} needs approval: {reason}. Type y to approve or n to deny.",
            end_stream(streaming)
        ),
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
/// `formless chat` (SPEC §10.7).
pub fn run_with(paths: Paths, start: ChatStart, label: &str) -> Result<()> {
    let cfg = Config::load(&paths)?;
    if !cfg.provider.is_configured() {
        eprintln!(
            "Formless isn't set up yet: run `formless` once without --plain to choose a provider."
        );
        return Ok(());
    }
    formless_core::daemon::ensure_daemon(&paths)?;
    let (client, rx) = DaemonClient::connect(&paths)?;
    let wait = |id: u64| -> Result<serde_json::Value> {
        loop {
            match rx.recv()? {
                ServerMessage::Response {
                    id: rid,
                    ok,
                    result,
                    error,
                } if rid == id => {
                    return if ok {
                        Ok(result.unwrap_or_default())
                    } else {
                        Err(anyhow::anyhow!(error.unwrap_or_default()))
                    };
                }
                _ => {}
            }
        }
    };
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
    let mut streaming = false;
    let mut running = false;
    loop {
        while let Ok(msg) = rx.try_recv() {
            let ServerMessage::Event(ev) = msg else {
                continue;
            };
            // Approvals are global; everything else is this Session's.
            let mine = ev.session_id().is_none_or(|s| s == session.id);
            if !mine && !matches!(ev, Event::ApprovalRequested { .. }) {
                continue;
            }
            match &ev {
                Event::RunStarted { .. } => running = true,
                Event::RunFinished { .. } | Event::RunPaused { .. } => running = false,
                _ => {}
            }
            if let Some(text) = format_event(&ev, &mut streaming) {
                if matches!(ev, Event::MessageDelta { .. }) || text == "\n" {
                    print!("{text}");
                    let _ = std::io::stdout().flush();
                } else {
                    say(&text);
                }
            }
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
                Event::UserQuestionClosed { question_id } => prompts
                    .retain(|p| !matches!(p, Prompt::Question { id, .. } if *id == question_id)),
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
                    if *promotion {
                        client.send(Request::PromotionDecision {
                            promotion_id: id.clone(),
                            approved,
                        });
                    } else {
                        client.send(Request::ApprovalDecision {
                            approval_id: id.clone(),
                            approved,
                            scope,
                        });
                    }
                    say(if approved { "Approved." } else { "Denied." });
                    prompts.pop_front();
                }
                Prompt::Question {
                    id,
                    questions,
                    answers,
                } => {
                    let answer = questions[answers.len()].parse_reply(&text);
                    say(&format!("Answered: {}.", answer.summary()));
                    answers.push(answer);
                    if answers.len() < questions.len() {
                        say(&question_text(
                            &questions[answers.len()],
                            answers.len() + 1,
                            questions.len(),
                        ));
                    } else {
                        client.send(Request::AnswerQuestion {
                            question_id: id.clone(),
                            answer: QuestionAnswer::summary_of(answers),
                            answers: std::mem::take(answers),
                        });
                        prompts.pop_front();
                    }
                }
            }
            continue;
        }
        match text.as_str() {
            "" => {}
            "/quit" | "/exit" => break,
            "/help" => say("Commands: /new starts a Session, /sessions lists them, /switch N opens one, /stop stops the Run, /quit exits (Runs keep going in the daemon). Anything else is sent as a message."),
            "/stop" => {
                client.send(Request::InterruptRun {
                    session_id: session.id.clone(),
                });
            }
            "/new" => {
                let s: SessionInfo = serde_json::from_value(
                    wait(client.send(Request::CreateSession { name: None }))?,
                )?;
                session = s;
                client.send(Request::Subscribe {
                    sessions: Some(vec![session.id.clone()]),
                    deltas: true,
                    client: Some("TUI (plain)".into()),
                });
                say(&format!("New Session: {}.", session.name));
            }
            "/sessions" => {
                let list: Vec<SessionInfo> =
                    serde_json::from_value(wait(client.send(Request::ListSessions))?).unwrap_or_default();
                for (i, s) in list.iter().enumerate().take(20) {
                    say(&format!("{}. {}", i + 1, s.name));
                }
                say("Type /switch and a number to open one.");
            }
            cmd if cmd.starts_with("/switch ") => {
                let list: Vec<SessionInfo> =
                    serde_json::from_value(wait(client.send(Request::ListSessions))?).unwrap_or_default();
                match cmd["/switch ".len()..].trim().parse::<usize>().ok().and_then(|n| list.get(n.wrapping_sub(1))) {
                    Some(s) => {
                        session = s.clone();
                        client.send(Request::Subscribe {
                            sessions: Some(vec![session.id.clone()]),
                            deltas: true,
                            client: Some("TUI (plain)".into()),
                        });
                        say(&format!("Switched to {}.", session.name));
                    }
                    None => say("No Session with that number; /sessions lists them."),
                }
            }
            msg => {
                client.send(Request::SendMessage {
                    session_id: session.id.clone(),
                    content: msg.to_string(),
                    mode: if running { SendMode::Steer } else { SendMode::Send },
                });
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

    #[test]
    fn events_print_as_plain_sentences() {
        let mut streaming = false;
        let delta = |d: &str| Event::MessageDelta {
            session_id: "s".into(),
            run_id: "r".into(),
            delta: d.into(),
        };
        assert_eq!(
            format_event(&delta("Hel"), &mut streaming).unwrap(),
            "Formless: Hel"
        );
        assert_eq!(format_event(&delta("lo"), &mut streaming).unwrap(), "lo");
        let started = Event::ToolCallStarted {
            session_id: "s".into(),
            run_id: "r".into(),
            tool_name: "fetch".into(),
            version: 1,
            arguments: String::new(),
            depth: 0,
        };
        // A tool after streamed text starts on its own line.
        assert_eq!(
            format_event(&started, &mut streaming).unwrap(),
            "\nTool started: fetch."
        );
        let approval = Event::ApprovalRequested {
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
        let text = format_event(&approval, &mut streaming).unwrap();
        assert!(
            text.contains("net:example.com (can reach example.com over the network)"),
            "{text}"
        );
        assert!(text.ends_with("or n to deny."));
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
