use super::*;

pub(super) fn acknowledged(mode: formless_protocol::SendMode, value: &Value) -> bool {
    use formless_protocol::SendMode;
    let flag = |key| match value.get(key) {
        None => Some(false),
        Some(Value::Bool(v)) => Some(*v),
        _ => None,
    };
    let (Some(started), Some(steered)) = (flag("started"), flag("steered")) else {
        return false;
    };
    let matches_mode = match mode {
        SendMode::Send => started && !steered,
        SendMode::Steer => steered && !started,
        SendMode::Queue => false,
    };
    matches_mode
        && value.get("queued").is_none_or(|v| v == false)
        && value["run_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
}

impl Preparation {
    pub(super) fn acknowledges_reviewed_send(&self, value: &Value) -> bool {
        matches!(&self.intent, Intent::SendMessage { mode, .. } if acknowledged(*mode, value))
            && self
                .expected_run_id
                .as_deref()
                .is_none_or(|run_id| value["run_id"].as_str() == Some(run_id))
    }
}

impl Engine {
    pub(super) fn start_send_review(&mut self, work: Box<Preparation>) {
        let session_id = work.intent.session_id().into();
        let request = match &work.intent {
            Intent::SendMessage {
                mode: formless_protocol::SendMode::Steer,
                ..
            } => Request::ReadSteerReview { session_id },
            Intent::SendMessage { .. } => Request::ListPendingAttachments { session_id },
            _ => return self.request_send_forecast(work),
        };
        self.request(request, Pending::SendInventory(work));
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
        let result = result.and_then(|value| {
            crate::pending_files::Report::decode(work.intent.session_id(), value)
        });
        match result {
            Ok(report) if work.expected_run_id.as_deref().is_none_or(|id| report.steer_run_id() == Some(id)) => {
                work.attachments = Some(report);
                self.request_send_forecast(work);
            }
            Ok(_) => self.updates.push(Update::PreparationFailed {
                id: work.id,
                epoch: work.epoch,
                error: "The Run changed before review. Refresh the Session before steering. Draft kept.".into(),
            }),
            Err(_) => self.updates.push(Update::PreparationFailed {
                id: work.id,
                epoch: work.epoch,
                error: "Complete pending-file metadata or the reviewed Run is unavailable. Refresh the Session, update the daemon, or use Formless TUI. Draft kept.".into(),
            }),
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
        let request = match mode {
            formless_protocol::SendMode::Send => Request::SendReviewedMessage {
                session_id: session_id.clone(),
                content: content.clone(),
                mode: *mode,
                expected_attachments: report.expected(),
                client_msg_id: work.id.to_string(),
            },
            formless_protocol::SendMode::Steer => {
                let Some(run_id) = work
                    .expected_run_id
                    .as_deref()
                    .filter(|id| report.steer_run_id() == Some(*id))
                else {
                    return;
                };
                Request::SendReviewedSteer {
                    session_id: session_id.clone(),
                    run_id: run_id.into(),
                    content: content.clone(),
                    expected_attachments: report.expected(),
                    client_msg_id: work.id.to_string(),
                }
            }
            formless_protocol::SendMode::Queue => return,
        };
        self.request(
            request,
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
