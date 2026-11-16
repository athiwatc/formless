use super::*;

pub(super) fn acknowledged(value: &Value) -> bool {
    let flag = |key| match value.get(key) {
        None => Some(false),
        Some(Value::Bool(v)) => Some(*v),
        _ => None,
    };
    let (Some(started), Some(steered)) = (flag("started"), flag("steered")) else {
        return false;
    };
    started != steered
        && value.get("queued").is_none_or(|v| v == false)
        && value["run_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
}

impl Engine {
    pub(super) fn start_send_review(&mut self, work: Box<Preparation>) {
        if matches!(work.intent, Intent::SendMessage { .. }) {
            self.request(
                Request::ListPendingAttachments {
                    session_id: work.intent.session_id().into(),
                },
                Pending::SendInventory(work),
            );
        } else {
            self.request_send_forecast(work);
        }
    }
    fn request_send_forecast(&mut self, work: Box<Preparation>) {
        self.request(
            Request::ForecastRun {
                input: forecast::input(&work.intent),
                for_send: true,
            },
            Pending::Forecast(work),
        );
    }
    pub(super) fn send_inventory_loaded(
        &mut self,
        mut work: Box<Preparation>,
        result: Result<Value, String>,
    ) {
        if !work.current(self) {
            return;
        }
        match result.and_then(|v| crate::pending_files::Report::decode(work.intent.session_id(),v)) {
            Ok(report) => {work.attachments=Some(report); self.request_send_forecast(work);}
            Err(_) => self.updates.push(Update::PreparationFailed {id:work.id,epoch:work.epoch,error:"Complete pending-file metadata is unavailable, unsupported or above bounds. Nothing was sent. Update the daemon or use Formless TUI; no legacy Send fallback was attempted".into()}),
        }
    }
    pub(super) fn commit_reviewed_send(&mut self, work: Box<Preparation>) {
        let Intent::SendMessage {
            session_id,
            content,
            mode,
        } = &work.intent
        else {
            return;
        };
        let Some(report) = work
            .attachments
            .as_ref()
            .filter(|r| r.session_id() == session_id)
        else {
            return;
        };
        if *mode == formless_protocol::SendMode::Queue {
            return;
        }
        self.request(
            Request::SendReviewedMessage {
                session_id: session_id.clone(),
                content: content.clone(),
                mode: *mode,
                expected_attachments: report.expected(),
                client_msg_id: work.id.to_string(),
            },
            Pending::ReviewedAction {
                action: Action {
                    id: work.id,
                    session_id: session_id.clone(),
                    question_id: None,
                },
                work,
                sent: false,
            },
        );
    }
}
