//! Local input/output for pipeline Runs. Execution policy lives in Core.
use super::*;
use std::path::{Path, PathBuf};
#[derive(Debug, Clone, Default, clap::Args)]
pub(super) struct Args {
    /// JSON Schema for the final answer; '-' reads the schema from stdin
    #[arg(long, value_name = "FILE|-")]
    pub schema: Option<PathBuf>,
    /// Instructions appended for this Run only, never Session settings
    #[arg(long, value_name = "TEXT")]
    pub system: Option<String>,
    /// Override this Run's USD cap (finite, nonnegative; zero sends no model request)
    #[arg(long, value_parser = parse_max_usd, value_name = "USD")]
    pub max_usd: Option<f64>,
    /// Atomically write successful output to this file; stdout stays empty
    #[arg(long, value_name = "PATH", conflicts_with = "estimate")]
    pub output_file: Option<PathBuf>,
}
pub(super) struct Prepared {
    pub task: String,
    pub attachment: Option<Vec<u8>>,
    pub options: formless_protocol::RunOptions,
}
fn parse_max_usd(value: &str) -> Result<f64, String> {
    let amount: f64 = value
        .parse()
        .map_err(|_| "--max-usd requires a finite nonnegative USD amount")?;
    if !amount.is_finite() || amount < 0.0 {
        return Err("--max-usd requires a finite nonnegative USD amount".into());
    }
    Ok(amount)
}
impl Args {
    pub fn enabled(&self) -> bool {
        self.schema.is_some()
            || self.system.is_some()
            || self.max_usd.is_some()
            || self.output_file.is_some()
    }
    pub fn prepare(&self, task: String, stdin: Option<Vec<u8>>) -> Result<Prepared> {
        let schema_stdin = self.schema.as_deref() == Some(Path::new("-"));
        anyhow::ensure!(
            !(schema_stdin && task == "-"),
            "--schema - and task '-' cannot both consume stdin"
        );
        let (task, attachment, stdin_schema) = if schema_stdin {
            (
                task,
                None,
                Some(stdin.context("--schema - requires a schema on stdin")?),
            )
        } else {
            let (task, attachment) = super::run_input(task, stdin).map_err(anyhow::Error::msg)?;
            (task, attachment, None)
        };
        let schema = if let Some(path) = &self.schema {
            Some(match stdin_schema {
                Some(bytes) => formless_core::pipeline::parse_schema_bytes(&bytes)?,
                None => formless_core::pipeline::read_schema_file(path)?,
            })
        } else {
            None
        };
        let options = formless_protocol::RunOptions {
            system: self.system.clone(),
            schema,
            max_usd: self.max_usd,
            ..Default::default()
        };
        formless_core::pipeline::CompiledRunOptions::compile(options.clone())?;
        Ok(Prepared {
            task,
            attachment,
            options,
        })
    }
}

/// Bound selected result output before it can be emitted or atomically committed.
/// JSON never includes a failed schema candidate in its `answer` field.
pub(super) fn render_result(
    output: RunOutput,
    session: &str,
    status: &str,
    answer: Option<&str>,
    result: Option<&Value>,
    error: Option<&str>,
    served_model: Option<&formless_protocol::ServedModelInfo>,
) -> Result<Vec<u8>> {
    let mut bytes = match output {
        RunOutput::Json => {
            let mut value = serde_json::json!({"session_id":session,"status":status,"answer":answer,"result":result,"error":error});
            if let Some(served_model) = served_model {
                value["served_model"] = serde_json::to_value(served_model)?;
            }
            serde_json::to_vec(&value)?
        }
        RunOutput::Text | RunOutput::Final => answer.unwrap_or_default().as_bytes().to_vec(),
        RunOutput::JsonlEvents => anyhow::bail!("event output must use its bounded event buffer"),
    };
    if !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    anyhow::ensure!(
        bytes.len() <= formless_core::pipeline::MAX_RESULT_FILE_BYTES,
        "Run output exceeds the 8 MiB result limit"
    );
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn buffered_answers_keep_all_whitespace_and_only_add_a_missing_lf() {
        for output in [RunOutput::Text, RunOutput::Final] {
            for (answer, expected) in [
                ("hello  \n", "hello  \n"),
                ("hello\n\n", "hello\n\n"),
                ("hello\t", "hello\t\n"),
                ("ไทย\u{a0}", "ไทย\u{a0}\n"),
                ("ไทย\r\n", "ไทย\r\n"),
                ("ไทย", "ไทย\n"),
                ("", "\n"),
            ] {
                assert_eq!(
                    render_result(output, "s", "completed", Some(answer), None, None, None)
                        .unwrap(),
                    expected.as_bytes()
                );
            }
            assert_eq!(
                render_result(output, "s", "completed", None, None, None, None).unwrap(),
                b"\n"
            );
            let oversized = " ".repeat(formless_core::pipeline::MAX_RESULT_FILE_BYTES);
            assert!(
                render_result(output, "s", "completed", Some(&oversized), None, None, None)
                    .is_err()
            );
        }
        let answer = "hello  \n\n";
        let served_model = formless_protocol::ServedModelInfo {
            name: "served".into(),
            via: "fallback".into(),
            requested: "requested".into(),
        };
        let json = render_result(
            RunOutput::Json,
            "s",
            "completed",
            Some(answer),
            Some(&serde_json::json!({"n": 2})),
            None,
            Some(&served_model),
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(value["answer"], answer);
        assert_eq!(value["result"], serde_json::json!({"n": 2}));
        assert_eq!(value["served_model"]["name"], "served");
        assert_eq!(value["served_model"]["via"], "fallback");
        assert_eq!(value["served_model"]["requested"], "requested");
    }

    #[test]
    fn bom_schema_stdin_matches_plain_and_keeps_the_raw_byte_limit() {
        let args = Args {
            schema: Some("-".into()),
            ..Default::default()
        };
        let plain = br#"{"type":"integer"}"#;
        let bom = [b"\xef\xbb\xbf".as_slice(), plain].concat();
        assert_eq!(
            args.prepare("task".into(), Some(bom))
                .unwrap()
                .options
                .schema,
            args.prepare("task".into(), Some(plain.to_vec()))
                .unwrap()
                .options
                .schema
        );
        for invalid in [
            b"\xef\xbb\xbfnot JSON".to_vec(),
            [
                b"\xef\xbb\xbf".as_slice(),
                &vec![b' '; formless_core::pipeline::MAX_SCHEMA_BYTES],
            ]
            .concat(),
        ] {
            assert!(args.prepare("task".into(), Some(invalid)).is_err());
        }
    }
    #[test]
    fn schema_stdin_is_never_attached_or_reused_as_task_input() {
        let args = Args {
            schema: Some("-".into()),
            ..Default::default()
        };
        let prepared = args
            .prepare(
                "return a number".into(),
                Some(br#"{"type":"integer"}"#.to_vec()),
            )
            .unwrap();
        assert_eq!(prepared.task, "return a number");
        assert!(prepared.attachment.is_none());
        assert_eq!(
            prepared.options.schema,
            Some(serde_json::json!({"type":"integer"}))
        );
        assert!(args
            .prepare("-".into(), Some(br#"{"type":"integer"}"#.to_vec()))
            .is_err());
        assert!(args.prepare("task".into(), None).is_err());
        assert!(args
            .prepare("task".into(), Some(b"not JSON".to_vec()))
            .is_err());
    }
    #[test]
    fn ordinary_stdin_and_explicit_zero_keep_distinct_meanings() {
        let args = Args {
            max_usd: Some(parse_max_usd("0").unwrap()),
            system: Some("Run only ไทย".into()),
            ..Default::default()
        };
        let prepared = args
            .prepare("review".into(), Some(b"diff bytes".to_vec()))
            .unwrap();
        assert_eq!(prepared.attachment, Some(b"diff bytes".to_vec()));
        assert_eq!(prepared.options.max_usd, Some(0.0));
        assert_eq!(prepared.options.system.as_deref(), Some("Run only ไทย"));
        for bad in ["-1", "NaN", "inf", "-inf", "no"] {
            assert!(parse_max_usd(bad).is_err());
        }
        assert_eq!(parse_max_usd("0.25").unwrap(), 0.25);
        assert!(Args::default()
            .prepare("-".into(), Some(b"task from stdin".to_vec()))
            .unwrap()
            .attachment
            .is_none());
    }
    #[test]
    fn failed_schema_json_result_has_no_candidate_answer() {
        let bytes = render_result(
            RunOutput::Json,
            "session",
            "schema_mismatch",
            None,
            None,
            Some("schema: mismatch"),
            None,
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value["answer"].is_null());
        assert!(value["result"].is_null());
        assert_eq!(value["status"], "schema_mismatch");
        assert_eq!(
            render_result(
                RunOutput::Final,
                "s",
                "completed",
                Some("{\"n\":2}"),
                None,
                None,
                None,
            )
            .unwrap(),
            b"{\"n\":2}\n"
        );
    }
}
