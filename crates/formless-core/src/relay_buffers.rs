//! Relay-only byte-backed retention accounting. This is not an OS/RSS limit,
//! a cap on arbitrary Store/business-handler heaps, or a native TCP guarantee.
//! Ordinary IPC connections keep their existing typed queues unchanged.
use anyhow::{anyhow, Result};
use formless_protocol::{codes, ServerMessage, MAX_FRAME_BYTES};
use formless_runtime::LockExt as _;
use std::{
    future::Future,
    io::Write,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex, Weak,
    },
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(crate) const MAX_CLIENTS: usize = 16;
pub(crate) const CLIENT_SMALL_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const CLIENT_BULK_BYTES: usize = 320 * 1024 * 1024;
pub(crate) const GLOBAL_BULK_BYTES: usize = 2 * CLIENT_BULK_BYTES;
pub(crate) const SMALL_CHARGE: usize = 512 * 1024;
pub(crate) const OUTBOUND_MESSAGES: usize = 64;
pub(crate) const INBOUND_MESSAGES: usize = 8;
pub(crate) const CONTROL_TASKS: usize = 8;
pub(crate) const KICK_COMMANDS: usize = 64;
// Persistent connection metadata has independent byte/count ceilings; a
// successfully parsed request must not escape its lease into an unbounded map.
pub(crate) const METADATA_ENTRIES: usize = 1024;
pub(crate) const METADATA_BYTES: usize = 64 * 1024;
pub(crate) const CLIENT_LABEL_BYTES: usize = 256;

pub(crate) fn subscription_fits(sessions: Option<&[String]>, label: Option<&str>) -> bool {
    label.is_none_or(|s| s.len() <= CLIENT_LABEL_BYTES)
        && sessions.is_none_or(|sessions| {
            sessions.len() <= METADATA_ENTRIES
                && sessions.iter().map(String::len).sum::<usize>() <= METADATA_BYTES
        })
}
pub(crate) fn remember_interrupt(
    cuts: &mut std::collections::HashMap<String, u64>,
    session: &str,
    arrival: u64,
    bounded: bool,
) -> bool {
    if bounded
        && !cuts.contains_key(session)
        && (cuts.len() >= METADATA_ENTRIES
            || session.len() > METADATA_BYTES.saturating_sub(cuts.keys().map(String::len).sum()))
    {
        return false;
    }
    cuts.insert(session.to_owned(), arrival);
    true
}
pub(crate) fn interruption_cut(
    cuts: &mut std::collections::HashMap<String, u64>,
    session: Option<&str>,
    arrival: u64,
    bounded: bool,
) -> Option<u64> {
    let cut = session.and_then(|session| cuts.get(session)).copied();
    // The sole FIFO worker has captured this request's cut. Every older
    // queued request is already processed; future requests cannot need a cut
    // at or before this arrival. A newer interrupt must survive for older
    // SendMessage envelopes still in the FIFO, even across other requests.
    if bounded {
        cuts.retain(|_, cut| *cut > arrival);
    }
    cut
}

#[derive(Default)]
pub(crate) struct Pool {
    clients: AtomicUsize,
    bulk: AtomicUsize,
    small: AtomicUsize,
    owners: Mutex<Vec<Weak<ClientMemory>>>,
}
fn add(counter: &AtomicUsize, bytes: usize, max: usize) -> bool {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            n.checked_add(bytes).filter(|n| *n <= max)
        })
        .is_ok()
}
impl Pool {
    pub(crate) fn client(self: &Arc<Self>, cancel: CancellationToken) -> Result<Arc<ClientMemory>> {
        if !add(&self.clients, 1, MAX_CLIENTS) {
            return Err(anyhow!("relay client resource limit"));
        }
        let owner = Arc::new(ClientMemory {
            pool: self.clone(),
            small: AtomicUsize::new(0),
            bulk: AtomicUsize::new(0),
            cancel,
            progress: Mutex::new((Instant::now(), false)),
            startup: AtomicBool::new(false),
            startup_changed: tokio::sync::Notify::new(),
        });
        let mut owners = self.owners.locked();
        owners.retain(|w| w.strong_count() > 0);
        owners.push(Arc::downgrade(&owner));
        Ok(owner)
    }
    fn evict_stalled(&self, except: &ClientMemory) {
        let mut selected: Option<(Instant, Arc<ClientMemory>)> = None;
        for owner in self.owners.locked().iter().filter_map(Weak::upgrade) {
            if std::ptr::eq(owner.as_ref(), except) || owner.cancel.is_cancelled() {
                continue;
            }
            let (at, waiting) = *owner.progress.locked();
            if waiting
                && at.elapsed() >= Duration::from_secs(1)
                && selected.as_ref().is_none_or(|(older, _)| at < *older)
            {
                selected = Some((at, owner));
            }
        }
        if let Some((_, owner)) = selected {
            owner.cancel.cancel();
        }
    }
    #[cfg(test)]
    pub(crate) fn usage(&self) -> (usize, usize, usize) {
        (
            self.clients.load(Ordering::Acquire),
            self.bulk.load(Ordering::Acquire),
            self.small.load(Ordering::Acquire),
        )
    }
}
pub(crate) struct ClientMemory {
    pool: Arc<Pool>,
    small: AtomicUsize,
    bulk: AtomicUsize,
    pub(crate) cancel: CancellationToken,
    progress: Mutex<(Instant, bool)>,
    startup: AtomicBool,
    startup_changed: tokio::sync::Notify,
}
impl Drop for ClientMemory {
    fn drop(&mut self) {
        self.pool.clients.fetch_sub(1, Ordering::AcqRel);
    }
}
impl ClientMemory {
    /// The endpoint acknowledges room readiness only after Core has installed
    /// its subscriber, bounded queues and worker. Cancellation is not startup.
    pub(crate) async fn started(&self) -> Result<()> {
        loop {
            let changed = self.startup_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.cancel.is_cancelled() {
                return Err(anyhow!("relay connection canceled before readiness"));
            }
            if self.startup.load(Ordering::Acquire) {
                return Ok(());
            }
            tokio::select! {
                _ = self.cancel.cancelled() => return Err(anyhow!("relay connection canceled before readiness")),
                _ = changed => {}
            }
        }
    }
    pub(crate) fn mark_started(&self) {
        self.startup.store(true, Ordering::Release);
        self.startup_changed.notify_waiters();
    }
    fn add_bytes(&self, bytes: usize, small: bool) -> Result<()> {
        let (local, max) = if small {
            (&self.small, CLIENT_SMALL_BYTES)
        } else {
            (&self.bulk, CLIENT_BULK_BYTES)
        };
        if !add(local, bytes, max) {
            return Err(anyhow!("relay client byte budget exhausted"));
        }
        if small {
            self.pool.small.fetch_add(bytes, Ordering::AcqRel);
        } else if !add(&self.pool.bulk, bytes, GLOBAL_BULK_BYTES) {
            local.fetch_sub(bytes, Ordering::AcqRel);
            self.pool.evict_stalled(self);
            return Err(anyhow!("relay bulk byte budget busy"));
        }
        Ok(())
    }
    pub(crate) fn reserve(self: &Arc<Self>, bytes: usize) -> Result<Lease> {
        let small = bytes <= SMALL_CHARGE;
        self.add_bytes(bytes, small)?;
        Ok(Lease {
            owner: self.clone(),
            bytes,
            small,
        })
    }
    pub(crate) fn waiting(&self, waiting: bool) {
        let mut progress = self.progress.locked();
        if !waiting || !progress.1 {
            progress.0 = Instant::now();
        }
        progress.1 = waiting;
    }
}
pub(crate) struct Lease {
    owner: Arc<ClientMemory>,
    bytes: usize,
    small: bool,
}
impl Lease {
    pub(crate) fn grow(&mut self, bytes: usize) -> Result<()> {
        if bytes <= self.bytes {
            return Ok(());
        }
        if self.small == (bytes <= SMALL_CHARGE) {
            self.owner.add_bytes(bytes - self.bytes, self.small)?;
            self.bytes = bytes;
        } else {
            let replacement = self.owner.reserve(bytes)?;
            *self = replacement;
        }
        Ok(())
    }
    pub(crate) fn shrink(&mut self, bytes: usize) {
        assert!(bytes <= self.bytes);
        let removed = self.bytes - bytes;
        self.bytes = bytes;
        if self.small {
            self.owner.small.fetch_sub(removed, Ordering::AcqRel);
            self.owner.pool.small.fetch_sub(removed, Ordering::AcqRel);
        } else {
            self.owner.bulk.fetch_sub(removed, Ordering::AcqRel);
            self.owner.pool.bulk.fetch_sub(removed, Ordering::AcqRel);
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.shrink(0);
    }
}
pub(crate) struct Serialized {
    pub(crate) line: String,
    pub(crate) lease: Lease,
}

struct Count {
    bytes: usize,
}
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_FRAME_BYTES.saturating_sub(self.bytes) {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
struct Bounded {
    bytes: Vec<u8>,
    max: usize,
}
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.max.saturating_sub(self.bytes.len()) {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode(message: ServerMessage, memory: &Arc<ClientMemory>) -> Result<Serialized> {
    encode_inner(message, memory, true)
}
fn encode_inner(
    message: ServerMessage,
    memory: &Arc<ClientMemory>,
    fallback: bool,
) -> Result<Serialized> {
    let id = match &message {
        ServerMessage::Response { id, .. } => Some(*id),
        _ => None,
    };
    let mut count = Count { bytes: 0 };
    let measured = serde_json::to_writer(&mut count, &message).is_ok();
    let charge = count.bytes.checked_mul(2).and_then(|n| n.checked_add(4096));
    let lease = if measured {
        charge.and_then(|n| memory.reserve(n).ok())
    } else {
        None
    };
    let Some(lease) = lease else {
        drop(message);
        if let Some(id) = id.filter(|_| fallback) {
            // The operation may already have taken effect: never label this a
            // pre-execution BUSY gate or invite an automatic mutation retry.
            return encode_inner(ServerMessage::err_code(id,codes::INTERNAL,"RPC response exceeds the relay buffer limit or capacity; the operation may already be applied. Inspect its state before retrying.".into()),memory,false);
        }
        return Err(anyhow!("relay event exceeds output capacity"));
    };
    let mut writer = Bounded {
        bytes: Vec::with_capacity(count.bytes),
        max: count.bytes,
    };
    serde_json::to_writer(&mut writer, &message)?;
    // No typed payload enters a queue, future or spawned producer wait.
    drop(message);
    Ok(Serialized {
        line: String::from_utf8(writer.bytes)?,
        lease,
    })
}

#[derive(Clone)]
pub(crate) struct RelayOutput {
    pub(crate) tx: mpsc::Sender<Serialized>,
    pub(crate) memory: Arc<ClientMemory>,
}
pub(crate) enum Outbound {
    Raw(ServerMessage),
    Wire(Serialized),
}
impl std::fmt::Debug for Outbound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Raw(_) => f.write_str("typed client message"),
            Self::Wire(frame) => f
                .debug_struct("serialized relay message")
                .field("bytes", &frame.line.len())
                .finish(),
        }
    }
}
#[derive(Clone)]
pub(crate) enum ConnectionSender {
    Raw(mpsc::Sender<ServerMessage>),
    Relay(RelayOutput),
}
impl From<mpsc::Sender<ServerMessage>> for ConnectionSender {
    fn from(tx: mpsc::Sender<ServerMessage>) -> Self {
        Self::Raw(tx)
    }
}
impl ConnectionSender {
    pub(crate) fn is_closed(&self) -> bool {
        match self {
            Self::Raw(tx) => tx.is_closed(),
            Self::Relay(out) => out.tx.is_closed(),
        }
    }
    pub(crate) fn max_capacity(&self) -> usize {
        match self {
            Self::Raw(tx) => tx.max_capacity(),
            Self::Relay(out) => out.tx.max_capacity(),
        }
    }
    pub(crate) fn capacity(&self) -> usize {
        match self {
            Self::Raw(tx) => tx.capacity(),
            Self::Relay(out) => out.tx.capacity(),
        }
    }
    pub(crate) fn try_send(
        &self,
        msg: ServerMessage,
    ) -> std::result::Result<(), mpsc::error::TrySendError<Outbound>> {
        use mpsc::error::TrySendError::{Closed, Full};
        match self {
            Self::Raw(tx) => tx.try_send(msg).map_err(|e| match e {
                Closed(m) => Closed(Outbound::Raw(m)),
                Full(m) => Full(Outbound::Raw(m)),
            }),
            Self::Relay(out) => {
                if out.memory.cancel.is_cancelled() || out.tx.is_closed() {
                    drop(msg);
                    return Err(Closed(Outbound::Raw(ServerMessage::err(
                        0,
                        "relay connection closed".into(),
                    ))));
                }
                let encoded = match encode(msg, &out.memory) {
                    Ok(m) => m,
                    Err(_) => {
                        out.memory.cancel.cancel();
                        return Err(Closed(Outbound::Raw(ServerMessage::err(
                            0,
                            "relay output unavailable".into(),
                        ))));
                    }
                };
                out.tx.try_send(encoded).map_err(|e| {
                    out.memory.cancel.cancel();
                    match e {
                        Closed(m) => Closed(Outbound::Wire(m)),
                        Full(m) => Full(Outbound::Wire(m)),
                    }
                })
            }
        }
    }
    pub(crate) fn send(
        &self,
        msg: ServerMessage,
    ) -> Pin<
        Box<
            dyn Future<Output = std::result::Result<(), mpsc::error::SendError<Outbound>>>
                + Send
                + '_,
        >,
    > {
        match self {
            Self::Raw(tx) => Box::pin(async move {
                tx.send(msg)
                    .await
                    .map_err(|e| mpsc::error::SendError(Outbound::Raw(e.0)))
            }),
            // Encode/drop typed message synchronously before constructing even
            // the returned future. A relay output never parks typed payloads.
            Self::Relay(_) => {
                let result = self.try_send(msg).map_err(|e| {
                    mpsc::error::SendError(match e {
                        mpsc::error::TrySendError::Closed(m)
                        | mpsc::error::TrySendError::Full(m) => m,
                    })
                });
                Box::pin(async move { result })
            }
        }
    }
}

pub(crate) async fn read_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    memory: &Arc<ClientMemory>,
) -> std::io::Result<Option<Serialized>> {
    let mut buffer = Vec::new();
    let mut lease = memory
        .reserve(4096)
        .map_err(|_| std::io::ErrorKind::OutOfMemory)?;
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            if buffer.is_empty() {
                return Ok(None);
            }
            if buffer.len() > MAX_FRAME_BYTES {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            return Ok(Some(Serialized {
                line: String::from_utf8(buffer).map_err(|_| std::io::ErrorKind::InvalidData)?,
                lease,
            }));
        }
        let (take, done) = chunk
            .iter()
            .position(|b| *b == b'\n')
            .map(|i| (i + 1, true))
            .unwrap_or((chunk.len(), false));
        let length = buffer
            .len()
            .checked_add(take)
            .filter(|n| *n <= MAX_FRAME_BYTES + 1)
            .ok_or(std::io::ErrorKind::InvalidData)?;
        // Reserve the growth delta before extending the owned buffer.
        let wanted = length.saturating_mul(4).saturating_add(4096);
        lease
            .grow(wanted)
            .map_err(|_| std::io::ErrorKind::OutOfMemory)?;
        buffer
            .try_reserve_exact(take)
            .map_err(|_| std::io::ErrorKind::OutOfMemory)?;
        buffer.extend_from_slice(&chunk[..take]);
        reader.consume(take);
        if done {
            buffer.pop();
            return Ok(Some(Serialized {
                line: String::from_utf8(buffer).map_err(|_| std::io::ErrorKind::InvalidData)?,
                lease,
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn persistent_metadata_is_bounded_without_pruning_newer_interrupts_for_older_fifo_work() {
        assert!(subscription_fits(None, None));
        assert!(!subscription_fits(
            Some(&vec!["s".into(); METADATA_ENTRIES + 1]),
            None
        ));
        assert!(!subscription_fits(
            Some(&["s".repeat(METADATA_BYTES + 1)]),
            None
        ));
        assert!(!subscription_fits(
            None,
            Some(&"c".repeat(CLIENT_LABEL_BYTES + 1))
        ));
        let mut cuts = std::collections::HashMap::new();
        assert!(remember_interrupt(&mut cuts, "session", 10, true));
        assert_eq!(interruption_cut(&mut cuts, None, 2, true), None);
        assert_eq!(
            interruption_cut(&mut cuts, Some("session"), 3, true),
            Some(10)
        );
        assert_eq!(
            interruption_cut(&mut cuts, Some("session"), 4, true),
            Some(10)
        );
        assert!(
            cuts.contains_key("session"),
            "newer cut protects every older queued SendMessage"
        );
        assert_eq!(
            interruption_cut(&mut cuts, Some("session"), 11, true),
            Some(10)
        );
        assert!(cuts.is_empty());
        assert!(remember_interrupt(
            &mut cuts,
            &"x".repeat(METADATA_BYTES),
            12,
            true
        ));
        assert!(!remember_interrupt(&mut cuts, "y", 13, true));
        assert_eq!(cuts.len(), 1, "refusal cannot mutate prior cuts");
        // The ordinary local path retains its previous unrestricted policy.
        assert!(remember_interrupt(&mut cuts, "y", 13, false));
        interruption_cut(&mut cuts, None, 20, false);
        assert_eq!(cuts.len(), 2);
        let mut cuts = (0..METADATA_ENTRIES).map(|i| (i.to_string(), 1)).collect();
        assert!(!remember_interrupt(&mut cuts, "new", 2, true));
    }
    #[tokio::test]
    async fn startup_barrier_requires_explicit_core_registration_and_observes_cancellation() {
        let pool = Arc::new(Pool::default());
        let memory = pool.client(CancellationToken::new()).unwrap();
        let startup = memory.started();
        tokio::pin!(startup);
        assert!(futures_util::poll!(&mut startup).is_pending());
        memory.mark_started();
        startup.await.unwrap();
        // A notification before the waiter is also visible through the flag.
        memory.started().await.unwrap();
        let canceled = pool.client(CancellationToken::new()).unwrap();
        let startup = canceled.started();
        tokio::pin!(startup);
        assert!(futures_util::poll!(&mut startup).is_pending());
        canceled.cancel.cancel();
        assert!(startup.await.is_err());
    }
    #[test]
    fn closing_client_cannot_recycle_resource_slot_while_an_owned_buffer_remains() {
        let pool = Arc::new(Pool::default());
        let mut clients: Vec<_> = (0..MAX_CLIENTS)
            .map(|_| pool.client(CancellationToken::new()).unwrap())
            .collect();
        let held = clients[0].reserve(100).unwrap();
        drop(clients.remove(0));
        assert!(pool.client(CancellationToken::new()).is_err());
        drop(held);
        assert!(pool.client(CancellationToken::new()).is_ok());
        drop(clients);
        assert_eq!(pool.usage(), (0, 0, 0));
    }
    #[test]
    fn bulk_pressure_keeps_an_independent_small_lane_and_evicts_only_stalled_peer() {
        let pool = Arc::new(Pool::default());
        let a = pool.client(CancellationToken::new()).unwrap();
        let b = pool.client(CancellationToken::new()).unwrap();
        let healthy = pool.client(CancellationToken::new()).unwrap();
        let first = a.reserve(CLIENT_BULK_BYTES).unwrap();
        let second = b.reserve(CLIENT_BULK_BYTES).unwrap();
        let control = healthy.reserve(128).unwrap();
        *a.progress.locked() = (Instant::now() - Duration::from_secs(2), true);
        assert!(healthy.reserve(SMALL_CHARGE + 1).is_err());
        assert!(a.cancel.is_cancelled());
        assert!(!b.cancel.is_cancelled());
        assert!(!healthy.cancel.is_cancelled());
        drop(first);
        let next = healthy.reserve(SMALL_CHARGE + 1).unwrap();
        drop((next, second, control));
        assert_eq!(pool.usage(), (3, 0, 0));
    }
    #[test]
    fn bulk_pressure_skips_canceled_victim_without_refunding_its_held_bytes() {
        let pool = Arc::new(Pool::default());
        let a = pool.client(CancellationToken::new()).unwrap();
        let b = pool.client(CancellationToken::new()).unwrap();
        let healthy = pool.client(CancellationToken::new()).unwrap();
        let first = a.reserve(CLIENT_BULK_BYTES).unwrap();
        let second = b.reserve(CLIENT_BULK_BYTES).unwrap();
        *a.progress.locked() = (Instant::now() - Duration::from_secs(3), true);
        *b.progress.locked() = (Instant::now() - Duration::from_secs(2), true);
        assert!(healthy.reserve(SMALL_CHARGE + 1).is_err());
        assert!(a.cancel.is_cancelled());
        assert!(!b.cancel.is_cancelled());
        assert_eq!(pool.usage(), (3, GLOBAL_BULK_BYTES, 0));
        assert!(healthy.reserve(SMALL_CHARGE + 1).is_err());
        assert!(
            b.cancel.is_cancelled(),
            "an already canceled holder cannot monopolize eviction selection"
        );
        assert_eq!(
            pool.usage(),
            (3, GLOBAL_BULK_BYTES, 0),
            "cancellation alone never refunds held memory"
        );
        drop(second);
        let progress = healthy.reserve(SMALL_CHARGE + 1).unwrap();
        assert!(!healthy.cancel.is_cancelled());
        drop((first, progress));
        assert_eq!(pool.usage(), (3, 0, 0));
    }
    #[test]
    fn bounded_serializer_keeps_exact_32mib_and_returns_honest_small_oversize_rpc_error() {
        let pool = Arc::new(Pool::default());
        let memory = pool.client(CancellationToken::new()).unwrap();
        let overhead = serde_json::to_string(&ServerMessage::ok(7, json!("")))
            .unwrap()
            .len();
        let output = encode(
            ServerMessage::ok(7, json!("x".repeat(MAX_FRAME_BYTES - overhead))),
            &memory,
        )
        .unwrap();
        assert_eq!(output.line.len(), MAX_FRAME_BYTES);
        assert!(pool.usage().1 >= 2 * MAX_FRAME_BYTES);
        drop(output);
        let rejected = encode(
            ServerMessage::ok(8, json!("x".repeat(MAX_FRAME_BYTES))),
            &memory,
        )
        .unwrap();
        let response: ServerMessage = serde_json::from_str(&rejected.line).unwrap();
        assert_eq!(response.error_code(), Some(codes::INTERNAL));
        assert!(rejected.line.len() < 1024);
        assert!(rejected.line.contains("may already be applied"));
        drop(rejected);
        assert_eq!(pool.usage(), (1, 0, 0));
    }
    #[tokio::test]
    async fn serialized_queue_owns_its_byte_lease_and_raw_local_queue_stays_typed() {
        let pool = Arc::new(Pool::default());
        let memory = pool.client(CancellationToken::new()).unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let sender = ConnectionSender::Relay(RelayOutput {
            tx,
            memory: memory.clone(),
        });
        sender
            .send(ServerMessage::ok(1, json!({"private":"fixture"})))
            .await
            .unwrap();
        assert!(pool.usage().2 > 0);
        let output = rx.recv().await.unwrap();
        let parsed: ServerMessage = serde_json::from_str(&output.line).unwrap();
        assert!(matches!(
            parsed,
            ServerMessage::Response {
                id: 1,
                ok: true,
                ..
            }
        ));
        assert!(pool.usage().2 > 0);
        drop(output);
        assert_eq!(pool.usage(), (1, 0, 0));
        let (tx, mut rx) = mpsc::channel(1);
        let raw: ConnectionSender = tx.into();
        raw.send(ServerMessage::ok(9, json!(true))).await.unwrap();
        assert!(matches!(
            rx.recv().await,
            Some(ServerMessage::Response { id: 9, .. })
        ));
    }
    #[test]
    fn exhausted_small_budget_does_not_recurse_on_the_fallback_error() {
        let pool = Arc::new(Pool::default());
        let memory = pool.client(CancellationToken::new()).unwrap();
        let held: Vec<_> = (0..CLIENT_SMALL_BYTES / SMALL_CHARGE)
            .map(|_| memory.reserve(SMALL_CHARGE).unwrap())
            .collect();
        assert!(encode(ServerMessage::ok(1, json!(true)), &memory).is_err());
        drop(held);
        assert_eq!(pool.usage(), (1, 0, 0));
    }
    #[tokio::test]
    async fn input_frame_growth_and_owned_request_waits_are_charged_then_released() {
        let pool = Arc::new(Pool::default());
        let memory = pool.client(CancellationToken::new()).unwrap();
        let data = b"{\"id\":1}\n";
        let mut reader = &data[..];
        let frame = read_frame(&mut reader, &memory).await.unwrap().unwrap();
        assert_eq!(frame.line, "{\"id\":1}");
        assert!(pool.usage().2 > 0);
        drop(frame);
        assert_eq!(pool.usage(), (1, 0, 0));
        let mut invalid = &b"\xff\n"[..];
        assert!(read_frame(&mut invalid, &memory).await.is_err());
        assert_eq!(pool.usage(), (1, 0, 0));
    }
}
