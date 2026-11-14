use super::*;
use formless_desktop::client::{Command as WireCommand, Update};
use formless_desktop::questions::{Open, Outcome, State};
use formless_protocol::{Event, Question, QuestionOption};

pub fn definition(session: &str) -> Open {
    Open::from_event(&Event::UserQuestion {
        question_id: "native-question".into(),
        session_id: session.into(),
        question: String::new(),
        options: vec![],
        questions: vec![
            Question {
                question: "Choose one ไทย?".into(),
                header: Some("Choice".into()),
                options: vec![
                    QuestionOption {
                        label: "First".into(),
                        description: Some("First choice".into()),
                    },
                    QuestionOption {
                        label: "Second".into(),
                        description: None,
                    },
                ],
                multi_select: false,
            },
            Question {
                question: "Choose any 漢字 😀?".into(),
                header: None,
                options: vec![
                    QuestionOption {
                        label: "Alpha".into(),
                        description: None,
                    },
                    QuestionOption {
                        label: "Beta".into(),
                        description: None,
                    },
                    QuestionOption {
                        label: "Gamma".into(),
                        description: None,
                    },
                ],
                multi_select: true,
            },
        ],
    })
    .unwrap()
}

fn show(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    question: Open,
) {
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Questions(State {
                    open: vec![question],
                    ..State::default()
                }),
                window,
                cx,
            )
        });
        window.render_frame(cx);
        window.click("show-questions", cx);
        window.render_frame(cx);
        window.click("open-question-native-question", cx);
        window.render_frame(cx);
    });
}

#[gpui_kit::test]
fn native_question_answers_preserve_original_session_choices_and_exact_text(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        shell.update(cx, |shell, _| {
            shell.take_client_commands();
        });
    });
    let question = definition(&sessions[1].id);
    show(cx, handle, &shell, question.clone());
    step(cx, handle, |window, cx| {
        assert_eq!(window.find("questions-close").focused(), Some(true));
        assert!(shell.read(cx).background_inert(window));
        assert_eq!(
            shell.read(cx).sidebar().active_id(),
            Some(sessions[0].id.as_str())
        );
        assert!(window
            .find("question-session")
            .value()
            .unwrap()
            .contains(&sessions[1].id));
        window.press("f6", cx);
        window.render_frame(cx);
        assert_eq!(window.find("questions-close").focused(), Some(true));
        window.press("tab", cx);
        window.render_frame(cx);
        assert_eq!(window.find("question-submit").focused(), Some(true));
        window.press("tab", cx);
        window.render_frame(cx);
        assert_eq!(
            window.find(("question-option", 0usize)).focused(),
            Some(true)
        );
        window.press("space", cx);
        window.click(("question-option", 1usize), cx);
        window.click_at("question-input-anchor", point(px(20.), px(15.)), cx);
        window.input("Own answer ไทย 😀", cx);
        window.press("enter", cx);
        window.input("second line", cx);
        let input = shell.read(cx).question_answer_input().unwrap();
        let end = input.read(cx).selected_range().end;
        window.press("up", cx);
        assert!(
            input.read(cx).selected_range().end < end,
            "question scrolling must not steal input caret keys"
        );
        window.press("down", cx);
        assert_eq!(input.read(cx).selected_range().end, end);
        window.click("question-next", cx);
        window.render_frame(cx);
        window.click(("question-option", 0usize), cx);
        window.click(("question-option", 2usize), cx);
        window.click_at("question-input-anchor", point(px(20.), px(15.)), cx);
        window.input("自由入力", cx);
        window.click("question-previous", cx);
        window.render_frame(cx);
        window.click_at("question-input-anchor", point(px(20.), px(15.)), cx);
        window.press(&chord("z"), cx);
        assert_eq!(
            shell
                .read(cx)
                .question_answer_input()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            "Own answer ไทย 😀\nsecond line"
        );
        window.click("question-submit", cx);
        window.render_frame(cx);
        window.click("question-submit", cx);
        let commands = shell.update(cx, |shell, _| shell.take_client_commands());
        let answers: Vec<_> = commands
            .into_iter()
            .filter_map(|command| match command {
                WireCommand::AnswerQuestion {
                    question,
                    answers,
                    operation,
                    ..
                } => Some((question, answers, operation)),
                _ => None,
            })
            .collect();
        assert_eq!(answers.len(), 1);
        assert_eq!(*answers[0].0, question);
        assert_eq!(answers[0].1[0].selected, ["Second"]);
        assert_eq!(
            answers[0].1[0].text.as_deref(),
            Some("Own answer ไทย 😀\nsecond line")
        );
        assert_eq!(answers[0].1[1].selected, ["Alpha", "Gamma"]);
        assert_eq!(answers[0].1[1].text.as_deref(), Some("自由入力"));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::QuestionAnswer {
                    connection: 0,
                    operation: answers[0].2,
                    question_id: question.id.clone(),
                    outcome: Outcome::Closed,
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("question-submit").disabled(), Some(true));
        assert!(window
            .find("questions-notice")
            .value()
            .unwrap()
            .contains("not accepted"));
        window.click("question-copy", cx);
        let copy: serde_json::Value =
            serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap();
        assert_eq!(copy["answers"][0]["text"], "Own answer ไทย 😀\nsecond line");
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        window.press("escape", cx);
        window.render_frame(cx);
        window.click("show-questions", cx);
        window.render_frame(cx);
        assert!(window.try_find("open-question-native-question").is_none());
        window.click("questions-copy-saved", cx);
        let saved: serde_json::Value =
            serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap();
        assert_eq!(
            saved[0]["answers"][0]["text"],
            "Own answer ไทย 😀\nsecond line"
        );
    });
}

#[gpui_kit::test]
fn native_question_close_preserves_text_and_stale_updates_cannot_enable_submission(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    let question = definition(&sessions[0].id);
    show(cx, handle, &shell, question.clone());
    step(cx, handle, |window, cx| {
        window.click_at("question-input-anchor", point(px(20.), px(15.)), cx);
        window.input("keep this draft", cx);
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(window.try_find("questions-dialog").is_none());
    });
    cx.run_until_parked();
    show(cx, handle, &shell, question.clone());
    step(cx, handle, |window, cx| {
        assert_eq!(
            shell
                .read(cx)
                .question_answer_input()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            "keep this draft"
        );
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "Disconnected fixture".into(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("question-submit").disabled(), Some(true));
        // A retired connection's question inventory cannot replace this one.
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Questions(State {
                    connection: 999,
                    open: vec![question],
                    ..State::default()
                }),
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("question-submit").disabled(), Some(true));
        window.click("question-copy", cx);
        assert!(cx
            .read_from_clipboard()
            .unwrap()
            .text()
            .unwrap()
            .contains("keep this draft"));
    });
}

#[gpui_kit::test]
fn native_question_submit_rechecks_live_marked_text_before_deferred_observers(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    show(cx, handle, &shell, definition(&sessions[0].id));
    step(cx, handle, |window, cx| {
        let input = shell.read(cx).question_answer_input().unwrap();
        let bounds = window.find_by_label("Your answer").bounds();
        TestInput::new(ElementInputHandler::new(bounds, input), window)
            .compose_text(None, "かな", None, cx);
        window.click("question-submit", cx);
        assert!(!shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|command| matches!(command, WireCommand::AnswerQuestion { .. })));
        window.press("escape", cx);
        window.render_frame(cx);
        assert!(
            window.try_find("questions-dialog").is_some(),
            "Escape ends composition before closing the form"
        );
    });
}

#[gpui_kit::test]
fn oversized_question_edits_are_rejected_whole_before_submission(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    show(cx, handle, &shell, definition(&sessions[0].id));
    step(cx, handle, |window, cx| {
        window.click_at("question-input-anchor", point(px(20.), px(15.)), cx);
        window.input("keep", cx);
        window.render_frame(cx);
    });
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        let input = shell.read(cx).question_answer_input().unwrap();
        input.update(cx, |input, cx| {
            input.set_value(
                "x".repeat(formless_desktop::questions::MAX_TEXT_BYTES + 1),
                window,
                cx,
            )
        });
        window.click("question-submit", cx);
        assert!(!shell
            .update(cx, |shell, _| shell.take_client_commands())
            .iter()
            .any(|command| matches!(command, WireCommand::AnswerQuestion { .. })));
    });
    cx.run_until_parked();
    step(cx, handle, |_, cx| {
        assert_eq!(
            shell
                .read(cx)
                .question_answer_input()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            "keep"
        )
    });
}
