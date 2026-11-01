//! Actual daemon connection routing with relay serialization/lease ownership;
//! no provider, external relay, keyring or native socket is needed here.
use super::*;
use crate::relay_buffers::{ClientMemory, RelayOutput, Serialized};
use std::time::Duration;
use tokio::io::DuplexStream;

struct TempRoot(std::path::PathBuf);
impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Fixture {
    daemon: Arc<Daemon>,
    _root: TempRoot,
}
impl Fixture {
    fn new(env: &crate::credential_env::EnvGuard) -> Self {
        env.remove("FORMLESS_SECURE");
        let root = std::env::temp_dir().join(format!("fl-relay-conn-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
        let mut cfg = Config::default();
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.save(&paths).unwrap();
        Self {
            daemon: Arc::new(Daemon::new(paths).unwrap()),
            _root: TempRoot(root),
        }
    }
    fn connection(
        &self,
        capacity: usize,
    ) -> (
        DuplexStream,
        mpsc::Receiver<Serialized>,
        Arc<ClientMemory>,
        tokio::task::JoinHandle<()>,
    ) {
        let memory = self
            .daemon
            .relay_memory
            .client(CancellationToken::new())
            .unwrap();
        let (tx, rx) = mpsc::channel(capacity);
        let (peer, stream) = tokio::io::duplex(64 * 1024);
        let task = tokio::spawn(self.daemon.clone().handle_conn_relay(
            stream,
            RelayOutput {
                tx,
                memory: memory.clone(),
            },
        ));
        (peer, rx, memory, task)
    }
}
async fn request(peer: &mut DuplexStream, id: u64, request: Request) {
    let mut line = serde_json::to_vec(&RequestEnvelope {
        id,
        protocol: PROTOCOL_VERSION,
        request,
    })
    .unwrap();
    line.push(b'\n');
    tokio::time::timeout(Duration::from_secs(5), peer.write_all(&line))
        .await
        .unwrap()
        .unwrap();
}
async fn response(rx: &mut mpsc::Receiver<Serialized>, id: u64) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = rx.recv().await.expect("connection response");
            let message: ServerMessage = serde_json::from_str(&frame.line).unwrap();
            if matches!(&message,ServerMessage::Response{id:got,..} if *got==id) {
                return message;
            }
        }
    })
    .await
    .expect("bounded response")
}

#[tokio::test]
async fn labeled_relay_connection_uses_actual_handler_and_releases_queues_after_cancel() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let (mut peer, mut rx, memory, task) = f.connection(crate::relay_buffers::OUTBOUND_MESSAGES);
    tokio::time::timeout(Duration::from_secs(5), memory.started())
        .await
        .expect("bounded Core startup barrier")
        .unwrap();
    assert_eq!(
        f.daemon.subscriber_count(),
        1,
        "readiness includes registration"
    );
    request(
        &mut peer,
        1,
        Request::Subscribe {
            sessions: Some(vec![]),
            deltas: false,
            client: Some("CLI".into()),
        },
    )
    .await;
    assert!(matches!(
        response(&mut rx, 1).await,
        ServerMessage::Response { ok: true, .. }
    ));
    assert_eq!(
        f.daemon
            .subs
            .locked()
            .iter()
            .find_map(|sub| sub.filter.locked().label.clone()),
        Some("CLI".into())
    );
    request(&mut peer, 2, Request::Ping).await;
    assert!(matches!(
        response(&mut rx, 2).await,
        ServerMessage::Response { ok: true, .. }
    ));
    let approval = uuid::Uuid::new_v4().to_string();
    let (tx, answered) = oneshot::channel();
    f.daemon.approvals.locked().insert(approval.clone(), tx);
    request(
        &mut peer,
        3,
        Request::ApprovalDecision {
            approval_id: approval.clone(),
            approved: true,
            scope: ApprovalScope::Once,
            paths: std::collections::BTreeMap::new(),
            remember_denial: false,
        },
    )
    .await;
    let result = response(&mut rx, 3).await;
    assert!(matches!(result, ServerMessage::Response { ok: true, .. }));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), answered)
            .await
            .unwrap()
            .unwrap(),
        (true, ApprovalScope::Once)
    );
    assert_eq!(
        f.daemon.approval_by.locked().remove(&approval),
        Some("CLI".into())
    );
    memory.cancel.cancel();
    drop(peer);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    drop(rx);
    drop(memory);
    assert_eq!(f.daemon.subscriber_count(), 0);
    assert_eq!(f.daemon.relay_memory.usage(), (0, 0, 0));
}
#[tokio::test]
async fn full_relay_output_queue_closes_only_that_connection_while_another_answers() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let (mut stalled, stalled_rx, stalled_memory, stalled_task) = f.connection(1);
    let (mut healthy, mut healthy_rx, healthy_memory, healthy_task) =
        f.connection(crate::relay_buffers::OUTBOUND_MESSAGES);
    let mut batch = Vec::new();
    for id in 1..=16 {
        serde_json::to_writer(
            &mut batch,
            &RequestEnvelope {
                id,
                protocol: PROTOCOL_VERSION,
                request: Request::Ping,
            },
        )
        .unwrap();
        batch.push(b'\n');
    }
    tokio::time::timeout(Duration::from_secs(5), stalled.write_all(&batch))
        .await
        .unwrap()
        .unwrap();
    request(&mut healthy, 100, Request::Ping).await;
    assert!(matches!(
        response(&mut healthy_rx, 100).await,
        ServerMessage::Response { ok: true, .. }
    ));
    tokio::time::timeout(Duration::from_secs(5), stalled_task)
        .await
        .unwrap()
        .unwrap();
    assert!(stalled_memory.cancel.is_cancelled());
    assert!(!healthy_memory.cancel.is_cancelled());
    drop(stalled);
    drop(stalled_rx);
    drop(stalled_memory);
    healthy_memory.cancel.cancel();
    drop(healthy);
    tokio::time::timeout(Duration::from_secs(5), healthy_task)
        .await
        .unwrap()
        .unwrap();
    drop(healthy_rx);
    drop(healthy_memory);
    assert_eq!(f.daemon.relay_memory.usage(), (0, 0, 0));
}

#[tokio::test]
async fn oversized_relay_subscription_preserves_filter_and_local_ipc_policy() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let (mut peer, mut rx, memory, task) = f.connection(crate::relay_buffers::OUTBOUND_MESSAGES);
    request(
        &mut peer,
        1,
        Request::Subscribe {
            sessions: Some(vec!["kept-session".into()]),
            deltas: false,
            client: Some("kept".into()),
        },
    )
    .await;
    assert!(matches!(
        response(&mut rx, 1).await,
        ServerMessage::Response { ok: true, .. }
    ));
    for (index, (sessions, client)) in [
        (
            Some(vec!["s".into(); crate::relay_buffers::METADATA_ENTRIES + 1]),
            Some("changed".into()),
        ),
        (
            Some(vec!["s".repeat(crate::relay_buffers::METADATA_BYTES + 1)]),
            Some("changed".into()),
        ),
        (
            None,
            Some("c".repeat(crate::relay_buffers::CLIENT_LABEL_BYTES + 1)),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let id = index as u64 + 2;
        request(
            &mut peer,
            id,
            Request::Subscribe {
                sessions,
                deltas: true,
                client,
            },
        )
        .await;
        assert_eq!(
            response(&mut rx, id).await.error_code(),
            Some(codes::BAD_REQUEST)
        );
        let subs = f.daemon.subs.locked();
        let filter = subs[0].filter.locked();
        assert_eq!(filter.label.as_deref(), Some("kept"));
        assert!(!filter.deltas);
        assert_eq!(
            filter.sessions.as_ref().unwrap(),
            &std::collections::HashSet::from(["kept-session".into()])
        );
    }
    request(&mut peer, 10, Request::Ping).await;
    assert!(matches!(
        response(&mut rx, 10).await,
        ServerMessage::Response { ok: true, .. }
    ));
    // The relay-only label ceiling must not change ordinary local IPC.
    let (local_peer, local_server) = tokio::io::duplex(64 * 1024);
    let local_task = tokio::spawn(f.daemon.clone().handle_conn(local_server));
    let mut local = BufReader::new(local_peer);
    request(
        local.get_mut(),
        20,
        Request::Subscribe {
            sessions: None,
            deltas: false,
            client: Some("c".repeat(crate::relay_buffers::CLIENT_LABEL_BYTES + 1)),
        },
    )
    .await;
    let reply = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let line = read_frame(&mut local, MAX_FRAME).await.unwrap().unwrap();
            let reply: ServerMessage = serde_json::from_str(&line).unwrap();
            if matches!(&reply, ServerMessage::Response { id: 20, .. }) {
                break reply;
            }
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        reply,
        ServerMessage::Response {
            id: 20,
            ok: true,
            ..
        }
    ));
    assert!(f.daemon.subs.locked().iter().any(|sub| sub
        .filter
        .locked()
        .label
        .as_ref()
        .is_some_and(|label| label.len() == crate::relay_buffers::CLIENT_LABEL_BYTES + 1)));
    memory.cancel.cancel();
    drop(peer);
    drop(local);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), local_task)
        .await
        .unwrap()
        .unwrap();
    drop(rx);
    drop(memory);
    assert_eq!(f.daemon.relay_memory.usage(), (0, 0, 0));
}

// A real Store lock blocks the sole FIFO worker without a provider, timer sleep
// or synthetic handler path. The gate thread releases even if the test unwinds.
async fn hold_store(f: &Fixture) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let daemon = f.daemon.clone();
    let (ready, waiting) = oneshot::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        let _store = daemon.store.locked();
        let _ = ready.send(());
        let _ = gate.recv_timeout(Duration::from_secs(5));
    });
    tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap();
    (release, thread)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn relay_interruption_cut_survives_older_fifo_requests_and_prevents_both_queued_sends() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let sid = f
        .daemon
        .handle(Request::CreateSession { name: None })
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (mut peer, mut rx, memory, task) = f.connection(crate::relay_buffers::OUTBOUND_MESSAGES);
    let (release, gate) = hold_store(&f).await;
    request(
        &mut peer,
        1,
        Request::CreateSession {
            name: Some("FIFO worker gate".into()),
        },
    )
    .await;
    for id in [2, 3] {
        request(
            &mut peer,
            id,
            Request::SendMessage {
                session_id: sid.clone(),
                content: "must not append".into(),
                mode: SendMode::Send,
                client_msg_id: None,
            },
        )
        .await;
    }
    request(
        &mut peer,
        4,
        Request::InterruptRun {
            session_id: sid.clone(),
        },
    )
    .await;
    request(&mut peer, 5, Request::Ping).await;
    // Ping proves that the reader has installed the cut while the worker is
    // held on the earlier CreateSession write. Keep interleaved responses for assertion.
    let mut replies = HashMap::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !replies.contains_key(&5) {
            let frame = rx.recv().await.unwrap();
            if let reply @ ServerMessage::Response { id, .. } =
                serde_json::from_str::<ServerMessage>(&frame.line).unwrap()
            {
                replies.insert(id, reply);
            }
        }
    })
    .await
    .unwrap();
    release.send(()).unwrap();
    gate.join().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !replies.contains_key(&2) || !replies.contains_key(&3) {
            let frame = rx.recv().await.unwrap();
            if let reply @ ServerMessage::Response { id, .. } =
                serde_json::from_str::<ServerMessage>(&frame.line).unwrap()
            {
                replies.insert(id, reply);
            }
        }
    })
    .await
    .unwrap();
    for id in [2, 3] {
        let ServerMessage::Response { ok, error, .. } = &replies[&id] else {
            unreachable!()
        };
        assert!(
            !ok && error
                .as_deref()
                .unwrap()
                .contains("interrupted before it started")
        );
    }
    assert!(f
        .daemon
        .store
        .locked()
        .list_messages(&sid)
        .unwrap()
        .is_empty());
    memory.cancel.cancel();
    drop(peer);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    drop(rx);
    drop(memory);
    assert_eq!(f.daemon.relay_memory.usage(), (0, 0, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn relay_interrupt_metadata_overflow_cancels_queued_work_but_not_a_healthy_connection() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let sid = f
        .daemon
        .handle(Request::CreateSession { name: None })
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (mut peer, rx, memory, task) = f.connection(crate::relay_buffers::OUTBOUND_MESSAGES);
    let (mut healthy, mut healthy_rx, healthy_memory, healthy_task) =
        f.connection(crate::relay_buffers::OUTBOUND_MESSAGES);
    let (release, gate) = hold_store(&f).await;
    request(
        &mut peer,
        1,
        Request::CreateSession {
            name: Some("FIFO worker gate".into()),
        },
    )
    .await;
    request(
        &mut peer,
        2,
        Request::SendMessage {
            session_id: sid.clone(),
            content: "must not append".into(),
            mode: SendMode::Send,
            client_msg_id: None,
        },
    )
    .await;
    request(
        &mut peer,
        3,
        Request::InterruptRun {
            session_id: "x".repeat(crate::relay_buffers::METADATA_BYTES + 1),
        },
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), memory.cancel.cancelled())
        .await
        .unwrap();
    request(&mut healthy, 100, Request::Ping).await;
    assert!(matches!(
        response(&mut healthy_rx, 100).await,
        ServerMessage::Response { ok: true, .. }
    ));
    assert!(!healthy_memory.cancel.is_cancelled());
    release.send(()).unwrap();
    gate.join().unwrap();
    drop(peer);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert!(f
        .daemon
        .store
        .locked()
        .list_messages(&sid)
        .unwrap()
        .is_empty());
    healthy_memory.cancel.cancel();
    drop(healthy);
    tokio::time::timeout(Duration::from_secs(5), healthy_task)
        .await
        .unwrap()
        .unwrap();
    drop((rx, memory, healthy_rx, healthy_memory));
    assert_eq!(f.daemon.relay_memory.usage(), (0, 0, 0));
}

#[tokio::test]
async fn relay_disconnect_rpc_reports_full_and_closed_queue_without_false_success_or_audit() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    f.daemon.relay_clients.locked().insert(
        "known".into(),
        RelayClientInfo {
            cid: "known".into(),
            user_agent: "private fixture".into(),
            first_seen: "now".into(),
            last_seen: "now".into(),
        },
    );
    let (kick, mut commands) = mpsc::channel(1);
    *f.daemon.relay_kick.locked() = Some(kick);
    let (mut peer, mut rx, memory, task) = f.connection(crate::relay_buffers::OUTBOUND_MESSAGES);
    request(
        &mut peer,
        1,
        Request::DisconnectRelayClient {
            cid: "known".into(),
        },
    )
    .await;
    let first = response(&mut rx, 1).await;
    assert!(matches!(first, ServerMessage::Response { ok: true, .. }));
    request(
        &mut peer,
        2,
        Request::DisconnectRelayClient {
            cid: "known".into(),
        },
    )
    .await;
    let full = response(&mut rx, 2).await;
    assert_eq!(full.error_code(), Some(codes::BUSY));
    assert!(serde_json::to_string(&full).unwrap().contains("not queued"));
    assert_eq!(commands.try_recv().unwrap(), "known");
    assert!(
        commands.try_recv().is_err(),
        "refused command was not buffered"
    );
    drop(commands);
    request(
        &mut peer,
        3,
        Request::DisconnectRelayClient {
            cid: "known".into(),
        },
    )
    .await;
    assert_eq!(response(&mut rx, 3).await.error_code(), Some(codes::BUSY));
    *f.daemon.relay_kick.locked() = None;
    request(
        &mut peer,
        4,
        Request::DisconnectRelayClient {
            cid: "known".into(),
        },
    )
    .await;
    assert_eq!(response(&mut rx, 4).await.error_code(), Some(codes::BUSY));
    request(
        &mut peer,
        5,
        Request::DisconnectRelayClient {
            cid: "unknown".into(),
        },
    )
    .await;
    assert!(matches!(
        response(&mut rx, 5).await,
        ServerMessage::Response { ok: true, .. }
    ));
    assert!(
        f.daemon.relay_clients.locked().contains_key("known"),
        "queue refusal cannot alter actual device state"
    );
    let audit = f
        .daemon
        .store
        .locked()
        .recent_audit(Some("relay.disconnect"), 10)
        .unwrap();
    assert_eq!(
        audit.len(),
        1,
        "only the successfully queued action is audited"
    );
    assert_eq!(audit[0].2, "known");
    memory.cancel.cancel();
    drop(peer);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    drop(rx);
    drop(memory);
    assert_eq!(f.daemon.relay_memory.usage(), (0, 0, 0));
}

#[tokio::test]
async fn compact_wide_relay_input_is_refused_before_typed_queue_while_healthy_client_answers() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let (mut peer, rx, memory, task) = f.connection(crate::relay_buffers::OUTBOUND_MESSAGES);
    let (mut healthy, mut healthy_rx, healthy_memory, healthy_task) =
        f.connection(crate::relay_buffers::OUTBOUND_MESSAGES);
    let template = serde_json::to_string(&RequestEnvelope {
        id: 1,
        protocol: PROTOCOL_VERSION,
        request: Request::RunTool {
            id: "not invoked".into(),
            input: Value::Null,
            session_id: "not invoked".into(),
        },
    })
    .unwrap();
    let array = format!("[{}]", "[0],".repeat(1_800_000).trim_end_matches(','));
    let mut line = template.replace("\"input\":null", &format!("\"input\":{array}"));
    assert!(line.len() < MAX_FRAME);
    assert!(!line.contains("\"input\":null"));
    line.push('\n');
    tokio::time::timeout(Duration::from_secs(5), peer.write_all(line.as_bytes()))
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), memory.cancel.cancelled())
        .await
        .unwrap();
    request(&mut healthy, 100, Request::Ping).await;
    assert!(matches!(
        response(&mut healthy_rx, 100).await,
        ServerMessage::Response { ok: true, .. }
    ));
    assert!(!healthy_memory.cancel.is_cancelled());
    drop(peer);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    healthy_memory.cancel.cancel();
    drop(healthy);
    tokio::time::timeout(Duration::from_secs(5), healthy_task)
        .await
        .unwrap()
        .unwrap();
    drop((rx, memory, healthy_rx, healthy_memory));
    assert_eq!(f.daemon.relay_memory.usage(), (0, 0, 0));
}

#[tokio::test]
async fn relay_invalid_type_errors_preserve_ids_without_echo_or_whole_value_reparse() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let (mut peer, mut rx, memory, task) = f.connection(crate::relay_buffers::OUTBOUND_MESSAGES);
    let private = "opaque-private-type-".repeat(200_000);
    let mut line =
        serde_json::json!({"id":71,"protocol":PROTOCOL_VERSION,"request":{"type":private}})
            .to_string();
    line.push('\n');
    tokio::time::timeout(Duration::from_secs(5), peer.write_all(line.as_bytes()))
        .await
        .unwrap()
        .unwrap();
    let reply = response(&mut rx, 71).await;
    assert_eq!(reply.error_code(), Some(codes::UNSUPPORTED));
    assert!(serde_json::to_string(&reply).unwrap().len() < 256);
    let input = format!("[{}]", "0,".repeat(32769).trim_end_matches(','));
    let mut invalid = format!(
        r#"{{"id":72,"protocol":{},"request":{{"type":"run_tool","id":false,"session_id":"fixture","input":{input}}}}}"#,
        PROTOCOL_VERSION
    );
    invalid.push('\n');
    tokio::time::timeout(Duration::from_secs(5), peer.write_all(invalid.as_bytes()))
        .await
        .unwrap()
        .unwrap();
    let reply = response(&mut rx, 72).await;
    assert_eq!(reply.error_code(), Some(codes::BAD_REQUEST));
    assert!(serde_json::to_string(&reply).unwrap().len() < 256);
    request(&mut peer, 73, Request::Ping).await;
    assert!(matches!(
        response(&mut rx, 73).await,
        ServerMessage::Response { ok: true, .. }
    ));
    assert!(!memory.cancel.is_cancelled());
    memory.cancel.cancel();
    drop(peer);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    drop(rx);
    drop(memory);
    assert_eq!(f.daemon.relay_memory.usage(), (0, 0, 0));
}

#[tokio::test]
async fn relay_control_escape_diagnostic_is_refused_before_initial_typed_error_formatting() {
    let env = crate::credential_env::EnvGuard::isolated();
    let f = Fixture::new(&env);
    let (mut peer, rx, memory, task) = f.connection(crate::relay_buffers::OUTBOUND_MESSAGES);
    let (mut healthy, mut healthy_rx, healthy_memory, healthy_task) =
        f.connection(crate::relay_buffers::OUTBOUND_MESSAGES);
    let private = "\u{7f}".repeat(24 * 1024 * 1024);
    let mut line =
        serde_json::json!({"id":71,"protocol":private,"request":{"type":"ping"}}).to_string();
    assert!(line.len() < MAX_FRAME);
    line.push('\n');
    tokio::time::timeout(Duration::from_secs(5), peer.write_all(line.as_bytes()))
        .await
        .unwrap()
        .unwrap();
    // Wrong protocol type ordinarily yields a recoverable BAD_REQUEST. Here
    // the preflight must cancel BEFORE Serde constructs its expanded error.
    tokio::time::timeout(Duration::from_secs(5), memory.cancel.cancelled())
        .await
        .unwrap();
    request(&mut healthy, 100, Request::Ping).await;
    assert!(matches!(
        response(&mut healthy_rx, 100).await,
        ServerMessage::Response { ok: true, .. }
    ));
    assert!(!healthy_memory.cancel.is_cancelled());
    drop(peer);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    healthy_memory.cancel.cancel();
    drop(healthy);
    tokio::time::timeout(Duration::from_secs(5), healthy_task)
        .await
        .unwrap()
        .unwrap();
    drop((rx, memory, healthy_rx, healthy_memory));
    assert_eq!(f.daemon.relay_memory.usage(), (0, 0, 0));
}
