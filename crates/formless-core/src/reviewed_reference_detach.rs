//! Bounded connection-local receipts for the synchronous compare/remove operation.
//! No cached result is evidence of current inventory state or durable idempotency.
use super::*;
pub(super) const MAX_RECEIPTS: usize = 256;
pub(super) const RETENTION: std::time::Duration = std::time::Duration::from_secs(600);
struct Receipt {
    at: std::time::Instant,
    result: std::result::Result<Value, CachedSendFailure>,
}
#[derive(Default)]
pub(super) struct Receipts(StdMutex<HashMap<SendKey, Receipt>>);
impl Receipts {
    pub(super) fn retire_connection(&self, connection: u64) {
        self.0
            .locked()
            .retain(|(owner, _, _), _| *owner != Some(connection));
    }
}
impl Daemon {
    pub(super) fn detach_reviewed_reference(
        &self,
        session: String,
        expected: Vec<Value>,
        index: u32,
        operation: String,
    ) -> Result<Value> {
        reviewed_send::validate(&session, &expected)?;
        if operation.is_empty() || operation.len() > 256 || operation.chars().any(char::is_control)
        {
            return Err(RequestFailure::invalid(
                "a bounded nonempty reference-removal action identity is required",
            )
            .into());
        }
        let key = (
            current_client().map(|(connection, _)| connection),
            session.clone(),
            operation.clone(),
        );
        // The operation does not await. Serializing the claim, transaction and
        // receipt prevents concurrent same-key calls from removing two rows.
        let mut receipts = self.reference_detaches.0.locked();
        receipts.retain(|_, receipt| receipt.at.elapsed() < RETENTION);
        if let Some(receipt) = receipts.get(&key) {
            return receipt
                .result
                .clone()
                .map_err(CachedSendFailure::into_error);
        }
        if receipts.len() >= MAX_RECEIPTS {
            return Err(RequestFailure::busy(
                "reference-removal receipt capacity reached; no reference was removed",
            )
            .into());
        }
        let result = self.with_store(|st| st.detach_reviewed_reference(&session, &expected, index))
            .map(|()| json!({"removed":true,"session_id":session,"index":index,"operation_id":operation}))
            .map_err(CachedSendFailure::capture);
        receipts.insert(
            key,
            Receipt {
                at: std::time::Instant::now(),
                result: result.clone(),
            },
        );
        result.map_err(CachedSendFailure::into_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn reviewed_reference_receipt_prevents_restored_inventory_replay_only_inside_its_retention_window(
    ) {
        let _env = crate::credential_env::EnvGuard::isolated();
        let root =
            std::env::temp_dir().join(format!("fl-reference-expiry-{}", uuid::Uuid::new_v4()));
        let paths = Paths::from_dirs(root.join("d"), root.join("c")).unwrap();
        let mut cfg = Config::default();
        cfg.provider.auto_test_models = false;
        cfg.naming.enabled = false;
        cfg.skills.auto_create = false;
        cfg.save(&paths).unwrap();
        let daemon = Arc::new(Daemon::new(paths).unwrap());
        let sid = daemon
            .with_store(|st| Ok(st.create_session(None)?.id))
            .unwrap();
        let row = json!({"reference":true,"name":"same","path":"/not/opened","dir":true,"bytes":0});
        let add = || {
            daemon
                .with_store(|st| st.add_pending_attachment(&sid, &row))
                .unwrap()
        };
        add();
        let expected = vec![row.clone()];
        let first = daemon
            .detach_reviewed_reference(sid.clone(), expected.clone(), 0, "one".into())
            .unwrap();
        add();
        assert_eq!(
            daemon
                .detach_reviewed_reference(sid.clone(), expected.clone(), 0, "one".into())
                .unwrap(),
            first
        );
        assert_eq!(
            daemon
                .with_reader(|st| st.pending_attachment_inventory(&sid))
                .unwrap()
                .len(),
            1,
            "restoring identical metadata cannot replay a live cached action"
        );
        for receipt in daemon.reference_detaches.0.locked().values_mut() {
            receipt.at = std::time::Instant::now() - RETENTION - std::time::Duration::from_secs(1);
        }
        daemon
            .detach_reviewed_reference(sid.clone(), expected, 0, "one".into())
            .unwrap();
        assert!(
            daemon
                .with_reader(|st| st.pending_attachment_inventory(&sid))
                .unwrap()
                .is_empty(),
            "expiry is an explicit idempotency limit, not durable protection"
        );
        let weak = Arc::downgrade(&daemon);
        drop(daemon);
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while weak.strong_count() > 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
