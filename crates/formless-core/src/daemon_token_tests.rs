//! Calibrated estimates use physical request input, not previous Session totals.
use super::*;
use axum::response::IntoResponse;

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn fixture(
    env: &crate::credential_env::EnvGuard,
    base: &str,
    style: &str,
) -> (Fixture, Paths, Config, Arc<Daemon>, String) {
    env.set("FORMLESS_API_KEY", "token-fixture-key");
    env.remove("FORMLESS_BASE_URL");
    env.remove("FORMLESS_MODEL");
    env.set("NO_PROXY", "127.0.0.1,localhost");
    let root = Fixture(std::env::temp_dir().join(format!("fl-tokens-{}", uuid::Uuid::new_v4())));
    let paths = Paths::from_dirs(root.0.join("data"), root.0.join("config")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = base.into();
    cfg.provider.model = "model".into();
    cfg.provider.api_style = style.into();
    cfg.provider.max_retries = 0;
    cfg.provider
        .model_context_tokens
        .insert("model".into(), 100_000);
    cfg.naming.enabled = false;
    cfg.skills.auto_create = false;
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let sid = store.create_session(None).unwrap().id;
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    (root, paths, cfg, daemon, sid)
}

struct Server(Option<tokio::task::JoinHandle<()>>);
impl Server {
    async fn stop(mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
            let _ = task.await;
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}

#[tokio::test]
async fn each_api_calibrates_the_dispatched_body_and_stream_snapshots_learn_once() {
    let env = crate::credential_env::EnvGuard::isolated();
    for style in ["chat", "responses", "anthropic"] {
        let seen = Arc::new(StdMutex::new(Vec::<Value>::new()));
        let captured = seen.clone();
        let handler = axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let seen = captured.clone();
            async move {
                seen.lock().unwrap().push(body.clone());
                let prompt = crate::tokens::request(&body) as u64 * 3;
                if body["stream"] == true {
                    // Identical cumulative counters in two chunks are one
                    // physical request observation, not two EMA updates.
                    let usage = json!({"prompt_tokens":prompt,"completion_tokens":1});
                    return format!(
                        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                        json!({"choices":[{"delta":{"content":"ok"}}],"usage":usage}),
                        json!({"choices":[{"delta":{},"finish_reason":"stop"}],"usage":usage})
                    )
                    .into_response();
                }
                if body.get("system").is_some() {
                    axum::Json(
                        json!({"content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn",
                        "usage":{"input_tokens":prompt,"output_tokens":1}}),
                    )
                    .into_response()
                } else if body.get("input").is_some() {
                    axum::Json(json!({"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"ok"}]}],
                        "usage":{"input_tokens":prompt,"output_tokens":1}})).into_response()
                } else {
                    axum::Json(
                        json!({"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],
                        "usage":{"prompt_tokens":prompt,"completion_tokens":1}}),
                    )
                    .into_response()
                }
            }
        });
        let app = axum::Router::new()
            .route("/chat/completions", handler.clone())
            .route("/responses", handler.clone())
            .route("/messages", handler);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = Server(Some(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        })));
        let (_root, _paths, cfg, daemon, _) = fixture(&env, &base, style);
        let provider = daemon.provider(&cfg).await.unwrap();
        let messages = vec![
            json!({"role":"system","content":"System: 你好"}),
            json!({"role":"user","content":"JSON {\"code\":true} ไทย"}),
        ];
        let raw = provider.prompt_tokens(&messages, &[], "model");
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            provider.complete(&messages, "model", None),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            crate::tokens::request(&seen.lock().unwrap()[0]),
            raw,
            "{style} preview must match dispatch"
        );
        let key = crate::tokens::provider_key("default", &cfg.provider);
        let ratio = daemon
            .with_reader(|st| st.token_ratio(&key, "model"))
            .unwrap();
        assert!((ratio - 1.4).abs() < 1e-9, "{style}: {ratio}");
        if style == "chat" {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                provider.chat_stream(
                    &messages,
                    &[],
                    "stream-model",
                    None,
                    &CancellationToken::new(),
                    |_, _| {},
                ),
            )
            .await
            .unwrap()
            .unwrap();
            let ratio = daemon
                .with_reader(|st| st.token_ratio(&key, "stream-model"))
                .unwrap();
            assert!(
                (ratio - 1.4).abs() < 1e-9,
                "repeated stream snapshots learned once: {ratio}"
            );
        }
        server.stop().await;
    }
}

#[tokio::test]
async fn fallback_and_named_provider_samples_are_isolated_and_failed_usage_is_not_training() {
    let env = crate::credential_env::EnvGuard::isolated();
    let app = axum::Router::new().route("/chat/completions", axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
        let model = body["model"].as_str().unwrap();
        let prompt = crate::tokens::request(&body) as u64 * 3;
        if model == "primary" || model == "failed" {
            return (axum::http::StatusCode::SERVICE_UNAVAILABLE, axum::Json(json!({"error":{"message":"fixture failure"},"usage":{"prompt_tokens":999999}}))).into_response();
        }
        if model == "stream-error" {
            return format!("data: {}\n\n", json!({"error":{"message":"stream fixture error","type":"server_error"},"usage":{"prompt_tokens":prompt}})).into_response();
        }
        let mut response = json!({"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]});
        if model != "missing" { response["usage"] = json!({"prompt_tokens": if model == "zero" {0} else {prompt},"completion_tokens":1}); }
        axum::Json(response).into_response()
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = Server(Some(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    })));
    let (_root, paths, mut cfg, daemon, _) = fixture(&env, &base, "chat");
    cfg.provider.fallback_models = vec!["fallback".into()];
    let provider = Provider::with_key(cfg.provider.clone(), "fixture".into(), daemon.http.clone())
        .with_prompt_hook(daemon.prompt_hook());
    let messages = [json!({"role":"user","content":"calibration request"})];
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        provider.complete_result(&messages, "primary", None),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.model_used, "fallback");
    let default_key = crate::tokens::provider_key("default", &cfg.provider);
    assert_eq!(
        daemon
            .with_reader(|st| st.token_ratio(&default_key, "primary"))
            .unwrap(),
        1.0
    );
    assert!(
        (daemon
            .with_reader(|st| st.token_ratio(&default_key, "fallback"))
            .unwrap()
            - 1.4)
            .abs()
            < 1e-9
    );
    cfg.provider.fallback_models.clear();
    cfg.providers.insert("named".into(), cfg.provider.clone());
    cfg.roles.insert(
        "reader".into(),
        crate::config::RoleCfg {
            provider: Some("named".into()),
            model: "fallback".into(),
            ..Default::default()
        },
    );
    cfg.save(&paths).unwrap();
    env.set("FORMLESS_API_KEY_NAMED", "fixture");
    let target = daemon
        .target(&cfg, "not-needed", Some("role:reader"))
        .await
        .unwrap();
    assert_eq!(target.provider.name(), "named");
    let named_key = crate::tokens::provider_key("named", target.provider.settings());
    assert_eq!(
        daemon
            .with_reader(|st| st.token_ratio(&named_key, "fallback"))
            .unwrap(),
        1.0
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        target.provider.complete(&messages, &target.model, None),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        (daemon
            .with_reader(|st| st.token_ratio(&named_key, "fallback"))
            .unwrap()
            - 1.4)
            .abs()
            < 1e-9
    );
    let provider = daemon.provider(&cfg).await.unwrap();
    for model in ["missing", "zero", "failed"] {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            provider.complete(&messages, model, None),
        )
        .await
        .unwrap();
        assert_eq!(
            daemon
                .with_reader(|st| st.token_ratio(&default_key, model))
                .unwrap(),
            1.0,
            "{model} must not train"
        );
    }
    assert!(tokio::time::timeout(
        std::time::Duration::from_secs(5),
        provider.chat_stream(
            &messages,
            &[],
            "stream-error",
            None,
            &CancellationToken::new(),
            |_, _| {}
        )
    )
    .await
    .unwrap()
    .is_err());
    assert_eq!(
        daemon
            .with_reader(|st| st.token_ratio(&default_key, "stream-error"))
            .unwrap(),
        1.0
    );
    server.stop().await;
}

#[tokio::test]
async fn current_context_compaction_and_routing_share_checkpoint_tools_image_and_calibration() {
    let env = crate::credential_env::EnvGuard::isolated();
    let (root, paths, mut cfg, daemon, sid) = fixture(&env, "http://127.0.0.1:9", "chat");
    cfg.provider.vision = Some(true);
    cfg.provider.image_detail = "high".into();
    cfg.save(&paths).unwrap();
    let image = root.0.join("image.png");
    image::DynamicImage::new_rgb8(1024, 1024)
        .save(&image)
        .unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let old = store
        .append_message(&sid, "user", "OLD HISTORY MUST NOT BE COUNTED")
        .unwrap();
    store
        .save_checkpoint(&sid, "saved summary", old.seq)
        .unwrap();
    for i in 0..4 {
        store
            .append_message(
                &sid,
                "user",
                &format!("你好 task{i} {}", "word ".repeat(20)),
            )
            .unwrap();
        store
            .append_message(&sid, "assistant", "tool response")
            .unwrap();
    }
    store.append_message_meta(&sid, "user", "image please", Some(json!({"attachments":[{"id":"image","name":"image.png","path":image,"mime":"image/png","bytes":100}]}).to_string())).unwrap();
    let provider = Provider::with_key(cfg.provider.clone(), "fixture".into(), daemon.http.clone());
    let ctx = daemon
        .session_context(&sid, &provider, "model", 100_000, 0.5)
        .unwrap()
        .0;
    assert!(!serde_json::to_string(&ctx).unwrap().contains("OLD HISTORY"));
    assert!(ctx[0]["content"]
        .as_str()
        .unwrap()
        .contains("saved summary"));
    let defs = daemon
        .with_store(|st| toolreg::tool_definitions(st, Some(&sid), cfg.tools.inline_limit))
        .unwrap();
    let raw = provider.prompt_tokens(&ctx, &defs, "model");
    let key = crate::tokens::provider_key("default", &cfg.provider);
    // A stale small measured prompt must not mask current context growth.
    store
        .record_usage(
            &sid,
            "old-run",
            "model",
            &crate::provider::Usage {
                prompt_tokens: 1,
                ..Default::default()
            },
        )
        .unwrap();
    for _ in 0..12 {
        store
            .observe_prompt_tokens(&key, "model", raw, raw as u64 * 3)
            .unwrap();
    }
    let expected = daemon
        .calibrated_prompt_tokens(&provider, "model", &ctx, &defs)
        .unwrap();
    let report = daemon
        .handle(Request::ContextUsage {
            session_id: sid.clone(),
        })
        .await
        .unwrap();
    assert_eq!(report["used_tokens"], expected as u64);
    assert_eq!(report["measured"], false);
    assert_eq!(report["breakdown"]["attachments"], 1);
    let window = (raw as u32 * 4).max(1);
    assert!(
        daemon
            .session_context(&sid, &provider, "model", window, 0.5)
            .unwrap()
            .1
    );
    let mut routing_cfg = cfg.clone();
    routing_cfg.provider.routing_complex_chars = provider.prompt_tokens(&ctx, &[], "model") * 8;
    // Remove the old tool-count shortcut from this assertion to exercise size.
    assert!(daemon
        .routing_complex(&provider, "model", &ctx, &[], &routing_cfg)
        .unwrap());
    assert!(!daemon
        .routing_complex(&provider, "unseen", &ctx, &[], &routing_cfg)
        .unwrap());
    assert!(
        !daemon
            .session_context(&sid, &provider, "unseen", window, 0.5)
            .unwrap()
            .1
    );
    let empty = provider.prompt_tokens(
        &[json!({"role":"user","content":"image please"})],
        &[],
        "model",
    );
    assert!(provider.prompt_tokens(&ctx, &[], "model") > empty + 85);
}
