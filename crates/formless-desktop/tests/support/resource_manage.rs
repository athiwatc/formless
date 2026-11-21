use super::*;
use formless_desktop::client::{Command as WireCommand, Update};
use formless_protocol::Request;
use serde_json::json;

#[gpui_kit::test]
fn resource_jobs_narrow_form_and_actions_stay_inside_without_collapsing_the_list(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, size(px(560.), px(780.)), 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |s, cx| {
            s.apply_client_update(Update::Page { destination: Destination::Jobs, result: Ok(json!([{"id":"j","prompt":"Review project status","enabled":true,"approved":false}])) }, window, cx);
            s.run_command(Command::GoTo(Destination::Jobs), window, cx);
        });
        settle_frames(window, cx);
        for id in [
            "resource-text-field",
            "resource-schedule-field",
            "resource-add",
            "resource-selected",
        ] {
            let bounds = window.find(id).bounds();
            assert!(
                bounds.left() >= px(0.) && bounds.right() <= px(560.),
                "{id}: {bounds:?}"
            );
        }
        let row = window.find(("resource-row", 0usize)).bounds();
        let title = window.find("resource-selected").bounds();
        assert!(title.top() - row.top() >= px(200.));
        for slot in 1..4usize {
            let bounds = window.find(("resource-action", slot)).bounds();
            assert!(bounds.left() >= px(0.) && bounds.right() <= px(560.));
        }
        let mut reached = [false; 4];
        for _ in 0..32 {
            window.press("tab", cx);
            settle_frames(window, cx);
            if window.find("resource-add").focused() == Some(true) {
                reached[0] = true;
                let bounds = window.find("resource-add").bounds();
                assert!(bounds.top() >= px(0.) && bounds.bottom() <= px(780.));
            }
            for slot in 1..4usize {
                if window.find(("resource-action", slot)).focused() == Some(true) {
                    reached[slot] = true;
                    let bounds = window.find(("resource-action", slot)).bounds();
                    assert!(bounds.top() >= px(0.) && bounds.bottom() <= px(780.));
                }
            }
        }
        assert!(reached.into_iter().all(|reached| reached));
    });
}

#[gpui_kit::test]
fn resource_management_confirms_cancels_and_invalidates_on_selection_refresh_and_navigation(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let value = json!([{"id":"a","text":"Memory A","enabled":true},{"id":"b","text":"Memory B","enabled":false}]);
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Page {
                    destination: Destination::Memory,
                    result: Ok(value.clone()),
                },
                window,
                cx,
            );
            s.run_command(Command::GoTo(Destination::Memory), window, cx);
            s.take_client_commands();
        });
        window.render_frame(cx);
        window.click(("resource-action", 2usize), cx);
        window.render_frame(cx);
        assert!(window.try_find("resource-action-cancel").is_some());
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
        window.click("resource-action-cancel", cx);
        window.render_frame(cx);
        assert!(window.try_find("resource-action-cancel").is_none());
        window.click(("resource-action", 2usize), cx);
        shell.update(cx, |s, cx| {
            s.focus_section(PageSection::Conversation, window, cx)
        });
        window.press("down", cx);
        window.render_frame(cx);
        assert!(window.try_find("resource-action-cancel").is_none());
        window.click(("resource-action", 2usize), cx);
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Page {
                    destination: Destination::Memory,
                    result: Ok(value.clone()),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.try_find("resource-action-cancel").is_none());
        window.click(("resource-action", 2usize), cx);
        shell.update(cx, |s, cx| {
            s.run_command(Command::GoTo(Destination::Tools), window, cx);
            s.run_command(Command::GoTo(Destination::Memory), window, cx);
            s.take_client_commands();
        });
        window.render_frame(cx);
        assert!(window.try_find("resource-action-cancel").is_none());
        window.click(("resource-action", 2usize), cx);
        window.render_frame(cx);
        window.click(("resource-action", 2usize), cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        assert!(commands.iter().any(|c| matches!(c, WireCommand::Manage { destination: Destination::Memory, request: Request::DeleteMemory { id }, .. } if id == "a")));
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "Offline".into(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        window.click(("resource-action", 1usize), cx);
        assert!(shell.update(cx, |s, _| s.take_client_commands()).is_empty());
    });
}

#[gpui_kit::test]
fn resource_add_form_validates_submits_clears_and_shows_receipts(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |s, cx| {
            s.apply_client_update(
                Update::Page {
                    destination: Destination::Memory,
                    result: Ok(json!([])),
                },
                window,
                cx,
            );
            s.run_command(Command::GoTo(Destination::Memory), window, cx);
            s.take_client_commands();
        });
        window.render_frame(cx);
        window.click("resource-add", cx);
        window.render_frame(cx);
        assert!(window
            .find("resource-validation-error")
            .value()
            .unwrap()
            .contains("must not be empty"));
        window.click("resource-text-field", cx);
        window.input("  Remember this  ", cx);
        window.render_frame(cx);
        window.click("resource-add", cx);
        let commands = shell.update(cx, |s, _| s.take_client_commands());
        assert!(commands.iter().any(|c| matches!(c, WireCommand::Manage { destination: Destination::Memory, request: Request::UpsertMemory { id: None, text: Some(text), .. }, .. } if text == "Remember this")));
        window.render_frame(cx);
        window.click("resource-add", cx);
        window.render_frame(cx);
        assert!(window
            .find("resource-validation-error")
            .value()
            .unwrap()
            .contains("must not be empty"));
        shell.update(cx, |s, cx| {
            s.apply_client_update(Update::Notice("Done: add memory".into()), window, cx)
        });
        window.render_frame(cx);
        assert_eq!(
            window.find("resource-result").value().as_deref(),
            Some("Done: add memory")
        );
    });
}
