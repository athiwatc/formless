//! Fixture tests: private fake official CLI executables and loopback model provider.
use super::*;
use crate::local_harness::HarnessUpdate;
use axum::response::IntoResponse;
use std::os::unix::fs::PermissionsExt;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

fn script(f: &Fixture, env: &crate::credential_env::EnvGuard) -> PathBuf {
    let path = f.root.join("fake-harness");
    std::fs::write(&path,r#"#!/usr/bin/env python3
import sys,os,json,socket,pathlib,subprocess,hashlib,time,shutil
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
if mode in ('report_nonzero','report_dirty_nonzero','report_valid_file_nonzero','report_mutated_file','report_mutated_directory','report_wrong_revision','report_workspace_replaced'):
    outputs=[]
    target=pathlib.Path(os.environ['FORMLESS_TOPIC_OUTPUTS_DIR'])/'reported'
    if mode in ('report_valid_file_nonzero','report_mutated_file'):
        target.write_text('valid bytes')
        outputs=[{'kind':'file','path':str(target),'sha256':hashlib.sha256(target.read_bytes()).hexdigest()}]
    if mode=='report_mutated_directory':
        target.mkdir();outputs=[{'kind':'directory','path':str(target)}]
    if mode=='report_wrong_revision':
        old=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
        pathlib.Path('change.txt').write_text('new commit')
        subprocess.check_call(['git','add','change.txt'])
        subprocess.check_call(['git','-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','new commit'])
        outputs=[{'kind':'git_revision','revision':old}]
    if mode=='report_dirty_nonzero':pathlib.Path('dirty.txt').write_text('uncommitted')
    rpc({'operation':'report_result','status':'succeeded','summary':'reported success','outputs':outputs})
    if mode=='report_mutated_file':target.write_text('changed after report')
    if mode=='report_mutated_directory':target.rmdir();target.symlink_to(pathlib.Path.cwd(),target_is_directory=True)
    if mode=='report_workspace_replaced':
        workspace=pathlib.Path.cwd();os.chdir(workspace.parent);shutil.rmtree(workspace);workspace.symlink_to(target.parent,target_is_directory=True)
    if mode in ('report_nonzero','report_valid_file_nonzero','report_dirty_nonzero'):sys.exit(9)
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
if mode in ('directory','directory_read','directory_merge'):
    prompt=sys.argv[-1];inputs=json.loads(prompt.split('Inputs: ',1)[1].split('\n',1)[0])
    if mode=='directory_merge':
        assert len(inputs)==2
        for item in inputs:
            assert item['kind']=='directory'
            assert list(pathlib.Path(item['path']).glob('result-*.txt'))
        pathlib.Path('combined.txt').write_text(json.dumps(inputs))
    else:
        assert pathlib.Path('seed.txt').exists()
        pathlib.Path('result-'+os.environ['FORMLESS_TOPIC_ATTEMPT_ID']+'.txt').write_text(os.getcwd())
        if pathlib.Path('internal-link').exists():pathlib.Path('internal-link').write_text('private copy')
if mode=='directory':
    gate=pathlib.Path(os.environ['TOPIC_FIXTURE_RELEASE_DIR'])/os.environ['FORMLESS_TOPIC_ATTEMPT_ID']
    gate.with_suffix('.ready').write_text(str(os.getpid()))
    while not gate.with_suffix('.go').exists():time.sleep(.01)
codex='exec' in sys.argv[1:3]
if mode=='substitute_model':
    expected=os.environ['TOPIC_FIXTURE_EXPECT_MODEL']
    if expected:assert sys.argv[sys.argv.index('--model')+1]==expected
    else:assert '--model' not in sys.argv
readonly=('plan' in sys.argv or 'read-only' in sys.argv)
if mode in ('git','git_isolated','git_merge'):
    def git(*args):return subprocess.check_output(['git',*args],stderr=subprocess.STDOUT,text=True).strip()
    if not readonly:
        if mode=='git_merge':
            prompt=sys.argv[-1];inputs=json.loads(prompt.split('Inputs: ',1)[1].split('\n',1)[0])
            trees=json.loads(prompt.split('Git input worktrees: ',1)[1].split('\n',1)[0])
            assert len(inputs)==len(trees)==2
            assert git('rev-parse','HEAD')==inputs[0]['revision']
            for item,tree in zip(inputs,trees):assert git('-C',tree['path'],'rev-parse','HEAD')==item['revision']
            git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','merge','--no-edit',inputs[1]['revision'])
        else:
            filename='result-'+os.environ['FORMLESS_TOPIC_ATTEMPT_ID']+'.txt' if mode=='git_isolated' else 'result.txt'
            pathlib.Path(filename).write_text('fixture change')
            git('add',filename);git('-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-m','fixture result')
    result=git('rev-parse','HEAD')
    if mode=='git_isolated':
        gate=pathlib.Path(os.environ['TOPIC_FIXTURE_RELEASE_DIR'])/os.environ['FORMLESS_TOPIC_ATTEMPT_ID']
        gate.with_suffix('.ready').write_text(str(os.getpid()))
        while not gate.with_suffix('.go').exists():time.sleep(.01)
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
fn attempt_workspace(f: &Fixture, id: &str) -> PathBuf {
    f.daemon()
        .with_reader(|st| {
            let attempt = st.get_topic_attempt(id)?.unwrap();
            Ok(PathBuf::from(
                st.get_session(&attempt.conversation_session_id)?
                    .unwrap()
                    .cwd
                    .unwrap(),
            ))
        })
        .unwrap()
}
fn git(path: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .current_dir(path)
        .args(args)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}
async fn finish_attempt(f: &Fixture, session: &str, id: &str) -> Value {
    let result = f
        .daemon()
        .wait_topic_steps(
            session,
            &json!({"attempt_ids":[id]}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result["event"], "completed", "{result}");
    assert_eq!(result["status"], "succeeded", "{result}");
    result
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
    assert!(message.content.contains(concat!(
        "Available harness kinds: claude, codex, antigravity, acp, muse; ",
        "pi and opencode currently return adapter-not-integrated errors. ",
        "ACP requires acp_agent."
    )));
    assert!(message
        .content
        .contains("Inputs are exact artifact references from any finalized output in this topic."));
    assert!(message
        .content
        .contains("Combine inputs only when intended, in an isolated step."));
    assert!(message
        .content
        .contains("Never push or modify the source workspace or its branch."));
    assert!(!message.content.contains("selected feature's outputs"));
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
        if dirty {
            assert!(done["summary"].as_str().unwrap().contains("clean worktree"));
            assert!(done["outputs"].as_array().unwrap().is_empty());
            assert!(state(&f, &topic.id).features[0].outputs.is_empty());
        } else {
            assert_eq!(done["summary"], "provisional result");
            assert_eq!(
                state(&f, &topic.id).features[0].status,
                TopicFeatureStatus::Pending
            );
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
async fn topic_harness_errors_after_success_still_require_daemon_output_validation() {
    let env = crate::credential_env::EnvGuard::isolated();
    for (mode, error) in [
        ("report_nonzero", None),
        ("report_valid_file_nonzero", None),
        ("report_dirty_nonzero", Some("clean worktree")),
        ("report_mutated_file", Some("SHA-256")),
        (
            "report_mutated_directory",
            Some("directory output must be inside"),
        ),
        (
            "report_wrong_revision",
            Some("does not match workspace HEAD"),
        ),
        (
            "report_workspace_replaced",
            Some("Directory Write workspace must remain"),
        ),
    ] {
        env.set("TOPIC_FIXTURE_MODE", mode);
        let f = Fixture::new(&env);
        script(&f, &env);
        let workspace = if matches!(mode, "report_dirty_nonzero" | "report_wrong_revision") {
            Some(repository(&f))
        } else if mode == "report_workspace_replaced" {
            let source = f.root.join("source");
            std::fs::create_dir(&source).unwrap();
            std::fs::write(source.join("seed.txt"), "source").unwrap();
            Some(source)
        } else {
            None
        };
        let topic = topic(&f, workspace, false);
        f.daemon()
            .with_store(|st| st.claim_topic_workflow(&topic.id))
            .unwrap();
        let ipc = tokio::spawn(f.daemon().clone().serve());
        wait_until(|| f.paths.socket_path().exists()).await;
        let done = f
            .daemon()
            .run_topic_step_until_done(
                topic.topic_session_id.as_ref().unwrap(),
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
        if let Some(error) = error {
            assert_eq!(done["status"], "failed", "{mode}: {done}");
            assert!(
                done["summary"].as_str().unwrap().contains(error),
                "{mode}: {done}"
            );
            assert!(done["outputs"].as_array().unwrap().is_empty());
            assert!(state(&f, &topic.id).features[0].outputs.is_empty());
        } else {
            assert_eq!(done["status"], "succeeded", "{mode}: {done}");
            assert_eq!(done["summary"], "reported success");
            assert_eq!(
                done["outputs"].as_array().unwrap().len(),
                usize::from(mode == "report_valid_file_nonzero")
            );
        }
        assert!(state(&f, &topic.id).features[0].active_attempt_id.is_none());
        if mode == "report_workspace_replaced" {
            assert_eq!(
                std::fs::read_to_string(f.root.join("source/seed.txt")).unwrap(),
                "source"
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
    env.set("FORMLESS_TOPIC_ACP_EXE", f.root.join("missing-acp"));
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
    let ids = crate::local_harness::acp_agents::agent_ids();
    assert_eq!(values.as_array().unwrap().len(), 6 + ids.len());
    for id in ids {
        let entries: Vec<_> = values
            .as_array()
            .unwrap()
            .iter()
            .filter(|value| value["kind"] == "acp" && value["acp_agent"] == *id)
            .collect();
        assert_eq!(entries.len(), 1, "{id}");
        assert_eq!(entries[0]["installed"], false);
    }
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
async fn topic_substitute_clears_original_model_or_uses_the_suggested_model() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "substitute_model");
    let f = Fixture::new(&env);
    script(&f, &env);
    env.set("FORMLESS_TOPIC_CLAUDE_EXE", f.root.join("missing"));
    let topic = topic(&f, None, false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let session = topic.topic_session_id.as_ref().unwrap();
    let cancel = CancellationToken::new();
    let mut args = step(
        &topic.features[0].id,
        HarnessKind::Claude,
        StepAccess::Write,
        &[],
    );
    args["model"] = json!("claude-source-model");
    for expected in [None, Some("codex-selected-model")] {
        env.set("TOPIC_FIXTURE_EXPECT_MODEL", expected.unwrap_or(""));
        let mut suggestion = json!({"harness":"codex"});
        if let Some(model) = expected {
            suggestion["model"] = json!(model);
        }
        let request = json!({"feature_id":topic.features[0].id,"step_title":args["title"],"wanted":{"harness":"claude"},"substitute":suggestion,"reason":"Use an available harness"});
        let daemon = f.daemon().clone();
        let session = session.clone();
        let c = cancel.clone();
        let waiting = tokio::spawn(async move {
            daemon
                .request_topic_harness_substitute(&session, &request, &c)
                .await
        });
        wait_until(|| !f.daemon().topic_decisions.locked().is_empty()).await;
        let id = f
            .daemon()
            .topic_decisions
            .locked()
            .keys()
            .next()
            .unwrap()
            .clone();
        f.daemon()
            .handle(Request::AnswerQuestion {
                question_id: id,
                answer: "Use Codex".into(),
                answers: vec![],
            })
            .await
            .unwrap();
        waiting.await.unwrap().unwrap();
        let result = f
            .daemon()
            .run_topic_step_until_done(topic.topic_session_id.as_ref().unwrap(), &args, &cancel)
            .await
            .unwrap();
        assert_eq!(result["status"], "succeeded", "{result}");
        assert_eq!(result["model"], json!(expected));
        assert_eq!(result["harness"], "codex");
        assert!(result["acp_agent"].is_null());
        assert!(f.daemon().topic_substitutes.locked().is_empty());
    }
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

#[tokio::test]
async fn topic_terminal_failure_closes_limit_question_and_does_not_block_idle() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "plain");
    let f = Fixture::new(&env);
    script(&f, &env);
    let topic = topic(&f, None, false);
    f.daemon()
        .with_store(|st| {
            st.claim_topic_workflow(&topic.id)?;
            st.set_topic_limits(
                &topic.id,
                &TopicLimits {
                    max_retries_per_step: 0,
                    max_attempts_per_topic: 0,
                },
            )
        })
        .unwrap();
    let session = topic.topic_session_id.as_ref().unwrap();
    assert!(f
        .daemon()
        .run_topic_step(
            session,
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
    assert_eq!(f.daemon().topic_decisions.locked().len(), 1);
    assert!(f.daemon().topic_work_in_flight());
    let id = f
        .daemon()
        .topic_decisions
        .locked()
        .keys()
        .next()
        .unwrap()
        .clone();
    let (tx, mut rx) = mpsc::channel(64);
    f.daemon().subs.locked().push(Subscriber {
        conn: 981,
        tx: tx.into(),
        filter: Arc::new(StdMutex::new(EventFilter::default())),
        kick: CancellationToken::new(),
        gaps: BTreeMap::new(),
    });
    f.daemon()
        .finish_topic_feature_tool(
            session,
            &json!({"feature_id":topic.features[0].id,"reason":"User is stopping this feature"}),
            false,
        )
        .unwrap();
    assert_eq!(state(&f, &topic.id).status, TopicStatus::Failed);
    assert!(f.daemon().topic_decisions.locked().is_empty());
    assert!(!f.daemon().topic_work_in_flight());
    assert!(!f
        .daemon()
        .pending_attention
        .locked()
        .iter()
        .any(|event| matches!(event,Event::UserQuestion{question_id,..} if question_id==&id)));
    let mut closed = false;
    while let Ok(message) = rx.try_recv() {
        if matches!(message,ServerMessage::Event(Event::UserQuestionClosed{question_id,answered_by:None}) if question_id==id)
        {
            closed = true;
        }
    }
    assert!(closed);
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
async fn topic_directory_writes_are_isolated_and_merge_receives_both_copies() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "directory");
    let f = Fixture::new(&env);
    script(&f, &env);
    let source = f.root.join("source directory");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("seed.txt"), "source stays unchanged").unwrap();
    std::os::unix::fs::symlink("seed.txt", source.join("internal-link")).unwrap();
    let topic = topic(&f, Some(source.clone()), true);
    assert_eq!(topic.workspace_kind, WorkspaceKind::Directory);
    std::fs::create_dir(source.join(".git")).unwrap();
    std::fs::write(source.join(".git").join("not-a-repo"), "skip").unwrap();
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let session = topic.topic_session_id.as_ref().unwrap();
    let cancel = CancellationToken::new();
    let mut ids = Vec::new();
    for feature in &topic.features {
        let started = f
            .daemon()
            .run_topic_step(
                session,
                &step(&feature.id, HarnessKind::Claude, StepAccess::Write, &[]),
                &cancel,
            )
            .await
            .unwrap();
        ids.push(started["attempt_id"].as_str().unwrap().to_owned());
    }
    wait_until(|| {
        ids.iter()
            .all(|id| f.root.join(format!("{id}.ready")).exists())
    })
    .await;
    let a = attempt_workspace(&f, &ids[0]);
    let b = attempt_workspace(&f, &ids[1]);
    assert_ne!(a, b);
    for workspace in [&a, &b] {
        assert!(!workspace.join(".git").exists());
        assert_eq!(
            std::fs::read_to_string(workspace.join("internal-link")).unwrap(),
            "private copy"
        );
        assert!(workspace
            .join("internal-link")
            .canonicalize()
            .unwrap()
            .starts_with(workspace));
    }
    assert_eq!(
        std::fs::read_to_string(source.join("seed.txt")).unwrap(),
        "source stays unchanged"
    );
    for id in &ids {
        std::fs::write(f.root.join(format!("{id}.go")), "").unwrap();
    }
    for id in &ids {
        finish_attempt(&f, session, id).await;
    }
    let state = state(&f, &topic.id);
    let inputs: Vec<_> = state
        .features
        .iter()
        .map(|feature| {
            feature
                .outputs
                .iter()
                .find(|artifact| matches!(artifact, TopicArtifact::Directory { .. }))
                .unwrap()
                .clone()
        })
        .collect();
    env.set("TOPIC_FIXTURE_MODE", "directory_merge");
    let mut merge = step(
        &topic.features[0].id,
        HarnessKind::Codex,
        StepAccess::Write,
        &inputs,
    );
    merge["title"] = json!("Combine independent copies");
    let result = f
        .daemon()
        .run_topic_step_until_done(session, &merge, &cancel)
        .await
        .unwrap();
    assert_eq!(result["status"], "succeeded", "{result}");
    let workspace = attempt_workspace(&f, result["id"].as_str().unwrap());
    let received: Value =
        serde_json::from_str(&std::fs::read_to_string(workspace.join("combined.txt")).unwrap())
            .unwrap();
    assert_eq!(received.as_array().unwrap().len(), 2);
    assert_ne!(received[0]["path"], received[1]["path"]);
    for item in received.as_array().unwrap() {
        assert!(Path::new(item["path"].as_str().unwrap()).starts_with(workspace.parent().unwrap()));
    }
    assert_eq!(
        std::fs::read_to_string(source.join("seed.txt")).unwrap(),
        "source stays unchanged"
    );
    assert_eq!(std::fs::read_dir(&source).unwrap().count(), 3);
}

#[tokio::test]
async fn topic_directory_read_only_empty_and_symlink_escape_cases() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "directory_read");
    let f = Fixture::new(&env);
    script(&f, &env);
    let source = f.root.join("source");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("seed.txt"), "original").unwrap();
    let read = topic(&f, Some(source.clone()), false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&read.id))
        .unwrap();
    let result = f
        .daemon()
        .run_topic_step_until_done(
            read.topic_session_id.as_ref().unwrap(),
            &step(
                &read.features[0].id,
                HarnessKind::Codex,
                StepAccess::ReadOnly,
                &[],
            ),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result["status"], "succeeded", "{result}");
    assert!(
        std::fs::read_dir(attempt_workspace(&f, result["id"].as_str().unwrap()))
            .unwrap()
            .count()
            > 1
    );
    assert_eq!(std::fs::read_dir(&source).unwrap().count(), 1);
    assert_eq!(
        std::fs::read_to_string(source.join("seed.txt")).unwrap(),
        "original"
    );
    env.set("TOPIC_FIXTURE_MODE", "plain");
    let empty = f.root.join("empty");
    std::fs::create_dir(&empty).unwrap();
    let empty = topic(&f, Some(empty), false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&empty.id))
        .unwrap();
    let result = f
        .daemon()
        .run_topic_step_until_done(
            empty.topic_session_id.as_ref().unwrap(),
            &step(
                &empty.features[0].id,
                HarnessKind::Claude,
                StepAccess::Write,
                &[],
            ),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(result["outputs"][0]["kind"], "directory");
    let outside = f.root.join("outside.txt");
    std::fs::write(&outside, "not copied").unwrap();
    std::os::unix::fs::symlink(&outside, source.join("escape")).unwrap();
    let bad = topic(&f, Some(source.clone()), false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&bad.id))
        .unwrap();
    let result = f
        .daemon()
        .run_topic_step_until_done(
            bad.topic_session_id.as_ref().unwrap(),
            &step(
                &bad.features[0].id,
                HarnessKind::Claude,
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
        .contains("symlink escapes"));
    assert!(std::fs::symlink_metadata(
        attempt_workspace(&f, result["id"].as_str().unwrap()).join("escape")
    )
    .is_err());
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "not copied");
}

#[tokio::test]
async fn topic_directory_preserves_dangling_internal_links_and_rejects_lexical_escapes() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "plain");
    let f = Fixture::new(&env);
    script(&f, &env);
    for (name, relative, accepted) in [
        ("internal", Some("../build/./generated.txt"), true),
        ("escape", Some("../../missing/generated.txt"), false),
        ("absolute", None, false),
    ] {
        let source = f.root.join(name);
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(source.join("links")).unwrap();
        let link = relative
            .map(PathBuf::from)
            .unwrap_or_else(|| source.join("missing/generated.txt"));
        std::os::unix::fs::symlink(&link, source.join("links/pending")).unwrap();
        let topic = topic(&f, Some(source.clone()), false);
        f.daemon()
            .with_store(|st| st.claim_topic_workflow(&topic.id))
            .unwrap();
        let result = f
            .daemon()
            .run_topic_step_until_done(
                topic.topic_session_id.as_ref().unwrap(),
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
        let copied = attempt_workspace(&f, result["id"].as_str().unwrap()).join("links/pending");
        if accepted {
            assert_eq!(result["status"], "succeeded", "{result}");
            assert!(std::fs::symlink_metadata(&copied)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(
                std::fs::read_link(&copied).unwrap().as_os_str(),
                link.as_os_str()
            );
            assert!(!copied.exists());
        } else {
            assert_eq!(result["status"], "failed", "{result}");
            assert!(result["summary"]
                .as_str()
                .unwrap()
                .contains("symlink escapes"));
            assert!(std::fs::symlink_metadata(&copied).is_err());
        }
        assert_eq!(
            std::fs::read_link(source.join("links/pending"))
                .unwrap()
                .as_os_str(),
            link.as_os_str()
        );
        assert!(!source.join("links/pending").exists());
    }
}

#[tokio::test]
async fn topic_git_writes_have_distinct_branches_and_explicit_merge_gets_both_revisions() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "git_isolated");
    let f = Fixture::new(&env);
    script(&f, &env);
    let repo = repository(&f);
    let original = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    let branch = git(&repo, &["symbolic-ref", "HEAD"]).unwrap();
    let topic = topic(&f, Some(repo.clone()), true);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let session = topic.topic_session_id.as_ref().unwrap();
    let cancel = CancellationToken::new();
    let mut ids = Vec::new();
    for feature in &topic.features {
        let started = f
            .daemon()
            .run_topic_step(
                session,
                &step(&feature.id, HarnessKind::Claude, StepAccess::Write, &[]),
                &cancel,
            )
            .await
            .unwrap();
        ids.push(started["attempt_id"].as_str().unwrap().to_owned());
    }
    wait_until(|| {
        ids.iter()
            .all(|id| f.root.join(format!("{id}.ready")).exists())
    })
    .await;
    assert_ne!(
        attempt_workspace(&f, &ids[0]),
        attempt_workspace(&f, &ids[1])
    );
    for id in &ids {
        assert_eq!(
            git(&attempt_workspace(&f, id), &["branch", "--show-current"]).unwrap(),
            format!("formless/topic/{}/{id}", topic.id)
        );
        std::fs::write(f.root.join(format!("{id}.go")), "").unwrap();
    }
    for id in &ids {
        finish_attempt(&f, session, id).await;
    }
    let saved = state(&f, &topic.id);
    let inputs: Vec<_> = saved
        .features
        .iter()
        .map(|feature| {
            feature
                .outputs
                .iter()
                .find(|artifact| matches!(artifact, TopicArtifact::GitRevision { .. }))
                .unwrap()
                .clone()
        })
        .collect();
    assert_ne!(inputs[0], inputs[1]);
    env.set("TOPIC_FIXTURE_MODE", "git_merge");
    let mut merge = step(
        &topic.features[0].id,
        HarnessKind::Codex,
        StepAccess::Write,
        &inputs,
    );
    merge["title"] = json!("Combine exact revisions");
    merge["instructions"] = json!(
        "Merge both selected revisions in this private branch; never push or change the source."
    );
    let result = f
        .daemon()
        .run_topic_step_until_done(session, &merge, &cancel)
        .await
        .unwrap();
    assert_eq!(result["status"], "succeeded", "{result}");
    let merged = result["outputs"][0]["revision"].as_str().unwrap();
    for input in inputs {
        let TopicArtifact::GitRevision { revision } = input else {
            unreachable!()
        };
        git(&repo, &["merge-base", "--is-ancestor", &revision, merged]).unwrap();
    }
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]).unwrap(), original);
    assert_eq!(git(&repo, &["symbolic-ref", "HEAD"]).unwrap(), branch);
    assert!(git(&repo, &["status", "--porcelain"]).unwrap().is_empty());
    for id in ids {
        assert!(!attempt_workspace(&f, &id).exists());
        assert!(git(
            &repo,
            &[
                "rev-parse",
                &format!("refs/heads/formless/topic/{}/{id}", topic.id)
            ]
        )
        .is_ok());
    }
}

#[tokio::test]
async fn topic_directory_symlink_chains_follow_existing_prefixes_before_parent_components() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "plain");
    let f = Fixture::new(&env);
    script(&f, &env);
    let victim = f.root.join("victim");
    std::fs::write(&victim, "outside").unwrap();
    for (name, hop, link, accepted) in [
        ("escape", ".", "hop/../victim", false),
        ("dangling_escape", ".", "hop/../absent-victim", false),
        ("internal", "inner", "hop/../seed.txt", true),
        ("dangling_internal", "inner", "hop/../missing/output", true),
        ("reentry", "inner", "missing/../hop/../../victim", false),
    ] {
        let source = f.root.join(name);
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(source.join("inner")).unwrap();
        std::fs::write(source.join("seed.txt"), "inside").unwrap();
        std::os::unix::fs::symlink(hop, source.join("hop")).unwrap();
        std::os::unix::fs::symlink(link, source.join("probe")).unwrap();
        let topic = topic(&f, Some(source.clone()), false);
        f.daemon()
            .with_store(|st| st.claim_topic_workflow(&topic.id))
            .unwrap();
        let result = f
            .daemon()
            .run_topic_step_until_done(
                topic.topic_session_id.as_ref().unwrap(),
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
        let copied = attempt_workspace(&f, result["id"].as_str().unwrap()).join("probe");
        if accepted {
            assert_eq!(result["status"], "succeeded", "{name}: {result}");
            assert_eq!(
                std::fs::read_link(&copied).unwrap().as_os_str(),
                std::ffi::OsStr::new(link)
            );
            if name == "internal" {
                assert_eq!(std::fs::read_to_string(&copied).unwrap(), "inside");
            } else {
                assert!(!copied.exists());
            }
        } else {
            assert_eq!(result["status"], "failed", "{name}: {result}");
            assert!(result["summary"]
                .as_str()
                .unwrap()
                .contains("symlink escapes"));
            assert!(std::fs::symlink_metadata(copied).is_err());
        }
        assert_eq!(
            std::fs::read_to_string(source.join("seed.txt")).unwrap(),
            "inside"
        );
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "outside");
    }
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
    env.set("FORMLESS_TOPIC_OPENCODE_EXE", executable);
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
                HarnessKind::OpenCode,
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

async fn run_muse_fixture(shutdown: bool) {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let path = f.root.join("fake-muse");
    std::fs::write(&path, r#"#!/usr/bin/env python3
import json, os, sys, time, uuid
if sys.argv[1:] == ["--version"]:
    print("Muse Code 1.4.24")
    sys.exit()
if sys.argv[1:] == ["usage", "--json"]:
    print(json.dumps({"schemaVersion":1,"allowance":{"tier":"fixture"}}))
    sys.exit()
session, turn = str(uuid.uuid4()), str(uuid.uuid4())
def receive(method):
    request = json.loads(sys.stdin.readline())
    assert request["method"] == method
    return request
def respond(request, result):
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
def notify(method, **params):
    print(json.dumps({"jsonrpc": "2.0", "method": method, "params": {"sessionId":session, **params}}), flush=True)
assert "--trust-workspace" in sys.argv and "--disable-write" in sys.argv and "--disable-shell" in sys.argv
assert "FORMLESS_TOPIC_CAPABILITY" not in os.environ
respond(receive("initialize"), {"schema":{"version":1}, "sessionDurability":"ephemeral", "grantedCapabilities":["sessionMcp"]})
receive("initialized")
request = receive("session/start")
assert request["params"]["workspaceRoot"] == os.getcwd()
bridge, = request["params"]["config"]["mcpServers"].values()
assert bridge["mode"] == "required" and bridge["env"]["FORMLESS_TOPIC_CAPABILITY"]
respond(request, {"session":{"sessionId":session, "path":"", "approvalMode":{"mode":"promptUnmatched"}}})
request = receive("turn/start")
respond(request, {"status":"accepted", "disposition":"started", "commandId":request["params"]["commandId"], "turnId":turn})
if fixture_pid := os.environ.get("TOPIC_FIXTURE_MUSE_PID"):
    with open(fixture_pid, "w") as marker:
        marker.write(str(os.getpid()))
    time.sleep(120)
item = {"itemId":"message", "kind":"agentMessage", "turnId":turn, "messagePhase":"finalAnswer"}
notify("item/started", item=item)
notify("item/delta", itemId="message", field="text", delta="Muse ")
notify("item/delta", itemId="message", field="text", delta="fixture complete")
notify("item/completed", item={**item, "text":"Muse fixture complete"})
notify("turn/completed", turnId=turn, terminal="completed")
assert sys.stdin.readline() == ""
"#).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    env.set("FORMLESS_TOPIC_MUSE_EXE", &path);
    let pid_path = f.root.join("muse-pid");
    if shutdown {
        env.set("TOPIC_FIXTURE_MUSE_PID", &pid_path);
    }
    let topic = topic(&f, None, false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let session = topic.topic_session_id.clone().unwrap();
    let args = step(
        &topic.features[0].id,
        HarnessKind::Muse,
        StepAccess::ReadOnly,
        &[],
    );
    let daemon = f.daemon().clone();
    let (tx, mut events) = mpsc::channel(64);
    daemon.subs.locked().push(Subscriber {
        conn: 99,
        tx: tx.into(),
        filter: Arc::new(StdMutex::new(EventFilter::default())),
        kick: CancellationToken::new(),
        gaps: BTreeMap::new(),
    });
    let task = tokio::spawn(async move {
        daemon
            .run_topic_step_until_done(&session, &args, &CancellationToken::new())
            .await
    });
    let (permission_id, options) = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let ServerMessage::Event(Event::HarnessPermissionRequested {
                permission_id,
                harness,
                kind,
                options,
                ..
            }) = events.recv().await.unwrap()
            {
                assert_eq!(harness, HarnessKind::Muse);
                assert_eq!(kind, "trust_workspace");
                break (permission_id, options);
            }
        }
    })
    .await
    .unwrap();
    let allow_once = options
        .into_iter()
        .find(|option| option.kind == formless_protocol::HarnessPermissionOptionKind::AllowOnce)
        .unwrap();
    f.daemon()
        .handle(Request::HarnessPermissionDecision {
            permission_id,
            option_id: Some(allow_once.option_id),
        })
        .await
        .unwrap();
    let pid = if shutdown {
        wait_until(|| std::fs::read_to_string(&pid_path).is_ok_and(|pid| !pid.is_empty())).await;
        let pid = std::fs::read_to_string(&pid_path)
            .unwrap()
            .parse::<i32>()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
        f.daemon().shutdown.cancel();
        Some(pid)
    } else {
        None
    };
    let result = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    if let Some(pid) = pid {
        assert_eq!(result["status"], "cancelled", "{result}");
        assert_eq!(result["summary"], "Muse Code run cancelled");
        wait_until(|| unsafe { libc::kill(pid, 0) } == -1).await;
        return;
    }
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(result["summary"], "Muse fixture complete");
    let conversation = result["conversation_session_id"].as_str().unwrap();
    let messages = f
        .daemon()
        .with_reader(|st| st.list_messages(conversation))
        .unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.role == "assistant")
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        ["Muse fixture complete"]
    );
}

#[tokio::test]
async fn topic_orchestrator_fixture_muse_grants_one_run_trust_and_streams() {
    run_muse_fixture(false).await;
}

#[tokio::test]
async fn topic_orchestrator_fixture_shutdown_stops_active_muse_step() {
    run_muse_fixture(true).await;
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

fn agy_script(f: &Fixture, env: &crate::credential_env::EnvGuard) {
    let path = f.root.join("fake-agy");
    std::fs::write(
        &path,
        r#"#!/usr/bin/env python3
import sys,os,json,subprocess,pathlib
if sys.argv[1:2]==['models']:print('gemini-fixture');sys.exit()
assert sys.argv[1:3]==['acp','--disable-telemetry'],sys.argv
assert 'FORMLESS_TOPIC_CAPABILITY' not in os.environ
mode=os.environ.get('TOPIC_FIXTURE_MODE','permission')
def send(m):sys.stdout.write(json.dumps(m)+'\n');sys.stdout.flush()
def read():
    line=sys.stdin.readline()
    if not line:sys.exit(0)
    return json.loads(line)
while True:
    m=read();method=m.get('method')
    if method=='initialize':send({'jsonrpc':'2.0','id':m['id'],'result':{'protocolVersion':1,'agentInfo':{'name':'Antigravity ACP Server','version':'1.5.14'},'authMethods':[]}})
    elif method=='session/new':
        bridge=m['params']['mcpServers'][0]
        assert any(e['name']=='FORMLESS_TOPIC_CAPABILITY' and e['value'] for e in bridge['env'])
        send({'jsonrpc':'2.0','id':m['id'],'result':{'sessionId':'agy-1'}})
    elif method=='session/prompt':
        prompt=m['id']
        if mode=='hold':
            subprocess.Popen(['sleep','120'])
            pathlib.Path(os.environ['TOPIC_FIXTURE_PID']).write_text(str(os.getpid()))
            while read().get('method')!='session/cancel':pass
            send({'jsonrpc':'2.0','id':prompt,'result':{'stopReason':'cancelled'}});continue
        send({'jsonrpc':'2.0','id':900,'method':'session/request_permission','params':{'sessionId':'agy-1','toolCall':{'toolCallId':'call-1','kind':'edit','title':'Fixture write'},'options':[{'optionId':'allow','name':'Allow once','kind':'allow_once'},{'optionId':'deny','name':'Reject','kind':'reject_once'}]}})
        outcome=read()['result']['outcome']
        if outcome.get('optionId')=='allow':
            send({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'agy-1','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'fixture complete'}}}})
            send({'jsonrpc':'2.0','id':prompt,'result':{'stopReason':'end_turn'}})
        else:send({'jsonrpc':'2.0','id':prompt,'result':{'stopReason':'cancelled'}})
"#,
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    env.set("FORMLESS_TOPIC_ANTIGRAVITY_EXE", &path);
    env.set("TOPIC_FIXTURE_PID", f.root.join("pid"));
}

fn gemini_acp_script(f: &Fixture, env: &crate::credential_env::EnvGuard, python: &Path) -> PathBuf {
    let path = f.root.join("fake-gemini-acp");
    std::fs::write(
        &path,
        r#"#!/usr/bin/env python3
import sys,os,json,subprocess,pathlib
assert sys.argv[1:] in (['--acp'],['--acp','--approval-mode','plan']),sys.argv
assert 'FORMLESS_TOPIC_CAPABILITY' not in os.environ
with pathlib.Path(os.environ['TOPIC_FIXTURE_TRUSTED_STARTS']).open('a') as starts:starts.write('trusted\n')
mode=os.environ.get('TOPIC_FIXTURE_MODE','allow')
def send(message):sys.stdout.write(json.dumps(message)+'\n');sys.stdout.flush()
def read():
    line=sys.stdin.readline()
    if not line:sys.exit(0)
    return json.loads(line)
while True:
    message=read();method=message.get('method')
    if method=='initialize':
        send({'jsonrpc':'2.0','id':message['id'],'result':{'protocolVersion':1,'agentInfo':{'name':'gemini-cli','version':'fixture'}}})
    elif method=='session/new':
        servers=message['params']['mcpServers']
        if servers:
            assert servers[0]['name']=='formless'
            assert any(item['name']=='FORMLESS_TOPIC_CAPABILITY' and item['value'] for item in servers[0]['env'])
        else:
            link=os.environ.get('TOPIC_FIXTURE_OVERRIDE_LINK')
            if link:
                pathlib.Path(link).unlink()
                pathlib.Path(link).symlink_to(os.environ['TOPIC_FIXTURE_OVERRIDE_TARGET'])
            selected=os.environ.get('TOPIC_FIXTURE_SELECTED_EXECUTABLE')
            if selected:pathlib.Path(selected).unlink()
        send({'jsonrpc':'2.0','id':message['id'],'result':{'sessionId':'gemini-topic'}})
    elif method=='session/set_mode':
        send({'jsonrpc':'2.0','id':message['id'],'result':{}})
    elif method=='session/prompt':
        prompt=message['id']
        pathlib.Path(os.environ['TOPIC_FIXTURE_PID']).write_text(str(os.getpid()))
        if mode=='hold':
            child=subprocess.Popen(['/bin/sleep','120'])
            pathlib.Path(os.environ['TOPIC_FIXTURE_CHILD_PID']).write_text(str(child.pid))
            while read().get('method')!='session/cancel':pass
            send({'jsonrpc':'2.0','id':prompt,'result':{'stopReason':'cancelled'}})
            continue
        if mode=='complete':
            send({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'gemini-topic','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'Gemini fixture completed'}}}})
            send({'jsonrpc':'2.0','id':prompt,'result':{'stopReason':'end_turn'}})
            continue
        send({'jsonrpc':'2.0','id':900,'method':'session/request_permission','params':{'sessionId':'gemini-topic','toolCall':{'toolCallId':'gemini-call','kind':'write','title':'Gemini fixture permission'},'options':[{'optionId':'allow','name':'Allow once','kind':'allow_once'},{'optionId':'deny','name':'Reject','kind':'reject_once'}]}})
        outcome=read()['result']['outcome']
        if outcome.get('optionId')=='allow':
            send({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'gemini-topic','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'Gemini fixture completed'}}}})
            send({'jsonrpc':'2.0','id':prompt,'result':{'stopReason':'end_turn'}})
        else:
            send({'jsonrpc':'2.0','id':prompt,'result':{'stopReason':'cancelled'}})
"#,
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let bin = f.root.join("acp-bin");
    std::fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink(python, bin.join("python3")).unwrap();
    env.set("TOPIC_FIXTURE_PID", f.root.join("gemini-pid"));
    env.set("TOPIC_FIXTURE_CHILD_PID", f.root.join("gemini-child-pid"));
    env.set("TOPIC_FIXTURE_TRUSTED_STARTS", f.root.join("gemini-starts"));
    env.set(
        "TOPIC_FIXTURE_DECOY_MARKER",
        f.root.join("gemini-decoy-started"),
    );
    env.set("PATH", &bin);
    path
}

fn pid_is_running(pid: i32) -> bool {
    #[cfg(target_os = "linux")]
    if std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(") ")
                .map(|(_, rest)| rest.starts_with('Z'))
        })
        == Some(true)
    {
        return false;
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

struct CwdFixtureDir(PathBuf);

impl Drop for CwdFixtureDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn add_workspace_gemini_decoy(repo: &Path, relative_bin: &Path) {
    let bin = repo.join(relative_bin);
    std::fs::create_dir_all(&bin).unwrap();
    let executable = bin.join("gemini");
    std::fs::write(
        &executable,
        "#!/bin/sh\nprintf 'decoy' > \"$TOPIC_FIXTURE_DECOY_MARKER\"\nexit 97\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let relative = relative_bin.join("gemini");
    git(repo, &["add", relative.to_str().unwrap()]).unwrap();
    git(
        repo,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "workspace ACP decoy",
        ],
    )
    .unwrap();
}

#[tokio::test]
async fn topic_gemini_uses_daemon_resolved_executable_over_workspace_path_decoy() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.remove("FORMLESS_TOPIC_ACP_EXE");
    env.set("TOPIC_FIXTURE_MODE", "complete");
    env.set("TOPIC_FIXTURE_OVERRIDE_LINK", "");
    env.set("TOPIC_FIXTURE_SELECTED_EXECUTABLE", "");
    let output = std::process::Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let python = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let f = Fixture::new(&env);
    let relative_root = PathBuf::from("target").join(f.root.file_name().unwrap());
    let relative_bin = relative_root.join("bin");
    let cwd_root = std::env::current_dir().unwrap().join(&relative_root);
    let _cwd_fixture = CwdFixtureDir(cwd_root.clone());
    std::fs::create_dir_all(cwd_root.join("bin")).unwrap();

    let repo = repository(&f);
    add_workspace_gemini_decoy(&repo, &relative_bin);
    let executable = gemini_acp_script(&f, &env, &python);
    std::os::unix::fs::symlink(&executable, cwd_root.join("bin/gemini")).unwrap();
    env.remove("FORMLESS_TOPIC_ACP_EXE");
    env.set(
        "TOPIC_FIXTURE_DECOY_MARKER",
        f.root.join("gemini-decoy-started"),
    );
    env.set(
        "PATH",
        std::env::join_paths([
            relative_bin,
            python.parent().unwrap().to_path_buf(),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ])
        .unwrap(),
    );

    let topic = topic(&f, Some(repo), false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let mut args = step(
        &topic.features[0].id,
        HarnessKind::Acp,
        StepAccess::ReadOnly,
        &[],
    );
    args["acp_agent"] = json!("gemini");
    let result = f
        .daemon()
        .run_topic_step_until_done(
            topic.topic_session_id.as_deref().unwrap(),
            &args,
            &CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(
        std::fs::read_to_string(f.root.join("gemini-starts"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert!(!f.root.join("gemini-decoy-started").exists());
}

#[tokio::test]
async fn topic_acp_missing_resolution_stops_before_launch_with_retry_guidance() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let empty_path = f.root.join("empty-path");
    std::fs::create_dir(&empty_path).unwrap();
    env.set("PATH", &empty_path);
    env.remove("FORMLESS_TOPIC_ACP_EXE");
    let topic = topic(&f, None, false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let mut args = step(
        &topic.features[0].id,
        HarnessKind::Acp,
        StepAccess::Write,
        &[],
    );
    args["acp_agent"] = json!("gemini");

    let error = f
        .daemon()
        .run_topic_step(
            topic.topic_session_id.as_deref().unwrap(),
            &args,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("not ready"), "{error}");
    assert!(
        error.contains("topic_request_harness_substitute"),
        "{error}"
    );
    assert!(!f.root.join("gemini-starts").exists());
}

#[tokio::test]
async fn topic_acp_removed_resolved_executable_does_not_fall_back_to_workspace_decoy() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_MODE", "complete");
    env.set("TOPIC_FIXTURE_OVERRIDE_LINK", "");
    let output = std::process::Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let python = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
    let f = Fixture::new(&env);
    let relative_bin = PathBuf::from("target")
        .join(f.root.file_name().unwrap())
        .join("bin");
    let repo = repository(&f);
    add_workspace_gemini_decoy(&repo, &relative_bin);
    let executable = gemini_acp_script(&f, &env, &python);
    env.set("FORMLESS_TOPIC_ACP_EXE", &executable);
    env.set("TOPIC_FIXTURE_SELECTED_EXECUTABLE", &executable);
    env.set(
        "TOPIC_FIXTURE_DECOY_MARKER",
        f.root.join("gemini-decoy-started"),
    );
    env.set(
        "PATH",
        std::env::join_paths([
            relative_bin,
            python.parent().unwrap().to_path_buf(),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ])
        .unwrap(),
    );
    let topic = topic(&f, Some(repo), false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let mut args = step(
        &topic.features[0].id,
        HarnessKind::Acp,
        StepAccess::ReadOnly,
        &[],
    );
    args["acp_agent"] = json!("gemini");
    let result = f
        .daemon()
        .run_topic_step_until_done(
            topic.topic_session_id.as_deref().unwrap(),
            &args,
            &CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(result["status"], "failed", "{result}");
    assert_eq!(
        std::fs::read_to_string(f.root.join("gemini-starts"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert!(!f.root.join("gemini-decoy-started").exists());
}

#[tokio::test]
async fn topic_orchestrator_fixture_gemini_acp_step_launches_with_permission_and_cancel() {
    let env = crate::credential_env::EnvGuard::isolated();
    let output = std::process::Command::new("python3")
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let python = String::from_utf8(output.stdout).unwrap();
    let python = PathBuf::from(python.trim());
    for mode in ["allow", "hold"] {
        env.set("TOPIC_FIXTURE_MODE", mode);
        env.set("TOPIC_FIXTURE_SELECTED_EXECUTABLE", "");
        let f = Fixture::new(&env);
        let executable = gemini_acp_script(&f, &env, &python);
        if mode == "allow" {
            let decoy = f.root.join("override-decoy");
            std::fs::write(
                &decoy,
                "#!/bin/sh\\nprintf 'decoy' > \\\"$TOPIC_FIXTURE_DECOY_MARKER\\\"\\nexit 97\\n",
            )
            .unwrap();
            std::fs::set_permissions(&decoy, std::fs::Permissions::from_mode(0o700)).unwrap();
            let override_link = f.root.join("gemini-override");
            std::os::unix::fs::symlink(&executable, &override_link).unwrap();
            env.set("TOPIC_FIXTURE_OVERRIDE_LINK", &override_link);
            env.set("TOPIC_FIXTURE_OVERRIDE_TARGET", &decoy);
            env.set("FORMLESS_TOPIC_ACP_EXE", &override_link);
        } else {
            env.set("TOPIC_FIXTURE_OVERRIDE_LINK", "");
            env.set("FORMLESS_TOPIC_ACP_EXE", &executable);
        }
        let topic = topic(&f, None, false);
        f.daemon()
            .with_store(|st| st.claim_topic_workflow(&topic.id))
            .unwrap();
        let session = topic.topic_session_id.clone().unwrap();
        let mut args = step(
            &topic.features[0].id,
            HarnessKind::Acp,
            StepAccess::Write,
            &[],
        );
        args["acp_agent"] = json!("gemini");
        let daemon = f.daemon().clone();
        let task = tokio::spawn(async move {
            daemon
                .run_topic_step_until_done(&session, &args, &CancellationToken::new())
                .await
        });
        let pid_path = f.root.join("gemini-pid");
        if mode == "hold" {
            let child_pid_path = f.root.join("gemini-child-pid");
            wait_until(|| pid_path.exists() && child_pid_path.exists()).await;
            f.daemon()
                .handle(Request::CancelTopic {
                    topic_id: topic.id.clone(),
                })
                .await
                .unwrap();
            let result = task.await.unwrap().unwrap();
            assert_eq!(result["status"], "cancelled", "{result}");
            for path in [pid_path, child_pid_path] {
                let pid: i32 = std::fs::read_to_string(path).unwrap().parse().unwrap();
                wait_until(|| !pid_is_running(pid)).await;
            }
            continue;
        }
        wait_until(|| !f.daemon().harness_permissions.locked().is_empty()).await;
        assert!(pid_path.exists());
        let permission_id = f
            .daemon()
            .harness_permissions
            .locked()
            .keys()
            .next()
            .unwrap()
            .clone();
        f.daemon()
            .handle(Request::HarnessPermissionDecision {
                permission_id,
                option_id: Some("allow".into()),
            })
            .await
            .unwrap();
        let result = task.await.unwrap().unwrap();
        assert_eq!(result["status"], "succeeded", "{result}");
        assert_eq!(result["harness"], "acp");
        assert_eq!(result["acp_agent"], "gemini");
        assert!(result["summary"]
            .as_str()
            .unwrap()
            .contains("Gemini fixture completed"));
        assert!(f.daemon().harness_permissions.locked().is_empty());
        assert_eq!(
            std::fs::read_to_string(f.root.join("gemini-starts"))
                .unwrap()
                .lines()
                .count(),
            2
        );
        assert!(!f.root.join("gemini-decoy-started").exists());
    }
}

#[tokio::test]
async fn topic_orchestrator_fixture_antigravity_step_reaches_acp_with_permissions_and_cancel() {
    let env = crate::credential_env::EnvGuard::isolated();
    for mode in ["allow", "deny", "hold"] {
        env.set(
            "TOPIC_FIXTURE_MODE",
            if mode == "hold" { "hold" } else { "permission" },
        );
        let f = Fixture::new(&env);
        agy_script(&f, &env);
        let topic = topic(&f, None, false);
        f.daemon()
            .with_store(|st| st.claim_topic_workflow(&topic.id))
            .unwrap();
        let session = topic.topic_session_id.clone().unwrap();
        let args = step(
            &topic.features[0].id,
            HarnessKind::Antigravity,
            StepAccess::Write,
            &[],
        );
        let daemon = f.daemon().clone();
        let task = tokio::spawn(async move {
            daemon
                .run_topic_step_until_done(&session, &args, &CancellationToken::new())
                .await
        });
        if mode == "hold" {
            wait_until(|| f.root.join("pid").exists()).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            f.daemon()
                .handle(Request::CancelTopic {
                    topic_id: topic.id.clone(),
                })
                .await
                .unwrap();
            assert_eq!(task.await.unwrap().unwrap()["status"], "cancelled");
            let pid: i32 = std::fs::read_to_string(f.root.join("pid"))
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
            continue;
        }
        wait_until(|| !f.daemon().harness_permissions.locked().is_empty()).await;
        let permission_id = f
            .daemon()
            .harness_permissions
            .locked()
            .keys()
            .next()
            .unwrap()
            .clone();
        f.daemon()
            .handle(Request::HarnessPermissionDecision {
                permission_id,
                option_id: Some(if mode == "allow" { "allow" } else { "deny" }.into()),
            })
            .await
            .unwrap();
        let result = task.await.unwrap().unwrap();
        assert_eq!(
            result["status"],
            if mode == "allow" {
                "succeeded"
            } else {
                "failed"
            },
            "{result}"
        );
        assert!(f.daemon().harness_permissions.locked().is_empty());
    }
}

fn grok_acp_script(f: &Fixture, env: &crate::credential_env::EnvGuard) -> PathBuf {
    let path = f.root.join("fake-grok-acp");
    std::fs::write(
        &path,
        r#"#!/usr/bin/env python3
import json,os,pathlib,subprocess,sys
assert os.environ.get('GROK_DISABLE_AUTOUPDATER')=='1'
def send(value):
    sys.stdout.write(json.dumps(value)+'\n');sys.stdout.flush()
def read():
    line=sys.stdin.readline()
    if not line:sys.exit(0)
    return json.loads(line)
while True:
    message=read();method=message.get('method')
    if method=='initialize':
        send({'jsonrpc':'2.0','id':message['id'],'result':{'protocolVersion':1,'_meta':{'agentVersion':'1.0.92'}}})
    elif method=='session/new':
        send({'jsonrpc':'2.0','id':message['id'],'result':{'sessionId':'grok-topic-fixture'}})
        if not message['params'].get('mcpServers'):sys.exit(0)
    elif method=='session/prompt':
        prompt_id=message['id']
        if os.environ.get('TOPIC_FIXTURE_GROK_MODE')=='hold':
            child=subprocess.Popen(['/bin/sleep','120'])
            pathlib.Path(os.environ['TOPIC_FIXTURE_GROK_PID']).write_text(str(os.getpid()))
            pathlib.Path(os.environ['TOPIC_FIXTURE_GROK_CHILD_PID']).write_text(str(child.pid))
            while read().get('method')!='session/cancel':pass
            send({'jsonrpc':'2.0','id':prompt_id,'result':{'stopReason':'cancelled'}})
            continue
        send({'jsonrpc':'2.0','id':90,'method':'session/request_permission','params':{'sessionId':'grok-topic-fixture','toolCall':{'toolCallId':'grok-call','kind':'execute','title':'Grok fixture tool','_meta':{'x.ai/tool':{'namespace':'grok_build','name':'use_tool'}}},'options':[{'optionId':'allow','name':'Allow once','kind':'allow_once'},{'optionId':'deny','name':'Reject','kind':'reject_once'}]}})
        permission=read()
        assert permission['id']==90 and permission['result']['outcome']['optionId']=='allow'
        send({'jsonrpc':'2.0','id':91,'method':'_x.ai/ask_user_question','params':{'questions':[{'question':'Proceed with the topic fixture?','options':[{'label':'Proceed'},{'label':'Stop'}],'multiSelect':False}]}})
        question=read()
        assert question['id']==91
        reply=question['result']
        assert reply=={'outcome':'accepted','answers':{'Proceed with the topic fixture?':'Proceed'}}
        with open(os.environ['TOPIC_FIXTURE_GROK_REPLY'],'w') as output:json.dump(reply,output)
        send({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'grok-topic-fixture','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'Grok ACP topic fixture completed'}}}})
        send({'jsonrpc':'2.0','id':prompt_id,'result':{'stopReason':'end_turn'}})
        break
"#,
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    env.set("FORMLESS_TOPIC_ACP_EXE", &path);
    env.set("TOPIC_FIXTURE_GROK_REPLY", f.root.join("grok-reply.json"));
    env.set("TOPIC_FIXTURE_GROK_PID", f.root.join("grok-pid"));
    env.set(
        "TOPIC_FIXTURE_GROK_CHILD_PID",
        f.root.join("grok-child-pid"),
    );
    path
}

fn grok_fixture_process_stopped(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } == -1 {
        return std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.split(')')
                .nth(1)
                .is_some_and(|state| matches!(state.trim_start().chars().next(), Some('Z' | 'X')))
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn grok_fixture_pid(path: &std::path::Path) -> Option<i32> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse::<i32>()
        .ok()
        .filter(|pid| *pid > 0)
}

struct GrokFixtureProcessGuard {
    cancel: CancellationToken,
    agent_pid_path: PathBuf,
    child_pid_path: PathBuf,
    armed: bool,
}

impl GrokFixtureProcessGuard {
    fn terminate(&self) {
        self.cancel.cancel();
        if let Some(pid) = grok_fixture_pid(&self.agent_pid_path) {
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
        for path in [&self.agent_pid_path, &self.child_pid_path] {
            if let Some(pid) = grok_fixture_pid(path) {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for GrokFixtureProcessGuard {
    fn drop(&mut self) {
        if self.armed {
            self.terminate();
        }
    }
}

#[tokio::test]
async fn topic_orchestrator_fixture_grok_acp_step_routes_permissions_questions_and_completion() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    grok_acp_script(&f, &env);
    let topic = topic(&f, None, true);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let session = topic.topic_session_id.clone().unwrap();
    let cancel = CancellationToken::new();
    let (tx, mut events) = mpsc::channel(256);
    f.daemon().subs.locked().push(Subscriber {
        conn: 99,
        tx: tx.into(),
        filter: Arc::new(StdMutex::new(EventFilter::default())),
        kick: CancellationToken::new(),
        gaps: BTreeMap::new(),
    });
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let mut args = step(
            &topic.features[0].id,
            HarnessKind::Acp,
            StepAccess::Write,
            &[],
        );
        args["acp_agent"] = json!("grok");
        let launched = f
            .daemon()
            .run_topic_step(&session, &args, &cancel)
            .await
            .unwrap();
        let attempt_id = launched["attempt_id"].as_str().unwrap();
        let permission = f
            .daemon()
            .wait_topic_steps(&session, &json!({"attempt_ids":[attempt_id]}), &cancel)
            .await
            .unwrap();
        assert_eq!(permission["event"], "permission");
        assert_eq!(permission["harness"], "acp");
        assert_eq!(permission["acp_agent"], "grok");
        let permission_id = permission["permission_id"].as_str().unwrap();
        let raw_input = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(ServerMessage::Event(Event::HarnessPermissionRequested {
                    permission_id: id,
                    raw_input: Some(raw_input),
                    ..
                })) = events.recv().await
                {
                    if id == permission_id {
                        break raw_input;
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            raw_input["_meta"]["x.ai/tool"],
            json!({"namespace":"grok_build","name":"use_tool"})
        );
        f.daemon()
            .handle(Request::HarnessPermissionDecision {
                permission_id: permission_id.into(),
                option_id: Some("allow".into()),
            })
            .await
            .unwrap();
        let question = f
            .daemon()
            .wait_topic_steps(&session, &json!({"attempt_ids":[attempt_id]}), &cancel)
            .await
            .unwrap();
        assert_eq!(question["event"], "question");
        assert_eq!(question["user_only"], false);
        assert_eq!(
            question["questions"][0]["question"],
            "Proceed with the topic fixture?"
        );
        let answered = f
            .daemon()
            .answer_topic_question_tool(
                &session,
                &json!({
                    "question_id":question["question_id"],
                    "answers":[{"selected":["Proceed"]}],
                    "basis":"The user's request says: Account for X and Y."
                }),
            )
            .unwrap();
        assert_eq!(answered["answered"], true);
        let completed = f
            .daemon()
            .wait_topic_steps(&session, &json!({"attempt_ids":[attempt_id]}), &cancel)
            .await
            .unwrap();
        assert_eq!(completed["event"], "completed");
        assert_eq!(completed["status"], "succeeded");
        let reply = std::fs::read_to_string(f.root.join("grok-reply.json")).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&reply).unwrap(),
            json!({
                "outcome":"accepted",
                "answers":{"Proceed with the topic fixture?":"Proceed"}
            })
        );
        let attempt = &state(&f, &topic.id).attempts[0];
        assert_eq!(attempt.harness, HarnessKind::Acp);
        assert_eq!(attempt.acp_agent.as_deref(), Some("grok"));
    })
    .await;
    if result.is_err() {
        cancel.cancel();
    }
    result.expect("Grok through-topic ACP fixture timed out");
}

#[tokio::test]
async fn topic_orchestrator_fixture_grok_acp_cancellation_cleans_up_process_group() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("TOPIC_FIXTURE_GROK_MODE", "hold");
    let f = Fixture::new(&env);
    grok_acp_script(&f, &env);
    let topic = topic(&f, None, false);
    f.daemon()
        .with_store(|st| st.claim_topic_workflow(&topic.id))
        .unwrap();
    let session = topic.topic_session_id.clone().unwrap();
    let mut args = step(
        &topic.features[0].id,
        HarnessKind::Acp,
        StepAccess::Write,
        &[],
    );
    args["acp_agent"] = json!("grok");
    let daemon = f.daemon().clone();
    let cancel = CancellationToken::new();
    let agent_pid_path = f.root.join("grok-pid");
    let child_pid_path = f.root.join("grok-child-pid");
    let mut cleanup = GrokFixtureProcessGuard {
        cancel: cancel.clone(),
        agent_pid_path: agent_pid_path.clone(),
        child_pid_path: child_pid_path.clone(),
        armed: true,
    };
    let run_cancel = cancel.clone();
    let mut task = tokio::spawn(async move {
        daemon
            .run_topic_step_until_done(&session, &args, &run_cancel)
            .await
    });
    wait_until(|| agent_pid_path.exists() && child_pid_path.exists()).await;
    f.daemon()
        .handle(Request::CancelTopic {
            topic_id: topic.id.clone(),
        })
        .await
        .unwrap();
    let result = match tokio::time::timeout(Duration::from_secs(10), &mut task).await {
        Ok(result) => result.unwrap().unwrap(),
        Err(_) => {
            cleanup.terminate();
            let agent_pid = grok_fixture_pid(&agent_pid_path).expect("ACP agent pid was captured");
            let child_pid = grok_fixture_pid(&child_pid_path).expect("ACP child pid was captured");
            let processes_stopped = tokio::time::timeout(Duration::from_secs(3), async {
                while !grok_fixture_process_stopped(agent_pid)
                    || !grok_fixture_process_stopped(child_pid)
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .is_ok();
            let mut task_stopped = tokio::time::timeout(Duration::from_secs(5), &mut task)
                .await
                .is_ok();
            if !task_stopped {
                task.abort();
                task_stopped = tokio::time::timeout(Duration::from_secs(2), &mut task)
                    .await
                    .is_ok();
            }
            if processes_stopped {
                cleanup.disarm();
            }
            assert!(
                processes_stopped,
                "watchdog left the Grok ACP process group alive"
            );
            assert!(task_stopped, "watchdog could not stop the Grok ACP task");
            panic!("Grok ACP cancellation exceeded its watchdog after cleanup");
        }
    };
    assert_eq!(result["status"], "cancelled", "{result}");
    assert_eq!(result["harness"], "acp");
    assert_eq!(result["acp_agent"], "grok");
    for path in [agent_pid_path, child_pid_path] {
        let pid: i32 = std::fs::read_to_string(path).unwrap().parse().unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !grok_fixture_process_stopped(pid) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(grok_fixture_process_stopped(pid));
    }
    cleanup.disarm();
}
