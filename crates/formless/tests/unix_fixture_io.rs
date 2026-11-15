#![cfg(unix)]
#[path = "support/unix_fixture_io.rs"]
mod support;
use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use support::FixtureStream;
fn wrap(stream: UnixStream, stop: Arc<AtomicBool>, deadline: Instant) -> FixtureStream {
    FixtureStream::new(stream, stop, deadline, Duration::from_secs(1)).unwrap()
}
#[test]
fn closed_reachability_probe_is_clean_eof() {
    use std::os::unix::fs::DirBuilderExt;
    struct OwnedDirectory(std::path::PathBuf);
    impl Drop for OwnedDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = (0..100)
        .find_map(|attempt| {
            // Keep sockaddr_un below Darwin's limit, even when TMPDIR is long.
            let path = std::path::PathBuf::from(format!(
                "/tmp/fl-io-{}-{nonce:x}-{attempt}",
                std::process::id()
            ));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => Some(OwnedDirectory(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(e) => panic!("private fixture directory: {e}"),
            }
        })
        .expect("unique private fixture directory");
    let path = dir.0.join("s");
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    drop(UnixStream::connect(&path).unwrap());
    let (peer, _) = listener.accept().unwrap();
    let mut stream = wrap(
        peer,
        Arc::new(AtomicBool::new(false)),
        Instant::now() + Duration::from_secs(1),
    );
    assert_eq!(stream.read(&mut [0; 8]).unwrap(), 0);
    drop(listener);
    drop(stream);
}
#[test]
fn fragmented_and_coalesced_bytes_are_preserved() {
    let (server, mut client) = UnixStream::pair().unwrap();
    let mut stream = wrap(
        server,
        Arc::new(AtomicBool::new(false)),
        Instant::now() + Duration::from_secs(2),
    );
    let message = "one\nไทย🙂\ntwo\n".as_bytes().to_vec();
    let expected = message.clone();
    let writer = std::thread::spawn(move || {
        for bytes in message.chunks(2) {
            client.write_all(bytes).unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
    });
    let mut actual = Vec::new();
    stream.read_to_end(&mut actual).unwrap();
    writer.join().unwrap();
    assert_eq!(actual, expected);
}
#[test]
fn original_deadline_is_not_extended_by_repeated_reads() {
    let (server, mut client) = UnixStream::pair().unwrap();
    let start = Instant::now();
    let mut stream = wrap(
        server,
        Arc::new(AtomicBool::new(false)),
        start + Duration::from_millis(200),
    );
    client.write_all(b"a").unwrap();
    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 1);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        stream.read(&mut [0; 1]).unwrap_err().kind(),
        std::io::ErrorKind::TimedOut
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(
        stream.read(&mut [0; 1]).unwrap_err().kind(),
        std::io::ErrorKind::TimedOut
    );
}
#[test]
fn stop_ends_reads_and_write_all_without_retry_loop() {
    let (server, _client) = UnixStream::pair().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let mut stream = wrap(
        server,
        stop.clone(),
        Instant::now() + Duration::from_secs(2),
    );
    stop.store(true, Ordering::SeqCst);
    assert_eq!(stream.write(&[]).unwrap(), 0);
    assert_eq!(stream.read(&mut []).unwrap(), 0);
    assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
    assert_eq!(
        stream.write_all(b"no").unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
}
#[test]
fn partial_writes_preserve_full_frame_under_backpressure() {
    let (server, mut client) = UnixStream::pair().unwrap();
    let mut stream = wrap(
        server,
        Arc::new(AtomicBool::new(false)),
        Instant::now() + Duration::from_secs(5),
    );
    let expected: Vec<u8> = (0..2_000_000).map(|x| (x % 251) as u8).collect();
    let reader = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(40));
        let mut bytes = Vec::new();
        client.read_to_end(&mut bytes).unwrap();
        bytes
    });
    stream.write_all(&expected).unwrap();
    drop(stream);
    assert_eq!(reader.join().unwrap(), expected);
}

#[test]
fn closed_peer_write_error_is_not_suppressed() {
    let (server, client) = UnixStream::pair().unwrap();
    let mut stream = wrap(
        server,
        Arc::new(AtomicBool::new(false)),
        Instant::now() + Duration::from_secs(2),
    );
    drop(client);
    assert_eq!(
        stream.write_all(b"required response").unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
}
#[test]
fn cloned_handle_keeps_original_deadline() {
    let (server, _client) = UnixStream::pair().unwrap();
    let stream = wrap(
        server,
        Arc::new(AtomicBool::new(false)),
        Instant::now() - Duration::from_millis(1),
    );
    let mut clone = stream.try_clone().unwrap();
    assert_eq!(
        clone.read(&mut [0; 1]).unwrap_err().kind(),
        std::io::ErrorKind::TimedOut
    );
}
