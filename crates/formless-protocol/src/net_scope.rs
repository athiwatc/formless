//! Exact host authority, explicit wildcard authority, and offline PSL warnings.
use std::{collections::HashSet, sync::OnceLock};

/// Inputs are already canonical host names, with ports separated by the caller.
/// A wildcard covers proper child labels; it never includes the apex itself.
pub fn host_covers(granted: &str, requested: &str) -> bool {
    let normalize = |host: &str| {
        let host = host.trim_matches(['[', ']']);
        host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase()
    };
    let granted = normalize(granted);
    let requested = normalize(requested);
    let valid = |host: &str| {
        let host = host.strip_prefix("*.").unwrap_or(host);
        !host.is_empty() && !host.split('.').any(str::is_empty)
    };
    if !valid(&granted) || !valid(&requested) {
        return false;
    }
    match (granted.strip_prefix("*."), requested.strip_prefix("*.")) {
        (None, None) => granted == requested,
        (Some(base), None) => requested.ends_with(&format!(".{base}")),
        (Some(base), Some(child)) => child == base || child.ends_with(&format!(".{base}")),
        (None, Some(_)) => false,
    }
}

struct Rules {
    exact: HashSet<String>,
    wildcard: HashSet<String>,
    exception: HashSet<String>,
    invalid: bool,
}
fn rules() -> &'static Rules {
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES.get_or_init(|| {
        let mut rules = Rules {
            exact: HashSet::new(),
            wildcard: HashSet::new(),
            exception: HashSet::new(),
            invalid: false,
        };
        for line in include_str!("../data/public_suffix_list.dat")
            .lines()
            .map(str::trim)
        {
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            let (set, domain) = if let Some(domain) = line.strip_prefix('!') {
                (&mut rules.exception, domain)
            } else if let Some(domain) = line.strip_prefix("*.") {
                (&mut rules.wildcard, domain)
            } else {
                (&mut rules.exact, line)
            };
            // The pinned upstream list is valid. Rejecting a malformed rule
            // instead of guessing keeps this warning-only classifier bounded.
            match idna::domain_to_ascii(domain) {
                Ok(domain) => {
                    set.insert(domain.to_ascii_lowercase());
                }
                Err(_) => {
                    rules.invalid = true;
                }
            }
        }
        rules
    })
}

/// Whether the whole host is a prevailing public suffix, including PRIVATE
/// rules. For this boolean, only an exact rule or a one-label wildcard can
/// make the entire input a suffix; exception rules remove that final label.
pub fn is_public_suffix(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
        return false;
    }
    let Ok(host) = idna::domain_to_ascii(host) else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    let rules = rules();
    if rules.exception.contains(&host) {
        return false;
    }
    rules.exact.contains(&host)
        || host
            .split_once('.')
            .is_some_and(|(_, tail)| rules.wildcard.contains(tail))
        || !host.contains('.') // PSL prevailing '*' rule, including unknown TLDs.
}

pub fn scope_warning(spec: &str) -> Option<String> {
    let scope = spec.strip_prefix("net:")?;
    if scope.starts_with('$') || scope.starts_with('[') {
        return None;
    }
    let host = scope
        .rsplit_once(':')
        .filter(|(host, port)| !host.contains(':') && port.parse::<u16>().is_ok())
        .map_or(scope, |(host, _)| host);
    if rules().invalid {
        return Some(
            "PUBLIC SUFFIX classification is unavailable: review this network grant conservatively"
                .into(),
        );
    }
    let (wildcard, base) = host
        .strip_prefix("*.")
        .map_or((false, host), |base| (true, base));
    if is_public_suffix(base) {
        Some(format!(
            "PUBLIC SUFFIX boundary: {base} can span independently owned sites (ICANN/PRIVATE PSL or prevailing '*' rule). {}",
            if wildcard {
                "This wildcard grants every subdomain, including unrelated tenants."
            } else {
                "This grant is exact only; it does not authorize its subdomains."
            }
        ))
    } else if wildcard {
        Some(format!("Wildcard: every subdomain of {base}; the apex itself is excluded. Subdomain labels can carry data."))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_and_explicit_wildcard_authority_are_distinct() {
        assert!(host_covers("api.example.com", "api.example.com"));
        assert!(!host_covers("example.com", "api.example.com"));
        assert!(host_covers("*.example.com", "a.b.example.com"));
        assert!(!host_covers("*.example.com", "example.com"));
        assert!(!host_covers("*.example.com", "evil-example.com"));
        assert!(host_covers("*.example.com", "*.sub.example.com"));
        assert!(!host_covers("sub.example.com", "*.sub.example.com"));
        assert!(!host_covers("*.sub.example.com", "*.example.com"));
        assert!(host_covers("[::1]", "::1"));
        for bad in [".foo", ".a.foo", "a..foo", "a.foo..", ""] {
            assert!(!host_covers("*.foo", bad), "{bad}");
        }
    }
    #[test]
    fn full_offline_psl_includes_private_wildcard_exception_and_idna_rules() {
        assert!(
            !rules().invalid,
            "every pinned PSL rule must normalize successfully"
        );
        assert!(
            rules().exact.len() > 9_000
                && rules().wildcard.len() > 100
                && !rules().exception.is_empty()
        );
        for suffix in [
            "com",
            "co.uk",
            "github.io",
            "workers.dev",
            "a.ck",
            "公司.cn",
            "unknown-tld",
        ] {
            assert!(is_public_suffix(suffix), "{suffix}");
        }
        for registrable in [
            "example.com",
            "account.github.io",
            "tenant.workers.dev",
            "www.ck",
            "a.www.ck",
            "a.b.ck",
            "city.kawasaki.jp",
            "127.0.0.1",
            "[::1]",
        ] {
            assert!(!is_public_suffix(registrable), "{registrable}");
        }
        assert!(is_public_suffix("foo.kawasaki.jp"));
        assert_eq!(
            is_public_suffix("公司.cn"),
            is_public_suffix("xn--55qx5d.cn")
        );
        assert!(scope_warning("net:*.github.io:443")
            .unwrap()
            .contains("PUBLIC SUFFIX"));
        assert!(scope_warning("net:*.example.com")
            .unwrap()
            .contains("Wildcard"));
        assert!(scope_warning("net:example.com").is_none());
        assert!(scope_warning("net:[::1]:8080").is_none());
    }
}
