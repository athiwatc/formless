//! Bounded, credential-scoped observations, never quota predictions.
use chrono::{DateTime, TimeDelta, Utc};
use formless_runtime::LockExt as _;
use reqwest::header::HeaderMap;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;

const MAX_AGE_SECONDS: i64 = 300;
const MAX_ENTRIES: usize = 64;
const KINDS: [&str; 4] = ["requests", "tokens", "input-tokens", "output-tokens"];

// Neither the credential nor its fingerprint is part of the public snapshot.
#[derive(Clone, Hash, PartialEq, Eq)]
struct Identity {
    provider: String,
    endpoint: String,
    fingerprint: [u8; 32],
}
struct Observation {
    id: String,
    provider: String,
    endpoint: String,
    at: DateTime<Utc>,
    fields: Map<String, Value>,
    resets: HashMap<String, DateTime<Utc>>,
}
#[derive(Default)]
struct Registry(HashMap<Identity, Observation>);
static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn normalized_endpoint(base: &str) -> String {
    match reqwest::Url::parse(base) {
        Ok(mut url) => {
            let path = url.path().trim_end_matches('/').to_string();
            url.set_path(&path);
            url.set_fragment(None);
            // Only a root path without a query has a trailing slash after
            // serialization. A slash in the query is part of its value.
            let text = url.to_string();
            if url.query().is_none() {
                text.trim_end_matches('/').to_string()
            } else {
                text
            }
        }
        Err(_) => base.trim_end_matches('/').to_string(),
    }
}

pub(crate) fn display_endpoint(base: &str) -> String {
    match reqwest::Url::parse(base) {
        Ok(mut url) => {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            url.to_string().trim_end_matches('/').to_string()
        }
        Err(_) => "(invalid endpoint)".into(),
    }
}

fn header_parts(header: &str) -> Option<(&str, &str)> {
    let (field, kind) = if let Some(short) = header.strip_prefix("x-ratelimit-") {
        short.split_once('-').unwrap_or((short, "requests"))
    } else {
        let (kind, field) = header
            .strip_prefix("anthropic-ratelimit-")?
            .rsplit_once('-')?;
        (field, kind)
    };
    (matches!(field, "limit" | "remaining" | "reset") && KINDS.contains(&kind))
        .then_some((field, kind))
}

fn canonical(header: &str) -> Option<String> {
    let (field, kind) = header_parts(header)?;
    Some(format!("{field}-{kind}"))
}

pub(crate) fn is_reset_header(header: &str) -> bool {
    matches!(header_parts(header), Some(("reset", _)))
}

fn duration_millis(raw: &str) -> Option<i64> {
    let mut rest = raw.trim();
    let mut total = 0.0;
    let mut parts = 0;
    while !rest.is_empty() {
        let n = rest
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(rest.len());
        let amount: f64 = rest[..n].parse().ok()?;
        rest = &rest[n..];
        let (unit, multiplier) = [
            ("ms", 1.0),
            ("s", 1000.0),
            ("m", 60_000.0),
            ("h", 3_600_000.0),
            ("d", 86_400_000.0),
        ]
        .into_iter()
        .find(|(unit, _)| rest.starts_with(unit))?;
        rest = rest[unit.len()..].trim_start();
        total += amount * multiplier;
        parts += 1;
    }
    (parts > 0 && total.is_finite() && total >= 0.0 && total < i64::MAX as f64)
        .then(|| total.ceil() as i64)
}

pub(crate) fn absolute_reset_at(raw: &str) -> Option<DateTime<Utc>> {
    if let Ok(date) = DateTime::parse_from_rfc3339(raw.trim()) {
        return Some(date.with_timezone(&Utc));
    }
    let epoch: f64 = raw.trim().parse().ok()?;
    let millis = if epoch >= 100_000_000_000.0 {
        epoch
    } else {
        epoch * 1000.0
    };
    if (1_000_000_000_000.0..i64::MAX as f64).contains(&millis) {
        return DateTime::from_timestamp_millis(millis.ceil() as i64);
    }
    None
}

fn reset_at(raw: &str, at: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if let Some(date) = absolute_reset_at(raw) {
        return Some(date);
    }
    if raw.trim().parse::<f64>().is_ok() {
        // A small bare number may mean relative seconds. Retain a labelled
        // raw value in the snapshot rather than guessing.
        return None;
    }
    at.checked_add_signed(TimeDelta::try_milliseconds(duration_millis(raw)?)?)
}

impl Observation {
    fn prune(&mut self, now: DateTime<Utc>) -> bool {
        let age = now.signed_duration_since(self.at);
        if age < TimeDelta::zero() || age >= TimeDelta::seconds(MAX_AGE_SECONDS) {
            return false;
        }
        for kind in KINDS {
            if self.resets.get(kind).is_some_and(|reset| *reset <= now) {
                self.resets.remove(kind);
                for field in ["limit", "remaining", "reset"] {
                    self.fields.remove(&format!("{field}-{kind}"));
                }
            }
        }
        !self.fields.is_empty()
    }

    fn public(&self, now: DateTime<Utc>) -> Value {
        let mut fields = self.fields.clone();
        let mut parts = Vec::new();
        for kind in KINDS {
            if let Some(remaining) = fields
                .get(&format!("remaining-{kind}"))
                .and_then(Value::as_str)
            {
                let limit = fields
                    .get(&format!("limit-{kind}"))
                    .and_then(Value::as_str)
                    .unwrap_or("?");
                let reset = match self.resets.get(kind) {
                    Some(reset) => format!(
                        ", resets in {}s",
                        (reset.signed_duration_since(now).num_milliseconds() + 999) / 1000
                    ),
                    None => fields
                        .get(&format!("reset-{kind}"))
                        .and_then(Value::as_str)
                        .map(|raw| format!(", reset {raw} (unparsed)"))
                        .unwrap_or_default(),
                };
                parts.push(format!("{remaining}/{limit} {kind} left{reset}"));
            }
        }
        fields.insert("provider".into(), json!(self.provider));
        fields.insert("endpoint".into(), json!(self.endpoint));
        fields.insert("at".into(), json!(self.at.to_rfc3339()));
        fields.insert("max_age_seconds".into(), json!(MAX_AGE_SECONDS));
        if !parts.is_empty() {
            fields.insert(
                "line".into(),
                json!(format!(
                    "rate limit for {} at {}: {}",
                    self.provider,
                    self.endpoint,
                    parts.join(" · ")
                )),
            );
        }
        Value::Object(fields)
    }
}

impl Registry {
    fn note(
        &mut self,
        provider: &str,
        base: &str,
        credential: &str,
        headers: &HeaderMap,
        at: DateTime<Utc>,
        redact: impl Fn(&str) -> String,
    ) {
        self.0.retain(|_, entry| entry.prune(at));
        let mut fields = Map::new();
        for (header, value) in headers {
            if let (Some(key), Ok(raw)) = (canonical(header.as_str()), value.to_str()) {
                // Known fields only; bound untrusted text before retaining it.
                let value = redact(raw)
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(128)
                    .collect::<String>();
                fields.insert(key, json!(value));
            }
        }
        // No headers must never make old quota data fresh again.
        if fields.is_empty() {
            return;
        }
        let resets = KINDS
            .into_iter()
            .filter_map(|kind| {
                fields
                    .get(&format!("reset-{kind}"))
                    .and_then(Value::as_str)
                    .and_then(|raw| reset_at(raw, at))
                    .map(|reset| (kind.to_string(), reset))
            })
            .collect();
        let endpoint = normalized_endpoint(base);
        let identity = Identity {
            provider: provider.into(),
            endpoint: endpoint.clone(),
            fingerprint: Sha256::digest(credential.as_bytes()).into(),
        };
        let id = self
            .0
            .get(&identity)
            .map(|entry| entry.id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if self.0.len() >= MAX_ENTRIES && !self.0.contains_key(&identity) {
            if let Some(oldest) = self
                .0
                .iter()
                .min_by_key(|(_, entry)| entry.at)
                .map(|(key, _)| key.clone())
            {
                self.0.remove(&oldest);
            }
        }
        self.0.insert(
            identity,
            Observation {
                id,
                provider: redact(provider),
                endpoint: redact(&display_endpoint(&endpoint)),
                at,
                fields,
                resets,
            },
        );
    }

    fn snapshot(&mut self, now: DateTime<Utc>) -> HashMap<String, Value> {
        self.0.retain(|_, entry| entry.prune(now));
        self.0
            .values()
            .map(|entry| (entry.id.clone(), entry.public(now)))
            .collect()
    }
}

pub(crate) fn note(
    provider: &str,
    base: &str,
    credential: &str,
    headers: &HeaderMap,
    redact: impl Fn(&str) -> String,
) {
    REGISTRY
        .locked()
        .get_or_insert_with(Registry::default)
        .note(provider, base, credential, headers, Utc::now(), redact);
}
pub(crate) fn snapshot() -> HashMap<String, Value> {
    REGISTRY
        .locked()
        .get_or_insert_with(Registry::default)
        .snapshot(Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kind_expiry_retains_the_other_quota_and_observation_identity() {
        let mut registry = Registry::default();
        let fields = headers(&[
            ("x-ratelimit-limit-requests", "50"),
            ("x-ratelimit-remaining-requests", "41"),
            ("x-ratelimit-reset-requests", "6ms"),
            ("x-ratelimit-limit-tokens", "1000"),
            ("x-ratelimit-remaining-tokens", "900"),
            ("x-ratelimit-reset-tokens", "2m"),
        ]);
        observe(
            &mut registry,
            "work",
            "https://example.com",
            "secret",
            &fields,
            at(),
        );
        let before = registry.snapshot(at());
        let id = before.keys().next().unwrap().clone();
        let during = registry.snapshot(at() + TimeDelta::seconds(10));
        assert_eq!(
            during.len(),
            1,
            "fast request window hid the still-current tokens"
        );
        let value = &during[&id];
        for field in ["limit", "remaining", "reset"] {
            assert!(value.get(format!("{field}-requests")).is_none());
        }
        assert_eq!(value["remaining-tokens"], "900");
        assert_eq!(value["at"], before[&id]["at"]);
        assert!(value["line"]
            .as_str()
            .unwrap()
            .contains("900/1000 tokens left, resets in 110s"));
        assert!(!value["line"].as_str().unwrap().contains("requests"));
        assert!(registry.snapshot(at() + TimeDelta::seconds(120)).is_empty());
    }

    #[test]
    fn kind_expiry_small_numeric_reset_remains_visible_and_labelled() {
        let mut registry = Registry::default();
        observe(
            &mut registry,
            "work",
            "https://example.com",
            "secret",
            &headers(&[
                ("x-ratelimit-remaining", "3"),
                ("x-ratelimit-limit", "10"),
                ("x-ratelimit-reset", "60"),
            ]),
            at(),
        );
        let snapshot = registry.snapshot(at());
        assert_eq!(
            snapshot.len(),
            1,
            "ambiguous number was misread as an expired epoch"
        );
        assert!(snapshot.values().next().unwrap()["line"]
            .as_str()
            .unwrap()
            .contains("3/10 requests left, reset 60 (unparsed)"));
        for raw in ["0", "60", "999999999", "100000000000"] {
            assert_eq!(reset_at(raw, at()), None, "{raw}");
        }
    }
    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000, 0).unwrap()
    }
    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        pairs
            .iter()
            .map(|(k, v)| {
                (
                    reqwest::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                    v.parse().unwrap(),
                )
            })
            .collect()
    }
    fn observe(
        registry: &mut Registry,
        name: &str,
        url: &str,
        key: &str,
        headers: &HeaderMap,
        at: DateTime<Utc>,
    ) {
        registry.note(name, url, key, headers, at, |s| {
            s.replace(key, "[REDACTED]")
        });
    }

    #[test]
    fn all_vendor_orders_normalize_every_supported_kind() {
        for kind in KINDS {
            for field in ["limit", "remaining", "reset"] {
                let expected = format!("{field}-{kind}");
                assert_eq!(
                    canonical(&format!("x-ratelimit-{field}-{kind}")),
                    Some(expected.clone())
                );
                assert_eq!(
                    canonical(&format!("anthropic-ratelimit-{kind}-{field}")),
                    Some(expected)
                );
            }
        }
        for field in ["limit", "remaining", "reset"] {
            assert_eq!(
                canonical(&format!("x-ratelimit-{field}")),
                Some(format!("{field}-requests"))
            );
        }
        assert_eq!(canonical("anthropic-ratelimit-surprise-secret"), None);
        assert_eq!(canonical("authorization"), None);
    }

    #[test]
    fn identities_isolate_names_and_credentials_and_normalize_urls() {
        assert_eq!(
            normalized_endpoint("HTTPS://EXAMPLE.COM:443/v1/?scope=a/#ignored"),
            "https://example.com/v1?scope=a/"
        );
        let mut registry = Registry::default();
        let first = headers(&[("x-ratelimit-remaining-requests", "41")]);
        let second = headers(&[("x-ratelimit-remaining-requests", "17")]);
        observe(
            &mut registry,
            "work",
            "HTTPS://EXAMPLE.COM:443/v1/",
            "work-secret",
            &first,
            at(),
        );
        observe(
            &mut registry,
            "personal",
            "https://example.com/v1",
            "personal-secret",
            &second,
            at(),
        );
        observe(
            &mut registry,
            "work",
            "https://example.com/v1",
            "rotated-secret",
            &second,
            at(),
        );
        assert_eq!(registry.snapshot(at()).len(), 3);
        let work_id = registry
            .snapshot(at())
            .into_iter()
            .find(|(_, v)| v["remaining-requests"] == "41")
            .unwrap()
            .0;
        observe(
            &mut registry,
            "work",
            "https://example.com/v1",
            "work-secret",
            &second,
            at(),
        );
        let snapshot = registry.snapshot(at());
        assert_eq!(
            snapshot.len(),
            3,
            "normalized aliases replace the same identity"
        );
        assert_eq!(snapshot[&work_id]["remaining-requests"], "17");
        let wire = json!(snapshot).to_string();
        for key in ["work-secret", "personal-secret", "rotated-secret"] {
            assert!(!wire.contains(key));
            assert!(!wire.contains(&format!("{:x}", Sha256::digest(key.as_bytes()))));
        }
        assert!(snapshot
            .values()
            .all(|v| v["endpoint"] == "https://example.com/v1"));
    }

    #[test]
    fn resets_parse_duration_rfc3339_and_epoch_seconds_or_millis() {
        for (raw, seconds) in [
            ("40s", 40),
            ("1m30s", 90),
            ("1h 2m", 3720),
            ("250ms", 0),
            (".5s", 0),
            ("1d", 86400),
        ] {
            assert_eq!(
                reset_at(raw, at())
                    .unwrap()
                    .signed_duration_since(at())
                    .num_seconds(),
                seconds
            );
        }
        let target = at() + TimeDelta::seconds(40);
        for raw in [
            target.to_rfc3339(),
            target.timestamp().to_string(),
            target.timestamp_millis().to_string(),
        ] {
            assert_eq!(reset_at(&raw, at()), Some(target));
        }
        for raw in [
            "vendor-window",
            "",
            "NaN",
            "inf",
            "-5s",
            "1.2.3s",
            "5s trailing",
            "1e300d",
        ] {
            assert_eq!(reset_at(raw, at()), None, "{raw}");
        }
    }

    #[test]
    fn snapshots_count_down_expire_and_label_unknown_resets() {
        let mut registry = Registry::default();
        let fields = headers(&[
            ("anthropic-ratelimit-requests-remaining", "41"),
            ("anthropic-ratelimit-requests-limit", "50"),
            ("anthropic-ratelimit-requests-reset", "40s"),
        ]);
        observe(
            &mut registry,
            "work",
            "https://example.com/",
            "secret",
            &fields,
            at(),
        );
        let snapshot = registry.snapshot(at() + TimeDelta::seconds(15));
        assert!(snapshot.values().next().unwrap()["line"]
            .as_str()
            .unwrap()
            .contains("41/50 requests left, resets in 25s"));
        assert!(registry.snapshot(at() + TimeDelta::seconds(40)).is_empty());
        observe(
            &mut registry,
            "work",
            "https://example.com",
            "secret",
            &headers(&[
                ("x-ratelimit-remaining-tokens", "100"),
                ("x-ratelimit-reset-tokens", "strange"),
            ]),
            at(),
        );
        assert!(registry.snapshot(at()).values().next().unwrap()["line"]
            .as_str()
            .unwrap()
            .contains("reset strange (unparsed)"));
        assert_eq!(registry.snapshot(at() + TimeDelta::seconds(299)).len(), 1);
        assert!(registry.snapshot(at() + TimeDelta::seconds(300)).is_empty());
    }

    #[test]
    fn no_header_refresh_no_field_carryover_and_independent_resets() {
        let mut registry = Registry::default();
        let fields = headers(&[
            ("x-ratelimit-remaining-requests", "41"),
            ("x-ratelimit-reset-requests", "10s"),
            ("x-ratelimit-remaining-tokens", "100"),
            ("x-ratelimit-reset-tokens", "2m"),
        ]);
        observe(
            &mut registry,
            "work",
            "https://example.com",
            "secret",
            &fields,
            at(),
        );
        observe(
            &mut registry,
            "work",
            "https://example.com",
            "secret",
            &HeaderMap::new(),
            at() + TimeDelta::seconds(5),
        );
        let partial = registry.snapshot(at() + TimeDelta::seconds(10));
        assert_eq!(partial.len(), 1);
        let value = partial.values().next().unwrap();
        assert!(value.get("remaining-requests").is_none());
        assert_eq!(value["remaining-tokens"], "100");
        assert_eq!(value["at"], at().to_rfc3339());
        assert!(registry.snapshot(at() + TimeDelta::seconds(120)).is_empty());
        observe(
            &mut registry,
            "work",
            "https://example.com",
            "secret",
            &fields,
            at(),
        );
        observe(
            &mut registry,
            "work",
            "https://example.com",
            "secret",
            &headers(&[("x-ratelimit-remaining-requests", "9")]),
            at() + TimeDelta::seconds(1),
        );
        let snapshot = registry.snapshot(at() + TimeDelta::seconds(11));
        let value = snapshot.values().next().unwrap();
        assert_eq!(value["remaining-requests"], "9");
        assert!(value.get("remaining-tokens").is_none());
        assert!(value.get("reset-requests").is_none());
        assert!(
            registry.snapshot(at() - TimeDelta::seconds(1)).is_empty(),
            "clock rollback must not extend freshness"
        );
    }

    #[test]
    fn snapshots_bound_memory_and_remove_endpoint_secrets() {
        let mut registry = Registry::default();
        let fields = headers(&[
            ("x-ratelimit-remaining-requests", "credential-secret"),
            ("x-ratelimit-reset-requests", &"a".repeat(1024)),
            ("x-ratelimit-unknown", "do-not-retain"),
        ]);
        for n in 0..MAX_ENTRIES + 1 {
            observe(
                &mut registry,
                &format!("p{n}"),
                "https://user:password@example.com/v1?token=query-secret",
                "credential-secret",
                &fields,
                at() + TimeDelta::milliseconds(n as i64),
            );
        }
        let snapshot = registry.snapshot(at() + TimeDelta::seconds(1));
        assert_eq!(snapshot.len(), MAX_ENTRIES);
        assert!(
            !snapshot.values().any(|v| v["provider"] == "p0"),
            "oldest identity evicted"
        );
        let wire = json!(snapshot).to_string();
        for secret in [
            "credential-secret",
            "password",
            "query-secret",
            "do-not-retain",
        ] {
            assert!(!wire.contains(secret), "{secret}");
        }
        assert!(snapshot
            .values()
            .all(|v| v["reset-requests"].as_str().unwrap().len() == 128));
    }
}
