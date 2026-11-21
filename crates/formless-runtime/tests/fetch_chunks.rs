use base64::Engine;
use formless_runtime::{execute, Capabilities, ExecRequest, HostMode, HostOps};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{atomic::AtomicBool, mpsc, Arc};
use std::time::Duration;

struct FirstCallback(mpsc::Sender<()>);

impl HostOps for FirstCallback {
    fn progress(&self, _fraction: Option<f64>, _label: &str) {
        let _ = self.0.send(());
    }
}

fn fetch_from_waiting_peer(
    first: &'static [u8],
    rest: &'static [u8],
    declared_bytes: usize,
    encoding: &str,
) -> Value {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (callback, delivered) = mpsc::channel();
    let peer = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut socket = BufReader::new(socket);
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(socket.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
        }
        let socket = socket.get_mut();
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {declared_bytes}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        socket.write_all(first).unwrap();
        delivered
            .recv_timeout(Duration::from_secs(5))
            .expect("the first callback should arrive before the peer sends more data");
        socket.write_all(rest).unwrap();
    });
    let request = ExecRequest {
        code: r#"function run(i) {
            const chunks = [];
            let error = null;
            try {
                fetchChunks(i.url, {encoding: i.encoding}, 65536, (chunk) => {
                    chunks.push(chunk);
                    if (chunks.length === 1) progress(0.5, "first");
                });
            } catch (failure) {
                error = failure.message;
            }
            return {chunks, terminal: error};
        }"#
        .into(),
        input: json!({"url": format!("http://{address}/"), "encoding": encoding}),
        caps: Capabilities::parse(&["net:127.0.0.1".into()]).unwrap(),
        memory_mb: 64,
        mode: HostMode::Live,
        cancel: Arc::new(AtomicBool::new(false)),
        overlay: None,
        activity: None,
        modules: Arc::new(Vec::new()),
        output_schema: None,
    };
    let result = execute(&request, Arc::new(FirstCallback(callback)));
    peer.join().unwrap();
    result.unwrap().output
}

#[test]
fn short_reads_deliver_immediately_and_keep_split_utf8_intact() {
    let first = b"first:\xf0\x9f";
    let second = b"\xa7\xaa.";
    let output = fetch_from_waiting_peer(first, second, first.len() + second.len(), "text");
    let chunks: Vec<String> = serde_json::from_value(output["chunks"].clone()).unwrap();
    assert_eq!(chunks.concat(), "first:🧪.");
    assert_eq!(output["terminal"], Value::Null);
}

#[test]
fn incomplete_text_is_lossy_both_at_eof_and_on_a_terminal_error() {
    let complete = fetch_from_waiting_peer(b"a\xc3", b"", 2, "text");
    assert_eq!(complete, json!({"chunks": ["a", "�"], "terminal": null}));
    let broken = fetch_from_waiting_peer(b"a\xc3", b"", 10, "text");
    assert_eq!(broken["chunks"], json!(["a", "�"]));
    assert!(broken["terminal"]
        .as_str()
        .unwrap()
        .contains("fetch failed"));
}

#[test]
fn base64_chunks_keep_the_received_bytes_before_reporting_a_terminal_error() {
    let raw = b"a\xc3\xff";
    let broken = fetch_from_waiting_peer(raw, b"", 10, "base64");
    let mut bytes = Vec::new();
    for chunk in broken["chunks"].as_array().unwrap() {
        bytes.extend(
            base64::engine::general_purpose::STANDARD
                .decode(chunk.as_str().unwrap())
                .unwrap(),
        );
    }
    assert_eq!(bytes, raw);
    assert!(broken["terminal"]
        .as_str()
        .unwrap()
        .contains("fetch failed"));
}
