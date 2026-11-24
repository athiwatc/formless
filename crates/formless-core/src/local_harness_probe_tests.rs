use super::*;
use std::os::unix::fs::PermissionsExt;

fn fixture() -> (ProbeDirectory, ProbeContext) {
    let root = ProbeDirectory::new().unwrap();
    let path = root.0.join("fixture");
    std::fs::write(&path,r#"#!/usr/bin/env python3
import sys,os,json,time,pathlib,subprocess
if pathlib.Path(sys.argv[0]).name=='muse':
    if sys.argv[1:]==['--version']:
        print('Muse Code 1.4.24');sys.exit()
    if sys.argv[1:]==['usage','--json']:
        print(json.dumps({'schemaVersion':1,'allowance':{'tier':'fixture'}}));sys.exit()
    sys.exit(7)
if '--version' in sys.argv:
    if os.environ.get('PROBE_VERSION_FAIL'):sys.exit(1)
    if os.environ.get('PROBE_VERSION_HANG'):
        child=subprocess.Popen(['sleep','120'])
        pathlib.Path(os.environ['PROBE_PID']).write_text(str(child.pid))
        time.sleep(120)
    if os.environ.get('PROBE_GATE'):
        gate=pathlib.Path(os.environ['PROBE_GATE'])
        (gate/(pathlib.Path(sys.argv[0]).name+'.ready')).write_text('ready')
        while not all((gate/(name+'.ready')).exists() for name in ['claude','codex']):time.sleep(.01)
    print('1.6.0' if pathlib.Path(sys.argv[0]).name=='pi' else 'fixture 1.0');sys.exit()
mode=os.environ.get('PROBE_MODE','ok')
if mode=='hang':time.sleep(120)
if sys.argv[1:]==['auth','status']:
    print(json.dumps({'loggedIn':mode!='auth'}));sys.exit()
if sys.argv[1:]==['login','status']:
    if mode=='auth':print('Not logged in',file=sys.stderr);sys.exit(1)
    print('Logged in');sys.exit()
for line in sys.stdin:
    request=json.loads(line)
    with open(os.environ['PROBE_REQUESTS'],'a') as f:f.write(request['method']+'\n')
    if request['method']=='session/new':
        assert request['params']['mcpServers']==[]
        assert pathlib.Path(request['params']['cwd']).is_dir()
        if mode=='auth':
            print(json.dumps({'jsonrpc':'2.0','id':request['id'],'error':{'code':-32001,'message':'auth required'}}),flush=True);continue
        if mode=='other':
            print(json.dumps({'jsonrpc':'2.0','id':request['id'],'error':{'code':-1,'message':'other'}}),flush=True);continue
        child=subprocess.Popen(['sleep','120'])
        pathlib.Path(os.environ['PROBE_PID']).write_text(str(child.pid))
    print(json.dumps({'jsonrpc':'2.0','method':'notification','params':{}}),flush=True)
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':{}}),flush=True)
"#).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut environment: BTreeMap<OsString, OsString> = std::env::vars_os().collect();
    environment.insert(
        "PROBE_REQUESTS".into(),
        root.0.join("requests").into_os_string(),
    );
    environment.insert("PROBE_PID".into(), root.0.join("pid").into_os_string());
    let ctx = ProbeContext {
        executable: path,
        environment,
        timeout: Duration::from_secs(2),
    };
    (root, ctx)
}

#[test]
fn antigravity_resolves_agy_from_path_without_override() {
    let root = ProbeDirectory::new().unwrap();
    let executable = root.0.join("agy");
    std::fs::write(&executable, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let environment = BTreeMap::from([("PATH".into(), root.0.clone().into_os_string())]);

    assert_eq!(
        lookup(HarnessKind::Antigravity, &environment),
        Some(executable)
    );
}

#[tokio::test]
async fn official_probes_report_version_and_explicit_auth_status() {
    let (_root, mut ctx) = fixture();
    for kind in [HarnessKind::Claude, HarnessKind::Codex] {
        let ready = dispatch(kind, &ctx, None).await;
        assert!(ready.installed);
        assert_eq!(ready.signed_in, Some(true));
        assert_eq!(ready.version.as_deref(), Some("fixture 1.0"));
        ctx.environment.insert("PROBE_MODE".into(), "auth".into());
        assert_eq!(dispatch(kind, &ctx, None).await.signed_in, Some(false));
        ctx.environment.remove(std::ffi::OsStr::new("PROBE_MODE"));
    }
}

#[tokio::test]
async fn list_probes_run_concurrently_with_pi_adapter_and_muse_allowance() {
    let (root, mut ctx) = fixture();
    for kind in [
        HarnessKind::Claude,
        HarnessKind::Codex,
        HarnessKind::Pi,
        HarnessKind::OpenCode,
        HarnessKind::Antigravity,
        HarnessKind::Muse,
    ] {
        let target = root.0.join(kind_name(kind));
        std::fs::copy(&ctx.executable, &target).unwrap();
        ctx.environment.insert(
            format!(
                "FORMLESS_TOPIC_{}_EXE",
                kind_name(kind).to_ascii_uppercase()
            )
            .into(),
            target.into_os_string(),
        );
    }
    ctx.environment
        .insert("PROBE_GATE".into(), root.0.clone().into_os_string());
    let values = list_environment(ctx.environment, Duration::from_secs(2)).await;
    assert_eq!(values.len(), 6);
    assert!(!values.iter().any(|value| value.kind == HarnessKind::Acp));
    assert!(values
        .iter()
        .filter(|value| value.kind != HarnessKind::Antigravity)
        .all(|value| value.installed));
    assert!(values[..2]
        .iter()
        .all(|value| value.version.as_deref() == Some("fixture 1.0")
            && value.signed_in == Some(true)));
    assert_eq!(values[2].kind, HarnessKind::Pi);
    assert_eq!(values[2].version.as_deref(), Some("1.6.0"));
    assert_eq!(values[2].signed_in, None);
    assert!(values[2]
        .detail
        .as_deref()
        .unwrap()
        .contains("Sign-in unknown"));
    assert_eq!(values[3].kind, HarnessKind::OpenCode);
    assert_eq!(values[4].kind, HarnessKind::Antigravity);
    assert!(values[3..5]
        .iter()
        .filter(|value| value.kind != HarnessKind::Antigravity)
        .all(|value| value.signed_in.is_none() && value.version.as_deref() == Some("fixture 1.0")));
    let antigravity = &values[4];
    assert_eq!(antigravity.kind, HarnessKind::Antigravity);
    assert!(!antigravity.installed);
    assert_eq!(antigravity.version, None);
    assert_eq!(antigravity.signed_in, None);
    assert!(antigravity
        .detail
        .as_deref()
        .is_some_and(|detail| detail.contains("does not speak ACP v1")));
    assert_eq!(values[5].kind, HarnessKind::Muse);
    assert_eq!(values[5].version.as_deref(), Some("1.4.24"));
    assert_eq!(values[5].signed_in, Some(true));
}

#[tokio::test]
async fn muse_pi_overrides_and_missing_or_relative_executables() {
    let (_root, mut ctx) = fixture();
    for kind in [HarnessKind::Muse, HarnessKind::Pi] {
        let key: OsString = format!(
            "FORMLESS_TOPIC_{}_EXE",
            kind_name(kind).to_ascii_uppercase()
        )
        .into();
        ctx.environment
            .insert(key.clone(), ctx.executable.clone().into_os_string());
        let value = probe_environment(kind, None, &ctx.environment, Duration::from_secs(1)).await;
        assert!(value.installed);
        assert_eq!(value.signed_in, None);
        ctx.environment.insert(key.clone(), "relative".into());
        assert!(
            !probe_environment(kind, None, &ctx.environment, Duration::from_secs(1))
                .await
                .installed
        );
        ctx.environment
            .insert(key, "/missing/formless-fixture".into());
        assert!(
            !probe_environment(kind, None, &ctx.environment, Duration::from_secs(1))
                .await
                .installed
        );
    }
}

#[tokio::test]
async fn official_probe_timeout_is_unknown_and_bounded() {
    assert_eq!(ProbeContext::default().timeout, Duration::from_secs(10));
    let (_root, mut ctx) = fixture();
    ctx.environment.insert("PROBE_MODE".into(), "hang".into());
    ctx.timeout = Duration::from_millis(100);
    let value = tokio::time::timeout(Duration::from_secs(1), claude_probe(&ctx))
        .await
        .unwrap();
    assert!(value.installed);
    assert_eq!(value.signed_in, None);
    assert_eq!(value.detail.as_deref(), Some("probe timed out"));
}

#[tokio::test]
async fn acp_dispatch_receives_agent_id_and_missing_executable_keeps_identity() {
    let (_root, mut ctx) = fixture();
    let value = dispatch(HarnessKind::Acp, &ctx, Some("grok")).await;
    assert_eq!(value.acp_agent.as_deref(), Some("grok"));
    assert!(value.installed);
    assert!(value.signed_in.is_none());
    ctx.environment.clear();
    let missing = probe_environment(
        HarnessKind::Acp,
        Some("grok"),
        &ctx.environment,
        Duration::from_secs(1),
    )
    .await;
    assert_eq!(missing.acp_agent.as_deref(), Some("grok"));
    assert!(!missing.installed);
    assert_eq!(
        missing.detail.as_deref(),
        Some("ACP agent executable is not configured")
    );
}

#[tokio::test]
async fn fallback_missing_version_failure_and_timeout_keep_auth_unknown_and_kill_children() {
    let (root, mut ctx) = fixture();
    let executable = ctx.executable.clone();
    ctx.executable = root.0.join("missing");
    let missing = dispatch(HarnessKind::OpenCode, &ctx, None).await;
    assert!(!missing.installed);
    assert_eq!(missing.signed_in, None);
    ctx.executable = executable;
    ctx.environment
        .insert("PROBE_VERSION_FAIL".into(), "1".into());
    let failed = dispatch(HarnessKind::OpenCode, &ctx, None).await;
    assert!(failed.installed);
    assert_eq!(failed.signed_in, None);
    assert_eq!(failed.version, None);
    assert_eq!(failed.detail.as_deref(), Some("version probe failed"));
    ctx.environment
        .remove(std::ffi::OsStr::new("PROBE_VERSION_FAIL"));
    ctx.environment
        .insert("PROBE_VERSION_HANG".into(), "1".into());
    ctx.timeout = Duration::from_millis(300);
    let timed = tokio::time::timeout(
        Duration::from_secs(2),
        dispatch(HarnessKind::OpenCode, &ctx, None),
    )
    .await
    .unwrap();
    assert!(timed.installed);
    assert_eq!(timed.signed_in, None);
    assert_eq!(timed.detail.as_deref(), Some("probe timed out"));
    #[cfg(target_os = "linux")]
    {
        let pid = std::fs::read_to_string(root.0.join("pid")).unwrap();
        let stat = PathBuf::from(format!("/proc/{pid}/stat"));
        for _ in 0..100 {
            if std::fs::read_to_string(&stat).map_or(true, |value| {
                value
                    .split(')')
                    .nth(1)
                    .is_some_and(|state| state.trim_start().starts_with('Z'))
            }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("fallback probe left a live child");
    }
}

#[tokio::test]
async fn acp_probe_initializes_without_turns_and_distinguishes_auth_errors() {
    let (root, mut ctx) = fixture();
    let value = acp_probe(&ctx, Some(&["--version"]), &["--acp"], &[-32001]).await;
    assert_eq!(value.signed_in, Some(true));
    assert_eq!(value.version.as_deref(), Some("fixture 1.0"));
    assert_eq!(
        std::fs::read_to_string(root.0.join("requests")).unwrap(),
        "initialize\nsession/new\n"
    );
    #[cfg(target_os = "linux")]
    {
        let pid = std::fs::read_to_string(root.0.join("pid")).unwrap();
        let path = PathBuf::from(format!("/proc/{pid}/stat"));
        for _ in 0..100 {
            if std::fs::read_to_string(&path).map_or(true, |stat| {
                stat.split(')')
                    .nth(1)
                    .is_some_and(|state| state.trim_start().starts_with('Z'))
            }) {
                return_auth_tests(&mut ctx).await;
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("ACP probe left a live descendant");
    }
    #[cfg(not(target_os = "linux"))]
    return_auth_tests(&mut ctx).await;
}
async fn return_auth_tests(ctx: &mut ProbeContext) {
    ctx.environment.insert("PROBE_MODE".into(), "auth".into());
    assert_eq!(
        acp_probe(ctx, None, &["--acp"], &[-32001]).await.signed_in,
        Some(false)
    );
    ctx.environment.insert("PROBE_MODE".into(), "other".into());
    let value = acp_probe(ctx, None, &["--acp"], &[-32001]).await;
    assert_eq!(value.signed_in, None);
    assert!(value.detail.is_some());
    ctx.environment.insert("PROBE_MODE".into(), "hang".into());
    ctx.timeout = Duration::from_millis(100);
    assert_eq!(
        acp_probe(ctx, None, &["--acp"], &[-32001])
            .await
            .detail
            .as_deref(),
        Some("probe timed out")
    );
}
