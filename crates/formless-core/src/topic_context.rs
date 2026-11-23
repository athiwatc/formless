use super::*;
use crate::main_topic_suggestions::READ_TOPIC_CONTEXT;
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
        let visible = self.topic_context_scope(id)?;
        let mut messages = self.list_messages(session)?;
        for message in &mut messages {
            message.content = scope_topic_context_result(
                &message.role,
                &message.content,
                message.meta.as_deref(),
                visible.as_ref(),
            );
        }
        Ok(
            json!({"session":self.get_session(session)?.context("conversation not found")?,"messages":messages}),
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
        let visible = self.topic_context_scope(id)?;
        let scope = serde_json::to_string(&self.topic_context_sessions(id)?)?;
        let mut statement=self.conn.prepare("SELECT session_id,id,role,meta,content FROM messages WHERE session_id IN (SELECT value FROM json_each(?1)) ORDER BY created_at DESC,rowid DESC")?;
        let mut rows = statement.query([scope])?;
        let mut matches = vec![];
        while let Some(row) = rows.next()? {
            let role: String = row.get(2)?;
            let meta: Option<String> = row.get(3)?;
            let raw_content: String = row.get(4)?;
            let content =
                scope_topic_context_result(&role, &raw_content, meta.as_deref(), visible.as_ref());
            let folded = content.to_lowercase();
            if let Some(offset) = folded.find(&query) {
                let mut folded_bytes = 0;
                let original_offset = content
                    .chars()
                    .position(|character| {
                        folded_bytes += character.to_lowercase().map(char::len_utf8).sum::<usize>();
                        offset < folded_bytes
                    })
                    .unwrap_or_default();
                let start = original_offset.saturating_sub(60);
                let snippet = content.chars().skip(start).take(240).collect::<String>();
                matches.push(json!({"session_id":row.get::<_,String>(0)?,"message_id":row.get::<_,String>(1)?,"snippet":snippet}));
                if matches.len() == 40 {
                    break;
                }
            }
        }
        Ok(json!(matches))
    }

    fn topic_context_scope(&self, id: &str) -> Result<Option<std::collections::HashSet<String>>> {
        let mut statement = self
            .conn
            .prepare("SELECT id,parent_topic_id,allow_external_context FROM topics")?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut parents = std::collections::HashMap::<String, Option<String>>::new();
        let mut children = std::collections::HashMap::<String, Vec<String>>::new();
        let mut external = std::collections::HashSet::new();
        for (topic_id, parent_id, allow_external_context) in rows {
            if allow_external_context {
                external.insert(topic_id.clone());
            }
            if let Some(parent_id) = &parent_id {
                children
                    .entry(parent_id.clone())
                    .or_default()
                    .push(topic_id.clone());
            }
            parents.insert(topic_id, parent_id);
        }
        let Some(parent) = parents.get(id) else {
            bail!("source topic not found");
        };
        if external.contains(id) {
            return Ok(None);
        }

        let mut visible = std::collections::HashSet::new();
        let mut pending = vec![id.to_owned()];
        while let Some(topic_id) = pending.pop() {
            if visible.insert(topic_id.clone()) {
                if let Some(descendants) = children.get(&topic_id) {
                    pending.extend(descendants.iter().cloned());
                }
            }
        }
        let mut ancestor = parent.as_deref();
        while let Some(topic_id) = ancestor {
            if !visible.insert(topic_id.to_owned()) {
                break;
            }
            ancestor = parents.get(topic_id).and_then(|parent| parent.as_deref());
        }
        Ok(Some(visible))
    }
}

fn scope_topic_context_result(
    role: &str,
    content: &str,
    meta: Option<&str>,
    visible: Option<&std::collections::HashSet<String>>,
) -> String {
    let Some(visible) = visible else {
        return content.to_owned();
    };
    let Some(metadata) = meta.and_then(|meta| serde_json::from_str::<Value>(meta).ok()) else {
        return content.to_owned();
    };
    if role != "tool" || metadata["name"].as_str() != Some(READ_TOPIC_CONTEXT) {
        return content.to_owned();
    }
    let mut frame = serde_json::from_str::<Value>(content).ok();
    let framed = frame.as_ref().is_some_and(|frame| {
        frame["formless_data"] == 1
            && matches!(
                frame["source_tool"].as_str(),
                Some(READ_TOPIC_CONTEXT) | Some("unknown")
            )
            && frame["content"].is_string()
    });
    let mut context = if framed {
        frame
            .as_ref()
            .and_then(|frame| frame["content"].as_str())
            .and_then(|content| serde_json::from_str::<Value>(content).ok())
    } else {
        serde_json::from_str::<Value>(content).ok()
    };
    let Some(context) = context.as_mut() else {
        return content.to_owned();
    };
    if !context["topics"].is_array() && !context["proposals"].is_array() {
        return content.to_owned();
    }
    for collection in ["topics", "proposals"] {
        if let Some(entries) = context[collection].as_array_mut() {
            entries.retain(|entry| entry["id"].as_str().is_some_and(|id| visible.contains(id)));
        }
    }
    let context = serde_json::to_string(context).unwrap_or_else(|_| "{}".into());
    if framed {
        let Some(frame) = frame.as_mut() else {
            return content.to_owned();
        };
        frame["content"] = Value::String(context);
        serde_json::to_string(frame).unwrap_or_else(|_| content.to_owned())
    } else {
        context
    }
}
