use std::{collections::BTreeMap, ffi::OsString, fs, path::PathBuf, time::Duration};

use formless_protocol::{HarnessAvailability, HarnessKind};

use super::{
    acp, require_spec, session_auth_policy, AgentCompatibilityError, HarnessRequest,
    SessionAuthPolicy, StepAccess,
};

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
    pub executable_argv0: Option<OsString>,
}

fn unlaunchable(availability: HarnessAvailability) -> AvailabilityWithExecutable {
    AvailabilityWithExecutable {
        availability,
        executable: None,
        executable_argv0: None,
    }
}

fn auth_methods_detail(methods: &[String]) -> String {
    if methods.is_empty() {
        "No ACP authentication methods were advertised.".into()
    } else {
        format!(
            "Advertised ACP authentication methods: {}.",
            methods.join(", ")
        )
    }
}

fn unverifiable_auth_detail(id: &str, sign_in: &str, methods: &[String]) -> String {
    format!(
        "ACP profile `{id}` is unavailable for execution because Formless has no supported non-model sign-in check. {sign_in} Signing in alone will not unblock it; ask the user to approve a supported substitute. This readiness result is not launchable. {}",
        auth_methods_detail(methods)
    )
}

pub(crate) fn availability_with_executable(
    id: &str,
    executable: Option<PathBuf>,
    environment: &BTreeMap<OsString, OsString>,
) -> AvailabilityWithExecutable {
    with_timeout_with_executable(id, executable, environment, TIMEOUT)
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
                executable_argv0: None,
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
                executable_argv0: None,
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
            executable_argv0: None,
        };
    };
    let executable_argv0 = program.as_os_str().to_os_string();
    let program = match fs::canonicalize(program) {
        Ok(path) if acp::is_executable(&path) => path,
        Ok(_) => {
            result.detail = Some("ACP executable is not executable.".into());
            return AvailabilityWithExecutable {
                availability: result,
                executable: None,
                executable_argv0: None,
            };
        }
        Err(error) => {
            result.detail = Some(format!("Could not resolve ACP executable path: {error}"));
            return AvailabilityWithExecutable {
                availability: result,
                executable: None,
                executable_argv0: None,
            };
        }
    };
    result.installed = true;
    let cwd = match ProbeDir::new() {
        Ok(cwd) => cwd,
        Err(error) => {
            result.detail = Some(format!("Could not create ACP probe directory: {error}"));
            return unlaunchable(result);
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
        executable_argv0: Some(executable_argv0.clone()),
        environment,
    };
    let probe = acp::probe_session(spec, &request, timeout);
    result.version = probe.version;
    let auth_methods = probe.auth_methods;
    match probe.session {
        Ok(Err(code)) if spec.auth_error_codes.contains(&code) => {
            result.signed_in = Some(false);
            result.detail = Some(if session_auth_policy(id)
                == Some(SessionAuthPolicy::Unverifiable)
            {
                format!(
                    "ACP readiness explicitly reported authentication required (JSON-RPC error {code}); signed-in status is false. {} Formless did not perform ACP authentication.",
                    format!("{} Formless did not perform ACP authentication.", unverifiable_auth_detail(id, spec.not_signed_in, &auth_methods))
                )
            } else {
                format!(
                    "ACP readiness reported authentication required (JSON-RPC error {code}). {} Formless did not perform ACP authentication; this readiness result is not launchable. {}",
                    spec.not_signed_in,
                    auth_methods_detail(&auth_methods)
                )
            });
            return unlaunchable(result);
        }
        Ok(Ok(session))
            if session["sessionId"]
                .as_str()
                .is_some_and(|id| !id.is_empty()) =>
        {
            match session_auth_policy(id).expect("registered ACP profile has an auth policy") {
                SessionAuthPolicy::Required => result.signed_in = Some(true),
                SessionAuthPolicy::Unverifiable => {
                    result.detail = Some(unverifiable_auth_detail(
                        id,
                        spec.not_signed_in,
                        &auth_methods,
                    ));
                    return unlaunchable(result);
                }
            }
        }
        Ok(Ok(_)) => {
            result.detail = Some(format!(
                "ACP session/new returned no session id; readiness is unverified and this result is not launchable. {}",
                auth_methods_detail(&auth_methods)
            ));
            return unlaunchable(result);
        }
        Ok(Err(code)) => {
            result.detail = Some(format!(
                "ACP readiness failed (JSON-RPC error {code}); authentication status is unverified and this result is not launchable. {} Formless does not perform ACP authentication.",
                auth_methods_detail(&auth_methods)
            ));
            return unlaunchable(result);
        }
        Err(error) if error.downcast_ref::<AgentCompatibilityError>().is_some() => {
            result.detail = Some(format!("ACP protocol or identity mismatch: {error:#}"));
            return unlaunchable(result);
        }
        Err(error) => {
            result.detail = Some(format!(
                "ACP readiness is unknown and this result is not launchable: {error:#}"
            ));
            return unlaunchable(result);
        }
    }
    AvailabilityWithExecutable {
        availability: result,
        executable: Some(program),
        executable_argv0: Some(executable_argv0.clone()),
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
