//! Bounded release metadata lookup; never downloads or installs an executable.
use anyhow::{anyhow, Context, Result};
use reqwest::blocking::{ClientBuilder, RequestBuilder};
use serde_json::Value;
use std::time::{Duration, Instant};

const RELEASES_PAGE: &str = "https://github.com/athiwatc/formless/releases";
const RELEASES_API: &str = "https://api.github.com/repos/athiwatc/formless/releases";
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(15);
const PAGE_SIZE: usize = 100;

fn builder(connect: Duration, total: Duration) -> ClientBuilder {
    ClientBuilder::new()
        .user_agent(concat!("formless/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(connect)
        .timeout(total)
}

pub(super) fn get_releases() -> Result<String> {
    let client = builder(Duration::from_secs(5), LOOKUP_TIMEOUT).build()?;
    collect_pages(|page, remaining| {
        fetch(
            client
                .get(RELEASES_API)
                .query(&[("per_page", PAGE_SIZE), ("page", page)])
                .timeout(remaining),
        )
    })
}

fn collect_pages(mut get_page: impl FnMut(usize, Duration) -> Result<String>) -> Result<String> {
    let started = Instant::now();
    let mut releases = Vec::new();
    for page in 1.. {
        let remaining = LOOKUP_TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(timeout_error());
        }
        let batch: Vec<Value> =
            serde_json::from_str(&get_page(page, remaining)?).context("parse releases")?;
        let last = batch.len() < PAGE_SIZE;
        releases.extend(batch);
        if last {
            break;
        }
    }
    Ok(serde_json::to_string(&releases)?)
}

fn fetch(request: RequestBuilder) -> Result<String> {
    let response = request.send().map_err(lookup_error)?;
    let headers = response.headers();
    if matches!(response.status().as_u16(), 403 | 429)
        && (headers
            .get("x-ratelimit-remaining")
            .is_some_and(|v| v == "0")
            || headers.contains_key(reqwest::header::RETRY_AFTER))
    {
        let when = retry_time(headers, chrono::Utc::now())
            .map(|time| {
                format!(
                    "try again after {} (local time)",
                    time.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M:%S")
                )
            })
            .unwrap_or_else(|| "try again later".into());
        return Err(anyhow!(
            "GitHub API rate limit reached; {when} or open {RELEASES_PAGE}"
        ));
    }
    response
        .error_for_status()
        .map_err(lookup_error)?
        .text()
        .map_err(lookup_error)
}

pub(super) fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<reqwest::Error>()
        .is_some_and(|e| e.status() == Some(reqwest::StatusCode::NOT_FOUND))
}

#[cfg(test)]
pub(super) fn fixture_not_found_error() -> anyhow::Error {
    tests::fixture(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
        .0
        .unwrap_err()
}

fn retry_time(
    headers: &reqwest::header::HeaderMap,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let retry = headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok());
    retry
        .and_then(|value| {
            value
                .parse::<i64>()
                .ok()
                .filter(|seconds| *seconds >= 0)
                .and_then(chrono::TimeDelta::try_seconds)
                .and_then(|delay| now.checked_add_signed(delay))
                .or_else(|| {
                    chrono::DateTime::parse_from_rfc2822(value)
                        .ok()
                        .map(|time| time.with_timezone(&chrono::Utc))
                })
        })
        .or_else(|| {
            headers
                .get("x-ratelimit-reset")?
                .to_str()
                .ok()?
                .parse::<i64>()
                .ok()
                .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
        })
}

fn lookup_error(error: reqwest::Error) -> anyhow::Error {
    if error.is_timeout() {
        timeout_error()
    } else {
        error.into()
    }
}

fn timeout_error() -> anyhow::Error {
    anyhow!("GitHub release lookup timed out; try again or open {RELEASES_PAGE}")
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

    #[test]
    fn stable_release_is_reported_after_a_page_of_previews() {
        let previews =
            vec![serde_json::json!({"tag_name": "main-fixture", "prerelease": true}); PAGE_SIZE];
        let stable =
            serde_json::json!([{"tag_name": "v1.2.0", "body": "notes", "html_url": "url"}]);
        let result = collect_pages(|page, remaining| {
            assert!(remaining <= LOOKUP_TIMEOUT);
            Ok(match page {
                1 => serde_json::to_string(&previews)?,
                2 => stable.to_string(),
                _ => panic!("unexpected page {page}"),
            })
        })
        .unwrap();
        let releases = serde_json::from_str(&result).unwrap();
        assert_eq!(
            super::super::newer_releases(&releases, "1.0.0"),
            vec![("v1.2.0".into(), "notes".into(), "url".into())]
        );
    }

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
    pub(super) fn fixture(response: &'static [u8]) -> (Result<String>, Duration) {
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
        let result = fetch(client.get(format!("http://{addr}/releases")));
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
    fn release_rate_limits_explain_retry_and_preserve_other_statuses() {
        for response in [
            &b"HTTP/1.1 403 Forbidden\r\nx-ratelimit-remaining: 0\r\nx-ratelimit-reset: 4102444800\r\nContent-Length: 0\r\n\r\n"[..],
            &b"HTTP/1.1 429 Too Many Requests\r\nretry-after: 60\r\nContent-Length: 0\r\n\r\n"[..],
        ] {
            let (result, _) = fixture(response);
            let error = result.unwrap_err().to_string();
            assert!(error.contains("GitHub API rate limit reached"), "{error}");
            assert!(error.contains("try again after") && error.contains("local time"), "{error}");
            assert!(error.contains(RELEASES_PAGE), "{error}");
        }
        let (result, _) = fixture(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
        let error = result.unwrap_err();
        assert!(error.to_string().contains("403"));
        assert!(!error.to_string().contains("rate limit reached"));
        let (result, _) = fixture(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        assert!(is_not_found(&result.unwrap_err()));
        assert!(!is_not_found(&anyhow!("unrelated text containing 404")));
    }

    #[test]
    fn retry_headers_accept_seconds_dates_and_reset_fallback_without_overflow() {
        let now = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-ratelimit-reset", "1700000120".parse().unwrap());
        assert_eq!(
            retry_time(&headers, now).unwrap().timestamp(),
            1_700_000_120
        );
        headers.insert("retry-after", "60".parse().unwrap());
        assert_eq!(
            retry_time(&headers, now).unwrap().timestamp(),
            1_700_000_060
        );
        headers.insert(
            "retry-after",
            "Tue, 14 Nov 2023 22:15:00 GMT".parse().unwrap(),
        );
        assert_eq!(
            retry_time(&headers, now).unwrap().timestamp(),
            1_700_000_100
        );
        for invalid in ["-1", "9223372036854775807", "nonsense"] {
            headers.insert("retry-after", invalid.parse().unwrap());
            assert_eq!(
                retry_time(&headers, now).unwrap().timestamp(),
                1_700_000_120
            );
        }
        headers.remove("x-ratelimit-reset");
        assert!(retry_time(&headers, now).is_none());
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
