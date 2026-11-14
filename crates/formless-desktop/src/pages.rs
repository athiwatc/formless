//! Bounded read-only projections of existing daemon destinations.
use crate::model::Destination;
use serde_json::Value;

pub const MAX_ROWS: usize = 1000;
pub const PREVIEW_CHARS: usize = 500;

#[derive(Debug)]
pub struct Row {
    pub label: String,
    pub preview: String,
    pub preview_truncated: bool,
    pub detail: String,
}

#[derive(Debug, Default)]
pub struct Page {
    pub revision: u64,
    pub rows: Vec<Row>,
    pub selected: Option<usize>,
    pub notice: String,
}

impl Page {
    pub fn decode(destination: Destination, value: Value) -> Result<Self, String> {
        let entries: Vec<(String, Value)> = match destination {
            Destination::Tools | Destination::Proposals => {
                let values = value.as_array().ok_or("Expected a list from the daemon")?;
                values
                    .iter()
                    .take(MAX_ROWS + 1)
                    .enumerate()
                    .map(|(index, value)| {
                        let name = value
                            .get("name")
                            .or_else(|| value.get("title"))
                            .or_else(|| value.get("id"))
                            .and_then(Value::as_str);
                        (
                            name.map(str::to_owned)
                                .unwrap_or_else(|| format!("Item {}", index + 1)),
                            value.clone(),
                        )
                    })
                    .collect()
            }
            Destination::Metrics => {
                let runs = value
                    .get("runs")
                    .and_then(Value::as_array)
                    .ok_or("Expected Run metrics from the daemon")?;
                let mut entries = vec![(
                    "Summary · recorded daemon measurements".into(),
                    value
                        .get("summary")
                        .cloned()
                        .ok_or("Missing metrics summary")?,
                )];
                entries.extend(
                    runs.iter()
                        .take(MAX_ROWS)
                        .enumerate()
                        .map(|(index, value)| {
                            let id = value
                                .get("run_id")
                                .or_else(|| value.get("id"))
                                .and_then(Value::as_str)
                                .unwrap_or("Run");
                            (format!("{} · {id}", index + 1), value.clone())
                        }),
                );
                entries
            }
            Destination::Settings => value
                .as_object()
                .ok_or("Expected grouped settings from the daemon")?
                .iter()
                .take(MAX_ROWS + 1)
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
            _ => return Err("This destination has no resource projection".into()),
        };
        let truncated = entries.len() > MAX_ROWS;
        let rows = entries
            .into_iter()
            .take(MAX_ROWS)
            .map(|(label, value)| {
                let detail = serde_json::to_string_pretty(&value).expect("JSON value serializes");
                let fields = match &value {
                    Value::Object(fields) => fields
                        .iter()
                        .map(|(key, value)| {
                            let label = key
                                .split('_')
                                .map(|word| if word == "id" { "ID" } else { word })
                                .collect::<Vec<_>>()
                                .join(" ");
                            let value = value
                                .as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| value.to_string());
                            format!("{label}: {value}")
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    Value::String(text) => text.clone(),
                    _ => value.to_string(),
                };
                let preview_truncated = fields.chars().nth(PREVIEW_CHARS).is_some();
                let preview: String = fields.chars().take(PREVIEW_CHARS).collect();
                Row {
                    label,
                    preview,
                    preview_truncated,
                    detail,
                }
            })
            .collect::<Vec<_>>();
        let scope = if destination == Destination::Metrics {
            " · Summary and up to 100 recent Runs"
        } else {
            ""
        };
        Ok(Self {
            revision: 0,
            selected: (!rows.is_empty()).then_some(0),
            notice: format!(
                "{} entries{}{scope} · Read only. Use the Formless TUI to edit.",
                rows.len(),
                if truncated { " · first 1000 only" } else { "" }
            ),
            rows,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_shapes_instead_of_showing_malformed_replies_as_empty() {
        for destination in [
            Destination::Tools,
            Destination::Metrics,
            Destination::Proposals,
            Destination::Settings,
        ] {
            assert!(Page::decode(destination, Value::Null).is_err());
        }
        assert!(Page::decode(Destination::Tools, json!([]))
            .unwrap()
            .rows
            .is_empty());
    }

    #[test]
    fn metrics_notice_names_the_server_request_scope() {
        let page = Page::decode(Destination::Metrics, json!({"summary":{},"runs":[]})).unwrap();
        assert!(page.notice.contains("up to 100 recent Runs"));
    }

    #[test]
    fn bounded_unicode_preview_keeps_exact_copy_source_and_labels_partial_results() {
        let value = json!({"name": "Thai ไทย", "text": "😀".repeat(900)});
        let page = Page::decode(
            Destination::Tools,
            Value::Array(vec![value.clone(); MAX_ROWS + 20]),
        )
        .unwrap();
        assert_eq!(page.rows.len(), MAX_ROWS);
        assert!(page.notice.contains("first 1000"));
        assert_eq!(page.rows[0].preview.chars().count(), PREVIEW_CHARS);
        assert_eq!(
            serde_json::from_str::<Value>(&page.rows[0].detail).unwrap(),
            value
        );
    }
}
