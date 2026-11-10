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
    use std::collections::VecDeque;
    use std::io::{ErrorKind, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::time::Instant;

    const HEAD_LIMIT: usize = 8192;
    const READ_TIMEOUT: Duration = Duration::from_secs(1);
    const FIXTURE_DEADLINE: Duration = Duration::from_secs(3);
    const REQUEST: &[u8] = b"GET /releases HTTP/1.1\r\nhost: 127.0.0.1\r\n\r\n";

    #[derive(Debug)]
    enum HeadError {
        Eof { received: usize },
        Oversize,
        TimedOut { received: usize },
        Io(std::io::Error),
    }

    impl std::fmt::Display for HeadError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Eof { received } => write!(f, "EOF after {received} request-head bytes"),
                Self::Oversize => write!(f, "request head exceeds {HEAD_LIMIT} bytes"),
                Self::TimedOut { received } => {
                    write!(f, "timed out after {received} request-head bytes")
                }
                Self::Io(error) => write!(f, "request-head read failed: {error}"),
            }
        }
    }

    #[derive(Clone, Copy)]
    struct Timing {
        deadline: Duration,
        per_read: Duration,
    }

    const FIXTURE_TIMING: Timing = Timing {
        deadline: FIXTURE_DEADLINE,
        per_read: READ_TIMEOUT,
    };

    // Pure per-read timeout: None once the absolute deadline has passed.
    fn read_timeout_for(now: Instant, deadline: Instant, per_read: Duration) -> Option<Duration> {
        let remaining = deadline.saturating_duration_since(now);
        if remaining.is_zero() {
            None
        } else {
            Some(remaining.min(per_read))
        }
    }

    // Each read waits at most `per_read` and never past the absolute deadline.
    // `waiting` is a test hook: before each blocking read it reports how many
    // bytes the head reader has consumed and rejected as incomplete.
    struct DeadlineRead<'a> {
        socket: &'a TcpStream,
        deadline: Instant,
        per_read: Duration,
        waiting: Option<&'a mpsc::Sender<usize>>,
        consumed: usize,
    }

    impl Read for DeadlineRead<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let Some(timeout) = read_timeout_for(Instant::now(), self.deadline, self.per_read)
            else {
                return Err(ErrorKind::TimedOut.into());
            };
            self.socket.set_read_timeout(Some(timeout))?;
            if let Some(waiting) = self.waiting {
                // A gone observer cannot change the outcome; the socket read decides.
                let _ = waiting.send(self.consumed);
            }
            let mut socket = self.socket;
            let read = socket.read(buf)?;
            self.consumed += read;
            Ok(read)
        }
    }

    // Accumulates until CRLFCRLF without reading past HEAD_LIMIT bytes. Bytes
    // after the delimiter in the same read are discarded (bodyless fixture).
    fn read_request_head(reader: &mut impl Read) -> Result<Vec<u8>, HeadError> {
        let mut head = [0; HEAD_LIMIT];
        let mut len = 0;
        loop {
            if len == HEAD_LIMIT {
                return Err(HeadError::Oversize);
            }
            let read = match reader.read(&mut head[len..]) {
                Ok(0) => return Err(HeadError::Eof { received: len }),
                Ok(read) => read,
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Err(HeadError::TimedOut { received: len })
                }
                Err(e) => return Err(HeadError::Io(e)),
            };
            let scan = len.saturating_sub(3);
            len += read;
            if let Some(end) = head[scan..len].windows(4).position(|w| w == b"\r\n\r\n") {
                return Ok(head[..scan + end + 4].to_vec());
            }
        }
    }

    // Responds only after a complete request head; Ok(None) if nothing connected.
    fn serve(
        listener: TcpListener,
        response: &[u8],
        stopped: mpsc::Receiver<()>,
        timing: Timing,
        waiting: Option<mpsc::Sender<usize>>,
    ) -> Result<Option<Vec<u8>>, HeadError> {
        let deadline = Instant::now() + timing.deadline;
        let mut socket = loop {
            match listener.accept() {
                Ok((socket, _)) => break socket,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline || stopped.try_recv().is_ok() {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("fixture accept: {e}"),
            }
        };
        // accept inherits the listener's nonblocking flag on Darwin and
        // Windows; restore blocking so the bounded per-read timeout applies.
        socket.set_nonblocking(false).unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let head = read_request_head(&mut DeadlineRead {
            socket: &socket,
            deadline,
            per_read: timing.per_read,
            waiting: waiting.as_ref(),
            consumed: 0,
        })?;
        if !response.is_empty() {
            let _ = socket.write_all(response);
        }
        let _ = stopped.recv_timeout(Duration::from_secs(3));
        Ok(Some(head))
    }

    // The socket and thread have independent deadlines even if the client
    // fails before connecting. No global env changes or public endpoint.
    fn fixture(response: &'static [u8]) -> (Result<String>, Duration) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let (stop, stopped) = mpsc::channel();
        let server =
            std::thread::spawn(move || serve(listener, response, stopped, FIXTURE_TIMING, None));
        let client = builder(Duration::from_millis(150), Duration::from_millis(200))
            .no_proxy()
            .build()
            .unwrap();
        let started = Instant::now();
        let result = fetch(&client, &format!("http://{addr}/releases"));
        let elapsed = started.elapsed();
        let _ = stop.send(());
        if let Err(error) = server.join().unwrap() {
            panic!("fixture request head: {error}");
        }
        (result, elapsed)
    }

    // Deterministic reader: yields scripted chunks (split to fit), then EOF.
    struct Segments(VecDeque<std::io::Result<Vec<u8>>>);

    impl Segments {
        fn new(chunks: &[&[u8]]) -> Self {
            Self(chunks.iter().map(|chunk| Ok(chunk.to_vec())).collect())
        }

        fn unread(&self) -> usize {
            self.0.iter().flatten().map(Vec::len).sum()
        }
    }

    impl Read for Segments {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.pop_front() {
                None => Ok(0),
                Some(Err(error)) => Err(error),
                Some(Ok(mut chunk)) => {
                    let read = chunk.len().min(buf.len());
                    buf[..read].copy_from_slice(&chunk[..read]);
                    if read < chunk.len() {
                        self.0.push_front(Ok(chunk.split_off(read)));
                    }
                    Ok(read)
                }
            }
        }
    }

    fn has_delimiter(bytes: &[u8]) -> bool {
        bytes.windows(4).any(|w| w == b"\r\n\r\n")
    }

    #[test]
    fn request_head_reader_waits_for_split_delimiter() {
        let split = REQUEST.len() - 1;
        // Baseline gap: the old fixture's single read sees an incomplete head.
        let mut single = [0; 4096];
        let read = Segments::new(&[&REQUEST[..split], &REQUEST[split..]])
            .read(&mut single)
            .unwrap();
        assert!(!has_delimiter(&single[..read]));
        for split in 1..REQUEST.len() {
            let mut reader = Segments::new(&[&REQUEST[..split], &REQUEST[split..], b"next"]);
            assert_eq!(read_request_head(&mut reader).unwrap(), REQUEST, "{split}");
            // "next" is a separate chunk, so it is never requested.
            assert_eq!(reader.unread(), 4, "{split}");
        }
        let bytes: Vec<&[u8]> = REQUEST.chunks(1).collect();
        assert_eq!(
            read_request_head(&mut Segments::new(&bytes)).unwrap(),
            REQUEST
        );
    }

    #[test]
    fn request_head_reader_accepts_exact_bound_and_rejects_oversize() {
        let head = |len: usize| {
            let mut head = b"GET /releases HTTP/1.1\r\nx: ".to_vec();
            head.resize(len - 4, b'a');
            head.extend_from_slice(b"\r\n\r\n");
            head
        };
        let exact = head(HEAD_LIMIT);
        assert_eq!(
            read_request_head(&mut Segments::new(&[&exact])).unwrap(),
            exact
        );
        let bytes: Vec<&[u8]> = exact.chunks(7).collect();
        assert_eq!(
            read_request_head(&mut Segments::new(&bytes)).unwrap(),
            exact
        );
        let mut reader = Segments::new(&[&head(HEAD_LIMIT + 1)]);
        assert!(matches!(
            read_request_head(&mut reader),
            Err(HeadError::Oversize)
        ));
        assert_eq!(reader.unread(), 1, "reader must not request past the bound");
    }

    #[test]
    fn request_head_reader_fails_closed_on_eof_timeout_and_error() {
        let partial = &REQUEST[..REQUEST.len() - 2];
        assert!(matches!(
            read_request_head(&mut Segments::new(&[])),
            Err(HeadError::Eof { received: 0 })
        ));
        let received = partial.len();
        assert!(matches!(
            read_request_head(&mut Segments::new(&[partial])),
            Err(HeadError::Eof { received: r }) if r == received
        ));
        for kind in [ErrorKind::WouldBlock, ErrorKind::TimedOut] {
            let mut reader = Segments::new(&[partial]);
            reader.0.push_back(Err(kind.into()));
            reader.0.push_back(Ok(b"\r\n".to_vec()));
            assert!(matches!(
                read_request_head(&mut reader),
                Err(HeadError::TimedOut { received: r }) if r == received
            ));
        }
        let mut reader = Segments::new(&[partial]);
        reader.0.push_back(Err(ErrorKind::ConnectionReset.into()));
        let error = read_request_head(&mut reader).unwrap_err();
        assert!(matches!(&error, HeadError::Io(e) if e.kind() == ErrorKind::ConnectionReset));
    }

    #[test]
    fn read_timeout_math_caps_each_read_at_absolute_deadline() {
        let now = Instant::now();
        let ms = Duration::from_millis;
        assert_eq!(read_timeout_for(now, now, READ_TIMEOUT), None);
        assert_eq!(read_timeout_for(now + ms(1), now, READ_TIMEOUT), None);
        let deadline = now + FIXTURE_DEADLINE;
        assert_eq!(
            read_timeout_for(now, deadline, READ_TIMEOUT),
            Some(READ_TIMEOUT)
        );
        let late = now + ms(2500);
        assert_eq!(
            read_timeout_for(late, deadline, READ_TIMEOUT),
            Some(ms(500))
        );
        assert_eq!(read_timeout_for(deadline, deadline, READ_TIMEOUT), None);
        assert_eq!(
            read_timeout_for(now, now + ms(250), READ_TIMEOUT),
            Some(ms(250))
        );
    }

    #[test]
    fn deadline_read_applies_timeout_math_to_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (socket, _) = listener.accept().unwrap();
        client.write_all(b"a").unwrap();
        let reader = |deadline| DeadlineRead {
            socket: &socket,
            deadline,
            per_read: READ_TIMEOUT,
            waiting: None,
            consumed: 0,
        };
        let mut byte = [0; 1];
        // Captured before read() samples the clock, so it is always expired.
        let expired = reader(Instant::now()).read(&mut byte).unwrap_err();
        assert_eq!(expired.kind(), ErrorKind::TimedOut);
        // An hour of slack: only the per-read cap can bind here.
        let far = Instant::now() + Duration::from_secs(3600);
        assert_eq!(reader(far).read(&mut byte).unwrap(), 1);
        assert_eq!(byte, *b"a", "expired read must not consume data");
        assert_eq!(socket.read_timeout().unwrap(), Some(READ_TIMEOUT));
    }

    #[test]
    fn fixture_server_withholds_response_until_request_head_completes() {
        const RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n[]";
        // Every client wait is bounded by WATCHDOG; the server's deadline and
        // per-read cap are longer, so server inactivity never races a step.
        const WATCHDOG: Duration = Duration::from_secs(10);
        let timing = Timing {
            deadline: WATCHDOG * 3,
            per_read: WATCHDOG * 3,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        // Scoped server: on any panic below, `client`, `stop` and `waiting`
        // drop during unwind (EOF / disconnect wake the server) before the
        // scope joins it.
        let head = std::thread::scope(|scope| {
            // Connected before the spawn, so the server's accept never waits.
            let mut client = TcpStream::connect(addr).unwrap();
            client.set_read_timeout(Some(WATCHDOG)).unwrap();
            let (stop, stopped) = mpsc::channel();
            let (waiting_tx, waiting) = mpsc::channel();
            let server =
                scope.spawn(move || serve(listener, RESPONSE, stopped, timing, Some(waiting_tx)));
            let split = REQUEST.len() - 1;
            let mut sent = 0;
            for fragment in [&REQUEST[..10], &REQUEST[10..split]] {
                client.write_all(fragment).unwrap();
                sent += fragment.len();
                // Wait until the server has consumed every sent byte, rejected
                // the head as incomplete, and entered its next blocking read.
                let mut consumed = 0;
                while consumed < sent {
                    consumed = waiting.recv_timeout(WATCHDOG).expect("server read wait");
                }
                assert_eq!(consumed, sent);
                let mut byte = [0; 1];
                client.set_nonblocking(true).unwrap();
                let peeked = client.peek(&mut byte);
                client.set_nonblocking(false).unwrap();
                match peeked {
                    Ok(read) => panic!("{read} response bytes before request delimiter"),
                    Err(e) => assert_eq!(e.kind(), ErrorKind::WouldBlock, "{e}"),
                }
            }
            client.write_all(&REQUEST[split..]).unwrap();
            let mut response = vec![0; RESPONSE.len()];
            client.read_exact(&mut response).unwrap();
            assert_eq!(response, RESPONSE);
            stop.send(()).unwrap();
            server.join().unwrap().unwrap()
        });
        assert_eq!(head.as_deref(), Some(REQUEST));
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
