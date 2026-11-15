//! Actual fake-provider -> daemon -> IPC -> Client -> TestPlatform Shell path.
//! No native renderer/window, real Profile, Tool or external provider executes.
use super::*;
use formless_desktop::client::{Endpoint, FUTURE_ATTEMPT_LIMITATION};
use formless_protocol::Request;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::Notify;
#[path = "../../../formless-core/tests/support/env.rs"]
mod credential_env;

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
                    axum::routing::get(|| async { axum::Json(json!({"data":[{"id":"v1-test"}]})) }),
                )
                .route(
                    "/messages",
                    axum::routing::post({
                        let complete = complete.clone();
                        let extra = extra.clone();
                        let calls = calls.clone();
                        move || {
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
        assert!(Instant::now() < deadline, "view deadline: {label}");
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
fn native_step_mode_enable_reaches_real_daemon_without_starting_work_and_preserves_live_text(
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
        step(cx, handle, |_, cx| {
            shell.update(cx, |s, cx| s.connect_recording_daemon(cx))
        });
        f.finish();
    }
}
