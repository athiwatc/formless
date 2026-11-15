//! Importing Tool bundles from a URL or gist (SPEC §7.8).

use formless_core::import_url::{bundle_in_gist, check_bundle, resolve, Source};
use serde_json::json;

#[test]
fn urls_resolve_to_what_holds_the_bundle() {
    assert!(resolve("http://example.com/t.json").is_err(), "https only");
    assert!(resolve("file:///etc/passwd").is_err());
    assert_eq!(
        resolve(" https://example.com/t.json ").unwrap(),
        Source::Direct("https://example.com/t.json".into())
    );
    assert_eq!(
        resolve("https://gist.github.com/ada/0123456789abcdef0123456789abcdef#file-x").unwrap(),
        Source::Gist("0123456789abcdef0123456789abcdef".into())
    );
    assert!(resolve("https://gist.github.com/ada").is_err());
    assert_eq!(
        resolve("https://github.com/ada/tools/blob/main/dir/fetch.json?plain=1").unwrap(),
        Source::Direct("https://raw.githubusercontent.com/ada/tools/main/dir/fetch.json".into())
    );
}

#[test]
fn only_tool_bundles_pass() {
    assert!(check_bundle(&json!({"format": "formless.tool/1"})).is_ok());
    assert!(check_bundle(&json!({"format": "formless.tools/1", "tools": []})).is_ok());
    assert!(check_bundle(&json!({"format": "formless.skill/1"})).is_err());
    assert!(check_bundle(&json!({"name": "x"})).is_err());
}

#[test]
fn a_gist_yields_its_bundle_file() {
    let bundle = json!({"format": "formless.tool/1", "name": "fetch_page"});
    let gist = json!({"files": {
        "README.md": {"content": "# tools"},
        "fetch.json": {"content": bundle.to_string()},
    }});
    let (name, b, raw) = bundle_in_gist(&gist).unwrap();
    assert_eq!(name, "fetch.json");
    assert_eq!(b.unwrap()["name"], "fetch_page");
    assert!(raw.is_none());
    // A file too big for the API response is fetched from its raw URL.
    let big = json!({"files": {"all.json": {"truncated": true, "raw_url": "https://gist.githubusercontent.com/r"}}});
    assert_eq!(
        bundle_in_gist(&big).unwrap().2.as_deref(),
        Some("https://gist.githubusercontent.com/r")
    );
    assert!(bundle_in_gist(&json!({"files": {"a.txt": {"content": "hi"}}})).is_err());
}

#[test]
fn service_urls_use_normalized_scheme_host_and_default_port() {
    for authority in [
        "github.com",
        "GitHub.com",
        "www.github.com",
        "github.com:443",
        "WWW.GitHub.com:443",
    ] {
        let url = format!("HTTPS://{authority}/ada/tools/blob/main/dir/fetch.json?plain=1#file");
        assert_eq!(
            resolve(&url).unwrap(),
            Source::Direct(
                "https://raw.githubusercontent.com/ada/tools/main/dir/fetch.json".into()
            ),
            "{url}"
        );
    }
    for authority in [
        "Gist.GitHub.com",
        "www.gist.github.com",
        "gist.github.com:443",
        "GIST.GITHUBUSERCONTENT.COM",
        "www.gist.githubusercontent.com:443",
    ] {
        let url = format!("HTTPS://{authority}/ada/0123456789abcdef0123456789abcdef#file-x");
        assert_eq!(
            resolve(&url).unwrap(),
            Source::Gist("0123456789abcdef0123456789abcdef".into()),
            "{url}"
        );
    }
}

#[test]
fn other_authorities_keep_the_original_direct_url() {
    for url in [
        "https://github.com:8443/ada/tools/blob/main/fetch.json",
        "https://demo:placeholder@github.com/ada/tools/blob/main/fetch.json",
        "https://gist.github.com:8443/ada/0123456789abcdef0123456789abcdef",
        "https://github.com.example.com/ada/tools/blob/main/fetch.json",
        "https://github.com@remote.invalid/ada/tools/blob/main/fetch.json",
        "HTTPS://Example.COM:443/t.json?case=Kept#fragment",
    ] {
        assert_eq!(
            resolve(&format!(" {url} ")).unwrap(),
            Source::Direct(url.to_string()),
            "{url}"
        );
    }
    for url in [
        "HTTP://GitHub.com/ada/tools/blob/main/t.json",
        "FILE:///etc/passwd",
        "not a URL",
        "https://",
    ] {
        assert!(resolve(url).is_err(), "{url}");
    }
}
