use super::{tr, tr_format, App, Pending, View};
use formless_core::onboarding::TOUR_TOOL_NAME;
use formless_protocol::{Event, Request, SendMode, SessionInfo};
use serde_json::Value;

pub(super) struct Tour {
    pub(super) session_id: String,
    prompt: String,
    stage: Stage,
    sample_reported: bool,
}

enum Stage {
    Draft,
    Sending(u64),
    Running(String),
}

pub(super) fn sample_prompt(reuse: bool) -> String {
    if reuse {
        tr_format("Use the existing Tool {} to count the words, lines and characters of: \"The quick brown fox jumps over the lazy dog.\"", &[TOUR_TOOL_NAME])
    } else {
        tr_format("Build a Tool named {} that counts the words, lines and characters of a text. Reuse it if it already exists. Then call it on: \"The quick brown fox jumps over the lazy dog.\"", &[TOUR_TOOL_NAME])
    }
}

impl App {
    pub(super) fn open_tour(&mut self) {
        if let Some(tour) = &self.tour {
            let id = tour.session_id.clone();
            if !self.is_current(&id) {
                self.switch_session(id.clone());
                self.request(Pending::GetSession, Request::GetSession { id });
            } else {
                self.refresh_tour();
            }
            self.view = View::Chat;
            return;
        }
        if !self
            .pending
            .values()
            .any(|kind| *kind == Pending::TourSession)
        {
            self.request(Pending::TourSession, Request::CreateTourSession);
        }
    }

    pub(super) fn open_tour_reply(&mut self, result: Value) {
        let Ok(session) = serde_json::from_value::<SessionInfo>(result["session"].clone()) else {
            return;
        };
        let sample_exists = result["sample_exists"] == true;
        let prompt = sample_prompt(sample_exists);
        self.tour = Some(Tour {
            session_id: session.id.clone(),
            prompt: prompt.clone(),
            stage: Stage::Draft,
            sample_reported: false,
        });
        self.adopt_new_session(session);
        self.input.set(&prompt);
        self.input.set_literal(true);
        self.notes.push(tr("Tour 1/3 · Press Enter to run the sample task in this Session. /tour cleanup archives the tour and retires its sample.").into());
    }

    pub(super) fn tour_draft_sent(
        &mut self,
        session_id: &str,
        text: &str,
        mode: &SendMode,
    ) -> bool {
        let Some(tour) = self.tour.as_ref().filter(|tour| {
            tour.session_id == session_id && matches!(tour.stage, Stage::Draft | Stage::Sending(_))
        }) else {
            return false;
        };
        if text == tour.prompt && matches!(mode, SendMode::Send) {
            return true;
        }
        self.tour = None;
        self.notes
            .push(tr("Tour guidance stopped. Use /tour to start a new sample task.").into());
        false
    }

    pub(super) fn tour_sending(&mut self, id: u64) {
        if let Some(tour) = &mut self.tour {
            tour.stage = Stage::Sending(id);
        }
    }

    pub(super) fn tour_send_lost(&mut self, id: Option<u64>) {
        if let Some(tour) = self.tour.as_mut().filter(
            |tour| matches!(tour.stage, Stage::Sending(sent) if id.is_none_or(|id| id == sent)),
        ) {
            if id.is_none() {
                tour.stage = Stage::Draft;
            }
            self.notes.push(
                tr("The tour send was unconfirmed. Check the Guided tour Session before retrying.")
                    .into(),
            );
        }
    }

    pub(super) fn tour_send_answered(&mut self, id: u64, ok: bool, result: Option<&Value>) {
        let Some(tour) = self
            .tour
            .as_mut()
            .filter(|tour| matches!(tour.stage, Stage::Sending(sent) if id == sent))
        else {
            return;
        };
        if !ok {
            tour.stage = Stage::Draft;
            return;
        }
        let Some(run_id) = result
            .filter(|reply| reply["started"] == true)
            .and_then(|reply| reply["run_id"].as_str())
        else {
            self.tour = None;
            self.notes
                .push(tr("The tour send did not start a new Run. Use /tour to try again.").into());
            return;
        };
        tour.stage = Stage::Running(run_id.into());
        self.refresh_tour();
    }

    pub(super) fn refresh_tour(&mut self) {
        let Some((session_id, run_id)) = self.tour.as_ref().and_then(|tour| match &tour.stage {
            Stage::Running(run) if self.is_current(&tour.session_id) => {
                Some((tour.session_id.clone(), run.clone()))
            }
            _ => None,
        }) else {
            return;
        };
        self.request(
            Pending::TourRun {
                session_id: session_id.clone(),
                run_id: run_id.clone(),
            },
            Request::GetTourRun { session_id, run_id },
        );
    }

    pub(super) fn observe_tour_event(&mut self, event: &Event) {
        let (session_id, run_id) = match event {
            Event::RunFinished {
                session_id, run_id, ..
            } => (session_id, run_id),
            Event::ToolCallFinished {
                session_id,
                run_id,
                tool_name,
                ok: true,
                ..
            } if tool_name == "create_tool" || tool_name == TOUR_TOOL_NAME => (session_id, run_id),
            Event::SessionDeleted { id }
                if self
                    .tour
                    .as_ref()
                    .is_some_and(|tour| tour.session_id == *id) =>
            {
                self.tour = None;
                return;
            }
            _ => return,
        };
        if self.tour.as_ref().is_some_and(|tour| {
            &tour.session_id == session_id
                && matches!(&tour.stage, Stage::Running(run) if run == run_id)
        }) {
            self.refresh_tour();
        }
    }

    pub(super) fn apply_tour_status(&mut self, session_id: &str, run_id: &str, result: &Value) {
        if !self.is_current(session_id) {
            return;
        }
        let Some(tour) = self.tour.as_mut().filter(|tour| {
            tour.session_id == session_id
                && matches!(&tour.stage, Stage::Running(run) if run == run_id)
        }) else {
            return;
        };
        let created = result["created"] == true;
        let used = result["used"] == true;
        if (created || used) && !tour.sample_reported {
            self.notes.push(if created {
                tr("Tour 2/3 · The agent built the sample Tool. Open the Tools view with Tab to inspect its code and versions.")
            } else {
                tr("Tour 2/3 · The agent reused the sample Tool. Open the Tools view with Tab to inspect its code and versions.")
            }.into());
            tour.sample_reported = true;
        }
        let Some(status) = result["status"].as_str() else {
            return;
        };
        let note = match status {
            "running" => return,
            "completed" if used => {
                self.seen.tour_done = true;
                self.save_seen();
                tr("Tour 3/3 · Done. The agent used its sample Tool. /help lists commands and keys.").into()
            }
            "completed" if created => tr("The tour built its Tool but did not use it successfully. Check the transcript for details.").into(),
            "completed" => tr("The agent answered without building or using the sample Tool. Some models answer directly. Try /tour with a different model.").into(),
            "paused" => tr("The tour Run paused. Check the Run error in this Session, or use /tour to try again.").into(),
            "interrupted" => tr("The tour Run was interrupted. Use /tour to try again.").into(),
            _ => tr_format("The tour Run ended with status {}. Check the transcript for details.", &[status]),
        };
        self.tour = None;
        self.notes.push(note);
    }

    pub(super) fn tour_cleanup_reply(&mut self, result: &Value) {
        if let Some(tour) = self.tour.take() {
            if self.is_current(&tour.session_id) && self.input.expanded() == tour.prompt {
                self.input.clear();
            } else if let Some(draft) = self.drafts.get_mut(&tour.session_id) {
                if draft.input.expanded() == tour.prompt {
                    draft.input.clear();
                }
            }
        }
        let count = result["archived"].as_u64().unwrap_or_default().to_string();
        self.notes
            .push(tr_format("Tour Sessions archived: {}.", &[&count]));
        if result["retired_sample"] == true {
            self.notes.push(tr("The sample Tool was retired.").into());
        } else if result["kept_sample"] == true {
            self.notes
                .push(tr("The sample Tool was kept because it was modified or pinned.").into());
        }
        self.request(Pending::ListSessions, Request::ListSessions);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{client::DaemonClient, tests::test_app};
    use formless_protocol::{RequestEnvelope, ServerMessage};
    use serde_json::json;
    use std::sync::mpsc;

    fn open_tour(app: &mut App) {
        let session = json!({
            "id": "tour", "name": "Guided tour", "model": null, "reasoning_effort": null,
            "created_at": "2026-11-21T00:00:00Z", "updated_at": "2026-11-21T00:00:00Z", "cwd": "."
        });
        app.open_tour_reply(json!({"session":session, "sample_exists":false}));
    }

    #[test]
    fn tour_uses_only_the_acknowledged_run_and_resumes_after_switching() {
        let mut app = test_app();
        let (client, sent) = DaemonClient::recording();
        let (reply, inbox) = mpsc::channel();
        app.client = Some(client);
        app.inbox = Some(inbox);
        app.current = Some("original".into());
        app.input.set("keep this draft");
        app.attachments
            .push(("draft.txt".into(), b"draft".to_vec()));
        open_tour(&mut app);
        assert_eq!(app.current.as_deref(), Some("tour"));
        assert!(app.attachments.is_empty());
        app.submit_chat(false);
        let send = sent.try_iter().map(|line| serde_json::from_str::<RequestEnvelope>(&line).unwrap())
            .find(|request| matches!(&request.request, Request::SendMessage { session_id, .. } if session_id == "tour"))
            .unwrap();
        app.on_event(Event::RunFinished {
            session_id: "tour".into(),
            run_id: "unrelated".into(),
            status: "completed".into(),
        });
        assert!(!app.seen.tour_done);
        app.switch_session("original".into());
        assert_eq!(app.input.expanded(), "keep this draft");
        assert_eq!(app.attachments[0].0, "draft.txt");
        reply
            .send(ServerMessage::Response {
                id: send.id,
                ok: true,
                result: Some(json!({"started":true,"run_id":"sample"})),
                error: None,
            })
            .unwrap();
        app.drain_daemon();
        assert!(sent
            .try_iter()
            .map(|line| serde_json::from_str::<RequestEnvelope>(&line).unwrap())
            .all(|request| !matches!(request.request, Request::GetTourRun { .. })));
        app.slash("tour");
        let read = sent.try_iter().map(|line| serde_json::from_str::<RequestEnvelope>(&line).unwrap())
            .find(|request| matches!(&request.request, Request::GetTourRun { session_id, run_id } if session_id == "tour" && run_id == "sample"))
            .unwrap();
        app.apply_tour_status(
            "tour",
            "unrelated",
            &json!({"status":"completed", "used":true}),
        );
        assert!(!app.seen.tour_done);
        reply
            .send(ServerMessage::Response {
                id: read.id,
                ok: true,
                result: Some(json!({"status":"completed", "created":true, "used":true})),
                error: None,
            })
            .unwrap();
        app.drain_daemon();
        assert!(app.seen.tour_done);
        assert!(app.tour.is_none());
    }

    #[test]
    fn interruption_pause_and_inline_answers_never_complete_the_tour() {
        for (status, created, used, note) in [
            ("completed", false, false, "Some models answer directly"),
            ("completed", true, false, "did not use it successfully"),
            ("paused", false, false, "The tour Run paused"),
            ("interrupted", true, true, "The tour Run was interrupted"),
            ("failed", false, false, "ended with status failed"),
        ] {
            let mut app = test_app();
            open_tour(&mut app);
            app.tour_sending(11);
            app.tour_send_answered(11, true, Some(&json!({"started":true,"run_id":"run"})));
            app.apply_tour_status(
                "tour",
                "run",
                &json!({"status":status,"created":created,"used":used}),
            );
            assert!(app.notes.last().unwrap().contains(note));
            assert!(!app.seen.tour_done);
            assert!(app.tour.is_none());
        }
    }
}
