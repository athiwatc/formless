//! Opt-in localhost web UI (SPEC §12): the relay web app served by the
//! daemon on 127.0.0.1, speaking the protocol over a local WebSocket.
//!
//! Security: off by default; a one-time token in the URL fragment is
//! exchanged for an HttpOnly, SameSite=Strict session cookie; every request
//! must name this server in `Host` (DNS rebinding), and state-changing ones
//! must come from its own `Origin`; the same CSP as the hosted relay.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::daemon::Daemon;

const INDEX: &str = include_str!("../../../relay/web/index.html");
const APP_JS: &str = include_str!("../../../relay/web/app.js");
const APP_CSS: &str = include_str!("../../../relay/web/app.css");
const MERMAID: &str = include_str!("../../../relay/web/vendor/mermaid.min.js");
// The installable app shell (SPEC §12).
const SW_JS: &str = include_str!("../../../relay/web/sw.js");
const MANIFEST: &str = include_str!("../../../relay/web/manifest.webmanifest");
const ICON_192: &[u8] = include_bytes!("../../../relay/web/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../../../relay/web/icon-512.png");

/// Same policy as the hosted relay frontend (relay/worker.js).
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
connect-src 'self' ws:; manifest-src 'self'; worker-src 'self'; base-uri 'none'; form-action 'none'; \
frame-ancestors 'none'";

const COOKIE: &str = "formless_session";

/// A running local web UI.
pub struct WebUi {
    pub port: u16,
    tokens: Mutex<HashSet<String>>,
    sessions: Mutex<HashSet<String>>,
    stop: tokio_util::sync::CancellationToken,
}

impl WebUi {
    /// A fresh one-time login URL.
    pub fn login_url(&self) -> String {
        let token = random_hex();
        self.tokens.lock().unwrap().insert(token.clone());
        format!("http://127.0.0.1:{}/#local={token}", self.port)
    }

    pub fn stop(&self) {
        self.stop.cancel();
    }

    fn host_ok(&self, headers: &HeaderMap) -> bool {
        let host = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default();
        host == format!("127.0.0.1:{}", self.port) || host == format!("localhost:{}", self.port)
    }

    fn origin_ok(&self, headers: &HeaderMap) -> bool {
        let origin = headers
            .get(header::ORIGIN)
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default();
        origin == format!("http://127.0.0.1:{}", self.port)
            || origin == format!("http://localhost:{}", self.port)
    }

    fn session_ok(&self, headers: &HeaderMap) -> bool {
        let cookies = headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .filter_map(|c| {
                c.trim()
                    .strip_prefix(&format!("{COOKIE}="))
                    .map(str::to_string)
            })
            .collect::<Vec<_>>();
        let sessions = self.sessions.lock().unwrap();
        cookies.iter().any(|c| sessions.contains(c))
    }
}

fn random_hex() -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(Clone)]
struct AppState {
    paths: crate::Paths,
    ui: Arc<WebUi>,
}

/// Bind 127.0.0.1:`port` (0 = any free port) and serve until stopped or
/// the daemon shuts down.
pub async fn start(daemon: Arc<Daemon>, port: u16) -> Result<Arc<WebUi>> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("bind 127.0.0.1:{port}"))?;
    let port = listener.local_addr()?.port();
    let ui = Arc::new(WebUi {
        port,
        tokens: Mutex::new(HashSet::new()),
        sessions: Mutex::new(HashSet::new()),
        stop: daemon.shutdown.child_token(),
    });
    let state = AppState {
        paths: daemon.paths.clone(),
        ui: ui.clone(),
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route(
            "/app.js",
            get(|s, h| asset(s, h, "text/javascript", APP_JS)),
        )
        .route("/app.css", get(|s, h| asset(s, h, "text/css", APP_CSS)))
        .route(
            "/vendor/mermaid.min.js",
            get(|s, h| asset(s, h, "text/javascript", MERMAID)),
        )
        .route("/sw.js", get(|s, h| asset(s, h, "text/javascript", SW_JS)))
        .route(
            "/manifest.webmanifest",
            get(|s, h| asset(s, h, "application/manifest+json", MANIFEST)),
        )
        .route("/icon-192.png", get(|s, h| icon(s, h, ICON_192)))
        .route("/icon-512.png", get(|s, h| icon(s, h, ICON_512)))
        .route("/auth", post(auth))
        .route("/ws", get(ws))
        .with_state(state);
    let stop = ui.stop.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move { stop.cancelled().await })
            .await;
    });
    Ok(ui)
}

fn secured(status: StatusCode, content_type: &str, body: String) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CONTENT_SECURITY_POLICY, CSP.to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (header::REFERRER_POLICY, "no-referrer".to_string()),
            (header::CACHE_CONTROL, "no-cache".to_string()),
        ],
        body,
    )
        .into_response()
}

fn forbidden() -> Response {
    secured(StatusCode::FORBIDDEN, "text/plain", "forbidden".into())
}

async fn index(State(st): State<AppState>, headers: HeaderMap) -> Response {
    if !st.ui.host_ok(&headers) {
        return forbidden();
    }
    // Tells the app to talk to this daemon directly (no relay crypto).
    let page = INDEX.replacen(
        "<head>",
        "<head>\n<meta name=\"formless-local\" content=\"1\">",
        1,
    );
    secured(StatusCode::OK, "text/html; charset=utf-8", page)
}

async fn asset(
    State(st): State<AppState>,
    headers: HeaderMap,
    content_type: &'static str,
    body: &'static str,
) -> Response {
    if !st.ui.host_ok(&headers) {
        return forbidden();
    }
    secured(StatusCode::OK, content_type, body.to_string())
}

async fn icon(State(st): State<AppState>, headers: HeaderMap, body: &'static [u8]) -> Response {
    if !st.ui.host_ok(&headers) {
        return forbidden();
    }
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

/// One-time token (from the URL fragment) for a session cookie.
async fn auth(State(st): State<AppState>, headers: HeaderMap, body: String) -> Response {
    if !st.ui.host_ok(&headers) || !st.ui.origin_ok(&headers) {
        return forbidden();
    }
    if !st.ui.tokens.lock().unwrap().remove(body.trim()) {
        return secured(
            StatusCode::UNAUTHORIZED,
            "text/plain",
            "this link was already used or is invalid; run formless web again".into(),
        );
    }
    let session = random_hex();
    st.ui.sessions.lock().unwrap().insert(session.clone());
    let mut res = secured(StatusCode::NO_CONTENT, "text/plain", String::new());
    if let Ok(v) = format!("{COOKIE}={session}; HttpOnly; SameSite=Strict; Path=/").parse() {
        res.headers_mut().insert(header::SET_COOKIE, v);
    }
    res
}

async fn ws(State(st): State<AppState>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    if !st.ui.host_ok(&headers) || !st.ui.origin_ok(&headers) || !st.ui.session_ok(&headers) {
        return forbidden();
    }
    let paths = st.paths.clone();
    upgrade.on_upgrade(move |socket| bridge(paths, socket))
}

/// Pump a WebSocket into an ordinary local connection to the daemon (its
/// socket or pipe): one protocol line per text frame, both ways.
async fn bridge(paths: crate::Paths, socket: WebSocket) {
    #[cfg(unix)]
    let Ok(conn) = tokio::net::UnixStream::connect(paths.socket_path()).await
    else {
        return;
    };
    #[cfg(windows)]
    let Ok(conn) = tokio::net::windows::named_pipe::ClientOptions::new().open(paths.pipe_name()) else {
        return;
    };
    let (read, mut write) = tokio::io::split(conn);
    let (mut tx, mut rx) = socket.split();
    let to_browser = async {
        let mut lines = BufReader::new(read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(Message::Text(line.into())).await.is_err() {
                break;
            }
        }
    };
    let from_browser = async {
        while let Some(Ok(msg)) = rx.next().await {
            match msg {
                Message::Text(t) => {
                    if write.write_all(t.as_bytes()).await.is_err()
                        || write.write_all(b"\n").await.is_err()
                    {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    };
    tokio::select! {
        _ = to_browser => {}
        _ = from_browser => {}
    }
}
