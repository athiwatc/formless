use super::*;
use formless_desktop::client::Update;
use serde_json::json;

fn snapshot(id: &str, start: i64, end: i64) -> replies::SessionSnapshot {
    let messages: Vec<_> = (start..=end)
        .map(|seq| {
            typed_message(
                id,
                seq,
                "user",
                &format!("Message {seq} ไทย 😀\nline two\nline three\nline four"),
            )
        })
        .collect();
    replies::session_snapshot(
        id,
        &json!({"session":{"id":id},"running":false,"messages":messages}),
    )
    .unwrap()
}
fn select(shell: &Entity<Shell>, id: &str, window: &mut Window, cx: &mut App) -> u64 {
    shell.update(cx, |shell, cx| {
        shell.run_command(Command::GoTo(Destination::Sessions), window, cx);
        shell.focus_section(PageSection::Conversation, window, cx);
    });
    for _ in 0..4 {
        window.press("up", cx);
    }
    for _ in 0..4 {
        if shell.read(cx).directory_selected_id() == Some(id) {
            break;
        }
        window.press("down", cx);
    }
    assert_eq!(shell.read(cx).directory_selected_id(), Some(id));
    window.press("enter", cx);
    let generation = shell.read(cx).client_generation();
    shell.update(cx, |shell, _| shell.take_client_commands());
    generation
}
fn reply(
    shell: &Entity<Shell>,
    id: &str,
    generation: u64,
    start: i64,
    end: i64,
    window: &mut Window,
    cx: &mut App,
) {
    shell.update(cx, |shell, cx| {
        shell.apply_client_update(
            Update::Snapshot {
                generation,
                snapshot: snapshot(id, start, end),
            },
            window,
            cx,
        )
    });
    settle_frames(window, cx);
}
fn older(shell: &Entity<Shell>, window: &mut Window, cx: &mut App) -> u64 {
    for _ in 0..2 {
        window.scroll(
            "timeline-area",
            ScrollDelta::Pixels(point(px(0.), px(1600.))),
            cx,
        );
        settle_frames(window, cx);
    }
    let message = shell
        .read(cx)
        .reading_message(cx)
        .expect("an older reading message");
    assert!(message < 90, "setup must be far from latest: {message}");
    assert!(!shell.read(cx).following_latest(cx));
    message
}
#[gpui_kit::test]
fn reading_positions_restore_message_identity_after_loading_and_keep_drafts(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let a = &sessions[0].id;
        let b = &sessions[1].id;
        let a_generation = select(&shell, a, window, cx);
        reply(&shell, a, a_generation, 1, 100, window, cx);
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Draft A ไทย 😀", cx);
        let a_message = older(&shell, window, cx);
        let b_generation = select(&shell, b, window, cx);
        assert_eq!(shell.read(cx).block_count(), 0);
        reply(&shell, b, b_generation, 1, 100, window, cx);
        assert!(shell.read(cx).following_latest(cx));
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Draft B", cx);
        let b_message = older(&shell, window, cx);
        let current = select(&shell, a, window, cx);
        assert_eq!(shell.read(cx).block_count(), 0);
        assert_eq!(
            shell.read(cx).composer().read(cx).value().as_ref(),
            "Draft A ไทย 😀"
        );
        // Neither an old generation for this ID nor another Session can restore.
        reply(&shell, a, a_generation, 1, 100, window, cx);
        assert_eq!(shell.read(cx).block_count(), 0);
        reply(&shell, b, current, 1, 100, window, cx);
        assert_eq!(shell.read(cx).block_count(), 0);
        // Earlier rows disappeared and new rows arrived: restore by seq, not index.
        reply(&shell, a, current, 10, 130, window, cx);
        assert_eq!(shell.read(cx).reading_message(cx), Some(a_message));
        assert!(!shell.read(cx).following_latest(cx));
        assert!(window.try_find(("block", a_message as usize)).is_some());
        // Refresh preserves a new manual position instead of replaying the old one.
        window.scroll(
            "timeline-area",
            ScrollDelta::Pixels(point(px(0.), px(1200.))),
            cx,
        );
        settle_frames(window, cx);
        let manual = shell.read(cx).reading_message(cx);
        assert_ne!(manual, Some(a_message));
        reply(&shell, a, current, 10, 140, window, cx);
        assert_eq!(shell.read(cx).reading_message(cx), manual);
        let current = select(&shell, b, window, cx);
        reply(&shell, b, current, 1, 130, window, cx);
        assert_eq!(shell.read(cx).reading_message(cx), Some(b_message));
        assert_eq!(
            shell.read(cx).composer().read(cx).value().as_ref(),
            "Draft B"
        );
        assert_eq!(shell.read(cx).saved_reading_positions(), 2);
        assert!(
            shell
                .update(cx, |shell, _| shell.take_client_commands())
                .is_empty(),
            "position restoration emits no RPC"
        );
    });
}
#[gpui_kit::test]
fn reading_positions_survive_leaving_an_unloaded_view_but_forget_resumed_latest(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let a = &sessions[0].id;
        let b = &sessions[1].id;
        let generation = select(&shell, a, window, cx);
        reply(&shell, a, generation, 1, 100, window, cx);
        let saved = older(&shell, window, cx);
        select(&shell, b, window, cx);
        let generation = select(&shell, a, window, cx);
        select(&shell, b, window, cx); // A never loaded; do not replace its saved position with latest.
        let current = select(&shell, a, window, cx);
        reply(&shell, a, generation, 1, 100, window, cx);
        assert_eq!(shell.read(cx).block_count(), 0);
        reply(&shell, a, current, 1, 100, window, cx);
        assert_eq!(shell.read(cx).reading_message(cx), Some(saved));
        for _ in 0..60 {
            if shell.read(cx).following_latest(cx) {
                break;
            }
            window.scroll(
                "timeline-area",
                ScrollDelta::Pixels(point(px(0.), px(-1600.))),
                cx,
            );
            settle_frames(window, cx);
        }
        assert!(shell.read(cx).following_latest(cx));
        select(&shell, b, window, cx);
        let generation = select(&shell, a, window, cx);
        reply(&shell, a, generation, 1, 130, window, cx);
        assert!(shell.read(cx).following_latest(cx));
        assert_eq!(shell.read(cx).saved_reading_positions(), 0);
    });
}
#[gpui_kit::test]
fn reading_positions_missing_message_and_empty_snapshot_fall_back_at_200_percent(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let a = &sessions[0].id;
        let b = &sessions[1].id;
        let generation = select(&shell, a, window, cx);
        reply(&shell, a, generation, 1, 100, window, cx);
        older(&shell, window, cx);
        select(&shell, b, window, cx);
        shell.update(cx, |shell, cx| {
            shell.set_text_scale(TextScale::LARGEST, window, cx)
        });
    });
    cx.simulate_window_resize(handle.into(), size(px(480.), px(360.)));
    step(cx, handle, |window, cx| {
        let a = &sessions[0].id;
        let b = &sessions[1].id;
        settle_frames(window, cx);
        let generation = select(&shell, a, window, cx);
        reply(&shell, a, generation, 200, 230, window, cx);
        assert!(shell.read(cx).following_latest(cx));
        assert_eq!(shell.read(cx).saved_reading_positions(), 0);
        let notice = window.find("reading-position-notice");
        assert!(notice.value().unwrap().contains("absent"));
        assert!(notice.bounds().size.width <= px(480.));
        assert!(window.try_find(("block", 230usize)).is_some());
        reply(&shell, a, generation, 200, 240, window, cx);
        assert!(window.try_find("reading-position-notice").is_none());
        assert!(shell.read(cx).following_latest(cx));
        select(&shell, b, window, cx);
        let generation = select(&shell, a, window, cx);
        reply(&shell, a, generation, 1, 0, window, cx);
        assert_eq!(shell.read(cx).block_count(), 0);
        assert!(shell.read(cx).following_latest(cx));
        assert!(window.try_find("reading-position-notice").is_none());
    });
}
#[gpui_kit::test]
fn reading_positions_cannot_cross_profile_attachment_or_reopen_old_dialogs(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let a = &sessions[0].id;
        let b = &sessions[1].id;
        let generation = select(&shell, a, window, cx);
        reply(&shell, a, generation, 1, 100, window, cx);
        let saved = older(&shell, window, cx);
        shell
            .update(cx, |shell, cx| shell.view_block(saved, window, cx))
            .unwrap();
        window.render_frame(cx);
        assert!(shell.read(cx).viewer_rows().is_some());
        // User dismisses the modal before choosing another Session.
        window.press("escape", cx);
        select(&shell, b, window, cx);
        assert_eq!(shell.read(cx).saved_reading_positions(), 1);
        shell.update(cx, |shell, cx| shell.connect_recording_daemon(cx));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "new attachment".into(),
                },
                window,
                cx,
            );
            shell.apply_client_update(Update::Sessions(sessions.clone()), window, cx);
        });
        assert_eq!(shell.read(cx).saved_reading_positions(), 0);
        let generation = select(&shell, a, window, cx);
        reply(&shell, a, generation, 1, 100, window, cx);
        assert!(shell.read(cx).following_latest(cx));
        assert!(shell.read(cx).viewer_rows().is_none());
    });
}

#[gpui_kit::test]
fn reading_positions_archive_restoration_keeps_position_and_deletion_prunes_it(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let mut sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let a = sessions[0].id.clone();
        let generation = select(&shell, &a, window, cx);
        reply(&shell, &a, generation, 1, 100, window, cx);
        let saved = older(&shell, window, cx);
        sessions[0].archived = true;
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Sessions(sessions.clone()), window, cx)
        });
        assert_eq!(shell.read(cx).saved_reading_positions(), 1);
        assert_eq!(shell.read(cx).block_count(), 0);
        sessions[0].archived = false;
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Sessions(sessions.clone()), window, cx)
        });
        let generation = select(&shell, &a, window, cx);
        reply(&shell, &a, generation, 1, 130, window, cx);
        assert_eq!(shell.read(cx).reading_message(cx), Some(saved));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(Update::Sessions(sessions[1..].to_vec()), window, cx)
        });
        assert_eq!(shell.read(cx).saved_reading_positions(), 0);
        assert_eq!(shell.read(cx).block_count(), 0);
    });
}

#[gpui_kit::test]
fn reading_positions_never_bookmark_a_provisional_streaming_message(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let a = &sessions[0].id;
        let generation = select(&shell, a, window, cx);
        reply(&shell, a, generation, 1, 3, window, cx);
        // This is the trusted synthetic reducer seam. Transport deltas stay off.
        shell
            .update(cx, |shell, cx| {
                shell.apply_transcript_event(&typed_delta(a, &"partial line\n".repeat(3000)), cx)
            })
            .unwrap();
    });
    cx.executor().advance_clock(Duration::from_millis(
        2 * formless_desktop::timeline::FLUSH_INTERVAL_MS,
    ));
    cx.run_until_parked();
    step(cx, handle, |window, cx| {
        let a = &sessions[0].id;
        let b = &sessions[1].id;
        settle_frames(window, cx);
        window.scroll(
            "timeline-area",
            ScrollDelta::Pixels(point(px(0.), px(800.))),
            cx,
        );
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).reading_message(cx), Some(u64::MAX));
        assert!(!shell.read(cx).following_latest(cx));
        select(&shell, b, window, cx);
        assert_eq!(shell.read(cx).saved_reading_positions(), 0);
        let generation = select(&shell, a, window, cx);
        reply(&shell, a, generation, 1, 100, window, cx);
        assert!(shell.read(cx).following_latest(cx));
    });
}
