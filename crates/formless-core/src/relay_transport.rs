//! Bounded relay transport v1. Ciphertexts remain protocol-v2 opaque strings.
//! Every queue item owns its allocation charge; no wire permit is a receipt.
use super::Channel;
use crate::relay_buffers::{ClientMemory, Lease};
use anyhow::{anyhow, ensure, Result};
use formless_runtime::LockExt as _;
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

pub(super) const CHUNK: usize = 64 * 1024;
pub(super) const WIRE: usize = CHUNK + 1024;
pub(super) const PRE_READY: usize = 2048;
pub(super) const PLAINTEXT: usize = formless_protocol::MAX_FRAME_BYTES;
pub(super) const ENCRYPTED: usize = 4 * (PLAINTEXT + 16).div_ceil(3) + 256;
pub(super) const WINDOW: usize = 8;
pub(super) const ACK_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const ASSEMBLY_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const RESERVE_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROL_SLOTS: usize = 256;
const DATA_SLOTS: usize = 16;

pub(super) fn generation(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
pub(super) fn cid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}
fn token(s: &str) -> bool {
    s.len() == 22
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}
pub(super) fn identity(cid: &str, g: &str) -> String {
    format!("{cid}:{g}")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Fragment {
    pub relay: String,
    pub v: u8,
    pub id: String,
    pub index: usize,
    pub bytes: usize,
    pub data: String,
}
#[derive(Deserialize)]
#[serde(tag = "relay", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Packet {
    Control {
        g: String,
        receipt: String,
        f: String,
    },
    Delivery {
        g: String,
        cid: String,
        receipt: String,
        fragment: Fragment,
    },
    Grant {
        g: String,
        cid: String,
        send_id: u64,
        permit: String,
    },
    Credit {
        g: String,
        cid: String,
        send_id: u64,
    },
}
impl Packet {
    pub(super) fn parse(text: &str) -> Result<Self> {
        ensure!(text.len() <= WIRE, "relay wire frame exceeds limit");
        let packet: Self = serde_json::from_str(text)?;
        match &packet {
            Self::Control { g, receipt, f } => ensure!(
                generation(g) && token(receipt) && f.len() <= PRE_READY,
                "invalid relay control"
            ),
            Self::Delivery {
                g,
                cid: c,
                receipt,
                fragment,
            } => ensure!(
                generation(g)
                    && cid(c)
                    && token(receipt)
                    && fragment.relay == "data"
                    && fragment.v == 1
                    && generation(&fragment.id)
                    && fragment.bytes > 0
                    && fragment.bytes <= ENCRYPTED
                    && fragment.index < fragment.bytes.div_ceil(CHUNK)
                    && fragment.data.is_ascii()
                    && fragment.data.len() == CHUNK.min(fragment.bytes - fragment.index * CHUNK),
                "invalid relay delivery"
            ),
            Self::Grant {
                g,
                cid: c,
                send_id,
                permit,
            } => ensure!(
                generation(g)
                    && cid(c)
                    && *send_id > 0
                    && *send_id <= 9_007_199_254_740_991
                    && token(permit),
                "invalid relay grant"
            ),
            Self::Credit { g, cid: c, send_id } => ensure!(
                generation(g) && cid(c) && *send_id > 0 && *send_id <= 9_007_199_254_740_991,
                "invalid relay credit"
            ),
        }
        Ok(packet)
    }
    pub(super) fn retained_charge(&self) -> usize {
        match self {
            Self::Delivery { fragment, .. } => 2 * fragment.data.capacity() + 4096,
            _ => 4096,
        }
    }
    pub(super) fn target(&self) -> Option<(&str, &str)> {
        match self {
            Self::Control { .. } => None,
            Self::Delivery { g, cid, .. }
            | Self::Grant { g, cid, .. }
            | Self::Credit { g, cid, .. } => Some((cid, g)),
        }
    }
}

pub(super) struct OwnedPacket {
    pub packet: Packet,
    pub _lease: Lease,
}
pub(super) struct Wire {
    pub text: String,
    pub _lease: Option<Lease>,
}
#[derive(Default)]
struct Queues {
    control: VecDeque<Wire>,
    data: BTreeMap<String, VecDeque<Wire>>,
    last: String,
    control_burst: usize,
}
#[derive(Default)]
pub(super) struct Scheduler {
    queues: Mutex<Queues>,
    pub changed: Notify,
}
impl Scheduler {
    // Native controls have a separate fixed 256*2048-byte lane. Per-client data
    // and handshake allocations instead retain a client resource lease.
    pub(super) fn control(&self, text: String) -> Result<()> {
        ensure!(text.len() <= PRE_READY, "relay control exceeds limit");
        let mut q = self.queues.locked();
        ensure!(q.control.len() < CONTROL_SLOTS, "relay control queue full");
        q.control.push_back(Wire { text, _lease: None });
        drop(q);
        self.changed.notify_one();
        Ok(())
    }
    pub(super) fn data(&self, key: &str, text: String, lease: Lease) -> Result<()> {
        ensure!(text.len() <= WIRE, "relay frame exceeds wire limit");
        let mut q = self.queues.locked();
        let queue = q
            .data
            .get_mut(key)
            .ok_or_else(|| anyhow!("relay client retired"))?;
        ensure!(queue.len() < DATA_SLOTS, "relay client wire queue full");
        queue.push_back(Wire {
            text,
            _lease: Some(lease),
        });
        drop(q);
        self.changed.notify_one();
        Ok(())
    }
    pub(super) fn register(&self, key: String) -> Result<()> {
        let mut q = self.queues.locked();
        ensure!(
            q.data.len() < crate::relay_buffers::MAX_CLIENTS && !q.data.contains_key(&key),
            "relay wire client limit"
        );
        q.data.insert(key, VecDeque::new());
        Ok(())
    }
    pub(super) fn retire(&self, key: &str) {
        self.queues.locked().data.remove(key);
    }
    pub(super) fn pop(&self) -> Option<Wire> {
        let mut q = self.queues.locked();
        let candidate = q
            .data
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, _)| k)
            .find(|k| *k > &q.last)
            .cloned()
            .or_else(|| {
                q.data
                    .iter()
                    .find(|(_, v)| !v.is_empty())
                    .map(|(k, _)| k.clone())
            });
        if !q.control.is_empty() && (q.control_burst < WINDOW || candidate.is_none()) {
            q.control_burst += 1;
            return q.control.pop_front();
        }
        if let Some(key) = candidate {
            q.last = key.clone();
            q.control_burst = 0;
            return q.data.get_mut(&key).and_then(VecDeque::pop_front);
        }
        None
    }
}

/// Actor-local retries release other actors/main socket loop to make progress.
/// The absolute deadline never restarts under continuing pressure.
pub(super) async fn reserve(memory: &Arc<ClientMemory>, bytes: usize) -> Result<Lease> {
    let deadline = tokio::time::Instant::now() + RESERVE_TIMEOUT;
    memory.waiting(true);
    let result = loop {
        if memory.cancel.is_cancelled() {
            break Err(anyhow!("relay client canceled"));
        }
        if let Ok(lease) = memory.reserve(bytes) {
            break Ok(lease);
        }
        tokio::select! {
            _=memory.cancel.cancelled()=>break Err(anyhow!("relay client canceled")),
            _=tokio::time::sleep_until(deadline)=>break Err(anyhow!("relay byte budget did not drain")),
            _=tokio::time::sleep(Duration::from_millis(25))=>{}
        }
    };
    memory.waiting(false);
    result
}

pub(super) struct Assembly {
    id: String,
    total: usize,
    next: usize,
    text: String,
    lease: Lease,
    pub deadline: tokio::time::Instant,
}
pub(super) struct Complete {
    pub text: String,
    pub lease: Lease,
}
fn assembly_charge(bytes: usize) -> Result<usize> {
    ensure!(
        bytes > 0 && bytes <= ENCRYPTED,
        "invalid relay assembly size"
    );
    // Three envelope copies cover ciphertext plus old/new serde scratch during
    // a realloc (a long raw key followed by one escape can double scratch).
    // Two decoded copies cover base64/decryption/IPC handoff in later phases.
    // Decoding cannot exceed three bytes per four envelope characters, so tiny
    // RPCs keep a proportional small-lane reservation rather than64MiB.
    let decoded = bytes
        .div_ceil(4)
        .checked_mul(3)
        .ok_or_else(|| anyhow!("relay decode size overflow"))?
        .min(PLAINTEXT);
    bytes
        .checked_mul(3)
        .and_then(|n| decoded.checked_mul(2).and_then(|d| n.checked_add(d)))
        .and_then(|n| n.checked_add(16 * 1024))
        .ok_or_else(|| anyhow!("relay assembly size overflow"))
}
impl Assembly {
    pub(super) async fn first(f: &Fragment, memory: &Arc<ClientMemory>) -> Result<Self> {
        ensure!(
            f.index == 0 && generation(&f.id) && f.bytes > 0 && f.bytes <= ENCRYPTED,
            "invalid relay assembly start"
        );
        // Reserve all phases before either ciphertext or parser scratch exists.
        let lease = reserve(memory, assembly_charge(f.bytes)?).await?;
        let mut text = String::new();
        text.try_reserve_exact(f.bytes)?;
        Ok(Self {
            id: f.id.clone(),
            total: f.bytes,
            next: 0,
            text,
            lease,
            deadline: tokio::time::Instant::now() + ASSEMBLY_TIMEOUT,
        })
    }
    pub(super) fn push(&mut self, f: Fragment) -> Result<bool> {
        ensure!(
            tokio::time::Instant::now() < self.deadline,
            "relay assembly deadline"
        );
        ensure!(
            f.relay == "data"
                && f.v == 1
                && f.id == self.id
                && f.bytes == self.total
                && f.index == self.next,
            "relay fragment identity/order"
        );
        let remaining = self.total - self.text.len();
        ensure!(
            f.data.len() == remaining.min(CHUNK) && f.data.is_ascii(),
            "relay fragment length/encoding"
        );
        self.text.push_str(&f.data);
        self.next += 1;
        Ok(self.text.len() == self.total)
    }
    pub(super) fn finish(self) -> Complete {
        Complete {
            text: self.text,
            lease: self.lease,
        }
    }
}

// Derived numeric/identifier visitors may construct an invalid_type message
// containing Debug-escaped attacker strings before map_err can discard it.
// Every JSON shape/type rejection below constructs a CONSTANT error directly.
const CIPHER_ERROR: &str = "invalid relay cipher envelope";
struct CipherShape<'a> {
    v: u64,
    seq: u64,
    iv: &'a str,
    ct: &'a str,
}
#[derive(Clone, Copy)]
enum CipherField {
    V,
    Seq,
    Iv,
    Ct,
}
#[derive(Clone, Copy)]
enum Expect {
    Object,
    Field,
    Unsigned,
    Text(usize),
}
enum ShapePart<'a> {
    Object(CipherShape<'a>),
    Field(CipherField),
    Unsigned(u64),
    Text(&'a str),
}
impl Expect {
    fn key<'de, E: serde::de::Error>(self, key: &str) -> std::result::Result<ShapePart<'de>, E> {
        if !matches!(self, Self::Field) {
            return Err(E::custom(CIPHER_ERROR));
        }
        let field = match key {
            "v" => CipherField::V,
            "seq" => CipherField::Seq,
            "iv" => CipherField::Iv,
            "ct" => CipherField::Ct,
            _ => return Err(E::custom(CIPHER_ERROR)),
        };
        Ok(ShapePart::Field(field))
    }
}
impl<'de> serde::de::DeserializeSeed<'de> for Expect {
    type Value = ShapePart<'de>;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<Self::Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}
impl<'de> serde::de::Visitor<'de> for Expect {
    type Value = ShapePart<'de>;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a bounded relay cipher envelope")
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> std::result::Result<Self::Value, E> {
        Err(E::custom(CIPHER_ERROR))
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> std::result::Result<Self::Value, E> {
        Err(E::custom(CIPHER_ERROR))
    }
    fn visit_u64<E: serde::de::Error>(self, value: u64) -> std::result::Result<Self::Value, E> {
        if matches!(self, Self::Unsigned) {
            Ok(ShapePart::Unsigned(value))
        } else {
            Err(E::custom(CIPHER_ERROR))
        }
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> std::result::Result<Self::Value, E> {
        Err(E::custom(CIPHER_ERROR))
    }
    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
        Err(E::custom(CIPHER_ERROR))
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> std::result::Result<Self::Value, E> {
        // Escaped keys may spell a known identifier. Traffic strings must be
        // canonical borrowed ASCII, as produced by Channel::seal; an escaped
        // traffic string cannot outlive serde_json's temporary scratch buffer.
        self.key(value)
    }
    fn visit_borrowed_str<E: serde::de::Error>(
        self,
        value: &'de str,
    ) -> std::result::Result<Self::Value, E> {
        match self {
            Self::Text(max) if value.len() <= max && value.is_ascii() => Ok(ShapePart::Text(value)),
            Self::Field => self.key(value),
            _ => Err(E::custom(CIPHER_ERROR)),
        }
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        _: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        Err(serde::de::Error::custom(CIPHER_ERROR))
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        use serde::de::Error as _;
        if !matches!(self, Self::Object) {
            return Err(A::Error::custom(CIPHER_ERROR));
        }
        let (mut v, mut seq, mut iv, mut ct) = (None, None, None, None);
        while let Some(part) = map.next_key_seed(Expect::Field)? {
            let ShapePart::Field(field) = part else {
                return Err(A::Error::custom(CIPHER_ERROR));
            };
            match field {
                CipherField::V => {
                    if v.is_some() {
                        return Err(A::Error::custom(CIPHER_ERROR));
                    }
                    let ShapePart::Unsigned(value) = map.next_value_seed(Expect::Unsigned)? else {
                        return Err(A::Error::custom(CIPHER_ERROR));
                    };
                    v = Some(value);
                }
                CipherField::Seq => {
                    if seq.is_some() {
                        return Err(A::Error::custom(CIPHER_ERROR));
                    }
                    let ShapePart::Unsigned(value) = map.next_value_seed(Expect::Unsigned)? else {
                        return Err(A::Error::custom(CIPHER_ERROR));
                    };
                    seq = Some(value);
                }
                CipherField::Iv => {
                    if iv.is_some() {
                        return Err(A::Error::custom(CIPHER_ERROR));
                    }
                    let ShapePart::Text(value) = map.next_value_seed(Expect::Text(16))? else {
                        return Err(A::Error::custom(CIPHER_ERROR));
                    };
                    iv = Some(value);
                }
                CipherField::Ct => {
                    if ct.is_some() {
                        return Err(A::Error::custom(CIPHER_ERROR));
                    }
                    let ShapePart::Text(value) =
                        map.next_value_seed(Expect::Text(4 * (PLAINTEXT + 16).div_ceil(3)))?
                    else {
                        return Err(A::Error::custom(CIPHER_ERROR));
                    };
                    ct = Some(value);
                }
            }
        }
        Ok(ShapePart::Object(CipherShape {
            v: v.ok_or_else(|| A::Error::custom(CIPHER_ERROR))?,
            seq: seq.ok_or_else(|| A::Error::custom(CIPHER_ERROR))?,
            iv: iv.ok_or_else(|| A::Error::custom(CIPHER_ERROR))?,
            ct: ct.ok_or_else(|| A::Error::custom(CIPHER_ERROR))?,
        }))
    }
}
impl<'de> Deserialize<'de> for CipherShape<'de> {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        use serde::de::DeserializeSeed as _;
        match Expect::Object.deserialize(deserializer)? {
            ShapePart::Object(shape) => Ok(shape),
            _ => Err(serde::de::Error::custom(CIPHER_ERROR)),
        }
    }
}
pub(super) fn decrypt(channel: &mut Channel, complete: Complete) -> Result<Complete> {
    // Validate scalar shape with borrowed strings before Channel::open's Value
    // parser: arbitrary attacker-supplied nested JSON never allocates a tree.
    let shape: CipherShape<'_> = serde_json::from_str(&complete.text)
        .map_err(|_| anyhow!("invalid relay cipher envelope"))?;
    ensure!(
        shape.v == 2
            && shape.seq > 0
            && shape.iv.len() == 16
            && shape.ct.len() <= 4 * (PLAINTEXT + 16).div_ceil(3),
        "invalid relay cipher envelope"
    );
    let text = channel.open(&complete.text)?;
    ensure!(
        text.len() <= PLAINTEXT && !text.contains('\n') && !text.contains('\r'),
        "invalid relay IPC line"
    );
    let Complete {
        text: encrypted,
        mut lease,
    } = complete;
    drop(encrypted);
    lease.shrink(2 * text.capacity() + 4096);
    Ok(Complete { text, lease })
}

struct Flight {
    send_id: u64,
    index: usize,
    permit: Option<String>,
    sent: bool,
    deadline: tokio::time::Instant,
}
pub(super) struct Sending {
    text: String,
    _lease: Lease,
    id: String,
    next: usize,
    flights: VecDeque<Flight>,
}
impl Sending {
    pub(super) async fn seal(
        channel: &mut Channel,
        line: crate::relay_buffers::Serialized,
        memory: &Arc<ClientMemory>,
    ) -> Result<Self> {
        ensure!(line.line.len() <= PLAINTEXT, "relay plaintext limit");
        let mut lease = reserve(memory, 4 * line.line.len() + 16 * 1024).await?;
        let text = channel.seal(&line.line)?;
        drop(line);
        ensure!(text.len() <= ENCRYPTED, "relay encrypted limit");
        lease.shrink(text.capacity() + 4096);
        let id = rand::random::<[u8; 16]>()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(Self {
            text,
            _lease: lease,
            id,
            next: 0,
            flights: VecDeque::new(),
        })
    }
    pub(super) fn deadline(&self) -> Option<tokio::time::Instant> {
        self.flights.iter().map(|f| f.deadline).min()
    }
    pub(super) fn done(&self) -> bool {
        self.next * CHUNK >= self.text.len() && self.flights.is_empty()
    }
    pub(super) fn fill(
        &mut self,
        cid: &str,
        g: &str,
        key: &str,
        next_id: &mut u64,
        memory: &Arc<ClientMemory>,
        out: &Scheduler,
    ) -> Result<()> {
        while self.flights.len() < WINDOW && self.next * CHUNK < self.text.len() {
            let mut lease = memory.reserve(4 * WIRE)?;
            *next_id = next_id
                .checked_add(1)
                .filter(|n| *n <= 9_007_199_254_740_991)
                .ok_or_else(|| anyhow!("relay send identity exhausted"))?;
            let text = json!({"relay":"need","g":g,"cid":cid,"send_id":*next_id}).to_string();
            lease.shrink(text.capacity() + 1024);
            out.data(key, text, lease)?;
            self.flights.push_back(Flight {
                send_id: *next_id,
                index: self.next,
                permit: None,
                sent: false,
                deadline: tokio::time::Instant::now() + ACK_TIMEOUT,
            });
            self.next += 1;
        }
        memory.waiting(!self.flights.is_empty());
        Ok(())
    }
    pub(super) fn grant(
        &mut self,
        send_id: u64,
        permit: String,
        cid: &str,
        g: &str,
        key: &str,
        memory: &Arc<ClientMemory>,
        out: &Scheduler,
    ) -> Result<()> {
        let f = self
            .flights
            .iter_mut()
            .find(|f| f.send_id == send_id)
            .ok_or_else(|| anyhow!("unknown relay grant"))?;
        ensure!(!f.sent && f.permit.is_none(), "replayed relay grant");
        f.permit = Some(permit);
        // Grants can arrive out of order; serialize fragments in source order.
        for f in self.flights.iter_mut().filter(|f| !f.sent) {
            let Some(permit) = f.permit.take() else { break };
            let mut lease = memory.reserve(4 * WIRE)?;
            let offset = f.index * CHUNK;
            let data = &self.text[offset..(offset + CHUNK).min(self.text.len())];
            let text=json!({"relay":"data","v":1,"g":g,"cid":cid,"id":self.id,"index":f.index,"bytes":self.text.len(),"data":data,"permit":permit}).to_string();
            lease.shrink(text.capacity() + 1024);
            out.data(key, text, lease)?;
            f.sent = true;
        }
        Ok(())
    }
    pub(super) fn credit(&mut self, id: u64) -> Result<()> {
        let index = self
            .flights
            .iter()
            .position(|f| f.send_id == id)
            .ok_or_else(|| anyhow!("unknown relay credit"))?;
        ensure!(self.flights[index].sent, "relay credit before data");
        self.flights.remove(index);
        Ok(())
    }
}

pub(super) struct WriterExit {
    pub error: Option<String>,
    pub _inflight: Option<Lease>,
}

pub(super) async fn write_socket<S>(
    mut sink: S,
    out: Arc<Scheduler>,
    cancel: CancellationToken,
) -> WriterExit
where
    S: futures_util::Sink<tokio_tungstenite::tungstenite::Message> + Unpin,
    S::Error: std::fmt::Display,
{
    use futures_util::SinkExt as _;
    loop {
        let changed = out.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if cancel.is_cancelled() {
            break;
        }
        if let Some(wire) = out.pop() {
            // Move text into tungstenite but retain the charge until flush or
            // actual sink drop. Cancellation never refunds an in-flight copy.
            let Wire { text, _lease } = wire;
            let result = tokio::select! {
                _=cancel.cancelled()=>None,
                result=tokio::time::timeout(ACK_TIMEOUT,sink.send(tokio_tungstenite::tungstenite::Message::Text(text)))=>Some(result)
            };
            match result {
                Some(Ok(Ok(()))) => drop(_lease),
                other => {
                    drop(sink);
                    return WriterExit {
                        error: other.map(|_| "relay socket write failed or stalled".to_owned()),
                        _inflight: _lease,
                    };
                }
            }
        } else {
            tokio::select! { _=cancel.cancelled()=>break, _=changed=>{} }
        }
    }
    drop(sink);
    WriterExit {
        error: None,
        _inflight: None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{new_hello, Side};
    use super::*;
    use crate::relay_buffers::{Pool, Serialized};
    fn memory(pool: &Arc<Pool>) -> Arc<ClientMemory> {
        pool.client(CancellationToken::new()).unwrap()
    }
    fn fragment(id: &str, index: usize, bytes: usize, data: String) -> Fragment {
        Fragment {
            relay: "data".into(),
            v: 1,
            id: id.into(),
            index,
            bytes,
            data,
        }
    }
    #[test]
    fn scheduler_is_bounded_fair_and_retirement_releases_queued_charges() {
        let pool = Arc::new(Pool::default());
        let a = memory(&pool);
        let b = memory(&pool);
        let out = Scheduler::default();
        out.register("a".into()).unwrap();
        out.register("b".into()).unwrap();
        for n in 0..DATA_SLOTS {
            out.data("a", format!("a{n}"), a.reserve(64).unwrap())
                .unwrap();
        }
        assert!(out
            .data("a", "overflow".into(), a.reserve(64).unwrap())
            .is_err());
        out.data("b", "b".into(), b.reserve(64).unwrap()).unwrap();
        for _ in 0..WINDOW + 1 {
            out.control("control".into()).unwrap();
        }
        for _ in 0..WINDOW {
            assert_eq!(out.pop().unwrap().text, "control");
        }
        assert_eq!(out.pop().unwrap().text, "a0");
        assert_eq!(out.pop().unwrap().text, "control");
        assert_eq!(out.pop().unwrap().text, "b");
        assert!(pool.usage().2 > 0);
        out.retire("a");
        out.retire("b");
        assert_eq!(pool.usage(), (2, 0, 0));
        for _ in 0..CONTROL_SLOTS {
            out.control("c".into()).unwrap();
        }
        assert!(out.control("overflow".into()).is_err());
    }
    #[tokio::test]
    async fn fragment_validation_rejects_overflow_reorder_and_expired_assembly_before_delivery() {
        let pool = Arc::new(Pool::default());
        let memory = memory(&pool);
        let id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        assert!(
            Assembly::first(&fragment(id, 0, ENCRYPTED + 1, "".into()), &memory)
                .await
                .is_err()
        );
        assert_eq!(pool.usage(), (1, 0, 0));
        let mut a = Assembly::first(&fragment(id, 0, CHUNK + 1, "x".repeat(CHUNK)), &memory)
            .await
            .unwrap();
        assert!(a
            .push(fragment(id, 1, CHUNK + 1, "x".repeat(CHUNK)))
            .is_err());
        assert!(
            a.push(fragment(id, 0, CHUNK + 1, "x".repeat(CHUNK)))
                .unwrap()
                == false
        );
        a.deadline = tokio::time::Instant::now();
        assert!(a.push(fragment(id, 1, CHUNK + 1, "x".into())).is_err());
        drop(a);
        assert_eq!(pool.usage(), (1, 0, 0));
    }
    #[tokio::test]
    async fn exact_32mib_cipher_roundtrip_crosses_windowed_fragments_without_truncation() {
        let pool = Arc::new(Pool::default());
        let source = memory(&pool);
        let dest = memory(&pool);
        let key = [7; 32];
        let (b, d) = (new_hello(), new_hello());
        let mut sender = Channel::new(&key, &b, &d, Side::Daemon);
        let mut receiver = Channel::new(&key, &b, &d, Side::Browser);
        let line = Serialized {
            line: "x".repeat(PLAINTEXT),
            lease: source.reserve(2 * PLAINTEXT + 4096).unwrap(),
        };
        let mut sending = Sending::seal(&mut sender, line, &source).await.unwrap();
        let out = Scheduler::default();
        out.register("a".into()).unwrap();
        let mut send_id = 0;
        let g = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        sending
            .fill("a", g, "a", &mut send_id, &source, &out)
            .unwrap();
        let mut assembly = None::<Assembly>;
        let mut complete = None;
        let mut count = 0;
        while let Some(wire) = out.pop() {
            assert!(wire.text.len() <= WIRE);
            let frame: serde_json::Value = serde_json::from_str(&wire.text).unwrap();
            match frame["relay"].as_str().unwrap() {
                "need" => {
                    let id = frame["send_id"].as_u64().unwrap();
                    drop(wire);
                    sending
                        .grant(id, format!("{id:022}"), "a", g, "a", &source, &out)
                        .unwrap();
                }
                "data" => {
                    let f = fragment(
                        frame["id"].as_str().unwrap(),
                        frame["index"].as_u64().unwrap() as usize,
                        frame["bytes"].as_u64().unwrap() as usize,
                        frame["data"].as_str().unwrap().into(),
                    );
                    if assembly.is_none() {
                        assembly = Some(Assembly::first(&f, &dest).await.unwrap());
                    }
                    let done = assembly.as_mut().unwrap().push(f).unwrap();
                    let id = frame["permit"].as_str().unwrap().parse().unwrap();
                    drop(wire);
                    count += 1;
                    sending.credit(id).unwrap();
                    sending
                        .fill("a", g, "a", &mut send_id, &source, &out)
                        .unwrap();
                    if done {
                        complete = Some(assembly.take().unwrap().finish());
                    }
                }
                _ => panic!("unexpected packet"),
            }
        }
        assert!(count > WINDOW);
        assert!(sending.done());
        drop(sending);
        let plain = decrypt(&mut receiver, complete.unwrap()).unwrap();
        assert_eq!(plain.text.len(), PLAINTEXT);
        assert!(plain.text.bytes().all(|b| b == b'x'));
        drop(plain);
        assert_eq!(pool.usage(), (2, 0, 0));
    }
    #[test]
    fn unknown_fields_nested_cipher_and_oversize_wire_fail_closed() {
        assert!(Packet::parse(&" ".repeat(WIRE + 1)).is_err());
        let g = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        assert!(
            Packet::parse(&json!({"relay":"credit","g":g,"cid":"a","send_id":0}).to_string())
                .is_err()
        );
        assert!(Packet::parse(
            &json!({"relay":"credit","g":g,"cid":"a","send_id":1,"surprise":{}}).to_string()
        )
        .is_err());
        let key = [7; 32];
        let (b, d) = (new_hello(), new_hello());
        let mut receiver = Channel::new(&key, &b, &d, Side::Browser);
        let pool = Arc::new(Pool::default());
        let memory = memory(&pool);
        let text =
            r#"{"v":2,"seq":1,"iv":"AAAAAAAAAAAAAAAA","ct":{},"nested":[[[[]]]]}"#.to_owned();
        let error = decrypt(
            &mut receiver,
            Complete {
                text,
                lease: memory.reserve(4096).unwrap(),
            },
        )
        .err()
        .unwrap();
        assert_eq!(error.to_string(), "invalid relay cipher envelope");
        assert_eq!(pool.usage(), (1, 0, 0));
    }
    #[tokio::test]
    async fn blocked_reservation_cancellation_releases_owner_and_never_restarts_deadline() {
        let pool = Arc::new(Pool::default());
        let memory = memory(&pool);
        let held = memory
            .reserve(crate::relay_buffers::CLIENT_BULK_BYTES)
            .unwrap();
        let waiting = reserve(&memory, 1024 * 1024);
        tokio::pin!(waiting);
        assert!(futures_util::poll!(&mut waiting).is_pending());
        memory.cancel.cancel();
        assert!(waiting.await.is_err());
        drop(held);
        assert_eq!(pool.usage(), (1, 0, 0));
    }
    #[tokio::test]
    async fn all_sixteen_tiny_rpc_assemblies_keep_the_small_lane_without_false_bulk_eviction() {
        let pool = Arc::new(Pool::default());
        let owners: Vec<_> = (0..crate::relay_buffers::MAX_CLIENTS)
            .map(|_| memory(&pool))
            .collect();
        let mut assemblies = Vec::new();
        let id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        for owner in &owners {
            assemblies.push(
                Assembly::first(&fragment(id, 0, 256, "x".repeat(256)), owner)
                    .await
                    .unwrap(),
            );
        }
        let (clients, bulk, small) = pool.usage();
        assert_eq!(clients, 16);
        assert_eq!(bulk, 0);
        assert!(small > 0);
        assert!(owners.iter().all(|owner| !owner.cancel.is_cancelled()));
        drop(assemblies);
        assert_eq!(pool.usage(), (16, 0, 0));
    }
    #[test]
    fn cipher_shape_errors_are_constant_at_construction_for_maximum_del_payloads() {
        let pool = Arc::new(Pool::default());
        let memory = memory(&pool);
        // Inspect the raw serde error BEFORE decrypt's map_err: checking only
        // the returned anyhow message would miss an enormous intermediate
        // Debug-escaped wrong-type diagnostic allocated by derived visitors.
        for (prefix, suffix) in [
            ("{\"", "\":0}"),
            ("{\"v\":\"", "\"}"),
            ("{\"v\":2,\"seq\":\"", "\"}"),
            ("\"", "\""),
        ] {
            let mut frame = String::with_capacity(ENCRYPTED);
            frame.push_str(prefix);
            frame.extend(std::iter::repeat_n(
                '\u{7f}',
                ENCRYPTED - prefix.len() - suffix.len(),
            ));
            frame.push_str(suffix);
            assert_eq!(frame.len(), ENCRYPTED);
            let lease = memory
                .reserve(assembly_charge(frame.len()).unwrap())
                .unwrap();
            let raw_error = serde_json::from_str::<CipherShape<'_>>(&frame)
                .err()
                .expect("invalid envelope rejected");
            let message = raw_error.to_string();
            assert!(message.starts_with(CIPHER_ERROR));
            assert!(
                message.len() < 128,
                "constructor did not echo or escape attacker data"
            );
            assert!(!message.contains('\u{7f}'));
            drop(raw_error);
            drop(message);
            drop(frame);
            drop(lease);
            assert_eq!(pool.usage(), (1, 0, 0));
        }
        // Escaped unknown keys use serde_json's bounded scratch, not an owned
        // key tree or a second error-sized string. Scratch is covered by the
        // same preallocated assembly reservation while parsing.
        let mut escaped = String::from("{\"");
        escaped.push_str(&"\\u007f".repeat((ENCRYPTED - 16) / 6));
        escaped.push_str("\":0}");
        assert!(escaped.len() <= ENCRYPTED);
        let lease = memory
            .reserve(assembly_charge(escaped.len()).unwrap())
            .unwrap();
        let error = serde_json::from_str::<CipherShape<'_>>(&escaped)
            .err()
            .unwrap()
            .to_string();
        assert!(error.starts_with(CIPHER_ERROR));
        assert!(error.len() < 128);
        assert!(!error.contains("u007f"));
        drop(escaped);
        drop(lease);
        assert_eq!(pool.usage(), (1, 0, 0));
    }
    #[test]
    fn cipher_shape_accepts_escaped_known_identifiers_but_never_escaped_traffic_strings() {
        let pool = Arc::new(Pool::default());
        let memory = memory(&pool);
        let key = [7; 32];
        let (b, d) = (new_hello(), new_hello());
        let mut sender = Channel::new(&key, &b, &d, Side::Daemon);
        let mut receiver = Channel::new(&key, &b, &d, Side::Browser);
        let frame = sender
            .seal(r#"{"private":true}"#)
            .unwrap()
            .replace("\"v\":2", "\"\\u0076\":2");
        let plain = decrypt(
            &mut receiver,
            Complete {
                lease: memory.reserve(4 * frame.len() + 16 * 1024).unwrap(),
                text: frame,
            },
        )
        .unwrap();
        assert_eq!(plain.text, r#"{"private":true}"#);
        drop(plain);
        for frame in [
            r#"{"v":2,"seq":1,"iv":"\u0041AAAAAAAAAAAAAAA","ct":"AA=="}"#,
            r#"{"v":2,"seq":1,"iv":"AAAAAAAAAAAAAAAA","ct":"\u0041A=="}"#,
            r#"{"v":2,"v":2,"seq":1,"iv":"AAAAAAAAAAAAAAAA","ct":"AA=="}"#,
            r#"{"v":[],"seq":1,"iv":"AAAAAAAAAAAAAAAA","ct":"AA=="}"#,
        ] {
            let error = serde_json::from_str::<CipherShape<'_>>(frame)
                .err()
                .unwrap()
                .to_string();
            assert!(error.starts_with(CIPHER_ERROR));
            assert!(error.len() < 128);
        }
        assert_eq!(pool.usage(), (1, 0, 0));
    }
    #[tokio::test]
    async fn one_escape_after_32mib_raw_key_reserves_old_and_new_parser_scratch_before_allocation()
    {
        const PREFIX: usize = 1 << 25;
        let pool = Arc::new(Pool::default());
        let memory = memory(&pool);
        let mut frame = String::with_capacity(PREFIX + 12);
        frame.push_str("{\"");
        frame.extend(std::iter::repeat_n('x', PREFIX));
        frame.push_str("\\u0061\":0}");
        assert_eq!(frame.len(), PREFIX + 12);
        let first = fragment(
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            0,
            frame.len(),
            frame[..CHUNK].to_owned(),
        );
        let mut assembly = Assembly::first(&first, &memory).await.unwrap();
        let charged = pool.usage().1;
        assert_eq!(charged, assembly_charge(frame.len()).unwrap());
        // Pinned serde_json extends32MiB of raw key bytes then pushes one
        // decoded escape. Vec can retain old32MiB +new64MiB during realloc,
        // simultaneously with the32MiB owned ciphertext. No error formatting
        // or post-parse reservation may hide/refund that transient peak.
        let transient_peak = frame.len() + 3 * PREFIX;
        assert!(charged >= transient_peak);
        let old_two_copy_charge =
            2 * frame.len() + 2 * (3 * frame.len().div_ceil(4)).min(PLAINTEXT) + 16 * 1024;
        assert!(
            old_two_copy_charge < transient_peak,
            "fixture distinguishes the missing realloc overlap"
        );
        for (index, piece) in frame.as_bytes().chunks(CHUNK).enumerate() {
            assembly
                .push(fragment(
                    &first.id,
                    index,
                    frame.len(),
                    std::str::from_utf8(piece).unwrap().to_owned(),
                ))
                .unwrap();
        }
        drop(frame);
        let complete = assembly.finish();
        let error = serde_json::from_str::<CipherShape<'_>>(&complete.text)
            .err()
            .unwrap()
            .to_string();
        assert!(error.starts_with(CIPHER_ERROR));
        assert!(error.len() < 128);
        assert!(!error.contains("u0061"));
        assert_eq!(
            pool.usage().1,
            charged,
            "full parse/error phase retains its preallocated charge"
        );
        drop(complete);
        assert_eq!(pool.usage(), (1, 0, 0));
    }
}
