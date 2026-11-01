//! Transaction-local schema impact and live/current dependency inventory.
use super::*;
use crate::tool_compatibility::{
    self as compatibility, SchemaDependent, ToolSchemaReport, MAX_DEPENDENCY_TEXT, MAX_DEPENDENTS,
};
impl Store {
    pub fn schema_change_report(
        &self,
        tool_id: &str,
        new_input: &str,
        new_output: &str,
    ) -> Result<ToolSchemaReport> {
        let (info, _) = self
            .get_tool_current(tool_id)?
            .context("Tool not found for compatibility report")?;
        self.schema_change_report_from_version(tool_id, info.current_version, new_input, new_output)
    }
    pub fn schema_change_report_from_version(
        &self,
        tool_id: &str,
        base_version: i64,
        new_input: &str,
        new_output: &str,
    ) -> Result<ToolSchemaReport> {
        let name: String = self.conn.query_row(
            "SELECT name FROM tools WHERE id=?1",
            params![tool_id],
            |row| row.get(0),
        )?;
        let (old_input, old_output): (String, String) = self.conn.query_row(
            "SELECT input_schema,output_schema FROM tool_versions WHERE tool_id=?1 AND version=?2",
            params![tool_id, base_version],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let mut report = compatibility::report(
            tool_id,
            base_version,
            &old_input,
            &old_output,
            new_input,
            new_output,
        );
        self.schema_dependents(&name, tool_id, &mut report)?;
        Ok(report)
    }
    pub fn tool_version_schema_report(
        &self,
        tool_id: &str,
        version: i64,
    ) -> Result<ToolSchemaReport> {
        let (new_input, new_output): (String, String) = self.conn.query_row(
            "SELECT input_schema,output_schema FROM tool_versions WHERE tool_id=?1 AND version=?2",
            params![tool_id, version],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let old: Option<i64> = self.conn.query_row(
            "SELECT MAX(version) FROM tool_versions WHERE tool_id=?1 AND version<?2",
            params![tool_id, version],
            |row| row.get(0),
        )?;
        match old {
            Some(old) => {
                self.schema_change_report_from_version(tool_id, old, &new_input, &new_output)
            }
            None => Ok(compatibility::report(
                tool_id,
                0,
                &new_input,
                &new_output,
                &new_input,
                &new_output,
            )),
        }
    }
    pub(crate) fn schema_report_for_name(
        &self,
        name: &str,
        new_input: &str,
        new_output: &str,
    ) -> Result<ToolSchemaReport> {
        match self.get_tool_by_name(name)? {
            Some((tool, _)) => self.schema_change_report(&tool.id, new_input, new_output),
            None => Ok(compatibility::report(
                "", 0, new_input, new_output, new_input, new_output,
            )),
        }
    }
    fn schema_dependents(
        &self,
        name: &str,
        target_id: &str,
        report: &mut ToolSchemaReport,
    ) -> Result<()> {
        let limit = MAX_DEPENDENTS as i64 + 1;
        let mut query = self.ps("SELECT t.id,substr(t.name,1,512),t.current_version,t.retired,t.disabled,CASE WHEN length(CAST(v.code AS BLOB))<=?2 THEN v.code END FROM tools t JOIN tool_versions v ON v.tool_id=t.id AND v.version=t.current_version WHERE t.id<>?1 ORDER BY t.id LIMIT ?3")?;
        let rows = query.query_map(params![target_id, MAX_DEPENDENCY_TEXT as i64, limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, bool>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        for (index, row) in rows.enumerate() {
            if index >= MAX_DEPENDENTS {
                report.inventory_truncated = true;
                break;
            }
            let (id, label, version, retired, disabled, code) = row?;
            let certainty = match code {
                Some(code) => compatibility::code_reference(&code, name),
                None => {
                    report.inventory_truncated = true;
                    Some("possible_dynamic")
                }
            };
            if let Some(certainty) = certainty {
                report.push(SchemaDependent {kind:"tool".into(),id,name:label,version:Some(version),live:!retired&&!disabled,certainty:certainty.into(),resolution:"nested_calls_resolve_current".into(),reason:"Arbitrary caller JavaScript may resolve Tools indirectly; its own version does not pin nested calls".into()});
            }
        }
        // Execution snapshots retain old caller code after an update/retirement;
        // their future nested callees still resolve the CURRENT target version.
        let mut query=self.ps("SELECT i.id,substr(t.name,1,512),i.version,CASE WHEN length(CAST(v.code AS BLOB))<=?1 THEN v.code END FROM invocations i JOIN tools t ON t.id=i.tool_id JOIN tool_versions v ON v.tool_id=i.tool_id AND v.version=i.version WHERE i.status IN ('running','pending') ORDER BY i.id LIMIT ?2")?;
        let rows = query.query_map(params![MAX_DEPENDENCY_TEXT as i64, limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?;
        for (index, row) in rows.enumerate() {
            if index >= MAX_DEPENDENTS {
                report.inventory_truncated = true;
                break;
            }
            let (id, label, version, code) = row?;
            let certainty = match code {
                Some(code) => compatibility::code_reference(&code, name),
                None => {
                    report.inventory_truncated = true;
                    Some("possible_dynamic")
                }
            };
            if let Some(certainty) = certainty {
                report.push(SchemaDependent{kind:"active_invocation".into(),id,name:label,version:Some(version),live:true,certainty:certainty.into(),resolution:"historical_caller_nested_calls_current".into(),reason:"This invocation retains arbitrary historical caller code; possible nested Tool calls still resolve the current callee".into()});
            }
        }
        let mut query=self.ps("SELECT id,session_id,substr(saved_tool,1,512),status,CASE WHEN length(CAST(uses AS BLOB))<=65536 THEN uses END FROM scripts ORDER BY created_at DESC,id LIMIT ?1")?;
        let rows = query.query_map(params![limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        for (index, row) in rows.enumerate() {
            if index >= MAX_DEPENDENTS {
                report.inventory_truncated = true;
                break;
            }
            let (id, _session, saved, status, uses) = row?;
            let uses = uses.and_then(|uses| serde_json::from_str::<Vec<String>>(&uses).ok());
            let relevant = uses
                .as_ref()
                .map(|uses| uses.iter().any(|used| used.eq_ignore_ascii_case(name)));
            if relevant == Some(false) {
                continue;
            }
            if relevant.is_none() {
                report.inventory_truncated = true;
            }
            let running = status == "running";
            let reusable = status == "success" && saved.is_empty();
            report.push(SchemaDependent {kind:"script".into(),id,name:if saved.is_empty(){"Stored orchestration script".into()}else{format!("Saved as Tool {saved}")},version:None,live:running||reusable,certainty:if running{"declared"}else if reusable{"possible_dynamic"}else{"historical_reference"}.into(),resolution:if running{"uses_resolve_current"}else if reusable{"stored_source_eligible_for_reuse_or_promotion"}else{"immutable_execution_history"}.into(),reason:if running{"Running script's declared Tools are resolved at each call"}else if reusable{"Successful unsaved script sources can be reused by recurring-script promotion; historical results themselves do not change"}else{"Execution is complete; a saved reusable Tool is inventoried separately"}.into()});
        }
        let mut query=self.ps("SELECT st.session_id,substr(s.name,1,512),st.pinned_version FROM session_tools st JOIN sessions s ON s.id=st.session_id WHERE st.tool_id=?1 AND st.pinned_version IS NOT NULL AND st.source<>'unloaded' AND s.deleted_at IS NULL ORDER BY st.session_id LIMIT ?2")?;
        let rows = query.query_map(params![target_id, limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for (index, row) in rows.enumerate() {
            if index >= MAX_DEPENDENTS {
                report.inventory_truncated = true;
                break;
            }
            let (id, label, version) = row?;
            report.push(SchemaDependent{kind:"session_definition".into(),id,name:label.unwrap_or_else(||"Session".into()),version:Some(version),live:true,certainty:"declared".into(),resolution:"definition_only_pin_calls_current".into(),reason:"A pinned prompt definition is not an execution-version pin; actual calls resolve the current Tool".into()});
        }
        let mut query=self.ps("SELECT j.id FROM jobs j WHERE j.enabled=1 AND j.approved=1 AND (j.next_run IS NOT NULL OR EXISTS(SELECT 1 FROM job_runs r WHERE r.job_id=j.id AND r.status IN ('queued','running'))) ORDER BY j.id LIMIT ?1")?;
        let rows = query.query_map(params![limit], |r| r.get::<_, String>(0))?;
        for (index, row) in rows.enumerate() {
            if index >= MAX_DEPENDENTS {
                report.inventory_truncated = true;
                break;
            }
            let id = row?;
            report.push(SchemaDependent{kind:"scheduled_job".into(),name:format!("Scheduled job {id}"),id,version:None,live:true,certainty:"possible_dynamic".into(),resolution:"future_prompt_has_no_authoritative_tool_binding".into(),reason:"Enabled approved job prompts can select current Tools dynamically; this is a possible consumer, not a proven call edge".into()});
        }
        report.sort();
        Ok(())
    }
}

#[cfg(test)]
#[path = "tool_compatibility_store_tests.rs"]
mod tests;
