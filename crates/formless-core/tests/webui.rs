//! Opt-in localhost web UI (SPEC §12): Host/Origin checks, one-time
//! token for a session cookie, protocol over a local WebSocket.
#![cfg(unix)]

use formless_core::{Config, Paths};
use formless_protocol::Request;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn local_web_ui_is_locked_to_this_origin_and_a_one_time_token() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::path::PathBuf::from(format!("/tmp/fl-web-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-web-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    Config::default().save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    for _ in 0..50 {
        if formless_core::daemon::daemon_reachable(&paths) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let r = daemon
        .handle(Request::StartWebUi { port: None })
        .await
        .unwrap();
    let port = r["port"].as_u64().unwrap();
    let url = r["url"].as_str().unwrap();
    let token = url.split("#local=").nth(1).unwrap().to_string();
    let origin = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    // The page: served with the CSP and the local-mode marker.
    let page = http.get(format!("{origin}/")).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .contains("default-src 'none'"));
    assert!(page.text().await.unwrap().contains("formless-local"));
    // The installable shell: manifest, service worker and icons.
    for (path, kind) in [
        ("/manifest.webmanifest", "application/manifest+json"),
        ("/sw.js", "text/javascript"),
        ("/transport.mjs", "text/javascript"),
        ("/icon-192.png", "image/png"),
        ("/icon-maskable-192.png", "image/png"),
        ("/icon-maskable-512.png", "image/png"),
    ] {
        let r = http.get(format!("{origin}{path}")).send().await.unwrap();
        assert_eq!(r.status(), 200, "{path}");
        assert_eq!(r.headers()["content-type"], kind, "{path}");
        if path == "/transport.mjs" {
            assert!(r
                .text()
                .await
                .unwrap()
                .contains("export class WireEndpoint"));
        }
    }
    let manifest = http
        .get(format!("{origin}/manifest.webmanifest"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let icons = serde_json::from_str::<serde_json::Value>(&manifest).unwrap();
    let icons = icons["icons"].as_array().unwrap();
    assert!(icons
        .iter()
        .any(|icon| { icon["src"] == "icon-192.png" && icon["purpose"] == "any" }));
    assert!(icons
        .iter()
        .any(|icon| { icon["src"] == "icon-512.png" && icon["purpose"] == "any" }));
    assert!(icons
        .iter()
        .any(|icon| { icon["src"] == "icon-maskable-192.png" && icon["purpose"] == "maskable" }));
    assert!(icons
        .iter()
        .any(|icon| { icon["src"] == "icon-maskable-512.png" && icon["purpose"] == "maskable" }));
    let csp = http
        .get(format!("{origin}/"))
        .send()
        .await
        .unwrap()
        .headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(csp.contains("manifest-src 'self'") && csp.contains("worker-src 'self'"));
    // DNS rebinding: another Host name is refused.
    let rebound = http
        .get(format!("{origin}/"))
        .header("host", "evil.example:80")
        .send()
        .await
        .unwrap();
    assert_eq!(rebound.status(), 403);

    // Token exchange: wrong origin refused; right one gets a cookie once.
    let foreign = http
        .post(format!("{origin}/auth"))
        .header("origin", "http://evil.example")
        .body(token.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), 403);
    let ok = http
        .post(format!("{origin}/auth"))
        .header("origin", &origin)
        .body(token.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 204);
    let cookie = ok.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(
        cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"),
        "{cookie}"
    );
    let session = cookie.split(';').next().unwrap().to_string();
    let reused = http
        .post(format!("{origin}/auth"))
        .header("origin", &origin)
        .body(token)
        .send()
        .await
        .unwrap();
    assert_eq!(reused.status(), 401, "the link works once");

    // WebSocket: refused without the cookie, then a Ping round trip.
    let ws_url = format!("ws://127.0.0.1:{port}/ws");
    let req = |cookie: Option<&str>| {
        let mut r = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
            ws_url.as_str(),
        )
        .unwrap();
        r.headers_mut().insert("origin", origin.parse().unwrap());
        if let Some(c) = cookie {
            r.headers_mut().insert("cookie", c.parse().unwrap());
        }
        r
    };
    assert!(tokio_tungstenite::connect_async(req(None)).await.is_err());
    let (mut ws, _) = tokio_tungstenite::connect_async(req(Some(&session)))
        .await
        .unwrap();
    let env = formless_protocol::RequestEnvelope {
        id: 7,
        protocol: formless_protocol::PROTOCOL_VERSION,
        request: Request::Ping,
    };
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&env).unwrap(),
    ))
    .await
    .unwrap();
    let reply = loop {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let text = msg.into_text().unwrap().to_string();
        if text.contains("\"id\":7") {
            break text;
        }
    };
    assert!(reply.contains("\"ok\":true"), "{reply}");

    let stopped = daemon.handle(Request::StopWebUi).await.unwrap();
    assert_eq!(stopped["stopped"], true);
}
