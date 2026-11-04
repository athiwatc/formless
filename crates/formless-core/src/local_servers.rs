//! Optional setup discovery. Read-only HTTP on literal loopback addresses;
//! callers run this bounded blocking work outside the UI thread.
use std::{
    io::Read,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

const MAX_BODY: u64 = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_millis(350);

#[derive(Default)]
pub struct Discovery {
    pub servers: Vec<LocalServer>,
    pub hint: Option<String>,
}

impl From<Vec<LocalServer>> for Discovery {
    fn from(servers: Vec<LocalServer>) -> Self {
        Self {
            servers,
            hint: None,
        }
    }
}

fn loopbacks(port: u16) -> Vec<SocketAddr> {
    vec![
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port),
    ]
}

// Supported Ollama Host forms, restricted to local addresses before any I/O.
// No hostname is resolved, including localhost and unspecified bind hosts.
fn ollama_origins(raw: Option<&str>) -> Result<Vec<String>, &'static str> {
    let raw = raw.unwrap_or("").trim().trim_matches(['\'', '"']);
    if raw.is_empty() {
        return Ok(loopbacks(11434)
            .into_iter()
            .map(|addr| format!("http://{addr}"))
            .collect());
    }
    let invalid = "OLLAMA_HOST is not a supported local address; add the provider manually.";
    if raw.len() > 2048 || raw.chars().any(char::is_control) || raw.contains(['@', '?', '#']) {
        return Err(invalid);
    }
    let (scheme, hostpath, default_port) = match raw.split_once("://") {
        Some(("http", rest)) => ("http", rest, 80),
        Some(("https", rest)) => ("https", rest, 443),
        Some(_) => return Err(invalid),
        None => ("http", raw, 11434),
    };
    let (authority, path) = hostpath
        .split_once('/')
        .map_or((hostpath, ""), |(host, path)| (host, path));
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, tail) = bracketed.split_once(']').ok_or(invalid)?;
        let port = if tail.is_empty() {
            default_port
        } else {
            tail.strip_prefix(':')
                .ok_or(invalid)?
                .parse::<u16>()
                .map_err(|_| invalid)?
        };
        (host, port)
    } else if authority.parse::<IpAddr>().is_ok() {
        (authority, default_port)
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        (host, port.parse::<u16>().map_err(|_| invalid)?)
    } else {
        (authority, default_port)
    };
    let addresses = if host.eq_ignore_ascii_case("localhost") {
        loopbacks(port)
    } else if host.is_empty() {
        vec![SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)]
    } else {
        let mut ip: IpAddr = host
            .parse()
            .map_err(|_| "OLLAMA_HOST points at a remote host; add it as a provider manually.")?;
        if let IpAddr::V6(v6) = ip {
            if let Some(v4) = v6.to_ipv4_mapped() {
                ip = IpAddr::V4(v4);
            }
        }
        if ip.is_unspecified() {
            ip = if ip.is_ipv4() {
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            } else {
                IpAddr::V6(Ipv6Addr::LOCALHOST)
            };
        }
        if !ip.is_loopback() {
            return Err("OLLAMA_HOST points at a remote host; add it as a provider manually.");
        }
        vec![SocketAddr::new(ip, port)]
    };
    addresses
        .into_iter()
        .map(|addr| {
            let value = format!("{scheme}://{addr}/{}", path.trim_end_matches('/'));
            reqwest::Url::parse(&value)
                .map(|url| url.as_str().trim_end_matches('/').to_string())
                .map_err(|_| invalid)
        })
        .collect()
}

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

#[cfg(test)]
fn probe(
    client: &reqwest::blocking::Client,
    id: &'static str,
    addresses: &[SocketAddr],
) -> Option<LocalServer> {
    let origins = addresses
        .iter()
        .map(|addr| format!("http://{addr}"))
        .collect::<Vec<_>>();
    probe_origins(client, id, &origins, &mut 4)
}

fn probe_origins(
    client: &reqwest::blocking::Client,
    id: &'static str,
    origins: &[String],
    budget: &mut usize,
) -> Option<LocalServer> {
    let mut empty = None;
    let path = if id == "ollama" {
        "/api/tags"
    } else {
        "/v1/models"
    };
    for origin in origins {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
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
    discover_with_guidance().servers
}

pub fn discover_with_guidance() -> Discovery {
    let value = std::env::var("OLLAMA_HOST");
    let raw = value.as_deref().ok();
    let configured = raw.is_some_and(|s| !s.trim().trim_matches(['\'', '"']).is_empty());
    let parsed = if matches!(&value, Err(std::env::VarError::NotUnicode(_))) {
        Err("OLLAMA_HOST is not a supported local address; add the provider manually.")
    } else {
        ollama_origins(raw)
    };
    let (ollama, mut hint) = match parsed {
        Ok(origins) => (origins, None),
        Err(hint) => (Vec::new(), Some(hint.to_string())),
    };
    let Ok(client) = client() else {
        return Discovery {
            servers: Vec::new(),
            hint,
        };
    };
    let mut found = Vec::new();
    let lmstudio = loopbacks(1234)
        .into_iter()
        .map(|addr| format!("http://{addr}"))
        .collect();
    let mut budget = 4;
    for (id, origins) in [("ollama", ollama), ("lmstudio", lmstudio)] {
        if let Some(server) = probe_origins(&client, id, &origins, &mut budget) {
            found.push(server);
        } else if id == "ollama" && configured && hint.is_none() {
            hint = Some("No model API answered at the local OLLAMA_HOST address. Check Ollama or add the provider manually.".into());
        }
    }
    found.sort_by_key(|s| s.models == 0);
    Discovery {
        servers: found,
        hint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ollama_host_environment_finds_a_random_local_port() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let env = crate::credential_env::EnvGuard::isolated();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        env.set("OLLAMA_HOST", address.to_string());
        for key in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            env.set(key, "http://127.0.0.1:9");
        }
        for key in ["NO_PROXY", "no_proxy"] {
            env.set(key, "");
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        socket
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut request = Vec::new();
                        let mut byte = [0; 1];
                        while !request.ends_with(b"\r\n\r\n") && request.len() < 16384 {
                            if socket.read(&mut byte).unwrap_or(0) == 0 {
                                break;
                            }
                            request.push(byte[0]);
                        }
                        let body = r#"{"models":[{"name":"fixture"}]}"#;
                        write!(socket,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
                        return String::from_utf8(request).unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("{error}"),
                }
            }
            String::new()
        });
        let found = discover();
        stop.store(true, Ordering::SeqCst);
        let request = thread.join().unwrap();
        let base = format!("http://{address}/v1");
        assert!(
            found
                .iter()
                .any(|server| server.base_url == base && server.models == 1),
            "configured random port ignored: {found:?}"
        );
        assert!(request.starts_with("GET /api/tags HTTP/1.1"), "{request}");
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
    }
    use std::{
        io::Write,
        net::TcpListener,
        sync::{Arc, Mutex},
    };
    fn serve(
        listener: TcpListener,
        body: String,
        status: &str,
    ) -> (std::thread::JoinHandle<()>, Arc<Mutex<String>>) {
        let status = status.to_owned();
        let seen = Arc::new(Mutex::new(String::new()));
        let captured = seen.clone();
        let thread = std::thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "owned HTTP fixture was never probed"
                        );
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
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
    fn ollama_host_local_forms_match_supported_ollama_defaults() {
        for (raw, expected) in [
            ("127.0.0.2:11500", vec!["http://127.0.0.2:11500"]),
            ("[::1]:11500", vec!["http://[::1]:11500"]),
            ("0.0.0.0:11500", vec!["http://127.0.0.1:11500"]),
            ("[::]:11500", vec!["http://[::1]:11500"]),
            ("::1", vec!["http://[::1]:11434"]),
            ("::ffff:127.0.0.1", vec!["http://127.0.0.1:11434"]),
            (
                "localhost",
                vec!["http://127.0.0.1:11434", "http://[::1]:11434"],
            ),
            ("http://localhost", vec!["http://127.0.0.1", "http://[::1]"]),
            ("https://127.0.0.1", vec!["https://127.0.0.1"]),
            (
                " 'localhost:11500/prefix/' ",
                vec!["http://127.0.0.1:11500/prefix", "http://[::1]:11500/prefix"],
            ),
        ] {
            assert_eq!(ollama_origins(Some(raw)).unwrap(), expected, "{raw}");
        }
        let defaults = vec!["http://127.0.0.1:11434", "http://[::1]:11434"];
        for raw in [None, Some(""), Some("  ")] {
            assert_eq!(ollama_origins(raw).unwrap(), defaults);
        }
    }

    #[test]
    fn ollama_host_remote_invalid_and_credential_forms_are_never_candidates() {
        for raw in [
            "example.com:11500",
            "192.168.1.2:11500",
            "[2001:db8::1]:11500",
            "http://user:secret@localhost:11500",
            "http://localhost:11500/?token=secret",
            "http://localhost:11500/#secret",
            "ftp://127.0.0.1",
            "localhost:65536",
            "[::1",
            "local\nhost",
        ] {
            assert!(ollama_origins(Some(raw)).is_err(), "{raw}");
        }
        let env = crate::credential_env::EnvGuard::isolated();
        env.set("OLLAMA_HOST", "http://user:secret@example.com:11500");
        let report = discover_with_guidance();
        assert!(report.servers.iter().all(|s| s.preset_id != "ollama"));
        assert!(report.hint.as_ref().unwrap().contains("manually"));
        assert!(!report.hint.unwrap().contains("secret"));
    }

    #[test]
    fn ollama_host_path_is_probed_and_retained_without_redirects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let origins = ollama_origins(Some(&format!("http://{addr}/prefix"))).unwrap();
        let (thread, seen) = serve(
            listener,
            r#"{"models":[{"name":"fixture"}]}"#.into(),
            "200 OK",
        );
        let server = probe_origins(&client().unwrap(), "ollama", &origins, &mut 4).unwrap();
        thread.join().unwrap();
        assert_eq!(server.base_url, format!("http://{addr}/prefix/v1"));
        assert!(seen
            .lock()
            .unwrap()
            .starts_with("GET /prefix/api/tags HTTP/1.1"));

        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (thread, _) = serve(
            listener,
            String::new(),
            &format!(
                "302 Found\r\nLocation: http://{}/api/tags",
                target.local_addr().unwrap()
            ),
        );
        assert!(probe(&client().unwrap(), "ollama", &[addr]).is_none());
        thread.join().unwrap();
        assert_eq!(
            target.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn ollama_host_probe_budget_stops_after_four_physical_gets() {
        let mut threads = Vec::new();
        let mut origins = Vec::new();
        for _ in 0..4 {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            origins.push(format!("http://{}", listener.local_addr().unwrap()));
            threads.push(serve(listener, String::new(), "503 Unavailable"));
        }
        let extra = TcpListener::bind("127.0.0.1:0").unwrap();
        extra.set_nonblocking(true).unwrap();
        origins.push(format!("http://{}", extra.local_addr().unwrap()));
        let mut budget = 4;
        assert!(probe_origins(&client().unwrap(), "ollama", &origins, &mut budget).is_none());
        assert_eq!(budget, 0);
        for (thread, seen) in threads {
            thread.join().unwrap();
            let request = seen.lock().unwrap();
            assert!(request.starts_with("GET /api/tags HTTP/1.1"));
            assert!(!request.to_ascii_lowercase().contains("authorization:"));
        }
        assert_eq!(
            extra.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
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
