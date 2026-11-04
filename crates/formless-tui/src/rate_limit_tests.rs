// Real HTTP headers -> Provider observation -> wire response -> /usage note.
#[test]
fn rate_limits_vendor_headers_reach_the_tui_and_status_line() {
    use serde_json::json;
    use std::io::{Read, Write};
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for (name, headers, expected) in [
        ("openai-fixture", "x-ratelimit-limit-requests: 50\r\nx-ratelimit-remaining-requests: 41\r\nx-ratelimit-reset-requests: 40s\r\n", "41/50 requests left, resets in 40s"),
        ("anthropic-fixture", "anthropic-ratelimit-input-tokens-limit: 5000\r\nanthropic-ratelimit-input-tokens-remaining: 4000\r\nanthropic-ratelimit-output-tokens-limit: 1000\r\nanthropic-ratelimit-output-tokens-remaining: 900\r\n", "4000/5000 input-tokens left"),
        ("openrouter-fixture", "x-ratelimit-limit: 20\r\nx-ratelimit-remaining: 0\r\nx-ratelimit-reset: vendor-window\r\n", "0/20 requests left, reset vendor-window (unparsed)"),
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(std::time::Instant::now() < deadline, "provider never connected");
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            socket.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut received = Vec::new();
            loop {
                let mut buf = [0; 4096];
                let n = socket.read(&mut buf).unwrap();
                assert!(n > 0);
                received.extend_from_slice(&buf[..n]);
                if let Some(end) = received.windows(4).position(|s| s == b"\r\n\r\n") {
                    let text = String::from_utf8_lossy(&received[..end]);
                    let size = text.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|s| s.trim().parse::<usize>().unwrap())).unwrap_or(0);
                    if received.len() >= end + 4 + size { break; }
                }
            }
            // OpenRouter exposes platform-limit headers on 429s.
            let (status, body) = if name == "openrouter-fixture" {
                ("429 Too Many Requests", r#"{"error":{"message":"rate limited"}}"#)
            } else {
                ("200 OK", r#"{"choices":[{"message":{"content":"ok"}}]}"#)
            };
            write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\n{headers}Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        });
        let cfg = formless_core::config::ProviderCfg {
            base_url: base.clone(),
            fallback_models: Vec::new(),
            max_retries: 0,
            ..Default::default()
        };
        let provider = formless_core::provider::Provider::new(cfg, "fixture-secret".into()).unwrap().with_name(name);
        let completion = runtime.block_on(provider.complete(&[json!({"role":"user","content":"hi"})], "fixture", None));
        if name == "openrouter-fixture" { assert!(completion.is_err()); } else { assert_eq!(completion.unwrap(), "ok"); }
        server.join().unwrap();
        let limits = formless_core::provider::rate_limits();
        let entry = limits.values().find(|entry| entry["provider"] == name && entry["endpoint"] == base).expect("scoped provider observation");
        let line = entry["line"].as_str().unwrap();
        assert!(line.contains(expected), "{line}");
        assert!(line.contains(name));
        if name == "anthropic-fixture" { assert!(line.contains("900/1000 output-tokens left")); }
        assert!(!serde_json::to_string(&limits).unwrap().contains("fixture-secret"));

        let mut app = test_app();
        if name == "anthropic-fixture" {
            let mut config = formless_core::Config::default();
            config.naming.enabled = false;
            config.save(&app.paths).unwrap();
            let daemon = std::sync::Arc::new(formless_core::daemon::Daemon::new(app.paths.clone()).unwrap());
            let status = runtime.block_on(daemon.handle(Request::Status)).unwrap();
            let direct = runtime.block_on(daemon.handle(Request::RateLimits)).unwrap();
            for result in [&status["rate_limits"], &direct] {
                assert!(result.as_object().unwrap().values().any(|value| value["provider"] == name && value["line"] == line), "{result}");
            }
        }
        let (client, sent) = DaemonClient::recording();
        let (tx, inbox) = mpsc::channel();
        app.client = Some(client);
        app.inbox = Some(inbox);
        app.request(Pending::RateLimits, Request::RateLimits);
        let request: RequestEnvelope = serde_json::from_str(&sent.try_recv().unwrap()).unwrap();
        tx.send(ServerMessage::Response { id: request.id, ok: true, result: Some(json!(limits)), error: None }).unwrap();
        app.drain_daemon();
        assert!(app.notes.iter().any(|note| note == line), "{:?}", app.notes);
    }
}
