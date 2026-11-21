//! Explicit text cells for the human Run list, with full-identifier records
//! when common columns exceed the reported terminal width.
use anyhow::Result;
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

const HEADERS: [&str; 7] = [
    "RUN ID", "CREATED", "OUTCOME", "STEPS", "TOOLS", "TOKENS", "DURATION",
];

pub(crate) fn render(value: &Value, width: Option<usize>) -> Result<String> {
    let summary: formless_protocol::RunSummary = serde_json::from_value(value["summary"].clone())?;
    let mut text = format!("{}\n", formless_core::runs::summary_line(&summary));
    let rows: Vec<Vec<String>> = value["runs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|row| {
            let integer = |key: &str| {
                row[key]
                    .as_i64()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "-".into())
            };
            vec![
                crate::comparison_terminal_text(row["run_id"].as_str().unwrap_or_default()),
                crate::comparison_terminal_text(&formless_tui::local_timestamp_seconds(
                    row["created_at"].as_str().unwrap_or_default(),
                )),
                crate::comparison_terminal_text(row["outcome"].as_str().unwrap_or_default()),
                integer("steps"),
                integer("tool_calls"),
                (i128::from(row["prompt_tokens"].as_i64().unwrap_or(0))
                    + i128::from(row["completion_tokens"].as_i64().unwrap_or(0)))
                .to_string(),
                format!(
                    "{:.1}s",
                    row["duration_ms"].as_i64().unwrap_or(0) as f64 / 1000.0
                ),
            ]
        })
        .collect();
    if rows.is_empty() {
        return Ok(text);
    }
    let widths: Vec<usize> = (0..HEADERS.len())
        .map(|i| {
            rows.iter()
                .map(|row| row[i].width())
                .chain([HEADERS[i].width()])
                .max()
                .unwrap()
        })
        .collect();
    let common_width = widths.iter().sum::<usize>() + 2 * (HEADERS.len() - 1);
    if width.is_some_and(|limit| common_width > limit) {
        for row in rows {
            text.push('\n');
            for (label, cell) in HEADERS.iter().zip(row) {
                text.push_str(&format!("{label}: {cell}\n"));
            }
        }
    } else {
        for row in std::iter::once(HEADERS.iter().map(|s| s.to_string()).collect()).chain(rows) {
            let cells: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(i, cell)| {
                    let padding = " ".repeat(widths[i].saturating_sub(cell.width()));
                    if i >= 3 {
                        format!("{padding}{cell}")
                    } else {
                        format!("{cell}{padding}")
                    }
                })
                .collect();
            text.push_str(&cells.join("  "));
            text.push('\n');
        }
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report() -> Value {
        json!({"summary":{"runs":4,"completed":0.5,"interrupted":0.25,"paused":0.25,"edited":0.0,"corrected":0.0,"avg_steps":4.0,"avg_tool_calls":2.0,"avg_tokens":100.0,"avg_duration_ms":1200.0},"runs":[
            {"run_id":"id-a","created_at":"2026-11-01T12:34:56Z","outcome":"completed","steps":1,"tool_calls":1,"prompt_tokens":10,"completion_tokens":5,"duration_ms":1200},
            {"run_id":"id-b","created_at":"2026-11-01T12:34:56Z","outcome":"very-long-outcome","steps":10,"tool_calls":10,"prompt_tokens":100,"completion_tokens":50,"duration_ms":2345},
            {"run_id":"id-c","created_at":"2026-11-01T12:34:56Z","outcome":"failed","steps":100,"tool_calls":100,"prompt_tokens":1000,"completion_tokens":500,"duration_ms":3456},
            {"run_id":"id-d","created_at":"2026-11-01T12:34:56Z","outcome":"interrupted","steps":1000,"tool_calls":1000,"prompt_tokens":10000,"completion_tokens":5000,"duration_ms":4567}
        ],"future":"unchanged"})
    }

    #[test]
    fn counters_align_as_text_cells_and_narrow_records_keep_full_values() {
        let value = report();
        let original = value.clone();
        let table = render(&value, None).unwrap();
        let lines: Vec<_> = table.lines().collect();
        let steps = lines[1].find("STEPS").unwrap();
        let tools = lines[1].find("TOOLS").unwrap();
        for (line, n) in lines[2..].iter().zip([1, 10, 100, 1000]) {
            assert_eq!(&line[steps..steps + 5], format!("{n:>5}"));
            assert_eq!(&line[tools..tools + 5], format!("{n:>5}"));
            assert_eq!(line.width(), lines[1].width());
        }
        assert!(table.contains("very-long-outcome"));
        assert!(table.contains("15000"));
        assert!(table.contains("4.6s"));
        for width in [1, 20, 40, lines[1].width() - 1] {
            let text = render(&value, Some(width)).unwrap();
            assert!(text.contains("RUN ID: id-a"));
            assert!(text.contains("STEPS: 1000\nTOOLS: 1000"));
            assert!(text.contains("OUTCOME: very-long-outcome"));
            assert_eq!(text.lines().next(), table.lines().next());
        }
        assert_eq!(value, original);
        assert_eq!(render(&value, Some(lines[1].width())).unwrap(), table);
        assert_eq!(render(&value, Some(1000)).unwrap(), table);
    }

    #[test]
    fn unicode_ids_and_defensive_counter_placeholders_are_not_clipped_or_reclassified() {
        let mut value = report();
        let id = "run-".to_owned() + &"日本語".repeat(100);
        value["runs"][0]["run_id"] = json!(id);
        value["runs"][1]["steps"] = Value::Null;
        value["runs"][1]["tool_calls"] = json!({"invalid":7});
        let full = render(&value, None).unwrap();
        assert!(full.contains(&id));
        let rows: Vec<_> = full.lines().skip(1).collect();
        assert!(rows.iter().all(|line| line.width() == rows[0].width()));
        let narrow = render(&value, Some(20)).unwrap();
        assert!(narrow.contains(&format!("RUN ID: {id}")));
        assert!(narrow.contains("STEPS: -\nTOOLS: -"));
        assert!(!narrow.contains("ok completed"));
        assert!(!narrow.contains("fail failed"));
    }
}
