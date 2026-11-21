use super::*;
use anyhow::bail;

impl Store {
    pub(crate) fn set_topic_context_access(&self, id: &str, allow: bool) -> Result<TopicInfo> {
        if self.conn.execute(
            "UPDATE topics SET allow_external_context=?2,updated_at=?3 WHERE id=?1",
            params![id, allow, now()],
        )? == 0
        {
            bail!("topic not found");
        }
        self.get_topic(id)?.context("topic not found")
    }
    pub(crate) fn topic_context_sessions(&self, id: &str) -> Result<Vec<String>> {
        let external: bool = self.conn.query_row(
            "SELECT allow_external_context FROM topics WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        if external {
            return Ok(self.list_sessions()?.into_iter().map(|s| s.id).collect());
        }
        Ok(self.conn.prepare("WITH RECURSIVE branch(id) AS (
            SELECT id FROM topics WHERE id=?1 UNION SELECT t.parent_topic_id FROM topics t JOIN branch b ON b.id=t.id WHERE t.parent_topic_id IS NOT NULL),
            contexts(id) AS (SELECT t.topic_session_id FROM topics t JOIN branch b ON b.id=t.id UNION
              SELECT a.conversation_session_id FROM topic_attempts a JOIN branch b ON b.id=a.topic_id)
            SELECT DISTINCT s.id FROM sessions s JOIN contexts c ON c.id=s.id WHERE s.deleted_at IS NULL")?
            .query_map([id],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?)
    }
    pub(crate) fn topic_list_conversations(&self, id: &str) -> Result<Value> {
        Ok(serde_json::to_value(
            self.topic_context_sessions(id)?
                .iter()
                .map(|s| self.get_session(s)?.context("conversation not found"))
                .collect::<Result<Vec<_>>>()?,
        )?)
    }
    pub(crate) fn topic_read_conversation(&self, id: &str, session: &str) -> Result<Value> {
        if !self
            .topic_context_sessions(id)?
            .iter()
            .any(|s| s == session)
        {
            bail!("conversation is outside this topic's context scope");
        }
        Ok(
            json!({"session":self.get_session(session)?.context("conversation not found")?,"messages":self.list_messages(session)?}),
        )
    }
    pub(crate) fn topic_search_conversations(&self, id: &str, query: &str) -> Result<Value> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Ok(json!([]));
        }
        if query.len() > 1000 {
            bail!("search query exceeds 1000 bytes");
        }
        let scope = serde_json::to_string(&self.topic_context_sessions(id)?)?;
        let mut statement=self.conn.prepare("SELECT session_id,id,content FROM messages WHERE session_id IN (SELECT value FROM json_each(?1)) ORDER BY created_at DESC,rowid DESC")?;
        let mut rows = statement.query([scope])?;
        let mut matches = vec![];
        while let Some(row) = rows.next()? {
            let content: String = row.get(2)?;
            let folded = content.to_lowercase();
            if let Some(offset) = folded.find(&query) {
                let start = folded[..offset].chars().count().saturating_sub(60);
                let snippet = content.chars().skip(start).take(240).collect::<String>();
                matches.push(json!({"session_id":row.get::<_,String>(0)?,"message_id":row.get::<_,String>(1)?,"snippet":snippet}));
                if matches.len() == 40 {
                    break;
                }
            }
        }
        Ok(json!(matches))
    }
}
