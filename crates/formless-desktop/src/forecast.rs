//! Presentation and validation of the daemon's prospective Run quote.
//! This module does not estimate costs or grant consent itself.

use formless_protocol::{ForecastInput, ForecastRange, RunForecast};

use crate::model::{send_mode_label, Intent};

/// Native confirmation renders complete text rather than an abbreviated draft.
/// Larger drafts remain editable but use the TUI until a bounded full-text review exists.
pub const MAX_REVIEW_DRAFT_BYTES: usize = 64 * 1024;
#[derive(Debug, Clone)]
pub struct Prepared {
    pub client: uuid::Uuid,
    pub connection: u64,
    pub attachments: Option<crate::pending_files::Report>,
    pub id: uuid::Uuid,
    pub epoch: u64,
    pub generation: u64,
    pub session_id: String,
    pub label: &'static str,
    /// The view rechecks the exact draft, including for Resume, before commit.
    pub draft_basis: String,
    pub quote: RunForecast,
}

impl Prepared {
    pub fn needs_review(&self) -> bool {
        self.attachments.is_some() || self.quote.confirmation_required
    }
    pub fn summary(&self) -> Vec<String> {
        let mut lines = summary(&self.quote);
        if let Some(report) = &self.attachments {
            if self.label == "Steer" {
                if let Some(run_id) = report.steer_run_id() {
                    lines.push(format!("Target Run: {run_id}. If this Run ends or is replaced, the message is refused."));
                }
            }
            lines.push(format!("Pending files: {}. Confirmation sends the complete metadata below with this exact draft. A changed inventory is rejected atomically.",report.rows.len()));
            lines.push("Private paths and draft text are included in Copy details. Referenced file contents can change even when metadata matches. Queue is unavailable; use Formless TUI for deferred messages.".into());
            lines.push(format!(
                "Exact draft (JSON escaped): {}",
                escape_directional(&serde_json::to_string(&self.draft_basis).unwrap())
            ));
            for (index, value) in report.expected().iter().enumerate() {
                lines.push(format!(
                    "Pending metadata {} (complete JSON): {}",
                    index + 1,
                    escape_directional(&value.to_string())
                ));
            }
        }
        lines
    }
    pub fn details(&self) -> String {
        if let Some(report) = &self.attachments {
            escape_directional(&serde_json::json!({"forecast":self.quote,"draft":self.draft_basis,"pending_files":serde_json::from_str::<serde_json::Value>(&report.copy()).unwrap()}).to_string())
        } else {
            serde_json::to_string_pretty(&self.quote).expect("serializable forecast")
        }
    }
}
fn escape_directional(text: &str) -> String {
    text.chars().map(|c| if matches!(c,'\u{061c}'|'\u{200e}'|'\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}') {format!("\\u{:04x}",c as u32)} else {c.to_string()}).collect()
}

pub fn label(intent: &Intent) -> &'static str {
    match intent {
        Intent::SendMessage { mode, .. } => send_mode_label(*mode),
        Intent::ResumeRun { .. } => "Resume",
        Intent::InterruptRun { .. } => "Stop",
        Intent::AbandonRun { .. } => "Abandon",
    }
}

pub fn input(intent: &Intent) -> ForecastInput {
    ForecastInput {
        session_id: Some(intent.session_id().into()),
        draft: match intent {
            Intent::SendMessage { content, .. } => content.clone(),
            _ => String::new(),
        },
        resume: matches!(intent, Intent::ResumeRun { .. }),
        ..Default::default()
    }
}

pub fn validate(session_id: &str, quote: &RunForecast) -> Result<(), String> {
    if quote.basis.session_id.as_deref() != Some(session_id) || quote.fingerprint.is_empty() {
        return Err(
            "The forecast does not identify this Session and action. Nothing was started.".into(),
        );
    }
    if quote.confirmation_required
        && quote
            .confirmation_id
            .as_ref()
            .is_none_or(|id| id.is_empty())
    {
        return Err("The daemon requires confirmation but supplied no confirmation identity. Nothing was started.".into());
    }
    for range in [
        &quote.cost_usd,
        &quote.steps,
        &quote.tokens,
        &quote.duration_ms,
        &quote.cached_fraction,
    ]
    .into_iter()
    .flatten()
    {
        if !range.p50.is_finite()
            || !range.p90.is_finite()
            || range.p50 < 0.
            || range.p90 < range.p50
        {
            return Err(
                "The daemon supplied an invalid forecast range. Nothing was started.".into(),
            );
        }
    }
    Ok(())
}

fn preview(text: &str, maximum: usize) -> String {
    let mut chars = text.chars();
    let shown: String = chars.by_ref().take(maximum).collect();
    if chars.next().is_some() {
        format!("{shown}… (copy details for full text)")
    } else {
        shown
    }
}

fn number(value: f64) -> String {
    let exact = value.to_string();
    if exact.len() <= 20 {
        exact
    } else {
        format!("{value:.6e}")
    }
}

fn range(label: &str, unit: &str, value: &Option<ForecastRange>) -> String {
    match value {
        Some(value) => format!(
            "Estimated {label}: p50 {} {unit}; p90 {} {unit}",
            number(value.p50),
            number(value.p90)
        ),
        None => format!("Estimated {label}: unavailable"),
    }
}

/// Bounded summaries built outside Render. The decoded quote stays copyable.
pub fn summary(quote: &RunForecast) -> Vec<String> {
    let mut lines = vec![
        format!("Provider: {}", preview(&quote.basis.provider_key, 120)),
        format!("Model: {}", preview(&quote.basis.model, 120)),
        format!("Role: {}", preview(&quote.basis.role, 120)),
        range("cost", "USD", &quote.cost_usd),
        range("duration", "ms", &quote.duration_ms),
        range("tokens", "tokens", &quote.tokens),
        format!("Comparable recorded Runs: {}", quote.comparable_runs),
        preview(&quote.hint, 600),
    ];
    if quote.unpriced {
        lines.push("Unpriced: the forecast has no complete monetary estimate.".into());
    }
    for warning in quote.warnings.iter().take(10) {
        lines.push(format!("Warning: {}", preview(warning, 400)));
    }
    if quote.warnings.len() > 10 {
        lines.push("More warnings are available in Copy details.".into());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_identity_session_and_numeric_ranges_are_required() {
        let mut quote = crate::fixture::run_forecast("a", true);
        assert!(validate("a", &quote).is_ok());
        assert!(validate("b", &quote).is_err());
        quote.confirmation_id = None;
        assert!(validate("a", &quote).is_err());
        quote.confirmation_required = false;
        quote.cost_usd.as_mut().unwrap().p50 = f64::NAN;
        assert!(validate("a", &quote).is_err());
    }

    #[test]
    fn missing_prices_are_not_zero_and_large_warnings_remain_bounded() {
        let mut quote = crate::fixture::run_forecast("a", true);
        quote.cost_usd = None;
        quote.unpriced = true;
        quote.warnings = vec!["ไทย😀".repeat(10000); 100];
        let lines = summary(&quote);
        assert!(lines
            .iter()
            .any(|line| line == "Estimated cost: unavailable"));
        assert!(lines.iter().any(|line| line.starts_with("Unpriced:")));
        assert!(lines.len() <= 20);
        assert!(lines.iter().all(|line| line.chars().count() < 700));
        quote.cost_usd = Some(ForecastRange {
            p50: 0.00000001,
            p90: 0.00000002,
        });
        assert!(summary(&quote)
            .iter()
            .any(|line| line.contains("0.00000001 USD")));
    }
}
