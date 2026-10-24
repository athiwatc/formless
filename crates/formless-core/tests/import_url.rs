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
