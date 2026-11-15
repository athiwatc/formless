//! Provider application helper against an owned endpoint/private profile.
//! Approval/permission gates are unchanged; no live provider or credential use.
use super::*;
use axum::{extract::OriginalUri, routing::get, Json, Router};

#[tokio::test]
async fn provider_upsert_preserves_url_remainder_and_probes_owned_endpoint() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    env.remove("FORMLESS_BASE_URL");
    env.remove("FORMLESS_MODEL");
    struct Owned(std::path::PathBuf);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let owned = Owned(std::env::temp_dir().join(format!("fl-url-case-{}", uuid::Uuid::new_v4())));
    let mut paths = Paths::from_dirs(owned.0.join("data"), owned.0.join("config")).unwrap();
    paths.profile = format!("fixture-url-{}", uuid::Uuid::new_v4().simple());
    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.save(&paths).unwrap();
    let daemon = Daemon::new(paths.clone()).unwrap();
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let router = Router::new().fallback(get({
        let seen = seen.clone();
        move |OriginalUri(uri): OriginalUri| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(uri.to_string());
                Json(json!({"data":[]}))
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let input = format!("HtTp://{endpoint}/Case/Keep%2FCase/");
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        daemon.harness_apply(
            "providers.upsert",
            &json!({"name":"casefixture", "base_url":input, "model":"fixture"}),
        ),
    )
    .await;
    let rejected = daemon
        .harness_apply(
            "providers.upsert",
            &json!({"name":"unsupported", "base_url":format!("FTP://{endpoint}/Case")}),
        )
        .await;
    server.abort();
    let _ = server.await;
    result
        .expect("owned URL fixture exceeded its deadline")
        .unwrap();
    assert!(rejected.is_err());
    let saved = Config::load_file_only(&paths).unwrap();
    assert_eq!(
        saved.providers["casefixture"].base_url,
        format!("http://{endpoint}/Case/Keep%2FCase")
    );
    assert_eq!(saved.providers["casefixture"].model, "fixture");
    assert!(!saved.providers.contains_key("unsupported"));
    assert_eq!(*seen.lock().unwrap(), ["/Case/Keep%2FCase/models"]);
}
