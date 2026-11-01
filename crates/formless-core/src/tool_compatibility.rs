//! Schema-change impact is derived from immutable version schemas and a bounded
//! current dependency inventory. Labels never confer write authority.
use crate::schema_compatibility::{self, Change, CompatibilityReport, Side, Verdict};
use anyhow::Result;
use serde::Serialize;
use sha2::{Digest, Sha256};

pub const MAX_DEPENDENTS: usize = 256;
pub const MAX_DEPENDENCY_TEXT: usize = 1024 * 1024;

/// Trusted call-site origin. Never deserialize this from agent arguments or
/// derive it from the stored/displayed `author` label.
#[derive(Clone, Copy, Debug)]
pub enum SchemaWritePolicy {
    Agent { allow_breaking: bool },
    User,
    ExternalDefinition,
}
#[derive(Clone, Debug, Serialize)]
pub struct SchemaDependent {
    pub kind: String,
    pub id: String,
    pub name: String,
    pub version: Option<i64>,
    pub live: bool,
    /// `declared`, `literal_reference`, or `possible_dynamic`.
    pub certainty: String,
    pub resolution: String,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct ToolSchemaReport {
    pub tool_id: String,
    pub base_version: i64,
    pub compatibility: CompatibilityReport,
    pub affected_dependents: Vec<SchemaDependent>,
    pub historical_references: Vec<SchemaDependent>,
    pub inventory_truncated: bool,
    pub candidate_digest: String,
    pub base_digest: String,
}
impl ToolSchemaReport {
    pub fn requires_acknowledgement(&self) -> bool {
        self.compatibility.requires_acknowledgement()
            && (self.inventory_truncated || self.affected_dependents.iter().any(|d| d.live))
    }
    pub fn context_digest(&self) -> Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }
    pub(crate) fn push(&mut self, dependent: SchemaDependent) {
        let target = if dependent.live {
            &mut self.affected_dependents
        } else {
            &mut self.historical_references
        };
        if target.len() >= MAX_DEPENDENTS {
            self.inventory_truncated = true;
        } else {
            target.push(dependent);
        }
    }
    pub(crate) fn sort(&mut self) {
        for values in [
            &mut self.affected_dependents,
            &mut self.historical_references,
        ] {
            values.sort_by(|a, b| {
                (&a.kind, &a.id, a.version, &a.certainty).cmp(&(
                    &b.kind,
                    &b.id,
                    b.version,
                    &b.certainty,
                ))
            });
        }
    }
}
#[derive(Debug)]
pub struct SchemaChangeBlocked(pub ToolSchemaReport);
impl std::fmt::Display for SchemaChangeBlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Schema change requires explicit allow_breaking=true before saving: {}",
            serde_json::to_string(&self.0)
                .unwrap_or_else(|_| "compatibility report unavailable".into())
        )
    }
}
impl std::error::Error for SchemaChangeBlocked {}

pub(crate) fn report(
    tool_id: &str,
    base_version: i64,
    old_input: &str,
    old_output: &str,
    new_input: &str,
    new_output: &str,
) -> ToolSchemaReport {
    let schemas = [old_input, new_input, old_output, new_output]
        .map(serde_json::from_str::<serde_json::Value>);
    let compatibility = match schemas {
        [Ok(old_input), Ok(new_input), Ok(old_output), Ok(new_output)] => {
            schema_compatibility::classify(&old_input, &new_input, &old_output, &new_output)
        }
        _ => CompatibilityReport {
            verdict: Verdict::Inconclusive,
            truncated: false,
            changes: vec![Change {
                side: Side::Input,
                pointer: String::new(),
                verdict: Verdict::Inconclusive,
                reason:
                    "Stored or proposed schema is not valid JSON; compatibility cannot be proved"
                        .into(),
            }],
        },
    };
    let digest = |a: &str, b: &str| {
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(a, b)).unwrap_or_default())
        )
    };
    ToolSchemaReport {
        tool_id: tool_id.into(),
        base_version,
        compatibility,
        affected_dependents: Vec::new(),
        historical_references: Vec::new(),
        inventory_truncated: false,
        candidate_digest: digest(new_input, new_output),
        base_digest: digest(old_input, old_output),
    }
}

/// The runtime resolves names to CURRENT versions, including calls made by a
/// parent whose own code version was captured earlier. Dynamic calls cannot be
/// proved unrelated and are reported as possible consumers, never exact edges.
pub(crate) fn code_reference(code: &str, target: &str) -> Option<&'static str> {
    if crate::tools::code_dependencies(code)
        .0
        .iter()
        .any(|name| name.eq_ignore_ascii_case(target))
    {
        return Some("literal_reference");
    }
    // Arbitrary JavaScript can retrieve host bindings through computed names,
    // aliases or constructors. No finite spelling scan proves absence. Unknown
    // callers are therefore possible consumers, including otherwise plain code;
    // this is an acknowledgement warning, never a claimed exact call edge.
    Some("possible_dynamic")
}
