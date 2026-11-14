//! Local templates use the plain client's ordinary literal message sender.
use formless_core::{
    user_commands::{self, Snapshot},
    Paths,
};
use formless_protocol::SessionInfo;

pub(super) fn load(paths: &Paths, session: &SessionInfo) -> Snapshot {
    let cwd = session
        .cwd
        .as_ref()
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    let mut reserved = crate::builtin_command_names();
    // Plain-only aliases have the same precedence as canonical builtins.
    reserved.extend(["exit".into(), "switch".into()]);
    user_commands::load(&paths.config_dir, cwd.as_deref(), &reserved)
}

pub(super) fn expand(snapshot: &Snapshot, input: &str) -> Result<Option<String>, String> {
    let Some(invocation) = input
        .strip_prefix('/')
        .filter(|text| !text.starts_with('/'))
    else {
        return Ok(None);
    };
    let end = invocation
        .find(char::is_whitespace)
        .unwrap_or(invocation.len());
    let name = &invocation[..end];
    if ["exit", "switch"].contains(&name)
        || crate::builtin_command_names()
            .iter()
            .any(|builtin| builtin == name)
    {
        return Ok(None);
    }
    let Some(command) = snapshot.find(name) else {
        return match crate::user_commands_ui::unavailable(snapshot, name) {
            Some(message) => Err(message),
            None => Ok(None),
        };
    };
    let args = invocation[end..]
        .chars()
        .next()
        .map_or("", |separator| &invocation[end + separator.len_utf8()..]);
    command
        .expand(args)
        .map(Some)
        .map_err(|error| format!("/{name}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plain_templates_follow_session_cwd_and_never_override_malformed_builtins() {
        let app = crate::tests::test_app();
        let global = app.paths.config_dir.join("commands");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(global.join("workflow.md"), "Global $ARGUMENTS").unwrap();
        std::fs::write(global.join("help.md"), "---\ndescription: not closed").unwrap();
        std::fs::write(global.join("exit.md"), "---\ndescription: not closed").unwrap();
        let mut session: SessionInfo = serde_json::from_value(
            serde_json::json!({"id":"s", "name":"Plain", "created_at":"", "updated_at":""}),
        )
        .unwrap();
        for project in ["first", "second"] {
            let cwd = app.paths.data_dir.join(project);
            let local = cwd.join(".formless/commands");
            std::fs::create_dir_all(&local).unwrap();
            std::fs::write(local.join("workflow.md"), format!("{project} $ARGUMENTS")).unwrap();
            std::fs::write(local.join("literal.md"), "/quit").unwrap();
            session.cwd = Some(cwd.to_string_lossy().into_owned());
            let snapshot = load(&app.paths, &session);
            assert_eq!(
                expand(&snapshot, "/workflow a  b").unwrap(),
                Some(format!("{project} a  b"))
            );
            assert_eq!(expand(&snapshot, "/literal").unwrap(), Some("/quit".into()));
            for builtin in ["/help", "/exit", "/quit", "/switch 1"] {
                assert_eq!(expand(&snapshot, builtin).unwrap(), None);
            }
            assert_eq!(expand(&snapshot, "//workflow literal").unwrap(), None);
            assert_eq!(expand(&snapshot, "/notpresent").unwrap(), None);
        }
    }
    #[test]
    fn plain_templates_report_missing_positions_and_keep_escaped_dollars_literal() {
        let app = crate::tests::test_app();
        let directory = app.paths.config_dir.join("commands");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("price.md"), "Cost $$5 or $$9.99; `echo $$1`").unwrap();
        std::fs::write(directory.join("required.md"), "$1 then $2").unwrap();
        let session: SessionInfo = serde_json::from_value(serde_json::json!({"id":"s","name":"Plain","created_at":"","updated_at":"","cwd":app.paths.data_dir})).unwrap();
        let snapshot = load(&app.paths, &session);
        assert_eq!(
            expand(&snapshot, "/price").unwrap(),
            Some("Cost $5 or $9.99; `echo $1`".into())
        );
        let error = expand(&snapshot, "/required private-argument").unwrap_err();
        assert!(
            error.contains("/required:")
                && error.contains("required.md")
                && error.contains("references $2")
                && error.contains("write $$2"),
            "{error}"
        );
        assert!(!error.contains("private-argument"), "{error}");
        assert_eq!(
            expand(&snapshot, "/required one two").unwrap(),
            Some("one then two".into())
        );
    }
}
