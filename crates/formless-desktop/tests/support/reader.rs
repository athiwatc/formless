use super::*;
use formless_desktop::client::{StreamBatch, StreamOwner, StreamStatus, Update};
fn future_view(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
) -> Vec<formless_protocol::SessionInfo> {
    let sessions = daemon_view(cx, handle, shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.connect_recording_future_attempts(cx);
            shell.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "future fixture".into(),
                },
                window,
                cx,
            );
            shell.apply_client_update(Update::Sessions(sessions.clone()), window, cx);
        });
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        shell.update(cx, |shell, _| shell.take_client_commands());
    });
    sessions
}
fn owner(shell: &Entity<Shell>, cx: &App) -> StreamOwner {
    StreamOwner {
        client: shell.read(cx).live_stream_instance().unwrap(),
        load: 1,
        connection: 0,
        view_epoch: shell.read(cx).live_stream_epoch().unwrap(),
        generation: shell.read(cx).client_generation(),
        session_id: shell.read(cx).sidebar().active_id().unwrap().into(),
        basis: uuid::Uuid::new_v4(),
    }
}
fn batch(
    owner: StreamOwner,
    baseline: Option<replies::SessionSnapshot>,
    events: Vec<formless_protocol::Event>,
    reload: bool,
) -> Update {
    Update::Transcript(StreamBatch {
        owner,
        baseline,
        events,
        reload,
        status: None,
    })
}
#[gpui_kit::test]
fn reader_unchanged_owned_baseline_resets_retirement_before_next_attempt(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    future_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let mut current = owner(&shell, cx);
        let id = current.session_id.clone();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                batch(
                    current.clone(),
                    Some(event_snapshot(&id, "Question")),
                    vec![typed_delta(&id, "discarded prefix")],
                    false,
                ),
                window,
                cx,
            );
        });
        for cycle in 0..2 {
            shell.update(cx, |shell, cx| {
                shell.apply_client_update(batch(current.clone(), None, vec![], true), window, cx);
                shell.take_client_commands();
            });
            assert_eq!(shell.read(cx).block_count(), 1);
            assert!(shell.read(cx).transcript_reload_needed().is_some());
            current.load += 1;
            current.basis = uuid::Uuid::new_v4();
            // In the second cycle an empty persisted assistant advances the
            // canonical sequence without changing any rendered row.
            let snapshot = if cycle == 0 {
                event_snapshot(&id, "Question")
            } else {
                replies::session_snapshot(
                    &id,
                    &serde_json::json!({
                        "session":{"id":id}, "running":true,
                        "messages":[typed_message(&id,1,"user","Question"),
                            typed_message(&id,2,"assistant","")]
                    }),
                )
                .unwrap()
            };
            let text = format!("fresh attempt {cycle} ไทย 😀");
            let mut delta = typed_delta(&id, &text);
            if let formless_protocol::Event::MessageDelta { run_id, .. } = &mut delta {
                *run_id = format!("new-run-{cycle}");
            }
            let mut events = vec![delta];
            if cycle == 1 {
                // This duplicate must be ignored using the new hidden seq=2
                // floor; treating it as new would discard the fresh prefix.
                events.push(formless_protocol::Event::MessageAppended {
                    session_id: id.clone(),
                    message: typed_message(&id, 2, "assistant", ""),
                });
            }
            shell.update(cx, |shell, cx| {
                // The baseline and buffered fresh prefix share one accepted
                // batch, as they do in the real worker handshake.
                shell.apply_client_update(
                    batch(current.clone(), Some(snapshot), events, false),
                    window,
                    cx,
                );
                assert!(!shell
                    .take_client_commands()
                    .iter()
                    .any(|command| matches!(command, formless_desktop::client::Command::Refresh)));
            });
            settle_frames(window, cx);
            assert!(shell.read(cx).transcript_reload_needed().is_none());
            assert_eq!(shell.read(cx).live_stream_owner(), Some(current.clone()));
            assert_eq!(shell.read(cx).block_count(), 2);
            assert!(shell.read(cx).last_block_preview().contains(&text));
        }
    });
}

#[gpui_kit::test]
fn reader_view_rejects_unrequested_mode_and_stale_baselines(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = daemon_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        let owner = owner(&shell, cx);
        let id = owner.session_id.clone();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                batch(
                    owner,
                    Some(event_snapshot(&id, "unrequested")),
                    vec![typed_delta(&id, "unrequested")],
                    false,
                ),
                window,
                cx,
            )
        });
        assert!(shell.read(cx).live_stream_owner().is_none());
        assert_eq!(shell.read(cx).block_count(), 0);
    });
}
#[gpui_kit::test]
fn reader_view_batches_preserve_draft_and_live_text_while_status_refreshes(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    future_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        shell.update(cx, |shell, cx| {
            shell.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Unsent ไทย 😀", cx);
        let composer = shell.read(cx).composer().clone();
        let owner = owner(&shell, cx);
        let id = owner.session_id.clone();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                batch(
                    owner.clone(),
                    Some(event_snapshot(&id, "Question")),
                    vec![typed_delta(&id, "live prefix")],
                    false,
                ),
                window,
                cx,
            )
        });
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).block_count(), 2);
        assert!(shell.read(cx).last_block_preview().contains("live prefix"));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Transcript(StreamBatch {
                    owner: owner.clone(),
                    baseline: None,
                    events: vec![],
                    reload: false,
                    status: Some(StreamStatus {
                        steps: Default::default(),
                        running: true,
                        unfinished_run: None,
                    }),
                }),
                window,
                cx,
            )
        });
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                Update::Snapshot {
                    generation: owner.generation.saturating_sub(1),
                    snapshot: event_snapshot(&id, "stale"),
                },
                window,
                cx,
            )
        });
        assert_eq!(shell.read(cx).live_stream_owner(), Some(owner.clone()));
        assert_eq!(shell.read(cx).composer(), &composer);
        assert_eq!(composer.read(cx).value().as_ref(), "Unsent ไทย 😀");
        assert_eq!(shell.read(cx).block_count(), 2);
        assert!(shell.read(cx).last_block_preview().contains("live prefix"));
        for kind in 0..3 {
            let mut stale = owner.clone();
            match kind {
                0 => stale.basis = uuid::Uuid::new_v4(),
                1 => stale.connection += 1,
                _ => stale.view_epoch += 1,
            };
            shell.update(cx, |shell, cx| {
                shell.apply_client_update(
                    batch(stale, None, vec![typed_delta(&id, "wrong")], false),
                    window,
                    cx,
                )
            });
        }
        assert!(!shell.read(cx).last_block_preview().contains("wrong"));
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(batch(owner.clone(), None, vec![], true), window, cx)
        });
        assert_eq!(shell.read(cx).block_count(), 1);
        assert!(shell.read(cx).live_stream_owner().is_none());
        assert_eq!(
            shell.read(cx).transcript_reload_needed(),
            Some(&formless_desktop::transcript::ReloadReason::CanonicalRefresh)
        );
        let mut fresh = owner.clone();
        fresh.basis = uuid::Uuid::new_v4();
        fresh.load += 1;
        let snapshot=replies::session_snapshot(&id,&serde_json::json!({"session":{"id":id},"running":false,"messages":[typed_message(&id,1,"user","Question"),typed_message(&id,2,"assistant","Stored final")]})).unwrap();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(batch(fresh, Some(snapshot), vec![], false), window, cx)
        });
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                batch(
                    owner.clone(),
                    Some(event_snapshot(&id, "obsolete baseline")),
                    vec![typed_delta(&id, "obsolete buffered prefix")],
                    false,
                ),
                window,
                cx,
            )
        });
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                batch(owner, None, vec![typed_delta(&id, "late")], false),
                window,
                cx,
            )
        });
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).block_count(), 2);
        assert!(shell.read(cx).last_block_preview().contains("Stored final"));
        assert!(!shell.read(cx).last_block_preview().contains("late"));
        assert_eq!(shell.read(cx).composer(), &composer);
    });
}
#[gpui_kit::test]
fn reader_view_selection_and_physical_retirement_reject_queued_frames(cx: &mut TestAppContext) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = future_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let old = owner(&shell, cx);
        let a = old.session_id.clone();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                batch(
                    old.clone(),
                    Some(event_snapshot(&a, "A")),
                    vec![typed_delta(&a, "A partial")],
                    false,
                ),
                window,
                cx,
            )
        });
        window.render_frame(cx);
        window.click(session_row_id(&sessions[1].id), cx);
        let current = owner(&shell, cx);
        let b = current.session_id.clone();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                batch(
                    current.clone(),
                    Some(event_snapshot(&b, "B")),
                    vec![typed_delta(&b, "B partial")],
                    false,
                ),
                window,
                cx,
            )
        });
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                batch(old, None, vec![typed_delta(&a, "old A callback")], false),
                window,
                cx,
            )
        });
        settle_frames(window, cx);
        assert_eq!(shell.read(cx).sidebar().active_id(), Some(b.as_str()));
        assert!(!shell.read(cx).last_block_preview().contains("old A"));
        assert_eq!(shell.read(cx).retire_recording_question_connection(), 1);
        shell.update(cx, |shell, cx| shell.poll_client(window, cx));
        assert_eq!(shell.read(cx).block_count(), 1);
        assert!(shell.read(cx).live_stream_owner().is_none());
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                batch(
                    current,
                    None,
                    vec![typed_delta(&b, "retired physical callback")],
                    false,
                ),
                window,
                cx,
            )
        });
        assert_eq!(shell.read(cx).block_count(), 1);
    });
}

#[gpui_kit::test]
fn reader_view_profile_replacement_rejects_old_client_even_with_matching_counters(
    cx: &mut TestAppContext,
) {
    let (handle, shell) = open(cx, WIDE, 0, 0);
    let sessions = future_view(cx, handle, &shell);
    step(cx, handle, |window, cx| {
        let old = owner(&shell, cx);
        let id = old.session_id.clone();
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                batch(
                    old.clone(),
                    Some(event_snapshot(&id, "old Profile")),
                    vec![typed_delta(&id, "old private prefix")],
                    false,
                ),
                window,
                cx,
            )
        });
        shell.update(cx, |shell, cx| {
            shell.connect_recording_future_attempts(cx);
            shell.apply_client_update(
                Update::Connection {
                    ready: true,
                    notice: "new Profile".into(),
                },
                window,
                cx,
            );
            shell.apply_client_update(Update::Sessions(sessions.clone()), window, cx);
        });
        window.render_frame(cx);
        window.click(session_row_id(&sessions[0].id), cx);
        let current = owner(&shell, cx);
        assert_ne!(current.client, old.client);
        let mut stale = old;
        stale.generation = current.generation;
        stale.view_epoch = current.view_epoch;
        stale.connection = current.connection;
        stale.load = current.load + 100;
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                batch(
                    stale,
                    Some(event_snapshot(&id, "wrong Profile")),
                    vec![typed_delta(&id, "wrong Profile prefix")],
                    false,
                ),
                window,
                cx,
            )
        });
        assert_eq!(shell.read(cx).block_count(), 0);
        shell.update(cx, |shell, cx| {
            shell.apply_client_update(
                batch(
                    current,
                    Some(event_snapshot(&id, "new Profile")),
                    vec![],
                    false,
                ),
                window,
                cx,
            )
        });
        assert_eq!(shell.read(cx).block_count(), 1);
    });
}
