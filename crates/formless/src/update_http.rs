//! Bounded release metadata lookup; never downloads or installs an executable.
use anyhow::{anyhow, Result};
use reqwest::blocking::{Client, ClientBuilder};
use std::time::Duration;

const RELEASES_PAGE: &str = "https://github.com/athiwatc/formless/releases";

fn builder(connect: Duration, total: Duration) -> ClientBuilder {
    Client::builder()
        .user_agent(concat!("formless/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(connect)
        .timeout(total)
}

pub(super) fn get(url: &str) -> Result<String> {
    let client = builder(Duration::from_secs(5), Duration::from_secs(15)).build()?;
    fetch(&client, url)
}

fn fetch(client: &Client, url: &str) -> Result<String> {
    let response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(lookup_error)?;
    response.text().map_err(lookup_error)
}

fn lookup_error(error: reqwest::Error) -> anyhow::Error {
    if error.is_timeout() {
        anyhow!("GitHub release lookup timed out; try again or open {RELEASES_PAGE}")
    } else {
        error.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Instant;

    // The socket and thread have independent deadlines even if the client
    // fails before connecting. No global env changes or public endpoint.
    fn fixture(response: &'static [u8]) -> (Result<String>, Duration) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let (stop, stopped) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline || stopped.try_recv().is_ok() {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("fixture accept: {e}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            socket
                .set_write_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut request = [0; 4096];
            let _ = socket.read(&mut request);
            if !response.is_empty() {
                let _ = socket.write_all(response);
            }
            let _ = stopped.recv_timeout(Duration::from_secs(3));
        });
        let client = builder(Duration::from_millis(150), Duration::from_millis(200))
            .no_proxy()
            .build()
            .unwrap();
        let started = Instant::now();
        let result = fetch(&client, &format!("http://{addr}/releases"));
        let elapsed = started.elapsed();
        let _ = stop.send(());
        server.join().unwrap();
        (result, elapsed)
    }

    #[test]
    fn update_lookup_bounds_stalled_headers_and_body() {
        for response in [
            &b""[..],
            &b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n["[..],
        ] {
            let (result, elapsed) = fixture(response);
            let error = result.unwrap_err().to_string();
            assert!(error.contains("timed out"), "{error}");
            assert!(error.contains(RELEASES_PAGE) && error.contains("try again"));
            assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
        }
    }

    #[test]
    fn update_lookup_preserves_success_and_http_status() {
        let (result, _) = fixture(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n[]");
        assert_eq!(result.unwrap(), "[]");
        for (response, status) in [
            (
                &b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"[..],
                "404",
            ),
            (
                &b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\n\r\n"[..],
                "429",
            ),
        ] {
            let (result, _) = fixture(response);
            let error = result.unwrap_err().to_string();
            assert!(error.contains(status), "{error}");
            assert!(!error.contains("timed out"), "{error}");
        }
    }
}
