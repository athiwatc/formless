//! Private native-wire peer for daemon integration tests. This is not the
//! production Room (which has its own dev-harness tests). It grants fragments,
//! acknowledges control consumption, retains UP receipts until actual daemon
//! ACK, and reconstructs DOWN ciphertexts for the existing E2E assertions.
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;
use tokio_tungstenite::{tungstenite::Message, WebSocketStream};
const CHUNK: usize = 64 * 1024;
const WIRE: usize = CHUNK + 1024;
const NATIVE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
struct UpProgress {
    index: usize,
    total: usize,
    sent_at: Instant,
}

pub struct RoomPeer {
    ws: WebSocketStream<tokio::net::TcpStream>,
    clients: HashMap<String, String>,
    pending: VecDeque<Value>,
    up: HashMap<String, UpProgress>,
    last_up_ack: Option<Instant>,
    down: HashMap<String, (String, usize, usize, String)>,
    sequence: u64,
    pub biggest: usize,
    pub down_fragments: usize,
    welcomed: bool,
    ready: HashSet<String>,
    permits: HashMap<String, u64>,
    withheld: HashMap<String, Vec<Value>>,
}
impl RoomPeer {
    pub async fn accept(tcp: tokio::net::TcpStream) -> Self {
        let ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let mut peer = Self {
            ws,
            clients: HashMap::new(),
            pending: VecDeque::new(),
            up: HashMap::new(),
            last_up_ack: None,
            down: HashMap::new(),
            sequence: 0,
            biggest: 0,
            down_fragments: 0,
            welcomed: false,
            ready: HashSet::new(),
            permits: HashMap::new(),
            withheld: HashMap::new(),
        };
        peer.control(json!({"relay":"transport","v":1,"g":NATIVE}))
            .await;
        peer
    }
    // These Core-native stress helpers share this peer with integration tests.
    #[allow(dead_code)]
    pub async fn wait_welcome_ack(&mut self) {
        while !self.welcomed {
            self.pump().await;
        }
    }
    #[allow(dead_code)]
    pub fn withhold_credits(&mut self, cid: &str) {
        self.withheld.insert(cid.to_owned(), Vec::new());
    }
    #[allow(dead_code)]
    pub async fn wait_withheld_window(&mut self, cid: &str) {
        while self.withheld.get(cid).map_or(0, Vec::len) < 8 {
            self.pump().await;
        }
    }
    #[allow(dead_code)]
    pub fn pending_up(&self) -> usize {
        self.up.len()
    }
    #[allow(dead_code)]
    pub async fn drain_up_to(&mut self, want: usize) {
        while self.up.len() > want {
            self.pump().await;
        }
    }
    fn receipt(&mut self) -> String {
        self.sequence += 1;
        format!("{:022}", self.sequence)
    }
    async fn wire(&mut self, frame: Value) {
        let text = frame.to_string();
        assert!(
            text.len() <= WIRE,
            "private native frame too large: {}",
            text.len()
        );
        self.biggest = self.biggest.max(text.len());
        self.ws.send(Message::Text(text)).await.unwrap();
    }
    async fn control(&mut self, inner: Value) {
        let receipt = self.receipt();
        self.wire(json!({"relay":"control","g":NATIVE,"receipt":receipt,"f":inner.to_string()}))
            .await;
    }
    async fn open(&mut self, cid: &str, ua: &str) {
        if self.clients.contains_key(cid) {
            return;
        }
        let g = format!("{:032x}", self.clients.len() + 1);
        self.clients.insert(cid.to_owned(), g.clone());
        self.control(json!({"relay":"client_open","cid":cid,"g":g,"ua":ua}))
            .await;
    }
    pub async fn send(&mut self, message: Message) -> Result<(), std::io::Error> {
        let Message::Text(text) = message else {
            panic!("private peer only sends text")
        };
        let frame: Value = serde_json::from_str(&text).unwrap();
        let cid = frame["cid"].as_str().unwrap();
        match frame["relay"].as_str() {
            Some("client_open") => self.open(cid, frame["ua"].as_str().unwrap_or("")).await,
            Some("client_close") => {
                let g = self.clients.get(cid).unwrap().clone();
                self.control(json!({"relay":"client_close","cid":cid,"g":g}))
                    .await;
                self.clients.remove(cid);
            }
            _ => {
                self.open(cid, "").await;
                let g = self.clients[cid].clone();
                let f = frame["f"].as_str().unwrap();
                let v: Value = serde_json::from_str(f).unwrap();
                if v.get("hello").is_some() || v.get("confirm").is_some() {
                    self.control(json!({"cid":cid,"g":g,"f":f})).await;
                } else {
                    while !self.ready.contains(cid) {
                        self.pump().await;
                    }
                    let id = format!("{:032x}", self.sequence + 1);
                    let total = f.len().div_ceil(CHUNK);
                    for (index, data) in f.as_bytes().chunks(CHUNK).enumerate() {
                        while self.up.len() >= 8 {
                            self.pump().await;
                        }
                        let receipt = self.receipt();
                        let progress = UpProgress {
                            index,
                            total,
                            sent_at: Instant::now(),
                        };
                        assert!(self.up.insert(receipt.clone(), progress).is_none());
                        let data = std::str::from_utf8(data).unwrap();
                        self.wire(json!({"relay":"delivery","cid":cid,"g":g,"receipt":receipt,"fragment":{"relay":"data","v":1,"id":id,"index":index,"bytes":f.len(),"data":data}})).await;
                    }
                }
            }
        }
        Ok(())
    }
    async fn pump(&mut self) {
        self.pump_with_timeout(std::time::Duration::from_secs(10), "normal frame")
            .await;
    }

    async fn pump_with_timeout(&mut self, wait: std::time::Duration, phase: &str) {
        let message = tokio::time::timeout(wait, self.ws.next())
            .await
            .unwrap_or_else(|_| {
                let pending_up: Vec<_> = self
                    .up
                    .values()
                    .map(|progress| (progress.index, progress.total, progress.sent_at.elapsed()))
                    .collect();
                let last_up_ack_ago = self.last_up_ack.map(|at| at.elapsed());
                panic!(
                    "private relay peer timed out during {phase}: wait={wait:?}, UP receipts={}, UP pending (zero-based index, fragment count, age)={pending_up:?}, last UP ACK age={last_up_ack_ago:?}, DOWN fragments={}, outstanding DOWN grants={}",
                    self.up.len(),
                    self.down_fragments,
                    self.permits.len()
                )
            })
            .expect("private relay peer open")
            .unwrap();
        let Message::Text(text) = message else { return };
        assert!(text.len() <= WIRE, "daemon emitted oversize native frame");
        self.biggest = self.biggest.max(text.len());
        let frame: Value = serde_json::from_str(&text).unwrap();
        match frame["relay"].as_str() {
            Some("ack_control") => {
                assert_eq!(frame["g"], NATIVE);
                self.welcomed = true;
            }
            Some("ack") => {
                assert!(
                    self.up.remove(frame["receipt"].as_str().unwrap()).is_some(),
                    "unknown/replayed UP receipt"
                );
                self.last_up_ack = Some(Instant::now());
            }
            Some("ready") => {
                let cid = frame["cid"].as_str().unwrap();
                assert_eq!(frame["g"], self.clients[cid]);
                self.control(json!({"relay":"ready","cid":cid,"g":self.clients[cid]}))
                    .await;
                self.ready.insert(cid.to_owned());
            }
            Some("need") => {
                let cid = frame["cid"].as_str().unwrap();
                let g = frame["g"].clone();
                assert_eq!(g, self.clients[cid]);
                let permit = self.receipt();
                self.permits
                    .insert(permit.clone(), frame["send_id"].as_u64().unwrap());
                self.wire(json!({"relay":"grant","cid":cid,"g":g,"send_id":frame["send_id"],"permit":permit})).await;
                // The send permit is separate from a destination receipt. This
                // test destination consumes each fragment immediately below.
            }
            Some("data") => {
                self.down_fragments += 1;
                let cid = frame["cid"].as_str().unwrap();
                assert_eq!(frame["g"], self.clients[cid]);
                let index = frame["index"].as_u64().unwrap() as usize;
                let assembly = self.down.entry(cid.to_owned()).or_insert_with(|| {
                    (
                        frame["id"].as_str().unwrap().to_owned(),
                        frame["bytes"].as_u64().unwrap() as usize,
                        0,
                        String::new(),
                    )
                });
                assert_eq!(assembly.0, frame["id"]);
                assert_eq!(assembly.1, frame["bytes"].as_u64().unwrap() as usize);
                assert_eq!(assembly.2, index);
                let data = frame["data"].as_str().unwrap();
                assert_eq!(data.len(), CHUNK.min(assembly.1 - assembly.3.len()));
                assembly.3.push_str(data);
                assembly.2 += 1;
                let done = assembly.3.len() == assembly.1;
                let send_id = self
                    .permits
                    .remove(frame["permit"].as_str().unwrap())
                    .expect("one-use grant permit");
                let credit = json!({"relay":"credit","cid":cid,"g":frame["g"],"send_id":send_id});
                if let Some(held) = self.withheld.get_mut(cid) {
                    assert!(held.len() < 8, "daemon exceeded destination credit window");
                    held.push(credit);
                } else {
                    self.wire(credit).await;
                }
                if done {
                    let (_, _, _, f) = self.down.remove(cid).unwrap();
                    self.pending.push_back(json!({"cid":cid,"f":f}));
                }
            }
            Some("disconnect") => {
                let cid = frame["cid"].as_str().unwrap().to_owned();
                let g = self.clients[&cid].clone();
                self.control(json!({"relay":"client_close","cid":cid,"g":g}))
                    .await;
                self.pending.push_back(frame);
            }
            Some("push") => self.pending.push_back(frame),
            _ => {
                let cid = frame["cid"].as_str().unwrap();
                assert_eq!(frame["g"], self.clients[cid]);
                self.pending.push_back(json!({"cid":cid,"f":frame["f"]}));
            }
        }
    }
    /// A full-size response needs time to read the DB, serialize and seal
    /// before requesting the first DOWN grant. Keep the normal per-frame
    /// deadline as soon as DOWN starts; this does not extend wire deadlines.
    #[allow(dead_code)]
    pub async fn next_full_size_reply(&mut self) -> Option<Result<Message, std::io::Error>> {
        assert!(self.permits.is_empty() && self.down_fragments == 0);
        let initial_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Some(Ok(Message::Text(frame.to_string())));
            }
            if self.permits.is_empty() && self.down_fragments == 0 {
                let wait = initial_deadline.saturating_duration_since(tokio::time::Instant::now());
                self.pump_with_timeout(wait, "waiting for first DOWN grant request")
                    .await;
            } else {
                self.pump_with_timeout(std::time::Duration::from_secs(10), "DOWN transfer")
                    .await;
            }
        }
    }

    pub async fn next(&mut self) -> Option<Result<Message, std::io::Error>> {
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Some(Ok(Message::Text(frame.to_string())));
            }
            self.pump().await;
        }
    }
}
