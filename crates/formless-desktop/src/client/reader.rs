//! Serialized transport integration for the future-attempt validation path.
//! Shipping Client::start never enables it. No event asserts Run/action state.
use super::*;
use crate::attempt_admission::{Attachment, Change, Gate, Pending as Staging};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamOwner {
    pub client: uuid::Uuid,
    pub load: u64,
    pub connection: u64,
    pub view_epoch: u64,
    pub generation: u64,
    pub session_id: String,
    pub basis: uuid::Uuid,
}
impl StreamOwner {
    pub fn same_context(&self, other: &Self) -> bool {
        self.client == other.client
            && self.connection == other.connection
            && self.view_epoch == other.view_epoch
            && self.generation == other.generation
            && self.session_id == other.session_id
    }
}
#[derive(Debug)]
pub struct StreamStatus {
    pub steps: crate::steps::Inventory,
    pub running: bool,
    pub unfinished_run: Option<replies::UnfinishedRun>,
}
#[derive(Debug)]
pub struct StreamBatch {
    pub owner: StreamOwner,
    pub baseline: Option<SessionSnapshot>,
    pub events: Vec<Event>,
    pub reload: bool,
    pub status: Option<StreamStatus>,
}
impl StreamBatch {
    fn empty(owner: StreamOwner) -> Self {
        Self {
            owner,
            baseline: None,
            events: Vec::new(),
            reload: false,
            status: None,
        }
    }
}
#[derive(Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Off,
    NeedSubscribe,
    Subscribing,
    NeedBarrier,
    Loading,
    Watching,
}
#[derive(Default)]
pub(super) struct Reader {
    pub(super) requested: bool,
    pub(super) available: bool,
    blocked: bool,
    phase: Phase,
    owner: Option<StreamOwner>,
    attachment: Attachment,
    staging: Option<Staging>,
    gate: Option<Gate>,
    failed_loads: u8,
}
impl Reader {
    pub(super) fn new(requested: bool) -> Self {
        Self {
            requested,
            ..Default::default()
        }
    }
    pub(super) fn clear(&mut self) {
        let (requested, available) = (self.requested, self.available);
        *self = Self {
            requested,
            available,
            ..Default::default()
        };
    }
}
impl Engine {
    pub(super) fn sync_stream_epoch(&mut self, epoch: u64) {
        if self.stream_epoch != epoch {
            self.stream_epoch = epoch;
            self.reader.clear();
            self.reader.blocked = true;
            self.updates
                .retain(|update| !matches!(update, Update::Transcript(_)));
        }
    }
    fn stream_owner(&self) -> Option<StreamOwner> {
        Some(StreamOwner {
            client: self.instance,
            load: self
                .next_id
                .checked_add(1)
                .expect("stream read counter exhausted"),
            connection: self.connection,
            view_epoch: self.stream_epoch,
            generation: self.generation,
            session_id: self.active.clone()?,
            basis: uuid::Uuid::new_v4(),
        })
    }
    pub(super) fn owns_stream(&self, owner: &StreamOwner) -> bool {
        !self.reader.blocked
            && owner.client == self.instance
            && owner.connection == self.connection
            && owner.view_epoch == self.stream_epoch
            && owner.generation == self.generation
            && self.active.as_deref() == Some(&owner.session_id)
            && self.reader.owner.as_ref() == Some(owner)
    }
    fn stream_batch(&mut self, batch: StreamBatch) {
        // Handshake replay is one bounded UI update, even with 256 staged
        // frames. A partial batch is never silently dropped by delivery.
        if let Some(Update::Transcript(last)) = self.updates.last_mut() {
            if last.owner == batch.owner && batch.baseline.is_none() && !last.reload {
                last.events.extend(batch.events);
                last.reload |= batch.reload;
                if batch.status.is_some() {
                    last.status = batch.status;
                }
                return;
            }
        }
        self.updates.push(Update::Transcript(batch));
    }
    pub(super) fn stream_subscribe(&mut self) -> bool {
        if !self.reader.requested || !self.reader.available || self.reader.blocked {
            return false;
        }
        let Some(owner) = self.stream_owner() else {
            return false;
        };
        self.reader.clear();
        self.reader.owner = Some(owner);
        self.reader.phase = Phase::NeedSubscribe;
        self.stream_send_subscription();
        true
    }
    fn stream_send_subscription(&mut self) {
        if self.reader.phase != Phase::NeedSubscribe
            || self
                .pending
                .values()
                .any(|pending| matches!(pending, Pending::StreamSubscribe(_)))
        {
            return;
        }
        let Some(owner) = self.reader.owner.clone() else {
            return;
        };
        self.reader.phase = Phase::Subscribing;
        self.request(
            Request::SubscribeTranscriptAttempts {
                session_id: owner.session_id.clone(),
            },
            Pending::StreamSubscribe(owner),
        );
    }
    pub(super) fn stream_subscribed(&mut self, owner: StreamOwner, result: Result<Value, String>) {
        if !self.owns_stream(&owner) || self.reader.phase != Phase::Subscribing {
            return;
        }
        if !result.as_ref().is_ok_and(|value| {
            value["subscribed"] == true && value["mode"] == "future_attempts_only_v1"
        }) {
            self.stream_disable("Future-attempt subscription unavailable; using stored messages.");
            return;
        }
        self.ready = true;
        self.dirty_list = true;
        self.dirty_session = true;
        self.reader.phase = Phase::NeedBarrier;
        self.updates.push(Update::Connection {
            ready: true,
            notice: "Connected · live reader validation · stored-message fallback".into(),
        });
        self.emit_questions();
        if let Some(page) = self.page {
            self.load_page(page);
        }
    }
    pub(super) fn stream_refresh(&mut self) -> bool {
        if self.reader.requested && self.reader.blocked {
            return true;
        }
        if !self.reader.requested || !self.reader.available || self.reader.phase == Phase::Off {
            return false;
        }
        if self.reader.phase == Phase::NeedSubscribe {
            self.stream_send_subscription();
            return true;
        }
        // Retired reads may finish, but cannot create an unbounded backlog on
        // rapid A→B→A or repeated Refresh. Timeout still reconnects canonically.
        if self.pending.values().any(|pending| {
            matches!(
                pending,
                Pending::StreamBarrier { .. } | Pending::StreamStatus { .. }
            )
        }) {
            return true;
        }
        if self.reader.phase == Phase::NeedBarrier {
            let Some(owner) = self.stream_owner() else {
                return true;
            };
            self.reader.attachment = Attachment::default();
            let Ok(staging) = Staging::new(self.reader.attachment.clone(), &owner.session_id)
            else {
                self.stream_disable("Invalid live-reader Session; using stored messages.");
                return true;
            };
            self.reader.owner = Some(owner.clone());
            self.reader.staging = Some(staging);
            self.reader.gate = None;
            self.reader.phase = Phase::Loading;
            self.dirty_session = false;
            self.request(
                Request::GetTranscriptBarrier {
                    id: owner.session_id.clone(),
                },
                Pending::StreamBarrier {
                    owner,
                    questions_revision: self.questions.revision(),
                    status_revision: self.status_revision,
                },
            );
        } else if self.reader.phase == Phase::Watching && self.dirty_session {
            let Some(owner) = self.reader.owner.clone() else {
                return true;
            };
            self.dirty_session = false;
            self.request(
                Request::GetSession {
                    id: owner.session_id.clone(),
                },
                Pending::StreamStatus {
                    owner,
                    questions_revision: self.questions.revision(),
                    status_revision: self.status_revision,
                },
            );
        }
        true
    }
    pub(super) fn stream_reload(&mut self) {
        if self.reader.phase == Phase::Off {
            return;
        }
        if matches!(self.reader.phase, Phase::NeedSubscribe | Phase::Subscribing) {
            self.dirty_session = true;
            return;
        }
        if let Some(owner) = self.reader.owner.clone() {
            let mut batch = StreamBatch::empty(owner);
            batch.reload = true;
            self.stream_batch(batch);
        }
        self.reader.gate = None;
        self.reader.staging = None;
        self.reader.phase = Phase::NeedBarrier;
        self.dirty_session = true;
    }
    fn stream_disable(&mut self, notice: &str) {
        self.stream_reload();
        self.reader.clear();
        self.reader.available = false;
        self.dirty_session = true;
        self.updates.push(Update::Notice(notice.into()));
        self.subscribe();
    }
    fn stream_questions(&mut self, value: &Value, revision: u64) {
        if let Some(events) = value.get("pending_events").and_then(Value::as_array) {
            if self.questions.snapshot(events, revision) {
                self.emit_questions();
            } else if revision != self.questions.revision() {
                self.dirty_session = true;
            }
        }
    }
    pub(super) fn stream_loaded(
        &mut self,
        owner: StreamOwner,
        revision: u64,
        status_revision: u64,
        result: Result<Value, String>,
    ) {
        // Validate request, selection, physical connection and synchronous
        // view invalidation before touching an old reply's JSON.
        if !self.owns_stream(&owner) || self.reader.phase != Phase::Loading {
            return;
        }
        if status_revision != self.status_revision {
            self.stream_reload();
            self.reader.failed_loads = self.reader.failed_loads.saturating_add(1);
            if self.reader.failed_loads >= 3 {
                self.stream_disable("Live status changed repeatedly; using stored messages.");
            }
            return;
        }
        let Some(staging) = self.reader.staging.take() else {
            return;
        };
        let loaded = result
            .as_ref()
            .ok()
            .and_then(|value| staging.finish(&self.reader.attachment, value).ok());
        let Some(loaded) = loaded else {
            self.stream_disable("Live barrier unavailable; using stored messages.");
            return;
        };
        if let Ok(value) = result {
            self.stream_questions(&value["snapshot"], revision);
        }
        self.reader.gate = Some(loaded.gate);
        self.reader.phase = Phase::Watching;
        let mut batch = StreamBatch::empty(owner);
        batch.baseline = Some(loaded.snapshot);
        self.stream_batch(batch);
        for event in loaded.buffered {
            self.stream_event(&event);
        }
        if self.reader.phase != Phase::Watching {
            self.reader.failed_loads = self.reader.failed_loads.saturating_add(1);
            if self.reader.failed_loads >= 3 {
                self.stream_disable("Live handshake changed repeatedly; using stored messages.");
            }
        } else {
            self.reader.failed_loads = 0;
        }
    }
    pub(super) fn stream_status(
        &mut self,
        owner: StreamOwner,
        revision: u64,
        status_revision: u64,
        result: Result<Value, String>,
    ) {
        if !self.owns_stream(&owner) || self.reader.phase != Phase::Watching {
            return;
        }
        if status_revision != self.status_revision {
            self.dirty_session = true;
            return;
        }
        let Ok(value) = result else {
            self.stream_disable("Session status unavailable; using stored messages.");
            return;
        };
        let Ok(snapshot) = replies::session_snapshot(&owner.session_id, &value) else {
            self.stream_disable("Session status invalid; using stored messages.");
            return;
        };
        let same_sequence = self
            .reader
            .gate
            .as_ref()
            .is_some_and(|gate| gate.canonical_seq() == snapshot.last_seq.unwrap_or(0));
        if !same_sequence {
            self.stream_reload();
            return;
        }
        self.stream_questions(&value, revision);
        // Never replace a live canonical baseline from a periodic status read.
        // Content changes are reconciled by canonical events or a fresh load.
        let mut batch = StreamBatch::empty(owner);
        batch.status = Some(StreamStatus {
            steps: snapshot.steps,
            running: snapshot.running,
            unfinished_run: snapshot.unfinished_run,
        });
        self.stream_batch(batch);
    }
    pub(super) fn stream_event(&mut self, event: &Event) -> bool {
        let Some(owner) = self.reader.owner.clone() else {
            return false;
        };
        if !self.owns_stream(&owner) {
            return false;
        }
        let relevant = crate::transcript::affects_transcript(event, &owner.session_id)
            || matches!(event,Event::TranscriptAttempt{update} if update.session_id==owner.session_id);
        if self.reader.phase == Phase::Loading {
            if self
                .reader
                .staging
                .as_mut()
                .is_some_and(|staging| staging.observe(&self.reader.attachment, event).is_err())
            {
                self.stream_disable("Live handshake staging limit reached; using stored messages.");
            }
            return relevant;
        }
        if self.reader.phase != Phase::Watching {
            return relevant;
        }
        let Some(gate) = self.reader.gate.as_mut() else {
            return relevant;
        };
        let mut batch = StreamBatch::empty(owner.clone());
        match gate.event(&self.reader.attachment, event) {
            Change::Ignore | Change::Started => {}
            Change::Append(text) => {
                if let Event::TranscriptAttempt { update } = event {
                    batch.events.push(Event::MessageDelta {
                        session_id: owner.session_id.clone(),
                        run_id: update.run_id.clone(),
                        delta: text.into(),
                    });
                }
            }
            Change::Canonical(_) => {
                if crate::attempt_admission::fits_event(event) {
                    batch.events.push(event.clone());
                } else {
                    batch.reload = true;
                }
            }
            Change::Reload => batch.reload = true,
        }
        let reload = batch.reload;
        if !batch.events.is_empty() || reload {
            self.stream_batch(batch);
        }
        if reload {
            self.reader.gate = None;
            self.reader.phase = Phase::NeedBarrier;
            self.dirty_session = true;
        }
        relevant
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use formless_protocol::{TranscriptAttemptChange as ChangeWire, TranscriptAttemptEvent};
    use serde_json::json;
    const EPOCH: &str = "3a354012-349e-4df9-a277-be97446c0868";
    fn snapshot(seq: i64, running: bool) -> Value {
        let messages = if seq == 0 {
            json!([])
        } else {
            json!([{"id":"m","session_id":"a","seq":seq,"role":"user","content":"Question","created_at":"now"}])
        };
        json!({"session":{"id":"a"},"messages":messages,"running":running,"pending_events":[]})
    }
    fn reply(seq: i64) -> Value {
        json!({"snapshot":snapshot(seq,false),"barrier":{"epoch":EPOCH,"cursor":10},"mode":"future_attempts_only_v1"})
    }
    fn request(engine: &mut Engine, kind: &str) -> RequestEnvelope {
        let at = engine
            .outgoing
            .iter()
            .position(|request| serde_json::to_value(&request.request).unwrap()["type"] == kind);
        engine
            .outgoing
            .remove(at.unwrap_or_else(|| panic!("missing request {kind}")))
            .unwrap()
    }
    fn respond(engine: &mut Engine, id: u64, value: Value) {
        engine.receive(ServerMessage::ok(id, value)).unwrap();
    }
    fn setup(requested: bool, capable: bool) -> Engine {
        let mut engine = Engine {
            reader: Reader::new(requested),
            active: Some("a".into()),
            generation: 1,
            ..Default::default()
        };
        engine.begin();
        let ping = request(&mut engine, "ping");
        respond(
            &mut engine,
            ping.id,
            json!({"min_protocol":1,"max_protocol":1,"features":if capable {vec!["future_transcript_attempts_v1"]}else{vec![]}}),
        );
        engine
    }
    fn loading() -> (Engine, u64) {
        let mut engine = setup(true, true);
        let subscribe = request(&mut engine, "subscribe_transcript_attempts");
        respond(
            &mut engine,
            subscribe.id,
            json!({"subscribed":true,"mode":"future_attempts_only_v1"}),
        );
        engine.refresh();
        let load = request(&mut engine, "get_transcript_barrier");
        engine.updates.clear();
        (engine, load.id)
    }
    fn watching() -> Engine {
        let (mut engine, id) = loading();
        respond(&mut engine, id, reply(1));
        engine.updates.clear();
        engine
    }
    fn wire(cursor: u64, change: ChangeWire) -> Event {
        Event::TranscriptAttempt {
            update: TranscriptAttemptEvent {
                epoch: EPOCH.into(),
                cursor,
                session_id: "a".into(),
                run_id: "run".into(),
                turn_id: "turn".into(),
                attempt_id: "attempt".into(),
                change,
            },
        }
    }
    fn start() -> Event {
        wire(11, ChangeWire::Started { base_seq: 1 })
    }
    fn delta(cursor: u64, offset: u64, text: &str) -> Event {
        wire(
            cursor,
            ChangeWire::Delta {
                offset,
                text: text.into(),
            },
        )
    }
    #[test]
    fn status_reply_sampled_before_run_start_cannot_restore_old_metadata() {
        let mut engine = watching();
        engine.dirty_session = true;
        engine.refresh();
        let status = request(&mut engine, "get_session");
        let owner = engine.reader.owner.clone().unwrap();
        engine
            .receive(ServerMessage::Event(Event::RunStarted {
                session_id: "a".into(),
                run_id: "run".into(),
            }))
            .unwrap();
        engine.receive(ServerMessage::Event(start())).unwrap();
        engine
            .receive(ServerMessage::Event(delta(12, 0, "prefix")))
            .unwrap();
        engine.updates.clear();
        respond(&mut engine, status.id, snapshot(1, false));
        assert!(!engine.updates.iter().any(|update| matches!(update,
            Update::Transcript(batch) if batch.status.is_some())));
        assert!(engine.reader.phase == Phase::Watching);
        assert_eq!(engine.reader.owner.as_ref(), Some(&owner));
        engine.refresh();
        let fresh = request(&mut engine, "get_session");
        respond(&mut engine, fresh.id, snapshot(1, true));
        assert!(engine.updates.iter().any(|update| matches!(update,
            Update::Transcript(batch) if batch.status.as_ref().is_some_and(|status| status.running))));
        engine.updates.clear();
        engine
            .receive(ServerMessage::Event(delta(13, 6, " tail")))
            .unwrap();
        assert!(engine.updates.iter().any(|update| matches!(update,
            Update::Transcript(batch) if !batch.reload && !batch.events.is_empty())));
    }
    fn lifecycle_event() -> Event {
        Event::StepResolved {
            session_id: "a".into(),
            step_id: "step".into(),
        }
    }
    #[test]
    fn obsolete_malformed_status_is_ignored_before_decoding() {
        let mut engine = watching();
        engine.dirty_session = true;
        engine.refresh();
        let status = request(&mut engine, "get_session");
        engine
            .receive(ServerMessage::Event(lifecycle_event()))
            .unwrap();
        engine.updates.clear();
        respond(&mut engine, status.id, json!(null));
        assert!(engine.updates.is_empty());
        assert!(engine.reader.phase == Phase::Watching);
        assert!(engine.dirty_session);
    }
    #[test]
    fn overlapped_barrier_retries_before_installing_metadata_and_is_bounded() {
        let (mut engine, mut id) = loading();
        for overlap in 0..3 {
            engine
                .receive(ServerMessage::Event(lifecycle_event()))
                .unwrap();
            engine.updates.clear();
            respond(&mut engine, id, json!(null));
            assert!(!engine.updates.iter().any(|u| matches!(u,
                Update::Transcript(b) if b.baseline.is_some())));
            if overlap < 2 {
                assert!(engine.reader.phase == Phase::NeedBarrier);
                engine.refresh();
                id = request(&mut engine, "get_transcript_barrier").id;
            }
        }
        assert!(!engine.reader.available);
        assert!(matches!(
            request(&mut engine, "subscribe").request,
            Request::Subscribe { deltas: false, .. }
        ));
    }
    #[test]
    fn fresh_barrier_after_lifecycle_overlap_establishes_new_status() {
        let (mut engine, id) = loading();
        engine
            .receive(ServerMessage::Event(lifecycle_event()))
            .unwrap();
        respond(&mut engine, id, reply(1));
        engine.refresh();
        let id = request(&mut engine, "get_transcript_barrier").id;
        let mut fresh = reply(1);
        fresh["snapshot"]["running"] = json!(true);
        engine.updates.clear();
        respond(&mut engine, id, fresh);
        assert!(engine.updates.iter().any(|u| matches!(u,
            Update::Transcript(b) if b.baseline.as_ref().is_some_and(|s| s.running))));
        assert!(engine.reader.phase == Phase::Watching);
        assert_eq!(engine.reader.failed_loads, 0);
    }
    #[test]
    fn canonical_reader_also_rejects_pre_lifecycle_snapshot() {
        let mut engine = setup(false, false);
        let subscribe = request(&mut engine, "subscribe");
        respond(&mut engine, subscribe.id, json!({"subscribed":true}));
        engine.refresh();
        let stale = request(&mut engine, "get_session");
        engine
            .receive(ServerMessage::Event(lifecycle_event()))
            .unwrap();
        engine.updates.clear();
        respond(&mut engine, stale.id, json!(null));
        assert!(engine.updates.is_empty());
        engine.refresh();
        let fresh = request(&mut engine, "get_session");
        respond(&mut engine, fresh.id, snapshot(1, true));
        assert!(engine
            .updates
            .iter()
            .any(|u| matches!(u, Update::Snapshot { snapshot, .. } if snapshot.running)));
    }
    #[test]
    fn foreign_lifecycle_does_not_invalidate_selected_status() {
        let mut engine = watching();
        let revision = engine.status_revision;
        engine
            .receive(ServerMessage::Event(Event::RunStarted {
                session_id: "other".into(),
                run_id: "run".into(),
            }))
            .unwrap();
        assert_eq!(engine.status_revision, revision);
        assert!(!engine
            .updates
            .iter()
            .any(|u| matches!(u, Update::SessionStatusInvalidated { .. })));
        engine.status_revision = u64::MAX;
        assert!(engine
            .receive(ServerMessage::Event(lifecycle_event()))
            .is_err());
    }
    #[test]
    fn reordered_start_after_ignored_future_frame_retires_live_and_staged_admission() {
        for staged in [false, true] {
            let (mut engine, load) = if staged { loading() } else { (watching(), 0) };
            engine
                .receive(ServerMessage::Event(delta(12, 0, "missing")))
                .unwrap();
            engine.receive(ServerMessage::Event(start())).unwrap();
            engine
                .receive(ServerMessage::Event(delta(13, 0, "must not appear")))
                .unwrap();
            if staged {
                respond(&mut engine, load, reply(1));
            }
            assert!(engine
                .updates
                .iter()
                .any(|u| matches!(u, Update::Transcript(b) if b.reload)));
            assert!(!engine
                .updates
                .iter()
                .any(|u| matches!(u, Update::Transcript(b)
                if b.events.iter().any(|e| matches!(e, Event::MessageDelta { .. })))));
            assert!(engine.reader.phase == Phase::NeedBarrier);
            engine.refresh();
            let fresh = request(&mut engine, "get_transcript_barrier");
            let mut value = reply(1);
            value["barrier"]["cursor"] = json!(20);
            respond(&mut engine, fresh.id, value);
            let mut next = start();
            if let Event::TranscriptAttempt { update } = &mut next {
                update.cursor = 21;
            }
            engine.receive(ServerMessage::Event(next)).unwrap();
            engine
                .receive(ServerMessage::Event(delta(22, 0, "fresh")))
                .unwrap();
            assert!(engine.updates.iter().any(|u| matches!(u, Update::Transcript(b)
                if b.events.iter().any(|e| matches!(e, Event::MessageDelta { delta, .. } if delta == "fresh")))));
        }
    }
    #[test]
    fn shipping_mode_and_old_or_lying_peers_keep_canonical_subscription() {
        for (requested, capable) in [(false, true), (true, false)] {
            let mut engine = setup(requested, capable);
            let subscribe = request(&mut engine, "subscribe");
            assert!(matches!(
                subscribe.request,
                Request::Subscribe { deltas: false, .. }
            ));
            assert!(!engine.outgoing.iter().any(|request| matches!(
                request.request,
                Request::SubscribeTranscriptAttempts { .. }
            )));
        }
        let mut engine = setup(true, true);
        let subscribe = request(&mut engine, "subscribe_transcript_attempts");
        respond(
            &mut engine,
            subscribe.id,
            json!({"subscribed":true,"mode":"unknown"}),
        );
        assert!(!engine.reader.available);
        assert!(matches!(
            request(&mut engine, "subscribe").request,
            Request::Subscribe { deltas: false, .. }
        ));
    }
    #[test]
    fn pre_reply_future_prefix_is_replayed_as_one_owned_ui_batch() {
        let (mut engine, id) = loading();
        engine.receive(ServerMessage::Event(start())).unwrap();
        engine
            .receive(ServerMessage::Event(delta(12, 0, "ไทย😀")))
            .unwrap();
        respond(&mut engine, id, reply(1));
        let batches = engine
            .updates
            .iter()
            .filter_map(|update| {
                if let Update::Transcript(batch) = update {
                    Some(batch)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(batches.len(), 1);
        let batch = batches[0];
        assert!(batch.baseline.is_some());
        assert!(!batch.reload);
        assert_eq!(batch.events.len(), 1);
        assert!(matches!(&batch.events[0],Event::MessageDelta{delta,..} if delta=="ไทย😀"));
        assert_eq!(batch.owner.generation, 1);
        assert_eq!(batch.owner.connection, 0);
        assert_eq!(batch.owner.view_epoch, 0);
    }
    #[test]
    fn status_reads_never_replace_live_baseline_or_infer_run_state_from_chunks() {
        let mut engine = watching();
        engine.receive(ServerMessage::Event(start())).unwrap();
        engine
            .receive(ServerMessage::Event(delta(12, 0, "partial")))
            .unwrap();
        assert!(engine
            .updates
            .iter()
            .all(|update| !matches!(update,Update::Transcript(batch) if batch.status.is_some())));
        engine.updates.clear();
        engine.dirty_session = true;
        engine.refresh();
        let status = request(&mut engine, "get_session");
        respond(&mut engine, status.id, snapshot(1, true));
        assert!(engine.updates.iter().any(|update|matches!(update,Update::Transcript(batch) if batch.baseline.is_none()&&batch.status.as_ref().is_some_and(|status|status.running)&&!batch.reload)));
        engine.updates.clear();
        engine.dirty_session = true;
        engine.refresh();
        let status = request(&mut engine, "get_session");
        respond(&mut engine, status.id, snapshot(2, true));
        assert!(engine.updates.iter().any(|update|matches!(update,Update::Transcript(batch) if batch.reload&&batch.status.is_none())));
    }
    #[test]
    fn synchronous_view_retirement_refuses_old_reply_before_json_decode() {
        let (mut engine, id) = loading();
        engine.sync_stream_epoch(1);
        respond(&mut engine, id, Value::Null);
        assert!(!engine
            .updates
            .iter()
            .any(|update| matches!(update, Update::Transcript(_) | Update::SnapshotError { .. })));
        assert!(engine.reader.blocked);
        engine.select(Some("a".into()), 3);
        engine.refresh();
        assert_eq!(engine.generation, 3);
        assert!(!engine.reader.blocked);
    }
    #[test]
    fn forecast_epoch_changes_do_not_retire_the_separate_stream_view() {
        let mut engine = watching();
        engine.receive(ServerMessage::Event(start())).unwrap();
        engine.sync_epoch(55);
        engine
            .receive(ServerMessage::Event(delta(12, 0, "kept")))
            .unwrap();
        assert!(engine.updates.iter().any(|update|matches!(update,Update::Transcript(batch) if batch.events.iter().any(|event|matches!(event,Event::MessageDelta{delta,..} if delta=="kept")))));
    }
    #[test]
    fn staging_overflow_latches_legacy_and_old_barrier_cannot_restore_live_mode() {
        let (mut engine, id) = loading();
        for cursor in 11..268 {
            engine
                .receive(ServerMessage::Event(delta(cursor, 0, "")))
                .unwrap();
        }
        assert!(!engine.reader.available);
        respond(&mut engine, id, reply(1));
        assert!(!engine
            .updates
            .iter()
            .any(|update| matches!(update,Update::Transcript(batch) if batch.baseline.is_some())));
        assert!(engine
            .outgoing
            .iter()
            .any(|request| matches!(request.request, Request::Subscribe { deltas: false, .. })));
    }
    #[test]
    fn refresh_does_not_bypass_pending_subscription_and_reads_remain_bounded() {
        let mut engine = setup(true, true);
        engine.command(Command::Refresh);
        assert!(engine.reader.phase == Phase::Subscribing);
        let subscribe = request(&mut engine, "subscribe_transcript_attempts");
        respond(
            &mut engine,
            subscribe.id,
            json!({"subscribed":true,"mode":"future_attempts_only_v1"}),
        );
        engine.refresh();
        request(&mut engine, "get_transcript_barrier");
        for _ in 0..100 {
            engine.command(Command::Refresh);
            engine.refresh();
        }
        assert_eq!(
            engine
                .pending
                .values()
                .filter(|pending| matches!(
                    pending,
                    Pending::StreamBarrier { .. } | Pending::StreamStatus { .. }
                ))
                .count(),
            1
        );
    }
    #[test]
    fn ui_queue_saturation_is_a_connection_failure_not_a_silent_delta_drop() {
        let mut engine = watching();
        engine.receive(ServerMessage::Event(start())).unwrap();
        engine
            .receive(ServerMessage::Event(delta(12, 0, "partial")))
            .unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        tx.try_send(Update::Notice("occupied".into())).unwrap();
        assert!(super::super::deliver(&tx, &mut engine).is_err());
        drop(rx);
    }
}

#[cfg(test)]
mod overlap_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn three_overlapped_barriers_latch_canonical_fallback_without_more_live_reads() {
        let mut engine = Engine {
            reader: Reader::new(true),
            active: Some("a".into()),
            generation: 1,
            ready: true,
            ..Default::default()
        };
        engine.reader.available = true;
        let owner = engine.stream_owner().unwrap();
        engine.reader.owner = Some(owner);
        engine.reader.phase = Phase::NeedBarrier;
        for _ in 0..3 {
            engine.refresh();
            let index = engine
                .outgoing
                .iter()
                .position(|request| matches!(request.request, Request::GetTranscriptBarrier { .. }))
                .unwrap();
            let request = engine.outgoing.remove(index).unwrap();
            engine
                .receive(ServerMessage::Event(Event::MessagesEdited {
                    session_id: "a".into(),
                }))
                .unwrap();
            engine.receive(ServerMessage::ok(request.id,json!({"snapshot":{"session":{"id":"a"},"messages":[],"running":false,"pending_events":[]},"barrier":{"epoch":"34c91207-90ec-47cf-99e4-9e080d96bc2c","cursor":1},"mode":"future_attempts_only_v1"}))).unwrap();
        }
        assert!(!engine.reader.available);
        assert!(engine
            .outgoing
            .iter()
            .any(|request| matches!(request.request, Request::Subscribe { deltas: false, .. })));
        engine.outgoing.clear();
        for _ in 0..20 {
            engine.refresh();
        }
        assert!(!engine.outgoing.iter().any(|request| matches!(
            request.request,
            Request::GetTranscriptBarrier { .. } | Request::SubscribeTranscriptAttempts { .. }
        )));
    }
}
