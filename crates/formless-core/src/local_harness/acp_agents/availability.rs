use std::{collections::BTreeMap, ffi::OsString, fs, path::PathBuf, time::Duration};

use formless_protocol::{HarnessAvailability, HarnessKind};

use super::{acp, require_spec, session_requires_auth, HarnessRequest, StepAccess};

const TIMEOUT: Duration = Duration::from_secs(8);
const BRIDGE_ENV: &[&str] = &[
    "FORMLESS_TOPIC_CAPABILITY",
    "FORMLESS_TOPIC_ATTEMPT_ID",
    "FORMLESS_DATA_DIR",
    "FORMLESS_CONFIG_DIR",
    "FORMLESS_TOPIC_OUTPUTS_DIR",
];

/// Probes an installed CLI with initialize and session/new, never a prompt or sign-in.
/// An explicit executable must be absolute; otherwise the CLI is looked up on PATH.
pub fn availability(
    id: &str,
    executable: Option<PathBuf>,
    environment: &BTreeMap<OsString, OsString>,
) -> HarnessAvailability {
    availability_with_executable(id, executable, environment).availability
}

pub(crate) struct AvailabilityWithExecutable {
    pub availability: HarnessAvailability,
    pub executable: Option<PathBuf>,
}

pub(crate) fn availability_with_executable(
    id: &str,
    executable: Option<PathBuf>,
    environment: &BTreeMap<OsString, OsString>,
) -> AvailabilityWithExecutable {
    with_timeout_with_executable(id, executable, environment, TIMEOUT)
}

fn with_timeout(
    id: &str,
    executable: Option<PathBuf>,
    environment: &BTreeMap<OsString, OsString>,
    timeout: Duration,
) -> HarnessAvailability {
    with_timeout_with_executable(id, executable, environment, timeout).availability
}

fn with_timeout_with_executable(
    id: &str,
    executable: Option<PathBuf>,
    environment: &BTreeMap<OsString, OsString>,
    timeout: Duration,
) -> AvailabilityWithExecutable {
    let mut result = HarnessAvailability {
        kind: HarnessKind::Acp,
        acp_agent: Some(id.into()),
        installed: false,
        signed_in: None,
        version: None,
        detail: None,
    };
    let spec = match require_spec(id) {
        Ok(spec) => spec,
        Err(error) => {
            result.detail = Some(error.to_string());
            return AvailabilityWithExecutable {
                availability: result,
                executable: None,
            };
        }
    };
    let program = match executable {
        Some(path) if path.is_absolute() => acp::is_executable(&path).then_some(path),
        Some(_) => {
            result.detail = Some("ACP executable override must be an absolute path.".into());
            return AvailabilityWithExecutable {
                availability: result,
                executable: None,
            };
        }
        None => acp::find_executable(spec.program, environment),
    };
    let Some(program) = program else {
        result.detail = Some(format!(
            "{} is not installed or its executable is not on PATH.",
            spec.name
        ));
        return AvailabilityWithExecutable {
            availability: result,
            executable: None,
        };
    };
    let program = match fs::canonicalize(program) {
        Ok(path) if acp::is_executable(&path) => path,
        Ok(_) => {
            result.detail = Some("ACP executable is not executable.".into());
            return AvailabilityWithExecutable {
                availability: result,
                executable: None,
            };
        }
        Err(error) => {
            result.detail = Some(format!("Could not resolve ACP executable path: {error}"));
            return AvailabilityWithExecutable {
                availability: result,
                executable: None,
            };
        }
    };
    result.installed = true;
    let cwd = match ProbeDir::new() {
        Ok(cwd) => cwd,
        Err(error) => {
            result.detail = Some(format!("Could not create ACP probe directory: {error}"));
            return AvailabilityWithExecutable {
                availability: result,
                executable: Some(program),
            };
        }
    };
    let mut environment = environment.clone();
    for name in BRIDGE_ENV {
        environment.insert((*name).into(), OsString::new());
    }
    let request = HarnessRequest {
        kind: HarnessKind::Acp,
        acp_agent: Some(id.into()),
        model: None,
        cwd: cwd.0.clone(),
        prompt: "probe".into(),
        access: StepAccess::Write,
        mcp_command: cwd.0.clone(),
        mcp_args: vec![],
        mcp_env_vars: BRIDGE_ENV.iter().map(|name| (*name).into()).collect(),
        executable: Some(program.clone()),
        environment,
    };
    let probe = acp::probe_session(spec, &request, timeout);
    result.version = probe.version;
    match probe.session {
        Ok(Err(code)) if spec.auth_error_codes.contains(&code) => {
            result.signed_in = Some(false);
            result.detail = Some(spec.not_signed_in.into());
        }
        Ok(Ok(session))
            if session["sessionId"]
                .as_str()
                .is_some_and(|id| !id.is_empty()) =>
        {
            if session_requires_auth(id) {
                result.signed_in = Some(true);
            } else {
                result.detail = Some("Sign-in not verifiable before a prompt: this agent opens unauthenticated sessions.".into());
            }
        }
        Ok(Ok(_)) => result.detail = Some("ACP session/new returned no session id.".into()),
        Ok(Err(code)) => {
            result.detail = Some(format!("ACP readiness failed (JSON-RPC error {code})."))
        }
        Err(error) => result.detail = Some(format!("ACP readiness is unknown: {error:#}")),
    }
    AvailabilityWithExecutable {
        availability: result,
        executable: Some(program),
    }
}

struct ProbeDir(PathBuf);

impl ProbeDir {
    fn new() -> std::io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "formless-acp-probe-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let mut builder = builder;
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(Self(path))
    }
}

impl Drop for ProbeDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(all(test, unix))]
#[path = "availability_tests.rs"]
mod tests;
