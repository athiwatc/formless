//! Native WebSocket supervisor. No per-client consumer is awaited in this loop.
use super::{daemon_authorization, key_from_secret, relay_ws_url, INTEGRITY_CLOSE};
use super::{
    endpoint::{Endpoint, Event, Input},
    transport::{self, OwnedPacket, Packet, Scheduler},
};
use crate::{
    daemon::Daemon,
    relay_buffers::{ClientMemory, MAX_CLIENTS},
};
use anyhow::{anyhow, ensure, Context, Result};
use formless_protocol::RelayClientInfo;
use formless_runtime::LockExt as _;
use futures_util::StreamExt as _;
use serde::Deserialize;
use serde_json::json;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{sync::mpsc, task::JoinSet};
use tokio_tungstenite::tungstenite::{protocol::WebSocketConfig, Message};
use tokio_util::sync::CancellationToken;

struct Client {
    g: String,
    memory: Arc<ClientMemory>,
    tx: mpsc::Sender<Input>,
}
struct Forget {
    daemon: Arc<Daemon>,
    cancel: CancellationToken,
}
impl Drop for Forget {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.daemon.relay_clients.locked().clear();
        *self.daemon.relay_kick.locked() = None;
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    #[serde(default)]
    relay: Option<String>,
    #[serde(default)]
    v: Option<u8>,
    #[serde(default)]
    g: Option<String>,
    #[serde(default)]
    cid: Option<String>,
    #[serde(default)]
    ua: Option<String>,
    #[serde(default)]
    f: Option<String>,
}
fn native_ack(g: &str, receipt: &str) -> String {
    json!({"relay":"ack_control","g":g,"receipt":receipt}).to_string()
}
fn disconnect(out: &Scheduler, cid: &str, code: u16) -> Result<()> {
    out.control(json!({"relay":"disconnect","cid":cid,"code":code}).to_string())
}
fn retire(clients: &mut HashMap<String, Client>, daemon: &Daemon, out: &Scheduler, cid: &str) {
    if let Some(client) = clients.remove(cid) {
        client.memory.cancel.cancel();
        out.retire(&transport::identity(cid, &client.g));
    }
    daemon.relay_clients.locked().remove(cid);
}

fn closed(
    clients: &mut HashMap<String, Client>,
    daemon: &Daemon,
    out: &Scheduler,
    cid: &str,
    g: &str,
    error: Option<String>,
) -> Result<()> {
    if clients.get(cid).is_some_and(|c| c.g == g) {
        retire(clients, daemon, out, cid);
        if let Some(error) = error {
            tracing::warn!("relay browser {cid} closed: {error}");
        }
        // Pool eviction and Core buffer overflow cancel without an actor error.
        // A ready Room/browser must still close instead of silently losing IPC.
        disconnect(out, cid, INTEGRITY_CLOSE)?;
    }
    Ok(())
}

pub(super) async fn connect(
    daemon: Arc<Daemon>,
    base_url: &str,
    secret: &str,
    shutdown: &CancellationToken,
) -> Result<()> {
    static CRYPTO: std::sync::Once = std::sync::Once::new();
    CRYPTO.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
    let key = key_from_secret(secret)?;
    let url = relay_ws_url(base_url, secret)?;
    // Notifications during the HTTP upgrade must not be missed.
    let mut attention = daemon.attention.subscribe();
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    let mut request = url.as_str().into_client_request()?;
    request.headers_mut().insert(
        tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
        daemon_authorization(&key),
    );
    let config = WebSocketConfig {
        write_buffer_size: 0,
        max_write_buffer_size: 2 * transport::WIRE,
        max_message_size: Some(transport::WIRE),
        max_frame_size: Some(transport::WIRE),
        ..WebSocketConfig::default()
    };
    let (ws, _) = tokio::select! {
        _=shutdown.cancelled()=>return Ok(()),
        result=tokio::time::timeout(transport::HANDSHAKE_TIMEOUT,tokio_tungstenite::connect_async_with_config(request,Some(config),false))=>result.context("relay HTTP upgrade deadline")?.with_context(||format!("connect relay {base_url}"))?,
    };
    let (sink, mut read) = ws.split();
    let cancel = shutdown.child_token();
    let _forget = Forget {
        daemon: daemon.clone(),
        cancel: cancel.clone(),
    };
    let out = Arc::new(Scheduler::default());
    let mut writer = tokio::spawn(transport::write_socket(sink, out.clone(), cancel.clone()));
    let (events, mut event_rx) = mpsc::channel::<Event>(2 * MAX_CLIENTS);
    let (kick_tx, mut kick_rx) = mpsc::channel::<String>(crate::relay_buffers::KICK_COMMANDS);
    *daemon.relay_kick.locked() = Some(kick_tx);
    let mut clients = HashMap::<String, Client>::new();
    let mut actors = JoinSet::new();
    let mut native = None::<String>;
    let mut last_push = None::<tokio::time::Instant>;
    let welcome_deadline = tokio::time::Instant::now() + transport::HANDSHAKE_TIMEOUT;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut writer_exit = None;
    let mut writer_finished = false;
    let mut result:Result<()>=async {
        loop {
            tokio::select! {
                _=cancel.cancelled()=>return Ok(()),
                result=&mut writer=>{
                    writer_finished=true;
                    writer_exit=Some(result.context("relay socket writer panicked")?);
                    // The clean writer exit and shutdown readiness can race in
                    // select!. Only an actually canceled link makes it normal.
                    if cancel.is_cancelled()&&writer_exit.as_ref().is_some_and(|exit|exit.error.is_none()){return Ok(())}
                    return Err(anyhow!(writer_exit.as_ref().and_then(|e|e.error.clone()).unwrap_or_else(||"relay socket writer ended".to_owned())));
                },
                _=tick.tick()=>ensure!(native.is_some()||tokio::time::Instant::now()<welcome_deadline,"relay transport welcome deadline"),
                Some(result)=actors.join_next(),if !actors.is_empty()=>{result.context("relay client actor panicked")?;},
                Some(event)=event_rx.recv()=>match event {
                    Event::Ready{cid,g,ua}=>{
                        if clients.get(&cid).is_some_and(|c|c.g==g&&!c.memory.cancel.is_cancelled()){
                            let now=chrono::Utc::now().to_rfc3339();
                            daemon.relay_clients.locked().insert(cid.clone(),RelayClientInfo{cid,user_agent:ua,first_seen:now.clone(),last_seen:now});
                        }
                    },
                    Event::Closed{cid,g,error}=>{
                        closed(&mut clients,&daemon,&out,&cid,&g,error)?;
                    },
                },
                Some(cid)=kick_rx.recv()=>{
                    if clients.contains_key(&cid){retire(&mut clients,&daemon,&out,&cid);disconnect(&out,&cid,4001)?;}
                },
                Ok(())=attention.recv()=>{
                    if last_push.is_none_or(|at|at.elapsed()>=Duration::from_secs(30)){
                        last_push=Some(tokio::time::Instant::now());out.control(r#"{"relay":"push"}"#.to_owned())?;
                    }
                },
                message=read.next()=>{
                    let text=match message {
                        Some(Ok(Message::Text(text)))=>text,
                        Some(Ok(Message::Close(_)))|None=>return Ok(()),
                        Some(Ok(Message::Ping(_)|Message::Pong(_)))=>continue,
                        Some(Ok(_))=>return Err(anyhow!("relay requires text frames")),
                        Some(Err(e))=>return Err(anyhow!("relay websocket: {e}")),
                    };
                    let packet=Packet::parse(&text)?;
                    drop(text);
                    if let Packet::Control{g,receipt,f}=packet {
                        let control:Control=serde_json::from_str(&f)?;
                        drop(f);
                        if native.is_none(){
                            ensure!(control.relay.as_deref()==Some("transport")&&control.v==Some(1)&&control.g.as_deref()==Some(&g)&&control.cid.is_none(),"invalid relay transport welcome");
                            native=Some(g.clone());out.control(native_ack(&g,&receipt))?;continue;
                        }
                        ensure!(native.as_deref()==Some(&g),"relay native generation changed");
                        let ack=native_ack(&g,&receipt);
                        let cid=control.cid.as_deref().ok_or_else(||anyhow!("relay control lacks CID"))?;
                        ensure!(transport::cid(cid),"invalid relay CID");
                        let cg=control.g.as_deref().ok_or_else(||anyhow!("relay control lacks client generation"))?;
                        ensure!(transport::generation(cg),"invalid relay client generation");
                        match control.relay.as_deref(){
                            Some("client_open")=>{
                                // Duplicate open is not a new handshake epoch and cannot
                                // reset its absolute deadline or consume another slot.
                                if let Some(client)=clients.get(cid){ensure!(client.g==cg,"relay CID reused without close");out.control(ack)?;continue;}
                                while let Some(joined)=actors.try_join_next(){joined.context("relay client actor panicked")?;}
                                let memory=match daemon.relay_memory.client(cancel.child_token()){
                                    Ok(memory)=>memory,
                                    Err(_)=>{disconnect(&out,cid,1008)?;out.control(ack)?;continue;},
                                };
                                let key_id=transport::identity(cid,cg);
                                out.register(key_id)?;
                                let (tx,input)=mpsc::channel(2*transport::WINDOW);
                                let endpoint=Endpoint{cid:cid.to_owned(),g:cg.to_owned(),ua:control.ua.unwrap_or_default().chars().take(200).collect(),memory:memory.clone(),opened:tokio::time::Instant::now(),link_cancel:cancel.clone(),input,out:out.clone(),events:events.clone(),key:key.clone(),daemon:daemon.clone()};
                                clients.insert(cid.to_owned(),Client{g:cg.to_owned(),memory,tx});actors.spawn(endpoint.run());out.control(ack)?;
                            },
                            Some("client_close")=>{
                                if clients.get(cid).is_some_and(|c|c.g==cg){retire(&mut clients,&daemon,&out,cid);}
                                out.control(ack)?;
                            },
                            Some("ready")|None=>{
                                let Some(client)=clients.get(cid).filter(|c|c.g==cg) else{out.control(ack)?;continue;};
                                let input=if control.relay.is_some(){Input::Ready{receipt:ack.clone()}}else{Input::Handshake{frame:control.f.ok_or_else(||anyhow!("relay handshake missing frame"))?,receipt:ack.clone()}};
                                if let Err(error)=client.tx.try_send(input){
                                    drop(error);retire(&mut clients,&daemon,&out,cid);disconnect(&out,cid,1008)?;out.control(ack)?;
                                }
                            },
                            _=>return Err(anyhow!("unknown relay control")),
                        }
                    }else{
                        let (cid,g)=packet.target().expect("non-control target");
                        let cid=cid.to_owned(); let g=g.to_owned();
                        let Some(client)=clients.get(&cid).filter(|c|c.g==g&&!c.memory.cancel.is_cancelled()) else{
                            if let Packet::Delivery{receipt,..}=packet{out.control(json!({"relay":"ack","cid":cid,"g":g,"receipt":receipt}).to_string())?;}
                            continue;
                        };
                        // The shared reader holds only one WIRE-sized transient parse;
                        // queued copies require a CID lease before queue admission.
                        let lease=client.memory.reserve(packet.retained_charge());
                        let queued=match lease {
                            Ok(lease)=>client.tx.try_send(Input::Packet(OwnedPacket{packet,_lease:lease})).err().map(|e|e.into_inner()),
                            Err(_)=>Some(Input::Packet(OwnedPacket{packet,_lease:client.memory.reserve(0)?})),
                        };
                        if let Some(input)=queued{
                            let receipt=match input {Input::Packet(OwnedPacket{packet:Packet::Delivery{receipt,..},..})=>Some(receipt),_=>None};
                            retire(&mut clients,&daemon,&out,&cid);disconnect(&out,&cid,1008)?;
                            if let Some(receipt)=receipt{out.control(json!({"relay":"ack","cid":cid,"g":g,"receipt":receipt}).to_string())?;}
                        }else if let Some(info)=daemon.relay_clients.locked().get_mut(&cid){info.last_seen=chrono::Utc::now().to_rfc3339();}
                    }
                }
            }
        }
    }.await;
    // Drop BOTH socket halves before refunding any in-flight native write lease.
    // SplitSink alone shares tungstenite's pending write allocation with read.
    drop(read);
    cancel.cancel();
    if !writer_finished {
        match writer.await {
            Ok(exit) => writer_exit = Some(exit),
            Err(error) => {
                result = Err(anyhow!(
                    "relay socket writer panicked during close: {error}"
                ))
            }
        }
    }
    drop(writer_exit);
    for (cid, client) in clients.drain() {
        client.memory.cancel.cancel();
        out.retire(&transport::identity(&cid, &client.g));
    }
    daemon.relay_clients.locked().clear();
    // No actor can remain parked reporting Ready/Closed after routing ends.
    drop(event_rx);
    while let Some(joined) = actors.join_next().await {
        if let Err(e) = joined {
            tracing::warn!("relay client cleanup failed: {e}");
        }
    }
    result
}

#[cfg(test)]
#[path = "../tests/support/relay_peer.rs"]
mod test_peer;

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn internal_memory_cancellation_disconnects_ready_room_peer_and_removes_device() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let base = std::env::temp_dir().join(format!("fl-relay-cancel-{}", uuid::Uuid::new_v4()));
        let paths = crate::Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        let memory = daemon
            .relay_memory
            .client(CancellationToken::new())
            .unwrap();
        let g = "b".repeat(32);
        let out = Arc::new(Scheduler::default());
        out.register(transport::identity("one", &g)).unwrap();
        let (tx, input) = mpsc::channel(1);
        let (events, mut rx) = mpsc::channel(2);
        let mut clients = HashMap::new();
        clients.insert(
            "one".into(),
            Client {
                g: g.clone(),
                memory: memory.clone(),
                tx,
            },
        );
        daemon.relay_clients.locked().insert(
            "one".into(),
            RelayClientInfo {
                cid: "one".into(),
                user_agent: "private fixture".into(),
                first_seen: "now".into(),
                last_seen: "now".into(),
            },
        );
        let endpoint = Endpoint {
            cid: "one".into(),
            g: g.clone(),
            ua: String::new(),
            memory: memory.clone(),
            opened: tokio::time::Instant::now(),
            link_cancel: CancellationToken::new(),
            input,
            out: out.clone(),
            events,
            key: zeroize::Zeroizing::new([7; 32]),
            daemon: daemon.clone(),
        };
        // Same cancellation path used by Core queue/budget failures and shared
        // pool eviction: no actor error is required to close its Room peer.
        memory.cancel.cancel();
        endpoint.run().await;
        let Event::Closed { cid, g, error } = rx.recv().await.unwrap() else {
            panic!("closed lifecycle event")
        };
        assert!(error.is_none());
        closed(&mut clients, &daemon, &out, &cid, &g, error).unwrap();
        assert!(clients.is_empty());
        assert!(daemon.relay_clients.locked().is_empty());
        let frame: serde_json::Value = serde_json::from_str(&out.pop().unwrap().text).unwrap();
        assert_eq!(frame["relay"], "disconnect");
        assert_eq!(frame["cid"], "one");
        assert_eq!(frame["code"], INTEGRITY_CLOSE);
        // An old close cannot disconnect a later incarnation with the same CID.
        closed(&mut clients, &daemon, &out, &cid, &g, None).unwrap();
        assert!(out.pop().is_none());
        drop(daemon);
        std::fs::remove_dir_all(base).unwrap();
    }
    struct StoreGate {
        release: Option<std::sync::mpsc::Sender<()>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for StoreGate {
        fn drop(&mut self) {
            if let Some(release) = self.release.take() {
                let _ = release.send(());
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
    async fn store_gate(daemon: Arc<Daemon>) -> StoreGate {
        let (ready, waiting) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _store = daemon.store.locked();
            let _ = ready.send(());
            let _ = gate.recv_timeout(Duration::from_secs(30));
        });
        let guard = StoreGate {
            release: Some(release),
            thread: Some(thread),
        };
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap();
        guard
    }
    async fn browser(
        peer: &mut super::test_peer::RoomPeer,
        cid: &str,
        key: &[u8; 32],
    ) -> super::super::Channel {
        let bh = super::super::new_hello();
        peer.send(Message::Text(
            json!({"cid":cid,"f":super::super::hello_frame(&bh)}).to_string(),
        ))
        .await
        .unwrap();
        let reply = peer.next().await.unwrap().unwrap();
        let reply: serde_json::Value = serde_json::from_str(reply.to_text().unwrap()).unwrap();
        assert_eq!(reply["cid"], cid);
        let dh = super::super::parse_hello(reply["f"].as_str().unwrap()).unwrap();
        peer.send(Message::Text(
            json!({"cid":cid,"f":super::super::confirm_frame(key,&bh,&dh)}).to_string(),
        ))
        .await
        .unwrap();
        super::super::Channel::new(key, &bh, &dh, super::super::Side::Browser)
    }
    async fn send_rpc(
        peer: &mut super::test_peer::RoomPeer,
        cid: &str,
        channel: &mut super::super::Channel,
        id: u64,
        request: serde_json::Value,
    ) {
        let line = json!({"id":id,"protocol":1,"request":request}).to_string();
        let frame = channel.seal(&line).unwrap();
        peer.send(Message::Text(json!({"cid":cid,"f":frame}).to_string()))
            .await
            .unwrap();
    }
    async fn response(
        peer: &mut super::test_peer::RoomPeer,
        cid: &str,
        channel: &mut super::super::Channel,
        id: u64,
    ) -> serde_json::Value {
        loop {
            let frame = peer.next().await.unwrap().unwrap();
            let frame: serde_json::Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
            assert_eq!(frame["cid"], cid, "other peer must not delay this response");
            let line = channel
                .open(frame["f"].as_str().expect("encrypted response"))
                .unwrap();
            let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
            if reply["kind"] == "response" && reply["id"] == id {
                return reply;
            }
        }
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn native_stalled_cid_expires_without_blocking_healthy_cid_or_refunding_orphan_buffers_early(
    ) {
        let env = crate::credential_env::EnvGuard::isolated();
        env.remove("FORMLESS_SECURE");
        let base = std::env::temp_dir().join(format!("fl-relay-fair-{}", uuid::Uuid::new_v4()));
        let paths = crate::Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
        let mut cfg = crate::Config::default();
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        let sid = daemon
            .with_store(|store| {
                let session = store.create_session(None)?;
                store.append_message(&session.id, "user", &"private".repeat(160_000))?;
                Ok(session.id)
            })
            .unwrap();
        let secret = crate::config::generate_relay_secret();
        let key = key_from_secret(&secret).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let shutdown = CancellationToken::new();
        let link = tokio::spawn({
            let (d, s, t) = (daemon.clone(), secret.clone(), shutdown.clone());
            async move { connect(d, &url, &s, &t).await }
        });
        let (tcp, _) = listener.accept().await.unwrap();
        let mut peer = super::test_peer::RoomPeer::accept(tcp).await;
        let mut a = browser(&mut peer, "slow", &key).await;
        let mut b = browser(&mut peer, "healthy", &key).await;
        peer.withhold_credits("slow");
        send_rpc(
            &mut peer,
            "slow",
            &mut a,
            1,
            json!({"type":"get_session","id":sid}),
        )
        .await;
        peer.wait_withheld_window("slow").await;
        // This blocks only the private profile's FIFO worker. Core's reader
        // and its separate Ping/control lane remain installed and bounded.
        let gate = store_gate(daemon.clone()).await;
        for id in 10..20 {
            send_rpc(
                &mut peer,
                "slow",
                &mut a,
                id,
                json!({"type":"create_session"}),
            )
            .await;
        }
        let mut line =
            json!({"id":44,"protocol":1,"request":{"type":"ping"},"padding":"p".repeat(1024*1024)})
                .to_string();
        let cipher = a.seal(&line).unwrap();
        line.clear();
        drop(line);
        peer.send(Message::Text(json!({"cid":"slow","f":cipher}).to_string()))
            .await
            .unwrap();
        peer.drain_up_to(1).await;
        assert_eq!(
            peer.pending_up(),
            1,
            "final UP receipt waits for the blocked IPC consumer"
        );
        send_rpc(&mut peer, "healthy", &mut b, 900, json!({"type":"ping"})).await;
        let healthy = tokio::time::timeout(
            Duration::from_secs(3),
            response(&mut peer, "healthy", &mut b, 900),
        )
        .await
        .expect("healthy CID progresses while slow destination credit is withheld");
        assert_eq!(healthy["ok"], true);
        assert_eq!(
            peer.pending_up(),
            1,
            "healthy response did not refund the slow CID's retained UP buffer"
        );
        // Do not advance or weaken the production10s credit deadline. Drain the
        // shared TCP socket while withholding only this logical destination.
        let expired = tokio::time::timeout(Duration::from_secs(12), async {
            loop {
                let frame = peer.next().await.unwrap().unwrap();
                let frame: serde_json::Value =
                    serde_json::from_str(frame.to_text().unwrap()).unwrap();
                if frame["relay"] == "disconnect" {
                    break frame;
                }
            }
        })
        .await
        .expect("stalled CID reaches unchanged production credit deadline");
        assert_eq!(expired["cid"], "slow");
        assert_eq!(expired["code"], INTEGRITY_CLOSE);
        peer.drain_up_to(0).await;
        assert_eq!(
            peer.pending_up(),
            0,
            "discarded final UP buffer acknowledged after cancellation"
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            while daemon.subscriber_count() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(daemon.relay_clients.locked().len(), 1);
        assert!(daemon.relay_clients.locked().contains_key("healthy"));
        assert_eq!(
            daemon.relay_memory.usage().0,
            2,
            "closing Core owner cannot recycle its slot before blocked worker/queued leases drop"
        );
        assert!(daemon.relay_memory.usage().1 + daemon.relay_memory.usage().2 > 0);
        send_rpc(&mut peer, "healthy", &mut b, 901, json!({"type":"ping"})).await;
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(3),
                response(&mut peer, "healthy", &mut b, 901)
            )
            .await
            .unwrap()["ok"],
            true
        );
        drop(gate);
        tokio::time::timeout(Duration::from_secs(5), async {
            while daemon.relay_memory.usage().0 != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned slow Core cleanup completes after private Store gate releases");
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(5), link)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(daemon.subscriber_count(), 0);
        assert_eq!(daemon.relay_memory.usage(), (0, 0, 0));
        drop(daemon);
        std::fs::remove_dir_all(base).unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn native_clean_shutdown_accepts_the_already_completed_writer_cancellation() {
        let env = crate::credential_env::EnvGuard::isolated();
        env.remove("FORMLESS_SECURE");
        let base = std::env::temp_dir().join(format!("fl-relay-shutdown-{}", uuid::Uuid::new_v4()));
        let paths = crate::Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
        let mut cfg = crate::Config::default();
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        let secret = crate::config::generate_relay_secret();
        for _ in 0..16 {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let shutdown = CancellationToken::new();
            let link = tokio::spawn({
                let (d, s, t) = (daemon.clone(), secret.clone(), shutdown.clone());
                async move { connect(d, &url, &s, &t).await }
            });
            let (tcp, _) = listener.accept().await.unwrap();
            let mut peer = super::test_peer::RoomPeer::accept(tcp).await;
            peer.wait_welcome_ack().await; // writer must really be installed.
            shutdown.cancel();
            tokio::time::timeout(Duration::from_secs(5), link)
                .await
                .unwrap()
                .unwrap()
                .expect("explicit native shutdown is clean whichever select branch wins");
            assert_eq!(daemon.subscriber_count(), 0);
            assert_eq!(daemon.relay_memory.usage(), (0, 0, 0));
        }
        drop(daemon);
        std::fs::remove_dir_all(base).unwrap();
    }
}
