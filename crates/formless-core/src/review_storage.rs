//! Read-only recorded-Run ownership and bounded snapshot projection. Filesystem
//! paths in these rows confer no authority until the daemon's path gate passes.
use super::*;
use crate::hunk_review::MAX_REVIEW_BYTES;
const MAX_REVIEW_FILES: i64 = 128;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReviewFailure {
    #[error("Session or owned Run not found")]
    NotFound,
    #[error("{0}")]
    Invalid(String),
}
#[derive(Debug)]
pub(crate) struct RecordedReviewFile {
    pub path: String,
    pub existed: bool,
    pub before: Option<Vec<u8>>,
    pub before_metadata: Option<String>,
    pub exclusion_reason: Option<String>,
}
#[derive(Debug)]
pub(crate) struct RecordedRunReview {
    pub run_id: String,
    pub files: Vec<RecordedReviewFile>,
}
impl Store {
    #[cfg(windows)]
    pub(crate) fn review_retained_run_active(&self, run: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND status='running') OR EXISTS(SELECT 1 FROM invocations WHERE run_id=?1 AND status IN ('running','pending'))",
            params![run], |row| row.get(0),
        )?)
    }
    /// Only the immutable FIRST pre-change row may acquire retained-object
    /// authority. A later Tool callback must not repair/replace a missing token
    /// with the object's post-change identity or security metadata.
    #[cfg(windows)]
    pub(crate) fn review_snapshot_recorded(
        &self,
        session: &str,
        run: &str,
        path: &str,
    ) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM run_files WHERE run_id=?1 AND path=?2)",
            params![run, path],
            |row| row.get::<_, bool>(0),
        )? || !self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND session_id=?2)",
            params![run, session],
            |row| row.get::<_, bool>(0),
        )?)
    }
    pub(crate) fn review_session_busy(
        &self,
        session: &str,
        completed: &std::collections::BTreeSet<String>,
    ) -> Result<bool> {
        if self.conn.query_row("SELECT EXISTS(SELECT 1 FROM invocations WHERE session_id=?1 AND status IN ('running','pending'))",params![session],|r|r.get::<_,bool>(0))? {return Ok(true);}
        let mut query =
            self.ps("SELECT handle FROM pending_handles WHERE session_id=?1 AND status='pending'")?;
        let handles = query.query_map(params![session], |r| r.get::<_, String>(0))?;
        for handle in handles {
            if !completed.contains(&handle?) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// Profile-wide write exclusion. The caller holds the daemon Store mutex
    /// through the subsequent file commit, serializing Run/Invocation admission.
    pub(crate) fn review_profile_busy(
        &self,
        completed: &std::collections::BTreeSet<String>,
    ) -> Result<bool> {
        if self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE status='running') OR EXISTS(SELECT 1 FROM invocations WHERE status IN ('running','pending'))",
            [], |r| r.get::<_, bool>(0),
        )? {
            return Ok(true);
        }
        let mut query = self.ps("SELECT handle FROM pending_handles WHERE status='pending'")?;
        let handles = query.query_map([], |r| r.get::<_, String>(0))?;
        for handle in handles {
            if !completed.contains(&handle?) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(crate) fn review_tool_code(&self, id: &str, version: i64) -> Result<String> {
        let row:Option<(i64,Option<String>)>=self.conn.query_row("SELECT t.current_version,CASE WHEN length(CAST(v.code AS BLOB))<=?2 THEN v.code ELSE NULL END FROM tools t JOIN tool_versions v ON v.tool_id=t.id AND v.version=t.current_version WHERE t.id=?1 AND t.retired=0",params![id,MAX_REVIEW_BYTES],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let (current, code) = row.context(ReviewFailure::NotFound)?;
        if current != version {
            return Err(ReviewFailure::Invalid(
                "The Tool version changed; reopen the editor before reviewing".into(),
            )
            .into());
        }
        code.context(ReviewFailure::Invalid(
            "The Tool code exceeds the 256 KiB interactive review limit".into(),
        ))
    }
    pub(crate) fn recorded_run_review(
        &self,
        session: &str,
        requested: Option<&str>,
    ) -> Result<RecordedRunReview> {
        self.read_transaction(|st| {
            if st.get_session(session)?.is_none() || st.session_deleted(session)? {return Err(ReviewFailure::NotFound.into());}
            let run_id=match requested {Some(id)=>id.to_owned(),None=>st.last_run(session)?.map(|(id,_)|id).context(ReviewFailure::NotFound)?};
            let owner:Option<String>=st.conn.query_row("SELECT session_id FROM runs WHERE id=?1",params![run_id],|r|r.get(0)).optional()?;
            if owner.as_deref()!=Some(session) {return Err(ReviewFailure::NotFound.into());}
            let (count,foreign):(i64,i64)=st.conn.query_row("SELECT COUNT(*),COALESCE(SUM(session_id<>?2),0) FROM run_files WHERE run_id=?1",params![run_id,session],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if foreign>0 {return Err(ReviewFailure::Invalid("The Run contains file snapshots with mismatched Session ownership".into()).into());}
            if count>MAX_REVIEW_FILES {return Err(ReviewFailure::Invalid("This Run exceeds the 128-file interactive review limit; use the whole-file listing instead".into()).into());}
            let mut query=st.ps("SELECT path,existed,kept,COALESCE(length(before),0),CASE WHEN length(before)<=?2 THEN before ELSE NULL END,CASE WHEN length(before_metadata)<=131072 THEN before_metadata ELSE NULL END FROM run_files WHERE run_id=?1 ORDER BY rowid")?;
            let records=query.query_map(params![run_id,MAX_REVIEW_BYTES],|r|Ok((r.get::<_,String>(0)?,r.get::<_,bool>(1)?,r.get::<_,bool>(2)?,r.get::<_,u64>(3)?,r.get::<_,Option<Vec<u8>>>(4)?,r.get::<_,Option<String>>(5)?)))?;
            let mut files=Vec::new();
            let mut bytes=0usize;
            for record in records {
                let (path,existed,kept,size,mut before,before_metadata)=record?;
                let exclusion_reason=if !existed && before.is_some() {Some("Recorded existence contradicts the retained original bytes")}
                    else if !kept {Some("The pre-Run regular-file content was not kept")}
                    else if size>MAX_REVIEW_BYTES as u64 {Some("The pre-Run image exceeds the 256 KiB interactive review limit")}
                    else if existed && before.is_none() {Some("The pre-Run image is unavailable")}
                    else if bytes.saturating_add(before.as_ref().map_or(0,Vec::len))>2*1024*1024 {Some("The Run exceeds the 2 MiB retained pre-image review budget")}
                    else {None};
                if exclusion_reason.is_some(){before=None;}else{bytes+=before.as_ref().map_or(0,Vec::len);}
                files.push(RecordedReviewFile{path,existed,before,before_metadata,exclusion_reason:exclusion_reason.map(str::to_owned)});
            }
            Ok(RecordedRunReview{run_id,files})
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requested_run_must_belong_to_live_session_and_snapshot_owner_must_match() {
        let st = Store::open_in_memory().unwrap();
        let a = st.create_session(None).unwrap();
        let b = st.create_session(None).unwrap();
        let run = st.create_run(&a.id).unwrap();
        st.snapshot_run_file(&a.id, &run, "not-opened.txt", true, Some(b"before\r\n"))
            .unwrap();
        let review = st.recorded_run_review(&a.id, None).unwrap();
        assert_eq!(review.run_id, run);
        assert_eq!(review.files.len(), 1);
        assert_eq!(review.files[0].path, "not-opened.txt");
        assert!(review.files[0].existed);
        assert_eq!(review.files[0].before.as_deref(), Some(&b"before\r\n"[..]));
        assert!(review.files[0].exclusion_reason.is_none());
        assert!(matches!(
            st.recorded_run_review(&b.id, Some(&run))
                .unwrap_err()
                .downcast_ref::<ReviewFailure>(),
            Some(ReviewFailure::NotFound)
        ));
        st.ex(
            "UPDATE run_files SET session_id=?2 WHERE run_id=?1",
            params![run, b.id],
        )
        .unwrap();
        assert!(matches!(
            st.recorded_run_review(&a.id, Some(&run))
                .unwrap_err()
                .downcast_ref::<ReviewFailure>(),
            Some(ReviewFailure::Invalid(_))
        ));
        st.delete_session(&a.id).unwrap();
        assert!(st.recorded_run_review(&a.id, Some(&run)).is_err());
    }
    #[test]
    fn unavailable_and_oversized_preimages_are_listed_without_loading_body() {
        let st = Store::open_in_memory().unwrap();
        let session = st.create_session(None).unwrap();
        let run = st.create_run(&session.id).unwrap();
        st.snapshot_run_file(
            &session.id,
            &run,
            "large.bin",
            true,
            Some(&vec![b'x'; MAX_REVIEW_BYTES + 1]),
        )
        .unwrap();
        st.snapshot_run_file(&session.id, &run, "not-kept.bin", true, None)
            .unwrap();
        st.snapshot_run_file(&session.id, &run, "created.txt", false, None)
            .unwrap();
        let review = st.recorded_run_review(&session.id, Some(&run)).unwrap();
        assert_eq!(review.files.len(), 3);
        for file in &review.files[..2] {
            assert!(file.exclusion_reason.is_some());
            assert!(file.before.is_none());
        }
        assert!(!review.files[2].existed);
        assert!(review.files[2].exclusion_reason.is_none());
        assert_eq!(
            st.run_files(&run).unwrap()[0].3.as_ref().unwrap().len(),
            MAX_REVIEW_BYTES + 1,
            "read-only projection retains original snapshot"
        );
    }
    #[test]
    fn file_count_limit_is_explicit_without_partial_or_cross_session_listing() {
        let st = Store::open_in_memory().unwrap();
        let session = st.create_session(None).unwrap();
        let run = st.create_run(&session.id).unwrap();
        for i in 0..=MAX_REVIEW_FILES {
            st.snapshot_run_file(&session.id, &run, &format!("owned-{i}.txt"), false, None)
                .unwrap();
        }
        let error = st.recorded_run_review(&session.id, None).unwrap_err();
        assert!(
            matches!(error.downcast_ref::<ReviewFailure>(),Some(ReviewFailure::Invalid(message)) if message.contains("128-file"))
        );
        assert_eq!(
            st.run_files(&run).unwrap().len(),
            MAX_REVIEW_FILES as usize + 1
        );
    }
    #[test]
    fn implicit_review_uses_actual_last_run_not_an_older_run_with_files() {
        let st = Store::open_in_memory().unwrap();
        let session = st.create_session(None).unwrap();
        let previous = st.create_run(&session.id).unwrap();
        st.snapshot_run_file(&session.id, &previous, "previous.txt", true, Some(b"old"))
            .unwrap();
        st.set_run_status(&previous, "completed").unwrap();
        let latest = st.create_run(&session.id).unwrap();
        st.set_run_status(&latest, "completed").unwrap();
        let review = st.recorded_run_review(&session.id, None).unwrap();
        assert_eq!(review.run_id, latest);
        assert!(review.files.is_empty());
        assert_eq!(
            st.recorded_run_review(&session.id, Some(&previous))
                .unwrap()
                .files
                .len(),
            1
        );
    }
}

#[cfg(test)]
mod before_metadata_tests {
    use super::*;
    #[test]
    fn actual_schema62_snapshots_upgrade_to_unknown_metadata_and_first_capture_is_immutable() {
        let root =
            std::env::temp_dir().join(format!("fl-before-migration-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("formless.db");
        // A real ordered historical DDL prefix, never a current schema whose
        // version was lowered. Schema 62 genuinely has no metadata column.
        let old = schema_at(62).unwrap();
        old.pragma_update(None, "user_version", 62).unwrap();
        let count: i64 = old
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('run_files') WHERE name='before_metadata'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        old.execute("INSERT INTO run_files(run_id,session_id,path,existed,kept,before,created_at) VALUES('legacy','s','legacy.txt',1,1,?1,'2026-01-01')", [b"old bytes".as_slice()]).unwrap();
        old.execute("VACUUM INTO ?1", [file.to_str().unwrap()])
            .unwrap();
        drop(old);
        {
            let st = Store::open(&file).unwrap();
            assert_eq!(
                st.conn
                    .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                    .unwrap(),
                SCHEMA_VERSION
            );
            let row: (Vec<u8>, Option<String>) = st
                .conn
                .query_row(
                    "SELECT before,before_metadata FROM run_files WHERE run_id='legacy'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(row, (b"old bytes".to_vec(), None));
            st.snapshot_run_file_with_metadata(
                "s",
                "legacy",
                "legacy.txt",
                true,
                Some(b"wrong replacement"),
                Some("wrong metadata"),
            )
            .unwrap();
            let row: (Vec<u8>, Option<String>) = st
                .conn
                .query_row(
                    "SELECT before,before_metadata FROM run_files WHERE run_id='legacy'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(
                row,
                (b"old bytes".to_vec(), None),
                "legacy absence of metadata must not be backfilled after mutation"
            );
            st.snapshot_run_file_with_metadata(
                "s",
                "new",
                "new.txt",
                true,
                Some(b"first"),
                Some("first metadata"),
            )
            .unwrap();
            st.snapshot_run_file_with_metadata(
                "s",
                "new",
                "new.txt",
                false,
                None,
                Some("later metadata"),
            )
            .unwrap();
            let row: (bool, Vec<u8>, String) = st
                .conn
                .query_row(
                    "SELECT existed,before,before_metadata FROM run_files WHERE run_id='new'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!(row, (true, b"first".to_vec(), "first metadata".into()));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
