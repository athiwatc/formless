//! Attempt presentation is durable data, never promotion approval authority.
//! Approval lives only in a typed, one-use daemon promotion ticket.
use super::*;

impl Store {
    pub(crate) fn set_attempt_holdout_detail(&self, id: &str, holdout: &Value) -> Result<()> {
        let (detail, _, _, _, _) = self.attempt_scores(id)?;
        let mut detail: Value = serde_json::from_str(&self.attempt_schema_detail(id, &detail)?)?;
        detail["holdout"] = holdout.clone();
        self.ex(
            "UPDATE optimiser_attempts SET detail=?2 WHERE id=?1",
            params![id, serde_json::to_string_pretty(&detail)?],
        )?;
        Ok(())
    }
    pub(crate) fn attempt_schema_report(
        &self,
        id: &str,
    ) -> Result<crate::tool_compatibility::ToolSchemaReport> {
        let attempt = self
            .get_attempt(id)?
            .context("Optimiser attempt no longer exists")?;
        self.schema_change_report_from_version(
            &attempt.tool_id,
            attempt.base_version,
            &attempt.candidate.input_schema,
            &attempt.candidate.output_schema,
        )
    }

    pub(super) fn attempt_schema_detail(&self, id: &str, summary: &str) -> Result<String> {
        self.attempt_schema_detail_with_validation(id, summary, None)
    }
    pub(crate) fn mark_attempt_validation_context(&self, id: &str) -> Result<()> {
        let (summary, _, _, _, _) = self.attempt_scores(id)?;
        let detail = self.attempt_schema_detail_with_validation(id, &summary, Some(true))?;
        self.ex(
            "UPDATE optimiser_attempts SET detail=?2 WHERE id=?1",
            params![id, detail],
        )?;
        Ok(())
    }
    pub(crate) fn set_attempt_generation_validation_context(
        &self,
        id: &str,
        validation: bool,
    ) -> Result<()> {
        let (summary, _, _, _, _) = self.attempt_scores(id)?;
        let detail = self.attempt_schema_detail_with_validation(id, &summary, Some(validation))?;
        self.ex(
            "UPDATE optimiser_attempts SET detail=?2 WHERE id=?1",
            params![id, detail],
        )?;
        Ok(())
    }
    fn attempt_schema_detail_with_validation(
        &self,
        id: &str,
        summary: &str,
        generated_for_validation: Option<bool>,
    ) -> Result<String> {
        let previous: Option<String> = self
            .conn
            .query_row(
                "SELECT detail FROM optimiser_attempts WHERE id=?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        let previous: Value = previous
            .and_then(|detail| serde_json::from_str(&detail).ok())
            .unwrap_or(Value::Null);
        let attempt = self
            .get_attempt(id)?
            .context("Optimiser attempt no longer exists")?;
        let generation_validation = match self.attempt_replay_snapshot(id)? {
            Some(snapshot) => crate::optimiser::generation_context(&snapshot)?
                .iter()
                .any(|item| !item.recorded_success && item.tag == "validation"),
            None => self
                .replay_set(&attempt.tool_id)?
                .iter()
                .any(|item| !item.recorded_success && item.tag == "validation"),
        };
        let validation_context = generated_for_validation.unwrap_or_else(|| {
            previous
                .get("validation_fix_context")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || generation_validation
        });
        let (report, warning) = match self.attempt_schema_report(id) {
            Ok(report) => {
                let warning = validation_context && report.compatibility.is_breaking();
                (serde_json::to_value(report)?, warning)
            }
            Err(error) => (
                json!({"compatibility":{"verdict":"inconclusive"},"error":error.to_string()}),
                false,
            ),
        };
        // A caller retaining a previous detail keeps the human summary rather
        // than recursively nesting earlier compatibility envelopes.
        let parsed: Value = serde_json::from_str(summary).unwrap_or(Value::Null);
        let summary = if parsed.get("format").and_then(Value::as_str)
            == Some("optimiser-attempt-schema-v1")
        {
            parsed
                .get("summary")
                .and_then(Value::as_str)
                .unwrap_or(summary)
        } else {
            summary
        };
        Ok(serde_json::to_string_pretty(&json!({
            "format":"optimiser-attempt-schema-v1", "summary":summary,
            "schema_compatibility":report, "validation_fix_context":validation_context,
            "holdout": previous.get("holdout").cloned().unwrap_or(Value::Null),
            "validation_schema_warning":if warning { Some("Candidate generated to fix validation-tag failures breaks the schema contract; passing recorded replay does not establish dependent compatibility.") } else { None },
        }))?)
    }

    pub(crate) fn attempt_scores(
        &self,
        id: &str,
    ) -> Result<(String, Option<f64>, Option<f64>, i64, i64)> {
        Ok(self.conn.query_row("SELECT detail,candidate_score,baseline_score,replay_total,regressions FROM optimiser_attempts WHERE id=?1", params![id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?)
    }
}
