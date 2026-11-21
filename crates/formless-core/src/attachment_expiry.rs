use super::Store;
use aho_corasick::{AhoCorasick, AhoCorasickBuilder};
use anyhow::Result;
use rusqlite::{params, types::ValueRef};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

const PAGE_SIZE: usize = 128;

struct IdPosition {
    pattern: usize,
    first: usize,
    last: usize,
}

struct ExpiryMatcher<'a> {
    search: AhoCorasick,
    positions: HashMap<&'a str, IdPosition>,
}

impl<'a> ExpiryMatcher<'a> {
    fn new(ids: &'a [String], fold_ascii: bool) -> Option<Self> {
        let mut patterns = HashMap::new();
        let mut positions: HashMap<&str, IdPosition> = HashMap::new();
        let mut needles = Vec::new();
        for (index, id) in ids.iter().enumerate() {
            let position = positions.entry(id).or_insert_with(|| {
                let needle = if fold_ascii {
                    id.to_ascii_lowercase()
                } else {
                    id.clone()
                };
                let pattern = *patterns.entry(needle.clone()).or_insert_with(|| {
                    needles.push(needle);
                    needles.len() - 1
                });
                IdPosition {
                    pattern,
                    first: index,
                    last: index,
                }
            });
            position.last = index;
        }
        let search = AhoCorasickBuilder::new()
            .ascii_case_insensitive(fold_ascii)
            .build(needles)
            .ok()?;
        Some(Self { search, positions })
    }

    fn mark(&self, meta: &[u8]) -> Result<Option<(String, usize)>> {
        // SQLite LIKE ignores the part of a string after its first NUL.
        let candidate = meta.split(|byte| *byte == 0).next().unwrap_or_default();
        let patterns: HashSet<_> = self
            .search
            .find_overlapping_iter(candidate)
            .map(|hit| hit.pattern().as_usize())
            .collect();
        if patterns.is_empty() {
            return Ok(None);
        }
        let Ok(mut value) = serde_json::from_str::<Value>(std::str::from_utf8(meta)?) else {
            return Ok(None);
        };
        let Some(attachments) = value.get_mut("attachments").and_then(Value::as_array_mut) else {
            return Ok(None);
        };
        // Once the first successful old-style update serializes the JSON,
        // later ASCII artifact IDs are visible to LIKE even if input JSON
        // spelled them with escapes. Earlier misses can recur as duplicate IDs.
        let first_write = attachments
            .iter()
            .filter(|attachment| attachment["expired"] != true)
            .filter_map(|attachment| self.positions.get(attachment["id"].as_str()?))
            .filter(|position| patterns.contains(&position.pattern))
            .map(|position| position.first)
            .min();
        let Some(first_write) = first_write else {
            return Ok(None);
        };
        let mut changed = HashSet::new();
        for attachment in attachments {
            if attachment["expired"] == true {
                continue;
            }
            let Some(position) = attachment["id"]
                .as_str()
                .and_then(|id| self.positions.get(id))
            else {
                continue;
            };
            if position.last >= first_write {
                attachment["expired"] = json!(true);
                changed.insert(position.first);
            }
        }
        Ok(Some((value.to_string(), changed.len())))
    }
}

impl Store {
    pub fn mark_attachments_expired(&self, ids: &[String]) -> Result<usize> {
        let can_batch = ids.len() > 1
            && ids.iter().all(|id| {
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            });
        let matcher = if can_batch {
            let fold_ascii = self
                .conn
                .query_row("SELECT 'a' LIKE 'A'", [], |row| row.get(0))?;
            ExpiryMatcher::new(ids, fold_ascii)
        } else {
            None
        };
        let changed = match matcher {
            Some(matcher) => self.mark_attachment_pages(&matcher)?,
            None => self.mark_attachment_ids(ids)?,
        };
        if changed > 0 {
            self.note_rewrite();
        }
        Ok(changed)
    }

    fn mark_attachment_pages(&self, matcher: &ExpiryMatcher<'_>) -> Result<usize> {
        let mut changed = 0;
        let mut first = i64::MIN;
        let mut select = self.ps(
            "SELECT rowid,id,meta FROM messages WHERE rowid >= ?1 AND role='user' AND meta IS NOT NULL ORDER BY rowid LIMIT ?2",
        )?;
        loop {
            let mut updates = Vec::new();
            let mut last = None;
            {
                let mut rows = select.query(params![first, PAGE_SIZE])?;
                while let Some(row) = rows.next()? {
                    last = Some(row.get::<_, i64>(0)?);
                    if let ValueRef::Text(raw) = row.get_ref(2)? {
                        if let Some((meta, count)) = matcher.mark(raw)? {
                            updates.push((row.get::<_, String>(1)?, meta, count));
                        }
                    }
                }
            }
            for (id, meta, count) in updates {
                self.ex("UPDATE messages SET meta=?1 WHERE id=?2", params![meta, id])?;
                changed += count;
            }
            let Some(next) = last.and_then(|last| last.checked_add(1)) else {
                return Ok(changed);
            };
            first = next;
        }
    }

    // Keep SQLite's pattern semantics for unusual imported IDs and the
    // single-ID fast path. The batched matcher handles ordinary artifact IDs.
    fn mark_attachment_ids(&self, ids: &[String]) -> Result<usize> {
        let mut changed = 0;
        for id in ids {
            let rows: Vec<(String, String)> = {
                let mut stmt =
                    self.ps("SELECT id, meta FROM messages WHERE role='user' AND meta LIKE ?1")?;
                let rows = stmt
                    .query_map(params![format!("%{id}%")], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                rows
            };
            for (msg_id, meta) in rows {
                let Ok(mut v) = serde_json::from_str::<Value>(&meta) else {
                    continue;
                };
                let mut hit = false;
                for a in v
                    .get_mut("attachments")
                    .and_then(Value::as_array_mut)
                    .into_iter()
                    .flatten()
                {
                    if a["id"].as_str() == Some(id.as_str()) && a["expired"] != true {
                        a["expired"] = json!(true);
                        hit = true;
                    }
                }
                if hit {
                    self.ex(
                        "UPDATE messages SET meta=?1 WHERE id=?2",
                        params![v.to_string(), msg_id],
                    )?;
                    changed += 1;
                }
            }
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_rows(store: &Store, session: &str) -> Vec<Option<String>> {
        store
            .list_messages(session)
            .unwrap()
            .into_iter()
            .map(|msg| msg.meta)
            .collect()
    }

    #[test]
    fn batched_expiry_matches_ordered_legacy_count_and_metadata() {
        let store = Store::open_in_memory().unwrap();
        let session = store.create_session(None).unwrap().id;
        let rows = [
            (
                "user",
                r#"{"attachments":[{"id":"first"},{"id":"first"},{"id":"second"},{"id":"expired","expired":true}],"other":"keep"}"#,
            ),
            (
                "user",
                r#"{"attachments":[{"id":"\u006cater"},{"id":"starter"}]}"#,
            ),
            (
                "user",
                r#"{"attachments":[{"id":"\u0065arly"},{"id":"starter"}]}"#,
            ),
            (
                "user",
                r#"{"attachments":[{"id":"\u0043ase"}],"name":"case"}"#,
            ),
            (
                "user",
                r#"{"attachments":[{"id":"another"}],"name":"first second"}"#,
            ),
            ("user", r#"{"attachments":{"id":"first"}}"#),
            ("user", r#"["first"]"#),
            ("user", r#"{"attachments":[{"id":"first"}"#),
            ("assistant", r#"{"attachments":[{"id":"first"}]}"#),
        ];
        for (role, metadata) in rows {
            store
                .append_message_meta(&session, role, "", Some(metadata.to_owned()))
                .unwrap();
        }
        let old = store.preview_copy().unwrap();
        let ids = [
            "early", "later", "first", "starter", "second", "later", "expired", "Case",
        ]
        .map(str::to_owned);
        assert_eq!(old.mark_attachment_ids(&ids).unwrap(), 6);
        let revision = store.rewrites.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(store.mark_attachments_expired(&ids).unwrap(), 6);
        assert_eq!(raw_rows(&store, &session), raw_rows(&old, &session));
        assert_eq!(
            store.rewrites.load(std::sync::atomic::Ordering::SeqCst),
            revision + 1
        );
        // The first pass normalized the earlier escaped ID after its turn.
        assert_eq!(old.mark_attachment_ids(&ids).unwrap(), 1);
        assert_eq!(store.mark_attachments_expired(&ids).unwrap(), 1);
        assert_eq!(raw_rows(&store, &session), raw_rows(&old, &session));
        assert_eq!(store.mark_attachments_expired(&ids).unwrap(), 0);
        assert_eq!(
            store.rewrites.load(std::sync::atomic::Ordering::SeqCst),
            revision + 2
        );
    }

    #[test]
    fn attachment_expiry_keeps_sqlite_wildcard_matching_for_unusual_ids() {
        let store = Store::open_in_memory().unwrap();
        let session = store.create_session(None).unwrap().id;
        store.append_message_meta(&session, "user", "", Some(
            r#"{"attachments":[{"id":"wild\u005fid"},{"id":"plain"},{"id":"wildXid"}],"other":"wildXid"}"#.into()
        )).unwrap();
        let old = store.preview_copy().unwrap();
        let ids = ["wild_id", "plain"].map(str::to_owned);
        assert_eq!(old.mark_attachment_ids(&ids).unwrap(), 2);
        assert_eq!(store.mark_attachments_expired(&ids).unwrap(), 2);
        assert_eq!(raw_rows(&store, &session), raw_rows(&old, &session));
    }

    #[test]
    fn attachment_expiry_updates_overlapping_rows_once_in_bounded_pages() {
        use rusqlite::ffi;
        use std::{
            cell::Cell,
            ffi::{c_int, c_uint, c_void, CStr},
        };
        let store = Store::open_in_memory().unwrap();
        let session = store.create_session(None).unwrap().id;
        let messages = PAGE_SIZE * 2 + 7;
        let ids: Vec<_> = (0..24).map(|i| format!("artifact-{i:04}")).collect();
        let attachments: Vec<_> = ids.iter().map(|id| json!({"id": id})).collect();
        let metadata = json!({"attachments": attachments, "keep":"description"}).to_string();
        for _ in 0..messages {
            store
                .append_message_meta(&session, "user", "", Some(metadata.clone()))
                .unwrap();
        }
        let old = store.preview_copy().unwrap();

        #[derive(Default)]
        struct Operations {
            reads: Cell<usize>,
            writes: Cell<usize>,
        }
        unsafe extern "C" fn count_statements(
            _: c_uint,
            data: *mut c_void,
            _: *mut c_void,
            sql: *mut c_void,
        ) -> c_int {
            // SAFETY: SQLite supplies the SQL as a C string. The caller
            // holds this Operations value until after the trace is removed.
            let sql = unsafe { CStr::from_ptr(sql.cast()) }.to_bytes();
            let counts = unsafe { &*data.cast::<Operations>() };
            if sql.starts_with(b"SELECT id, meta FROM messages")
                || sql.starts_with(b"SELECT rowid,id,meta FROM messages")
            {
                counts.reads.set(counts.reads.get() + 1);
            }
            if sql.starts_with(b"UPDATE messages SET meta=") {
                counts.writes.set(counts.writes.get() + 1);
            }
            0
        }

        let measure = |store: &Store, batch: bool| {
            let counts = Operations::default();
            // SAFETY: the connection and callback data live throughout
            // SQLite's synchronous trace and we unregister before returning.
            let handle = unsafe { store.conn.handle() };
            assert_eq!(
                unsafe {
                    ffi::sqlite3_trace_v2(
                        handle,
                        ffi::SQLITE_TRACE_STMT as c_uint,
                        Some(count_statements),
                        (&counts as *const Operations).cast_mut().cast(),
                    )
                },
                ffi::SQLITE_OK
            );
            let start = std::time::Instant::now();
            let result = store.transaction(|st| {
                if batch {
                    st.mark_attachments_expired(&ids)
                } else {
                    st.mark_attachment_ids(&ids)
                }
            });
            let elapsed = start.elapsed();
            unsafe { ffi::sqlite3_trace_v2(handle, 0, None, std::ptr::null_mut()) };
            (
                result.unwrap(),
                counts.reads.get(),
                counts.writes.get(),
                elapsed,
            )
        };

        let previous = measure(&old, false);
        let current = measure(&store, true);
        assert_eq!(previous.0, messages * ids.len());
        assert_eq!(current.0, previous.0);
        assert_eq!(raw_rows(&store, &session), raw_rows(&old, &session));
        assert_eq!((previous.1, previous.2), (ids.len(), messages * ids.len()));
        assert_eq!((current.1, current.2), (4, messages));
        eprintln!("previous {previous:?}, batched {current:?} (count, reads, updates, elapsed)");
    }
}
