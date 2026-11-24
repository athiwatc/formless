use std::{ffi::OsStr, os::unix::fs::PermissionsExt, path::Path, time::Instant};

use serde_json::{json, Value};

use super::*;
use crate::local_harness::acp_agents::{agent_ids, spec};

struct Fixture {
    root: ProbeDir,
    executable: PathBuf,
    environment: BTreeMap<OsString, OsString>,
}

impl Fixture {
    fn new(id: &str) -> Self {
        let root = ProbeDir::new().unwrap();
        let executable = root.0.join(spec(id).unwrap().program);
        fs::write(
            &executable,
            br##"#!/bin/sh
set -eu
if [ "${PROBE_GROK-}" = 1 ]; then
    [ "$GROK_DISABLE_AUTOUPDATER" = 1 ]
    [ "$XAI_API_KEY" = fixture-key ]
    [ "$*" = '--permission-mode default agent --no-leader stdio' ]
fi
[ "${FORMLESS_TOPIC_CAPABILITY+x}" != x ]
[ "${FORMLESS_TOPIC_ATTEMPT_ID+x}" != x ]
[ "${FORMLESS_DATA_DIR+x}" != x ]
[ "${FORMLESS_CONFIG_DIR+x}" != x ]
[ "${FORMLESS_TOPIC_OUTPUTS_DIR+x}" != x ]
case "$PWD" in */formless-acp-probe-*) ;; *) exit 1 ;; esac
for entry in "$PWD"/* "$PWD"/.[!.]* "$PWD"/..?*; do
    [ ! -e "$entry" ] || exit 1
done
printf '%s' "$PWD" > "$PROBE_CWD"
while IFS= read -r request; do
    case "$request" in
        *'"method":"initialize"'*)
            method=initialize
            case "$request" in *'"protocolVersion":1'*) ;; *) exit 1 ;; esac
            case "$request" in *'"terminal":false'*) ;; *) exit 1 ;; esac
            response=$PROBE_INITIALIZE ;;
        *'"method":"session/new"'*)
            method=session/new
            case "$request" in
                *'"cwd":"'*)
                    request_cwd=${request#*'"cwd":"'}
                    request_cwd=${request_cwd%%'"'*} ;;
                *) exit 1 ;;
            esac
            [ -n "$request_cwd" ] || exit 1
            request_physical=$(CDPATH= cd "$request_cwd" && pwd -P)
            current_physical=$(pwd -P)
            [ "$request_physical" = "$current_physical" ] || exit 1
            case "$request" in *'"mcpServers":[]'*) ;; *) exit 1 ;; esac
            response=$PROBE_SESSION ;;
        *) exit 1 ;;
    esac
    if [ "${PROBE_BEHAVIOR-}" = "$method" ]; then
        /bin/sleep 60 &
        if [ -n "${PROBE_CHILD_PID-}" ]; then
            printf '%s' "$!" > "$PROBE_CHILD_PID"
        fi
        wait
    fi
    printf '%s\n' "$response"
done
"##,
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let captured: Value = serde_json::from_str(include_str!("captured.json")).unwrap();
        let mut environment = BTreeMap::from([
            (
                "PROBE_INITIALIZE".into(),
                captured[id]["initialize"].to_string().into(),
            ),
            (
                "PROBE_SESSION".into(),
                captured[id]["session_new"].to_string().into(),
            ),
            ("PROBE_CWD".into(), root.0.join("cwd").into_os_string()),
            ("PATH".into(), root.0.clone().into_os_string()),
        ]);
        for name in BRIDGE_ENV {
            environment.insert((*name).into(), "must-not-reach-agent".into());
        }
        if id == "grok" {
            environment.insert("PROBE_GROK".into(), "1".into());
            environment.insert("XAI_API_KEY".into(), "fixture-key".into());
            environment.insert("GROK_DISABLE_AUTOUPDATER".into(), "0".into());
        }
        Self {
            root,
            executable,
            environment,
        }
    }

    fn probe_with_executable(&self, id: &str) -> AvailabilityWithExecutable {
        let result =
            availability_with_executable(id, Some(self.executable.clone()), &self.environment);
        let cwd = fs::read_to_string(self.root.0.join("cwd")).unwrap_or_else(|error| {
            panic!(
                "{id}: {:?}; fixture did not record cwd: {error}",
                result.availability
            )
        });
        assert!(!Path::new(&cwd).exists(), "probe cwd must be removed");
        result
    }

    fn probe(&self, id: &str) -> HarnessAvailability {
        self.probe_with_executable(id).availability
    }

    fn session(&mut self, value: Value) {
        self.environment
            .insert("PROBE_SESSION".into(), value.to_string().into());
    }
}

#[test]
fn selected_executable_path_is_retained_as_argv0_without_spawning() {
    let root = ProbeDir::new().unwrap();
    let target = root.0.join("native-target");
    let selected = root.0.join("gemini");
    fs::write(&target, b"not executed").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
    std::os::unix::fs::symlink(&target, &selected).unwrap();

    let (canonical, argv0) = resolve_executable_identity(selected.clone()).unwrap();

    assert_eq!(canonical, fs::canonicalize(target).unwrap());
    assert_eq!(argv0, selected.as_os_str());
}

#[test]
fn captured_readiness_auth_errors_and_unverifiable_successes() {
    let captured: Value = serde_json::from_str(include_str!("captured.json")).unwrap();
    for id in agent_ids() {
        let fixture = Fixture::new(id);
        let readiness = fixture.probe_with_executable(id);
        let result = &readiness.availability;
        assert_eq!(result.kind, HarnessKind::Acp);
        assert_eq!(result.acp_agent.as_deref(), Some(*id));
        assert!(result.installed, "{id}: {result:?}");
        assert_eq!(
            result.version.as_deref(),
            captured[id]["initialize"]["result"]
                .pointer("/agentInfo/version")
                .or_else(|| captured[id]["initialize"]["result"].pointer("/_meta/agentVersion"))
                .and_then(Value::as_str)
        );
        if matches!(*id, "junie" | "auggie" | "devin") {
            assert_eq!(result.signed_in, None, "{id}: {result:?}");
            let detail = result.detail.as_deref().unwrap();
            assert!(
                detail.contains("unavailable for execution"),
                "{id}: {detail}"
            );
            assert!(
                detail.contains("Signing in alone will not unblock"),
                "{id}: {detail}"
            );
            assert!(readiness.executable.is_none(), "{id}: {result:?}");
            assert!(readiness.executable_argv0.is_none(), "{id}: {result:?}");
        } else {
            assert_eq!(result.signed_in, Some(false), "{id}: {result:?}");
            let detail = result.detail.as_deref().unwrap();
            assert!(
                detail.contains(spec(id).unwrap().not_signed_in),
                "{id}: {detail}"
            );
            assert!(detail.contains("Formless did not perform ACP authentication"));
            assert!(detail.contains("not launchable"));
            if session_auth_policy(id) == Some(SessionAuthPolicy::Unverifiable) {
                assert_eq!(result.signed_in, Some(false), "{id}: {result:?}");
                assert!(detail.contains("Signing in alone will not unblock"));
            }
            if captured[id]["initialize"]["result"]["authMethods"]
                .as_array()
                .is_some_and(|methods| !methods.is_empty())
            {
                assert!(detail.contains("Advertised ACP authentication methods"));
            }
            assert!(readiness.executable.is_none(), "{id}: {result:?}");
            assert!(readiness.executable_argv0.is_none(), "{id}: {result:?}");
        }
    }
}

#[test]
fn grok_readiness_signed_in_signed_out_and_missing() {
    let mut fixture = Fixture::new("grok");
    let signed_out = fixture.probe("grok");
    assert_eq!(signed_out.acp_agent.as_deref(), Some("grok"));
    assert!(signed_out.installed);
    assert_eq!(signed_out.version.as_deref(), Some("1.0.92"));
    assert_eq!(signed_out.signed_in, Some(false));
    assert!(signed_out
        .detail
        .as_deref()
        .unwrap()
        .contains(crate::local_harness::grok::AUTH_HINT));

    fixture.session(json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"grok-ready"}}));
    let signed_in = fixture.probe("grok");
    assert_eq!(signed_in.kind, HarnessKind::Acp);
    assert_eq!(signed_in.acp_agent.as_deref(), Some("grok"));
    assert!(signed_in.installed);
    assert_eq!(signed_in.version.as_deref(), Some("1.0.92"));
    assert_eq!(signed_in.signed_in, Some(true));
    assert_eq!(signed_in.detail, None);

    for session in [
        json!({"jsonrpc":"2.0","id":2,"result":{}}),
        json!({"jsonrpc":"2.0","id":2,"error":{"code":-32603,"message":"fixture failure"}}),
    ] {
        fixture.session(session);
        let unknown = fixture.probe("grok");
        assert!(unknown.installed);
        assert_eq!(unknown.signed_in, None);
        assert_eq!(unknown.version.as_deref(), Some("1.0.92"));
        assert!(unknown.detail.is_some());
    }

    fs::remove_file(&fixture.executable).unwrap();
    let missing = availability("grok", None, &fixture.environment);
    assert_eq!(missing.acp_agent.as_deref(), Some("grok"));
    assert!(!missing.installed);
    assert_eq!(missing.signed_in, None);
    assert_eq!(missing.version, None);
    assert!(missing.detail.unwrap().contains("not installed"));
}

#[test]
fn readiness_version_falls_back_to_initialize_metadata() {
    let mut fixture = Fixture::new("gemini");
    let mut initialized: Value = serde_json::from_str(
        fixture.environment[OsStr::new("PROBE_INITIALIZE")]
            .to_str()
            .unwrap(),
    )
    .unwrap();
    initialized["result"]["agentInfo"]
        .as_object_mut()
        .unwrap()
        .remove("version");
    initialized["result"]["_meta"] = json!({"agentVersion":"metadata-version"});
    fixture
        .environment
        .insert("PROBE_INITIALIZE".into(), initialized.to_string().into());
    let result = fixture.probe("gemini");
    assert_eq!(result.version.as_deref(), Some("metadata-version"));
    assert_eq!(result.signed_in, Some(false));
    initialized["result"]["agentInfo"]["version"] = json!("agent-info-version");
    fixture
        .environment
        .insert("PROBE_INITIALIZE".into(), initialized.to_string().into());
    assert_eq!(
        fixture.probe("gemini").version.as_deref(),
        Some("agent-info-version")
    );
}

#[test]
fn readiness_initialize_auth_error_uses_the_sign_in_hint() {
    let mut fixture = Fixture::new("gemini");
    fixture.environment.insert(
        "PROBE_INITIALIZE".into(),
        json!({"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"authentication required"}})
            .to_string()
            .into(),
    );
    let result = fixture.probe_with_executable("gemini");
    assert!(result.availability.installed);
    assert_eq!(result.availability.signed_in, Some(false));
    assert_eq!(result.availability.version, None);
    let detail = result.availability.detail.as_deref().unwrap();
    assert!(detail.contains(spec("gemini").unwrap().not_signed_in));
    assert!(detail.contains("No ACP authentication methods were advertised"));
    assert!(detail.contains("not launchable"));
    assert!(result.executable.is_none());
    assert!(result.executable_argv0.is_none());
}

#[test]
fn readiness_session_success_uses_profile_auth_policy_not_method_presence() {
    let captured: Value = serde_json::from_str(include_str!("captured.json")).unwrap();
    for id in ["gemini", "kimi"] {
        assert!(!captured[id]["initialize"]["result"]["authMethods"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    for id in agent_ids() {
        let mut fixture = Fixture::new(id);
        fixture.session(json!({"jsonrpc":"2.0", "id":2, "result":{"sessionId":"ready"}}));
        let readiness = fixture.probe_with_executable(id);
        let result = &readiness.availability;
        match session_auth_policy(id).unwrap() {
            SessionAuthPolicy::Required => {
                assert_eq!(result.signed_in, Some(true), "{id}: {result:?}");
                assert!(readiness.executable.is_some(), "{id}: {result:?}");
            }
            SessionAuthPolicy::Unverifiable => {
                assert_eq!(result.signed_in, None, "{id}: {result:?}");
                let detail = result.detail.as_deref().unwrap();
                assert!(
                    detail.contains("unavailable for execution"),
                    "{id}: {detail}"
                );
                assert!(
                    detail.contains("Signing in alone will not unblock"),
                    "{id}: {detail}"
                );
                assert!(readiness.executable.is_none(), "{id}: {result:?}");
                assert!(readiness.executable_argv0.is_none(), "{id}: {result:?}");
            }
        }
    }
}

#[test]
fn unverified_profile_successes_remain_unlaunchable() {
    for id in ["cursor", "cline", "junie", "auggie", "devin"] {
        let mut fixture = Fixture::new(id);
        fixture.session(json!({"jsonrpc":"2.0", "id":2, "result":{"sessionId":"simulated"}}));
        let readiness = fixture.probe_with_executable(id);
        assert!(readiness.availability.installed, "{id}");
        assert_eq!(readiness.availability.signed_in, None, "{id}");
        assert!(readiness.executable.is_none(), "{id}");
        assert!(readiness.executable_argv0.is_none(), "{id}");
    }
}

#[test]
fn readiness_uses_path_lookup_and_rejects_missing_or_non_executable_files() {
    let fixture = Fixture::new("gemini");
    assert!(availability("gemini", None, &fixture.environment).installed);
    assert!(!availability("devin", None, &fixture.environment).installed);
    assert!(
        !availability(
            "gemini",
            Some(fixture.root.0.join("missing")),
            &fixture.environment
        )
        .installed
    );
    fs::set_permissions(&fixture.executable, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!availability("gemini", None, &fixture.environment).installed);
    assert!(!availability("gemini", Some(fixture.executable), &fixture.environment).installed);
}

#[test]
fn readiness_unknown_agent_and_relative_override_have_actionable_details() {
    let result = availability("unknown", None, &BTreeMap::new());
    assert!(!result.installed);
    assert!(result.detail.unwrap().contains("available agents:"));
    let result = availability("gemini", Some("gemini".into()), &BTreeMap::new());
    assert!(!result.installed);
    assert!(result.detail.unwrap().contains("absolute"));
}

#[test]
fn readiness_other_errors_and_empty_sessions_are_unknown() {
    let mut fixture = Fixture::new("gemini");
    for (session, expected_detail) in [
        (
            json!({"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"unavailable"}}),
            "authentication status is unverified",
        ),
        (
            json!({"jsonrpc":"2.0","id":2,"result":{}}),
            "readiness is unverified",
        ),
    ] {
        fixture.session(session);
        let result = fixture.probe_with_executable("gemini");
        assert!(result.availability.installed);
        assert_eq!(result.availability.version.as_deref(), Some("0.70.0"));
        assert_eq!(result.availability.signed_in, None);
        let detail = result.availability.detail.as_deref().unwrap();
        assert!(detail.contains(expected_detail));
        assert!(detail.contains("not launchable"));
        assert!(detail.contains("oauth-personal"));
        assert!(result.executable.is_none());
        assert!(result.executable_argv0.is_none());
    }
    fixture.environment.insert(
        "PROBE_INITIALIZE".into(),
        json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":2}})
            .to_string()
            .into(),
    );
    let result = fixture.probe_with_executable("gemini");
    assert!(result.availability.installed);
    assert_eq!(result.availability.signed_in, None);
    let detail = result.availability.detail.as_deref().unwrap();
    assert!(detail.contains("ACP protocol or identity mismatch"));
    assert!(detail.contains("ACP v1"));
    assert!(result.executable.is_none());
    assert!(result.executable_argv0.is_none());
}

#[test]
fn readiness_identity_mismatch_is_not_launchable() {
    let mut fixture = Fixture::new("cline");
    let mut initialize: Value = serde_json::from_str(include_str!("captured.json")).unwrap();
    initialize["cline"]["initialize"]["result"]["agentInfo"]["name"] = json!("other-agent");
    fixture.environment.insert(
        "PROBE_INITIALIZE".into(),
        initialize["cline"]["initialize"].to_string().into(),
    );

    let readiness = fixture.probe_with_executable("cline");

    assert!(readiness.availability.installed);
    assert_eq!(readiness.availability.signed_in, None);
    let detail = readiness.availability.detail.as_deref().unwrap();
    assert!(detail.contains("ACP protocol or identity mismatch"));
    assert!(detail.contains("Expected ACP agentInfo.name `cline`."));
    assert!(readiness.executable.is_none());
    assert!(readiness.executable_argv0.is_none());
}

#[test]
fn readiness_timeout_is_unknown_and_retains_an_initialized_version() {
    for method in ["initialize", "session/new"] {
        let mut fixture = Fixture::new("gemini");
        fixture
            .environment
            .insert("PROBE_BEHAVIOR".into(), method.into());
        fixture.environment.insert(
            "PROBE_CHILD_PID".into(),
            fixture.root.0.join("pid").into_os_string(),
        );
        let start = Instant::now();
        let result = with_timeout_with_executable(
            "gemini",
            Some(fixture.executable.clone()),
            &fixture.environment,
            Duration::from_millis(500),
        );
        assert!(start.elapsed() < Duration::from_secs(3));
        assert!(result.availability.installed);
        assert_eq!(result.availability.signed_in, None);
        assert_eq!(
            result.availability.version.as_deref(),
            if method == "initialize" {
                None
            } else {
                Some("0.70.0")
            }
        );
        assert!(result.availability.detail.unwrap().contains("timed out"));
        assert!(result.executable.is_none());
        assert!(result.executable_argv0.is_none());
        let cwd = fs::read_to_string(fixture.root.0.join("cwd")).unwrap();
        assert!(!Path::new(&cwd).exists());
        #[cfg(target_os = "linux")]
        {
            let pid = fs::read_to_string(fixture.root.0.join("pid")).unwrap();
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                let running = fs::read_to_string(format!("/proc/{pid}/stat"))
                    .is_ok_and(|stat| !stat.contains(") Z"));
                if !running {
                    break;
                }
                assert!(Instant::now() < deadline, "probe left a descendant running");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

/// Live initialize + session/new only, with credential-free environment and fresh HOME.
#[test]
#[ignore = "live ACP readiness; requires FORMLESS_LIVE_ACP_BIN_DIR"]
fn live_acp_agents_availability_installed_binaries() {
    let bin_dir = PathBuf::from(
        std::env::var_os("FORMLESS_LIVE_ACP_BIN_DIR").expect("set FORMLESS_LIVE_ACP_BIN_DIR"),
    );
    let path = std::env::var_os("PATH").unwrap_or_default();
    let env_guard = crate::credential_env::EnvGuard::isolated();
    for (name, _) in std::env::vars_os() {
        env_guard.remove(name);
    }
    env_guard.set("PATH", &path);
    let root = ProbeDir::new().unwrap();
    let mut checked = 0;
    for id in agent_ids() {
        let binary = bin_dir.join(spec(id).unwrap().program);
        if !acp::is_executable(&binary) {
            continue;
        }
        let home = root.0.join(id);
        fs::create_dir(&home).unwrap();
        let environment = BTreeMap::from([
            ("HOME".into(), home.clone().into_os_string()),
            (
                "XDG_CONFIG_HOME".into(),
                home.join("config").into_os_string(),
            ),
            ("XDG_DATA_HOME".into(), home.join("data").into_os_string()),
            ("XDG_CACHE_HOME".into(), home.join("cache").into_os_string()),
            ("XDG_STATE_HOME".into(), home.join("state").into_os_string()),
            ("PATH".into(), path.clone()),
        ]);
        let result = availability(id, Some(binary), &environment);
        println!(
            "live availability {id}: installed={} signed_in={:?} version={:?} detail={:?}",
            result.installed, result.signed_in, result.version, result.detail
        );
        assert!(result.installed, "{id}: {result:?}");
        assert_ne!(
            result.signed_in,
            Some(true),
            "{id}: unexpected sign-in in isolated HOME"
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no installed ACP binaries in {}",
        bin_dir.display()
    );
    println!("live availability: {checked} installed binaries probed");
}
