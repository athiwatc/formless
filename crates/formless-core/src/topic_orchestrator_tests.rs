//! Fixture tests: private fake official CLI executables and loopback model provider.
use super::*;
use crate::local_harness::HarnessUpdate;
use axum::response::IntoResponse;
use std::os::unix::fs::PermissionsExt;
use std::{path::PathBuf, time::Duration};

fn script(f: &Fixture, env: &crate::credential_env::EnvGuard) -> PathBuf {
    let path = f.root.join("fake-harness");
    std::fs::write(&path,r#"#!/usr/bin/env python3
import sys,os,json,socket,pathlib,subprocess,hashlib,time
mode=os.environ.get('TOPIC_FIXTURE_MODE','git')
if mode=='hold':
    pathlib.Path(os.environ['TOPIC_FIXTURE_PID']).write_text(str(os.getpid()))
    subprocess.Popen(['sleep','120'])
    time.sleep(120)
def rpc(op):
    s=socket.socket(socket.AF_UNIX);s.connect(os.environ['TOPIC_FIXTURE_SOCKET'])
    request={'id':17,'protocol':PROTOCOL_VERSION,'request':{'type':'topic_mcp','request':{'attempt_id':os.environ['FORMLESS_TOPIC_ATTEMPT_ID'],'capability_token':os.environ['FORMLESS_TOPIC_CAPABILITY'],'operation':op}}}
    s.sendall((json.dumps(request)+'\n').encode());reader=s.makefile()
    while True:
        response=json.loads(reader.readline())
        if response.get('kind')=='response' and response['id']==17:
            if not response['ok']:raise RuntimeError(response.get('error'))
            return response.get('result')
if mode=='permission':
    answer=rpc({'operation':'request_permission','title':'Fixture write','kind':'write','options':[{'option_id':'allow','name':'Allow once','kind':'allow_once'},{'option_id':'deny','name':'Deny','kind':'reject_once'}]})
    if answer!='allow':sys.exit(9)
if mode=='scratch':
    prompt=sys.argv[-1];inputs=json.loads(prompt.split('Inputs: ',1)[1].split('\n',1)[0])
    if inputs:
        assert inputs[0]['kind']=='file'
        source=pathlib.Path(inputs[0]['path']);assert source.read_text()=='1,2\n'
        assert source.stat().st_mode & 0o222 == 0
        assert inputs[1]['content']=='ledger input'
        outputs=[{'kind':'text','title':'Total','content':'total=3'}]
    else:
        target=pathlib.Path(os.environ['FORMLESS_TOPIC_OUTPUTS_DIR'])/'ledger.csv'
        target.write_text('1,2\n')
        outputs=[{'kind':'file','path':str(target),'sha256':hashlib.sha256(target.read_bytes()).hexdigest()},{'kind':'text','title':'Input','content':'ledger input'}]
    rpc({'operation':'report_result','status':'succeeded','summary':'ledger step','outputs':outputs})
codex='exec' in sys.argv[1:3]
readonly=('plan' in sys.argv or 'read-only' in sys.argv)
if mode=='git':
    def git(*args):return subprocess.check_output(['git',*args],stderr=subprocess.STDOUT,text=True).strip()
    if not readonly:
        pathlib.Path('result.txt').write_text('fixture change')
        git('add','result.txt');git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-m','fixture result')
    result=git('rev-parse','HEAD')
else:result='fixture complete'
if codex:
    for event in [{'type':'thread.started','thread_id':'fixture-codex'},{'type':'item.completed','item':{'type':'agent_message','text':result}},{'type':'turn.completed'}]:print(json.dumps(event))
else:print(json.dumps({'result':result,'session_id':'fixture-claude'}))
"#.replace("PROTOCOL_VERSION",&PROTOCOL_VERSION.to_string())).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    env.set("FORMLESS_TOPIC_CLAUDE_EXE", &path);
    env.set("FORMLESS_TOPIC_CODEX_EXE", &path);
    env.set("TOPIC_FIXTURE_SOCKET", f.paths.socket_path());
    env.set("TOPIC_FIXTURE_PID", f.root.join("pid"));
    path
}
fn topic(f: &Fixture, workspace: Option<PathBuf>, two: bool) -> TopicInfo {
    let message = f
        .daemon()
        .with_store(|st| st.append_message(&f.session, "user", "Account for X and Y"))
        .unwrap();
    let features = if two {
        vec![
            TopicFeatureProposal {
                source_message_id: message.id.clone(),
                request: "X".into(),
            },
            TopicFeatureProposal {
                source_message_id: message.id.clone(),
                request: "Y".into(),
            },
        ]
    } else {
        vec![]
    };
    let proposal = TopicProposalRequest {
        parent_topic_id: None,
        source_session_id: f.session.clone(),
        message_ids: vec![message.id],
        features,
        name: "Fixture topic".into(),
        workspace,
        outline: vec![],
    };
    let topic = f.daemon().propose_topic_request(&proposal).unwrap();
    f.daemon()
        .with_store(|st| st.approve_topic(&topic.id))
        .unwrap()
}
fn state(f: &Fixture, id: &str) -> TopicInfo {
    f.daemon()
        .with_reader(|st| st.get_topic(id))
        .unwrap()
        .unwrap()
}
fn step(
    feature: &str,
    harness: HarnessKind,
    access: StepAccess,
    inputs: &[TopicArtifact],
) -> Value {
    json!({"feature_id":feature,"title":"Fixture step","instructions":"Perform exactly this step; never merge or push","harness":harness,"access":access,"inputs":inputs})
}
async fn settled(f: &Fixture, session: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        while f.daemon().runs.locked().contains_key(session) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn wait_until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
async fn model(
    f: &Fixture,
    env: &crate::credential_env::EnvGuard,
    topic: &TopicInfo,
    builder: HarnessKind,
    reviewer: HarnessKind,
    empty: bool,
) -> tokio::task::JoinHandle<()> {
    use std::sync::atomic::AtomicUsize;
    env.set("FORMLESS_API_KEY", "fixture-key");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    env.set("no_proxy", "127.0.0.1,localhost");
    env.remove("FORMLESS_BASE_URL");
    env.remove("FORMLESS_MODEL");
    let features: Vec<_> = topic.features.iter().map(|f| f.id.clone()).collect();
    let count = Arc::new(AtomicUsize::new(0));
    let fail = std::env::var("TOPIC_FIXTURE_MODE").ok().as_deref() == Some("provider_error");
    let app=axum::Router::new().route("/chat/completions",axum::routing::post(move|axum::Json(body):axum::Json<Value>|{
        let features=features.clone();let count=count.clone();
        async move{
            if fail{return (axum::http::StatusCode::INTERNAL_SERVER_ERROR,"fixture provider failure").into_response();}
            let round=count.fetch_add(1,Ordering::SeqCst);
            if empty||round<features.len()*3{assert!(body["tools"].as_array().unwrap().iter().any(|tool|tool["function"]["name"]=="topic_run_step"));}
            let delta=if empty||round>=features.len()*3{json!({"content":"Done"})}else{
                let feature=&features[round/3];
                let (name,args)=match round%3{
                    0=>("topic_run_step",step(feature,builder,StepAccess::Write,&[])),
                    1=>{
                        let tool=body["messages"].as_array().unwrap().iter().rev().find(|m|m["role"]=="tool").unwrap();
                        let mut result:Value=serde_json::from_str(tool["content"].as_str().unwrap()).unwrap();
                        if result["formless_data"]==1{result=serde_json::from_str(result["content"].as_str().unwrap()).unwrap();}
                        assert_eq!(result["status"],"succeeded","{result}");
                        let inputs:Vec<TopicArtifact>=serde_json::from_value(result["outputs"].clone()).unwrap();
                        ("topic_run_step",step(feature,reviewer,StepAccess::ReadOnly,&inputs))
                    },
                    _=>("topic_complete_feature",json!({"feature_id":feature,"summary":"Fixture verified exact revision"})),
                };
                json!({"tool_calls":[{"index":0,"id":format!("call-{round}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
            };
            let text=format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",json!({"choices":[{"index":0,"delta":delta}]}),json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}));
            ([("content-type","text/event-stream")],text).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = Config::load(&f.paths).unwrap();
    cfg.provider.base_url = format!("http://{}", listener.local_addr().unwrap());
    cfg.provider.model = "model".into();
    cfg.provider
        .model_context_tokens
        .insert("model".into(), 100_000);
    cfg.provider.max_retries = 0;
    cfg.provider.tool_use_nudge = false;
    cfg.save(&f.paths).unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() })
}
fn repository(f: &Fixture) -> PathBuf {
    let repo = f.root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "base",
        ],
    ] {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    repo
}

#[tokio::test]
async fn topic_orchestrator_fixture_git_harness_matrix_and_exact_revision_handoff() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "git");
    for (builder, reviewer) in [
        (HarnessKind::Claude, HarnessKind::Claude),
        (HarnessKind::Codex, HarnessKind::Codex),
        (HarnessKind::Codex, HarnessKind::Claude),
        (HarnessKind::Claude, HarnessKind::Codex),
    ] {
        let f = Fixture::new(&env);
        script(&f, &env);
        let repo = repository(&f);
        let topic = topic(&f, Some(repo.clone()), true);
        let server = model(&f, &env, &topic, builder, reviewer, false).await;
        f.daemon()
            .handle(Request::StartTopicWorkflow {
                topic_id: topic.id.clone(),
            })
            .await
            .unwrap();
        settled(&f, topic.topic_session_id.as_ref().unwrap()).await;
        let final_topic = state(&f, &topic.id);
        assert_eq!(final_topic.status, TopicStatus::Complete, "{final_topic:?}");
        assert_eq!(final_topic.attempts.len(), 4);
        for feature in &final_topic.features {
            let mut attempts: Vec<_> = final_topic
                .attempts
                .iter()
                .filter(|a| a.feature_id == feature.id)
                .collect();
            attempts.sort_by_key(|a| a.access == StepAccess::ReadOnly);
            let TopicArtifact::GitRevision { revision } = &attempts[0].outputs[0] else {
                panic!("missing Git revision")
            };
            assert_eq!(attempts[1].inputs, attempts[0].outputs);
            assert!(attempts[1].summary.as_ref().unwrap().contains(revision));
            assert!(attempts.iter().all(|a| a.harness_session_id.is_some()));
        }
        assert_eq!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(["status", "--porcelain"])
                .output()
                .unwrap()
                .stdout,
            b""
        );
        server.abort();
    }
}

#[tokio::test]
async fn topic_orchestrator_fixture_scratch_file_text_steps_and_empty_outline() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "scratch");
    let f = Fixture::new(&env);
    script(&f, &env);
    let topic = topic(&f, None, false);
    assert!(topic.outline.is_empty());
    let daemon = f.daemon().clone();
    let ipc = tokio::spawn(daemon.serve());
    wait_until(|| f.paths.socket_path().exists()).await;
    let server = model(
        &f,
        &env,
        &topic,
        HarnessKind::Claude,
        HarnessKind::Codex,
        false,
    )
    .await;
    f.daemon()
        .handle(Request::StartTopicWorkflow {
            topic_id: topic.id.clone(),
        })
        .await
        .unwrap();
    settled(&f, topic.topic_session_id.as_ref().unwrap()).await;
    let result = state(&f, &topic.id);
    assert_eq!(result.status, TopicStatus::Complete, "{result:?}");
    assert!(result.features[0].outputs.contains(&TopicArtifact::Text {
        title: "Total".into(),
        content: "total=3".into()
    }));
    f.daemon().shutdown.cancel();
    ipc.await.unwrap().unwrap();
    server.abort();
}

#[tokio::test]
async fn topic_orchestrator_fixture_duplicate_cancel_kills_cli_and_prevents_later_launch() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "hold");
    let f = Fixture::new(&env);
    script(&f, &env);
    let topic = topic(&f, None, false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let session = topic.topic_session_id.clone().unwrap();
    let args = step(
        &topic.features[0].id,
        HarnessKind::Claude,
        StepAccess::Write,
        &[],
    );
    let daemon = f.daemon().clone();
    let (s, a) = (session.clone(), args.clone());
    let task = tokio::spawn(async move {
        daemon
            .run_topic_step(&s, &a, &CancellationToken::new())
            .await
    });
    wait_until(|| {
        std::fs::read_to_string(f.root.join("pid"))
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .is_some()
    })
    .await;
    assert!(f
        .daemon()
        .run_topic_step(&session, &args, &CancellationToken::new())
        .await
        .unwrap_err()
        .to_string()
        .contains("active attempt"));
    let pid: i32 = std::fs::read_to_string(f.root.join("pid"))
        .unwrap()
        .parse()
        .unwrap();
    f.daemon()
        .handle(Request::CancelTopic {
            topic_id: topic.id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(task.await.unwrap().unwrap()["status"], "cancelled");
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert!(f
        .daemon()
        .run_topic_step(&session, &args, &CancellationToken::new())
        .await
        .is_err());
    assert_eq!(state(&f, &topic.id).attempts.len(), 1);
}

#[tokio::test]
async fn topic_orchestrator_fixture_unavailable_adapter_and_missing_executable_are_step_failures() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let topic = topic(&f, None, false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let session = topic.topic_session_id.clone().unwrap();
    let result = f
        .daemon()
        .run_topic_step(
            &session,
            &step(
                &topic.features[0].id,
                HarnessKind::Muse,
                StepAccess::Write,
                &[],
            ),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result["status"], "failed");
    assert!(result["summary"]
        .as_str()
        .unwrap()
        .contains("not integrated"));
    env.set("FORMLESS_TOPIC_CLAUDE_EXE", f.root.join("missing"));
    let result = f
        .daemon()
        .run_topic_step(
            &session,
            &step(
                &topic.features[0].id,
                HarnessKind::Claude,
                StepAccess::Write,
                &[],
            ),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result["status"], "failed");
    assert_eq!(state(&f, &topic.id).status, TopicStatus::Running);
    assert_eq!(
        state(&f, &topic.id).features[0].status,
        TopicFeatureStatus::Pending
    );
    assert!(f
        .daemon()
        .run_topic_step(
            &f.session,
            &step(
                &topic.features[0].id,
                HarnessKind::Claude,
                StepAccess::Write,
                &[]
            ),
            &CancellationToken::new()
        )
        .await
        .is_err());
}

#[tokio::test]
async fn topic_orchestrator_fixture_start_failure_retry_move_once_and_restart_recovery() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.remove("FORMLESS_API_KEY");
    let mut f = Fixture::new(&env);
    let topic = topic(&f, None, false);
    let session = topic.topic_session_id.clone().unwrap();
    let (steer_tx, _) = mpsc::unbounded_channel();
    f.daemon().runs.locked().insert(
        session.clone(),
        RunCtl {
            run_id: "fixture-busy-run".into(),
            cancel: CancellationToken::new(),
            cancel_flag: Arc::new(AtomicBool::new(false)),
            steer_tx,
            steering: Arc::new(Default::default()),
            retry: Arc::new(provider_retry_daemon::ProviderRetry::default()),
        },
    );
    let source = f
        .daemon()
        .with_reader(|st| st.list_messages(&f.session))
        .unwrap();
    assert!(f
        .daemon()
        .handle(Request::StartTopicWorkflow {
            topic_id: topic.id.clone()
        })
        .await
        .is_err());
    f.daemon().runs.locked().remove(&session);
    assert_eq!(state(&f, &topic.id).status, TopicStatus::Failed);
    f.daemon()
        .handle(Request::RetryTopicFeature {
            retry: TopicFeatureRetryRequest {
                topic_id: topic.id.clone(),
                feature_id: topic.features[0].id.clone(),
            },
        })
        .await
        .unwrap();
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let attempt = f
        .daemon()
        .with_store(|st| {
            st.begin_topic_attempt(
                &topic.id,
                &topic.features[0].id,
                "Interrupted",
                StepAccess::Write,
                HarnessKind::Claude,
                None,
                None,
                &[],
                "a sufficiently long fixture capability",
                topic.workspace.as_ref().unwrap(),
            )
        })
        .unwrap();
    assert_eq!(
        f.daemon()
            .with_reader(|st| st.list_messages(&f.session))
            .unwrap()
            .len(),
        source.len()
    );
    drop(f.daemon.take());
    f.daemon = Some(Arc::new(Daemon::new(f.paths.clone()).unwrap()));
    let recovered = state(&f, &topic.id);
    assert_eq!(recovered.status, TopicStatus::NeedsUser);
    assert_eq!(recovered.attempts[0].status, TopicAttemptStatus::Abandoned);
    assert!(f
        .daemon()
        .with_reader(|st| st
            .authenticate_topic_attempt(&attempt.id, "a sufficiently long fixture capability"))
        .is_err());
    assert!(f
        .daemon()
        .with_reader(|st| st.list_messages(&session))
        .unwrap()
        .iter()
        .any(|m| m.content.contains("Daemon restarted")));
}

#[tokio::test]
async fn topic_orchestrator_fixture_recover_stops_live_harness_before_retry() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "hold");
    let f = Fixture::new(&env);
    script(&f, &env);
    let topic = topic(&f, None, false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let session = topic.topic_session_id.clone().unwrap();
    let args = step(
        &topic.features[0].id,
        HarnessKind::Claude,
        StepAccess::Write,
        &[],
    );
    let daemon = f.daemon().clone();
    let s = session.clone();
    let a = args.clone();
    let task = tokio::spawn(async move {
        daemon
            .run_topic_step(&s, &a, &CancellationToken::new())
            .await
    });
    wait_until(|| {
        std::fs::read_to_string(f.root.join("pid"))
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .is_some()
    })
    .await;
    let pid: i32 = std::fs::read_to_string(f.root.join("pid"))
        .unwrap()
        .parse()
        .unwrap();
    f.daemon()
        .handle(Request::RecoverTopic {
            recovery: TopicRecoveryRequest {
                topic_id: topic.id.clone(),
            },
        })
        .await
        .unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        state(&f, &topic.id).attempts[0].status,
        TopicAttemptStatus::Abandoned
    );
    f.daemon()
        .handle(Request::RetryTopicFeature {
            retry: TopicFeatureRetryRequest {
                topic_id: topic.id.clone(),
                feature_id: topic.features[0].id.clone(),
            },
        })
        .await
        .unwrap();
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    env.set("TOPIC_FIXTURE_MODE", "done");
    let result = f
        .daemon()
        .run_topic_step(&session, &args, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result["status"], "succeeded");
    assert_eq!(state(&f, &topic.id).attempts.len(), 2);
}

#[tokio::test]
async fn topic_orchestrator_fixture_permission_answer_and_undecided_denial() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "permission");
    for answer in [Some(true), Some(false), None] {
        let f = Fixture::new(&env);
        script(&f, &env);
        let topic = topic(&f, None, false);
        f.daemon()
            .with_store(|st| st.claim_topic_workflow(&topic.id))
            .unwrap();
        let daemon = f.daemon().clone();
        let ipc = tokio::spawn(daemon.serve());
        wait_until(|| f.paths.socket_path().exists()).await;
        let session = topic.topic_session_id.clone().unwrap();
        let args = step(
            &topic.features[0].id,
            HarnessKind::Claude,
            StepAccess::Write,
            &[],
        );
        let daemon = f.daemon().clone();
        let task = tokio::spawn(async move {
            daemon
                .run_topic_step(&session, &args, &CancellationToken::new())
                .await
        });
        wait_until(|| !f.daemon().harness_permissions.locked().is_empty()).await;
        let permission_id = f
            .daemon()
            .harness_permissions
            .locked()
            .keys()
            .next()
            .unwrap()
            .clone();
        if let Some(answer) = answer {
            f.daemon()
                .handle(Request::HarnessPermissionDecision {
                    permission_id,
                    option_id: answer.then(|| "allow".into()),
                })
                .await
                .unwrap();
        } else {
            f.daemon()
                .handle(Request::CancelTopic {
                    topic_id: topic.id.clone(),
                })
                .await
                .unwrap();
        }
        assert_eq!(
            task.await.unwrap().unwrap()["status"],
            match answer {
                Some(true) => "succeeded",
                Some(false) => "failed",
                None => "cancelled",
            }
        );
        assert!(f.daemon().harness_permissions.locked().is_empty());
        f.daemon().shutdown.cancel();
        ipc.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn topic_orchestrator_fixture_normal_run_end_needs_user_and_tools_reappear() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let topic = topic(&f, None, false);
    let server = model(
        &f,
        &env,
        &topic,
        HarnessKind::Claude,
        HarnessKind::Claude,
        true,
    )
    .await;
    let session = topic.topic_session_id.clone().unwrap();
    f.daemon()
        .handle(Request::StartTopicWorkflow {
            topic_id: topic.id.clone(),
        })
        .await
        .unwrap();
    settled(&f, &session).await;
    assert_eq!(state(&f, &topic.id).status, TopicStatus::NeedsUser);
    f.daemon()
        .handle(Request::SendMessage {
            session_id: session.clone(),
            content: "Continue".into(),
            mode: SendMode::Send,
            client_msg_id: None,
        })
        .await
        .unwrap();
    settled(&f, &session).await;
    assert_eq!(state(&f, &topic.id).status, TopicStatus::NeedsUser);
    f.daemon()
        .with_reader(|st| {
            assert!(toolreg::tool_definitions(st, Some(&session), 20)?
                .iter()
                .any(|tool| tool["function"]["name"] == "topic_run_step"));
            assert!(!toolreg::tool_definitions(st, Some(&f.session), 20)?
                .iter()
                .any(|tool| tool["function"]["name"] == "topic_run_step"));
            Ok(())
        })
        .unwrap();
    server.abort();
}

#[test]
fn topic_orchestrator_fixture_streamed_updates_coalesce_until_tool_plan_or_end() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let topic = topic(&f, None, false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let token = "streaming fixture capability token";
    let attempt = f
        .daemon()
        .with_store(|st| {
            st.begin_topic_attempt(
                &topic.id,
                &topic.features[0].id,
                "Stream",
                StepAccess::Write,
                HarnessKind::Claude,
                None,
                None,
                &[],
                token,
                topic.workspace.as_ref().unwrap(),
            )
        })
        .unwrap();
    let handler = f
        .daemon()
        .topic_interaction_handler(&attempt.id, token)
        .unwrap();
    handler
        .on_update(HarnessUpdate::Message {
            content: "Hello ".into(),
        })
        .unwrap();
    handler
        .on_update(HarnessUpdate::Thought {
            content: "think ".into(),
        })
        .unwrap();
    handler
        .on_update(HarnessUpdate::Message {
            content: "world".into(),
        })
        .unwrap();
    handler
        .on_update(HarnessUpdate::Thought {
            content: "more".into(),
        })
        .unwrap();
    handler
        .on_update(HarnessUpdate::ToolCall {
            name: "Read".into(),
            input: json!({}),
        })
        .unwrap();
    handler
        .on_update(HarnessUpdate::Message {
            content: "Next".into(),
        })
        .unwrap();
    handler
        .on_update(HarnessUpdate::Plan {
            content: "Plan".into(),
        })
        .unwrap();
    handler
        .on_update(HarnessUpdate::Message {
            content: "End".into(),
        })
        .unwrap();
    handler.finish().unwrap();
    handler
        .on_update(HarnessUpdate::Message {
            content: "New turn".into(),
        })
        .unwrap();
    let messages = f
        .daemon()
        .with_reader(|st| st.list_messages(&attempt.conversation_session_id))
        .unwrap();
    assert_eq!(messages.len(), 6);
    assert_eq!(messages[0].content, "Hello world");
    assert_eq!(
        serde_json::from_str::<Value>(messages[0].meta.as_ref().unwrap()).unwrap()["thought"],
        "think more"
    );
}

#[tokio::test]
async fn topic_orchestrator_fixture_run_error_fails_topic_and_releases_workspace() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "provider_error");
    let f = Fixture::new(&env);
    let topic = topic(&f, None, false);
    let server = model(
        &f,
        &env,
        &topic,
        HarnessKind::Claude,
        HarnessKind::Claude,
        true,
    )
    .await;
    f.daemon()
        .handle(Request::StartTopicWorkflow {
            topic_id: topic.id.clone(),
        })
        .await
        .unwrap();
    settled(&f, topic.topic_session_id.as_ref().unwrap()).await;
    assert_eq!(state(&f, &topic.id).status, TopicStatus::Failed);
    f.daemon()
        .handle(Request::RetryTopicFeature {
            retry: TopicFeatureRetryRequest {
                topic_id: topic.id.clone(),
                feature_id: topic.features[0].id.clone(),
            },
        })
        .await
        .unwrap();
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    server.abort();
}
