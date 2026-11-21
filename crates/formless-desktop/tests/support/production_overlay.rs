use super::*;
use formless_desktop::client::Update;
use formless_desktop::live_overlay::View;

#[gpui_kit::test]
fn production_preview_is_distinct_owned_and_replaced_by_canonical_history(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        let (client, connection) = shell.read(cx).client_identity().unwrap();
        let generation = shell.read(cx).client_generation();
        let session_id = sessions[0].id.clone();
        let view = View {
            answer: "Live answer".into(),
            reasoning: "Private reasoning".into(),
            unavailable: false,
        };
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::LiveOverlay {
                    client,
                    connection,
                    generation,
                    session_id: session_id.clone(),
                    view: view.clone(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("live-assistant").value(), Some("Live answer"));
        assert_eq!(
            window.find("live-reasoning").value(),
            Some("Private reasoning")
        );
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::LiveOverlay {
                    client,
                    connection,
                    generation: generation + 1,
                    session_id: session_id.clone(),
                    view: View::default(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert_eq!(window.find("live-assistant").value(), Some("Live answer"));
        assert!(window.find("production-live-overlay").bounds().size.height > px(0.));
        let snapshot = replies::session_snapshot(
            &session_id,
            &serde_json::json!({
                "session":{"id":session_id},"running":false,
                "messages":[typed_message(&session_id,1,"assistant","Canonical replacement")]
            }),
        )
        .unwrap();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Snapshot {
                    generation,
                    snapshot,
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.try_find("live-assistant").is_none());
        assert_eq!(shell.read(cx).last_block_preview(), "Canonical replacement");
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::LiveOverlay {
                    client,
                    connection,
                    generation,
                    session_id,
                    view,
                },
                window,
                cx,
            )
        });
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: false,
                    notice: "disconnected".into(),
                },
                window,
                cx,
            )
        });
        window.render_frame(cx);
        assert!(window.try_find("live-assistant").is_none());
    });
}
