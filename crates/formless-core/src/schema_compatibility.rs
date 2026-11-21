//! Conservative structural compatibility for versioned Tool schemas.
//!
//! Inputs are contravariant (old accepted calls must remain accepted); outputs
//! are covariant (new responses must fit the old contract). Removing a declared
//! input property is additionally breaking even when open-object validation
//! would continue accepting that spelling. Unsupported constraints require an
//! acknowledgement whenever semantic changes prevent a structural proof.
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashSet};

pub const MAX_NODES: usize = 4096;
pub const MAX_DEPTH: usize = 32;
pub const MAX_CHANGES: usize = 128;
const MAX_SCALAR_BYTES: usize = 256 * 1024;
const MAX_POINTER_BYTES: usize = 1024;
const MAX_ENUM_VALUES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Compatible,
    Breaking,
    Inconclusive,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Input,
    Output,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Change {
    pub side: Side,
    /// RFC 6901 pointer within this side's schema; empty means the root.
    pub pointer: String,
    pub verdict: Verdict,
    pub reason: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CompatibilityReport {
    pub verdict: Verdict,
    pub changes: Vec<Change>,
    pub truncated: bool,
}
impl CompatibilityReport {
    pub fn requires_acknowledgement(&self) -> bool {
        self.verdict != Verdict::Compatible
    }
    pub fn is_breaking(&self) -> bool {
        self.verdict == Verdict::Breaking
    }
    pub fn is_inconclusive(&self) -> bool {
        self.verdict == Verdict::Inconclusive
    }
}
struct State {
    changes: Vec<Change>,
    truncated: bool,
    halted: bool,
}
impl State {
    fn change(&mut self, side: Side, pointer: &str, verdict: Verdict, reason: &str) {
        if self.halted {
            return;
        }
        if self.changes.len() >= MAX_CHANGES - 1 {
            self.truncated = true;
            self.halted = true;
            self.changes.push(Change {
                side,
                pointer: String::new(),
                verdict: Verdict::Inconclusive,
                reason:
                    "Schema change report limit reached; remaining changes require acknowledgement"
                        .into(),
            });
        } else {
            self.changes.push(Change {
                side,
                pointer: pointer.into(),
                verdict,
                reason: reason.into(),
            });
        }
    }
    fn child(&mut self, side: Side, pointer: &str, key: &str) -> Option<String> {
        let path = format!("{pointer}/{}", key.replace('~', "~0").replace('/', "~1"));
        if path.len() > MAX_POINTER_BYTES {
            self.change(
                side,
                pointer,
                Verdict::Inconclusive,
                "Schema pointer limit reached",
            );
            self.truncated = true;
            self.halted = true;
            None
        } else {
            Some(path)
        }
    }
    fn report(self) -> CompatibilityReport {
        let verdict = if self
            .changes
            .iter()
            .any(|change| change.verdict == Verdict::Breaking)
        {
            Verdict::Breaking
        } else if self.truncated
            || self
                .changes
                .iter()
                .any(|change| change.verdict == Verdict::Inconclusive)
        {
            Verdict::Inconclusive
        } else {
            Verdict::Compatible
        };
        CompatibilityReport {
            verdict,
            changes: self.changes,
            truncated: self.truncated,
        }
    }
}
#[derive(Default)]
struct Budget {
    nodes: usize,
    bytes: usize,
}
fn bounded(value: &Value, depth: usize, budget: &mut Budget) -> bool {
    budget.nodes += 1;
    if depth > MAX_DEPTH || budget.nodes > MAX_NODES {
        return false;
    }
    match value {
        Value::String(text) => budget.bytes = budget.bytes.saturating_add(text.len()),
        Value::Array(values) => {
            for value in values {
                if !bounded(value, depth + 1, budget) {
                    return false;
                }
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                budget.bytes = budget.bytes.saturating_add(key.len());
                if budget.bytes > MAX_SCALAR_BYTES || !bounded(value, depth + 1, budget) {
                    return false;
                }
            }
        }
        _ => {}
    }
    budget.bytes <= MAX_SCALAR_BYTES
}
fn annotation(key: &str) -> bool {
    matches!(
        key,
        "title"
            | "description"
            | "default"
            | "examples"
            | "$comment"
            | "deprecated"
            | "readOnly"
            | "writeOnly"
    )
}
// Stable metadata does not block the structural proof. A dialect change or
// unknown dialect can change the meaning of assertions. Draft 4 also lacks
// some assertions understood by this comparator, such as const.
fn stable_schema_metadata(key: &str, sub: &Map<String, Value>, sup: &Map<String, Value>) -> bool {
    let Some(value) = sub.get(key).and_then(Value::as_str) else {
        return false;
    };
    if sup.get(key).and_then(Value::as_str) != Some(value) {
        return false;
    }
    match key {
        "$id" | "$anchor" => true,
        "$schema" => matches!(
            jsonschema::Draft::from_schema_uri(value),
            jsonschema::Draft::Draft6
                | jsonschema::Draft::Draft7
                | jsonschema::Draft::Draft201909
                | jsonschema::Draft::Draft202012
        ),
        _ => false,
    }
}
fn schema_map(key: &str) -> bool {
    matches!(
        key,
        "properties" | "patternProperties" | "$defs" | "definitions" | "dependentSchemas"
    )
}
fn schema_array(key: &str) -> bool {
    matches!(key, "allOf" | "anyOf" | "oneOf" | "prefixItems")
}
fn single_schema(key: &str) -> bool {
    matches!(
        key,
        "items"
            | "additionalProperties"
            | "additionalItems"
            | "unevaluatedProperties"
            | "unevaluatedItems"
            | "contains"
            | "propertyNames"
            | "not"
            | "if"
            | "then"
            | "else"
    )
}
/// Strip annotations ONLY at schema positions. Objects in enum/const remain
/// literal data, including keys named description/default/properties.
fn semantic_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: BTreeSet<_> = a
                .keys()
                .chain(b.keys())
                .filter(|key| !annotation(key))
                .collect();
            keys.into_iter().all(|key| match (a.get(key), b.get(key)) {
                (Some(a), Some(b)) if schema_map(key) => match (a.as_object(), b.as_object()) {
                    (Some(a), Some(b)) => {
                        a.len() == b.len()
                            && a.iter().all(|(key, value)| {
                                b.get(key).is_some_and(|other| semantic_eq(value, other))
                            })
                    }
                    _ => a == b,
                },
                (Some(a), Some(b)) if schema_array(key) => match (a.as_array(), b.as_array()) {
                    (Some(a), Some(b)) => {
                        a.len() == b.len() && a.iter().zip(b).all(|(a, b)| semantic_eq(a, b))
                    }
                    _ => a == b,
                },
                // Draft-07 tuple items are schema positions too. Normalize
                // only annotations here; changed tuple assertions still reach
                // the conservative unsupported-shape classification below.
                (Some(Value::Array(a)), Some(Value::Array(b))) if key == "items" => {
                    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| semantic_eq(a, b))
                }
                (Some(a), Some(b)) if single_schema(key) => semantic_eq(a, b),
                (Some(a), Some(b)) => a == b,
                _ => false,
            })
        }
        _ => a == b,
    }
}
const NULL: u8 = 1;
const BOOL: u8 = 2;
const OBJECT: u8 = 4;
const ARRAY: u8 = 8;
const STRING: u8 = 16;
const INTEGER: u8 = 32;
const REAL: u8 = 64;
const ANY: u8 = 127;
fn type_bit(name: &str) -> Option<u8> {
    Some(match name {
        "null" => NULL,
        "boolean" => BOOL,
        "object" => OBJECT,
        "array" => ARRAY,
        "string" => STRING,
        "integer" => INTEGER,
        "number" => INTEGER | REAL,
        _ => return None,
    })
}
fn types(schema: &Map<String, Value>) -> Option<u8> {
    match schema.get("type") {
        None => Some(ANY),
        Some(Value::String(name)) => type_bit(name),
        Some(Value::Array(names)) if !names.is_empty() => {
            let mut bits = 0;
            let mut seen = HashSet::new();
            for name in names {
                let name = name.as_str()?;
                if !seen.insert(name) {
                    return None;
                }
                bits |= type_bit(name)?;
            }
            Some(bits)
        }
        _ => None,
    }
}
fn value_type(value: &Value) -> u8 {
    match value {
        Value::Null => NULL,
        Value::Bool(_) => BOOL,
        Value::String(_) => STRING,
        Value::Array(_) => ARRAY,
        Value::Object(_) => OBJECT,
        Value::Number(number) => {
            if number.is_i64()
                || number.is_u64()
                || number.as_f64().is_some_and(|number| number.fract() == 0.0)
            {
                INTEGER
            } else {
                REAL
            }
        }
    }
}
fn finite(schema: &Map<String, Value>) -> Result<Option<Vec<Value>>, ()> {
    let enumeration = match schema.get("enum") {
        None => None,
        Some(Value::Array(values)) if !values.is_empty() && values.len() <= MAX_ENUM_VALUES => {
            Some(values.clone())
        }
        _ => return Err(()),
    };
    if let Some(value) = schema.get("const") {
        return Ok(Some(
            if enumeration
                .as_ref()
                .is_none_or(|values| values.contains(value))
            {
                vec![value.clone()]
            } else {
                Vec::new()
            },
        ));
    }
    if enumeration.is_some() {
        return Ok(enumeration);
    }
    let bits = types(schema).ok_or(())?;
    if bits & !(NULL | BOOL) == 0 {
        let mut values = Vec::new();
        if bits & NULL != 0 {
            values.push(Value::Null);
        }
        if bits & BOOL != 0 {
            values.extend([Value::Bool(false), Value::Bool(true)]);
        }
        return Ok(Some(values));
    }
    Ok(None)
}
fn required(schema: &Map<String, Value>) -> Option<BTreeSet<&str>> {
    match schema.get("required") {
        None => Some(BTreeSet::new()),
        Some(Value::Array(names)) => {
            let mut result = BTreeSet::new();
            for name in names {
                if !result.insert(name.as_str()?) {
                    return None;
                }
            }
            Some(result)
        }
        _ => None,
    }
}
fn properties(schema: &Map<String, Value>) -> Option<&Map<String, Value>> {
    match schema.get("properties") {
        None => Some(empty_map()),
        Some(Value::Object(map)) => Some(map),
        _ => None,
    }
}
fn empty_map() -> &'static Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(Map::new)
}
fn is_schema(value: &Value) -> bool {
    value.is_boolean() || value.is_object()
}
fn unconstrained(value: &Value) -> bool {
    value == &Value::Bool(true)
        || value
            .as_object()
            .is_some_and(|map| map.keys().all(|key| annotation(key)))
}
static TRUE: Value = Value::Bool(true);
static FALSE: Value = Value::Bool(false);
fn extras(schema: &Map<String, Value>) -> Option<&Value> {
    let value = schema.get("additionalProperties").unwrap_or(&TRUE);
    is_schema(value).then_some(value)
}

fn subset(sub: &Value, sup: &Value, side: Side, pointer: &str, state: &mut State) {
    if state.halted || semantic_eq(sub, sup) {
        return;
    }
    let start = state.changes.len();
    if !is_schema(sub) || !is_schema(sup) {
        state.change(
            side,
            pointer,
            Verdict::Inconclusive,
            "Expected a Boolean or object schema",
        );
        return;
    }
    // Removed input fields are an explicit public Tool contract change even if
    // JSON Schema's open-object semantics continue admitting the raw spelling.
    if side == Side::Input {
        if let Some(old) = sub.as_object().and_then(properties) {
            let new = sup.as_object().and_then(properties);
            for name in old
                .keys()
                .filter(|name| new.is_none_or(|new| !new.contains_key(*name)))
            {
                let Some(path) = state
                    .child(side, pointer, "properties")
                    .and_then(|path| state.child(side, &path, name))
                else {
                    return;
                };
                state.change(
                    side,
                    &path,
                    Verdict::Breaking,
                    "Declared input property was removed or renamed",
                );
            }
        }
    }
    if sub == &FALSE || unconstrained(sup) {
        if state.changes.len() == start {
            state.change(
                side,
                pointer,
                Verdict::Compatible,
                "Accepted values remain within the receiving contract",
            );
        }
        return;
    }
    if sup == &FALSE {
        state.change(
            side,
            pointer,
            Verdict::Breaking,
            "Receiving schema rejects all values",
        );
        return;
    }
    let empty = empty_map();
    let sub = sub.as_object().unwrap_or(empty);
    let sup = sup.as_object().unwrap_or(empty);
    let (Some(mut sub_types), Some(sup_types)) = (types(sub), types(sup)) else {
        state.change(
            side,
            pointer,
            Verdict::Inconclusive,
            "Unsupported or invalid type declaration",
        );
        return;
    };
    let (Ok(sub_finite), Ok(sup_finite)) = (finite(sub), finite(sup)) else {
        state.change(
            side,
            pointer,
            Verdict::Inconclusive,
            "Invalid or excessive enum/const declaration",
        );
        return;
    };
    if let Some(values) = &sub_finite {
        sub_types = values
            .iter()
            .filter(|value| value_type(value) & sub_types != 0)
            .fold(0, |bits, value| bits | value_type(value));
    }
    if sub_types & !sup_types != 0 {
        let path = state
            .child(side, pointer, "type")
            .unwrap_or_else(|| pointer.into());
        state.change(
            side,
            &path,
            Verdict::Breaking,
            if side == Side::Input {
                "Accepted input types were narrowed"
            } else {
                "Possible output types were widened"
            },
        );
    }
    if let Some(allowed) = &sup_finite {
        let contained = sub_finite.as_ref().is_some_and(|values| {
            values
                .iter()
                .filter(|value| value_type(value) & sub_types != 0)
                .all(|value| allowed.contains(value))
        });
        if !contained {
            let path = state
                .child(
                    side,
                    pointer,
                    if sup.contains_key("const") {
                        "const"
                    } else {
                        "enum"
                    },
                )
                .unwrap_or_else(|| pointer.into());
            state.change(
                side,
                &path,
                Verdict::Breaking,
                if side == Side::Input {
                    "Accepted input enum/const values were narrowed"
                } else {
                    "Possible output enum/const values were widened"
                },
            );
        }
    }
    let unsupported: BTreeSet<_> = sub
        .keys()
        .chain(sup.keys())
        .filter(|key| {
            !annotation(key)
                && !stable_schema_metadata(key, sub, sup)
                && !matches!(
                    key.as_str(),
                    "type"
                        | "enum"
                        | "const"
                        | "properties"
                        | "required"
                        | "additionalProperties"
                        | "items"
                )
        })
        .collect();
    for key in unsupported {
        let path = state
            .child(side, pointer, key)
            .unwrap_or_else(|| pointer.into());
        state.change(
            side,
            &path,
            Verdict::Inconclusive,
            "Complex or unknown constraint prevents a structural compatibility proof",
        );
    }
    if state.halted {
        return;
    }
    if sub_types & OBJECT != 0 && sup_types & OBJECT != 0 {
        let (
            Some(sub_props),
            Some(sup_props),
            Some(sub_required),
            Some(sup_required),
            Some(sub_extra),
            Some(sup_extra),
        ) = (
            properties(sub),
            properties(sup),
            required(sub),
            required(sup),
            extras(sub),
            extras(sup),
        )
        else {
            state.change(
                side,
                pointer,
                Verdict::Inconclusive,
                "Invalid object properties, required list or additionalProperties schema",
            );
            return;
        };
        for name in sup_required.difference(&sub_required) {
            let index = sup
                .get("required")
                .and_then(Value::as_array)
                .and_then(|values| {
                    values
                        .iter()
                        .position(|value| value.as_str() == Some(*name))
                })
                .expect("validated required field");
            let path = state
                .child(side, pointer, "required")
                .and_then(|path| state.child(side, &path, &index.to_string()))
                .unwrap_or_else(|| pointer.into());
            state.change(
                side,
                &path,
                Verdict::Breaking,
                if side == Side::Input {
                    "Input field became required"
                } else {
                    "Output field is no longer guaranteed"
                },
            );
        }
        let names: BTreeSet<_> = sub_props.keys().chain(sup_props.keys()).collect();
        for name in names {
            if state.halted {
                return;
            }
            if side == Side::Input && sub_props.contains_key(name) && !sup_props.contains_key(name)
            {
                // The declaration removal above already records this path as breaking.
                continue;
            }
            let sub_property = sub_props.get(name).unwrap_or(sub_extra);
            let sup_property = sup_props.get(name).unwrap_or(sup_extra);
            let Some(path) = state
                .child(side, pointer, "properties")
                .and_then(|path| state.child(side, &path, name))
            else {
                return;
            };
            subset(sub_property, sup_property, side, &path, state);
        }
        if !semantic_eq(sub_extra, sup_extra) {
            let Some(path) = state.child(side, pointer, "additionalProperties") else {
                return;
            };
            subset(sub_extra, sup_extra, side, &path, state);
        }
    }
    if sub_types & ARRAY != 0 && sup_types & ARRAY != 0 {
        let sub_items = sub.get("items").unwrap_or(&TRUE);
        let sup_items = sup.get("items").unwrap_or(&TRUE);
        let Some(path) = state.child(side, pointer, "items") else {
            return;
        };
        subset(sub_items, sup_items, side, &path, state);
    }
    if state.changes.len() == start {
        state.change(
            side,
            pointer,
            Verdict::Compatible,
            "Structural change preserves accepted input or guaranteed output values",
        );
    }
}

/// Argument order is OLD INPUT, NEW INPUT, OLD OUTPUT, NEW OUTPUT. Unknown
/// constraints and exhausted bounds require the same explicit acknowledgement
/// path as known breaking changes; they are never silently called compatible.
pub fn classify(
    old_input: &Value,
    new_input: &Value,
    old_output: &Value,
    new_output: &Value,
) -> CompatibilityReport {
    let mut state = State {
        changes: Vec::new(),
        truncated: false,
        halted: false,
    };
    let mut budget = Budget::default();
    for (side, schema) in [
        (Side::Input, old_input),
        (Side::Input, new_input),
        (Side::Output, old_output),
        (Side::Output, new_output),
    ] {
        if !bounded(schema, 0, &mut budget) {
            state.change(
                side,
                "",
                Verdict::Inconclusive,
                "Schema node, depth or scalar-byte budget exceeded",
            );
            state.truncated = true;
            return state.report();
        }
    }
    subset(old_input, new_input, Side::Input, "", &mut state);
    subset(new_output, old_output, Side::Output, "", &mut state);
    state.report()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn input(old: Value, new: Value) -> CompatibilityReport {
        classify(&old, &new, &json!({}), &json!({}))
    }
    fn output(old: Value, new: Value) -> CompatibilityReport {
        classify(&json!({}), &json!({}), &old, &new)
    }
    fn object(properties: Value, required: Value) -> Value {
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
    }

    #[test]
    fn closed_input_optional_required_removed_and_renamed_matrix() {
        let old = object(json!({"a":{"type":"string"}}), json!([]));
        let optional = object(
            json!({"a":{"type":"string"},"b":{"type":"integer"}}),
            json!([]),
        );
        assert_eq!(
            input(old.clone(), optional.clone()).verdict,
            Verdict::Compatible
        );
        let required = object(
            json!({"a":{"type":"string"},"b":{"type":"integer"}}),
            json!(["b"]),
        );
        assert!(input(old.clone(), required).is_breaking());
        assert!(input(old.clone(), object(json!({}), json!([]))).is_breaking());
        assert!(input(
            old.clone(),
            object(json!({"renamed":{"type":"string"}}), json!([]))
        )
        .is_breaking());
        let old_required = object(json!({"a":{"type":"string"}}), json!(["a"]));
        assert_eq!(input(old_required, old).verdict, Verdict::Compatible);
    }
    #[test]
    fn stable_draft_and_identifier_allow_a_structurally_compatible_edit() {
        let mut old = object(json!({"a":{"type":"string"}}), json!(["a"]));
        old["$schema"] = json!("http://json-schema.org/draft-07/schema#");
        old["$id"] = json!("https://example.invalid/tool");
        let mut optional = old.clone();
        optional["properties"]["b"] = json!({"type":"integer"});
        assert_eq!(
            input(old.clone(), optional.clone()).verdict,
            Verdict::Compatible
        );

        let mut changed = optional.clone();
        changed["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
        assert_eq!(input(old.clone(), changed).verdict, Verdict::Inconclusive);
        optional["$id"] = json!("https://example.invalid/different");
        assert_eq!(input(old, optional).verdict, Verdict::Inconclusive);

        let mut old = json!({"$schema":"https://example.invalid/dialect", "type":"string"});
        let mut widened = json!({
            "$schema": "https://example.invalid/dialect",
            "type": ["string", "null"]
        });
        assert_eq!(
            input(old.clone(), widened.clone()).verdict,
            Verdict::Inconclusive
        );
        old["$schema"] = json!("http://json-schema.org/draft-04/schema#");
        widened["$schema"] = old["$schema"].clone();
        assert_eq!(input(old, widened).verdict, Verdict::Inconclusive);
    }

    #[test]
    fn removing_a_closed_input_property_reports_its_path_once() {
        let old = object(json!({"a":{"type":"string"}}), json!([]));
        let new = object(json!({}), json!([]));
        let report = input(old, new);
        assert!(report.is_breaking());
        assert_eq!(report.changes.len(), 1, "{report:?}");
        assert_eq!(report.changes[0].pointer, "/properties/a");
    }
    #[test]
    fn output_required_optional_and_closed_extra_fields_mirror_input_variance() {
        let required = object(json!({"a":{"type":"string"}}), json!(["a"]));
        let optional = object(json!({"a":{"type":"string"}}), json!([]));
        assert!(output(required.clone(), optional.clone()).is_breaking());
        assert_eq!(
            output(optional.clone(), required.clone()).verdict,
            Verdict::Compatible
        );
        assert!(output(required.clone(), object(json!({}), json!([]))).is_breaking());
        assert_eq!(
            output(optional, object(json!({}), json!([]))).verdict,
            Verdict::Compatible
        );
        let added = object(
            json!({"a":{"type":"string"},"extra":{"type":"string"}}),
            json!(["a"]),
        );
        assert!(output(required, added.clone()).is_breaking());
        let open = json!({"type":"object","properties":{"a":{"type":"string"}},"required":["a"]});
        assert_eq!(output(open, added).verdict, Verdict::Compatible);
    }
    #[test]
    fn type_enum_const_and_integer_number_direction_matrix() {
        for (old, new) in [
            (json!({"type":"string"}), json!({"type":["string","null"]})),
            (json!({"type":"integer"}), json!({"type":"number"})),
            (
                json!({"type":"string","enum":["a"]}),
                json!({"type":"string","enum":["a","b"]}),
            ),
            (json!({"const":"a"}), json!({"enum":["a","b"]})),
        ] {
            assert_eq!(input(old.clone(), new.clone()).verdict, Verdict::Compatible);
            assert!(input(new.clone(), old.clone()).is_breaking());
            assert!(output(old.clone(), new.clone()).is_breaking());
            assert_eq!(output(new, old).verdict, Verdict::Compatible);
        }
        assert_eq!(
            input(json!({"type":"boolean"}), json!({"enum":[true,false]})).verdict,
            Verdict::Compatible
        );
        assert_eq!(
            input(json!({"type":"null"}), json!({"const":null})).verdict,
            Verdict::Compatible
        );
        assert!(input(json!({}), json!({"type":"object"})).is_breaking());
    }
    #[test]
    fn nested_objects_arrays_and_json_pointer_escaping_are_precise() {
        let old = object(
            json!({"a/b~c":{"type":"object","properties":{"items":{"type":"array","items":{"type":"number"}}},"additionalProperties":false}}),
            json!([]),
        );
        let new = object(
            json!({"a/b~c":{"type":"object","properties":{"items":{"type":"array","items":{"type":"integer"}}},"additionalProperties":false}}),
            json!([]),
        );
        let report = input(old.clone(), new.clone());
        assert!(report.is_breaking());
        assert!(report
            .changes
            .iter()
            .any(|change| change.pointer == "/properties/a~1b~0c/properties/items/items/type"));
        assert_eq!(output(old, new).verdict, Verdict::Compatible);
        let before = object(
            json!({"nested":object(json!({"field":{"type":"string"}}),json!([]))}),
            json!([]),
        );
        let after = object(
            json!({"nested":object(json!({"field":{"type":"string"}}),json!(["field"]))}),
            json!([]),
        );
        let report = input(before, after.clone());
        let required = report
            .changes
            .iter()
            .find(|change| change.reason == "Input field became required")
            .unwrap();
        assert_eq!(after.pointer(&required.pointer).unwrap(), "field");
    }
    #[test]
    fn open_objects_and_schema_valued_extras_are_not_falsely_compatible() {
        let old = json!({"type":"object"});
        let new = json!({"type":"object","properties":{"optional":{"type":"string"}}});
        assert!(input(old, new).requires_acknowledgement());
        let old = json!({"type":"object","additionalProperties":{"type":"string"}});
        let new = json!({"type":"object","properties":{"optional":{"type":["string","null"]}},"additionalProperties":{"type":"string"}});
        assert_eq!(input(old, new).verdict, Verdict::Compatible);
        assert!(input(
            json!({"type":"object"}),
            json!({"type":"object","additionalProperties":false})
        )
        .is_breaking());
        assert!(input(
            json!({"type":"object","properties":{"removed":true}}),
            json!({"type":"object"})
        )
        .is_breaking());
    }
    #[test]
    fn annotations_are_ignored_only_at_schema_positions_not_literal_enum_data() {
        let old = json!({"type":"object","description":"old","default":{"data":1},"properties":{"a":{"type":"string","title":"old"}}});
        let new = json!({"type":"object","description":"new","default":{"data":2},"properties":{"a":{"type":"string","title":"new"}}});
        let report = input(old, new);
        assert_eq!(report.verdict, Verdict::Compatible);
        assert!(report.changes.is_empty());
        assert!(input(
            json!({"const":{"description":"old"}}),
            json!({"const":{"description":"new"}})
        )
        .is_breaking());
        assert!(input(
            json!({"enum":[{"default":1}]}),
            json!({"enum":[{"default":2}]})
        )
        .is_breaking());
        let old = json!({"allOf":[{"type":"string","description":"old"}]});
        let new = json!({"allOf":[{"type":"string","description":"new"}]});
        assert_eq!(input(old, new).verdict, Verdict::Compatible);
    }
    #[test]
    fn legacy_tuple_items_ignore_only_schema_annotations() {
        let old = json!({"$schema":"http://json-schema.org/draft-07/schema#","type":"array","items":[{"type":"string","description":"old"},{"type":"object","properties":{"field":{"type":"integer","default":1}}}]});
        let new = json!({"$schema":"http://json-schema.org/draft-07/schema#","type":"array","items":[{"type":"string","description":"new"},{"type":"object","properties":{"field":{"type":"integer","default":2}}}]});
        let report = input(old.clone(), new);
        assert_eq!(report.verdict, Verdict::Compatible);
        assert!(report.changes.is_empty());
        let mut changed = old.clone();
        changed["items"][0]["type"] = json!("number");
        assert_eq!(input(old, changed).verdict, Verdict::Inconclusive);
        let literal_old = json!({"type":"array","items":[{"const":{"description":"old"}}]});
        let literal_new = json!({"type":"array","items":[{"const":{"description":"new"}}]});
        assert_eq!(
            input(literal_old, literal_new).verdict,
            Verdict::Inconclusive
        );
    }
    #[test]
    fn complex_changed_constraints_are_inconclusive_and_require_acknowledgement() {
        for (old, new) in [
            (
                json!({"type":"string","pattern":"a"}),
                json!({"type":"string","pattern":"b"}),
            ),
            (
                json!({"$ref":"#/$defs/a"}),
                json!({"$ref":"https://never-fetch.invalid/schema"}),
            ),
            (
                json!({"type":"integer","minimum":0}),
                json!({"type":"integer","minimum":-1}),
            ),
            (
                json!({"oneOf":[{"type":"string"}]}),
                json!({"oneOf":[{"type":"string"},{"type":"null"}]}),
            ),
            (
                json!({"type":"object","dependentRequired":{"a":["b"]}}),
                json!({"type":"object"}),
            ),
        ] {
            let report = input(old, new);
            assert_eq!(report.verdict, Verdict::Inconclusive);
            assert!(report.requires_acknowledgement());
            assert!(report.is_inconclusive());
        }
        let old = json!({"type":"string","pattern":"a"});
        let new = json!({"type":["string","null"],"pattern":"a"});
        assert!(input(old, new).requires_acknowledgement());
    }
    #[test]
    fn boolean_schemas_empty_enum_and_malformed_shapes_fail_conservatively() {
        assert_eq!(
            input(json!(false), json!(true)).verdict,
            Verdict::Compatible
        );
        assert!(input(json!(true), json!(false)).is_breaking());
        assert_eq!(
            output(json!(true), json!(false)).verdict,
            Verdict::Compatible
        );
        for bad in [
            json!({"type":[]}),
            json!({"type":"imaginary"}),
            json!({"enum":[]}),
            json!({"type":"object","required":"a"}),
            json!({"type":"object","properties":[]}),
            json!({"type":"object","additionalProperties":1}),
            json!(42),
        ] {
            assert!(input(json!({}), bad).requires_acknowledgement());
        }
    }
    #[test]
    fn node_depth_scalar_and_change_limits_are_bounded_and_fail_closed() {
        let mut deep = json!({"type":"string"});
        for _ in 0..MAX_DEPTH {
            deep = json!({"type":"array","items":deep});
        }
        let report = input(json!({}), deep);
        assert!(report.truncated && report.requires_acknowledgement());
        let many = json!({"enum":(0..MAX_NODES).collect::<Vec<_>>()});
        let report = input(json!({}), many);
        assert!(report.truncated && report.requires_acknowledgement());
        let large = json!({"description":"x".repeat(MAX_SCALAR_BYTES+1)});
        assert!(input(json!({}), large).truncated);
        let properties: Map<String, Value> = (0..MAX_CHANGES + 10)
            .map(|index| (format!("p{index}"), json!({"type":"string"})))
            .collect();
        let report = input(
            object(Value::Object(properties), json!([])),
            object(json!({}), json!([])),
        );
        assert!(report.truncated && report.is_breaking());
        assert!(report.changes.len() <= MAX_CHANGES);
    }
    #[test]
    fn full_report_is_deterministic_serializable_and_labels_both_sides() {
        let old_input = object(json!({"a":{"type":"string"}}), json!(["a"]));
        let new_input = object(json!({}), json!([]));
        let old_output = json!({"type":"string"});
        let new_output = json!({"type":["string","number"]});
        let report = classify(&old_input, &new_input, &old_output, &new_output);
        assert!(report
            .changes
            .iter()
            .any(|change| change.side == Side::Input));
        assert!(report
            .changes
            .iter()
            .any(|change| change.side == Side::Output));
        assert_eq!(
            report,
            classify(&old_input, &new_input, &old_output, &new_output)
        );
        let json = serde_json::to_value(report).unwrap();
        assert_eq!(json["verdict"], "breaking");
        assert!(json["changes"].is_array());
    }
}
