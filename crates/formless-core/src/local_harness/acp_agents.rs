//! Named ACP profiles; adding a harness only requires another registry entry.

mod availability;

pub use availability::availability;
pub(crate) use availability::availability_with_executable;

use std::ffi::OsString;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::acp::{self, AgentSpec, PermissionHandler};
use super::{HarnessRequest, HarnessResult, StepAccess};

fn check_identity(initialized: &Value, expected: Option<&str>) -> Result<()> {
    if initialized.get("protocolVersion").and_then(Value::as_u64) != Some(1) {
        bail!("The agent does not speak ACP v1.");
    }
    if let Some(expected) = expected {
        if initialized
            .pointer("/agentInfo/name")
            .and_then(Value::as_str)
            != Some(expected)
        {
            bail!("Expected ACP agentInfo.name `{expected}`.");
        }
    }
    Ok(())
}

macro_rules! agents {
    ($($id:literal => $symbol:ident {
        name: $name:literal, program: $program:literal, identity: $identity:expr,
        args: [$($arg:literal),*], review_args: [$($review_arg:literal),*],
        modes: $modes:expr, fail_closed: $strict:literal, write_modes: $writes:expr,
        auth_codes: $codes:expr, session_auth: $auth:literal, sign_in: $sign_in:literal
    }),* $(,)?) => {
        $(pub const $symbol: AgentSpec = AgentSpec {
            name: $name,
            program: $program,
            args: |request| {
                let mut args: Vec<OsString> = vec![$($arg.into()),*];
                if request.access == StepAccess::ReadOnly {
                    let review_args: &[&str] = &[$($review_arg),*];
                    args.extend(review_args.iter().map(|arg| OsString::from(*arg)));
                }
                args
            },
            check_agent: |initialized| check_identity(initialized, $identity),
            not_signed_in: concat!($sign_in, " Formless never handles credentials; configure them with the CLI itself, not in argv."),
            auth_error_codes: $codes,
            read_only_modes: $modes,
            read_only_fail_closed: $strict,
            write_modes: $writes,
            client_capabilities: acp::no_extra_capabilities,
            strip_env: &[],
            set_env: &[],
            bridge_tool: acp::no_bridge_tools,
            vendor_request: acp::no_vendor_requests,
        };)*

        fn session_requires_auth(id: &str) -> bool {
            match id {
                $($id => $auth,)*
                _ => false,
            }
        }

        pub fn agent_ids() -> &'static [&'static str] {
            &[$($id),*]
        }

        pub fn spec(id: &str) -> Option<&'static AgentSpec> {
            match id {
                $($id => Some(&$symbol),)*
                _ => None,
            }
        }
    };
}

agents! {
    "gemini" => GEMINI {
        name: "Gemini", program: "gemini", identity: Some("gemini-cli"),
        args: ["--acp"], review_args: ["--approval-mode", "plan"],
        modes: &["plan"], fail_closed: false, write_modes: &[], auth_codes: &[-32000],
        session_auth: true,
        sign_in: "Run `gemini` and sign in with Google, or set GEMINI_API_KEY."
    },
    "copilot" => COPILOT {
        name: "Copilot", program: "copilot", identity: Some("Copilot"),
        args: ["--acp", "--stdio"], review_args: ["--plan"],
        modes: &["plan"], fail_closed: false, write_modes: &[], auth_codes: &[-32000],
        session_auth: true,
        sign_in: "Run `copilot login` to sign in."
    },
    "cursor" => CURSOR {
        name: "Cursor", program: "cursor-agent", identity: None,
        args: ["acp"], review_args: ["--mode", "plan"],
        modes: &["plan", "ask"], fail_closed: false, write_modes: &[], auth_codes: &[-32000],
        session_auth: true,
        sign_in: "Run `cursor-agent login`, or set CURSOR_AUTH_TOKEN."
    },
    "droid" => DROID {
        name: "Droid", program: "droid", identity: Some("@factory/cli"),
        args: ["exec", "--output-format", "acp"], review_args: [],
        modes: &[], fail_closed: false, write_modes: &[], auth_codes: &[-32000],
        session_auth: true,
        sign_in: "Run `droid` and sign in, or set FACTORY_API_KEY."
    },
    "kiro" => KIRO {
        name: "Kiro", program: "kiro-cli", identity: Some("Kiro CLI Agent"),
        args: ["acp"], review_args: [],
        modes: &[], fail_closed: false, write_modes: &[], auth_codes: &[-32000],
        session_auth: true,
        sign_in: "Run `kiro-cli login`, or set KIRO_API_KEY."
    },
    "junie" => JUNIE {
        name: "Junie", program: "junie", identity: Some("@jetbrains/junie"),
        args: ["--acp=true"], review_args: [],
        modes: &["plan"], fail_closed: true, write_modes: &["default"], auth_codes: &[-32000],
        session_auth: false,
        sign_in: "Run `junie` and sign in with your Junie Account in the browser."
    },
    "qwen" => QWEN {
        name: "Qwen", program: "qwen", identity: Some("qwen-code"),
        args: ["--acp"], review_args: ["--approval-mode", "plan"],
        modes: &["plan"], fail_closed: false, write_modes: &[], auth_codes: &[-32000],
        session_auth: true,
        sign_in: "Run `qwen` and use /auth to sign in or configure a model, or set OPENAI_API_KEY."
    },
    "kimi" => KIMI {
        name: "Kimi", program: "kimi", identity: Some("Kimi Code CLI"),
        args: ["acp"], review_args: [],
        modes: &["plan"], fail_closed: false, write_modes: &[], auth_codes: &[-32000],
        session_auth: true,
        sign_in: "Run `kimi login` to sign in with Kimi Code."
    },
    "vibe" => VIBE {
        name: "Vibe", program: "vibe-acp", identity: Some("@mistralai/mistral-vibe"),
        args: [], review_args: [],
        modes: &[], fail_closed: false, write_modes: &[], auth_codes: &[-32000],
        session_auth: true,
        sign_in: "Run `vibe` and sign in through Mistral AI Studio, or set MISTRAL_API_KEY."
    },
    "auggie" => AUGGIE {
        name: "Auggie", program: "auggie", identity: Some("auggie"),
        args: ["--acp"], review_args: [],
        modes: &[], fail_closed: false, write_modes: &[], auth_codes: &[-32000],
        session_auth: false,
        sign_in: "Run `auggie auth login` to sign in."
    },
    "goose" => GOOSE {
        name: "Goose", program: "goose", identity: Some("goose"),
        args: ["acp"], review_args: [],
        modes: &[], fail_closed: false, write_modes: &[], auth_codes: &[-32000, -32603],
        session_auth: true,
        sign_in: "Run `goose configure` to configure a provider and credentials, including GOOSE_PROVIDER and its model."
    },
    "cline" => CLINE {
        name: "Cline", program: "cline", identity: Some("cline"),
        args: ["--acp"], review_args: ["--plan"],
        modes: &["plan"], fail_closed: false, write_modes: &[], auth_codes: &[-32000],
        session_auth: true,
        sign_in: "Run `cline auth` to sign in or configure your provider."
    },
    "devin" => DEVIN {
        name: "Devin", program: "devin", identity: Some("affogato"),
        args: ["acp"], review_args: [],
        modes: &["plan", "ask"], fail_closed: true, write_modes: &["accept-edits"], auth_codes: &[-32000],
        session_auth: false,
        sign_in: "Run `devin auth login` to sign in with your browser."
    },
}

fn require_spec(id: &str) -> Result<&'static AgentSpec> {
    spec(id).with_context(|| {
        format!(
            "Unknown ACP agent `{id}`; available agents: {}",
            agent_ids().join(", ")
        )
    })
}

pub fn run(
    id: &str,
    request: &HarnessRequest,
    handler: &mut dyn PermissionHandler,
    cancel: &CancellationToken,
) -> Result<HarnessResult> {
    acp::run(require_spec(id)?, request, handler, cancel)
}

pub fn run_request(
    request: &HarnessRequest,
    handler: &mut dyn PermissionHandler,
    cancel: &CancellationToken,
) -> Result<HarnessResult> {
    let id = request.acp_agent.as_deref().context("missing acp_agent")?;
    run(id, request, handler, cancel)
}

pub fn run_request_with_spawn(
    request: &HarnessRequest,
    handler: &mut dyn PermissionHandler,
    cancel: &CancellationToken,
    spawn: &mut acp::ProcessSpawner,
) -> Result<HarnessResult> {
    let id = request.acp_agent.as_deref().context("missing acp_agent")?;
    acp::run_with_spawn(require_spec(id)?, request, handler, cancel, spawn)
}

pub fn open_session(id: &str, request: &HarnessRequest) -> Result<String> {
    acp::open_session(require_spec(id)?, request)
}

#[cfg(test)]
mod tests;
