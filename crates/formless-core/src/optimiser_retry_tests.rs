//! Owned loopback measurement failures: no model/provider operation is real.
use super::*;
use axum::response::IntoResponse;
use std::sync::atomic::{AtomicUsize, Ordering};

async fn evaluate_failure(message: &str, first_server_failure: bool) -> (usize, String) {
    let count = Arc::new(AtomicUsize::new(0));
    let router = axum::Router::new().route(
        "/chat/completions",
        axum::routing::post({
            let count = count.clone();
            let message = message.to_owned();
            move || {
                let count = count.clone();
                let message = message.clone();
                async move {
                    let n = count.fetch_add(1, Ordering::SeqCst);
                    if first_server_failure && n == 0 {
                        return (
                            axum::http::StatusCode::SERVICE_UNAVAILABLE,
                            "fixture unavailable",
                        )
                            .into_response();
                    }
                    let error = json!({"error":{"type":"invalid_request_error","message":message}});
                    (
                        [("content-type", "text/event-stream")],
                        format!("data: {error}\n\n"),
                    )
                        .into_response()
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cfg = crate::config::ProviderCfg {
        base_url: format!("http://{}", listener.local_addr().unwrap()),
        max_retries: 0,
        fallback_models: Vec::new(),
        ..Default::default()
    };
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let provider = Provider::new(cfg, "owned-loopback-fixture-key".into()).unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        evaluate_prompt(&provider, "fixture prompt", "fixture", None, 1),
    )
    .await;
    server.abort();
    let _ = server.await;
    let error = result.expect("owned prompt fixture timed out").unwrap_err();
    (count.load(Ordering::SeqCst), format!("{error:#}"))
}

#[tokio::test]
async fn prompt_evaluation_stops_after_one_permanent_stream_or_reset_failure() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    for message in [
        "model does not support streaming",
        "downstream rejected the schema",
        "password reset required",
    ] {
        let (calls, error) = evaluate_failure(message, false).await;
        assert!(error.contains(message), "{error}");
        assert_eq!(
            calls, 1,
            "permanent failure must not cost a second request: {error}"
        );
    }
}

#[tokio::test]
async fn prompt_evaluation_keeps_one_retry_for_an_actual_http_server_failure() {
    let env = crate::credential_env::EnvGuard::isolated();
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let (calls, error) = evaluate_failure("model does not support streaming", true).await;
    assert!(
        error.contains("model does not support streaming"),
        "{error}"
    );
    assert_eq!(calls, 2, "one additional request for the initial HTTP503");
}
