//! Real private daemon/Tool/fake provider into the TestPlatform approval modal.
use super::*;
use axum::response::IntoResponse;
use formless_desktop::approvals::{Action, Inventory, Outcome, Status};
use formless_desktop::client::Endpoint;
use formless_protocol::{Request, SendMode};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
struct Fixture {
    runtime: tokio::runtime::Runtime,
    root: std::path::PathBuf,
    marker: std::path::PathBuf,
    session: String,
    tool: String,
    endpoint: Endpoint,
    daemon: Arc<formless_core::daemon::Daemon>,
    server: tokio::task::JoinHandle<Result<(), String>>,
    provider: tokio::task::JoinHandle<()>,
    calls: Arc<AtomicUsize>,
    tool_response: Arc<tokio::sync::Semaphore>,
}
fn response(n: usize, tool_call: bool) -> axum::response::Response {
    let value = if tool_call {
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":format!("private-call-{n}"),"type":"function","function":{"name":"native_approval_fixture","arguments":"{}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":1,"completion_tokens":1}})
    } else {
        json!({"choices":[{"delta":{"content":format!("Private flow {} complete",n/2+1)},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}})
    };
    axum::response::Sse::new(futures_util::stream::iter([Ok::<
        _,
        std::convert::Infallible,
    >(
        axum::response::sse::Event::default().data(value.to_string()),
    )]))
    .into_response()
}
impl Fixture {
    fn start() -> Self {
        Self::start_mode(false)
    }
    fn start_mode(multiple: bool) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        #[cfg(unix)]
        let temp = std::path::PathBuf::from("/tmp");
        #[cfg(windows)]
        let temp = std::env::temp_dir();
        let root = temp.join(format!("fl-apv-{}", &uuid::Uuid::new_v4().to_string()[..8]));
        std::fs::create_dir(&root).unwrap();
        let marker = root.join("approved-marker.txt");
        let calls = Arc::new(AtomicUsize::new(0));
        let tool_response = Arc::new(tokio::sync::Semaphore::new(0));
        let (daemon,server,provider,endpoint,session,tool)=runtime.block_on(async{
            let app=axum::Router::new().route("/models",axum::routing::get(||async{axum::Json(json!({"data":[{"id":"approval-private"}]}))})).route("/chat/completions",axum::routing::post({let calls=calls.clone();let tool_response=tool_response.clone();move|axum::Json(body):axum::Json<Value>|{let calls=calls.clone();let tool_response=tool_response.clone();async move{let n=calls.fetch_add(1,Ordering::SeqCst);assert!(n<4,"unexpected extra provider work");let tool_call=if multiple{!body["messages"].as_array().unwrap().iter().any(|m|m["role"]=="tool")}else{n.is_multiple_of(2)};if tool_call { tool_response.acquire().await.unwrap().forget(); } response(n,tool_call)}}}));
            let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let base=format!("http://{}",listener.local_addr().unwrap());let provider=tokio::spawn(async move{axum::serve(listener,app).await.unwrap()});
            let paths=formless_core::Paths::from_dirs(root.join("d"),root.join("c")).unwrap();let mut config=formless_core::Config::default();config.provider.base_url=base;config.provider.api_style="openai".into();config.provider.model="approval-private".into();config.provider.max_retries=0;config.provider.auto_test_models=false;config.provider.model_context_tokens.insert("approval-private".into(),100_000);config.security.secure=true;config.naming.enabled=false;config.skills.auto_create=false;config.tools.approval_summaries=false;config.daemon.idle_grace_secs=3600;config.save(&paths).unwrap();
            let store=formless_core::Store::open(&paths.db_file()).unwrap();let project=root.join("project");std::fs::create_dir(&project).unwrap();let session=store.create_session(Some("Native approval private flow".into())).unwrap();store.set_session_cwd(&session.id,project.to_str()).unwrap();
            let caps=serde_json::from_value(json!({"net":null,"fs_read":[],"fs_write":[marker.to_str().unwrap()],"env":[],"process":null,"clipboard_read":false,"clipboard_write":false,"notify":false,"llm":null,"agent":null})).unwrap();
            let code=format!("function run(){{fs.write({},'native approved');return 'private approved result';}}",json!(marker.to_str().unwrap()));
            let (tool,_)=store.create_tool_version("native_approval_fixture","Write only a private fixture marker","{\"type\":\"object\"}","{}",&code,&caps,64,"user").unwrap();drop(store);
            let daemon=Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());let server=tokio::spawn({let daemon=daemon.clone();async move{daemon.serve().await.map_err(|e|e.to_string())}});
            #[cfg(unix)]let endpoint=Endpoint(paths.socket_path());
            #[cfg(windows)]let endpoint=Endpoint(paths.pipe_name().into());
            (daemon,server,provider,endpoint,session.id,tool.id)
        });
        Self {
            runtime,
            root,
            marker,
            session,
            tool,
            endpoint,
            daemon,
            server,
            provider,
            calls,
            tool_response,
        }
    }
    fn send(&self, session: &str) -> tokio::task::JoinHandle<Value> {
        let daemon = self.daemon.clone();
        let session_id = session.to_owned();
        self.runtime.spawn(async move {
            daemon
                .handle(Request::SendMessage {
                    session_id,
                    content: "Private fixture request".into(),
                    mode: SendMode::Send,
                    client_msg_id: Some(uuid::Uuid::new_v4().to_string()),
                })
                .await
                .unwrap()
        })
    }
    fn snapshot(&self) -> Value {
        self.runtime
            .block_on(self.daemon.handle(Request::GetSession {
                id: self.session.clone(),
            }))
            .unwrap()
    }
    fn sent_run(
        &self,
        cx: &mut TestAppContext,
        h: WindowHandle<Root>,
        s: &Entity<Shell>,
        sending: tokio::task::JoinHandle<Value>,
    ) -> String {
        until(cx, h, s, "Send acknowledged", |_| sending.is_finished());
        self.runtime.block_on(sending).unwrap()["run_id"]
            .as_str()
            .expect("direct Send starts one Run")
            .to_owned()
    }
    fn pending(&self) -> Value {
        self.runtime
            .block_on(self.daemon.handle(Request::PendingApprovals))
            .unwrap()
    }
    fn wait(
        &self,
        cx: &mut TestAppContext,
        h: WindowHandle<Root>,
        s: &Entity<Shell>,
        label: &str,
        mut test: impl FnMut() -> bool,
    ) {
        // The first real Tool run includes cold sandbox startup on CI runners.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            pump(cx, h, s);
            if test() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{label}; provider calls={}; subscribers={}; desktop={}; session={}; pending={}",
                self.calls.load(Ordering::SeqCst),
                self.daemon.subscriber_count(),
                step(cx, h, |_, cx| s.read(cx).client_status_notice().to_owned()),
                self.snapshot(),
                self.pending()
            );
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
    test: impl FnMut(&Shell) -> bool,
) {
    until_with_failure_category(cx, handle, shell, label, test, |_| None);
}
fn until_with_approval_diagnostic(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    label: &str,
    selected_approval_id: &str,
    test: impl FnMut(&Shell) -> bool,
) {
    until_with_failure_category(cx, handle, shell, label, test, |s| {
        Some(approval_status_category(
            &s.approval_review_status(),
            selected_approval_id,
        ))
    });
}
fn until_with_failure_category(
    cx: &mut TestAppContext,
    handle: WindowHandle<Root>,
    shell: &Entity<Shell>,
    label: &str,
    mut test: impl FnMut(&Shell) -> bool,
    mut failure_category: impl FnMut(&Shell) -> Option<String>,
) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        pump(cx, handle, shell);
        if step(cx, handle, |_, cx| test(shell.read(cx))) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{}",
            step(cx, handle, |_, cx| match failure_category(shell.read(cx)) {
                Some(category) => format!("{label}; approval status category: {category}"),
                None => label.to_owned(),
            })
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn approval_status_category(status: &Status, selected_approval_id: &str) -> String {
    let action_category = |action: &Action| {
        let selection = if action.prompt.id() == selected_approval_id {
            "selected"
        } else {
            "other"
        };
        let decision = if action.allow { "allow" } else { "deny" };
        format!("{selection}-{decision}")
    };
    match status {
        Status::Idle => "idle".into(),
        Status::Reserved(action) => format!("reserved-{}", action_category(action)),
        Status::Sent(action) => format!("sent-{}", action_category(action)),
        Status::Finished { action, outcome } => {
            let outcome = match outcome {
                Outcome::Accepted {
                    audit_recorded: true,
                } => "accepted-audited",
                Outcome::Accepted {
                    audit_recorded: false,
                } => "accepted-audit-not-recorded",
                Outcome::Rejected(_) => "rejected",
                Outcome::Unknown => "unknown",
            };
            format!("finished-{}-{outcome}", action_category(action))
        }
    }
}
#[test]
fn approval_status_diagnostic_exposes_only_state_categories() {
    let inventory = Inventory::from_list(&formless_desktop::approvals::fixture_multiple()).unwrap();
    let selected_prompt = inventory.prompts()[0].clone();
    let other_prompt = inventory.prompts()[1].clone();
    let selected_id = selected_prompt.id().to_owned();
    let other_id = other_prompt.id().to_owned();
    let action = |prompt, allow| Action {
        client: uuid::Uuid::new_v4(),
        connection: 1,
        epoch: 1,
        operation: uuid::Uuid::new_v4(),
        prompt,
        allow,
    };
    let selected_allow = action(selected_prompt.clone(), true);
    let selected_deny = action(selected_prompt, false);
    let other_allow = action(other_prompt.clone(), true);
    let other_deny = action(other_prompt, false);
    let statuses = [
        (Status::Idle, "idle"),
        (
            Status::Reserved(selected_allow.clone()),
            "reserved-selected-allow",
        ),
        (Status::Sent(other_deny.clone()), "sent-other-deny"),
        (
            Status::Finished {
                action: selected_allow.clone(),
                outcome: Outcome::Accepted {
                    audit_recorded: true,
                },
            },
            "finished-selected-allow-accepted-audited",
        ),
        (
            Status::Finished {
                action: selected_deny.clone(),
                outcome: Outcome::Accepted {
                    audit_recorded: false,
                },
            },
            "finished-selected-deny-accepted-audit-not-recorded",
        ),
        (
            Status::Finished {
                action: selected_allow,
                outcome: Outcome::Rejected("private rejection detail".into()),
            },
            "finished-selected-allow-rejected",
        ),
        (
            Status::Finished {
                action: other_deny,
                outcome: Outcome::Unknown,
            },
            "finished-other-deny-unknown",
        ),
        (
            Status::Finished {
                action: other_allow,
                outcome: Outcome::Accepted {
                    audit_recorded: true,
                },
            },
            "finished-other-allow-accepted-audited",
        ),
    ];
    for (status, expected) in statuses {
        let category = approval_status_category(&status, &selected_id);
        assert_eq!(category, expected);
        assert!(!category.contains(selected_id.as_str()));
        assert!(!category.contains(other_id.as_str()));
        assert!(!category.contains("private rejection detail"));
    }
}
#[gpui_kit::test]
fn native_security_approval_once_then_deny_reaches_real_daemon_without_persisting_grants(
    cx: &mut TestAppContext,
) {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-native-approval-fixture");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let f = Fixture::start();
    let (handle, shell) = open(cx, WIDE, 0, 0);
    step(cx, handle, |_, cx| {
        shell.update(cx, |s, cx| s.connect_daemon(f.endpoint.clone(), cx))
    });
    until(cx, handle, &shell, "Session list", |s| {
        s.sidebar().rows().iter().any(|r| r.id == f.session)
    });
    step(cx, handle, |window, cx| {
        open_saved_session(window, cx, &shell, &f.session)
    });
    until(cx, handle, &shell, "loaded Session", |s| {
        s.step_mode_enable_target().is_some()
    });
    let composer = step(cx, handle, |window, cx| {
        shell.update(cx, |s, cx| {
            s.focus_section(PageSection::Composer, window, cx)
        });
        window.input("Unsent approval draft ไทย 😀", cx);
        shell.read(cx).composer().clone()
    });
    let mut first_id = None;
    for (index, allow) in [true, false].into_iter().enumerate() {
        assert!(!f.marker.exists());
        let run_id = f.sent_run(cx, handle, &shell, f.send(&f.session));
        // A fake model must not emit a whole Tool exchange while this fixture
        // blocks the desktop's bounded update consumer. Observe the new Run
        // and a live desktop subscriber before releasing its Tool response.
        until(
            cx,
            handle,
            &shell,
            "new Run observed before Tool response",
            |s| {
                s.sampled_steerable_run_id() == Some(run_id.as_str())
                    && f.calls.load(Ordering::SeqCst) == index * 2 + 1
                    && s.step_mode_enable_target()
                        .is_some_and(|a| a.session_id == f.session)
                    && s.sidebar()
                        .active_row()
                        .is_some_and(|r| r.run_state == formless_desktop::model::RunState::Running)
                    && f.daemon.subscriber_count() > 0
            },
        );
        f.tool_response.add_permits(1);
        f.wait(cx, handle, &shell, "one real approval", || {
            f.pending().as_array().is_some_and(|a| a.len() == 1)
        });
        step(cx, handle, |window, cx| {
            assert!(shell
                .update(cx, |s, cx| s.run_command(Command::Approvals, window, cx))
                .is_none());
        });
        // The daemon inventory can become visible before the Client receives
        // its lifecycle event. That event deliberately retires an overlapping
        // read. Model a user explicitly reloading, never a decision retry.
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut reloads = 0;
        loop {
            pump(cx, handle, &shell);
            if step(cx, handle, |_, cx| {
                shell.read(cx).approval_target().is_some()
            }) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "native approval loaded; reloads={reloads}; {}",
                step(cx, handle, |_, cx| shell
                    .read(cx)
                    .approval_review_diagnostic())
            );
            step(cx, handle, |window, cx| {
                if window.find("approval-reload").disabled() != Some(true) {
                    reloads += 1;
                    assert!(reloads <= 8, "approval read did not stabilize");
                    window.click("approval-reload", cx);
                }
            });
            std::thread::sleep(Duration::from_millis(5));
        }
        eprintln!("native approval review {index}: explicit read reloads={reloads}");
        step(cx, handle, |window, cx| {
            let (_, prompt) = shell.read(cx).approval_target().unwrap();
            assert_eq!(prompt.session(), f.session);
            if let Some(old) = &first_id {
                assert_ne!(prompt.id(), old);
            } else {
                first_id = Some(prompt.id().to_string());
            }
            assert_eq!(shell.read(cx).composer(), &composer);
            assert_eq!(
                composer.read(cx).value().as_ref(),
                "Unsent approval draft ไทย 😀"
            );
            window.click(
                if allow {
                    "approval-allow-once"
                } else {
                    "approval-deny"
                },
                cx,
            );
        });
        until(
            cx,
            handle,
            &shell,
            "real decision receipt",
            |s| matches!(s.approval_review_status(),Status::Finished{action,outcome:Outcome::Accepted{audit_recorded:true}}if action.allow==allow),
        );
        let expected = format!("Private flow {} complete", index + 1);
        until(cx, handle, &shell, "canonical completion", |s| {
            s.last_block_preview() == expected
        });
        f.wait(cx, handle, &shell, "Run idle", || {
            f.snapshot()["running"] == false
        });
        if allow {
            assert_eq!(
                std::fs::read_to_string(&f.marker).unwrap(),
                "native approved"
            );
            std::fs::remove_file(&f.marker).unwrap();
        } else {
            assert!(!f.marker.exists(), "denied Tool must not write");
        }
        let grants = f
            .runtime
            .block_on(f.daemon.handle(Request::ListApprovals))
            .unwrap();
        assert!(
            !grants
                .as_array()
                .unwrap()
                .iter()
                .any(|g| g["tool_id"].as_str() == Some(&f.tool)),
            "Once must not persist a Tool grant"
        );
        assert_eq!(f.calls.load(Ordering::SeqCst), (index + 1) * 2);
        step(cx, handle, |window, cx| {
            window.click("approval-review-close", cx);
            assert_eq!(
                composer.read(cx).value().as_ref(),
                "Unsent approval draft ไทย 😀"
            );
        });
    }
    step(cx, handle, |_, cx| {
        shell.update(cx, |s, cx| s.connect_recording_daemon(cx))
    });
    f.finish();
}

fn activate(window: &mut Window, cx: &mut App, id: &'static str) {
    for _ in 0..40 {
        settle_frames(window, cx);
        if window.find(id).focused() == Some(true) {
            assert!(window.find(id).visible());
            window.press("space", cx);
            return;
        }
        window.press("tab", cx);
    }
    panic!("approval control not reachable: {id}");
}
fn load_current(cx: &mut TestAppContext, h: WindowHandle<Root>, s: &Entity<Shell>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut reloads = 0;
    loop {
        pump(cx, h, s);
        let ready = step(cx, h, |w, cx| {
            s.read(cx).approval_target().is_some()
                && w.find("approval-allow-once").disabled() != Some(true)
        });
        if ready {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "multi approval inventory did not stabilize"
        );
        step(cx, h, |w, cx| {
            if w.find("approval-reload").disabled() != Some(true) {
                reloads += 1;
                assert!(reloads <= 8);
                activate(w, cx, "approval-reload");
            }
        });
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[gpui_kit::test]
fn native_multiple_security_approvals_decide_only_the_selected_session_and_never_grant_the_other(
    cx: &mut TestAppContext,
) {
    let env = credential_env::EnvGuard::isolated();
    env.set("FORMLESS_API_KEY", "private-multiple-approval-fixture");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let f = Fixture::start_mode(true);
    let other = f
        .runtime
        .block_on(f.daemon.handle(Request::CreateSession {
            name: Some("Second private approval Session".into()),
        }))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let snapshot = |id: &str| {
        f.runtime
            .block_on(f.daemon.handle(Request::GetSession { id: id.into() }))
            .unwrap()
    };
    let (h, s) = open(cx, WIDE, 0, 0);
    step(cx, h, |_, cx| {
        s.update(cx, |s, cx| s.connect_daemon(f.endpoint.clone(), cx))
    });
    until(cx, h, &s, "two approval Sessions", |s| {
        s.sidebar().rows().iter().any(|r| r.id == f.session)
            && s.sidebar().rows().iter().any(|r| r.id == other)
    });
    step(cx, h, |w, cx| open_saved_session(w, cx, &s, &f.session));
    until(cx, h, &s, "approval current Session", |s| {
        s.step_mode_enable_target().is_some()
    });
    let composer = step(cx, h, |w, cx| {
        s.update(cx, |s, cx| s.focus_section(PageSection::Composer, w, cx));
        w.input("Unsent multi approval draft ไทย😀", cx);
        s.read(cx).composer().clone()
    });
    let first = f.send(&f.session);
    let second = f.send(&other);
    let selected_run = f.sent_run(cx, h, &s, first);
    let other_run = f.sent_run(cx, h, &s, second);
    assert_ne!(selected_run, other_run);
    until(cx, h, &s, "both Runs started before Tool responses", |s| {
        s.sampled_steerable_run_id() == Some(selected_run.as_str())
            && f.calls.load(Ordering::SeqCst) == 2
            && s.step_mode_enable_target()
                .is_some_and(|a| a.session_id == f.session)
            && s.sidebar()
                .active_row()
                .is_some_and(|r| r.run_state == formless_desktop::model::RunState::Running)
            && f.daemon.subscriber_count() > 0
    });
    f.tool_response.add_permits(2);
    f.wait(cx, h, &s, "two simultaneous approvals", || {
        f.pending().as_array().unwrap().len() == 2
    });
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    assert!(!f.marker.exists());
    step(cx, h, |w, cx| {
        s.update(cx, |s, cx| s.run_command(Command::Approvals, w, cx));
    });
    load_current(cx, h, &s);
    let chosen = step(cx, h, |w, cx| {
        assert!(w
            .find("approval-selection")
            .value()
            .unwrap()
            .contains("Request 1 of 2"));
        activate(w, cx, "approval-next");
        let (_, prompt) = s.read(cx).approval_target().unwrap();
        assert!(w
            .find("approval-selection")
            .value()
            .unwrap()
            .contains("Request 2 of 2"));
        assert_eq!(w.find("approval-review-close").focused(), Some(true));
        assert_eq!(
            composer.read(cx).value().as_ref(),
            "Unsent multi approval draft ไทย😀"
        );
        let chosen = (prompt.id().to_string(), prompt.session().to_string());
        activate(w, cx, "approval-allow-once");
        chosen
    });
    until_with_approval_diagnostic(
        cx,
        h,
        &s,
        "selected allow receipt",
        &chosen.0,
        |s| matches!(s.approval_review_status(),Status::Finished{action,outcome:Outcome::Accepted{audit_recorded:true}}if action.allow&&action.prompt.id()==chosen.0),
    );
    f.wait(cx, h, &s, "selected Run completed only", || {
        snapshot(&chosen.1)["running"] == false && f.pending().as_array().unwrap().len() == 1
    });
    assert_eq!(f.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        std::fs::read_to_string(&f.marker).unwrap(),
        "native approved"
    );
    std::fs::remove_file(&f.marker).unwrap();
    let remaining = f.pending()[0]["session_id"].as_str().unwrap().to_string();
    assert_ne!(remaining, chosen.1);
    assert_eq!(snapshot(&remaining)["running"], true);
    step(cx, h, |w, cx| activate(w, cx, "approval-reload"));
    load_current(cx, h, &s);
    step(cx, h, |w, cx| {
        let (_, prompt) = s.read(cx).approval_target().unwrap();
        assert_eq!(prompt.session(), remaining);
        assert_ne!(prompt.id(), chosen.0);
        assert!(!f.marker.exists());
        activate(w, cx, "approval-deny");
    });
    until(
        cx,
        h,
        &s,
        "other deny receipt",
        |s| matches!(s.approval_review_status(),Status::Finished{action,outcome:Outcome::Accepted{audit_recorded:true}}if !action.allow&&action.prompt.session()==remaining),
    );
    f.wait(cx, h, &s, "both explicit Runs idle", || {
        snapshot(&f.session)["running"] == false
            && snapshot(&other)["running"] == false
            && f.pending().as_array().unwrap().is_empty()
    });
    assert_eq!(f.calls.load(Ordering::SeqCst), 4);
    assert!(!f.marker.exists(), "denied other Tool must not write");
    let grants = f
        .runtime
        .block_on(f.daemon.handle(Request::ListApprovals))
        .unwrap();
    assert!(!grants
        .as_array()
        .unwrap()
        .iter()
        .any(|g| g["tool_id"].as_str() == Some(&f.tool)));
    eprintln!(
        "private_multiple_approval_case {}",
        json!({"initial_pending":2,"selected_approval":chosen.0,"selected_session":chosen.1,"remaining_session":remaining,"provider_calls":f.calls.load(Ordering::SeqCst),"final_pending":f.pending().as_array().unwrap().len(),"denied_marker_absent":!f.marker.exists(),"persistent_grant":false})
    );
    step(cx, h, |w, cx| {
        assert_eq!(s.read(cx).composer(), &composer);
        assert_eq!(
            composer.read(cx).value().as_ref(),
            "Unsent multi approval draft ไทย😀"
        );
        w.click("approval-review-close", cx);
        s.update(cx, |s, cx| s.connect_recording_daemon(cx));
    });
    f.finish();
}
