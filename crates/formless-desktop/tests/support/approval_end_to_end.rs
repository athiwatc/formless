//! Real private daemon/Tool/fake provider into the TestPlatform approval modal.
use super::*;
use axum::response::IntoResponse;
use formless_desktop::approvals::{Outcome, Status};
use formless_desktop::client::Endpoint;
use formless_protocol::{Request, SendMode};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
#[path = "../../../formless-core/tests/support/env.rs"]
mod credential_env;
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
}
fn response(n: usize) -> axum::response::Response {
    let value = if n % 2 == 0 {
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
        let (daemon,server,provider,endpoint,session,tool)=runtime.block_on(async{
            let app=axum::Router::new().route("/models",axum::routing::get(||async{axum::Json(json!({"data":[{"id":"approval-private"}]}))})).route("/chat/completions",axum::routing::post({let calls=calls.clone();move||{let calls=calls.clone();async move{let n=calls.fetch_add(1,Ordering::SeqCst);assert!(n<4,"unexpected extra provider work");response(n)}}}));
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
        }
    }
    fn send(&self) {
        self.runtime
            .block_on(self.daemon.handle(Request::SendMessage {
                session_id: self.session.clone(),
                content: "Private fixture request".into(),
                mode: SendMode::Send,
                client_msg_id: Some(uuid::Uuid::new_v4().to_string()),
            }))
            .unwrap();
    }
    fn snapshot(&self) -> Value {
        self.runtime
            .block_on(self.daemon.handle(Request::GetSession {
                id: self.session.clone(),
            }))
            .unwrap()
    }
    fn pending(&self) -> Value {
        self.runtime
            .block_on(self.daemon.handle(Request::PendingApprovals))
            .unwrap()
    }
    fn wait(&self, label: &str, mut test: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !test() {
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
    mut test: impl FnMut(&Shell) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        pump(cx, handle, shell);
        if step(cx, handle, |_, cx| test(shell.read(cx))) {
            break;
        }
        assert!(Instant::now() < deadline, "{label}");
        std::thread::sleep(Duration::from_millis(5));
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
        window.click(session_row_id(&f.session), cx)
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
        f.send();
        f.wait("one real approval", || {
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
        f.wait("Run idle", || f.snapshot()["running"] == false);
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
