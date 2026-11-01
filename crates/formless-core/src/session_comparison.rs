//! Local branch comparison: transcript lineage is not a billing allocation.
use super::*;
use formless_protocol::{
    SessionBranchUsage, SessionComparison, SessionComparisonSide, SessionMessageDiff,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, thiserror::Error)]
pub(crate) enum SessionComparisonFailure {
    #[error("Session not found")]
    NotFound,
    #[error("Comparison exceeds the safe RPC detail budget ({limit} bytes). No partial diff was sent. Use `formless sessions compare --json` with the daemon stopped for complete offline output.")]
    ReplyTooLarge { limit: usize },
}

impl Store {
    pub fn compare_sessions(
        &self,
        left: &str,
        right: &str,
        cfg: &crate::Config,
    ) -> Result<SessionComparison> {
        self.compare_sessions_with_budget(left, right, cfg, None)
    }

    pub fn compare_sessions_for_rpc(
        &self,
        left: &str,
        right: &str,
        cfg: &crate::Config,
    ) -> Result<SessionComparison> {
        self.compare_sessions_with_budget(
            left,
            right,
            cfg,
            Some(formless_protocol::MAX_FRAME_BYTES / 4),
        )
    }

    pub(super) fn compare_sessions_with_budget(
        &self,
        left: &str,
        right: &str,
        cfg: &crate::Config,
        budget: Option<usize>,
    ) -> Result<SessionComparison> {
        self.read_transaction(|st| {
            for id in [left, right] {
                let exists: bool = st.conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1 AND deleted_at IS NULL)",
                    params![id], |r| r.get(0),
                )?;
                if !exists { return Err(SessionComparisonFailure::NotFound.into()); }
            }
            if let Some(limit) = budget {
                // Worst-case JSON escaping plus per-message keys, before loading
                // bodies. UNION ALL counts both sides even when IDs are equal.
                let bytes: i64 = st.conn.query_row("SELECT COALESCE(SUM(bytes),0) FROM (
                    SELECT 6*(length(CAST(id AS BLOB))+length(CAST(session_id AS BLOB))+length(CAST(role AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(content AS BLOB))+COALESCE(length(CAST(meta AS BLOB)),0))+512 AS bytes FROM messages WHERE session_id=?1
                    UNION ALL
                    SELECT 6*(length(CAST(id AS BLOB))+length(CAST(session_id AS BLOB))+length(CAST(role AS BLOB))+length(CAST(created_at AS BLOB))+length(CAST(content AS BLOB))+COALESCE(length(CAST(meta AS BLOB)),0))+512 AS bytes FROM messages WHERE session_id=?2)",params![left,right],|r|r.get(0))?;
                if bytes < 0 || bytes as u128 > limit as u128 {
                    return Err(SessionComparisonFailure::ReplyTooLarge { limit }.into());
                }
            }
            let read = |id: &str| -> Result<SessionInfo> {
                let session = st.get_session(id)?.context(SessionComparisonFailure::NotFound)?;
                if st.session_deleted(id)? {
                    return Err(SessionComparisonFailure::NotFound.into());
                }
                Ok(session)
            };
            let left = read(left)?;
            let right = read(right)?;
            let left_path = st.lineage_path(&left)?;
            let right_path = st.lineage_path(&right)?;
            let common = right_path.iter().find_map(|(id, right_cut)| {
                left_path.iter().find(|(other, _)| other == id).map(|(_, left_cut)| {
                    let cut = match (left_cut, right_cut) {
                        (Some(a), Some(b)) => Some((*a).min(*b)),
                        (Some(a), None) | (None, Some(a)) => Some(*a),
                        (None, None) => None,
                    };
                    (id.clone(), cut)
                })
            });
            let (common_ancestor, fork_point_seq) = common.map_or((None, None), |(id, cut)| (Some(id), cut));
            let left_messages = st.list_messages(&left.id)?;
            let right_messages = st.list_messages(&right.id)?;
            let cut = fork_point_seq.unwrap_or(0);
            let prefix_changed = fork_point_seq.is_some() && !same_prefix(&left_messages, &right_messages, cut);
            let diff_from_seq = if prefix_changed { 0 } else { cut };
            let mut warnings = Vec::new();
            if common_ancestor.is_none() {
                warnings.push("No recorded common ancestor; comparing full stored histories without inventing lineage.".into());
            }
            if prefix_changed {
                warnings.push("The copied prefix has changed since the fork. The diff includes full histories; usage scope still uses the recorded fork boundary.".into());
            }
            if let Some(id) = &common_ancestor {
                if st.get_session(id)?.is_none() || st.session_deleted(id)? {
                    warnings.push("The recorded common parent is deleted or hidden; its ID and the surviving branches' fork points are retained.".into());
                }
            }
            if left.id == right.id {
                warnings.push("Both sides are the same Session; displayed usage is the same physical work, not two separate charges.".into());
            }
            let left_tail: Vec<_> = left_messages.iter().filter(|m| m.seq > diff_from_seq).collect();
            let right_tail: Vec<_> = right_messages.iter().filter(|m| m.seq > diff_from_seq).collect();
            if left_tail.len().saturating_mul(right_tail.len()) > 1_000_000 {
                warnings.push("Large histories: both complete tails are shown without quadratic message alignment; no messages are omitted.".into());
            }
            let messages = message_diff(left_tail, right_tail);
            let left_usage = st.branch_usage(&left, cut, &left_messages, cfg)?;
            let right_usage = st.branch_usage(&right, cut, &right_messages, cfg)?;
            let result = SessionComparison {
                left: SessionComparisonSide { session: left, usage: left_usage },
                right: SessionComparisonSide { session: right, usage: right_usage },
                common_ancestor,
                fork_point_seq,
                diff_from_seq,
                prefix_changed,
                messages,
                warnings,
            };
            if let Some(limit) = budget {
                if serde_json::to_vec(&result)?.len() > limit {
                    return Err(SessionComparisonFailure::ReplyTooLarge { limit }.into());
                }
            }
            Ok(result)
        })
    }

    fn lineage_path(&self, session: &SessionInfo) -> Result<Vec<(String, Option<i64>)>> {
        let mut path = vec![(session.id.clone(), None)];
        let mut current = session.clone();
        let mut seen = BTreeSet::from([current.id.clone()]);
        let mut cut: Option<i64> = None;
        while let (Some(parent), Some(point)) = (current.forked_from, current.fork_point_seq) {
            if !seen.insert(parent.clone()) {
                anyhow::bail!("cyclic Session lineage; comparison refused");
            }
            cut = Some(cut.map_or(point, |previous| previous.min(point)));
            path.push((parent.clone(), cut));
            let Some(next) = self.get_session(&parent)? else {
                break;
            };
            current = next;
        }
        Ok(path)
    }

    fn branch_usage(
        &self,
        session: &SessionInfo,
        cut: i64,
        messages: &[MessageInfo],
        cfg: &crate::Config,
    ) -> Result<SessionBranchUsage> {
        let all_owned = cut == 0 || session.fork_point_seq.is_some_and(|birth| cut <= birth);
        let valid = |id: &Option<String>, seq: i64| -> bool {
            (seq == 0 && id.is_none())
                || messages
                    .iter()
                    .any(|m| Some(&m.id) == id.as_ref() && m.seq == seq)
        };
        // true=allocated, false=known pre-cut, None=not safely assignable.
        let mut runs = BTreeMap::new();
        let mut unknown = BTreeSet::new();
        let mut complete = true;
        let mut query = self.ps("SELECT id,status,start_message_id,start_message_seq,end_message_id,end_message_seq FROM runs WHERE session_id=?1")?;
        let records = query.query_map(params![session.id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<i64>>(5)?,
            ))
        })?;
        for record in records {
            let (id, status, start_id, start, end_id, end) = record?;
            let allocated =
                if all_owned || start.is_some_and(|seq| seq > cut && valid(&start_id, seq)) {
                    Some(true)
                } else if end.is_some_and(|seq| seq <= cut && valid(&end_id, seq)) {
                    Some(false)
                } else {
                    None
                };
            if allocated == Some(true)
                && (status == "running" || !self.forecast_usage_complete(&id)?)
            {
                complete = false;
            }
            if allocated.is_none() {
                unknown.insert(id.clone());
            }
            runs.insert(id, allocated);
        }
        let mut out = SessionBranchUsage {
            scope: if all_owned {
                "recorded local work owned by this Session; copied transcripts carry no copied charges".into()
            } else {
                format!("recorded whole Runs beginning after fork seq {cut}; no exact split of a straddling Run")
            },
            recorded_prompt_tokens: 0,
            recorded_completion_tokens: 0,
            recorded_cached_tokens: 0,
            recorded_cache_write_tokens: 0,
            requests: 0,
            unallocated_requests: 0,
            unallocated_prompt_tokens: 0,
            unallocated_completion_tokens: 0,
            unknown_runs: 0,
            usage_complete: false,
            cost: None,
            priced_cost_subtotal: 0.0,
            unallocated_priced_cost_subtotal: 0.0,
            known_records_cost_range: None,
            estimated_requests: 0,
            unpriced: Vec::new(),
            warnings: Vec::new(),
        };
        let (mut lower, mut upper) = (0.0, 0.0);
        let mut prices_complete = true;
        let mut query = self.ps("SELECT run_id,provider,model,prompt_tokens,completion_tokens,cached_tokens,cache_write_tokens,cost,usage_complete FROM run_usage WHERE session_id=?1")?;
        let records = query.query_map(params![session.id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, Option<f64>>(7)?,
                r.get::<_, i64>(8)?,
            ))
        })?;
        for record in records {
            let (run, provider, model, p, c, cache, write, reported, covered) = record?;
            let assignment = if all_owned {
                Some(true)
            } else {
                runs.get(&run).copied().flatten()
            };
            if assignment == Some(false) {
                continue;
            }
            if [p, c, cache, write].iter().any(|n| *n < 0) {
                anyhow::bail!("invalid negative stored usage counters");
            }
            let price = reported
                .or_else(|| {
                    cfg.provider_cfg(Some(&provider))
                        .ok()?
                        .cost_of(&model, p, c, cache, write, None)
                })
                .filter(|value| value.is_finite());
            if reported.is_none() && price.is_some() {
                out.estimated_requests += 1;
            }
            if price.is_none() {
                prices_complete = false;
                let name = format!(
                    "{}/{}",
                    if provider.is_empty() {
                        "default"
                    } else {
                        &provider
                    },
                    model
                );
                if !out.unpriced.contains(&name) {
                    out.unpriced.push(name);
                }
            }
            if assignment == Some(true) {
                if !runs.contains_key(&run) {
                    complete = false;
                }
                out.requests += 1;
                add_count(&mut out.recorded_prompt_tokens, p)?;
                add_count(&mut out.recorded_completion_tokens, c)?;
                add_count(&mut out.recorded_cached_tokens, cache)?;
                add_count(&mut out.recorded_cache_write_tokens, write)?;
                if let Some(price) = price {
                    add_price(&mut out.priced_cost_subtotal, price)?;
                }
                if covered != 1 {
                    complete = false;
                }
            } else {
                unknown.insert(run);
                out.unallocated_requests += 1;
                add_count(&mut out.unallocated_prompt_tokens, p)?;
                add_count(&mut out.unallocated_completion_tokens, c)?;
                if let Some(price) = price {
                    add_price(&mut out.unallocated_priced_cost_subtotal, price)?;
                    add_price(&mut lower, price.min(0.0))?;
                    add_price(&mut upper, price.max(0.0))?;
                }
            }
        }
        if runs.is_empty() && !messages.is_empty() && session.forked_from.is_none() {
            complete = false;
            out.warnings.push("This transcript has no tracked Runs; imported or historical charges cannot be reconstructed from its text.".into());
        }
        out.unknown_runs = unknown.len() as u64;
        out.usage_complete = complete && unknown.is_empty();
        if out.usage_complete && prices_complete {
            out.cost = Some(out.priced_cost_subtotal);
        }
        if prices_complete {
            add_price(&mut lower, out.priced_cost_subtotal)?;
            add_price(&mut upper, out.priced_cost_subtotal)?;
            out.known_records_cost_range = Some([lower, upper]);
        }
        if !out.usage_complete {
            out.warnings.push("Usage is partial: missing request coverage, running work or legacy/straddling Runs cannot be assigned exactly. Recorded counters/subtotals are not the complete bill.".into());
        }
        if out.estimated_requests > 0 {
            out.warnings.push("Some observed requests use today's configured provider/model prices; per-request provider-reported cost takes precedence.".into());
        }
        if !prices_complete {
            out.warnings.push("Unpriced observed requests make the total cost unknown; no dollar total or known-records range is invented.".into());
        }
        if out.known_records_cost_range.is_some() {
            out.warnings.push("The cost range covers observed priced rows only, excluding missing provider usage. It is not an upper bound on the final bill.".into());
        }
        Ok(out)
    }
}

fn add_count(total: &mut i64, amount: i64) -> Result<()> {
    *total = total
        .checked_add(amount)
        .context("stored usage totals exceed supported range")?;
    Ok(())
}
fn add_price(total: &mut f64, amount: f64) -> Result<()> {
    let combined = *total + amount;
    if !combined.is_finite() {
        anyhow::bail!("stored usage cost totals exceed supported range");
    }
    *total = combined;
    Ok(())
}
fn same_message(a: &MessageInfo, b: &MessageInfo) -> bool {
    a.role == b.role && a.content == b.content && a.meta == b.meta
}
fn same_prefix(a: &[MessageInfo], b: &[MessageInfo], cut: i64) -> bool {
    let mut a = a.iter().filter(|m| m.seq <= cut);
    let mut b = b.iter().filter(|m| m.seq <= cut);
    loop {
        match (a.next(), b.next()) {
            (None, None) => return true,
            (Some(a), Some(b)) if a.seq == b.seq && same_message(a, b) => {}
            _ => return false,
        }
    }
}
fn message_diff(a: Vec<&MessageInfo>, b: Vec<&MessageInfo>) -> Vec<SessionMessageDiff> {
    let row =
        |kind: &str, left: Option<&MessageInfo>, right: Option<&MessageInfo>| SessionMessageDiff {
            kind: kind.into(),
            left: left.cloned(),
            right: right.cloned(),
        };
    // Bound quadratic work without truncating any stored message.
    if a.is_empty() || b.is_empty() || a.len().saturating_mul(b.len()) > 1_000_000 {
        return a
            .into_iter()
            .map(|m| row("left_only", Some(m), None))
            .chain(b.into_iter().map(|m| row("right_only", None, Some(m))))
            .collect();
    }
    // Hash payloads once, rather than scan megabyte bodies in every DP cell.
    // Exact verification below prevents even a hash collision from inventing
    // an unchanged message. Hashing and aligned verification are linear bytes.
    let left_keys: Vec<_> = a.iter().map(|m| message_key(m)).collect();
    let right_keys: Vec<_> = b.iter().map(|m| message_key(m)).collect();
    let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if left_keys[i] == right_keys[j] {
                1 + lcs[i + 1][j + 1]
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        if left_keys[i] == right_keys[j] {
            if same_message(a[i], b[j]) {
                out.push(row("unchanged", Some(a[i]), Some(b[j])));
            } else {
                out.push(row("left_only", Some(a[i]), None));
                out.push(row("right_only", None, Some(b[j])));
            }
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            out.push(row("left_only", Some(a[i]), None));
            i += 1;
        } else {
            out.push(row("right_only", None, Some(b[j])));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|m| row("left_only", Some(m), None)));
    out.extend(b[j..].iter().map(|m| row("right_only", None, Some(m))));
    out
}

fn message_key(message: &MessageInfo) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut key = Sha256::new();
    for value in [
        Some(message.role.as_str()),
        Some(message.content.as_str()),
        message.meta.as_deref(),
    ] {
        key.update([u8::from(value.is_some())]);
        if let Some(value) = value {
            key.update((value.len() as u64).to_le_bytes());
            key.update(value.as_bytes());
        }
    }
    key.finalize().into()
}
