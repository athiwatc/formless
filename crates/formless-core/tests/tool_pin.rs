//! Pinned Tools are always listed to the model (SPEC §7.1).

use formless_core::tools::{is_builtin, tool_definitions};
use formless_core::Store;
use formless_runtime::Capabilities;

fn registry_names(defs: &[serde_json::Value]) -> Vec<String> {
    defs.iter()
        .map(|d| {
            d["function"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .filter(|n| !is_builtin(n))
        .collect()
}

#[test]
fn pinned_tools_are_listed_first_from_the_next_listing() {
    let store = Store::open_in_memory().unwrap();
    let caps = Capabilities::parse(&[]).unwrap();
    let mut ids = Vec::new();
    for name in ["alpha", "beta", "zeta"] {
        let (info, _) = store
            .create_tool_version(
                name,
                "d",
                "{}",
                "{}",
                "function run(i){}",
                &caps,
                64,
                "agent",
            )
            .unwrap();
        ids.push(info.id);
    }
    // Only one Tool fits the inline listing; zeta is the least used.
    store
        .record_invocation(
            &ids[0], 1, None, None, None, "success", "", "{}", "{}", "", 1, 200,
        )
        .unwrap();
    let old = store.create_session(None).unwrap();
    assert_eq!(
        registry_names(&tool_definitions(&store, Some(&old.id), 1).unwrap()),
        ["alpha"]
    );

    store.set_tool_pinned(&ids[2], true).unwrap();
    // A Session already listing keeps its cached prefix…
    assert_eq!(
        registry_names(&tool_definitions(&store, Some(&old.id), 1).unwrap()),
        ["alpha"]
    );
    // …a new one lists the pinned Tool first, beyond the inline limit.
    let new = store.create_session(None).unwrap();
    assert_eq!(
        registry_names(&tool_definitions(&store, Some(&new.id), 1).unwrap()),
        ["zeta"]
    );
    let fresh = store.create_session(None).unwrap();
    assert_eq!(
        registry_names(&tool_definitions(&store, Some(&fresh.id), 3).unwrap()),
        ["zeta", "alpha", "beta"]
    );
}
