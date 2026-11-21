//! Client-local Tool Run input preparation.
use super::{usage, Result, Value};
use std::{fs, io::Read, path::Path};

const MAX_INPUT_BYTES: usize = 1024 * 1024;

pub(super) fn prepare(input: Option<&str>, file: Option<&Path>, stdin: impl Read) -> Result<Value> {
    match (input, file) {
        (Some(_), Some(_)) => Err(usage("--input and --input-file cannot be used together")),
        (inline, None) => parse_object(inline.unwrap_or("{}"), "--input"),
        (None, Some(file)) if file == Path::new("-") => read_object(stdin, "stdin"),
        (None, Some(file)) => read_file(file),
    }
}

fn read_file(path: &Path) -> Result<Value> {
    let source = format!("--input-file {path:?}");
    let regular_file = || usage(format!("{source}: expected a regular file"));
    if !fs::metadata(path)
        .map_err(|error| usage(format!("{source}: cannot read: {error}")))?
        .is_file()
    {
        return Err(regular_file());
    }

    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Like --schema, avoid blocking if the path changes before open.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let handle = options
        .open(path)
        .map_err(|error| usage(format!("{source}: cannot open: {error}")))?;
    if !handle
        .metadata()
        .map_err(|error| usage(format!("{source}: cannot inspect: {error}")))?
        .is_file()
    {
        return Err(regular_file());
    }
    read_object(handle, &source)
}

fn read_object(reader: impl Read, source: &str) -> Result<Value> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| usage(format!("{source}: cannot read: {error}")))?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(usage(format!("{source}: input exceeds the 1 MiB limit")));
    }
    let text =
        std::str::from_utf8(&bytes).map_err(|_| usage(format!("{source}: expected UTF-8 JSON")))?;
    parse_object(text, source)
}

fn parse_object(text: &str, source: &str) -> Result<Value> {
    match serde_json::from_str(text) {
        Ok(value @ Value::Object(_)) => Ok(value),
        _ => Err(usage(format!(
            "{source} must be a JSON object, like '{{\"url\": \"https://…\"}}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{exit_code, run_cli, Cli};
    use clap::{error::ErrorKind, Parser};
    use serde_json::json;

    #[test]
    fn default_and_inline_prepare_without_reading_stdin() {
        struct Unread;
        impl Read for Unread {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                panic!("default or inline input must not read stdin")
            }
        }
        assert_eq!(prepare(None, None, Unread).unwrap(), json!({}));
        let text = "{\n \"name\": \"fixture\"\n}";
        let want = json!({"name":"fixture"});
        assert_eq!(prepare(Some(text), None, Unread).unwrap(), want);
        assert_eq!(
            prepare(None, Some(Path::new("-")), text.as_bytes()).unwrap(),
            want
        );
    }

    #[test]
    fn stdin_read_stops_after_the_limit_plus_one_byte() {
        let mut at_limit = b"{}".to_vec();
        at_limit.resize(MAX_INPUT_BYTES, b' ');
        assert_eq!(
            prepare(None, Some(Path::new("-")), at_limit.as_slice()).unwrap(),
            json!({})
        );
        let mut excess = std::io::Cursor::new(at_limit);
        excess.get_mut().resize(MAX_INPUT_BYTES + 100, b' ');
        let error = prepare(None, Some(Path::new("-")), &mut excess).unwrap_err();
        assert_eq!(exit_code(&error), 2);
        assert!(error
            .to_string()
            .contains("stdin: input exceeds the 1 MiB limit"));
        assert_eq!(excess.position() as usize, MAX_INPUT_BYTES + 1);
    }

    #[test]
    fn stdin_errors_name_the_source_without_echoing_content() {
        for (invalid, hint) in [
            (&[0xff][..], "UTF-8"),
            (&b"{synthetic-private-content"[..], "JSON object"),
            (&b"[1]"[..], "JSON object"),
            (&b"{} {}"[..], "JSON object"),
        ] {
            let e = prepare(None, Some(Path::new("-")), invalid).unwrap_err();
            assert_eq!(exit_code(&e), 2);
            let text = e.to_string();
            assert!(text.contains("stdin") && text.contains(hint), "{text}");
            assert!(!text.contains("synthetic-private-content"));
        }
    }

    #[test]
    fn cli_conflict_and_local_validation_precede_profile_dispatch() {
        for args in [
            ["--input", "{}", "--input-file", "-"],
            ["--input-file", "-", "--input", "{}"],
        ] {
            let e = Cli::try_parse_from(
                ["formless", "tools", "run", "fixture"]
                    .into_iter()
                    .chain(args),
            )
            .err()
            .unwrap();
            assert_eq!(e.kind(), ErrorKind::ArgumentConflict);
            assert_eq!(e.exit_code(), 2);
        }
        let mut cli =
            Cli::try_parse_from(["formless", "tools", "run", "fixture", "--input", "[]"]).unwrap();
        // Even a regression moving this past profile validation cannot touch a real profile.
        cli.profile = Some(":invalid".into());
        let e = run_cli(cli).unwrap_err();
        assert_eq!(exit_code(&e), 2);
        assert!(e.to_string().starts_with("--input must be a JSON object"));

        let help = Cli::try_parse_from(["formless", "tools", "run", "--help"])
            .err()
            .unwrap();
        assert_eq!(help.kind(), ErrorKind::DisplayHelp);
        let text = help.to_string();
        assert!(
            text.contains("--input") && text.contains("--input-file") && text.contains("stdin")
        );
    }
}
