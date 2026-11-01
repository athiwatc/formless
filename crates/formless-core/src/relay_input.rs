//! Allocation-bounded shape preflight for relay-owned typed request queues.
//! This visits JSON without collecting a tree, then reserves conservative AST
//! storage BEFORE RequestEnvelope/Value allocation. It is not an OS/RSS bound.
use crate::relay_buffers::{Lease, CLIENT_BULK_BYTES};
use anyhow::{anyhow, Result};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::fmt;

// Request is an internally tagged enum: Serde first collects a Content tree,
// then ContentDeserializer builds the actual Value/typed fields while the
// original Vec allocations may still be retained. Budget BOTH representations,
// including geometric Vec reallocations. Pinned Value and Content are <=32B:
// Content capacity <2N + old Value capacity <N + new Value capacity <2N can
// coexist. 256/node (eight slots) covers that five-slot peak plus headers.
// Array floors cover both representations and minimum-growth slack with 512B; object
// floors cover Content's four key/value pairs plus the first BTreeMap leaf.
// Later map growth is covered by key/value node charges plus 256/entry. Raw
// line/string/escape-scratch and diagnostic overlap is covered by 7*wire+4096:
// raw N + parser scratch <=2N + owned Content strings <=N + diagnostic old/new
// or shrink overlap <=3N. Escaped diagnostics additionally reserve 3*expansion.
// These are pinned-implementation conservative application-buffer assumptions,
// not allocator/RSS measurements. Tests exercise actual RequestEnvelope parsing
// just above Vec growth powers and fail if the observed final layout outgrows it.
// Sources: serde/private/de.rs TaggedContentVisitor + visit_content_seq;
// serde_json/value/de.rs ValueVisitor; Rust alloc/collections/btree/node.rs
// https://doc.rust-lang.org/src/alloc/collections/btree/node.rs.html (capacity11).
const NODE: usize = 256;
const ARRAY: usize = 512;
const OBJECT: usize = 2048;
const MAP_ENTRY: usize = 256;
const DIAGNOSTIC_OVERHEAD: usize = 32 * 1024;
const LIMIT_ERROR: &str = "relay request exceeds the typed input buffer budget";

struct Counter {
    bytes: usize,
    diagnostic_extra: usize,
}
// Rust str Debug escapes DEL/control/nonprinting Unicode. Account before
// Serde can format Unexpected::Str / unknown_variant / unknown_field. Printable
// ASCII has an exact fast path; other scalars use allocation-free escape_debug.
fn debug_bytes(value: &str) -> Option<usize> {
    let mut bytes = 2usize; // opening and closing quotes
    for character in value.chars() {
        let escaped = match character {
            '\0' | '\t' | '\r' | '\n' | '\\' | '\"' => 2,
            '\u{1}'..='\u{f}' => 5,
            '\u{10}'..='\u{1f}' | '\u{7f}' => 6,
            c if c.is_ascii() => 1, // str Debug does not escape single quotes.
            c => c.escape_debug().map(char::len_utf8).sum(),
        };
        bytes = bytes.checked_add(escaped)?;
    }
    Some(bytes)
}
impl Counter {
    fn string<E: de::Error>(&mut self, value: &str) -> std::result::Result<(), E> {
        // A diagnostic may retain old/new String storage or its boxed shrink
        // destination at once: allow three times its text. The normal-copy
        // portion is already in 7*wire; reserve the extra
        // expansion of the ONE string a serde diagnostic can quote.
        let extra = debug_bytes(value)
            .and_then(|n| n.saturating_sub(value.len()).checked_mul(3))
            .ok_or_else(|| E::custom(LIMIT_ERROR))?;
        if extra > self.diagnostic_extra {
            self.add::<E>(extra - self.diagnostic_extra)?;
            self.diagnostic_extra = extra;
        }
        Ok(())
    }
    fn add<E: de::Error>(&mut self, bytes: usize) -> std::result::Result<(), E> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|bytes| *bytes <= CLIENT_BULK_BYTES)
            .ok_or_else(|| E::custom(LIMIT_ERROR))?;
        Ok(())
    }
}
struct Seed<'a>(&'a mut Counter);
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> std::result::Result<(), D::Error> {
        self.0.add::<D::Error>(NODE)?;
        deserializer.deserialize_any(Shape(self.0))
    }
}
struct Shape<'a>(&'a mut Counter);
impl<'de> Visitor<'de> for Shape<'_> {
    type Value = ();
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded JSON request")
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<(), E> {
        self.0.string::<E>(value)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> std::result::Result<(), A::Error> {
        self.0.add::<A::Error>(ARRAY)?;
        while sequence.next_element_seed(Seed(self.0))?.is_some() {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
        self.0.add::<A::Error>(OBJECT)?;
        while map.next_key_seed(Seed(self.0))?.is_some() {
            self.0.add::<A::Error>(MAP_ENTRY)?;
            map.next_value_seed(Seed(self.0))?;
        }
        Ok(())
    }
}
fn estimate(line: &str) -> Result<usize> {
    let base = line
        .len()
        .checked_mul(7)
        .and_then(|n| n.checked_add(4096))
        .filter(|n| *n <= CLIENT_BULK_BYTES)
        .ok_or_else(|| anyhow!(LIMIT_ERROR))?;
    let mut counter = Counter {
        bytes: base,
        diagnostic_extra: 0,
    };
    counter
        .add::<serde_json::Error>(DIAGNOSTIC_OVERHEAD)
        .map_err(|_| anyhow!(LIMIT_ERROR))?;
    {
        // Keep serde_json's default recursion limit (128). The prepass owns no
        // strings, Vecs or map of nodes. Its escape scratch is dropped here
        // before the typed parse can allocate its own representation.
        let mut parser = serde_json::Deserializer::from_str(line);
        Seed(&mut counter)
            .deserialize(&mut parser)
            .map_err(|_| anyhow!(LIMIT_ERROR))?;
        parser.end().map_err(|_| anyhow!(LIMIT_ERROR))?;
    }
    Ok(counter.bytes)
}
pub(crate) fn prepare(line: &str, lease: &mut Lease) -> Result<()> {
    let bytes = estimate(line)?;
    lease.grow(bytes).map_err(|_| anyhow!(LIMIT_ERROR))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay_buffers::{ClientMemory, Pool};
    use serde_json::Value;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;
    fn memory() -> (Arc<Pool>, Arc<ClientMemory>) {
        let pool = Arc::new(Pool::default());
        let memory = pool.client(CancellationToken::new()).unwrap();
        (pool, memory)
    }
    #[test]
    fn counts_keys_container_floors_and_vec_slack_before_materializing_the_tree() {
        assert!(std::mem::size_of::<Value>() <= 32);
        // Intentional private-version guard: this accounting proof must be
        // re-audited when the pinned Serde tagged representation changes.
        assert!(std::mem::size_of::<serde::__private229::de::Content<'static>>() <= 32);
        assert!(std::mem::size_of::<formless_protocol::RequestEnvelope>() <= 4096);
        let btree_node_floor = 11 * (std::mem::size_of::<String>() + std::mem::size_of::<Value>())
            + 16 * std::mem::size_of::<usize>();
        assert!(
            OBJECT >= btree_node_floor,
            "small-map node floor remains covered on this target"
        );
        let line = format!("[{}]", "[0],".repeat(1000).trim_end_matches(','));
        let wanted = estimate(&line).unwrap();
        let parsed: Value = serde_json::from_str(&line).unwrap();
        let outer = parsed.as_array().unwrap();
        let retained = outer.capacity() * std::mem::size_of::<Value>()
            + outer
                .iter()
                .map(|value| value.as_array().unwrap().capacity() * std::mem::size_of::<Value>())
                .sum::<usize>();
        assert!(
            wanted >= retained + line.len(),
            "small nested Vec allocation floors are covered"
        );
        let object = estimate("{\"a\":0}").unwrap();
        assert_eq!(
            object,
            7 * 7 + 4096 + 3 * NODE + OBJECT + MAP_ENTRY + DIAGNOSTIC_OVERHEAD + 6
        );
    }
    #[test]
    fn actual_tagged_request_prepass_covers_content_and_value_vec_peaks_at_growth_edges() {
        use formless_protocol::{Request, RequestEnvelope, PROTOCOL_VERSION};
        let template = serde_json::to_string(&RequestEnvelope {
            id: 7,
            protocol: PROTOCOL_VERSION,
            request: Request::RunTool {
                id: "fixture".into(),
                input: Value::Null,
                session_id: "fixture".into(),
            },
        })
        .unwrap();
        for elements in [5usize, 4097, 32769, 262145] {
            let input = format!("[{}]", "0,".repeat(elements).trim_end_matches(','));
            let line = template.replace("\"input\":null", &format!("\"input\":{input}"));
            let wanted = estimate(&line).unwrap();
            let envelope: RequestEnvelope = serde_json::from_str(&line).unwrap();
            let Request::RunTool { input, .. } = envelope.request else {
                unreachable!()
            };
            let values = input.as_array().unwrap();
            assert_eq!(values.len(), elements);
            let content_slots = elements.next_power_of_two().max(4);
            let tagged_content_peak =
                content_slots * std::mem::size_of::<serde::__private229::de::Content<'static>>();
            // Value Vec's old allocation can coexist with its replacement
            // while the original Content VecIntoIter still owns its buffer.
            let final_value_peak =
                (values.capacity() + values.capacity() / 2) * std::mem::size_of::<Value>();
            assert!(
                wanted >= line.len() + tagged_content_peak + final_value_peak,
                "both tagged Content and Value Vec capacities remain reserved at {elements}"
            );
        }
    }
    #[test]
    fn compact_wide_ast_is_refused_without_a_value_tree_or_large_key_error_copy() {
        // 7.2 MiB is below IPC's 32 MiB ceiling, but 1.8 million nested
        // arrays have a much larger typed heap than their compact wire form.
        let line = format!("[{}]", "[0],".repeat(1_800_000).trim_end_matches(','));
        assert!(line.len() < formless_protocol::MAX_FRAME_BYTES);
        let (pool, memory) = memory();
        let base = 4 * line.len() + 4096;
        let mut lease = memory.reserve(base).unwrap();
        assert!(prepare(&line, &mut lease).is_err());
        assert_eq!(
            pool.usage(),
            (1, base, 0),
            "prepass did not materialize or park an uncharged AST"
        );
        drop(lease);
        assert_eq!(pool.usage(), (1, 0, 0));
        let small_maps = format!("[{}]", "{\"\":0},".repeat(400_000).trim_end_matches(','));
        assert!(small_maps.len() < formless_protocol::MAX_FRAME_BYTES);
        assert!(
            estimate(&small_maps).is_err(),
            "many one-entry maps pay their full initial B-tree nodes"
        );
        assert_eq!(
            estimate(&format!("{{\"{}\":", "private key".repeat(10000)))
                .unwrap_err()
                .to_string(),
            LIMIT_ERROR
        );
    }
    #[test]
    fn exact_maximum_string_and_escape_scratch_remain_admissible() {
        let line = format!("\"{}\"", "x".repeat(formless_protocol::MAX_FRAME_BYTES - 2));
        let (pool, memory) = memory();
        let mut lease = memory.reserve(4 * line.len() + 4096).unwrap();
        prepare(&line, &mut lease).unwrap();
        let parsed: String = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed.len(), formless_protocol::MAX_FRAME_BYTES - 2);
        assert!(pool.usage().1 >= 7 * line.len() + 4096 + NODE);
        drop(parsed);
        drop(lease);
        assert_eq!(pool.usage(), (1, 0, 0));
        let escaped = "\"\\u0078\\n\\\"\"";
        assert_eq!(
            estimate(escaped).unwrap(),
            7 * escaped.len()
                + 4096
                + NODE
                + DIAGNOSTIC_OVERHEAD
                + 3 * (debug_bytes(&serde_json::from_str::<String>(escaped).unwrap()).unwrap()
                    - serde_json::from_str::<String>(escaped).unwrap().len())
        );
    }
    #[test]
    fn full_32mib_actual_tagged_request_string_remains_admissible() {
        use formless_protocol::{Request, RequestEnvelope, PROTOCOL_VERSION};
        let mut envelope = RequestEnvelope {
            id: 7,
            protocol: PROTOCOL_VERSION,
            request: Request::RunTool {
                id: "fixture".into(),
                input: Value::String(String::new()),
                session_id: "fixture".into(),
            },
        };
        let overhead = serde_json::to_string(&envelope).unwrap().len();
        let Request::RunTool { input, .. } = &mut envelope.request else {
            unreachable!()
        };
        *input = Value::String("x".repeat(formless_protocol::MAX_FRAME_BYTES - overhead));
        let line = serde_json::to_string(&envelope).unwrap();
        drop(envelope);
        assert_eq!(line.len(), formless_protocol::MAX_FRAME_BYTES);
        let (pool, memory) = memory();
        let mut lease = memory.reserve(4 * line.len() + 4096).unwrap();
        prepare(&line, &mut lease).unwrap();
        let parsed: RequestEnvelope = serde_json::from_str(&line).unwrap();
        let Request::RunTool {
            input: Value::String(input),
            ..
        } = parsed.request
        else {
            unreachable!()
        };
        assert_eq!(input.len(), formless_protocol::MAX_FRAME_BYTES - overhead);
        drop(input);
        drop(lease);
        assert_eq!(pool.usage(), (1, 0, 0));
    }
    #[test]
    fn growth_and_cancellation_keep_the_owner_charged_until_the_queued_tree_drops() {
        let (pool, memory) = memory();
        let line = format!("[{}]", "{\"k\":0},".repeat(500).trim_end_matches(','));
        let mut lease = memory.reserve(4 * line.len() + 4096).unwrap();
        prepare(&line, &mut lease).unwrap();
        let expected = estimate(&line).unwrap();
        let parsed: Value = serde_json::from_str(&line).unwrap();
        memory.cancel.cancel();
        assert_eq!(pool.usage(), (1, expected, 0));
        drop(parsed);
        drop(lease);
        assert_eq!(pool.usage(), (1, 0, 0));
        let deeply_nested = format!("{}0{}", "[".repeat(129), "]".repeat(129));
        assert!(
            estimate(&deeply_nested).is_err(),
            "default JSON recursion limit is retained"
        );
    }
}

// The error path must not rebuild the original JSON Value tree, nor retain a
// giant unknown-variant/field diagnostic. Only bounded envelope metadata is
// collected; ignored fields are streamed through serde's IgnoredAny visitor.
#[derive(Default)]
enum Kind {
    #[default]
    Missing,
    Short(String),
    Long,
}
fn bounded_kind<'de, D: de::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Kind, D::Error> {
    struct Bounded;
    impl<'de> Visitor<'de> for Bounded {
        type Value = Kind;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a request type")
        }
        fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Kind, E> {
            Ok(if value.len() <= 256 {
                Kind::Short(value.to_owned())
            } else {
                Kind::Long
            })
        }
        fn visit_unit<E: de::Error>(self) -> std::result::Result<Kind, E> {
            Ok(Kind::Missing)
        }
    }
    deserializer.deserialize_any(Bounded)
}
#[derive(Default, serde::Deserialize)]
struct RequestKind {
    #[serde(default, rename = "type", deserialize_with = "bounded_kind")]
    kind: Kind,
}
#[derive(Default, serde::Deserialize)]
struct Metadata {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    request: RequestKind,
}
pub(crate) fn error_reply(line: &str) -> formless_protocol::ServerMessage {
    use formless_protocol::{codes, ServerMessage};
    let metadata = match serde_json::from_str::<Metadata>(line) {
        Ok(metadata) => metadata,
        Err(error) => {
            drop(error);
            #[derive(serde::Deserialize)]
            struct IdOnly {
                #[serde(default)]
                id: u64,
            }
            Metadata {
                id: serde_json::from_str::<IdOnly>(line).map_or(0, |m| m.id),
                request: RequestKind::default(),
            }
        }
    };
    let unsupported = match &metadata.request.kind {
        Kind::Long => true,
        Kind::Short(kind) => {
            // Never pass the original body to Value-based legacy helpers. This
            // synthetic input has one <=256-byte type and fixed structure.
            let small = serde_json::json!({"request":{"type":kind}}).to_string();
            formless_protocol::unknown_request_type(&small).is_some()
        }
        Kind::Missing => false,
    };
    if unsupported {
        let mut result = serde_json::json!({"code":codes::UNSUPPORTED});
        if let Kind::Short(kind) = metadata.request.kind {
            result["type"] = serde_json::Value::String(kind);
        }
        ServerMessage::Response {
            id: metadata.id,
            ok: false,
            result: Some(result),
            error: Some("unsupported relay request type".into()),
        }
    } else {
        ServerMessage::err_code(
            metadata.id,
            codes::BAD_REQUEST,
            "invalid relay request".into(),
        )
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;
    use formless_protocol::{codes, RequestEnvelope};
    #[test]
    fn error_metadata_preserves_ids_and_codes_without_echoing_large_type_or_bad_body() {
        let private = "opaque-private-type-".repeat(200_000);
        let line = serde_json::json!({"id":71,"protocol":1,"request":{"type":private}}).to_string();
        let error = serde_json::from_str::<RequestEnvelope>(&line).unwrap_err();
        drop(error);
        let reply = error_reply(&line);
        assert_eq!(reply.error_code(), Some(codes::UNSUPPORTED));
        let json = serde_json::to_value(&reply).unwrap();
        assert_eq!(json["id"], 71);
        assert!(json["result"].get("type").is_none());
        assert!(serde_json::to_string(&reply).unwrap().len() < 256);
        let small = error_reply(r#"{"id":72,"request":{"type":"future_fixture"}}"#);
        assert_eq!(small.error_code(), Some(codes::UNSUPPORTED));
        assert_eq!(
            serde_json::to_value(small).unwrap()["result"]["type"],
            "future_fixture"
        );
        for request in [
            serde_json::Value::String(private),
            serde_json::Value::Null,
            serde_json::json!({"type":[]}),
        ] {
            let line = serde_json::json!({"id":73,"request":request}).to_string();
            let reply = error_reply(&line);
            assert_eq!(reply.error_code(), Some(codes::BAD_REQUEST));
            assert_eq!(serde_json::to_value(&reply).unwrap()["id"], 73);
            assert!(serde_json::to_string(&reply).unwrap().len() < 256);
        }
    }
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    #[test]
    fn debug_preflight_matches_rust_strings_and_reserves_geometric_error_capacity() {
        let ascii = (0u8..=127).map(char::from).collect::<String>();
        for value in [
            ascii,
            "printable ' ASCII \" \\".into(),
            "\u{85}\u{202e}😀é".into(),
        ] {
            assert_eq!(debug_bytes(&value).unwrap(), format!("{value:?}").len());
        }
        let line = r#"{"id":7,"protocol":1,"request":{"type":"not_a_known_request"}}"#;
        let error = serde_json::from_str::<formless_protocol::RequestEnvelope>(line).unwrap_err();
        assert!(error.to_string().contains("unknown variant"));
        assert!(
            3 * error.to_string().len() + 4096 <= DIAGNOSTIC_OVERHEAD,
            "pinned expected-variant list and formatting copies remain within fixed headroom"
        );
        let private = "\u{7f}".repeat(24 * 1024 * 1024);
        let line =
            serde_json::json!({"id":7,"protocol":private,"request":{"type":"ping"}}).to_string();
        assert!(line.len() < formless_protocol::MAX_FRAME_BYTES);
        assert!(
            estimate(&line).is_err(),
            "refuse before constructing a huge Unexpected::Str diagnostic"
        );
        // Keys can also be quoted by an initial unknown-field diagnostic.
        let key = "\u{7f}".repeat(24 * 1024 * 1024);
        let line = format!("{{\"{key}\":0}}");
        assert!(estimate(&line).is_err());
    }
}
