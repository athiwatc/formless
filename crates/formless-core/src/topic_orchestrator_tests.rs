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
if '--version' in sys.argv:print('fixture 1.0');sys.exit()
if sys.argv[1:]==['auth','status']:print(json.dumps({} if os.environ.get('TOPIC_FIXTURE_AUTH_UNKNOWN') else {'loggedIn':True}));sys.exit()
if sys.argv[1:]==['login','status']:print('Logged in');sys.exit()
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
if mode=='question':
    answer=rpc({'operation':'ask_user','questions':[{'question':'Use the user-approved accounting inputs?','options':[{'label':'Yes'},{'label':'No'}]}]})
    assert answer[0]['selected']==['Yes']
if mode in ('report_release','report_dirty'):
    if mode=='report_dirty':pathlib.Path('uncommitted.txt').write_text('not a validated commit')
    rpc({'operation':'report_result','status':'succeeded','summary':'provisional result','outputs':[]})
if mode in ('release','report_release','report_dirty'):
    gate=pathlib.Path(os.environ['TOPIC_FIXTURE_RELEASE_DIR'])/os.environ['FORMLESS_TOPIC_ATTEMPT_ID']
    gate.with_suffix('.ready').write_text(str(os.getpid()))
    while not gate.with_suffix('.go').exists():time.sleep(0.01)
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
    env.set("TOPIC_FIXTURE_RELEASE_DIR", &f.root);
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

#[tokio::test]
async fn topic_generated_kickoff_is_marked_in_first_broadcast_and_session_wire() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let topic = topic(&f, None, false);
    let (tx, mut rx) = mpsc::channel(256);
    f.daemon().subs.locked().push(Subscriber {
        conn: 98,
        tx: tx.into(),
        filter: Arc::new(StdMutex::new(EventFilter::default())),
        kick: CancellationToken::new(),
        gaps: BTreeMap::new(),
    });
    let server = model(
        &f,
        &env,
        &topic,
        HarnessKind::Claude,
        HarnessKind::Codex,
        true,
    )
    .await;
    f.daemon()
        .handle(Request::StartTopicWorkflow {
            topic_id: topic.id.clone(),
        })
        .await
        .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(ServerMessage::Event(Event::MessageAppended { message, .. })) =
                rx.recv().await
            {
                if message
                    .content
                    .starts_with("You are the Formless topic orchestrator.")
                {
                    break message;
                }
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(message.role, "user");
    assert_eq!(message.generated_by.as_deref(), Some("topic_kickoff"));
    let session = topic.topic_session_id.clone().unwrap();
    let wire = f
        .daemon()
        .handle(Request::GetSession {
            id: session.clone(),
        })
        .await
        .unwrap();
    assert_eq!(
        wire["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == message.id)
            .unwrap()["generated_by"],
        "topic_kickoff"
    );
    settled(&f, &session).await;
    assert!(f
        .daemon()
        .handle(Request::ProposeTopic {
            proposal: TopicProposalRequest {
                parent_topic_id: None,
                source_session_id: session,
                message_ids: vec![message.id],
                features: vec![],
                name: "Not a user request".into(),
                workspace: None,
                outline: vec![],
            }
        })
        .await
        .unwrap_err()
        .to_string()
        .contains("generated"));
    server.abort();
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
            if empty||round<features.len()*5{assert!(body["tools"].as_array().unwrap().iter().any(|tool|tool["function"]["name"]=="topic_run_step"));}
            let delta=if empty||round>=features.len()*5{json!({"content":"Done"})}else{
                let feature=&features[round/5];
                let (name,args)=match round%5{
                    0=>("topic_run_step",step(feature,builder,StepAccess::Write,&[])),
                    1|3=>{
                        let tool=body["messages"].as_array().unwrap().iter().rev().find(|m|m["role"]=="tool").unwrap();
                        let mut result:Value=serde_json::from_str(tool["content"].as_str().unwrap()).unwrap();
                        if result["formless_data"]==1{result=serde_json::from_str(result["content"].as_str().unwrap()).unwrap();}
                        ("topic_wait",json!({"attempt_ids":[result["attempt_id"]]}))
                    },
                    2=>{
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
impl Daemon {
    async fn run_topic_step_until_done(
        self: &Arc<Self>,
        session: &str,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let launched = self.run_topic_step(session, args, cancel).await?;
        let id = launched["attempt_id"]
            .as_str()
            .context("missing attempt_id")?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let wake = self.topic_activity.notified();
            tokio::pin!(wake);
            wake.as_mut().enable();
            let saved = self
                .with_reader(|st| st.get_topic_attempt(id))?
                .context("attempt missing")?;
            if !self
                .topic_steps
                .locked()
                .contains_key(&(saved.topic_id.clone(), saved.id.clone()))
            {
                return Ok(serde_json::to_value(saved)?);
            }
            tokio::select! {_=&mut wake=>{},_=tokio::time::sleep_until(deadline)=>bail!("attempt did not finish within fixture deadline"),}
        }
    }
}

#[tokio::test]
async fn topic_async_wait_concurrent_completions_empty_errors_and_cancel() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "release");
    let f = Fixture::new(&env);
    script(&f, &env);
    let topic = topic(&f, None, true);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let session = topic.topic_session_id.clone().unwrap();
    let cancel = CancellationToken::new();
    assert_eq!(
        f.daemon()
            .wait_topic_steps(&session, &json!({}), &cancel)
            .await
            .unwrap()["event"],
        "snapshot"
    );
    for args in [
        json!({"timeout_secs":0}),
        json!({"timeout_secs":601}),
        json!({"attempt_ids":["foreign"]}),
    ] {
        assert!(f
            .daemon()
            .wait_topic_steps(&session, &args, &cancel)
            .await
            .is_err());
    }
    let first = tokio::time::timeout(
        Duration::from_secs(1),
        f.daemon().run_topic_step(
            &session,
            &step(
                &topic.features[0].id,
                HarnessKind::Claude,
                StepAccess::Write,
                &[],
            ),
            &cancel,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let second = f
        .daemon()
        .run_topic_step(
            &session,
            &step(
                &topic.features[1].id,
                HarnessKind::Codex,
                StepAccess::Write,
                &[],
            ),
            &cancel,
        )
        .await
        .unwrap();
    let a = first["attempt_id"].as_str().unwrap();
    let b = second["attempt_id"].as_str().unwrap();
    wait_until(|| {
        f.root.join(format!("{a}.ready")).exists() && f.root.join(format!("{b}.ready")).exists()
    })
    .await;
    assert_eq!(f.daemon().topic_steps.locked().len(), 2);
    assert_eq!(
        f.daemon()
            .wait_topic_steps(&session, &json!({"attempt_ids":[]}), &cancel)
            .await
            .unwrap()["attempts"],
        json!([])
    );
    let timed = f
        .daemon()
        .wait_topic_steps(&session, &json!({"timeout_secs":1}), &cancel)
        .await
        .unwrap();
    assert_eq!(timed["event"], "snapshot");
    assert_eq!(timed["attempts"].as_array().unwrap().len(), 2);
    std::fs::write(f.root.join(format!("{b}.go")), "").unwrap();
    let done = f
        .daemon()
        .wait_topic_steps(&session, &json!({}), &cancel)
        .await
        .unwrap();
    assert_eq!(done["attempt_id"], b);
    assert_eq!(done["status"], "succeeded");
    std::fs::write(f.root.join(format!("{a}.go")), "").unwrap();
    assert_eq!(
        f.daemon()
            .wait_topic_steps(&session, &json!({}), &cancel)
            .await
            .unwrap()["attempt_id"],
        a
    );
    let started = f
        .daemon()
        .run_topic_step(
            &session,
            &step(
                &topic.features[0].id,
                HarnessKind::Claude,
                StepAccess::Write,
                &[],
            ),
            &cancel,
        )
        .await
        .unwrap();
    let id = started["attempt_id"].as_str().unwrap();
    wait_until(|| f.root.join(format!("{id}.ready")).exists()).await;
    let d = f.daemon().clone();
    let s = session.clone();
    let c = cancel.clone();
    let waiting = tokio::spawn(async move { d.wait_topic_steps(&s, &json!({}), &c).await });
    cancel.cancel();
    assert_eq!(waiting.await.unwrap().unwrap()["event"], "cancelled");
    wait_until(|| f.daemon().topic_steps.locked().is_empty()).await;
    assert_eq!(
        state(&f, &topic.id)
            .attempts
            .iter()
            .find(|attempt| attempt.id == id)
            .unwrap()
            .status,
        TopicAttemptStatus::Cancelled
    );
    let cancel = CancellationToken::new();
    let launched = f
        .daemon()
        .run_topic_step(
            &session,
            &step(
                &topic.features[0].id,
                HarnessKind::Claude,
                StepAccess::Write,
                &[],
            ),
            &cancel,
        )
        .await
        .unwrap();
    let id = launched["attempt_id"].as_str().unwrap();
    wait_until(|| f.root.join(format!("{id}.ready")).exists()).await;
    let d = f.daemon().clone();
    let s = session.clone();
    let c = cancel.clone();
    let waiting = tokio::spawn(async move { d.wait_topic_steps(&s, &json!({}), &c).await });
    f.daemon()
        .handle(Request::CancelTopic {
            topic_id: topic.id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(waiting.await.unwrap().unwrap()["event"], "cancelled");
    wait_until(|| f.daemon().topic_steps.locked().is_empty()).await;
}

#[tokio::test]
async fn topic_async_wait_surfaces_live_question_model_answer_and_informational_permission() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "question");
    let f = Fixture::new(&env);
    script(&f, &env);
    let topic = topic(&f, None, true);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let ipc = tokio::spawn(f.daemon().clone().serve());
    wait_until(|| f.paths.socket_path().exists()).await;
    let session = topic.topic_session_id.clone().unwrap();
    let cancel = CancellationToken::new();
    let launched = f
        .daemon()
        .run_topic_step(
            &session,
            &step(
                &topic.features[0].id,
                HarnessKind::Claude,
                StepAccess::Write,
                &[],
            ),
            &cancel,
        )
        .await
        .unwrap();
    let question = f
        .daemon()
        .wait_topic_steps(
            &session,
            &json!({"attempt_ids":[launched["attempt_id"]]}),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(question["event"], "question");
    assert_eq!(question["user_only"], false);
    assert_eq!(f.daemon().topic_steps.locked().len(), 1);
    let answered=f.daemon().answer_topic_question_tool(&session,&json!({"question_id":question["question_id"],"answers":[{"selected":["Yes"]}],"basis":"The user's moved request says: Account for X and Y."})).unwrap();
    assert_eq!(answered["answered"], true);
    assert_eq!(
        f.daemon()
            .wait_topic_steps(
                &session,
                &json!({"attempt_ids":[launched["attempt_id"]]}),
                &cancel
            )
            .await
            .unwrap()["status"],
        "succeeded"
    );
    env.set("TOPIC_FIXTURE_MODE", "permission");
    let permission_step = f
        .daemon()
        .run_topic_step(
            &session,
            &step(
                &topic.features[1].id,
                HarnessKind::Codex,
                StepAccess::Write,
                &[],
            ),
            &cancel,
        )
        .await
        .unwrap();
    let permission = f
        .daemon()
        .wait_topic_steps(
            &session,
            &json!({"attempt_ids":[permission_step["attempt_id"]]}),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(permission["event"], "permission");
    assert_eq!(permission["informational_only"], true);
    assert!(f.daemon().answer_topic_question_tool(&session,&json!({"question_id":permission["permission_id"],"answers":[{"selected":["allow"]}],"basis":"Not a permission decision path"})).is_err());
    f.daemon()
        .handle(Request::HarnessPermissionDecision {
            permission_id: permission["permission_id"].as_str().unwrap().into(),
            option_id: Some("allow".into()),
        })
        .await
        .unwrap();
    assert_eq!(
        f.daemon()
            .wait_topic_steps(
                &session,
                &json!({"attempt_ids":[permission_step["attempt_id"]]}),
                &cancel
            )
            .await
            .unwrap()["status"],
        "succeeded"
    );
    f.daemon().request_shutdown();
    ipc.await.unwrap().unwrap();
}

#[tokio::test]
async fn topic_async_normal_model_end_keeps_attempt_and_waiting_alive() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "release");
    let f = Fixture::new(&env);
    script(&f, &env);
    let topic = topic(&f, None, false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let session = topic.topic_session_id.clone().unwrap();
    let cancel = CancellationToken::new();
    let started = f
        .daemon()
        .run_topic_step(
            &session,
            &step(
                &topic.features[0].id,
                HarnessKind::Claude,
                StepAccess::Write,
                &[],
            ),
            &cancel,
        )
        .await
        .unwrap();
    let id = started["attempt_id"].as_str().unwrap();
    wait_until(|| f.root.join(format!("{id}.ready")).exists()).await;
    f.daemon()
        .topic_orchestrator_ended(&session, "", "completed");
    assert_eq!(state(&f, &topic.id).status, TopicStatus::NeedsUser);
    assert!(!f
        .daemon()
        .topic_steps
        .locked()
        .values()
        .next()
        .unwrap()
        .is_cancelled());
    std::fs::write(f.root.join(format!("{id}.go")), "").unwrap();
    assert_eq!(
        f.daemon()
            .wait_topic_steps(&session, &json!({"attempt_ids":[id]}), &cancel)
            .await
            .unwrap()["status"],
        "succeeded"
    );
    assert!(f.daemon().runs.locked().is_empty());
}

#[tokio::test]
async fn topic_idle_watcher_keeps_live_harnesses_and_pending_interactions_alive() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.remove(STAY_ENV);
    env.set("TOPIC_FIXTURE_MODE", "release");
    let f = Fixture::new(&env);
    script(&f, &env);
    let topic = topic(&f, None, false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    f.daemon().config().unwrap();
    f.daemon()
        .config
        .locked()
        .as_mut()
        .unwrap()
        .1
        .daemon
        .idle_grace_secs = 0;
    let session = topic.topic_session_id.clone().unwrap();
    let cancel = CancellationToken::new();
    let started = f
        .daemon()
        .run_topic_step(
            &session,
            &step(
                &topic.features[0].id,
                HarnessKind::Claude,
                StepAccess::Write,
                &[],
            ),
            &cancel,
        )
        .await
        .unwrap();
    let id = started["attempt_id"].as_str().unwrap();
    wait_until(|| f.root.join(format!("{id}.ready")).exists()).await;
    assert!(f.daemon().runs.locked().is_empty());
    assert_eq!(f.daemon().subscriber_count(), 0);
    assert!(!f.daemon().nothing_in_flight());
    let mut watcher = tokio::spawn(f.daemon().clone().idle_watcher());
    assert!(
        tokio::time::timeout(Duration::from_millis(1200), &mut watcher)
            .await
            .is_err()
    );
    assert!(!f.daemon().shutdown.is_cancelled());
    std::fs::write(f.root.join(format!("{id}.go")), "").unwrap();
    assert_eq!(
        f.daemon()
            .wait_topic_steps(&session, &json!({"attempt_ids":[id]}), &cancel)
            .await
            .unwrap()["status"],
        "succeeded"
    );
    let token = "idle watcher interaction capability token";
    let attempt = f
        .daemon()
        .with_store(|st| {
            st.begin_topic_attempt(
                &topic.id,
                &topic.features[0].id,
                "Idle question",
                StepAccess::ReadOnly,
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
    let question = tokio::spawn(async move {
        handler
            .ask_question(
                vec![Question::single("Continue?".into(), vec!["Yes".into()])],
                &CancellationToken::new(),
            )
            .await
    });
    wait_until(|| !f.daemon().topic_questions.locked().is_empty()).await;
    assert!(f.daemon().topic_steps.locked().is_empty());
    assert!(!f.daemon().nothing_in_flight());
    assert!(
        tokio::time::timeout(Duration::from_millis(1200), &mut watcher)
            .await
            .is_err()
    );
    assert!(!f.daemon().shutdown.is_cancelled());
    f.daemon().close_topic_interactions(&topic.id, None);
    assert!(question.await.unwrap().is_none());
    tokio::time::timeout(Duration::from_secs(2), watcher)
        .await
        .unwrap()
        .unwrap();
    assert!(f.daemon().shutdown.is_cancelled());
}

#[tokio::test]
async fn topic_retry_refuses_live_steps_after_normal_model_end_without_losing_results() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "release");
    let f = Fixture::new(&env);
    script(&f, &env);
    let topic = topic(&f, None, true);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let session = topic.topic_session_id.clone().unwrap();
    let cancel = CancellationToken::new();
    let launched = f
        .daemon()
        .run_topic_step(
            &session,
            &step(
                &topic.features[0].id,
                HarnessKind::Claude,
                StepAccess::Write,
                &[],
            ),
            &cancel,
        )
        .await
        .unwrap();
    let id = launched["attempt_id"].as_str().unwrap();
    wait_until(|| f.root.join(format!("{id}.ready")).exists()).await;
    f.daemon()
        .topic_orchestrator_ended(&session, "", "completed");
    for feature in &topic.features {
        assert!(f
            .daemon()
            .handle(Request::RetryTopicFeature {
                retry: TopicFeatureRetryRequest {
                    topic_id: topic.id.clone(),
                    feature_id: feature.id.clone()
                }
            })
            .await
            .unwrap_err()
            .to_string()
            .contains("live harness"));
        assert!(f
            .daemon()
            .with_store(|st| st.retry_topic_feature(&topic.id, &feature.id))
            .is_err());
    }
    assert_eq!(state(&f, &topic.id).status, TopicStatus::NeedsUser);
    std::fs::write(f.root.join(format!("{id}.go")), "").unwrap();
    assert_eq!(
        f.daemon()
            .wait_topic_steps(&session, &json!({"attempt_ids":[id]}), &cancel)
            .await
            .unwrap()["status"],
        "succeeded"
    );
    let retried = f
        .daemon()
        .handle(Request::RetryTopicFeature {
            retry: TopicFeatureRetryRequest {
                topic_id: topic.id.clone(),
                feature_id: topic.features[0].id.clone(),
            },
        })
        .await
        .unwrap();
    assert_eq!(retried["status"], "approved");
    assert_eq!(retried["attempts"][0]["status"], "succeeded");
    assert!(f.daemon().runs.locked().is_empty());
    assert!(f.daemon().topic_steps.locked().is_empty());
}

#[tokio::test]
async fn topic_reported_result_cannot_complete_feature_before_exit_and_validation() {
    let env = crate::credential_env::EnvGuard::isolated();
    for dirty in [false, true] {
        env.set(
            "TOPIC_FIXTURE_MODE",
            if dirty {
                "report_dirty"
            } else {
                "report_release"
            },
        );
        let f = Fixture::new(&env);
        script(&f, &env);
        let repo = dirty.then(|| repository(&f));
        let topic = topic(&f, repo, false);
        f.daemon()
            .with_store(|st| st.claim_topic_workflow(&topic.id))
            .unwrap();
        let ipc = tokio::spawn(f.daemon().clone().serve());
        wait_until(|| f.paths.socket_path().exists()).await;
        let session = topic.topic_session_id.clone().unwrap();
        let cancel = CancellationToken::new();
        let started = f
            .daemon()
            .run_topic_step(
                &session,
                &step(
                    &topic.features[0].id,
                    HarnessKind::Claude,
                    StepAccess::Write,
                    &[],
                ),
                &cancel,
            )
            .await
            .unwrap();
        let id = started["attempt_id"].as_str().unwrap();
        wait_until(|| f.root.join(format!("{id}.ready")).exists()).await;
        let reported = state(&f, &topic.id);
        assert_eq!(reported.features[0].active_attempt_id.as_deref(), Some(id));
        let args = json!({"feature_id":topic.features[0].id,"summary":"too early"});
        assert!(f
            .daemon()
            .finish_topic_feature_tool(&session, &args, true)
            .is_err());
        assert!(f
            .daemon()
            .with_store(|st| st.finish_topic_feature(
                &session,
                &topic.features[0].id,
                true,
                "too early"
            ))
            .is_err());
        std::fs::write(f.root.join(format!("{id}.go")), "").unwrap();
        let done = f
            .daemon()
            .wait_topic_steps(&session, &json!({"attempt_ids":[id]}), &cancel)
            .await
            .unwrap();
        assert_eq!(done["status"], if dirty { "failed" } else { "succeeded" });
        assert_eq!(
            state(&f, &topic.id).features[0].status,
            TopicFeatureStatus::Pending
        );
        if dirty {
            assert!(done["summary"].as_str().unwrap().contains("clean worktree"));
        } else {
            assert_eq!(
                f.daemon()
                    .finish_topic_feature_tool(&session, &args, true)
                    .unwrap()["status"],
                "complete"
            );
        }
        f.daemon().request_shutdown();
        ipc.await.unwrap().unwrap();
    }
}

#[tokio::test]
async fn topic_wait_returns_old_attempt_while_newer_attempt_of_same_feature_runs() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "release");
    let f = Fixture::new(&env);
    script(&f, &env);
    let topic = topic(&f, None, false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let session = topic.topic_session_id.clone().unwrap();
    let cancel = CancellationToken::new();
    let first = f
        .daemon()
        .run_topic_step(
            &session,
            &step(
                &topic.features[0].id,
                HarnessKind::Claude,
                StepAccess::Write,
                &[],
            ),
            &cancel,
        )
        .await
        .unwrap();
    let first = first["attempt_id"].as_str().unwrap();
    wait_until(|| f.root.join(format!("{first}.ready")).exists()).await;
    std::fs::write(f.root.join(format!("{first}.go")), "").unwrap();
    wait_until(|| f.daemon().topic_steps.locked().is_empty()).await;
    let second = f
        .daemon()
        .run_topic_step(
            &session,
            &step(
                &topic.features[0].id,
                HarnessKind::Codex,
                StepAccess::ReadOnly,
                &[],
            ),
            &cancel,
        )
        .await
        .unwrap();
    let second = second["attempt_id"].as_str().unwrap();
    wait_until(|| f.root.join(format!("{second}.ready")).exists()).await;
    assert!(f
        .daemon()
        .topic_steps
        .locked()
        .contains_key(&(topic.id.clone(), second.into())));
    let done = f
        .daemon()
        .wait_topic_steps(
            &session,
            &json!({"attempt_ids":[first],"timeout_secs":1}),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(done["event"], "completed");
    assert_eq!(done["attempt_id"], first);
    assert_eq!(done["status"], "succeeded");
    assert_eq!(
        state(&f, &topic.id)
            .attempts
            .iter()
            .find(|attempt| attempt.id == second)
            .unwrap()
            .status,
        TopicAttemptStatus::Running
    );
    std::fs::write(f.root.join(format!("{second}.go")), "").unwrap();
    assert_eq!(
        f.daemon()
            .wait_topic_steps(&session, &json!({"attempt_ids":[second]}), &cancel)
            .await
            .unwrap()["attempt_id"],
        second
    );
}

#[tokio::test]
async fn topic_limits_refuse_fourth_retry_require_user_and_allow_explicit_increase() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "plain");
    let f = Fixture::new(&env);
    script(&f, &env);
    let topic = topic(&f, None, false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let session = topic.topic_session_id.clone().unwrap();
    let cancel = CancellationToken::new();
    let args = step(
        &topic.features[0].id,
        HarnessKind::Claude,
        StepAccess::Write,
        &[],
    );
    for _ in 0..4 {
        assert_eq!(
            f.daemon()
                .run_topic_step_until_done(&session, &args, &cancel)
                .await
                .unwrap()["status"],
            "succeeded"
        );
    }
    assert!(f
        .daemon()
        .run_topic_step(&session, &args, &cancel)
        .await
        .unwrap_err()
        .to_string()
        .contains("Limit reached"));
    let snapshot = state(&f, &topic.id);
    assert_eq!(snapshot.attempts.len(), 4);
    assert_eq!(snapshot.status, TopicStatus::NeedsUser);
    let question = f
        .daemon()
        .wait_topic_steps(&session, &json!({}), &cancel)
        .await
        .unwrap();
    assert_eq!(question["event"], "question");
    assert_eq!(question["user_only"], true);
    let id = question["question_id"].as_str().unwrap().to_owned();
    assert!(f.daemon().answer_topic_question_tool(&session,&json!({"question_id":id,"answers":[{"selected":["Raise limit and continue"]}],"basis":"made up"})).unwrap_err().to_string().contains("user_only"));
    assert!(f
        .daemon()
        .handle(Request::AnswerQuestion {
            question_id: id.clone(),
            answer: "invalid".into(),
            answers: vec![]
        })
        .await
        .is_err());
    f.daemon()
        .handle(Request::AnswerQuestion {
            question_id: id,
            answer: "Raise limit and continue".into(),
            answers: vec![],
        })
        .await
        .unwrap();
    assert_eq!(state(&f, &topic.id).limits.max_retries_per_step, 6);
    assert_eq!(state(&f, &topic.id).status, TopicStatus::Running);
    assert_eq!(
        f.daemon()
            .run_topic_step_until_done(&session, &args, &cancel)
            .await
            .unwrap()["status"],
        "succeeded"
    );
    f.daemon()
        .handle(Request::SetTopicLimits {
            topic_id: topic.id.clone(),
            limits: TopicLimits {
                max_retries_per_step: 6,
                max_attempts_per_topic: 5,
            },
        })
        .await
        .unwrap();
    assert!(f
        .daemon()
        .run_topic_step(&session, &args, &cancel)
        .await
        .is_err());
    let question = f
        .daemon()
        .wait_topic_steps(&session, &json!({}), &cancel)
        .await
        .unwrap();
    f.daemon()
        .handle(Request::AnswerQuestion {
            question_id: question["question_id"].as_str().unwrap().into(),
            answer: "Stop".into(),
            answers: vec![],
        })
        .await
        .unwrap();
    assert_eq!(state(&f, &topic.id).status, TopicStatus::NeedsUser);
    f.daemon()
        .handle(Request::SetTopicLimits {
            topic_id: topic.id.clone(),
            limits: TopicLimits {
                max_retries_per_step: 6,
                max_attempts_per_topic: 6,
            },
        })
        .await
        .unwrap();
    assert_eq!(
        f.daemon()
            .run_topic_step_until_done(&session, &args, &cancel)
            .await
            .unwrap()["status"],
        "succeeded"
    );
    assert!(f
        .daemon()
        .handle(Request::SetTopicLimits {
            topic_id: "missing".into(),
            limits: TopicLimits::default()
        })
        .await
        .is_err());
}

#[tokio::test]
async fn topic_readiness_rejects_missing_harness_and_user_can_approve_one_step_substitute() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "plain");
    let f = Fixture::new(&env);
    script(&f, &env);
    env.set("FORMLESS_TOPIC_CLAUDE_EXE", f.root.join("missing"));
    let topic = topic(&f, None, false);
    let session = topic.topic_session_id.clone().unwrap();
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let cancel = CancellationToken::new();
    let args = step(
        &topic.features[0].id,
        HarnessKind::Claude,
        StepAccess::Write,
        &[],
    );
    assert!(f
        .daemon()
        .run_topic_step(&session, &args, &cancel)
        .await
        .unwrap_err()
        .to_string()
        .contains("not ready"));
    assert!(state(&f, &topic.id).attempts.is_empty());
    let values = f.daemon().handle(Request::ListHarnesses).await.unwrap();
    assert_eq!(values.as_array().unwrap().len(), 7);
    assert_eq!(values[0]["installed"], false);
    assert_eq!(values[1]["installed"], true);
    let request = json!({"feature_id":topic.features[0].id,"step_title":args["title"],"wanted":{"harness":"claude"},"substitute":{"harness":"codex"},"reason":"Claude is unavailable"});
    let daemon = f.daemon().clone();
    let s = session.clone();
    let c = cancel.clone();
    let waiting = tokio::spawn(async move {
        daemon
            .request_topic_harness_substitute(&s, &request, &c)
            .await
    });
    wait_until(|| !f.daemon().topic_decisions.locked().is_empty()).await;
    let question = f
        .daemon()
        .wait_topic_steps(&session, &json!({}), &cancel)
        .await
        .unwrap();
    assert_eq!(question["user_only"], true);
    let id = question["question_id"].as_str().unwrap().to_owned();
    assert!(f.daemon().answer_topic_question_tool(&session,&json!({"question_id":id,"answers":[{"selected":["Use Codex"]}],"basis":"No human approved this"})).is_err());
    f.daemon()
        .handle(Request::AnswerQuestion {
            question_id: id,
            answer: "Use Codex".into(),
            answers: vec![],
        })
        .await
        .unwrap();
    assert_eq!(waiting.await.unwrap().unwrap()["answer"], "Use Codex");
    assert_eq!(
        f.daemon()
            .run_topic_step_until_done(&session, &args, &cancel)
            .await
            .unwrap()["status"],
        "succeeded"
    );
    assert_eq!(state(&f, &topic.id).attempts[0].harness, HarnessKind::Codex);
    assert!(f.daemon().topic_substitutes.locked().is_empty());
    assert!(f
        .daemon()
        .run_topic_step(&session, &args, &cancel)
        .await
        .is_err());
}

#[tokio::test]
async fn topic_substitute_retry_skip_cancel_and_invalid_requests_do_not_authorize_launch() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let topic = topic(&f, None, false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let session = topic.topic_session_id.clone().unwrap();
    let request = json!({"feature_id":topic.features[0].id,"step_title":"step","wanted":{"harness":"claude"},"substitute":{"harness":"codex"},"reason":"unavailable"});
    let mut invalid = request.clone();
    invalid["reason"] = json!("");
    assert!(f
        .daemon()
        .request_topic_harness_substitute(&session, &invalid, &CancellationToken::new())
        .await
        .is_err());
    invalid = request.clone();
    invalid["substitute"] = invalid["wanted"].clone();
    assert!(f
        .daemon()
        .request_topic_harness_substitute(&session, &invalid, &CancellationToken::new())
        .await
        .is_err());
    for answer in [Some("Sign in and retry"), Some("Skip step"), None] {
        let daemon = f.daemon().clone();
        let s = session.clone();
        let args = request.clone();
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        let waiting =
            tokio::spawn(
                async move { daemon.request_topic_harness_substitute(&s, &args, &c).await },
            );
        wait_until(|| !f.daemon().topic_decisions.locked().is_empty()).await;
        let id = f
            .daemon()
            .topic_decisions
            .locked()
            .keys()
            .next()
            .unwrap()
            .clone();
        if let Some(answer) = answer {
            f.daemon()
                .handle(Request::AnswerQuestion {
                    question_id: id,
                    answer: answer.into(),
                    answers: vec![],
                })
                .await
                .unwrap();
            assert_eq!(waiting.await.unwrap().unwrap()["answer"], answer);
        } else {
            cancel.cancel();
            assert!(waiting.await.unwrap().is_err());
        }
        assert!(f.daemon().topic_decisions.locked().is_empty());
        assert!(f.daemon().topic_substitutes.locked().is_empty());
    }
    assert!(state(&f, &topic.id).attempts.is_empty());
}

#[tokio::test]
async fn topic_unknown_auth_is_ready() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "plain");
    env.set("TOPIC_FIXTURE_AUTH_UNKNOWN", "1");
    let f = Fixture::new(&env);
    script(&f, &env);
    let topic = topic(&f, None, false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    assert_eq!(
        crate::local_harness::harness_availability(HarnessKind::Claude, None)
            .await
            .signed_in,
        None
    );
    assert_eq!(
        f.daemon()
            .run_topic_step_until_done(
                topic.topic_session_id.as_ref().unwrap(),
                &step(
                    &topic.features[0].id,
                    HarnessKind::Claude,
                    StepAccess::Write,
                    &[]
                ),
                &CancellationToken::new()
            )
            .await
            .unwrap()["status"],
        "succeeded"
    );
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
            .run_topic_step_until_done(&s, &a, &CancellationToken::new())
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
        .run_topic_step_until_done(&session, &args, &CancellationToken::new())
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
        .run_topic_step_until_done(&session, &args, &CancellationToken::new())
        .await
        .is_err());
    assert_eq!(state(&f, &topic.id).attempts.len(), 1);
}

#[tokio::test]
async fn topic_shutdown_cancels_live_step_token_and_harness() {
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
    let running = tokio::spawn(async move {
        daemon
            .run_topic_step_until_done(&session, &args, &CancellationToken::new())
            .await
    });
    wait_until(|| {
        std::fs::read_to_string(f.root.join("pid"))
            .ok()
            .and_then(|p| p.parse::<i32>().ok())
            .is_some()
    })
    .await;
    let pid: i32 = std::fs::read_to_string(f.root.join("pid"))
        .unwrap()
        .parse()
        .unwrap();
    let token = f
        .daemon()
        .topic_steps
        .locked()
        .values()
        .next()
        .unwrap()
        .clone();
    f.daemon().request_shutdown();
    assert!(token.is_cancelled());
    assert_eq!(running.await.unwrap().unwrap()["status"], "cancelled");
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
}

#[tokio::test]
async fn topic_interrupt_run_stops_live_harness_and_releases_chat() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "hold");
    let f = Fixture::new(&env);
    script(&f, &env);
    let topic = topic(&f, None, false);
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
    let session = topic.topic_session_id.unwrap();
    f.daemon()
        .handle(Request::InterruptRun {
            session_id: session.clone(),
        })
        .await
        .unwrap();
    settled(&f, &session).await;
    wait_until(
        || unsafe { libc::kill(pid, 0) } == -1 && f.daemon().topic_steps.locked().is_empty(),
    )
    .await;
    assert!(f.daemon().topic_steps.locked().is_empty());
    assert!(!f.daemon().shutdown.is_cancelled());
    server.abort();
}

#[tokio::test]
async fn topic_orchestrator_fixture_unavailable_adapter_and_missing_executable_are_step_failures() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let executable = script(&f, &env);
    env.set("FORMLESS_TOPIC_MUSE_EXE", executable);
    let topic = topic(&f, None, false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let session = topic.topic_session_id.clone().unwrap();
    let result = f
        .daemon()
        .run_topic_step_until_done(
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
        .run_topic_step_until_done(
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
        .unwrap_err();
    assert!(result.to_string().contains("not ready"));
    assert_eq!(state(&f, &topic.id).status, TopicStatus::Running);
    assert_eq!(
        state(&f, &topic.id).features[0].status,
        TopicFeatureStatus::Pending
    );
    assert!(f
        .daemon()
        .run_topic_step_until_done(
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
            .run_topic_step_until_done(&s, &a, &CancellationToken::new())
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
        .run_topic_step_until_done(&session, &args, &CancellationToken::new())
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
                .run_topic_step_until_done(&session, &args, &CancellationToken::new())
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
    let repo = repository(&f);
    let topic = topic(&f, Some(repo.clone()), false);
    let (tx, mut events) = mpsc::channel(256);
    f.daemon().subs.locked().push(Subscriber {
        conn: 97,
        tx: tx.into(),
        filter: Arc::new(StdMutex::new(EventFilter::default())),
        kick: CancellationToken::new(),
        gaps: BTreeMap::new(),
    });
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
    let failed = state(&f, &topic.id);
    assert_eq!(failed.status, TopicStatus::Failed);
    assert_eq!(failed.features[0].status, TopicFeatureStatus::Failed);
    let last = f
        .daemon()
        .with_reader(|st| st.last_run(topic.topic_session_id.as_ref().unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(last.1, "abandoned");
    assert!(failed.features[0]
        .detail
        .as_ref()
        .unwrap()
        .contains("ended with paused"));
    let mut changed = false;
    while let Ok(event) = events.try_recv() {
        if matches!(event,ServerMessage::Event(Event::TopicChanged{topic_id}) if topic_id==topic.id)
        {
            changed = true;
        }
    }
    assert!(changed, "provider failure must publish TopicChanged");
    let another = self::topic(&f, Some(repo), false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&another.id))
        .unwrap();
    f.daemon().cancel_topic_work(&another.id).unwrap();
    server.abort();
    f.daemon()
        .handle(Request::RetryTopicFeature {
            retry: TopicFeatureRetryRequest {
                topic_id: topic.id.clone(),
                feature_id: topic.features[0].id.clone(),
            },
        })
        .await
        .unwrap();
    env.set("TOPIC_FIXTURE_MODE", "done");
    let retry_server = model(
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
    let retried = state(&f, &topic.id);
    assert_eq!(retried.status, TopicStatus::NeedsUser);
    assert_eq!(retried.features[0].status, TopicFeatureStatus::Pending);
    let next = f
        .daemon()
        .with_reader(|st| st.last_run(topic.topic_session_id.as_ref().unwrap()))
        .unwrap()
        .unwrap();
    assert_ne!(next.0, last.0);
    assert_eq!(next.1, "completed");
    retry_server.abort();
}

#[tokio::test]
async fn topic_failed_kickoffs_retire_runs_and_allow_child_approval_after_abandon() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "provider_error");
    let f = Fixture::new(&env);
    let topic = topic(&f, None, false);
    let session = topic.topic_session_id.clone().unwrap();
    let server = model(
        &f,
        &env,
        &topic,
        HarnessKind::Claude,
        HarnessKind::Claude,
        true,
    )
    .await;
    let mut ids = Vec::new();
    for iteration in 0..2 {
        if iteration > 0 {
            f.daemon()
                .handle(Request::RetryTopicFeature {
                    retry: TopicFeatureRetryRequest {
                        topic_id: topic.id.clone(),
                        feature_id: topic.features[0].id.clone(),
                    },
                })
                .await
                .unwrap();
        }
        f.daemon()
            .handle(Request::StartTopicWorkflow {
                topic_id: topic.id.clone(),
            })
            .await
            .unwrap();
        settled(&f, &session).await;
        assert_eq!(state(&f, &topic.id).status, TopicStatus::Failed);
        let last = f
            .daemon()
            .with_reader(|st| st.last_run(&session))
            .unwrap()
            .unwrap();
        assert_eq!(last.1, "abandoned");
        ids.push(last.0);
    }
    f.daemon()
        .handle(Request::AbandonRun {
            session_id: session.clone(),
        })
        .await
        .unwrap();
    let snapshot = f
        .daemon()
        .handle(Request::GetSession {
            id: session.clone(),
        })
        .await
        .unwrap();
    assert_eq!(snapshot["running"], false);
    assert!(snapshot["unfinished_run"].is_null());
    assert_eq!(snapshot["queued"], json!([]));
    assert!(snapshot["last_recap"].is_null());
    let proposal = TopicProposalRequest {
        source_session_id: session.clone(),
        message_ids: vec![topic.features[0].source_message_id.clone()],
        name: "Child after errors".into(),
        workspace: None,
        parent_topic_id: Some(topic.id.clone()),
        outline: vec![],
        features: vec![],
    };
    let child: TopicInfo = serde_json::from_value(
        f.daemon()
            .handle(Request::ProposeTopic { proposal })
            .await
            .unwrap(),
    )
    .unwrap();
    let publishing = f
        .daemon()
        .begin_run_publication(&session, ids.last().unwrap());
    assert!(f
        .daemon()
        .handle(Request::ApproveTopic {
            approval: TopicApprovalRequest {
                topic_id: child.id.clone()
            }
        })
        .await
        .unwrap_err()
        .to_string()
        .contains("finishing"));
    drop(publishing);
    f.daemon()
        .handle(Request::ApproveTopic {
            approval: TopicApprovalRequest {
                topic_id: child.id.clone(),
            },
        })
        .await
        .unwrap();
    for id in ids {
        assert_eq!(
            f.daemon()
                .with_reader(|st| st.run_status(&id))
                .unwrap()
                .as_deref(),
            Some("abandoned")
        );
    }
    server.abort();
}

#[tokio::test]
async fn topic_startup_retires_legacy_failed_kickoffs_but_preserves_resumable_runs() {
    let env = crate::credential_env::EnvGuard::isolated();
    let mut f = Fixture::new(&env);
    let failed = topic(&f, None, false);
    let active = topic(&f, None, false);
    let (old, latest, continuable, ordinary) = f
        .daemon()
        .with_store(|st| {
            st.claim_topic_workflow(&failed.id)?;
            let session = failed.topic_session_id.as_ref().unwrap();
            st.append_message_generated(
                session,
                "user",
                "legacy kickoff",
                None,
                Some("topic_kickoff"),
            )?;
            let old = st.create_run(session)?;
            st.set_run_status(&old, "paused")?;
            st.append_message_generated(
                session,
                "user",
                "another kickoff",
                None,
                Some("topic_kickoff"),
            )?;
            let latest = st.create_run(session)?;
            st.set_run_status(&latest, "paused")?;
            st.set_run_status(&old, "paused")?;
            st.fail_topic_workflow(&failed.id, "legacy provider failure")?;
            st.claim_topic_workflow(&active.id)?;
            let session = active.topic_session_id.as_ref().unwrap();
            st.append_message_generated(
                session,
                "user",
                "resumable kickoff",
                None,
                Some("topic_kickoff"),
            )?;
            let continuable = st.create_run(session)?;
            st.set_run_status(&continuable, "paused")?;
            let ordinary = st.create_run(&f.session)?;
            st.set_run_status(&ordinary, "paused")?;
            Ok((old, latest, continuable, ordinary))
        })
        .unwrap();
    drop(f.daemon.take());
    let restarted = Daemon::new(f.paths.clone()).unwrap();
    for id in [old, latest] {
        assert_eq!(
            restarted
                .with_reader(|st| st.run_status(&id))
                .unwrap()
                .as_deref(),
            Some("abandoned")
        );
    }
    for id in [continuable, ordinary] {
        assert_eq!(
            restarted
                .with_reader(|st| st.run_status(&id))
                .unwrap()
                .as_deref(),
            Some("paused")
        );
    }
}
