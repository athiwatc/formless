use super::HarnessKind;
use anyhow::{bail, Context, Result};
use formless_protocol::HarnessAvailability;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

pub struct ProbeContext {
    pub executable: PathBuf,
    pub environment: BTreeMap<OsString, OsString>,
    pub timeout: Duration,
}

impl Default for ProbeContext {
    fn default() -> Self {
        Self {
            executable: PathBuf::new(),
            environment: BTreeMap::new(),
            timeout: Duration::from_secs(10),
        }
    }
}

pub(super) struct ProbeDirectory(pub(super) PathBuf);
impl ProbeDirectory {
    pub(super) fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("formless-probe-{}", uuid::Uuid::new_v4()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(Self(path))
    }
}
impl Drop for ProbeDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct ProbeGroup(u32);
impl Drop for ProbeGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill")
                .args(["/PID", &self.0.to_string(), "/T", "/F"])
                .output();
        }
    }
}

fn executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    meta.is_file()
}

fn kind_name(kind: HarnessKind) -> &'static str {
    match kind {
        HarnessKind::Claude => "claude",
        HarnessKind::Codex => "codex",
        HarnessKind::Pi => "pi",
        HarnessKind::OpenCode => "opencode",
        HarnessKind::Antigravity => "antigravity",
        HarnessKind::Acp => "acp",
        HarnessKind::Muse => "muse",
    }
}
fn lookup(kind: HarnessKind, environment: &BTreeMap<OsString, OsString>) -> Option<PathBuf> {
    let override_key = format!(
        "FORMLESS_TOPIC_{}_EXE",
        kind_name(kind).to_ascii_uppercase()
    );
    if let Some(value) = environment.get(std::ffi::OsStr::new(&override_key)) {
        let path = PathBuf::from(value);
        return (path.is_absolute() && executable_file(&path)).then_some(path);
    }
    let path = environment
        .get(std::ffi::OsStr::new("PATH"))
        .cloned()
        .unwrap_or_default();
    for directory in std::env::split_paths(&path) {
        if !directory.is_absolute() {
            continue;
        }
        let candidate = directory.join(kind_name(kind));
        if executable_file(&candidate) {
            return Some(candidate);
        }
        #[cfg(windows)]
        for extension in ["exe", "cmd", "bat"] {
            let candidate = candidate.with_extension(extension);
            if executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}
pub(super) fn availability(kind: HarnessKind, executable: Option<&Path>) -> HarnessAvailability {
    HarnessAvailability {
        kind,
        acp_agent: None,
        installed: executable.is_some_and(executable_file),
        signed_in: None,
        version: None,
        detail: None,
    }
}
fn timed_out(mut value: HarnessAvailability) -> HarnessAvailability {
    value.signed_in = None;
    value.detail = Some("probe timed out".into());
    value
}
fn command(ctx: &ProbeContext, args: &[&str], cwd: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(&ctx.executable);
    cmd.args(args)
        .envs(&ctx.environment)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    cmd
}
async fn output(ctx: &ProbeContext, args: &[&str], cwd: &Path) -> Result<(bool, Vec<u8>, Vec<u8>)> {
    let mut child = command(ctx, args, cwd).spawn()?;
    let _group = ProbeGroup(child.id().context("probe has no pid")?);
    drop(child.stdin.take());
    let mut stdout = child
        .stdout
        .take()
        .context("probe stdout missing")?
        .take(65536);
    let mut stderr = child
        .stderr
        .take()
        .context("probe stderr missing")?
        .take(65536);
    let read_out = async move {
        let mut data = Vec::new();
        stdout.read_to_end(&mut data).await?;
        Ok::<_, std::io::Error>(data)
    };
    let read_err = async move {
        let mut data = Vec::new();
        stderr.read_to_end(&mut data).await?;
        Ok::<_, std::io::Error>(data)
    };
    let (status, out, err) = tokio::try_join!(child.wait(), read_out, read_err)?;
    Ok((status.success(), out, err))
}
fn version(value: &(bool, Vec<u8>, Vec<u8>)) -> Option<String> {
    if !value.0 {
        return None;
    }
    let value = String::from_utf8_lossy(&value.1)
        .lines()
        .next()?
        .chars()
        .take(160)
        .collect::<String>();
    let redacted = crate::tools::redact(&serde_json::Value::String(value));
    redacted
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

pub async fn claude_probe(ctx: &ProbeContext) -> HarnessAvailability {
    official_probe(HarnessKind::Claude, ctx).await
}
pub async fn codex_probe(ctx: &ProbeContext) -> HarnessAvailability {
    official_probe(HarnessKind::Codex, ctx).await
}
async fn fallback_probe(kind: HarnessKind, ctx: &ProbeContext) -> HarnessAvailability {
    let mut value = availability(kind, Some(&ctx.executable));
    if !value.installed {
        return value;
    }
    let probe = async {
        let cwd = ProbeDirectory::new()?;
        Ok::<_, anyhow::Error>(version(&output(ctx, &["--version"], &cwd.0).await?))
    };
    match tokio::time::timeout(ctx.timeout.min(Duration::from_secs(10)), probe).await {
        Ok(Ok(Some(version))) => value.version = Some(version),
        Ok(Ok(None)) | Ok(Err(_)) => value.detail = Some("version probe failed".into()),
        Err(_) => return timed_out(value),
    }
    value
}
async fn official_probe(kind: HarnessKind, ctx: &ProbeContext) -> HarnessAvailability {
    let base = availability(kind, Some(&ctx.executable));
    if !base.installed {
        return base;
    }
    let probe = async {
        let cwd = ProbeDirectory::new()?;
        let auth_args: &[&str] = if kind == HarnessKind::Claude {
            &["auth", "status"]
        } else {
            &["login", "status"]
        };
        let (ver, auth) = tokio::join!(
            output(ctx, &["--version"], &cwd.0),
            output(ctx, auth_args, &cwd.0)
        );
        let mut value = base.clone();
        value.version = ver.as_ref().ok().and_then(version);
        if let Ok((success, out, err)) = auth {
            if kind == HarnessKind::Claude {
                value.signed_in = serde_json::from_slice::<serde_json::Value>(&out)
                    .ok()
                    .and_then(|json| json["loggedIn"].as_bool());
            } else if success {
                value.signed_in = Some(true);
            } else if String::from_utf8_lossy(&out)
                .to_ascii_lowercase()
                .contains("not logged in")
                || String::from_utf8_lossy(&err)
                    .to_ascii_lowercase()
                    .contains("not logged in")
            {
                value.signed_in = Some(false);
            }
        }
        if value.signed_in.is_none() {
            value.detail = Some("auth status unavailable".into());
        }
        Ok::<_, anyhow::Error>(value)
    };
    match tokio::time::timeout(ctx.timeout.min(Duration::from_secs(10)), probe).await {
        Ok(Ok(value)) => value,
        Ok(Err(_)) => {
            let mut value = base;
            value.detail = Some("probe failed".into());
            value
        }
        Err(_) => timed_out(base),
    }
}

pub async fn blocking_probe(
    kind: HarnessKind,
    executable: Option<PathBuf>,
    environment: BTreeMap<OsString, OsString>,
    probe: fn(Option<PathBuf>, &BTreeMap<OsString, OsString>) -> HarnessAvailability,
) -> HarnessAvailability {
    let base = availability(kind, executable.as_deref());
    match tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || probe(executable, &environment)),
    )
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(_)) => {
            let mut value = base;
            value.detail = Some("probe failed".into());
            value
        }
        Err(_) => timed_out(base),
    }
}

async fn dispatch(
    kind: HarnessKind,
    ctx: &ProbeContext,
    agent: Option<&str>,
) -> HarnessAvailability {
    match kind {
        HarnessKind::Claude => claude_probe(ctx).await,
        HarnessKind::Codex => codex_probe(ctx).await,
        HarnessKind::Pi => fallback_probe(kind, ctx).await,
        HarnessKind::OpenCode => fallback_probe(kind, ctx).await,
        HarnessKind::Antigravity => fallback_probe(kind, ctx).await,
        HarnessKind::Acp => {
            let mut value = availability(kind, Some(&ctx.executable));
            value.acp_agent = agent.map(str::to_owned);
            value
        }
        HarnessKind::Muse => super::muse::probe(ctx).await,
    }
}
async fn probe_environment(
    kind: HarnessKind,
    agent: Option<&str>,
    environment: &BTreeMap<OsString, OsString>,
    timeout: Duration,
) -> HarnessAvailability {
    let mut base = availability(kind, None);
    base.acp_agent = agent.map(str::to_owned);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let lookup_environment = environment.clone();
    let executable = match tokio::time::timeout_at(
        deadline,
        tokio::task::spawn_blocking(move || lookup(kind, &lookup_environment)),
    )
    .await
    {
        Ok(Ok(path)) => path,
        Ok(Err(_)) => None,
        Err(_) => return timed_out(base),
    };
    let Some(executable) = executable else {
        base.detail = Some(if kind == HarnessKind::Acp {
            "ACP agent executable is not configured".into()
        } else {
            format!(
                "CLI executable not found; install {} or configure FORMLESS_TOPIC_{}_EXE",
                kind_name(kind),
                kind_name(kind).to_ascii_uppercase()
            )
        });
        return base;
    };
    base.installed = executable_file(&executable);
    let ctx = ProbeContext {
        executable,
        environment: environment.clone(),
        timeout,
    };
    match tokio::time::timeout_at(deadline, dispatch(kind, &ctx, agent)).await {
        Ok(value) => value,
        Err(_) => timed_out(base),
    }
}
pub async fn harness_availability(kind: HarnessKind, agent: Option<&str>) -> HarnessAvailability {
    probe_environment(
        kind,
        agent,
        &std::env::vars_os().collect(),
        Duration::from_secs(10),
    )
    .await
}
async fn list_environment(
    environment: BTreeMap<OsString, OsString>,
    timeout: Duration,
) -> Vec<HarnessAvailability> {
    futures_util::future::join_all(
        [
            HarnessKind::Claude,
            HarnessKind::Codex,
            HarnessKind::Pi,
            HarnessKind::OpenCode,
            HarnessKind::Antigravity,
            HarnessKind::Muse,
        ]
        .into_iter()
        .map(|kind| probe_environment(kind, None, &environment, timeout)),
    )
    .await
}
pub async fn list_harnesses() -> Vec<HarnessAvailability> {
    list_environment(std::env::vars_os().collect(), Duration::from_secs(10)).await
}

async fn acp_response(
    stdin: &mut tokio::process::ChildStdin,
    stdout: &mut BufReader<tokio::io::Take<tokio::process::ChildStdout>>,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let request = serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
    stdin.write_all(format!("{request}\n").as_bytes()).await?;
    loop {
        let mut line = String::new();
        if stdout.read_line(&mut line).await? == 0 {
            bail!("ACP probe ended");
        }
        let response: serde_json::Value = serde_json::from_str(&line)?;
        if response["id"].as_u64() == Some(id) {
            return Ok(response);
        }
    }
}
pub async fn acp_probe(
    ctx: &ProbeContext,
    version_args: Option<&[&str]>,
    acp_args: &[&str],
    auth_error_codes: &[i64],
) -> HarnessAvailability {
    let base = availability(HarnessKind::Acp, Some(&ctx.executable));
    if !base.installed {
        return base;
    }
    let probe = async {
        let cwd = ProbeDirectory::new()?;
        let mut value = base.clone();
        if let Some(args) = version_args {
            value.version = output(ctx, args, &cwd.0)
                .await
                .as_ref()
                .ok()
                .and_then(version);
        }
        let mut cmd = command(ctx, acp_args, &cwd.0);
        cmd.stderr(Stdio::null());
        let mut child = cmd.spawn()?;
        let _group = ProbeGroup(child.id().context("ACP probe has no pid")?);
        let mut stdin = child.stdin.take().context("ACP stdin missing")?;
        let mut stdout = BufReader::new(
            child
                .stdout
                .take()
                .context("ACP stdout missing")?
                .take(65536),
        );
        for (id, method, params) in [
            (
                1,
                "initialize",
                serde_json::json!({"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"formless-readiness","version":env!("CARGO_PKG_VERSION")}}),
            ),
            (
                2,
                "session/new",
                serde_json::json!({"cwd":cwd.0,"mcpServers":[]}),
            ),
        ] {
            let response = acp_response(&mut stdin, &mut stdout, id, method, params).await?;
            if let Some(error) = response.get("error") {
                if error["code"]
                    .as_i64()
                    .is_some_and(|code| auth_error_codes.contains(&code))
                {
                    value.signed_in = Some(false);
                } else {
                    value.detail = Some("ACP readiness could not be determined".into());
                }
                return Ok::<_, anyhow::Error>(value);
            }
            if response.get("result").is_none() {
                bail!("missing ACP result");
            }
        }
        value.signed_in = Some(true);
        Ok(value)
    };
    match tokio::time::timeout(ctx.timeout.min(Duration::from_secs(10)), probe).await {
        Ok(Ok(value)) => value,
        Ok(Err(_)) => {
            let mut value = base;
            value.detail = Some("ACP probe failed".into());
            value
        }
        Err(_) => timed_out(base),
    }
}

#[cfg(all(test, unix))]
#[path = "local_harness_probe_tests.rs"]
mod tests;
