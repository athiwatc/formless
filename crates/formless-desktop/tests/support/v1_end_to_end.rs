//! Actual fake-provider -> daemon -> IPC -> Client -> TestPlatform Shell path.
//! No native renderer/window, real Profile, Tool or external provider executes.
use super::*;
use formless_desktop::client::{Endpoint, FUTURE_ATTEMPT_LIMITATION};
use formless_protocol::Request;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use tokio::sync::Notify;

fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect()
}
fn prefix(text: &str) -> String {
    sse(&[
        json!({"type":"message_start","message":{"usage":{"input_tokens":10}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
    ])
}
fn delta(text: &str) -> String {
    sse(&[
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
    ])
}
fn tail(text: &str) -> String {
    delta(text)
        + &sse(&[
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":10}}),
            json!({"type":"message_stop"}),
        ])
}
struct Fixture {
    runtime: tokio::runtime::Runtime,
    root: std::path::PathBuf,
    daemon: Arc<formless_core::daemon::Daemon>,
    server: tokio::task::JoinHandle<Result<(), String>>,
    provider: tokio::task::JoinHandle<()>,
    endpoint: Endpoint,
    session: String,
    calls: Arc<AtomicUsize>,
    models: Arc<Mutex<Vec<String>>>,
    catalog_calls: Arc<AtomicUsize>,
    complete: Vec<Arc<Notify>>,
    extra: Arc<Notify>,
}
impl Fixture {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let complete = (0..3).map(|_| Arc::new(Notify::new())).collect::<Vec<_>>();
        let extra = Arc::new(Notify::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let models = Arc::new(Mutex::new(Vec::new()));
        let catalog_calls = Arc::new(AtomicUsize::new(0));
        #[cfg(unix)]
        let temp = std::path::PathBuf::from("/tmp");
        #[cfg(windows)]
        let temp = std::env::temp_dir();
        let root = temp.join(format!(
            "fl-v1-ui-{}",
            &uuid::Uuid::new_v4().to_string()[..8]
        ));
        std::fs::create_dir(&root).unwrap();
        let (daemon, server, provider, endpoint, session) = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let app = axum::Router::new()
                .route(
                    "/models",
                    axum::routing::get({
                        let calls = catalog_calls.clone();
                        move || {
                            calls.fetch_add(1, Ordering::SeqCst);
                            async { axum::Json(json!({"data":[{"id":"v1-test"}]})) }
                        }
                    }),
                )
                .route(
                    "/messages",
                    axum::routing::post({
                        let complete = complete.clone();
                        let extra = extra.clone();
                        let calls = calls.clone();
                        let models = models.clone();
                        move |axum::Json(body): axum::Json<Value>| {
                            models
                                .lock()
                                .unwrap()
                                .push(body["model"].as_str().expect("provider model").to_string());
                            let number = calls.fetch_add(1, Ordering::SeqCst);
                            assert!(number < 3, "unexpected provider request");
                            let complete = complete[number].clone();
                            let extra = extra.clone();
                            async move {
                                let stream = futures_util::stream::unfold(
                                    (0, complete, extra),
                                    move |(phase, complete, extra)| async move {
                                        let chunk = match phase {
                                            0 => prefix(
                                                [
                                                    "A prefix before attachment ",
                                                    "B future prefix ",
                                                    "C next eligible prefix ",
                                                ][number],
                                            ),
                                            1 if number == 1 => {
                                                extra.notified().await;
                                                delta("B suffix after reconnect ")
                                            }
                                            1 | 2 if phase == 1 || number == 1 => {
                                                complete.notified().await;
                                                tail(
                                                    ["A final 😀", "B final 漢字", "C final ไทย"]
                                                        [number],
                                                )
                                            }
                                            _ => return None,
                                        };
                                        Some((
                                            Ok::<_, std::io::Error>(chunk),
                                            (phase + 1, complete, extra),
                                        ))
                                    },
                                );
                                axum::response::Response::builder()
                                    .header("content-type", "text/event-stream")
                                    .body(axum::body::Body::from_stream(stream))
                                    .unwrap()
                            }
                        }
                    }),
                );
            let provider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let paths = formless_core::Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
            let mut config = formless_core::Config::default();
            config.provider.base_url = base;
            config.provider.api_style = "anthropic".into();
            config.provider.model = "v1-test".into();
            config.provider.max_retries = 0;
            config.provider.auto_test_models = false;
            config.roles.insert(
                "desktop-fixture".into(),
                serde_json::from_value(json!({"model":"v1-test"})).unwrap(),
            );
            config.naming.enabled = false;
            config.skills.auto_create = false;
            config.daemon.idle_grace_secs = 3600;
            config.save(&paths).unwrap();
            let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
            let session = daemon
                .handle(Request::CreateSession {
                    name: Some("Private v1 view".into()),
                })
                .await
                .unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned();
            let server = tokio::spawn({
                let daemon = daemon.clone();
                async move { daemon.serve().await.map_err(|e| e.to_string()) }
            });
            #[cfg(unix)]
            let endpoint = Endpoint(paths.socket_path());
            #[cfg(windows)]
            let endpoint = Endpoint(paths.pipe_name().into());
            (daemon, server, provider, endpoint, session)
        });
        Self {
            runtime,
            root,
            daemon,
            server,
            provider,
            endpoint,
            session,
            calls,
            models,
            catalog_calls,
            complete,
            extra,
        }
    }
    fn send(&self) -> String {
        let value = self
            .runtime
            .block_on(self.daemon.handle(Request::SendMessage {
                session_id: self.session.clone(),
                content: "Private v1 answer".into(),
                mode: SendMode::Send,
                client_msg_id: Some(uuid::Uuid::new_v4().to_string()),
            }))
            .unwrap();
        value["run_id"].as_str().expect("Run identity").to_owned()
    }
    fn snapshot(&self) -> Value {
        self.runtime
            .block_on(self.daemon.handle(Request::GetSession {
                id: self.session.clone(),
            }))
            .unwrap()
    }
    fn cursor(&self) -> u64 {
        self.runtime
            .block_on(self.daemon.handle(Request::GetTranscriptBarrier {
                id: self.session.clone(),
            }))
            .unwrap()["barrier"]["cursor"]
            .as_u64()
            .unwrap()
    }
    fn wait(&self, label: &str, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready() {
            assert!(Instant::now() < deadline, "{label}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn finish(self) {
        self.runtime.block_on(async {
            self.daemon.handle(Request::Shutdown).await.unwrap();
            tokio::time::timeout(Duration::from_secs(10), self.server)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            self.provider.abort();
            let _ = self.provider.await;
        });
        let weak = Arc::downgrade(&self.daemon);
        drop(self.daemon);
        self.runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(10), async {
                while weak.strong_count() != 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        });
        drop(self.runtime);
        std::fs::remove_dir_all(self.root).unwrap();
    }
}
fn pump(cx: &mut TestAppContext, handle: WindowHandle<Root>, shell: &Entity<Shell>) {
    step(cx, handle, |window, cx| {
        shell.update(cx, |s, cx| s.poll_client(window, cx));
        window.render_frame(cx);
    });
}
fn until(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    label: &str,
    mut ready: impl FnMut(&Shell, &App) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        pump(cx, handle, shell);
        if step(cx, handle, |_, cx| ready(shell.read(cx), cx)) {
            break;
        }
        if Instant::now() >= deadline {
            let status = step(cx, handle, |_, cx| {
                format!(
                    "status={}; composer={:?}; Session={:?}",
                    shell.read(cx).client_status_notice(),
                    shell.read(cx).composer_notice(),
                    shell.read(cx).sidebar().active_id()
                )
            });
            panic!("view deadline: {label}; {status}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[gpui_kit::test]
fn v1_real_daemon_to_view_waits_on_mid_answer_attach_and_reconnect_then_streams_next_attempt(
    cx: &mut TestAppContext,
) {
    for retry in [false, true] {
        exercise_v1_view(cx, retry);
    }
}
fn exercise_v1_view(cx: &mut TestAppContext, retry: bool) {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-v1-view-key");
    let f = Fixture::start();
    f.send();
    f.wait("first provider prefix", || {
        f.calls.load(Ordering::SeqCst) == 1
    });
    let (handle, shell) = open(cx, WIDE, 0, 0);
    step(cx, handle, |_, cx| {
        shell.update(cx, |s, cx| {
            s.connect_future_attempts_for_validation(f.endpoint.clone(), cx)
        })
    });
    until(cx, handle, &shell, "session list", |s, _| {
        s.sidebar().rows().iter().any(|row| row.id == f.session)
    });
    step(cx, handle, |window, cx| {
        window.click(session_row_id(&f.session), cx)
    });
    until(cx, handle, &shell, "mid-answer baseline", |s, _| {
        s.live_stream_owner().is_some() && s.block_count() == 1
    });
    step(cx, handle, |window, cx| {
        assert_eq!(
            window.find("stream-recovery-policy").value(),
            Some(FUTURE_ATTEMPT_LIMITATION)
        );
        assert!(!shell.read(cx).last_block_preview().contains("A prefix"));
        shell.update(cx, |s, cx| {
            s.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Unsent ไทย 😀", cx);
    });
    let composer = step(cx, handle, |_, cx| shell.read(cx).composer().clone());
    f.complete[0].notify_one();
    until(cx, handle, &shell, "persisted first answer", |s, _| {
        s.block_count() == 2 && s.last_block_preview() == "A prefix before attachment A final 😀"
    });
    f.wait("first idle", || f.snapshot()["running"] == false);
    let second_run = f.send();
    until(cx, handle, &shell, "eligible live prefix", |s, _| {
        s.block_count() == 4 && s.last_block_preview() == "B future prefix "
    });
    let old = step(cx, handle, |_, cx| {
        shell.read(cx).live_stream_owner().unwrap()
    });
    step(cx, handle, |_, cx| {
        shell.update(cx, |s, _| {
            s.reconnect_future_attempts_for_validation().unwrap()
        })
    });
    until(
        cx,
        handle,
        &shell,
        "reconnected persisted baseline",
        |s, _| {
            s.live_stream_owner()
                .is_some_and(|owner| owner.connection > old.connection)
                && s.block_count() == 3
        },
    );
    step(cx, handle, |_, cx| {
        assert!(!shell
            .read(cx)
            .last_block_preview()
            .contains("B future prefix"))
    });
    let cursor = f.cursor();
    f.extra.notify_one();
    f.wait("post-reconnect suffix emitted", || f.cursor() > cursor);
    // Negative visibility sample after actual producer receipt; semantic
    // ordering itself is covered by deterministic model/private IPC schedules.
    let deadline = Instant::now() + Duration::from_millis(200);
    while Instant::now() < deadline {
        pump(cx, handle, &shell);
        step(cx, handle, |_, cx| {
            assert_eq!(shell.read(cx).block_count(), 3)
        });
        std::thread::sleep(Duration::from_millis(5));
    }
    step(cx, handle, |_, cx| {
        assert_eq!(shell.read(cx).composer(), &composer);
        assert_eq!(composer.read(cx).value().as_ref(), "Unsent ไทย 😀");
    });
    step(cx, handle, |window, _cx| {
        assert_eq!(
            window.find("stream-recovery-policy").value(),
            Some(FUTURE_ATTEMPT_LIMITATION)
        )
    });
    if retry {
        f.runtime
            .block_on(f.daemon.handle(Request::RetryProviderRequest {
                session_id: f.session.clone(),
            }))
            .unwrap();
        until(cx, handle, &shell, "same-Run future retry", |s, _| {
            s.block_count() == 4 && s.last_block_preview() == "C next eligible prefix "
        });
        assert_eq!(f.snapshot()["latest_run"]["run_id"], second_run);
    } else {
        f.complete[1].notify_one();
        until(
            cx,
            handle,
            &shell,
            "persisted reconnected answer",
            |s, _| {
                s.block_count() == 4
                    && s.last_block_preview()
                        == "B future prefix B suffix after reconnect B final 漢字"
            },
        );
        f.wait("second idle", || f.snapshot()["running"] == false);
        let next_run = f.send();
        assert_ne!(next_run, second_run);
        until(cx, handle, &shell, "next eligible Run", |s, _| {
            s.block_count() == 6 && s.last_block_preview() == "C next eligible prefix "
        });
    }
    let messages = if retry { 4 } else { 6 };
    f.complete[2].notify_one();
    until(cx, handle, &shell, "final persisted answer", |s, _| {
        s.block_count() == messages
            && s.last_block_preview() == "C next eligible prefix C final ไทย"
    });
    f.wait("final idle", || f.snapshot()["running"] == false);
    assert_eq!(f.calls.load(Ordering::SeqCst), 3);
    step(cx, handle, |_, cx| {
        assert_eq!(shell.read(cx).composer(), &composer);
        assert_eq!(composer.read(cx).value().as_ref(), "Unsent ไทย 😀");
    });
    assert_eq!(f.snapshot()["messages"].as_array().unwrap().len(), messages);
    eprintln!("v1_provider_view_case {{\"route\":\"{}\",\"provider_calls\":3,\"messages\":{messages},\"reconnected\":true,\"draft_preserved\":true}}", if retry {"same_run_retry"} else {"completion_then_new_run"});
    step(cx, handle, |_, cx| {
        shell.update(cx, |s, cx| s.connect_recording_daemon(cx))
    });
    f.finish();
}

#[cfg(unix)]
#[gpui_kit::test]
fn v1_older_peer_uses_only_canonical_subscription_through_the_actual_view(cx: &mut TestAppContext) {
    use formless_protocol::{RequestEnvelope, ServerMessage};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let root = std::path::PathBuf::from("/tmp").join(format!(
        "fl-v1-old-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    ));
    std::fs::create_dir(&root).unwrap();
    let endpoint = Endpoint(root.join("ipc.sock"));
    let session = fixture::sessions(1).remove(0);
    let id = session.id.clone();
    let subscriptions = Arc::new(AtomicUsize::new(0));
    let seen = subscriptions.clone();
    let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
    let peer = {
        let _entered = runtime.enter();
        let listener = tokio::net::UnixListener::bind(&endpoint.0).unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            loop {
                let mut line = String::new();
                let count = tokio::select! {_=&mut stopped=>break,result=reader.read_line(&mut line)=>result.unwrap()};
                if count == 0 {
                    break;
                }
                let envelope: RequestEnvelope = serde_json::from_str(&line).unwrap();
                let value = match envelope.request {
                    Request::Ping => json!({"min_protocol":1,"max_protocol":1,"features":[]}),
                    Request::Subscribe { deltas: false, .. } => {
                        seen.fetch_add(1, Ordering::SeqCst);
                        json!({"subscribed":true})
                    }
                    Request::ListSessions => json!([session]),
                    Request::GetSession { id } => {
                        json!({"session":{"id":id},"running":true,"messages":[{"id":"old-message","session_id":id,"seq":1,"role":"assistant","content":"Legacy canonical answer","created_at":"now"}],"pending_events":[]})
                    }
                    other => panic!("unsupported or mutating request sent to old peer: {other:?}"),
                };
                let mut bytes = serde_json::to_vec(&ServerMessage::ok(envelope.id, value)).unwrap();
                bytes.push(b'\n');
                write.write_all(&bytes).await.unwrap();
            }
        })
    };
    let (handle, shell) = open(cx, WIDE, 0, 0);
    step(cx, handle, |_, cx| {
        shell.update(cx, |s, cx| {
            s.connect_future_attempts_for_validation(endpoint, cx)
        })
    });
    until(cx, handle, &shell, "old peer list", |s, _| {
        s.sidebar().rows().iter().any(|row| row.id == id)
    });
    step(cx, handle, |window, cx| {
        window.click(session_row_id(&id), cx)
    });
    until(cx, handle, &shell, "old peer canonical answer", |s, _| {
        s.block_count() == 1 && s.last_block_preview() == "Legacy canonical answer"
    });
    step(cx, handle, |window, cx| {
        assert!(shell.read(cx).live_stream_owner().is_none());
        assert_eq!(
            window.find("stream-recovery-policy").value(),
            Some(FUTURE_ATTEMPT_LIMITATION)
        );
        assert!(window
            .find("status-intent")
            .value()
            .unwrap()
            .contains("live token streaming unavailable"));
        shell.update(cx, |s, cx| s.connect_recording_daemon(cx));
    });
    assert!(subscriptions.load(Ordering::SeqCst) >= 1);
    let _ = stop.send(());
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(3), peer)
            .await
            .unwrap()
            .unwrap();
    });
    drop(runtime);
    std::fs::remove_file(root.join("ipc.sock")).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[gpui_kit::test]
fn native_step_mode_changes_reach_real_daemon_without_starting_work_and_preserve_live_text(
    cx: &mut TestAppContext,
) {
    use formless_desktop::step_mode::{Outcome, Status};
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-step-enable-key");
    for future in [false, true] {
        let f = Fixture::start();
        let (handle, shell) = open(cx, WIDE, 0, 0);
        step(cx, handle, |_, cx| {
            shell.update(cx, |s, cx| {
                if future {
                    s.connect_future_attempts_for_validation(f.endpoint.clone(), cx)
                } else {
                    s.connect_daemon(f.endpoint.clone(), cx)
                }
            })
        });
        until(cx, handle, &shell, "mode Session list", |s, _| {
            s.sidebar().rows().iter().any(|r| r.id == f.session)
        });
        step(cx, handle, |window, cx| {
            window.click(session_row_id(&f.session), cx)
        });
        until(cx, handle, &shell, "mode metadata", |s, _| {
            s.step_mode_enable_target().is_some()
        });
        if future {
            let _ = f.send();
            until(cx, handle, &shell, "mode live prefix", |s, _| {
                s.last_block_preview() == "A prefix before attachment "
                    && s.step_mode_enable_target().is_some()
            });
        }
        let (composer, owner) = step(cx, handle, |window, cx| {
            shell.update(cx, |s, cx| {
                s.focus_section(PageSection::Composer, window, cx)
            });
            window.input("Mode draft ไทย 😀", cx);
            let input = shell.read(cx).composer().clone();
            let owner = shell.read(cx).live_stream_owner();
            window.click("step-mode-enable", cx);
            (input, owner)
        });
        until(cx, handle, &shell, "mode enabled and sampled", |s, _| {
            s.sampled_step_mode() == Some(true)
                && matches!(
                    s.mode_enable_status(),
                    Status::Finished {
                        outcome: Outcome::Enabled,
                        ..
                    }
                )
        });
        assert_eq!(f.snapshot()["step_mode"], true);
        assert_eq!(
            f.calls.load(Ordering::SeqCst),
            usize::from(future),
            "Enable must not start provider work"
        );
        step(cx, handle, |_, cx| {
            assert_eq!(shell.read(cx).composer(), &composer);
            assert_eq!(composer.read(cx).value().as_ref(), "Mode draft ไทย 😀");
            if future {
                assert_eq!(shell.read(cx).live_stream_owner(), owner);
                assert_eq!(
                    shell.read(cx).last_block_preview(),
                    "A prefix before attachment "
                );
            }
        });
        if future {
            f.complete[0].notify_one();
            until(cx, handle, &shell, "mode canonical completion", |s, _| {
                s.block_count() == 2
                    && s.last_block_preview() == "A prefix before attachment A final 😀"
            });
            f.wait("mode Run idle", || f.snapshot()["running"] == false);
        }
        until(cx, handle, &shell, "idle mode disable control", |s, _| {
            s.step_mode_disable_target().is_some()
        });
        let calls_before_off = f.calls.load(Ordering::SeqCst);
        step(cx, handle, |window, cx| {
            window.click("step-mode-disable", cx);
        });
        until(cx, handle, &shell, "mode disabled and sampled", |s, _| {
            s.sampled_step_mode() == Some(false)
                && matches!(
                    s.mode_enable_status(),
                    Status::Finished {
                        outcome: Outcome::Disabled,
                        ..
                    }
                )
        });
        assert_eq!(f.snapshot()["step_mode"], false);
        assert_eq!(
            f.calls.load(Ordering::SeqCst),
            calls_before_off,
            "Disable must not start provider work"
        );
        step(cx, handle, |_, cx| {
            assert_eq!(shell.read(cx).composer(), &composer);
            assert_eq!(composer.read(cx).value().as_ref(), "Mode draft ไทย 😀");
        });
        step(cx, handle, |_, cx| {
            shell.update(cx, |s, cx| s.connect_recording_daemon(cx))
        });
        f.finish();
    }
}

#[gpui_kit::test]
fn native_session_model_role_effort_and_clear_follow_real_daemon_next_run_contract(
    cx: &mut TestAppContext,
) {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-session-model-key");
    let f = Fixture::start();
    let (h, shell) = open(cx, WIDE, 0, 0);
    step(cx, h, |_, cx| {
        shell.update(cx, |s, cx| s.connect_daemon(f.endpoint.clone(), cx))
    });
    until(cx, h, &shell, "model Session list", |s, _| {
        s.sidebar().rows().iter().any(|r| r.id == f.session)
    });
    step(cx, h, |window, cx| {
        window.click(session_row_id(&f.session), cx)
    });
    until(cx, h, &shell, "model canonical baseline", |s, _| {
        s.step_mode_enable_target().is_some()
    });
    let composer = step(cx, h, |window, cx| {
        shell.update(cx, |s, cx| {
            s.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Model draft ไทย 😀", cx);
        shell.read(cx).composer().clone()
    });
    let open_form = |cx: &mut TestAppContext| {
        step(cx, h, |window, cx| {
            assert!(shell
                .update(cx, |s, cx| s.run_command(
                    Command::SessionDetails,
                    window,
                    cx
                ))
                .is_none())
        });
        until(cx, h, &shell, "model metadata loaded", |s, _| {
            s.session_edit_input().is_some()
        });
    };
    let save = |cx: &mut TestAppContext, field: &'static str, value: &str| {
        step(cx, h, |window, cx| {
            window.click(field, cx);
            let i = shell.read(cx).session_edit_input().unwrap();
            i.update(cx, |i, cx| i.set_value(value, window, cx));
            assert!(window.find("session-model-paid-footer").visible());
            window.click("session-edit-save", cx);
        });
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            pump(cx, h, &shell);
            let done = step(cx, h, |window, _| {
                window.find("session-edit-notice").value().is_some_and(|n| {
                    n.starts_with("Saved and acknowledged")
                        || n.starts_with("The current model/effort pair already matches")
                })
            });
            if done {
                break;
            }
            assert!(Instant::now() < deadline, "model save receipt");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    open_form(cx);
    save(cx, "session-edit-model", "role:desktop-fixture");
    assert_eq!(f.snapshot()["session"]["model"], "role:desktop-fixture");
    assert_eq!(
        f.calls.load(Ordering::SeqCst),
        0,
        "auto-test disabled; saving must not start work"
    );
    step(cx, h, |window, cx| window.click("session-edit-cancel", cx));
    let _ = f.send();
    f.wait("role resolved to provider model", || {
        f.models.lock().unwrap().len() == 1
    });
    assert_eq!(f.models.lock().unwrap()[0], "v1-test");
    open_form(cx);
    save(cx, "session-edit-model", "v1-other-test");
    assert_eq!(f.snapshot()["session"]["model"], "v1-other-test");
    assert_eq!(
        f.calls.load(Ordering::SeqCst),
        1,
        "changing selection must not start or replace the active Run"
    );
    step(cx, h, |window, cx| window.click("session-edit-cancel", cx));
    f.complete[0].notify_one();
    until(cx, h, &shell, "old-model Run completes", |s, _| {
        s.last_block_preview() == "A prefix before attachment A final 😀"
    });
    f.wait("first model Run idle", || f.snapshot()["running"] == false);
    let _ = f.send();
    f.wait("next Run uses saved model", || {
        f.models.lock().unwrap().len() == 2
    });
    assert_eq!(f.models.lock().unwrap()[1], "v1-other-test");
    f.extra.notify_one();
    f.complete[1].notify_one();
    until(cx, h, &shell, "new-model Run completes", |s, _| {
        s.last_block_preview() == "B future prefix B suffix after reconnect B final 漢字"
    });
    f.wait("second model Run idle", || f.snapshot()["running"] == false);
    open_form(cx);
    save(cx, "session-edit-effort", "high");
    assert_eq!(f.snapshot()["session"]["reasoning_effort"], "high");
    assert_eq!(f.snapshot()["session"]["model"], "v1-other-test");
    save(cx, "session-edit-model", "");
    assert!(f.snapshot()["session"]["model"].is_null());
    assert_eq!(f.snapshot()["session"]["reasoning_effort"], "high");
    save(cx, "session-edit-effort", "");
    assert!(f.snapshot()["session"]["reasoning_effort"].is_null());
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    step(cx, h, |window, cx| {
        window.click("session-edit-cancel", cx);
        assert_eq!(shell.read(cx).composer(), &composer);
        assert_eq!(composer.read(cx).value().as_ref(), "Model draft ไทย 😀");
        shell.update(cx, |s, cx| s.connect_recording_daemon(cx));
    });
    f.finish();
}

#[gpui_kit::test]
fn native_model_catalog_read_fill_and_save_follow_real_daemon_without_starting_work(
    cx: &mut TestAppContext,
) {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-catalog-key");
    let f = Fixture::start();
    let (h, s) = open(cx, WIDE, 0, 0);
    step(cx, h, |_, cx| {
        s.update(cx, |s, cx| s.connect_daemon(f.endpoint.clone(), cx))
    });
    until(cx, h, &s, "catalog Session list", |s, _| {
        s.sidebar().rows().iter().any(|r| r.id == f.session)
    });
    step(cx, h, |w, cx| w.click(session_row_id(&f.session), cx));
    until(cx, h, &s, "catalog canonical baseline", |s, _| {
        s.step_mode_enable_target().is_some()
    });
    let before = f.catalog_calls.load(Ordering::SeqCst);
    step(cx, h, |w, cx| {
        s.update(cx, |s, cx| s.run_command(Command::SessionDetails, w, cx));
    });
    until(cx, h, &s, "catalog metadata", |s, _| {
        s.session_edit_input().is_some()
    });
    step(cx, h, |w, cx| {
        w.click("session-edit-model", cx);
        s.read(cx)
            .session_edit_input()
            .unwrap()
            .update(cx, |i, cx| i.set_value("review", w, cx));
    });
    assert_eq!(
        f.catalog_calls.load(Ordering::SeqCst),
        before,
        "opening and typing do not request a catalog"
    );
    step(cx, h, |w, cx| {
        super::model_catalog_tests::click(w, cx, "session-catalog-read")
    });
    until(cx, h, &s, "catalog response", |s, _| {
        s.session_catalog_revision().is_some()
    });
    assert_eq!(f.catalog_calls.load(Ordering::SeqCst), before + 1);
    step(cx, h, |w, cx| {
        let i = s.read(cx).session_edit_input().unwrap();
        i.update(cx, |i, cx| i.set_value("desktop-fixture", w, cx));
    });
    step(cx, h, |w, cx| {
        assert!(w
            .find("session-catalog-count")
            .value()
            .unwrap()
            .contains("1 matching choices"));
        super::model_catalog_tests::click(w, cx, "session-catalog-choice-0");
        assert_eq!(
            s.read(cx)
                .session_edit_input()
                .unwrap()
                .read(cx)
                .value()
                .as_ref(),
            "role:desktop-fixture"
        );
    });
    assert!(
        f.snapshot()["session"]["model"].is_null(),
        "a fill is not a save"
    );
    assert_eq!(
        f.calls.load(Ordering::SeqCst),
        0,
        "catalog and fill start neither Run nor automatic test"
    );
    step(cx, h, |w, cx| w.click("session-edit-save", cx));
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        pump(cx, h, &s);
        if step(cx, h, |w, _| {
            w.find("session-edit-notice")
                .value()
                .is_some_and(|v| v.starts_with("Saved and acknowledged"))
        }) {
            break;
        }
        assert!(Instant::now() < deadline, "catalog selection save");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(f.snapshot()["session"]["model"], "role:desktop-fixture");
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.catalog_calls.load(Ordering::SeqCst), before + 1);
    step(cx, h, |_, cx| {
        s.update(cx, |s, cx| s.connect_recording_daemon(cx))
    });
    f.finish();
}

#[gpui_kit::test]
fn native_pending_inventory_reads_shared_metadata_without_consuming_and_refreshes_after_send(
    cx: &mut TestAppContext,
) {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-pending-inventory-key");
    let f = Fixture::start();
    let held = f
        .runtime
        .block_on(f.daemon.handle(Request::AttachFile {
            session_id: f.session.clone(),
            name: "private pending.txt".into(),
            data_b64: "aGVsbG8=".into(),
            mime: None,
        }))
        .unwrap();
    let directory = f.root.join("pending-dir");
    std::fs::create_dir(&directory).unwrap();
    f.runtime
        .block_on(f.daemon.handle(Request::AttachPath {
            session_id: f.session.clone(),
            path: "pending-dir".into(),
            root: Some(f.root.to_str().unwrap().into()),
        }))
        .unwrap();
    let (h, s) = open(cx, WIDE, 0, 0);
    step(cx, h, |_, cx| {
        s.update(cx, |s, cx| s.connect_daemon(f.endpoint.clone(), cx))
    });
    until(cx, h, &s, "pending Session", |s, _| {
        s.sidebar().rows().iter().any(|r| r.id == f.session)
    });
    step(cx, h, |w, cx| w.click(session_row_id(&f.session), cx));
    until(cx, h, &s, "pending canonical baseline", |s, _| {
        s.step_mode_enable_target().is_some()
    });
    step(cx, h, |w, cx| {
        s.update(cx, |s, cx| s.run_command(Command::SessionDetails, w, cx));
    });
    until(cx, h, &s, "pending metadata form", |s, _| {
        s.session_edit_input().is_some()
    });
    let read = |cx: &mut TestAppContext, expected| {
        step(cx, h, |w, cx| {
            super::model_catalog_tests::click(w, cx, "pending-files-read")
        });
        until(cx, h, &s, "complete pending inventory", |s, _| {
            s.pending_files_count() == Some(expected)
        });
    };
    read(cx, 2);
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    let snapshot = f
        .runtime
        .block_on(f.daemon.handle(Request::ListPendingAttachments {
            session_id: f.session.clone(),
        }))
        .unwrap();
    assert_eq!(snapshot["attachments"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot["attachments"][0]["id"], held["id"]);
    f.runtime
        .block_on(f.daemon.handle(Request::DetachFile {
            session_id: f.session.clone(),
            id: held["id"].as_str().unwrap().into(),
        }))
        .unwrap();
    pump(cx, h, &s);
    assert_eq!(
        step(cx, h, |_, cx| s.read(cx).pending_files_count()),
        Some(2),
        "read remains an explicitly dated snapshot"
    );
    read(cx, 1);
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    let _ = f.send();
    f.wait("one explicit provider Run", || {
        f.calls.load(Ordering::SeqCst) == 1
    });
    read(cx, 0);
    let messages = f.snapshot()["messages"].as_array().unwrap().clone();
    assert!(messages.iter().any(|m| m["meta"]
        .as_str()
        .is_some_and(|meta| meta.contains("pending-dir"))));
    f.complete[0].notify_one();
    f.wait("pending fixture Run idle", || {
        f.snapshot()["running"] == false
    });
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    eprintln!(
        "pending_inventory_case {}",
        json!({"initial":2,"after_peer_detach":1,"after_explicit_send":0,"provider_posts":1,"reads_consume_nothing":true})
    );
    step(cx, h, |_, cx| {
        s.update(cx, |s, cx| s.connect_recording_daemon(cx))
    });
    f.finish();
}

#[test]
fn reviewed_attachment_send_idle_admission_uses_exact_metadata_and_only_one_fake_provider_run() {
    use formless_protocol::{RequestEnvelope, ServerMessage, PROTOCOL_VERSION};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-reviewed-send-key");
    let f = Fixture::start();
    f.runtime.block_on(async {
        let socket = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                #[cfg(unix)]
                let opened = tokio::net::UnixStream::connect(&f.endpoint.0).await;
                #[cfg(windows)]
                let opened = tokio::net::windows::named_pipe::ClientOptions::new().open(&f.endpoint.0);
                match opened {
                    Ok(socket) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(231) => tokio::time::sleep(Duration::from_millis(10)).await,
                    Err(error) => panic!("private reviewed IPC: {error}"),
                }
            }
        }).await.unwrap();
        let mut reader = BufReader::new(socket);
        let mut id = 0;
        macro_rules! rpc {
            ($request:expr) => {{
                id += 1;
                let mut bytes = serde_json::to_vec(&RequestEnvelope {id, protocol: PROTOCOL_VERSION, request: $request}).unwrap();
                bytes.push(b'\n');
                reader.get_mut().write_all(&bytes).await.unwrap();
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        let mut line = String::new();
                        assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
                        if let ServerMessage::Response {id: got, ok, result, error} = serde_json::from_str::<ServerMessage>(&line).unwrap() {
                            assert_eq!(got, id);
                            break if ok {Ok(result.unwrap())} else {Err(error.unwrap())};
                        }
                    }
                }).await.unwrap()
            }};
        }
        macro_rules! inventory { () => { rpc!(Request::ListPendingAttachments {session_id: f.session.clone()}).unwrap()["attachments"].as_array().unwrap().clone() }; }
        let empty = inventory!();
        assert!(empty.is_empty());
        // A peer changes shared pending state after the review read.
        f.daemon.handle(Request::AttachFile {session_id:f.session.clone(),name:"reviewed.txt".into(),data_b64:"aGVsbG8=".into(),mime:Some("text/plain".into())}).await.unwrap();
        let request = |key: &str, expected| Request::SendReviewedMessage {
            session_id: f.session.clone(), content: "Private reviewed answer".into(), mode: SendMode::Send,
            client_msg_id: key.into(), expected_attachments: expected,
        };
        assert!(rpc!(request("stale-empty", empty)).unwrap_err().contains("pending attachments changed"));
        assert_eq!(f.calls.load(Ordering::SeqCst), 0);
        let before = rpc!(Request::GetSession {id:f.session.clone()}).unwrap();
        assert!(before["messages"].as_array().unwrap().is_empty());
        let expected = inventory!();
        assert_eq!(expected.len(), 1);
        let ack = rpc!(request("explicit-reviewed-action", expected.clone())).unwrap();
        assert_eq!(ack["started"], true);
        tokio::time::timeout(Duration::from_secs(10), async {
            while f.calls.load(Ordering::SeqCst) != 1 {tokio::time::sleep(Duration::from_millis(5)).await;}
        }).await.unwrap();
        assert!(inventory!().is_empty());
        let snapshot = rpc!(Request::GetSession {id:f.session.clone()}).unwrap();
        let persisted = snapshot["messages"].as_array().unwrap().iter().find(|m| m["role"] == "user").unwrap();
        let metadata: Value = serde_json::from_str(persisted["meta"].as_str().unwrap()).unwrap();
        assert_eq!(metadata["attachments"], json!(expected));
        assert_eq!(persisted["content"], "Private reviewed answer");
        assert_eq!(rpc!(request("explicit-reviewed-action", vec![])).unwrap(), ack);
        assert!(rpc!(request("different-action-stale", expected)).is_err());
        f.complete[0].notify_one();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if rpc!(Request::GetSession {id:f.session.clone()}).unwrap()["running"] == false {break;}
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.unwrap();
        assert_eq!(f.calls.load(Ordering::SeqCst), 1);
        let snapshot = rpc!(Request::GetSession {id:f.session.clone()}).unwrap();
        assert_eq!(snapshot["messages"].as_array().unwrap().iter().filter(|m| m["role"] == "user").count(), 1);
        eprintln!("reviewed_attachment_send_case {}", json!({"real_ipc":true,"stale_empty_rejected":true,"consumed":1,"user_messages":1,"provider_posts":1,"replay_does_not_send":true}));
        drop(reader);
    });
    f.finish();
}

#[gpui_kit::test]
fn native_composer_confirms_pending_files_and_retains_unknown_after_a_peer_changes_them(
    cx: &mut TestAppContext,
) {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-native-review-key");
    for peer_change in [false, true] {
        let f = Fixture::start();
        f.runtime
            .block_on(f.daemon.handle(Request::AttachFile {
                session_id: f.session.clone(),
                name: "review me ไทย.txt".into(),
                data_b64: "aGVsbG8=".into(),
                mime: Some("text/plain".into()),
            }))
            .unwrap();
        let (h, s) = open(cx, WIDE, 0, 0);
        step(cx, h, |_, cx| {
            s.update(cx, |s, cx| s.connect_daemon(f.endpoint.clone(), cx))
        });
        until(cx, h, &s, "native review Session", |s, _| {
            s.sidebar().rows().iter().any(|r| r.id == f.session)
        });
        step(cx, h, |w, cx| w.click(session_row_id(&f.session), cx));
        until(cx, h, &s, "native review baseline", |s, _| {
            s.step_mode_enable_target().is_some()
        });
        step(cx, h, |w, cx| {
            s.update(cx, |s, cx| s.focus_section(PageSection::Composer, w, cx));
            w.input("native reviewed draft ไทย😀", cx);
            settle_frames(w, cx);
        });
        pump(cx, h, &s);
        step(cx, h, |w, cx| {
            assert_eq!(s.read(cx).draft(cx).as_ref(), "native reviewed draft ไทย😀");
            assert_ne!(w.find("composer-primary").disabled(), Some(true));
            let enter = s.update(cx, |s, cx| s.command_state(w, cx).enter_sends);
            w.press(
                if enter {
                    "enter"
                } else {
                    formless_desktop::commands::composer_submit_key()
                },
                cx,
            );
        });
        until(cx, h, &s, "mandatory native review", |s, _| {
            s.send_review_open()
        });
        assert_eq!(
            f.calls.load(Ordering::SeqCst),
            0,
            "opening review cannot start work"
        );
        step(cx, h, |w, cx| {
            assert_eq!(w.find("forecast-cancel").focused(), Some(true));
            assert!(s.read(cx).background_inert(w));
            w.click("forecast-copy", cx);
            let details: Value =
                serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap()).unwrap();
            assert_eq!(details["draft"], "native reviewed draft ไทย😀");
            assert_eq!(
                details["pending_files"]["attachments"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
        });
        if peer_change {
            f.runtime
                .block_on(f.daemon.handle(Request::AttachFile {
                    session_id: f.session.clone(),
                    name: "peer added.txt".into(),
                    data_b64: "aGVsbG8=".into(),
                    mime: None,
                }))
                .unwrap();
        }
        step(cx, h, |w, cx| {
            w.click("forecast-confirm", cx);
        });
        if peer_change {
            until(cx, h, &s, "unknown reviewed result", |s, _| {
                s.composer_notice()
                    .is_some_and(|n| n.contains("outcome is unknown"))
            });
            assert_eq!(f.calls.load(Ordering::SeqCst), 0);
            let pending = f
                .runtime
                .block_on(f.daemon.handle(Request::ListPendingAttachments {
                    session_id: f.session.clone(),
                }))
                .unwrap();
            assert_eq!(pending["attachments"].as_array().unwrap().len(), 2);
            assert!(f.snapshot()["messages"].as_array().unwrap().is_empty());
            step(cx, h, |w, cx| {
                settle_frames(w, cx);
                s.update(cx, |s, cx| s.focus_section(PageSection::Composer, w, cx));
                let enter = s.update(cx, |s, cx| s.command_state(w, cx).enter_sends);
                w.press(
                    if enter {
                        "enter"
                    } else {
                        formless_desktop::commands::composer_submit_key()
                    },
                    cx,
                );
            });
            until(cx, h, &s, "unknown retry refused", |s, _| {
                s.client_status_notice()
                    .starts_with("A previous action's outcome is unknown.")
            });
            pump(cx, h, &s);
            assert!(!step(cx, h, |_, cx| s.read(cx).send_review_open()));
            assert_eq!(f.calls.load(Ordering::SeqCst), 0);
        } else {
            until(cx, h, &s, "native send acknowledgement", |s, _| {
                s.composer_notice()
                    .is_some_and(|n| n.contains("acknowledged"))
            });
            f.wait("native reviewed provider request", || {
                f.calls.load(Ordering::SeqCst) == 1
            });
            let messages = f.snapshot()["messages"].as_array().unwrap().clone();
            let msg = messages.iter().find(|m| m["role"] == "user").unwrap();
            assert_eq!(msg["content"], "native reviewed draft ไทย😀");
            assert!(msg["meta"].as_str().unwrap().contains("review me"));
            assert_eq!(
                step(cx, h, |_, cx| s.read(cx).draft(cx).to_string()),
                "native reviewed draft ไทย😀"
            );
            until(cx, h, &s, "native Steer state", |s, _| {
                s.sidebar()
                    .active_row()
                    .is_some_and(|r| r.run_state == formless_desktop::model::RunState::Running)
            });
            step(cx, h, |w, cx| {
                let input = s.read(cx).composer().clone();
                input.update(cx, |input, cx| {
                    input.set_value("native steer draft ไทย😀", w, cx)
                });
                s.update(cx, |s, cx| s.focus_section(PageSection::Composer, w, cx));
            });
            pump(cx, h, &s);
            step(cx, h, |w, cx| {
                let enter = s.update(cx, |s, cx| s.command_state(w, cx).enter_sends);
                w.press(
                    if enter {
                        "enter"
                    } else {
                        formless_desktop::commands::composer_submit_key()
                    },
                    cx,
                );
            });
            until(cx, h, &s, "native empty Steer review", |s, _| {
                s.send_review_open()
            });
            assert_eq!(f.calls.load(Ordering::SeqCst), 1);
            step(cx, h, |w, cx| {
                w.click("forecast-copy", cx);
                let copied: Value =
                    serde_json::from_str(&cx.read_from_clipboard().unwrap().text().unwrap())
                        .unwrap();
                assert_eq!(copied["pending_files"]["attachments"], json!([]));
                assert_eq!(copied["draft"], "native steer draft ไทย😀");
                w.click("forecast-confirm", cx);
            });
            until(cx, h, &s, "native Steer acknowledgement", |s, _| {
                s.composer_notice()
                    .is_some_and(|n| n.contains("acknowledged"))
            });
            let messages = f.snapshot()["messages"].as_array().unwrap().clone();
            let steer = messages
                .iter()
                .find(|m| m["content"] == "native steer draft ไทย😀")
                .unwrap();
            let metadata: Value = serde_json::from_str(steer["meta"].as_str().unwrap()).unwrap();
            assert!(metadata["steer"]["run_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty()));
            assert!(metadata.get("attachments").is_none());
            f.complete[0].notify_one();
            f.wait("native Steer provider extension", || {
                f.calls.load(Ordering::SeqCst) == 2
            });
            f.extra.notify_one();
            f.complete[1].notify_one();
            f.wait("native reviewed idle", || f.snapshot()["running"] == false);
            assert_eq!(f.calls.load(Ordering::SeqCst), 2);
        }
        assert_eq!(
            step(cx, h, |_, cx| s.read(cx).draft(cx).to_string()),
            if peer_change {
                "native reviewed draft ไทย😀"
            } else {
                "native steer draft ไทย😀"
            }
        );
        eprintln!(
            "native_send_review_case {}",
            json!({"peer_changed":peer_change,"provider_posts":f.calls.load(Ordering::SeqCst),"draft_preserved":true,"confirmation_required":true,"automatic_replay":false})
        );
        step(cx, h, |_, cx| {
            s.update(cx, |s, cx| s.connect_recording_daemon(cx))
        });
        f.finish();
    }
}

#[test]
fn reviewed_reference_detach_over_real_ipc_removes_one_duplicate_without_touching_files_or_provider(
) {
    use formless_protocol::{RequestEnvelope, ServerMessage, PROTOCOL_VERSION};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-reference-detach-key");
    let f = Fixture::start();
    let directory = f.root.join("reference-dir");
    std::fs::create_dir(&directory).unwrap();
    let marker = directory.join("keep.txt");
    std::fs::write(&marker, b"keep reference target").unwrap();
    f.runtime.block_on(async {
        f.daemon.handle(Request::AttachFile{session_id:f.session.clone(),name:"keep stored.txt".into(),data_b64:"aGVsbG8=".into(),mime:Some("text/plain".into())}).await.unwrap();
        for _ in 0..2 {f.daemon.handle(Request::AttachPath{session_id:f.session.clone(),path:"reference-dir".into(),root:Some(f.root.to_str().unwrap().into())}).await.unwrap();}
        let socket=tokio::time::timeout(Duration::from_secs(10),async {loop {
            #[cfg(unix)]let opened=tokio::net::UnixStream::connect(&f.endpoint.0).await;
            #[cfg(windows)]let opened=tokio::net::windows::named_pipe::ClientOptions::new().open(&f.endpoint.0);
            match opened {Ok(socket)=>break socket,Err(error) if error.kind()==std::io::ErrorKind::NotFound || error.raw_os_error()==Some(231)=>tokio::time::sleep(Duration::from_millis(10)).await,Err(error)=>panic!("private reference IPC: {error}")}
        }}).await.unwrap();
        let mut reader=BufReader::new(socket);let mut id=0;
        macro_rules! rpc {($request:expr)=>{{
            id+=1;let mut frame=serde_json::to_vec(&RequestEnvelope{id,protocol:PROTOCOL_VERSION,request:$request}).unwrap();frame.push(b'\n');reader.get_mut().write_all(&frame).await.unwrap();
            tokio::time::timeout(Duration::from_secs(10),async{loop {let mut line=String::new();assert_ne!(reader.read_line(&mut line).await.unwrap(),0);if let ServerMessage::Response{id:got,ok,result,error}=serde_json::from_str::<ServerMessage>(&line).unwrap(){assert_eq!(got,id);break if ok{Ok(result.unwrap())}else{Err(error.unwrap())};}}}).await.unwrap()
        }};}
        let ping=rpc!(Request::Ping).unwrap();assert!(ping["features"].as_array().unwrap().iter().any(|v|v=="reviewed_reference_detach_v1"));
        let before=rpc!(Request::ListPendingAttachments{session_id:f.session.clone()}).unwrap()["attachments"].as_array().unwrap().clone();
        assert_eq!(before.len(),3);assert_eq!(before[1],before[2]);assert_eq!(before[1]["reference"],true);
        let stored=std::path::PathBuf::from(before[0]["path"].as_str().unwrap());let records=||formless_core::storage::Store::open_reader(&f.root.join("d/formless.db")).unwrap().list_artifacts().unwrap();let artifacts=records();
        let request=|op:&str,expected,index|Request::DetachReviewedReference{session_id:f.session.clone(),expected_attachments:expected,index,operation_id:op.into()};
        let ack=rpc!(request("selected-duplicate",before.clone(),2)).unwrap();
        assert_eq!(ack,json!({"removed":true,"session_id":f.session,"index":2,"operation_id":"selected-duplicate"}));
        assert_eq!(rpc!(request("selected-duplicate",before.clone(),2)).unwrap(),ack);
        let remaining=rpc!(Request::ListPendingAttachments{session_id:f.session.clone()}).unwrap()["attachments"].as_array().unwrap().clone();
        assert_eq!(remaining,vec![before[0].clone(),before[1].clone()]);
        assert!(rpc!(request("stale",before,1)).unwrap_err().contains("pending attachments changed"));
        assert!(rpc!(request("stored-not-supported",remaining.clone(),0)).is_err());
        assert_eq!(rpc!(Request::ListPendingAttachments{session_id:f.session.clone()}).unwrap()["attachments"],json!(remaining));
        assert_eq!(records(),artifacts);
        assert_eq!(std::fs::read(&stored).unwrap(),b"hello");assert_eq!(std::fs::read(&marker).unwrap(),b"keep reference target");
        let snapshot=rpc!(Request::GetSession{id:f.session.clone()}).unwrap();assert!(snapshot["messages"].as_array().unwrap().is_empty());assert_eq!(snapshot["running"],false);assert_eq!(f.calls.load(Ordering::SeqCst),0);
        eprintln!("reviewed_reference_detach_case {}",json!({"initial":3,"remaining":2,"selected_index":2,"replay_removes_no_second_row":true,"stale_rejected":true,"stored_rejected":true,"artifact_records_unchanged":true,"reference_target_unchanged":true,"stored_body_unchanged":true,"provider_posts":0}));
        drop(reader);
    });
    f.finish();
}
