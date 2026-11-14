use super::*;
use crate::questions::{self as model, Open, Outcome, MAX_TEXT_BYTES};
use formless_protocol::QuestionAnswer;
use gpui_kit::component::checkbox::Checkbox;
use serde_json::Value;

#[derive(serde::Serialize, serde::Deserialize)]
struct Saved {
    question: Open,
    answers: Vec<QuestionAnswer>,
}

pub(super) struct Panel {
    form: Option<Form>,
    focus: FocusHandle,
    pub close_focus: FocusHandle,
    scroll: ScrollHandle,
    pub notice: Option<String>,
    disconnected: bool,
    revealed: Option<FocusHandle>,
    frames: u8,
    connection: u64,
    viewport: Option<Size<Pixels>>,
    scale: TextScale,
    directory: Vec<(String, FocusHandle, gpui_kit::ScrollAnchor)>,
    saved_focus: FocusHandle,
    saved_anchor: gpui_kit::ScrollAnchor,
}

struct Form {
    question: Open,
    answers: Vec<QuestionAnswer>,
    field: usize,
    input: Entity<TextareaState>,
    _subscription: Subscription,
    accepted: String,
    rows: Vec<(FocusHandle, gpui_kit::ScrollAnchor)>,
    auxiliary: Vec<(FocusHandle, gpui_kit::ScrollAnchor)>,
    input_anchor: gpui_kit::ScrollAnchor,
    waiting: Option<uuid::Uuid>,
}

impl Shell {
    #[cfg(feature = "test-support")]
    pub fn retire_recording_question_connection(&self) -> u64 {
        let client = self.client.as_ref().unwrap();
        client.retire_recording_connection();
        client.connection()
    }
    #[cfg(feature = "test-support")]
    pub fn question_answer_input(&self) -> Option<Entity<TextareaState>> {
        self.question_panel
            .as_ref()
            .and_then(|panel| panel.form.as_ref())
            .map(|form| form.input.clone())
    }
    pub(super) fn open_questions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.question_panel.is_some() {
            return;
        }
        self.invalidate_forecast(cx);
        self.palette = None;
        self.viewer = None;
        self.prefs.close_overlays();
        let scroll = ScrollHandle::new();
        self.question_panel = Some(Panel {
            form: None,
            focus: cx.focus_handle(),
            close_focus: cx.focus_handle(),
            scroll: scroll.clone(),
            notice: None,
            disconnected: false,
            revealed: None,
            frames: 0,
            connection: self.client.as_ref().map_or(0, Client::connection),
            viewport: None,
            scale: self.text_scale,
            directory: Vec::new(),
            saved_focus: cx.focus_handle(),
            saved_anchor: gpui_kit::ScrollAnchor::for_handle(scroll),
        });
        self.questions_restored(window, cx);
        self.settle(window, cx);
    }

    fn question_input(
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<TextareaState>, Subscription) {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 2)
                .submit_on_enter(false)
                .placeholder("Your own answer (optional)")
                .default_value(text.to_string())
        });
        let subscription = cx.observe_in(&input, window, |this, _, window, cx| {
            this.question_input_changed(window, cx)
        });
        (input, subscription)
    }

    fn save_question_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(panel) = &mut self.question_panel else {
            return true;
        };
        let Some(form) = &mut panel.form else {
            return true;
        };
        if form.input.update(cx, |input, cx| {
            input.marked_text_range(window, cx).is_some()
        }) {
            panel.notice = Some("Finish composing the answer first.".into());
            cx.notify();
            return false;
        }
        let text = form.input.read(cx).value().to_string();
        if text.len() > MAX_TEXT_BYTES {
            panel.notice=Some("Answer text exceeds 64 KiB. It was not submitted; shorten it or copy it before closing.".into());
            cx.notify();
            return false;
        }
        form.answers[form.field].text = (!text.is_empty()).then_some(text.clone());
        form.accepted = text;
        true
    }

    fn question_input_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = &mut self.question_panel {
            panel.frames = 2;
            cx.notify();
        }
        let Some(form) = self
            .question_panel
            .as_ref()
            .and_then(|panel| panel.form.as_ref())
        else {
            return;
        };
        let input = form.input.clone();
        let accepted = form.accepted.clone();
        if self.save_question_input(window, cx) {
            return;
        }
        if input.read(cx).value().len() > MAX_TEXT_BYTES {
            cx.defer_in(window,move |this,window,cx| {
                let Some(form)=this.question_panel.as_ref().and_then(|panel| panel.form.as_ref()) else { return; };
                if form.input != input || input.update(cx,|input,cx| input.marked_text_range(window,cx).is_some()) || input.read(cx).value().len()<=MAX_TEXT_BYTES { return; }
                let focused=input.focus_handle(cx).contains_focused(window,cx);
                let (fresh,subscription)=Self::question_input(&accepted,window,cx);
                if focused { fresh.update(cx,|input,cx| input.focus(window,cx)); }
                let panel=this.question_panel.as_mut().unwrap();
                let form=panel.form.as_mut().unwrap(); form.input=fresh; form._subscription=subscription;
                panel.notice=Some("Edit not added: answer text is limited to 64 KiB. Earlier text kept; undo history cleared.".into());
                cx.notify();
            });
        }
    }

    fn keep_question_draft(&mut self, cx: &mut Context<Self>) {
        if let Some(form) = self
            .question_panel
            .as_ref()
            .and_then(|panel| panel.form.as_ref())
        {
            let authored = form.answers.iter().any(|answer| {
                !answer.selected.is_empty()
                    || answer.text.as_ref().is_some_and(|text| !text.is_empty())
            });
            let saved = if authored {
                serde_json::to_string(&Saved {
                    question: form.question.clone(),
                    answers: form.answers.clone(),
                })
                .expect("question draft serializes")
            } else {
                String::new()
            };
            let evicted = self.question_drafts.set(&form.question.id, &saved);
            self.question_draft_ids
                .retain(|id| id != &form.question.id && !evicted.contains(id));
            if self.question_drafts.has(&form.question.id) {
                self.question_draft_ids.push(form.question.id.clone());
            }
            if !evicted.is_empty() {
                self.composer_notice=Some(format!("Answers kept. {} older question drafts were evicted by the 16-question / 4 MiB retention limit.",evicted.len()).into());
            }
        }
        cx.notify();
    }

    pub(super) fn close_questions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(form) = self
            .question_panel
            .as_ref()
            .and_then(|panel| panel.form.as_ref())
        {
            let input = form.input.clone();
            if input.update(cx, |input, cx| {
                input.marked_text_range(window, cx).is_some()
            }) {
                input.update(cx, |input, cx| input.unmark_text(window, cx));
                self.question_input_changed(window, cx);
                cx.notify();
                return;
            }
        }
        if !self.save_question_input(window, cx) {
            return;
        }
        self.keep_question_draft(cx);
        self.question_panel = None;
        self.settle(window, cx);
    }

    fn open_question(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(question) = self
            .client_questions
            .open
            .iter()
            .find(|question| question.id == id)
            .cloned()
        else {
            return;
        };
        let saved = serde_json::from_str::<Saved>(self.question_drafts.get(id)).ok();
        if saved
            .as_ref()
            .is_some_and(|saved| saved.question != question)
        {
            self.question_panel.as_mut().unwrap().notice=Some("This question changed identity or contents. Earlier answers were kept; continue in the TUI.".into());
            cx.notify();
            return;
        }
        let answers = saved
            .map(|saved| saved.answers)
            .filter(|answers| question.validate_answers(answers).is_ok())
            .unwrap_or_else(|| vec![QuestionAnswer::default(); question.questions.len()]);
        let text = answers[0].text.clone().unwrap_or_default();
        let (input, subscription) = Self::question_input(&text, window, cx);
        let panel = self.question_panel.as_mut().unwrap();
        let rows = (0..question.questions[0].options.len())
            .map(|_| {
                (
                    cx.focus_handle().tab_stop(false),
                    gpui_kit::ScrollAnchor::for_handle(panel.scroll.clone()),
                )
            })
            .collect();
        let auxiliary = (0..4)
            .map(|_| {
                (
                    cx.focus_handle().tab_stop(false),
                    gpui_kit::ScrollAnchor::for_handle(panel.scroll.clone()),
                )
            })
            .collect();
        panel.form = Some(Form {
            question,
            answers,
            field: 0,
            input,
            _subscription: subscription,
            accepted: text,
            rows,
            auxiliary,
            input_anchor: gpui_kit::ScrollAnchor::for_handle(panel.scroll.clone()),
            waiting: None,
        });
        panel.notice = None;
        panel.scroll.set_offset(point(px(0.), px(0.)));
        panel.close_focus.focus(window, cx);
        cx.notify();
    }

    fn question_page(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.save_question_input(window, cx) {
            return;
        }
        let Some(form) = self
            .question_panel
            .as_ref()
            .and_then(|panel| panel.form.as_ref())
        else {
            return;
        };
        if form.waiting.is_some() {
            return;
        }
        let field = form
            .field
            .saturating_add_signed(delta)
            .min(form.question.questions.len() - 1);
        let text = form.answers[field].text.clone().unwrap_or_default();
        let (input, subscription) = Self::question_input(&text, window, cx);
        let panel = self.question_panel.as_mut().unwrap();
        let form = panel.form.as_mut().unwrap();
        form.field = field;
        form.input = input;
        form._subscription = subscription;
        form.accepted = text;
        form.rows = (0..form.question.questions[field].options.len())
            .map(|_| {
                (
                    cx.focus_handle().tab_stop(false),
                    gpui_kit::ScrollAnchor::for_handle(panel.scroll.clone()),
                )
            })
            .collect();
        panel.scroll.set_offset(point(px(0.), px(0.)));
        panel.close_focus.focus(window, cx);
        cx.notify();
    }

    fn question_current(&self, form: &Form) -> bool {
        self.client_connected
            && self.client.as_ref().is_some_and(|client| {
                client.is_ready() && client.connection() == self.client_questions.connection
            })
            && self
                .client_questions
                .open
                .iter()
                .any(|question| question == &form.question)
            && !self.client_questions.blocked.contains(&form.question.id)
            && !self.client_questions.inflight.contains(&form.question.id)
    }

    fn submit_question(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.save_question_input(window, cx) {
            return;
        }
        let Some(form) = self
            .question_panel
            .as_ref()
            .and_then(|panel| panel.form.as_ref())
        else {
            return;
        };
        if form.waiting.is_some() || !self.question_current(form) {
            return;
        }
        if let Err(error) = form.question.validate_answers(&form.answers) {
            self.question_panel.as_mut().unwrap().notice = Some(error.into());
            cx.notify();
            return;
        }
        let operation = uuid::Uuid::new_v4();
        let command = ClientCommand::AnswerQuestion {
            connection: self.client_questions.connection,
            operation,
            question: Box::new(form.question.clone()),
            answers: form.answers.clone(),
        };
        self.keep_question_draft(cx);
        let panel = self.question_panel.as_mut().unwrap();
        match self.client.as_ref().unwrap().send(command) {
            Ok(()) => {
                panel.form.as_mut().unwrap().waiting = Some(operation);
                panel.close_focus.focus(window, cx);
                panel.notice=Some("Waiting for the daemon to acknowledge these answers. They will not be retried automatically.".into());
            }
            Err(error) => {
                panel.notice = Some(format!("{error} Answers kept."));
            }
        }
        cx.notify();
    }

    pub(super) fn question_outcome(
        &mut self,
        connection: u64,
        operation: uuid::Uuid,
        id: &str,
        outcome: Outcome,
        cx: &mut Context<Self>,
    ) {
        if self
            .client
            .as_ref()
            .is_none_or(|client| client.connection() != connection)
        {
            return;
        }
        let Some(panel) = &mut self.question_panel else {
            return;
        };
        let Some(form) = &mut panel.form else {
            return;
        };
        if form.question.id != id || form.waiting != Some(operation) {
            return;
        }
        form.waiting = None;
        panel.notice = Some(match outcome {
            Outcome::Accepted => {
                self.client_questions
                    .open
                    .retain(|question| question.id != id);
                "Daemon accepted the answers. A copy is kept in this window.".into()
            }
            Outcome::Closed => {
                self.client_questions
                    .open
                    .retain(|question| question.id != id);
                "This question was no longer waiting. These answers were not accepted; a copy is kept.".into()
            }
            Outcome::Rejected(error) => format!(
                "Answer rejected: {}{}. Text kept.",
                error.chars().take(500).collect::<String>(),
                if error.chars().nth(500).is_some() {
                    "… (details in the TUI)"
                } else {
                    ""
                }
            ),
            Outcome::Unknown => {
                self.client_questions.blocked.insert(id.into());
                "Answer outcome unknown. Do not retry; inspect this Session in the TUI. Text kept."
                    .into()
            }
        });
        cx.notify();
    }

    pub(super) fn questions_disconnected(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = &mut self.question_panel {
            panel.disconnected = true;
            if let Some(form) = &mut panel.form {
                form.waiting = None;
            }
            panel.notice=Some("Disconnected. Answers are kept and will not be retried. Wait for the current question to reload before taking another action.".into());
        }
        cx.notify();
    }

    pub(super) fn questions_restored(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(panel) = &mut self.question_panel {
            panel.frames = 2;
            if panel.connection != self.client_questions.connection {
                panel.connection = self.client_questions.connection;
                panel.disconnected = true;
                if let Some(form) = &mut panel.form {
                    form.waiting = None;
                }
            }
            let lost_focus = panel.directory.iter().any(|(id, focus, _)| {
                !self
                    .client_questions
                    .open
                    .iter()
                    .any(|question| &question.id == id)
                    && focus.is_focused(window)
            });
            let mut old: HashMap<_, _> = panel
                .directory
                .drain(..)
                .map(|(id, focus, anchor)| (id, (focus, anchor)))
                .collect();
            panel.directory = self
                .client_questions
                .open
                .iter()
                .map(|question| {
                    let (focus, anchor) = old.remove(&question.id).unwrap_or_else(|| {
                        (
                            cx.focus_handle(),
                            gpui_kit::ScrollAnchor::for_handle(panel.scroll.clone()),
                        )
                    });
                    (question.id.clone(), focus, anchor)
                })
                .collect();
            if lost_focus && panel.form.is_none() {
                panel.close_focus.focus(window, cx);
            }
            if panel.disconnected {
                panel.disconnected = false;
                panel.notice=Some(if panel.form.as_ref().is_some_and(|form|self.client_questions.blocked.contains(&form.question.id)) {
                    "Answer outcome unknown. Do not retry; inspect its Session in the TUI. Text kept."
                }else{"Connected again. Review the question's current status before submitting; your answers were kept."}.into());
            }
        }
        cx.notify();
    }

    pub(super) fn reveal_question_focus(&mut self, window: &mut Window, cx: &mut App) {
        let Some(panel) = &mut self.question_panel else {
            return;
        };
        let focused = window.focused(cx);
        if panel.revealed != focused
            || panel.viewport != Some(window.viewport_size())
            || panel.scale != self.text_scale
        {
            panel.revealed = focused;
            panel.viewport = Some(window.viewport_size());
            panel.scale = self.text_scale;
            panel.frames = 2;
        }
        if panel.frames == 0 {
            return;
        }
        panel.frames -= 1;
        if let Some(form) = &panel.form {
            let anchor = if form.input.focus_handle(cx).contains_focused(window, cx) {
                Some(&form.input_anchor)
            } else {
                form.rows
                    .iter()
                    .chain(form.auxiliary.iter())
                    .find(|(focus, _)| focus.contains_focused(window, cx))
                    .map(|(_, anchor)| anchor)
            };
            if let Some(anchor) = anchor {
                anchor.scroll_to(window, cx);
                window.request_animation_frame();
            }
        } else if panel.saved_focus.is_focused(window) {
            panel.saved_anchor.scroll_to(window, cx);
            window.request_animation_frame();
        } else if let Some((_, _, anchor)) = panel
            .directory
            .iter()
            .find(|(_, focus, _)| focus.is_focused(window))
        {
            anchor.scroll_to(window, cx);
            window.request_animation_frame();
        }
    }

    pub(super) fn render_questions(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let panel = self.question_panel.as_ref().unwrap();
        let margin = px(16.);
        let viewport = window.viewport_size();
        let mut body = v_flex()
            .id("questions-body")
            .test_support()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&panel.scroll)
            .gap_2();
        if let Some(form) = &panel.form {
            let question = &form.question.questions[form.field];
            let current = self.question_current(form);
            let waiting = form.waiting.is_some();
            body=body.child(readable("question-session",format!("Session: {}",model::preview(&form.question.session_id,160))))
                .child(readable("question-position",format!("Question {} of {} · {}",form.field+1,form.question.questions.len(),if question.multi_select {"Choose any options and/or write your answer"} else {"Choose one option and/or write your answer"})))
                .children(question.header.as_ref().map(|header|readable("question-header",model::preview(header,100)).text_sm()))
                .child(readable("question-prompt",model::preview(&question.question,1500)))
                .when(!current,|body|body.child(readable("question-stale","This question is closed, disconnected or has an unknown previous answer. Submission is disabled; your answers remain copyable.")))
                .children(question.options.iter().enumerate().map(|(index,option)|{
                    let short:String=option.label.chars().take(16).collect();
                    let clipped=short.len()!=option.label.len();
                    v_flex().id(("question-option-row",index)).test_support().track_focus(&form.rows[index].0).gap_1()
                        .child(div().id(("question-option-focus",index)).test_support().anchor_scroll(Some(form.rows[index].1.clone())).pt_1().child(Checkbox::new(("question-option",index)).w_full().label(format!("{short}{}",if clipped {"…"}else{""})).accessibility_label(option.label.clone())
                            .checked(form.answers[form.field].selected.contains(&option.label)).disabled(waiting || !current)
                            .on_click(cx.listener(move|this,_,_,cx|{if let Some(form)=this.question_panel.as_mut().and_then(|panel|panel.form.as_mut()){form.question.toggle(&mut form.answers,form.field,index);cx.notify();}}))))
                        .when(clipped,|row|row.child(readable(SharedString::from(format!("question-option-label-{index}")),model::preview(&option.label,400)).text_sm()))
                        .children(option.description.as_ref().map(|description|readable(SharedString::from(format!("question-option-description-{index}")),model::preview(description,400)).text_sm()))
                }))
                .child(div().id("question-input-anchor").test_support().pt_1().anchor_scroll(Some(form.input_anchor.clone()))
                    .child(Textarea::new(&form.input).aria_label("Your answer").disabled(waiting).w_full().on_paste({let shell=cx.entity();move|item,_,cx|{
                        if item.text().is_some_and(|text|text.len()>MAX_TEXT_BYTES) {shell.update(cx,|this,cx|{if let Some(panel)=&mut this.question_panel{panel.notice=Some("Paste not added: answer text is limited to 64 KiB.".into());cx.notify();}});true}else{false}
                    }})))
                .child(readable("question-empty-help","Blank fields are returned as unanswered. Enter inserts a line; Submit is an explicit button." ).text_sm());
        } else {
            body=body.child(readable("questions-scope","Questions belong to the Sessions shown below, across this Profile. Approvals still require the TUI."))
                .when(self.client_questions.open.is_empty(),|body|body.child(readable("questions-empty","No pending questions loaded. Open a Session to restore existing questions; newly arriving questions appear here without changing focus.")))
                .children(self.client_questions.open.iter().map(|question|{
                    let id=question.id.clone();
                    let (_,focus,anchor)=panel.directory.iter().find(|(current,_,_)|current==&id).expect("question directory focus is synchronized");
                    div().id(SharedString::from(format!("question-entry-{id}"))).test_support().pt_1().anchor_scroll(Some(anchor.clone()))
                        .child(Button::new(SharedString::from(format!("open-question-{id}"))).w_full().track_focus(focus).label(format!("Session {} · {}",model::preview(&question.session_id,100),model::preview(&question.questions[0].question,200))).on_click(cx.listener(move|this,_,window,cx|this.open_question(&id,window,cx))))
                }));
            if !self.question_draft_ids.is_empty() {
                body=body.child(readable("questions-saved-help",format!("{} saved answer(s) in this window. Retention is limited to 16 questions and 4 MiB; closing the app clears them.",self.question_draft_ids.len())))
                    .child(div().id("questions-saved-anchor").test_support().pt_1().anchor_scroll(Some(panel.saved_anchor.clone()))
                        .child(Button::new("questions-copy-saved").label("Copy saved answers").track_focus(&panel.saved_focus).on_click(cx.listener(|this,_,_,cx|{
                            let saved:Vec<Value>=this.question_draft_ids.iter().filter_map(|id|serde_json::from_str(this.question_drafts.get(id)).ok()).collect();
                            cx.write_to_clipboard(ClipboardItem::new_string(serde_json::to_string_pretty(&saved).unwrap()));
                        }))));
            }
        }
        body = body
            .children(
                self.client_questions
                    .notice
                    .as_ref()
                    .map(|notice| readable("questions-directory-notice", notice.clone())),
            )
            .children(panel.notice.as_ref().map(|notice| {
                announced(div().id("questions-notice"))
                    .test_support()
                    .role(Role::Label)
                    .aria_value(notice.clone())
                    .child(notice.clone())
            }));
        let mut footer = h_flex().flex_shrink_0().flex_wrap().gap_2().child(
            Button::new("questions-close")
                .label("Close")
                .track_focus(&panel.close_focus)
                .on_click(cx.listener(|this, _, window, cx| this.close_questions(window, cx))),
        );
        if let Some(form) = &panel.form {
            let actions = h_flex()
                .flex_wrap()
                .gap_2()
                .child(
                    div()
                        .id("question-copy-anchor")
                        .test_support()
                        .pt_1()
                        .anchor_scroll(Some(form.auxiliary[0].1.clone()))
                        .child(
                            Button::new("question-copy")
                                .track_focus(&form.auxiliary[0].0)
                                .label("Copy answers")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(form) = this
                                        .question_panel
                                        .as_ref()
                                        .and_then(|panel| panel.form.as_ref())
                                    {
                                        let mut answers = form.answers.clone();
                                        answers[form.field].text =
                                            Some(form.input.read(cx).value().to_string());
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            serde_json::to_string_pretty(&Saved {
                                                question: form.question.clone(),
                                                answers,
                                            })
                                            .unwrap(),
                                        ));
                                    }
                                })),
                        ),
                )
                .child(
                    div()
                        .id("question-prompt-copy-anchor")
                        .test_support()
                        .pt_1()
                        .anchor_scroll(Some(form.auxiliary[1].1.clone()))
                        .child(
                            Button::new("question-prompt-copy")
                                .track_focus(&form.auxiliary[1].0)
                                .label("Copy prompt")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(form) = this
                                        .question_panel
                                        .as_ref()
                                        .and_then(|panel| panel.form.as_ref())
                                    {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            serde_json::to_string_pretty(&form.question).unwrap(),
                                        ));
                                    }
                                })),
                        ),
                )
                .child(
                    div()
                        .id("question-previous-anchor")
                        .test_support()
                        .pt_1()
                        .anchor_scroll(Some(form.auxiliary[2].1.clone()))
                        .child(
                            Button::new("question-previous")
                                .track_focus(&form.auxiliary[2].0)
                                .label("Previous")
                                .disabled(form.field == 0 || form.waiting.is_some())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.question_page(-1, window, cx)
                                })),
                        ),
                )
                .child(
                    div()
                        .id("question-next-anchor")
                        .test_support()
                        .pt_1()
                        .anchor_scroll(Some(form.auxiliary[3].1.clone()))
                        .child(
                            Button::new("question-next")
                                .track_focus(&form.auxiliary[3].0)
                                .label("Next")
                                .disabled(
                                    form.field + 1 == form.question.questions.len()
                                        || form.waiting.is_some(),
                                )
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.question_page(1, window, cx)
                                })),
                        ),
                );
            body = body.child(actions);
            footer = footer.child(
                Button::new("question-submit")
                    .label("Submit")
                    .accessibility_label("Submit answers")
                    .tooltip("Submit all answers to this question")
                    .primary()
                    .disabled(form.waiting.is_some() || !self.question_current(form))
                    .on_click(cx.listener(|this, _, window, cx| this.submit_question(window, cx))),
            );
        }
        let dialog=v_flex().id("questions-dialog").test_support().role(Role::Dialog).aria_label("Profile questions").aria_description("Modal. Tab reaches controls. Arrow, Page, Home and End keys scroll outside text fields. Escape closes after composition; unsent answers are kept for this window.")
            .map(modal_dialog).key_context(QUESTIONS_CONTEXT).on_action(cx.listener(|this,_:&CloseQuestions,window,cx|this.close_questions(window,cx)))
            .on_action(cx.listener(|this,_:&ScrollViewerUp,_,cx|this.scroll_questions(1.,false,cx)))
            .on_action(cx.listener(|this,_:&ScrollViewerDown,_,cx|this.scroll_questions(-1.,false,cx)))
            .on_action(cx.listener(|this,_:&ScrollViewerPageUp,_,cx|this.scroll_questions(1.,true,cx)))
            .on_action(cx.listener(|this,_:&ScrollViewerPageDown,_,cx|this.scroll_questions(-1.,true,cx)))
            .on_action(cx.listener(|this,_:&ScrollViewerStart,_,cx|{if let Some(panel)=&this.question_panel{panel.scroll.set_offset(point(px(0.),px(0.)));cx.notify();}}))
            .on_action(cx.listener(|this,_:&ScrollViewerEnd,_,cx|{if let Some(panel)=&this.question_panel{panel.scroll.scroll_to_bottom();cx.notify();}}))
            .w(self.scaled(680.).min(viewport.width-margin*2.)).max_h(viewport.height-margin*2.).p_3().gap_2()
            .bg(cx.theme().popover).text_color(cx.theme().popover_foreground).border_1().border_color(cx.theme().border).rounded_lg().shadow_lg().occlude()
            .child(readable("questions-heading","Profile questions").text_lg().flex_shrink_0()).child(body).child(footer).focus_trap("questions-dialog",&panel.focus);
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(cx.theme().overlay)
                    .occlude(),
            )
            .child(
                h_flex()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .justify_center()
                    .items_start()
                    .pt(margin)
                    .child(dialog),
            )
    }

    fn scroll_questions(&mut self, lines: f32, page: bool, cx: &mut Context<Self>) {
        let Some(panel) = &self.question_panel else {
            return;
        };
        let amount = if page {
            panel.scroll.bounds().size.height.max(self.scaled(40.))
        } else {
            self.scaled(40.)
        };
        let offset = panel.scroll.offset();
        let y = (offset.y + amount * lines).clamp(-panel.scroll.max_offset().y, px(0.));
        panel.scroll.set_offset(point(offset.x, y));
        cx.notify();
    }
}
