//! Immutable, offline structured-output policy shared by completion callers.
//!
//! This module performs no requests and does not change legacy text completions.
//! Limits bound data size, reference expansion and regex work; they are not a
//! wall-clock deadline or an OS isolation boundary for the schema compiler.

use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

pub const MAX_SCHEMA_BYTES: usize = 64 * 1024;
pub const MAX_SCHEMA_NODES: usize = 4096;
pub const MAX_SCHEMA_DEPTH: usize = 48;
pub const MAX_EXPANDED_SCHEMA_NODES: usize = 16 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
pub const MAX_OUTPUT_NODES: usize = 16 * 1024;
pub const MAX_OUTPUT_DEPTH: usize = 64;
const MAX_REFERENCE_DEPTH: usize = 64;

/// A call-local option, independent of probe limits and request hooks.
#[derive(Clone, Default)]
pub struct CompletionOptions {
    schema: Option<Arc<JsonOutputSchema>>,
}

impl CompletionOptions {
    pub fn json_schema(schema: Value) -> Result<Self, StructuredOutputError> {
        Ok(Self {
            schema: Some(Arc::new(JsonOutputSchema::compile(schema)?)),
        })
    }

    pub fn output_schema(&self) -> Option<&JsonOutputSchema> {
        self.schema.as_deref()
    }
}

/// An unchanged schema together with its offline validator. Clones of call
/// options share this immutable policy, never mutable fallback/retry state.
pub struct JsonOutputSchema {
    schema: Value,
    validator: jsonschema::Validator,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructuredOutputError {
    SchemaLimit(&'static str),
    UnsupportedSchema(&'static str),
    InvalidSchema,
    OutputLimit(&'static str),
    InvalidJson,
    SchemaViolation,
    IncompleteResponse,
    RefusedResponse,
}

impl fmt::Display for StructuredOutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompleteResponse => f.write_str("structured output response was incomplete"),
            Self::RefusedResponse => f.write_str("structured output request was refused"),
            Self::SchemaLimit(limit) => write!(f, "structured output schema exceeds {limit} limit"),
            Self::UnsupportedSchema(reason) => {
                write!(f, "unsupported structured output schema: {reason}")
            }
            Self::InvalidSchema => f.write_str("invalid structured output JSON Schema"),
            Self::OutputLimit(limit) => write!(f, "structured output exceeds {limit} limit"),
            Self::InvalidJson => f.write_str("structured output is not one complete JSON value"),
            Self::SchemaViolation => {
                f.write_str("structured output does not satisfy its JSON Schema")
            }
        }
    }
}

impl std::error::Error for StructuredOutputError {}

impl JsonOutputSchema {
    pub fn compile(schema: Value) -> Result<Self, StructuredOutputError> {
        measure(
            &schema,
            MAX_SCHEMA_NODES,
            MAX_SCHEMA_DEPTH,
            MAX_SCHEMA_BYTES,
        )
        .map_err(StructuredOutputError::SchemaLimit)?;
        if serde_json::to_vec(&schema)
            .map_err(|_| StructuredOutputError::InvalidSchema)?
            .len()
            > MAX_SCHEMA_BYTES
        {
            return Err(StructuredOutputError::SchemaLimit("serialized bytes"));
        }
        reference_work(&schema)?;
        let validator = jsonschema::options()
            .offline()
            .with_pattern_options(
                jsonschema::PatternOptions::fancy_regex()
                    .backtrack_limit(10_000)
                    .size_limit(1024 * 1024)
                    .dfa_size_limit(1024 * 1024),
            )
            .build(&schema)
            // Compiler diagnostics can contain schema values. Do not echo them.
            .map_err(|_| StructuredOutputError::InvalidSchema)?;
        Ok(Self { schema, validator })
    }

    pub fn schema(&self) -> &Value {
        &self.schema
    }

    /// Stable across identical requests, without a timestamp/random wire name.
    pub fn format_name(&self) -> &'static str {
        "formless_output"
    }

    /// Validate the entire response, including after an unconstrained provider
    /// fallback. No Markdown extraction, trailing prose or string-to-JSON coercion.
    /// Callers must record known usage before invoking this fallible operation.
    pub fn parse_and_validate(&self, text: &str) -> Result<Value, StructuredOutputError> {
        if text.len() > MAX_OUTPUT_BYTES {
            return Err(StructuredOutputError::OutputLimit("bytes"));
        }
        let value = serde_json::from_str(text).map_err(|_| StructuredOutputError::InvalidJson)?;
        measure(&value, MAX_OUTPUT_NODES, MAX_OUTPUT_DEPTH, MAX_OUTPUT_BYTES)
            .map_err(StructuredOutputError::OutputLimit)?;
        if self.validator.validate(&value).is_err() {
            return Err(StructuredOutputError::SchemaViolation);
        }
        Ok(value)
    }
}

// Check before serialization/compiler recursion, including annotation/data nodes.
fn measure(
    root: &Value,
    max_nodes: usize,
    max_depth: usize,
    max_bytes: usize,
) -> Result<(), &'static str> {
    let mut pending = vec![(root, 0)];
    let mut nodes = 0usize;
    let mut bytes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if nodes > max_nodes {
            return Err("nodes");
        }
        if depth > max_depth {
            return Err("depth");
        }
        let children = match value {
            Value::Array(values) => values.len(),
            Value::Object(values) => values.len(),
            _ => 0,
        };
        if nodes.saturating_add(pending.len()).saturating_add(children) > max_nodes {
            return Err("nodes");
        }
        bytes = bytes.saturating_add(match value {
            Value::String(text) => text.len(),
            Value::Object(values) => values
                .keys()
                .fold(0usize, |sum, key| sum.saturating_add(key.len())),
            _ => 1,
        });
        if bytes > max_bytes {
            return Err("bytes");
        }
        match value {
            Value::Array(values) => pending.extend(values.iter().map(|value| (value, depth + 1))),
            Value::Object(values) => {
                pending.extend(values.values().map(|value| (value, depth + 1)));
            }
            _ => {}
        }
        // Do not allocate an unbounded traversal stack for a huge flat container.
        if pending.len() > max_nodes {
            return Err("nodes");
        }
    }
    Ok(())
}

fn pointer(parent: &str, key: &str) -> String {
    format!("{parent}/{}", key.replace('~', "~0").replace('/', "~1"))
}

/// Only schema locations contribute edges. A `$ref` inside const/default/enum/
/// examples is ordinary JSON data and is preserved without interpretation.
fn reference_work(schema: &Value) -> Result<(), StructuredOutputError> {
    let mut nodes = BTreeMap::<String, Vec<String>>::new();
    let mut pending = vec![(String::new(), schema)];
    while let Some((path, value)) = pending.pop() {
        let mut edges = Vec::new();
        if let Some(object) = value.as_object() {
            if object.contains_key("$id") || object.contains_key("id") {
                return Err(StructuredOutputError::UnsupportedSchema("resource IDs"));
            }
            if object.contains_key("$dynamicRef") || object.contains_key("$recursiveRef") {
                return Err(StructuredOutputError::UnsupportedSchema(
                    "dynamic references",
                ));
            }
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                edges.push(local_pointer(reference)?);
            }
            for key in [
                "$defs",
                "definitions",
                "properties",
                "patternProperties",
                "dependentSchemas",
                "dependencies",
            ] {
                if let Some(children) = value[key].as_object() {
                    let base = pointer(&path, key);
                    for (name, child) in children {
                        if child.is_object() || child.is_boolean() {
                            let target = pointer(&base, name);
                            edges.push(target.clone());
                            pending.push((target, child));
                        }
                    }
                }
            }
            for key in [
                "additionalProperties",
                "unevaluatedProperties",
                "propertyNames",
                "items",
                "additionalItems",
                "contains",
                "unevaluatedItems",
                "not",
                "if",
                "then",
                "else",
                "contentSchema",
                "allOf",
                "anyOf",
                "oneOf",
                "prefixItems",
            ] {
                let child = &value[key];
                let base = pointer(&path, key);
                if child.is_object() || child.is_boolean() {
                    edges.push(base.clone());
                    pending.push((base, child));
                } else if let Some(children) = child.as_array() {
                    for (index, child) in children.iter().enumerate() {
                        if child.is_object() || child.is_boolean() {
                            let target = pointer(&base, &index.to_string());
                            edges.push(target.clone());
                            pending.push((target, child));
                        }
                    }
                }
            }
        }
        nodes.insert(path, edges);
    }

    fn visit(
        path: &str,
        nodes: &BTreeMap<String, Vec<String>>,
        visiting: &mut Vec<String>,
        costs: &mut BTreeMap<String, (usize, usize)>,
    ) -> Result<(usize, usize), StructuredOutputError> {
        if visiting.iter().any(|current| current == path) {
            return Err(StructuredOutputError::UnsupportedSchema(
                "recursive references",
            ));
        }
        if visiting.len() >= MAX_REFERENCE_DEPTH {
            return Err(StructuredOutputError::SchemaLimit("reference depth"));
        }
        if let Some((cost, height)) = costs.get(path) {
            if visiting.len().saturating_add(*height) > MAX_REFERENCE_DEPTH {
                return Err(StructuredOutputError::SchemaLimit("reference depth"));
            }
            return Ok((*cost, *height));
        }
        let edges = nodes
            .get(path)
            .ok_or(StructuredOutputError::UnsupportedSchema(
                "reference must target a recognized local schema location",
            ))?;
        visiting.push(path.to_owned());
        let mut cost = 1usize;
        let mut height = 1usize;
        for edge in edges {
            let (edge_cost, edge_height) = visit(edge, nodes, visiting, costs)?;
            cost = cost.saturating_add(edge_cost);
            height = height.max(1 + edge_height);
            if cost > MAX_EXPANDED_SCHEMA_NODES {
                return Err(StructuredOutputError::SchemaLimit("reference expansion"));
            }
        }
        visiting.pop();
        if visiting.len().saturating_add(height) > MAX_REFERENCE_DEPTH {
            return Err(StructuredOutputError::SchemaLimit("reference depth"));
        }
        costs.insert(path.to_owned(), (cost, height));
        Ok((cost, height))
    }
    visit("", &nodes, &mut Vec::new(), &mut BTreeMap::new())?;
    Ok(())
}

fn local_pointer(reference: &str) -> Result<String, StructuredOutputError> {
    let fragment = reference
        .strip_prefix('#')
        .ok_or(StructuredOutputError::UnsupportedSchema(
            "external references",
        ))?;
    if !fragment.is_empty() && !fragment.starts_with('/') {
        return Err(StructuredOutputError::UnsupportedSchema(
            "anchor references",
        ));
    }
    let mut decoded = Vec::with_capacity(fragment.len());
    let mut bytes = fragment.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let digit = |byte: u8| (byte as char).to_digit(16).map(|digit| digit as u8);
            let high = bytes.next().and_then(digit);
            let low = bytes.next().and_then(digit);
            let (Some(high), Some(low)) = (high, low) else {
                return Err(StructuredOutputError::InvalidSchema);
            };
            decoded.push(high * 16 + low);
        } else {
            decoded.push(byte);
        }
    }
    String::from_utf8(decoded).map_err(|_| StructuredOutputError::InvalidSchema)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn failure(schema: Value) -> StructuredOutputError {
        match JsonOutputSchema::compile(schema) {
            Ok(_) => panic!("schema unexpectedly accepted"),
            Err(error) => error,
        }
    }

    #[test]
    fn immutable_default_and_cloned_options_preserve_the_exact_schema() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<CompletionOptions>();
        assert!(CompletionOptions::default().output_schema().is_none());
        let schema = json!({
            "type":"object",
            "properties":{"value":{"const":{"$ref":"https://never-fetch.invalid/x","type":"input_audio"}}},
            "required":["value"],"additionalProperties":false,
            "default":{"$id":"file:///never-read","arbitrary":"unchanged"},
            "examples":[{"$dynamicRef":"https://never-fetch.invalid/example"}]
        });
        let options = CompletionOptions::json_schema(schema.clone()).unwrap();
        let cloned = options.clone();
        let compiled = cloned.output_schema().unwrap();
        assert_eq!(compiled.schema(), &schema);
        assert_eq!(
            compiled.format_name(),
            options.output_schema().unwrap().format_name()
        );
        assert_eq!(
            compiled
                .parse_and_validate(
                    r#"{"value":{"$ref":"https://never-fetch.invalid/x","type":"input_audio"}}"#
                )
                .unwrap(),
            json!({"value":schema["properties"]["value"]["const"]})
        );
        assert!(std::ptr::eq(compiled, options.output_schema().unwrap()));
    }

    #[test]
    fn local_definitions_composition_and_escaped_pointers_validate_whole_values() {
        let schema = JsonOutputSchema::compile(json!({
            "$defs":{"a/b~c":{"type":"integer","minimum":2},"named value":{"type":"string","minLength":2}},
            "allOf":[{"type":"object"},{"properties":{"n":{"$ref":"#/$defs/a~1b~0c"},"s":{"$ref":"#/$defs/named%20value"}},"required":["n","s"],"additionalProperties":false}]
        })).unwrap();
        assert_eq!(
            schema
                .parse_and_validate(" \n{\"n\":2,\"s\":\"ok\"}\t")
                .unwrap(),
            json!({"n":2,"s":"ok"})
        );
        assert_eq!(
            schema.parse_and_validate(r#"{"n":1,"s":"ok"}"#),
            Err(StructuredOutputError::SchemaViolation)
        );
        for invalid in [
            "```json\n{\"n\":2,\"s\":\"ok\"}\n```",
            "prefix {}",
            "{} {}",
            "NaN",
            "",
        ] {
            assert_eq!(
                schema.parse_and_validate(invalid),
                Err(StructuredOutputError::InvalidJson)
            );
        }
        assert_eq!(
            schema.parse_and_validate(r#""{\"n\":2,\"s\":\"ok\"}""#),
            Err(StructuredOutputError::SchemaViolation)
        );
    }

    #[test]
    fn external_and_unused_references_are_rejected_without_network_or_file_resolution() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let network = format!("http://{}/schema", listener.local_addr().unwrap());
        for reference in [
            network.as_str(),
            "https://example.invalid/schema",
            "file:///private/nonexistent-schema",
            "relative.json",
            "//example.invalid/schema",
        ] {
            assert_eq!(
                failure(json!({"$ref":reference})),
                StructuredOutputError::UnsupportedSchema("external references")
            );
            assert_eq!(
                failure(json!({"$defs":{"unused":{"$ref":reference}},"type":"string"})),
                StructuredOutputError::UnsupportedSchema("external references")
            );
        }
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(
            failure(json!({"const":{"type":"string"},"$ref":"#/const"})),
            StructuredOutputError::UnsupportedSchema(
                "reference must target a recognized local schema location"
            )
        );
    }

    #[test]
    fn recursive_dynamic_and_resource_scoped_schemas_fail_closed() {
        for schema in [
            json!({"$ref":"#"}),
            json!({"$defs":{"a":{"$ref":"#/$defs/b"},"b":{"$ref":"#/$defs/a"}}}),
        ] {
            assert_eq!(
                failure(schema),
                StructuredOutputError::UnsupportedSchema("recursive references")
            );
        }
        assert_eq!(
            failure(json!({"$dynamicRef":"#anchor"})),
            StructuredOutputError::UnsupportedSchema("dynamic references")
        );
        assert_eq!(
            failure(json!({"$id":"https://example.invalid/schema","type":"string"})),
            StructuredOutputError::UnsupportedSchema("resource IDs")
        );
        assert_eq!(
            failure(json!({"$ref":"#anchor","$anchor":"anchor"})),
            StructuredOutputError::UnsupportedSchema("anchor references")
        );
        assert_eq!(
            failure(json!({"$ref":"#/$defs/missing"})),
            StructuredOutputError::UnsupportedSchema(
                "reference must target a recognized local schema location"
            )
        );
        assert_eq!(
            failure(json!({"type":42})),
            StructuredOutputError::InvalidSchema
        );
    }

    #[test]
    fn schema_bytes_nodes_depth_and_reference_expansion_have_independent_bounds() {
        assert_eq!(
            failure(json!({"description":"x".repeat(MAX_SCHEMA_BYTES)})),
            StructuredOutputError::SchemaLimit("bytes")
        );
        assert_eq!(
            failure(json!({"description":"\n".repeat(MAX_SCHEMA_BYTES / 2)})),
            StructuredOutputError::SchemaLimit("serialized bytes")
        );
        assert_eq!(
            failure(json!({"examples":vec![Value::Null; MAX_SCHEMA_NODES]})),
            StructuredOutputError::SchemaLimit("nodes")
        );
        let mut deep = json!(true);
        for _ in 0..=MAX_SCHEMA_DEPTH {
            deep = json!({"not":deep});
        }
        assert_eq!(failure(deep), StructuredOutputError::SchemaLimit("depth"));
        let mut defs = serde_json::Map::new();
        defs.insert("n0".into(), json!({"type":"string"}));
        for index in 1..16 {
            let reference = format!("#/$defs/n{}", index - 1);
            defs.insert(
                format!("n{index}"),
                json!({"allOf":[{"$ref":reference},{"$ref":reference}]}),
            );
        }
        assert_eq!(
            failure(json!({"$defs":defs,"$ref":"#/$defs/n15"})),
            StructuredOutputError::SchemaLimit("reference expansion")
        );
    }

    #[test]
    fn whole_output_has_size_structure_type_and_privacy_limits() {
        let any = JsonOutputSchema::compile(json!(true)).unwrap();
        for (text, expected) in [
            ("null", Value::Null),
            ("false", json!(false)),
            ("42", json!(42)),
            ("\"text\"", json!("text")),
            ("[1,2]", json!([1, 2])),
        ] {
            assert_eq!(any.parse_and_validate(text).unwrap(), expected);
        }
        assert_eq!(
            any.parse_and_validate(&" ".repeat(MAX_OUTPUT_BYTES + 1)),
            Err(StructuredOutputError::OutputLimit("bytes"))
        );
        assert_eq!(
            any.parse_and_validate(&serde_json::to_string(&vec![0; MAX_OUTPUT_NODES]).unwrap()),
            Err(StructuredOutputError::OutputLimit("nodes"))
        );
        let nested = format!(
            "{}0{}",
            "[".repeat(MAX_OUTPUT_DEPTH + 1),
            "]".repeat(MAX_OUTPUT_DEPTH + 1)
        );
        assert_eq!(
            any.parse_and_validate(&nested),
            Err(StructuredOutputError::OutputLimit("depth"))
        );
        let integer = JsonOutputSchema::compile(json!({"type":"integer"})).unwrap();
        let secret = "private-output-sentinel";
        let error = integer
            .parse_and_validate(&json!(secret).to_string())
            .unwrap_err();
        assert_eq!(error, StructuredOutputError::SchemaViolation);
        assert!(!error.to_string().contains(secret));
        assert!(JsonOutputSchema::compile(json!(false))
            .unwrap()
            .parse_and_validate("null")
            .is_err());
    }

    #[test]
    fn memoized_local_definitions_do_not_bypass_the_reference_depth_limit() {
        let mut defs = serde_json::Map::new();
        defs.insert("n000".into(), json!({"type":"integer"}));
        // Sorted earlier definitions are memoized at a shallow traversal depth.
        // Their cached height must still count when a later definition uses them.
        for index in 1..=MAX_REFERENCE_DEPTH {
            defs.insert(
                format!("n{index:03}"),
                json!({"$ref":format!("#/$defs/n{:03}", index - 1)}),
            );
        }
        assert_eq!(
            failure(json!({"$defs":defs})),
            StructuredOutputError::SchemaLimit("reference depth")
        );
    }
}
