//! Explicit catalog reads are suggestions, never validated provider support or writes.
use crate::session_edit::ModelSelection;
use serde_json::Value;
use std::collections::BTreeMap;
pub const PAGE_SIZE: usize = 12;
pub const MAX_ROWS: usize = 4096;
pub const MAX_CHOICES: usize = 2048;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub selector: String,
    pub sources: u8,
    search: String,
}
impl Choice {
    pub fn label(&self) -> String {
        let sources = [
            (1, "provider listed"),
            (2, "configured alias"),
            (4, "configured Role"),
            (8, "recent"),
        ]
        .into_iter()
        .filter_map(|(bit, name)| (self.sources & bit != 0).then_some(name))
        .collect::<Vec<_>>()
        .join(", ");
        format!("{} ({sources})", self.selector)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    pub choices: Vec<Choice>,
    pub provider_unavailable: bool,
}
fn valid(selector: &str) -> bool {
    ModelSelection {
        model: Some(selector.into()),
        reasoning_effort: None,
    }
    .validate(true)
    .is_ok()
}
impl Catalog {
    pub fn decode(value: Value) -> Result<Self, String> {
        let fail = || {
            "Catalog reply is unsupported or exceeds native bounds. Use manual entry or Formless TUI.".to_string()
        };
        let object = value.as_object().ok_or_else(fail)?;
        let models = object
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(fail)?;
        let aliases = object
            .get("aliases")
            .and_then(Value::as_object)
            .ok_or_else(fail)?;
        let roles = object
            .get("roles")
            .and_then(Value::as_array)
            .ok_or_else(fail)?;
        let recent = object
            .get("recent")
            .and_then(Value::as_array)
            .ok_or_else(fail)?;
        let unavailable = match object.get("error") {
            Some(Value::Null) => false,
            Some(Value::String(_)) => true,
            _ => return Err(fail()),
        };
        if unavailable && !models.is_empty() {
            return Err(fail());
        }
        if models
            .len()
            .saturating_add(aliases.len())
            .saturating_add(roles.len())
            .saturating_add(recent.len())
            > MAX_ROWS
        {
            return Err(fail());
        }
        let mut rows = BTreeMap::<String, u8>::new();
        let mut add = |selector: &str, source| -> Result<(), String> {
            // Check bytes before allocating a copy of untrusted identifiers.
            if selector.len() > crate::session_edit::MAX_MODEL_BYTES {
                return Err(fail());
            }
            // Deprecated/unsafe selectors are excluded whole, never rewritten.
            if !valid(selector) {
                return Ok(());
            }
            if !rows.contains_key(selector) && rows.len() >= MAX_CHOICES {
                return Err(fail());
            }
            *rows.entry(selector.into()).or_default() |= source;
            Ok(())
        };
        for v in models {
            add(v.as_str().ok_or_else(fail)?, 1)?;
        }
        for (alias, target) in aliases {
            if !target.is_string() {
                return Err(fail());
            }
            add(alias, 2)?;
        }
        for v in roles {
            let name = v.get("name").and_then(Value::as_str).ok_or_else(fail)?;
            if name.len() > crate::session_edit::MAX_MODEL_BYTES - 5 {
                return Err(fail());
            }
            add(&format!("role:{name}"), 4)?;
        }
        for v in recent {
            add(v.as_str().ok_or_else(fail)?, 8)?;
        }
        Ok(Self {
            provider_unavailable: unavailable,
            choices: rows
                .into_iter()
                .map(|(selector, sources)| Choice {
                    search: selector.to_lowercase(),
                    selector,
                    sources,
                })
                .collect(),
        })
    }
    pub fn matches(&self, query: &str) -> Vec<usize> {
        if query.len() > crate::session_edit::MAX_INPUT {
            return Vec::new();
        }
        let query = query.trim().to_lowercase();
        self.choices
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.search.contains(&query).then_some(i))
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn value() -> Value {
        json!({"models":["m","auto","bad\nmodel"],"aliases":{"m":"other","alias":"target"},"roles":[{"name":"reviewer"}],"recent":["m","role:reviewer"],"error":null,"meta":{"secret":"ignored"}})
    }
    #[test]
    fn exact_selectors_deduplicated_and_filtered_without_alias_resolution() {
        let c = Catalog::decode(value()).unwrap();
        assert_eq!(
            c.choices
                .iter()
                .map(|c| c.selector.as_str())
                .collect::<Vec<_>>(),
            vec!["alias", "m", "role:reviewer"]
        );
        assert_eq!(c.choices[1].sources, 11);
        assert_eq!(c.matches("REVIEW"), vec![2]);
        assert!(!c.provider_unavailable);
    }
    #[test]
    fn partial_provider_failure_never_exposes_raw_error() {
        let mut v = value();
        v["error"] = json!("https://secret@example/token");
        v["models"] = json!([]);
        let c = Catalog::decode(v).unwrap();
        assert!(c.provider_unavailable);
        assert!(!format!("{c:?}").contains("token"));
    }
    #[test]
    fn malformed_and_oversized_replies_rejected_whole() {
        for field in ["models", "aliases", "roles", "recent", "error"] {
            let mut v = value();
            v.as_object_mut().unwrap().remove(field);
            assert!(Catalog::decode(v).is_err());
        }
        let mut v = value();
        v["models"] = json!(["x".repeat(513)]);
        assert!(Catalog::decode(v).is_err());
        let mut v = value();
        v["models"] = json!((0..MAX_CHOICES + 1)
            .map(|i| format!("m{i}"))
            .collect::<Vec<_>>());
        assert!(Catalog::decode(v).is_err());
        let mut v = value();
        v["recent"] = json!(vec!["m"; MAX_ROWS + 1]);
        assert!(Catalog::decode(v).is_err());
    }
}
