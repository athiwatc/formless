//! One independently cancellable actor per actual browser generation.
use super::transport::{self, Assembly, Complete, OwnedPacket, Packet, Scheduler, Sending};
use super::{
    confirm_tag, daemon_hello_frame, new_hello, parse_confirm, parse_hello, Channel, Side,
};
use crate::{
    daemon::Daemon,
    relay_buffers::{ClientMemory, RelayOutput, Serialized, OUTBOUND_MESSAGES},
};
use anyhow::{anyhow, ensure, Result};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, sync::mpsc, task::JoinHandle};
use zeroize::Zeroizing;

pub(super) enum Input {
    Handshake { frame: String, receipt: String },
    Ready { receipt: String },
    Packet(OwnedPacket),
}
pub(super) enum Event {
    Ready {
        cid: String,
        g: String,
        ua: String,
    },
    Closed {
        cid: String,
        g: String,
        error: Option<String>,
    },
}
pub(super) struct Endpoint {
    pub cid: String,
    pub g: String,
    pub ua: String,
    pub memory: Arc<ClientMemory>,
    pub opened: tokio::time::Instant,
    pub link_cancel: tokio_util::sync::CancellationToken,
    pub input: mpsc::Receiver<Input>,
    pub out: Arc<Scheduler>,
    pub events: mpsc::Sender<Event>,
    pub key: Zeroizing<[u8; 32]>,
    pub daemon: Arc<Daemon>,
}
struct Core {
    memory: Arc<ClientMemory>,
    task: Option<JoinHandle<()>>,
    write: Option<tokio::io::WriteHalf<tokio::io::DuplexStream>>,
    output: mpsc::Receiver<Serialized>,
}
impl Core {
    fn start(daemon: Arc<Daemon>, memory: Arc<ClientMemory>) -> Self {
        let (relay, daemon_side) = tokio::io::duplex(64 * 1024);
        let (read, write) = tokio::io::split(relay);
        drop(read);
        let (tx, output) = mpsc::channel(OUTBOUND_MESSAGES);
        let task = tokio::spawn(daemon.handle_conn_relay(
            daemon_side,
            RelayOutput {
                tx,
                memory: memory.clone(),
            },
        ));
        Self {
            memory,
            task: Some(task),
            write: Some(write),
            output,
        }
    }
    async fn write(
        &mut self,
        frame: Complete,
        credit_deadline: Option<tokio::time::Instant>,
    ) -> Result<()> {
        self.memory.waiting(true);
        let write = self
            .write
            .as_mut()
            .ok_or_else(|| anyhow!("relay IPC closed"))?;
        let deadline = credit_deadline.map_or(
            tokio::time::Instant::now() + transport::ACK_TIMEOUT,
            |credit| credit.min(tokio::time::Instant::now() + transport::ACK_TIMEOUT),
        );
        ensure!(
            tokio::time::Instant::now() < deadline,
            "relay credit/IPC deadline"
        );
        let result = tokio::select! {
            _=self.memory.cancel.cancelled()=>Err(anyhow!("relay IPC canceled")),
            result=tokio::time::timeout_at(deadline,async {write.write_all(frame.text.as_bytes()).await?;write.write_all(b"\n").await})=>result.map_err(|_|anyhow!("relay credit/IPC consumer deadline"))?.map_err(Into::into),
        };
        // Both plaintext and its lease drop before the final delivery ACK.
        drop(frame);
        self.memory.waiting(false);
        result
    }
    async fn close(mut self) {
        self.memory.cancel.cancel();
        drop(self.write.take());
        self.output.close();
        while self.output.try_recv().is_ok() {}
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        while self.output.try_recv().is_ok() {}
    }
}
impl Drop for Core {
    fn drop(&mut self) {
        self.memory.cancel.cancel();
    }
}

// Error text is metadata, not an exemption from the endpoint queue bound.
// Format into a fresh bounded allocation: truncate() would retain an enormous
// attack-controlled serde diagnostic capacity after releasing its frame lease.
const DIAGNOSTIC_BYTES: usize = 512;
fn diagnostic(error: &anyhow::Error) -> String {
    struct Bounded(String);
    impl std::fmt::Write for Bounded {
        fn write_str(&mut self, text: &str) -> std::fmt::Result {
            let mut length = (DIAGNOSTIC_BYTES - self.0.len()).min(text.len());
            while !text.is_char_boundary(length) {
                length -= 1;
            }
            self.0.push_str(&text[..length]);
            Ok(())
        }
    }
    use std::fmt::Write as _;
    let mut output = Bounded(String::with_capacity(DIAGNOSTIC_BYTES));
    let _ = write!(&mut output, "{error}");
    output.0
}
async fn startup(memory: &ClientMemory, opened: tokio::time::Instant) -> Result<()> {
    let deadline = opened + transport::HANDSHAKE_TIMEOUT;
    ensure!(
        tokio::time::Instant::now() < deadline,
        "relay Core startup deadline"
    );
    tokio::time::timeout_at(deadline, memory.started())
        .await
        .map_err(|_| anyhow!("relay Core startup deadline"))??;
    ensure!(
        tokio::time::Instant::now() < deadline,
        "relay Core startup deadline"
    );
    Ok(())
}

impl Endpoint {
    fn ack(&self, receipt: &str) -> Result<()> {
        self.out
            .control(json!({"relay":"ack","cid":self.cid,"g":self.g,"receipt":receipt}).to_string())
    }
    // Handshake/ready control receipts belong to the native DS generation,
    // attached by the manager. The actor emits a typed completion instead of
    // guessing the native generation from the CID generation.
    fn control_ack(&self, receipt: &str) -> Result<()> {
        self.out.control(receipt.to_owned())
    }
    fn raw(&self, frame: String) -> Result<()> {
        let lease = self.memory.reserve(4 * transport::PRE_READY)?;
        self.out.data(
            &transport::identity(&self.cid, &self.g),
            json!({"cid":self.cid,"g":self.g,"f":frame}).to_string(),
            lease,
        )
    }
    pub async fn run(mut self) {
        let mut core = None;
        let result = self.work(&mut core).await;
        self.memory.cancel.cancel();
        self.out.retire(&transport::identity(&self.cid, &self.g));
        // Remove the listed device now; cleanup may wait for a canceled Core
        // worker to release Store ownership. Its leases/slot remain charged.
        let error = result.err().map(|e| diagnostic(&e));
        let closed = Event::Closed {
            cid: self.cid.clone(),
            g: self.g.clone(),
            error,
        };
        tokio::select! {
            _=self.link_cancel.cancelled()=>{},
            _=self.events.send(closed)=>{},
        }
        // Drain only after each queued delivery and local buffer is dropped.
        // A retired endpoint STILL ACKs valid receipts to release orphan UP
        // charges on the Room; source grants never serve as drainage proof.
        self.input.close();
        let mut receipts = Vec::new();
        while let Ok(input) = self.input.try_recv() {
            match input {
                Input::Packet(OwnedPacket {
                    packet: Packet::Delivery { receipt, .. },
                    _lease,
                }) => {
                    drop(_lease);
                    receipts.push((false, receipt));
                }
                Input::Handshake { receipt, .. } | Input::Ready { receipt } => {
                    receipts.push((true, receipt))
                }
                _ => {}
            }
        }
        if let Some(core) = core.take() {
            core.close().await;
        }
        for (control, receipt) in receipts {
            let _ = if control {
                self.control_ack(&receipt)
            } else {
                self.ack(&receipt)
            };
        }
    }
    async fn work(&mut self, core: &mut Option<Core>) -> Result<()> {
        self.exchange(core, None, false).await
    }
    async fn exchange(
        &mut self,
        core: &mut Option<Core>,
        mut channel: Option<Channel>,
        mut ready: bool,
    ) -> Result<()> {
        let opened = self.opened;
        let mut seen = opened;
        let mut pending: Option<(Vec<u8>, [u8; 16], String)> = None;
        let mut known_hello = None;
        let mut assembly: Option<Assembly> = None;
        let mut sending: Option<Sending> = None;
        let mut send_id = 0;
        let key = transport::identity(&self.cid, &self.g);
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                biased;
                _=self.memory.cancel.cancelled()=>return Ok(()),
                _=tick.tick()=>{
                    let now=tokio::time::Instant::now();
                    ensure!(ready||now-opened<transport::HANDSHAKE_TIMEOUT,"relay handshake/readiness deadline");
                    ensure!(now-seen<Duration::from_secs(300),"relay browser idle deadline");
                    ensure!(assembly.as_ref().is_none_or(|a|now<a.deadline),"relay assembly deadline");
                    ensure!(sending.as_ref().and_then(Sending::deadline).is_none_or(|at|now<at),"relay grant/credit deadline");
                },
                input=self.input.recv()=>{
                    let Some(input)=input else{return Ok(())}; seen=tokio::time::Instant::now();
                    match input {
                        Input::Handshake{frame,receipt}=>{
                            let result=async {
                                ensure!(frame.len()<=transport::PRE_READY,"relay handshake limit");
                                if let Some(browser)=parse_hello(&frame) {
                                    if channel.is_some() {
                                        ensure!(known_hello.as_ref()==Some(&browser),"new hello requires a fresh browser generation");
                                        // Late duplicate has no reply/rekey side effect.
                                    } else if let Some((known,_,reply))=&pending {
                                        ensure!(known==&browser,"changed pending hello requires reconnect");
                                        self.raw(reply.clone())?;
                                    } else {
                                        let daemon_hello=new_hello();
                                        let reply=daemon_hello_frame(&self.key,&browser,&daemon_hello);
                                        self.raw(reply.clone())?; pending=Some((browser,daemon_hello,reply));
                                    }
                                } else if let Some(tag)=parse_confirm(&frame) {
                                    if channel.is_none() {
                                        let (browser,daemon_hello,_)=pending.as_ref().ok_or_else(||anyhow!("confirmation before hello"))?;
                                        if tag!=confirm_tag(&self.key,browser,daemon_hello,Side::Browser) {
                                            // Failed proof does not open Core or reset the
                                            // absolute handshake deadline. A retry can still
                                            // prove this same pending transcript.
                                            tracing::warn!("relay browser failed key confirmation");
                                            return Ok(());
                                        }
                                        channel=Some(Channel::new(&self.key,browser,daemon_hello,Side::Daemon)); known_hello=Some(browser.clone()); pending=None;
                                        *core=Some(Core::start(self.daemon.clone(),self.memory.clone()));
                                        startup(&self.memory,opened).await?;
                                        self.out.control(json!({"relay":"ready","cid":self.cid,"g":self.g}).to_string())?;
                                    }
                                } else {return Err(anyhow!("invalid relay handshake"))}
                                Ok(())
                            }.await;
                            drop(frame); self.control_ack(&receipt)?; result?;
                        },
                        Input::Ready{receipt}=>{
                            let result=async {
                                ensure!(channel.is_some()&&core.is_some(),"room ready before key proof");
                                startup(&self.memory,opened).await?;
                                if !ready {
                                    tokio::select! {
                                        _=self.memory.cancel.cancelled()=>return Err(anyhow!("relay readiness canceled")),
                                        result=self.events.send(Event::Ready{cid:self.cid.clone(),g:self.g.clone(),ua:self.ua.clone()})=>result.map_err(|_|anyhow!("relay link closed"))?,
                                    }
                                    ready=true;
                                }
                                Ok(())
                            }.await;
                            self.control_ack(&receipt)?;result?;
                        },
                        Input::Packet(OwnedPacket{packet,_lease})=>match packet {
                            Packet::Delivery{receipt,fragment,..}=>{
                                let result=async {
                                    ensure!(ready,"bulk relay data before readiness");
                                    if assembly.is_none(){assembly=Some(Assembly::first(&fragment,&self.memory).await?)}
                                    let finished=assembly.as_mut().expect("installed").push(fragment)?;
                                    if finished {
                                        let complete=assembly.take().expect("installed").finish();
                                        let plain=transport::decrypt(channel.as_mut().expect("proved"),complete)?;
                                        core.as_mut().expect("started").write(plain,sending.as_ref().and_then(Sending::deadline)).await?;
                                    }
                                    Ok(())
                                }.await;
                                // Any failed partial assembly is dropped before its final
                                // receipt is acknowledged as discarded, never on enqueue.
                                if result.is_err(){drop(assembly.take());}
                                drop(_lease); self.ack(&receipt)?; result?;
                            },
                            Packet::Grant{send_id:id,permit,..}=>{
                                ensure!(ready,"relay grant before readiness");
                                sending.as_mut().ok_or_else(||anyhow!("unsolicited relay grant"))?.grant(id,permit,&self.cid,&self.g,&key,&self.memory,&self.out)?;
                            },
                            Packet::Credit{send_id:id,..}=>{
                                let sending=sending.as_mut().ok_or_else(||anyhow!("unsolicited relay credit"))?;
                                sending.credit(id)?;sending.fill(&self.cid,&self.g,&key,&mut send_id,&self.memory,&self.out)?;
                                if sending.done(){self.memory.waiting(false);}
                            },
                            Packet::Control{..}=>return Err(anyhow!("unrouted relay control")),
                        }
                    }
                    if sending.as_ref().is_some_and(Sending::done){sending=None;}
                    // Consuming a packet is progress, but a successful IPC write
                    // must not erase a simultaneous blocked DOWN-credit wait.
                    self.memory.waiting(false);
                    self.memory.waiting(sending.is_some()||assembly.is_some());
                },
                line=async {core.as_mut().expect("guarded").output.recv().await},if ready&&sending.is_none()&&assembly.is_none()&&core.is_some()=>{
                    let Some(line)=line else{return Err(anyhow!("relay Core output closed"))};
                    let mut next=Sending::seal(channel.as_mut().expect("proved"),line,&self.memory).await?;
                    next.fill(&self.cid,&self.g,&key,&mut send_id,&self.memory,&self.out)?;sending=Some(next);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{relay_buffers::Pool, Paths};
    use tokio_util::sync::CancellationToken;
    fn private_daemon() -> (Arc<Daemon>, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("fl-relay-actor-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(base.join("d"), base.join("c")).unwrap();
        (Arc::new(Daemon::new(paths).unwrap()), base)
    }
    #[tokio::test]
    async fn canceled_actor_drains_receipts_without_parking_on_full_lifecycle_queue() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let (daemon, base) = private_daemon();
        let pool = Arc::new(Pool::default());
        let memory = pool.client(CancellationToken::new()).unwrap();
        let link_cancel = CancellationToken::new();
        let (events, mut event_rx) = mpsc::channel(1);
        events
            .try_send(Event::Closed {
                cid: "prior".into(),
                g: "a".repeat(32),
                error: None,
            })
            .unwrap_or_else(|_| panic!("private queue available"));
        let (tx, input) = mpsc::channel(1);
        let g = "b".repeat(32);
        let receipt = "c".repeat(22);
        tx.try_send(Input::Packet(OwnedPacket {
            packet: Packet::Delivery {
                g: g.clone(),
                cid: "one".into(),
                receipt: receipt.clone(),
                fragment: transport::Fragment {
                    relay: "data".into(),
                    v: 1,
                    id: "d".repeat(32),
                    index: 0,
                    bytes: transport::CHUNK,
                    data: "x".repeat(transport::CHUNK),
                },
            },
            _lease: memory.reserve(2 * transport::WIRE).unwrap(),
        }))
        .unwrap_or_else(|_| panic!("private queue available"));
        let out = Arc::new(Scheduler::default());
        out.register(transport::identity("one", &g)).unwrap();
        let endpoint = Endpoint {
            cid: "one".into(),
            g: g.clone(),
            ua: String::new(),
            memory: memory.clone(),
            opened: tokio::time::Instant::now(),
            link_cancel: link_cancel.clone(),
            input,
            out: out.clone(),
            events,
            key: Zeroizing::new([7; 32]),
            daemon: daemon.clone(),
        };
        memory.cancel.cancel();
        link_cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), endpoint.run())
            .await
            .expect("canceled cleanup cannot await a full event queue");
        assert_eq!(
            pool.usage(),
            (1, 0, 0),
            "discarded transport lease released before ACK"
        );
        let ack = out.pop().expect("discard receipt still acknowledged");
        let ack: serde_json::Value = serde_json::from_str(&ack.text).unwrap();
        assert_eq!(ack["relay"], "ack");
        assert_eq!(ack["g"], g);
        assert_eq!(ack["receipt"], receipt);
        assert!(matches!(event_rx.try_recv(),Ok(Event::Closed{cid,..})if cid=="prior"));
        assert_eq!(daemon.subscriber_count(), 0);
        drop(daemon);
        std::fs::remove_dir_all(base).unwrap();
    }
    #[tokio::test]
    async fn expired_open_epoch_cannot_gain_core_subscriber_or_reset_deadline() {
        let _env = crate::credential_env::EnvGuard::isolated();
        let (daemon, base) = private_daemon();
        let pool = Arc::new(Pool::default());
        let memory = pool.client(CancellationToken::new()).unwrap();
        let (tx, input) = mpsc::channel(1);
        let (events, mut event_rx) = mpsc::channel(1);
        let out = Arc::new(Scheduler::default());
        let g = "b".repeat(32);
        out.register(transport::identity("one", &g)).unwrap();
        tx.try_send(Input::Handshake {
            frame: super::super::hello_frame(&new_hello()),
            receipt: "private-prewrapped-ack".into(),
        })
        .unwrap_or_else(|_| panic!("private queue available"));
        let endpoint = Endpoint {
            cid: "one".into(),
            g,
            ua: String::new(),
            memory: memory.clone(),
            opened: tokio::time::Instant::now() - transport::HANDSHAKE_TIMEOUT,
            link_cancel: CancellationToken::new(),
            input,
            out,
            events,
            key: Zeroizing::new([7; 32]),
            daemon: daemon.clone(),
        };
        tokio::time::timeout(Duration::from_secs(1), endpoint.run())
            .await
            .unwrap();
        assert!(
            matches!(event_rx.recv().await,Some(Event::Closed{error:Some(error),..})if error.contains("handshake/readiness deadline"))
        );
        assert_eq!(daemon.subscriber_count(), 0);
        assert_eq!(pool.usage(), (1, 0, 0));
        drop(daemon);
        std::fs::remove_dir_all(base).unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn partial_input_and_queued_large_outputs_make_progress_under_real_byte_pressure() {
        // The deadlock is structural, not a crypto throughput assertion.
        // Seventeen real queued 8MiB replies retain272MiB of producer leases;
        // a partial4MiB input then excludes a32MiB sealing reservation. Once
        // that input drains, the exact same reservation fits. Independent
        // fixtures exercise actual32MiB cipher/RPC boundaries in both directions.
        const INPUT: usize = 4 * 1024 * 1024;
        const OUTPUT: usize = 8 * 1024 * 1024;
        const QUEUED: usize = 17;
        let _env = crate::credential_env::EnvGuard::isolated();
        let (daemon, base) = private_daemon();
        let pool = Arc::new(Pool::default());
        let memory = pool.client(CancellationToken::new()).unwrap();
        let out = Arc::new(Scheduler::default());
        let g = "b".repeat(32);
        out.register(transport::identity("one", &g)).unwrap();
        let (tx, input) = mpsc::channel(1);
        let (events, _event_rx) = mpsc::channel(2);
        let key = [7; 32];
        let (b, d) = (new_hello(), new_hello());
        let mut browser = Channel::new(&key, &b, &d, Side::Browser);
        let encrypted = browser.seal(&"p".repeat(INPUT)).unwrap();
        let id = "d".repeat(32);
        let (relay, mut sink) = tokio::io::duplex(64 * 1024);
        let (read, write) = tokio::io::split(relay);
        drop(read);
        let read_progress = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let consumer = tokio::spawn({
            let progress = read_progress.clone();
            async move {
                use tokio::io::AsyncReadExt as _;
                let mut bytes = Vec::with_capacity(INPUT + 1);
                let mut chunk = [0u8; 64 * 1024];
                loop {
                    let n = sink.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    assert!(bytes.len() + n <= INPUT + 1);
                    bytes.extend_from_slice(&chunk[..n]);
                    progress.store(bytes.len(), std::sync::atomic::Ordering::Release);
                }
                bytes
            }
        });
        let (output_tx, output) = mpsc::channel(QUEUED);
        for _ in 0..QUEUED {
            output_tx
                .try_send(Serialized {
                    line: "r".repeat(OUTPUT),
                    lease: memory.reserve(2 * OUTPUT + 4096).unwrap(),
                })
                .unwrap_or_else(|_| panic!("private queue has bounded capacity"));
        }
        let mut core = Some(Core {
            memory: memory.clone(),
            task: None,
            write: Some(write),
            output,
        });
        let mut endpoint = Endpoint {
            cid: "one".into(),
            g: g.clone(),
            ua: String::new(),
            memory: memory.clone(),
            opened: tokio::time::Instant::now(),
            link_cancel: CancellationToken::new(),
            input,
            out: out.clone(),
            events,
            key: Zeroizing::new(key),
            daemon: daemon.clone(),
        };
        {
            let actor = endpoint.exchange(
                &mut core,
                Some(Channel::new(&key, &b, &d, Side::Daemon)),
                true,
            );
            tokio::pin!(actor);
            let mut early_needs = Vec::<transport::Wire>::new();
            for (index, data) in encrypted.as_bytes().chunks(transport::CHUNK).enumerate() {
                let receipt = format!("{index:022}");
                tx.try_send(Input::Packet(OwnedPacket {
                    packet: Packet::Delivery {
                        g: g.clone(),
                        cid: "one".into(),
                        receipt: receipt.clone(),
                        fragment: transport::Fragment {
                            relay: "data".into(),
                            v: 1,
                            id: id.clone(),
                            index,
                            bytes: encrypted.len(),
                            data: std::str::from_utf8(data).unwrap().into(),
                        },
                    },
                    _lease: memory.reserve(2 * transport::WIRE).unwrap(),
                }))
                .unwrap_or_else(|_| panic!("previous fragment consumed"));
                let fragment_started = std::time::Instant::now();
                tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        assert!(futures_util::poll!(&mut actor).is_pending());
                        if let Some(frame)=out.pop(){
                            let packet:serde_json::Value=serde_json::from_str(&frame.text).unwrap();
                            match packet["relay"].as_str(){
                                Some("ack")=>{assert_eq!(packet["receipt"],receipt);break;},
                                Some("need")=>{
                                    // Fair scheduling gives data a turn after eight
                                    // controls. The first DOWN need may precede the
                                    // final input ACK, but never an earlier fragment.
                                    assert_eq!(index+1,encrypted.len().div_ceil(transport::CHUNK));
                                    assert_eq!(packet["cid"],"one");assert_eq!(packet["g"],g);
                                    let id=packet["send_id"].as_u64().unwrap();assert!((1..=transport::WINDOW as u64).contains(&id));
                                    assert!(!early_needs.iter().any(|old|serde_json::from_str::<serde_json::Value>(&old.text).unwrap()["send_id"]==id));
                                    assert!(early_needs.len()<transport::WINDOW);early_needs.push(frame);
                                },_=>panic!("unexpected actor output before final input acknowledgement: {packet}"),
                            }
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap_or_else(|error|panic!("each input fragment progresses independently of queued large output: index={index}, total={}, pool={:?}, output_capacity={}, read_bytes={}, elapsed={:?}, error={error}",encrypted.len().div_ceil(transport::CHUNK),pool.usage(),output_tx.capacity(),read_progress.load(std::sync::atomic::Ordering::Acquire),fragment_started.elapsed()));
                if index == 0 {
                    assert_eq!(
                        output_tx.capacity(),
                        0,
                        "partial input must leave the large output in its charged Core queue"
                    );
                    assert!(
                        memory.reserve(4 * OUTPUT + 16 * 1024).is_err(),
                        "fixture actually excludes simultaneous sealing reservation"
                    );
                }
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    assert!(futures_util::poll!(&mut actor).is_pending());
                    if let Some(frame) = early_needs.pop().or_else(|| out.pop()) {
                        let packet: serde_json::Value = serde_json::from_str(&frame.text).unwrap();
                        assert_eq!(packet["relay"], "need");
                        assert_eq!(packet["cid"], "one");
                        assert_eq!(packet["g"], g);
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("large output starts after full input drains");
            assert_eq!(
                output_tx.capacity(),
                1,
                "only the first queued reply was consumed for sealing"
            );
            tokio::time::timeout(Duration::from_secs(5), async {
                while read_progress.load(std::sync::atomic::Ordering::Acquire) != INPUT + 1 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("owned IPC consumer drained all input before cancellation");
            memory.cancel.cancel();
            actor.await.unwrap();
        }
        if let Some(core) = core.take() {
            core.close().await;
        }
        out.retire(&transport::identity("one", &g));
        let bytes = consumer.await.unwrap();
        assert_eq!(bytes.len(), INPUT + 1);
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert!(bytes[..INPUT].iter().all(|b| *b == b'p'));
        assert_eq!(pool.usage(), (1, 0, 0));
        drop(endpoint);
        drop(daemon);
        std::fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn lifecycle_diagnostics_use_fresh_bounded_utf8_capacity() {
        let error = anyhow!("{}", "秘密".repeat(30_000));
        let text = diagnostic(&error);
        assert!(text.len() <= DIAGNOSTIC_BYTES);
        assert!(text.capacity() <= DIAGNOSTIC_BYTES);
        assert!(text.starts_with("秘密"));
    }
    #[tokio::test]
    async fn startup_wait_uses_the_original_open_deadline_even_without_core_registration() {
        let pool = Arc::new(Pool::default());
        let memory = pool.client(CancellationToken::new()).unwrap();
        let opened = tokio::time::Instant::now() - transport::HANDSHAKE_TIMEOUT;
        assert!(startup(&memory, opened)
            .await
            .unwrap_err()
            .to_string()
            .contains("startup deadline"));
        // Registration after expiration cannot make the same epoch ready.
        memory.mark_started();
        assert!(startup(&memory, opened).await.is_err());
        assert!(!memory.cancel.is_cancelled());
        assert_eq!(pool.usage(), (1, 0, 0));
    }
}
