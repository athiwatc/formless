#![cfg(unix)]

#[path = "../../formless-core/tests/support/env.rs"]
mod credential_env;

use formless_core::{daemon::Daemon, Config, Paths, Store};
use formless_protocol::{
    QuestionAnswer, Request, RequestEnvelope, ServerMessage, TopicAttemptStatus,
    TopicFeatureProposal, TopicProposalRequest, WorkspaceKind, PROTOCOL_VERSION,
};
use std::os::unix::fs::PermissionsExt;
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

const HARNESS: &str = r#"
import hashlib,json,os,sqlite3,subprocess,sys
if sys.argv[1]=='seed':
    db,topic,feature,session,parent,workspace,outputs=sys.argv[2:]
    with sqlite3.connect(db) as c:
        c.execute("UPDATE topics SET status='running' WHERE id=?",(topic,))
        c.execute("UPDATE topic_features SET status='running',attempt_count=1,active_attempt_id='stdio-attempt' WHERE id=?",(feature,))
        c.execute("UPDATE sessions SET session_kind='topic_attempt',parent_session_id=?,topic_id=?,topic_attempt_id='stdio-attempt',harness_kind='claude' WHERE id=?",(parent,topic,session))
        c.execute("""INSERT INTO topic_attempts(id,topic_id,feature_id,step_title,access,harness_kind,status,conversation_session_id,capability_hash,worktree_path,outputs_dir,created_at,updated_at)
          VALUES('stdio-attempt',?,?,'Stdio fixture','read_only','claude','running',?,?,?,?,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')""",
          (topic,feature,session,hashlib.sha256(os.environ['FORMLESS_TOPIC_CAPABILITY'].encode()).hexdigest(),workspace,outputs))
    sys.exit(0)
bridge=subprocess.Popen([sys.argv[1],'--profile','stdio-fixture','topic-mcp','--attempt','stdio-attempt'],
    stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
def call(i,method,params):
    bridge.stdin.write(json.dumps({'jsonrpc':'2.0','id':i,'method':method,'params':params})+'\n');bridge.stdin.flush()
    response=json.loads(bridge.stdout.readline())
    assert response['id']==i and 'error' not in response,response
    return response['result']
initialized=call(1,'initialize',{'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'fixture','version':'1'}})
assert initialized['protocolVersion']=='2024-11-05'
bridge.stdin.write(json.dumps({'jsonrpc':'2.0','method':'notifications/initialized'})+'\n');bridge.stdin.flush()
tools=call(2,'tools/list',{})
assert 'report_result' in [tool['name'] for tool in tools['tools']]
result=call(3,'tools/call',{'name':'report_result','arguments':{'status':'succeeded','summary':'Real stdio bridge result','outputs':[{'kind':'text','title':'Fixture','content':'stdio artifact'}]}})
assert result['isError']==False,result
saved=json.loads(result['content'][0]['text'])
assert saved['status']=='succeeded',saved
bridge.stdin.close()
assert bridge.wait(timeout=5)==0,bridge.stderr.read()
print('initialize -> tools/list -> report_result: ok')
"#;

const GEMINI_QUESTION_ACP: &str = r#"#!/usr/bin/env python3
import json,os,pathlib,subprocess,sys
assert sys.argv[1:]==['--acp'],sys.argv
servers=[]
def send(message):
    sys.stdout.write(json.dumps(message)+'\n');sys.stdout.flush()
def read():
    line=sys.stdin.readline()
    if not line:sys.exit(0)
    return json.loads(line)
def mcp_call(bridge,request_id,method,params):
    bridge.stdin.write(json.dumps({'jsonrpc':'2.0','id':request_id,'method':method,'params':params})+'\n');bridge.stdin.flush()
    response=json.loads(bridge.stdout.readline())
    assert response['id']==request_id and 'error' not in response,response
    return response['result']
while True:
    message=read();method=message.get('method')
    if method=='initialize':
        send({'jsonrpc':'2.0','id':message['id'],'result':{'protocolVersion':1,'agentInfo':{'name':'gemini-cli','version':'fixture'}}})
    elif method=='session/new':
        servers=message['params']['mcpServers']
        send({'jsonrpc':'2.0','id':message['id'],'result':{'sessionId':'gemini-question'}})
    elif method=='session/set_mode':
        send({'jsonrpc':'2.0','id':message['id'],'result':{}})
    elif method=='session/prompt':
        try:
            assert len(servers)==1 and servers[0]['name']=='formless',servers
            server=servers[0]
            environment=os.environ.copy()
            environment.update({item['name']:item['value'] for item in server['env']})
            bridge=subprocess.Popen([server['command']]+server['args'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,env=environment)
            initialized=mcp_call(bridge,1,'initialize',{'protocolVersion':'2024-11-05','capabilities':{},'clientInfo':{'name':'gemini-fixture','version':'1'}})
            assert initialized['protocolVersion']=='2024-11-05',initialized
            bridge.stdin.write(json.dumps({'jsonrpc':'2.0','method':'notifications/initialized'})+'\n');bridge.stdin.flush()
            tools=mcp_call(bridge,2,'tools/list',{})
            assert 'ask_user' in [tool['name'] for tool in tools['tools']],tools
            pathlib.Path(os.environ['FORMLESS_TOPIC_QUESTION_STARTED']).write_text('waiting')
            result=mcp_call(bridge,3,'tools/call',{'name':'ask_user','arguments':{'questions':[{'question':'Which fresh deployment cove should the preview use?','header':'Cove','multi_select':False,'options':[{'label':'copper'},{'label':'violet'}]}]}})
            assert result['isError']==False,result
            answers=json.loads(result['content'][0]['text'])
            selected=answers[0]['selected']
            marker=pathlib.Path(os.environ['FORMLESS_TOPIC_QUESTION_REPLY'])
            temporary=marker.with_suffix('.tmp')
            temporary.write_text(json.dumps(selected))
            temporary.replace(marker)
            bridge.stdin.close()
            assert bridge.wait(timeout=5)==0,bridge.stderr.read()
            send({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'gemini-question','update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'Selected '+', '.join(selected)}}}})
            send({'jsonrpc':'2.0','id':message['id'],'result':{'stopReason':'end_turn'}})
        except Exception as error:
            pathlib.Path(os.environ['FORMLESS_TOPIC_QUESTION_ERROR']).write_text(repr(error))
            raise
"#;

struct StopDaemon(Arc<Daemon>);
impl Drop for StopDaemon {
    fn drop(&mut self) {
        self.0.request_shutdown();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn topic_mcp_real_stdio_negotiates_and_resolves_named_profile() {
    let env = credential_env::EnvGuard::isolated();
    env.remove("FORMLESS_SECURE");
    let root = tempfile::tempdir().unwrap();
    let base = Paths::from_dirs(root.path().join("data"), root.path().join("config")).unwrap();
    let paths = base.named("stdio-fixture").unwrap();
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    Config::default().save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let source = store.create_session(None).unwrap();
    let message = store
        .append_message(&source.id, "user", "Test the real stdio topic bridge")
        .unwrap();
    let topic = store
        .propose_topic(
            &TopicProposalRequest {
                parent_topic_id: None,
                source_session_id: source.id,
                message_ids: vec![message.id],
                features: vec![],
                name: "Stdio fixture".into(),
                workspace: None,
                outline: vec![],
            },
            WorkspaceKind::Scratch,
            None,
        )
        .unwrap();
    let topic = store.approve_topic(&topic.id).unwrap();
    let conversation = store.create_session(Some("Stdio attempt".into())).unwrap();
    drop(store);
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let _stop = StopDaemon(daemon.clone());
    let fixture = root.path().join("fake-harness.py");
    std::fs::write(&fixture, HARNESS).unwrap();
    let workspace = paths
        .data_dir
        .join("topic-attempts/stdio-attempt/workspace");
    let outputs = paths.data_dir.join("topic-attempts/stdio-attempt/outputs");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&outputs).unwrap();
    let token = "fixture-capability-not-a-production-secret";
    let seeded = std::process::Command::new("python3")
        .arg(&fixture)
        .arg("seed")
        .arg(paths.db_file())
        .arg(&topic.id)
        .arg(&topic.features[0].id)
        .arg(&conversation.id)
        .arg(topic.topic_session_id.as_ref().unwrap())
        .arg(&workspace)
        .arg(&outputs)
        .env("FORMLESS_TOPIC_CAPABILITY", token)
        .output()
        .unwrap();
    assert!(
        seeded.status.success(),
        "{}",
        String::from_utf8_lossy(&seeded.stderr)
    );
    let server = tokio::spawn(daemon.clone().serve());
    tokio::time::timeout(Duration::from_secs(5), async {
        while !paths.socket_path().exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::process::Command::new("python3")
            .arg(&fixture)
            .arg(env!("CARGO_BIN_EXE_formless"))
            .env("FORMLESS_DATA_DIR", &base.data_dir)
            .env("FORMLESS_CONFIG_DIR", &base.config_dir)
            .env("FORMLESS_TOPIC_CAPABILITY", token)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout)
        .contains("initialize -> tools/list -> report_result: ok"));
    let store = Store::open(&paths.db_file()).unwrap();
    let saved = store.get_topic_attempt("stdio-attempt").unwrap().unwrap();
    assert_eq!(saved.status, TopicAttemptStatus::Succeeded);
    assert_eq!(saved.summary.as_deref(), Some("Real stdio bridge result"));
    assert_eq!(saved.conversation_session_id, conversation.id);
    assert!(
        !base.db_file().exists(),
        "must not fall back to the default profile"
    );
    daemon.request_shutdown();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn named_gemini_topic_ask_user_waits_for_and_delivers_user_choice() {
    let env = credential_env::EnvGuard::isolated();
    env.remove("FORMLESS_SECURE");
    let root = tempfile::tempdir().unwrap();
    let base = Paths::from_dirs(root.path().join("data"), root.path().join("config")).unwrap();
    let paths = base.named("gemini-question").unwrap();
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    Config::default().save(&paths).unwrap();

    let fixture = root.path().join("gemini-ask-user.py");
    std::fs::write(&fixture, GEMINI_QUESTION_ACP).unwrap();
    std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o700)).unwrap();
    let started = root.path().join("ask-user-started");
    let reply = root.path().join("ask-user-reply");
    let error = root.path().join("ask-user-error");
    env.set("FORMLESS_TOPIC_ACP_EXE", &fixture);
    env.set("FORMLESS_TOPIC_MCP_EXE", env!("CARGO_BIN_EXE_formless"));
    env.set("FORMLESS_TOPIC_QUESTION_STARTED", &started);
    env.set("FORMLESS_TOPIC_QUESTION_REPLY", &reply);
    env.set("FORMLESS_TOPIC_QUESTION_ERROR", &error);

    let store = Store::open(&paths.db_file()).unwrap();
    let source = store.create_session(None).unwrap();
    let message = store
        .append_message(
            &source.id,
            "user",
            "Create a release preview with an unspecified deployment choice.",
        )
        .unwrap();
    let topic = store
        .propose_topic(
            &TopicProposalRequest {
                parent_topic_id: None,
                source_session_id: source.id,
                message_ids: vec![message.id.clone()],
                features: vec![TopicFeatureProposal {
                    source_message_id: message.id,
                    request: "Create the requested release preview.".into(),
                }],
                name: "Fresh deployment choice fixture".into(),
                workspace: None,
                outline: vec![],
            },
            WorkspaceKind::Scratch,
            None,
        )
        .unwrap();
    let topic = store.approve_topic(&topic.id).unwrap();
    let topic_session = topic.topic_session_id.clone().unwrap();
    let feature_id = topic.features[0].id.clone();
    drop(store);
    let running = std::process::Command::new("python3")
        .args([
            "-c",
            "import sqlite3,sys\nwith sqlite3.connect(sys.argv[1]) as db: db.execute(\"UPDATE topics SET status='running' WHERE id=?\",(sys.argv[2],))",
        ])
        .arg(paths.db_file())
        .arg(&topic.id)
        .output()
        .unwrap();
    assert!(
        running.status.success(),
        "{}",
        String::from_utf8_lossy(&running.stderr)
    );

    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let _stop = StopDaemon(daemon.clone());
    let server = tokio::spawn(daemon.clone().serve());
    tokio::time::timeout(Duration::from_secs(5), async {
        while !paths.socket_path().exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let stream = tokio::net::UnixStream::connect(paths.socket_path())
        .await
        .unwrap();
    let (reader, mut writer) = tokio::io::split(stream);
    let mut lines = tokio::io::BufReader::new(reader).lines();
    tokio::time::timeout(Duration::from_secs(10), async {
        while daemon.subscriber_count() == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("topic user connection was not registered");

    let (launched, ok) = daemon
        .call_tool_direct_outcome(
            &topic_session,
            "topic_run_step",
            serde_json::json!({
                "feature_id":feature_id,
                "title":"Ask for a fresh deployment choice",
                "instructions":"Use the scoped Formless MCP ask_user tool for a fresh choice, then complete the step with the exact selected reply.",
                "harness":"acp",
                "acp_agent":"gemini",
                "access":"write"
            }),
        )
        .await;
    assert!(ok, "{launched}");
    let launched: serde_json::Value = serde_json::from_str(&launched).unwrap();
    let attempt_id = launched["attempt_id"].as_str().unwrap().to_owned();

    let question_text = tokio::time::timeout(
        Duration::from_secs(30),
        daemon.call_tool_direct(
            &topic_session,
            "topic_wait",
            serde_json::json!({"attempt_ids":[attempt_id],"timeout_secs":30}),
        ),
    )
    .await
    .expect("topic did not surface the fresh ask_user question");
    let question: serde_json::Value = serde_json::from_str(&question_text).unwrap();
    assert_eq!(question["event"], "question", "{question}");
    assert_eq!(question["user_only"], false, "{question}");
    assert_eq!(
        question["questions"][0]["question"],
        "Which fresh deployment cove should the preview use?"
    );
    assert_eq!(question["questions"][0]["options"][1]["label"], "violet");
    let question_id = question["question_id"].as_str().unwrap().to_owned();
    assert_eq!(question["attempt_id"], attempt_id);
    assert!(
        started.exists(),
        "ACP profile did not call the scoped MCP ask_user tool"
    );
    assert!(
        !reply.exists(),
        "the step must remain pending before the user answers"
    );

    let pending = Store::open(&paths.db_file())
        .unwrap()
        .get_topic_attempt(&attempt_id)
        .unwrap()
        .unwrap();
    assert_eq!(pending.status, TopicAttemptStatus::NeedsUser);
    let (empty_basis, accepted) = daemon
        .call_tool_direct_outcome(
            &topic_session,
            "topic_answer_question",
            serde_json::json!({
                "question_id":question_id,
                "answers":[{"selected":["copper"]}],
                "basis":""
            }),
        )
        .await;
    assert!(
        !accepted,
        "empty-basis model answer was accepted: {empty_basis}"
    );
    assert!(empty_basis.contains("basis"), "{empty_basis}");
    assert_eq!(
        Store::open(&paths.db_file())
            .unwrap()
            .get_topic_attempt(&attempt_id)
            .unwrap()
            .unwrap()
            .status,
        TopicAttemptStatus::NeedsUser
    );
    assert!(
        !reply.exists(),
        "an empty-basis answer must not resolve the pending MCP call"
    );

    let request = RequestEnvelope {
        id: 17,
        protocol: PROTOCOL_VERSION,
        request: Request::AnswerQuestion {
            question_id,
            answer: "violet".into(),
            answers: vec![QuestionAnswer {
                selected: vec!["violet".into()],
                text: None,
            }],
        },
    };
    writer
        .write_all(format!("{}\n", serde_json::to_string(&request).unwrap()).as_bytes())
        .await
        .unwrap();
    let response_ok = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let line = lines.next_line().await.unwrap().unwrap();
            if let Ok(ServerMessage::Response { id, ok, .. }) =
                serde_json::from_str::<ServerMessage>(&line)
            {
                if id == 17 {
                    break ok;
                }
            }
        }
    })
    .await
    .expect("daemon did not accept the user's question answer");
    assert!(response_ok);

    let completed_text = tokio::time::timeout(
        Duration::from_secs(30),
        daemon.call_tool_direct(
            &topic_session,
            "topic_wait",
            serde_json::json!({"attempt_ids":[attempt_id],"timeout_secs":30}),
        ),
    )
    .await
    .expect("Gemini topic step did not complete after the user's answer");
    let completed: serde_json::Value = serde_json::from_str(&completed_text).unwrap();
    assert_eq!(completed["event"], "completed", "{completed}");
    assert_eq!(completed["status"], "succeeded", "{completed}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&reply).unwrap())
            .unwrap(),
        serde_json::json!(["violet"])
    );
    assert!(
        !error.exists(),
        "{}",
        std::fs::read_to_string(error).unwrap_or_default()
    );

    drop(lines);
    drop(writer);
    daemon.request_shutdown();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
