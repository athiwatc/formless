//! Optional setup discovery. Read-only HTTP on literal loopback addresses;
//! callers run this bounded blocking work outside the UI thread.
use std::{
    io::Read,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

const MAX_BODY: u64 = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_millis(350);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalServer {
    pub preset_id: &'static str,
    pub base_url: String,
    pub models: usize,
}

impl LocalServer {
    pub fn offer(&self) -> String {
        if self.models == 0 {
            if self.preset_id == "ollama" {
                "Ollama's model API is available but has no models. Run `ollama pull MODEL`, then retry /setup, or choose another provider.".into()
            } else {
                format!("Local model API at {} has no models. Load a model in the local server, then retry /setup, or choose another provider.",self.base_url)
            }
        } else {
            format!("Local model API at {} offers {} model(s). Press Enter to select one, or choose another provider.",self.base_url,self.models)
        }
    }
}

fn client() -> reqwest::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_millis(100))
        .timeout(TIMEOUT)
        .build()
}

fn model_count(id: &str, body: &[u8]) -> Option<usize> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let (items, field) = match id {
        "ollama" => (value.get("models")?.as_array()?, "name"),
        "lmstudio" if value["object"] == "list" => (value.get("data")?.as_array()?, "id"),
        _ => return None,
    };
    items
        .iter()
        .all(|item| {
            item.get(field)
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.trim().is_empty())
        })
        .then_some(items.len())
}

fn probe(
    client: &reqwest::blocking::Client,
    id: &'static str,
    addresses: &[SocketAddr],
) -> Option<LocalServer> {
    let mut empty = None;
    let path = if id == "ollama" {
        "/api/tags"
    } else {
        "/v1/models"
    };
    for addr in addresses {
        let origin = format!("http://{addr}");
        let Ok(response) = client.get(format!("{origin}{path}")).send() else {
            continue;
        };
        if !response.status().is_success()
            || response.content_length().is_some_and(|n| n > MAX_BODY)
        {
            continue;
        }
        let mut body = Vec::new();
        if response.take(MAX_BODY + 1).read_to_end(&mut body).is_err()
            || body.len() as u64 > MAX_BODY
        {
            continue;
        }
        if let Some(models) = model_count(id, &body) {
            let server = LocalServer {
                preset_id: id,
                base_url: format!("{origin}/v1"),
                models,
            };
            if models > 0 {
                return Some(server);
            }
            empty = Some(server);
        }
    }
    empty
}

/// No credentials, proxy, redirects, model loads or downloads. At most four
/// bounded GETs; prefer a usable server to a server with an empty model list.
pub fn discover() -> Vec<LocalServer> {
    let Ok(client) = client() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (id, port) in [("ollama", 11434), ("lmstudio", 1234)] {
        let addresses = [
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
        ];
        if let Some(server) = probe(&client, id, &addresses) {
            found.push(server);
        }
    }
    found.sort_by_key(|s| s.models == 0);
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        net::TcpListener,
        sync::{Arc, Mutex},
    };
    fn serve(
        listener: TcpListener,
        body: String,
        status: &'static str,
    ) -> (std::thread::JoinHandle<()>, Arc<Mutex<String>>) {
        let seen = Arc::new(Mutex::new(String::new()));
        let captured = seen.clone();
        let thread = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0; 1];
            while !request.ends_with(b"\r\n\r\n") && request.len() < 16384 {
                if socket.read(&mut byte).unwrap_or(0) == 0 {
                    break;
                }
                request.push(byte[0]);
            }
            *captured.lock().unwrap() = String::from_utf8_lossy(&request).into();
            let _=write!(socket,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
        });
        (thread, seen)
    }
    #[test]
    fn http_shapes_identify_model_apis_and_empty_ollama_has_pull_guidance() {
        for (id, body, path, count) in [
            (
                "ollama",
                r#"{"models":[{"name":"private-model"}]}"#,
                "/api/tags",
                1,
            ),
            ("ollama", r#"{"models":[]}"#, "/api/tags", 0),
            (
                "lmstudio",
                r#"{"object":"list","data":[{"id":"private-model"}]}"#,
                "/v1/models",
                1,
            ),
            (
                "lmstudio",
                r#"{"object":"list","data":[]}"#,
                "/v1/models",
                0,
            ),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let (thread, seen) = serve(listener, body.into(), "200 OK");
            let server = probe(&client().unwrap(), id, &[addr]).unwrap();
            thread.join().unwrap();
            assert_eq!(server.models, count);
            let request = seen.lock().unwrap();
            assert!(request.starts_with(&format!("GET {path} HTTP/1.1")));
            assert!(!request.to_lowercase().contains("authorization:"));
            if count == 0 {
                assert!(server.offer().contains(if id == "ollama" {
                    "ollama pull MODEL"
                } else {
                    "Load a model"
                }));
                assert!(!server.offer().contains("Press Enter"));
            }
        }
    }
    #[test]
    fn unrelated_http_and_bare_tcp_listeners_produce_no_offer() {
        for body in [r#"{"ok":true}"#, r#"{"models":[{}]}"#, "not json"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let (thread, _) = serve(listener, body.into(), "200 OK");
            assert!(probe(&client().unwrap(), "ollama", &[addr]).is_none());
            thread.join().unwrap();
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let start = std::time::Instant::now();
        assert!(probe(
            &client().unwrap(),
            "ollama",
            &[listener.local_addr().unwrap()]
        )
        .is_none());
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn ipv6_only_model_server_is_found_after_ipv4_does_not_answer_http() {
        let listener = TcpListener::bind("[::1]:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let v4 = TcpListener::bind((Ipv4Addr::LOCALHOST, addr.port())).unwrap();
        let (thread, _) = serve(
            listener,
            r#"{"models":[{"name":"private-v6"}]}"#.into(),
            "200 OK",
        );
        let server = probe(
            &client().unwrap(),
            "ollama",
            &[v4.local_addr().unwrap(), addr],
        )
        .unwrap();
        thread.join().unwrap();
        assert!(server.base_url.starts_with("http://[::1]:"));
        assert_eq!(server.models, 1);
    }
    #[test]
    fn failures_and_oversized_bodies_are_not_product_claims() {
        for (body, status) in [
            (r#"{"models":[]}"#.into(), "401 Unauthorized"),
            ("x".repeat(MAX_BODY as usize + 1), "200 OK"),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let (thread, _) = serve(listener, body, status);
            assert!(probe(&client().unwrap(), "ollama", &[addr]).is_none());
            thread.join().unwrap();
        }
    }
}
