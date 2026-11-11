//! Windows true-duplex daemon client (issue 1789).
//!
//! The old Windows client opened the pipe with a synchronous `CreateFile`
//! handle and duplicated it for a reader and a writer thread. A blocking
//! read on a synchronous handle serializes every later write on the same
//! file object, so a request could sit behind a read the daemon would only
//! answer after that request arrived.
//!
//! Here one worker thread owns a current-thread Tokio runtime with I/O and
//! time enabled, and owns the pipe for its whole life. The pipe is opened by
//! default `ClientOptions` (overlapped, read and write, and
//! `SECURITY_IDENTIFICATION | SECURITY_SQOS_PRESENT`) while that runtime is
//! entered, and it is never handed to the synchronous caller. The caller
//! only receives a handshake result on a std channel, so it never enters,
//! nests or blocks a runtime of its own.
//!
//! Inside the worker, one framed reader and one ordered writer run
//! concurrently. Commands come from an unbounded channel, like the inherited
//! std queue. A writer that ends (write error or closed queue) ends the
//! session at once, which drops the reader: a silent server cannot keep the
//! session alive after the write side failed. A reader that ends (EOF, read
//! error, oversized or non-UTF-8 frame, dropped event receiver) closes the
//! event channel and leaves the writer running until stop, a closed queue or
//! a write error, as the inherited writer thread did.
//!
//! Teardown: dropping the client sets an explicit stop that is checked
//! before the writer is polled again, so a queued backlog is not dequeued
//! after stop. The caller then waits at most [`WORKER_TEARDOWN_BOUND`] and
//! joins the worker only if it has already finished; otherwise the thread is
//! detached.
//!
//! Retained resources, stated plainly. Mio 1.2.4 cancels a pending read or
//! connect when the pipe is dropped, but not a pending write. A write that
//! the OS has not completed keeps its buffer and the pipe handle alive. If
//! the worker's runtime is dropped before that write completes, Mio cannot
//! reclaim them, so they can stay allocated and the handle open for the
//! rest of the process, and the server may not see end-of-file. A detached
//! worker additionally keeps its thread, runtime and unsent queue until it
//! finishes. Nothing here cancels an in-flight write, waits for write
//! completion, or bounds the total memory retained.

use formless_protocol::ServerMessage;
use std::io::{Error, ErrorKind};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc as queue, watch};

/// How long a dropped client waits for its worker before detaching it.
pub(super) const WORKER_TEARDOWN_BOUND: Duration = Duration::from_millis(200);

/// Read size per call, like the inherited `BufReader` default.
const READ_CHUNK: usize = 8 * 1024;

/// Why the framed reader stopped.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ReaderEnd {
    Eof,
    ReadFailed,
    Oversize,
    InvalidUtf8,
    ReceiverGone,
}

/// Why the ordered writer stopped.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum WriterEnd {
    Closed,
    Failed,
    Stopped,
}

/// Which side ended the session first.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SessionEnd {
    Stopped,
    WriterEnded,
    ReaderEnded,
}

/// Teardown result. `Detached` does not mean the write was canceled.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum WorkerFinish {
    Joined,
    Detached,
}

fn deadline_before_attempt() -> Error {
    Error::new(
        ErrorKind::TimedOut,
        "named pipe open deadline expired before an attempt",
    )
}

fn missed_handshake() -> Error {
    Error::new(
        ErrorKind::TimedOut,
        "named pipe handshake missed the construction deadline",
    )
}

/// Request capacity only within `limit`; allocators may round allocations up.
/// Doubling keeps long frames amortized. The logical buffered length stays
/// within `MAX_FRAME_BYTES + 1`, checked before extending the buffer.
fn reserve_within(buf: &mut Vec<u8>, extra: usize, limit: usize) {
    let need = buf.len().saturating_add(extra);
    if need > buf.capacity() {
        let target = buf.capacity().saturating_mul(2).min(limit).max(need);
        buf.reserve_exact(target - buf.len());
    }
}

/// One complete line, parsed like the inherited reader thread: invalid UTF-8
/// ends the connection, JSON that is not a `ServerMessage` is skipped.
fn deliver(line: &[u8], events: &mpsc::Sender<ServerMessage>) -> Result<(), ReaderEnd> {
    let text = std::str::from_utf8(line).map_err(|_| ReaderEnd::InvalidUtf8)?;
    if let Ok(message) = serde_json::from_str::<ServerMessage>(text) {
        events.send(message).map_err(|_| ReaderEnd::ReceiverGone)?;
    }
    Ok(())
}

/// Newline-framed reader with the `read_line_capped` limits: a line of at
/// most `max` bytes is accepted, a longer one ends the reader after at most
/// `max + 1` of its bytes are buffered, and a short unterminated tail at EOF
/// is still delivered.
pub(super) async fn framed_reader<R>(
    mut reader: R,
    events: mpsc::Sender<ServerMessage>,
    max: usize,
) -> ReaderEnd
where
    R: AsyncRead + Unpin,
{
    let limit = max.saturating_add(1);
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; READ_CHUNK];
    loop {
        // Invariant: `buf` holds no newline and at most `max` bytes.
        let take = (limit - buf.len()).min(READ_CHUNK);
        let n = match reader.read(&mut chunk[..take]).await {
            Ok(0) => {
                if !buf.is_empty() {
                    if let Err(end) = deliver(&buf, &events) {
                        return end;
                    }
                }
                return ReaderEnd::Eof;
            }
            Ok(n) => n,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return ReaderEnd::ReadFailed,
        };
        let scan_from = buf.len();
        reserve_within(&mut buf, n, limit);
        buf.extend_from_slice(&chunk[..n]);
        let mut start = 0;
        let mut at = scan_from;
        while let Some(offset) = buf[at..].iter().position(|byte| *byte == b'\n') {
            let end = at + offset;
            if let Err(stop) = deliver(&buf[start..end], &events) {
                return stop;
            }
            start = end + 1;
            at = start;
        }
        buf.drain(..start);
        if buf.len() > max {
            return ReaderEnd::Oversize;
        }
    }
}

/// The only writer. Lines go out in queue order, one frame per write call,
/// never replayed. A flush error is ignored, as in the inherited writer.
pub(super) async fn ordered_writer<W>(
    mut writer: W,
    mut commands: queue::UnboundedReceiver<String>,
    mut stop: watch::Receiver<bool>,
) -> WriterEnd
where
    W: AsyncWrite + Unpin,
{
    loop {
        if *stop.borrow() {
            return WriterEnd::Stopped;
        }
        let incoming = tokio::select! {
            biased;
            () = stopped(&mut stop) => return WriterEnd::Stopped,
            incoming = commands.recv() => incoming,
        };
        let Some(mut frame) = incoming else {
            return WriterEnd::Closed;
        };
        // Recheck even when recv completed immediately. One poll can handle
        // multiple ready frames without yielding to run_session's select.
        if *stop.borrow() {
            return WriterEnd::Stopped;
        }
        frame.push('\n');
        if writer.write_all(frame.as_bytes()).await.is_err() {
            return WriterEnd::Failed;
        }
        let _ = writer.flush().await;
    }
}

/// Resolves once stop is set, or its sender is gone.
async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|stop| *stop).await;
}

/// Reader and writer in one task. Stop is polled first every time, so a
/// ready backlog is never dequeued after stop. The writer ending drops the
/// reader at once (the earlier `join!` draft left a silent reader alive).
pub(super) async fn run_session<P>(
    pipe: P,
    commands: queue::UnboundedReceiver<String>,
    mut stop: watch::Receiver<bool>,
    events: mpsc::Sender<ServerMessage>,
    max_frame: usize,
) -> SessionEnd
where
    P: AsyncRead + AsyncWrite + Unpin,
{
    let (reader, writer) = tokio::io::split(pipe);
    let read = framed_reader(reader, events, max_frame);
    let write = ordered_writer(writer, commands, stop.clone());
    tokio::pin!(read);
    tokio::pin!(write);
    tokio::select! {
        biased;
        () = stopped(&mut stop) => return SessionEnd::Stopped,
        _ = &mut write => return SessionEnd::WriterEnded,
        _ = &mut read => {}
    }
    // The event channel is closed. Keep the writer, as the inherited
    // writer thread did, until stop, a closed queue or a write error.
    tokio::select! {
        biased;
        () = stopped(&mut stop) => {}
        _ = &mut write => {}
    }
    SessionEnd::ReaderEnded
}

/// The client side of a running worker.
pub(super) struct Duplex {
    commands: queue::UnboundedSender<String>,
    stop: watch::Sender<bool>,
    done: Mutex<mpsc::Receiver<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Duplex {
    /// Non-blocking, unbounded, like the inherited std queue. After the
    /// worker ends the line is handed back and dropped by the caller.
    pub(super) fn enqueue(&self, line: String) -> Result<(), String> {
        self.commands.send(line).map_err(|error| error.0)
    }

    fn finish(&mut self, bound: Duration) -> WorkerFinish {
        self.stop.send_replace(true);
        let Some(handle) = self.thread.take() else {
            return WorkerFinish::Joined;
        };
        let until = Instant::now() + bound;
        let done = self
            .done
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = done.recv_timeout(bound);
        while !handle.is_finished() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
        }
        if handle.is_finished() {
            let _ = handle.join();
            WorkerFinish::Joined
        } else {
            WorkerFinish::Detached
        }
    }

    /// Stop and report how teardown ended.
    #[cfg(test)]
    pub(super) fn close(mut self, bound: Duration) -> WorkerFinish {
        self.finish(bound)
    }
}

impl Drop for Duplex {
    fn drop(&mut self) {
        let _ = self.finish(WORKER_TEARDOWN_BOUND);
    }
}

struct NotifyDone(Option<mpsc::Sender<()>>);

impl Drop for NotifyDone {
    fn drop(&mut self) {
        if let Some(done) = self.0.take() {
            let _ = done.send(());
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_worker<P, F>(
    open: F,
    deadline: Instant,
    handshake: mpsc::Sender<std::io::Result<()>>,
    commands: queue::UnboundedReceiver<String>,
    stop: watch::Receiver<bool>,
    events: mpsc::Sender<ServerMessage>,
    done: mpsc::Sender<()>,
    max_frame: usize,
) where
    P: AsyncRead + AsyncWrite + Unpin,
    F: FnOnce() -> std::io::Result<P>,
{
    // Declared first, so it is signaled after the runtime is dropped.
    let _done = NotifyDone(Some(done));
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = handshake.send(Err(error));
            return;
        }
    };
    let opened = {
        let _entered = runtime.enter();
        open()
    };
    let pipe = match opened {
        Ok(pipe) => pipe,
        Err(error) => {
            let _ = handshake.send(Err(error));
            return;
        }
    };
    let accepted = !*stop.borrow() && Instant::now() < deadline && handshake.send(Ok(())).is_ok();
    if !accepted {
        // Late, stopped, or nobody waiting: the pipe is dropped here, on
        // the worker, and never used.
        let _ = handshake.send(Err(missed_handshake()));
        drop(pipe);
        return;
    }
    drop(handshake);
    let _ = runtime.block_on(run_session(pipe, commands, stop, events, max_frame));
}

/// Start a worker that opens with `open` inside its runtime, and wait for
/// its handshake until `deadline`. A handshake that arrives late, or a
/// caller that already gave up, discards the pipe on the worker.
pub(super) fn start<P, F>(
    open: F,
    deadline: Instant,
    max_frame: usize,
) -> std::io::Result<(Duplex, mpsc::Receiver<ServerMessage>)>
where
    P: AsyncRead + AsyncWrite + Unpin + 'static,
    F: FnOnce() -> std::io::Result<P> + Send + 'static,
{
    if Instant::now() >= deadline {
        return Err(deadline_before_attempt());
    }
    let (command_tx, command_rx) = queue::unbounded_channel();
    let (stop_tx, stop_rx) = watch::channel(false);
    let (event_tx, event_rx) = mpsc::channel();
    let (handshake_tx, handshake_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("formless-pipe-client".into())
        .spawn(move || {
            run_worker(
                open,
                deadline,
                handshake_tx,
                command_rx,
                stop_rx,
                event_tx,
                done_tx,
                max_frame,
            )
        })?;
    // From here on every early return drops `duplex`: stop, bounded wait,
    // join only if finished.
    let duplex = Duplex {
        commands: command_tx,
        stop: stop_tx,
        done: Mutex::new(done_rx),
        thread: Some(thread),
    };
    let wait = deadline.saturating_duration_since(Instant::now());
    match handshake_rx.recv_timeout(wait) {
        Ok(Ok(())) if Instant::now() < deadline => Ok((duplex, event_rx)),
        Ok(Ok(())) => Err(missed_handshake()),
        Ok(Err(error)) => Err(error),
        // An open error that raced the deadline is reported as itself.
        Err(mpsc::RecvTimeoutError::Timeout) => match handshake_rx.try_recv() {
            Ok(Err(error)) => Err(error),
            _ => Err(missed_handshake()),
        },
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::new(
            ErrorKind::ConnectionAborted,
            "named pipe worker ended before the handshake",
        )),
    }
}

/// Open `name` on a new worker. Only raw `ERROR_PIPE_BUSY` (231) is retried,
/// by the reviewed synchronous loop, until `deadline`. Its short sleeps run
/// on the worker thread before any task exists, never in the caller.
#[cfg(windows)]
pub(super) fn connect(
    name: String,
    deadline: Instant,
) -> std::io::Result<(Duplex, mpsc::Receiver<ServerMessage>)> {
    // The caller's handshake deadline can fire just before the worker sends
    // its final 231. Retain the most recent completed open result so that
    // this scheduling race does not relabel persistent busy as a new error.
    let last_open = std::sync::Arc::new(std::sync::atomic::AtomicI32::new(0));
    let observed = last_open.clone();
    let result = start(
        move || {
            // Defaults kept on purpose: overlapped, read and write, and
            // SECURITY_IDENTIFICATION | SECURITY_SQOS_PRESENT, so the server
            // can identify but not impersonate this client. The inherited
            // `OpenOptions` passed no QoS flags.
            let options = tokio::net::windows::named_pipe::ClientOptions::new();
            super::open_retrying_pipe_busy(
                deadline,
                || {
                    let result = options.open(&name);
                    observed.store(
                        result
                            .as_ref()
                            .err()
                            .and_then(Error::raw_os_error)
                            .unwrap_or(0),
                        std::sync::atomic::Ordering::Release,
                    );
                    result
                },
                Instant::now,
                std::thread::sleep,
            )
        },
        deadline,
        formless_protocol::MAX_FRAME_BYTES,
    );
    result.map_err(|error| {
        preserve_busy_at_handshake_deadline(
            error,
            last_open.load(std::sync::atomic::Ordering::Acquire),
        )
    })
}

fn preserve_busy_at_handshake_deadline(error: Error, last_open: i32) -> Error {
    if error.kind() == ErrorKind::TimedOut && last_open == super::RAW_ERROR_PIPE_BUSY {
        Error::from_raw_os_error(super::RAW_ERROR_PIPE_BUSY)
    } else {
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use formless_protocol::Event;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use tokio::io::{DuplexStream, ReadBuf};

    fn event(session: &str) -> String {
        serde_json::to_string(&ServerMessage::Event(Event::RunStarted {
            session_id: session.into(),
            run_id: "r".into(),
        }))
        .unwrap()
    }

    fn session_of(message: ServerMessage) -> String {
        match message {
            ServerMessage::Event(Event::RunStarted { session_id, .. }) => session_id,
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Everything the peer receives until EOF or `idle` without data.
    async fn read_for(peer: &mut DuplexStream, idle: Duration) -> Vec<u8> {
        let mut got = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            match tokio::time::timeout(idle, peer.read(&mut tmp)).await {
                Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return got,
                Ok(Ok(n)) => got.extend_from_slice(&tmp[..n]),
            }
        }
    }

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn handshake_timeout_preserves_observed_busy_without_masking_other_errors() {
        assert_eq!(
            preserve_busy_at_handshake_deadline(missed_handshake(), 231).raw_os_error(),
            Some(231)
        );
        assert_eq!(
            preserve_busy_at_handshake_deadline(missed_handshake(), 0).kind(),
            ErrorKind::TimedOut
        );
        assert_eq!(
            preserve_busy_at_handshake_deadline(Error::from_raw_os_error(5), 231).raw_os_error(),
            Some(5)
        );
    }

    #[test]
    fn production_cap_is_the_protocol_frame_cap() {
        assert_eq!(formless_protocol::MAX_FRAME_BYTES, 32 * 1024 * 1024);
    }

    #[tokio::test]
    async fn reader_keeps_order_skips_bad_json_and_parses_a_short_tail_at_eof() {
        let (client, mut server) = tokio::io::duplex(64);
        let (tx, rx) = mpsc::channel();
        let reader = tokio::spawn(framed_reader(client, tx, 4096));
        let wire = format!(
            "not-json\n{}\n{}\n{}",
            event("a"),
            event("b"),
            event("tail")
        );
        server.write_all(wire.as_bytes()).await.unwrap();
        drop(server);
        assert_eq!(reader.await.unwrap(), ReaderEnd::Eof);
        assert_eq!(session_of(rx.recv().unwrap()), "a");
        assert_eq!(session_of(rx.recv().unwrap()), "b");
        assert_eq!(session_of(rx.recv().unwrap()), "tail");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn a_line_of_exactly_max_is_accepted_and_max_plus_one_ends_the_reader() {
        let line = event("m");
        let max = line.len();
        let (client, mut server) = tokio::io::duplex(1024);
        let (tx, rx) = mpsc::channel();
        let reader = tokio::spawn(framed_reader(client, tx, max));
        let wire = format!("{line}\n{}", "x".repeat(max + 1));
        server.write_all(wire.as_bytes()).await.unwrap();
        assert_eq!(reader.await.unwrap(), ReaderEnd::Oversize);
        assert_eq!(session_of(rx.recv().unwrap()), "m");
        assert!(rx.try_recv().is_err());
    }

    struct Probe {
        data: Vec<u8>,
        pos: usize,
        largest_ask: Arc<AtomicUsize>,
        total: Arc<AtomicUsize>,
    }

    impl AsyncRead for Probe {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let this = self.get_mut();
            let ask = buf.remaining();
            this.largest_ask.fetch_max(ask, Ordering::SeqCst);
            let n = ask.min(this.data.len() - this.pos);
            buf.put_slice(&this.data[this.pos..this.pos + n]);
            this.pos += n;
            this.total.fetch_add(n, Ordering::SeqCst);
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn the_frame_cap_is_checked_before_the_buffer_can_pass_it() {
        let largest_ask = Arc::new(AtomicUsize::new(0));
        let total = Arc::new(AtomicUsize::new(0));
        let probe = Probe {
            data: vec![b'a'; 100],
            pos: 0,
            largest_ask: largest_ask.clone(),
            total: total.clone(),
        };
        let (tx, rx) = mpsc::channel();
        assert_eq!(framed_reader(probe, tx, 8).await, ReaderEnd::Oversize);
        assert!(largest_ask.load(Ordering::SeqCst) <= 9);
        assert!(total.load(Ordering::SeqCst) <= 9);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn reserve_within_never_passes_the_limit() {
        let mut buf = Vec::new();
        for _ in 0..40 {
            let extra = 7.min(101 - buf.len());
            if extra == 0 {
                break;
            }
            reserve_within(&mut buf, extra, 101);
            buf.extend(std::iter::repeat(b'a').take(extra));
            assert!(buf.capacity() <= 101 || buf.capacity() == buf.len());
        }
        assert_eq!(buf.len(), 101);
    }

    #[tokio::test]
    async fn invalid_utf8_ends_the_reader_like_read_line_capped() {
        let (client, mut server) = tokio::io::duplex(1024);
        let (tx, rx) = mpsc::channel();
        let reader = tokio::spawn(framed_reader(client, tx, 4096));
        let mut wire = b"\xff\n".to_vec();
        wire.extend_from_slice(format!("{}\n", event("after")).as_bytes());
        server.write_all(&wire).await.unwrap();
        assert_eq!(reader.await.unwrap(), ReaderEnd::InvalidUtf8);
        assert!(rx.try_recv().is_err());
    }

    struct Failing;

    impl AsyncRead for Failing {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Err(Error::from(ErrorKind::BrokenPipe)))
        }
    }

    impl AsyncWrite for Failing {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Err(Error::from(ErrorKind::BrokenPipe)))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn a_read_error_ends_the_reader_and_a_write_error_ends_the_writer() {
        let (tx, _rx) = mpsc::channel();
        assert_eq!(framed_reader(Failing, tx, 64).await, ReaderEnd::ReadFailed);
        let (queue_tx, queue_rx) = queue::unbounded_channel();
        queue_tx.send("x".to_string()).unwrap();
        let (_stop, stop_rx) = watch::channel(false);
        assert_eq!(
            ordered_writer(Failing, queue_rx, stop_rx).await,
            WriterEnd::Failed
        );
    }

    #[tokio::test]
    async fn a_dropped_event_receiver_ends_the_reader() {
        let (client, mut server) = tokio::io::duplex(1024);
        let (tx, rx) = mpsc::channel();
        drop(rx);
        let reader = tokio::spawn(framed_reader(client, tx, 4096));
        server
            .write_all(format!("{}\n", event("x")).as_bytes())
            .await
            .unwrap();
        assert_eq!(reader.await.unwrap(), ReaderEnd::ReceiverGone);
    }

    #[tokio::test]
    async fn the_writer_keeps_queue_order_one_frame_each() {
        let (client, mut server) = tokio::io::duplex(4096);
        let (tx, rx) = queue::unbounded_channel();
        for line in ["one", "two", "three"] {
            tx.send(line.to_string()).unwrap();
        }
        drop(tx);
        let (_stop, stop_rx) = watch::channel(false);
        assert_eq!(ordered_writer(client, rx, stop_rx).await, WriterEnd::Closed);
        let got = read_for(&mut server, Duration::from_millis(200)).await;
        assert_eq!(String::from_utf8(got).unwrap(), "one\ntwo\nthree\n");
    }

    /// Stop can become ready inside a write that completes synchronously.
    /// The writer must recheck it before taking a second queued frame.
    struct StopOnWrite {
        stop: watch::Sender<bool>,
        writes: Arc<AtomicUsize>,
    }

    impl AsyncWrite for StopOnWrite {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.stop.send_replace(true);
            Poll::Ready(Ok(bytes.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn stop_during_a_ready_write_prevents_dequeuing_the_next_frame() {
        let (stop, stop_rx) = watch::channel(false);
        let writes = Arc::new(AtomicUsize::new(0));
        let writer = StopOnWrite {
            stop,
            writes: writes.clone(),
        };
        let (tx, rx) = queue::unbounded_channel();
        tx.send("first".into()).unwrap();
        tx.send("must-not-write".into()).unwrap();
        assert_eq!(
            ordered_writer(writer, rx, stop_rx).await,
            WriterEnd::Stopped
        );
        assert_eq!(writes.load(Ordering::SeqCst), 1);
    }

    /// Reads never complete; every write fails.
    struct SilentBroken;

    impl AsyncRead for SilentBroken {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
    }

    impl AsyncWrite for SilentBroken {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Err(Error::from(ErrorKind::BrokenPipe)))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// Regression for the `join!(reader, writer)` draft: a write error with
    /// a silent server must end the session and close the event channel.
    #[tokio::test]
    async fn a_writer_error_ends_the_session_even_when_the_reader_is_silent() {
        let (tx, rx) = queue::unbounded_channel();
        tx.send("request".to_string()).unwrap();
        let (_stop_tx, stop_rx) = watch::channel(false);
        let (events_tx, events_rx) = mpsc::channel();
        let ended = tokio::time::timeout(
            Duration::from_secs(2),
            run_session(SilentBroken, rx, stop_rx, events_tx, 4096),
        )
        .await
        .expect("a silent reader kept the session alive");
        assert_eq!(ended, SessionEnd::WriterEnded);
        assert!(matches!(
            events_rx.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[tokio::test]
    async fn explicit_stop_preempts_a_ready_backlog() {
        let (client, mut server) = tokio::io::duplex(4096);
        let (tx, rx) = queue::unbounded_channel();
        tx.send("a".to_string()).unwrap();
        tx.send("b".to_string()).unwrap();
        let (stop_tx, stop_rx) = watch::channel(false);
        stop_tx.send_replace(true);
        let (events_tx, _events_rx) = mpsc::channel();
        assert_eq!(
            run_session(client, rx, stop_rx, events_tx, 4096).await,
            SessionEnd::Stopped
        );
        assert!(read_for(&mut server, Duration::from_millis(100))
            .await
            .is_empty());
    }

    /// Fake-stream semantics only: a `DuplexStream` accepts partial writes,
    /// so the head frame can be cut. Mio hands a whole frame to the OS in
    /// one call; that path is covered by the native fixture below.
    #[tokio::test]
    async fn stop_while_the_peer_does_not_read_returns_and_skips_the_backlog() {
        let (client, mut server) = tokio::io::duplex(8);
        let (tx, rx) = queue::unbounded_channel();
        tx.send("abcdefghijklmnop".to_string()).unwrap();
        tx.send("second".to_string()).unwrap();
        let (stop_tx, stop_rx) = watch::channel(false);
        let (events_tx, _events_rx) = mpsc::channel();
        let session = tokio::spawn(run_session(client, rx, stop_rx, events_tx, 4096));
        tokio::time::sleep(Duration::from_millis(50)).await;
        stop_tx.send_replace(true);
        let ended = tokio::time::timeout(Duration::from_secs(2), session)
            .await
            .expect("stop did not end a blocked writer")
            .unwrap();
        assert_eq!(ended, SessionEnd::Stopped);
        let got =
            String::from_utf8(read_for(&mut server, Duration::from_millis(200)).await).unwrap();
        assert!(got.starts_with("abcdefgh"), "{got}");
        assert!(!got.contains("second"), "{got}");
    }

    /// Reads end at once; writes land in `sink`.
    struct EofThenSink(Arc<Mutex<Vec<u8>>>);

    impl AsyncRead for EofThenSink {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for EofThenSink {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn reader_eof_closes_events_and_the_writer_runs_until_stop() {
        let sink = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = queue::unbounded_channel();
        let (stop_tx, stop_rx) = watch::channel(false);
        let (events_tx, events_rx) = mpsc::channel();
        let session = tokio::spawn(run_session(
            EofThenSink(sink.clone()),
            rx,
            stop_rx,
            events_tx,
            4096,
        ));
        let closed = Instant::now() + Duration::from_secs(2);
        while !matches!(events_rx.try_recv(), Err(mpsc::TryRecvError::Disconnected)) {
            assert!(Instant::now() < closed, "EOF did not close the events");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tx.send("late".to_string()).unwrap();
        let written = Instant::now() + Duration::from_secs(2);
        while sink.lock().unwrap().as_slice() != b"late\n" {
            assert!(Instant::now() < written, "writer stopped after reader EOF");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        stop_tx.send_replace(true);
        assert_eq!(session.await.unwrap(), SessionEnd::ReaderEnded);
    }

    /// A duplex end that records its drop, optionally slowly.
    struct Tracked {
        inner: DuplexStream,
        drops: Arc<AtomicUsize>,
        linger: Duration,
    }

    impl Drop for Tracked {
        fn drop(&mut self) {
            std::thread::sleep(self.linger);
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl AsyncRead for Tracked {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for Tracked {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
        }
        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.get_mut().inner).poll_flush(cx)
        }
        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
        }
    }

    fn wait_for_drops(drops: &AtomicUsize, want: usize) {
        let until = Instant::now() + Duration::from_secs(3);
        while drops.load(Ordering::SeqCst) < want {
            assert!(Instant::now() < until, "the worker never dropped its pipe");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_started_worker_carries_frames_both_ways_and_joins_on_close() {
        let (client, mut server) = tokio::io::duplex(4096);
        let (duplex, events) = start(
            move || Ok(client),
            Instant::now() + Duration::from_secs(5),
            4096,
        )
        .unwrap();
        duplex.enqueue("ping".to_string()).unwrap();
        let rt = test_runtime();
        let mut line = [0u8; 5];
        rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), server.read_exact(&mut line))
                .await
                .expect("request never reached the peer")
                .unwrap();
            server
                .write_all(format!("{}\n", event("e")).as_bytes())
                .await
                .unwrap();
        });
        assert_eq!(&line, b"ping\n");
        let got = events.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(session_of(got), "e");
        let started = Instant::now();
        assert_eq!(duplex.close(WORKER_TEARDOWN_BOUND), WorkerFinish::Joined);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }

    #[test]
    fn an_open_error_is_returned_as_itself() {
        let error = start(
            || Err::<DuplexStream, _>(Error::new(ErrorKind::PermissionDenied, "denied")),
            Instant::now() + Duration::from_secs(5),
            4096,
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "denied");
    }

    #[test]
    fn a_worker_that_dies_before_the_handshake_is_a_start_failure() {
        let started = Instant::now();
        let error = start(
            || -> std::io::Result<DuplexStream> { panic!("synthetic start failure") },
            Instant::now() + Duration::from_secs(5),
            4096,
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), ErrorKind::ConnectionAborted);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn an_already_due_deadline_spawns_and_opens_nothing() {
        let opened = Arc::new(AtomicBool::new(false));
        let seen = opened.clone();
        let error = start(
            move || {
                seen.store(true, Ordering::SeqCst);
                Ok(tokio::io::duplex(8).0)
            },
            Instant::now(),
            4096,
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        assert!(error.to_string().contains("before an attempt"));
        assert!(!opened.load(Ordering::SeqCst));
    }

    #[test]
    fn a_late_handshake_is_refused_and_the_worker_drops_the_pipe() {
        let drops = Arc::new(AtomicUsize::new(0));
        let tracked = drops.clone();
        let started = Instant::now();
        let error = start(
            move || {
                std::thread::sleep(Duration::from_millis(300));
                Ok(Tracked {
                    inner: tokio::io::duplex(8).0,
                    drops: tracked,
                    linger: Duration::ZERO,
                })
            },
            Instant::now() + Duration::from_millis(50),
            4096,
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        // Construction wait plus at most the teardown bound, with slack.
        assert!(started.elapsed() < Duration::from_secs(1));
        wait_for_drops(&drops, 1);
    }

    #[test]
    fn teardown_detaches_a_worker_that_is_still_busy_within_the_bound() {
        let drops = Arc::new(AtomicUsize::new(0));
        let tracked = drops.clone();
        let (duplex, _events) = start(
            move || {
                Ok(Tracked {
                    inner: tokio::io::duplex(8).0,
                    drops: tracked,
                    linger: Duration::from_millis(800),
                })
            },
            Instant::now() + Duration::from_secs(5),
            4096,
        )
        .unwrap();
        let started = Instant::now();
        assert_eq!(
            duplex.close(Duration::from_millis(50)),
            WorkerFinish::Detached
        );
        assert!(started.elapsed() < Duration::from_millis(500));
        wait_for_drops(&drops, 1);
    }

    #[test]
    fn repeated_connect_and_drop_joins_every_worker() {
        for _ in 0..25 {
            let (client, _server) = tokio::io::duplex(64);
            let (duplex, _events) = start(
                move || Ok(client),
                Instant::now() + Duration::from_secs(5),
                64,
            )
            .unwrap();
            duplex.enqueue("x".to_string()).unwrap();
            assert_eq!(duplex.close(WORKER_TEARDOWN_BOUND), WorkerFinish::Joined);
        }
    }

    async fn connect_inside_an_existing_runtime() {
        let (client, mut server) = tokio::io::duplex(64);
        let (duplex, _events) = start(
            move || Ok(client),
            Instant::now() + Duration::from_secs(5),
            64,
        )
        .unwrap();
        duplex.enqueue("hi".to_string()).unwrap();
        let mut got = [0u8; 3];
        tokio::time::timeout(Duration::from_secs(2), server.read_exact(&mut got))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&got, b"hi\n");
        drop(duplex);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn connect_from_a_current_thread_runtime_does_not_nest() {
        connect_inside_an_existing_runtime().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connect_from_a_multi_thread_runtime_does_not_nest() {
        connect_inside_an_existing_runtime().await;
    }
}

/// Owned synthetic Windows pipe servers. No daemon, profile or user data.
#[cfg(all(test, windows))]
mod native_tests {
    use super::*;
    use formless_protocol::Event;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::AsyncBufReadExt;
    use tokio::io::ReadBuf;
    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    const IO_BOUND: Duration = Duration::from_secs(5);

    async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(IO_BOUND, future)
            .await
            .expect("owned pipe operation timed out")
    }

    #[derive(Debug)]
    enum Observed {
        ReadPending,
        ReadBytes(usize),
        ReadError(ErrorKind, Option<i32>),
        WritePending,
    }

    struct ObservedPipe {
        pipe: NamedPipeClient,
        observations: mpsc::Sender<Observed>,
    }

    impl AsyncRead for ObservedPipe {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let this = self.get_mut();
            let before = buf.filled().len();
            let result = Pin::new(&mut this.pipe).poll_read(cx, buf);
            match &result {
                Poll::Pending => {
                    let _ = this.observations.send(Observed::ReadPending);
                }
                Poll::Ready(Ok(())) => {
                    let _ = this
                        .observations
                        .send(Observed::ReadBytes(buf.filled().len() - before));
                }
                Poll::Ready(Err(error)) => {
                    let _ = this
                        .observations
                        .send(Observed::ReadError(error.kind(), error.raw_os_error()));
                }
            }
            result
        }
    }
    impl AsyncWrite for ObservedPipe {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            let this = self.get_mut();
            let result = Pin::new(&mut this.pipe).poll_write(cx, buf);
            if result.is_pending() {
                let _ = this.observations.send(Observed::WritePending);
            }
            result
        }
        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.get_mut().pipe).poll_flush(cx)
        }
        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.get_mut().pipe).poll_shutdown(cx)
        }
    }

    fn observed_connect(
        name: String,
    ) -> (
        Duplex,
        mpsc::Receiver<ServerMessage>,
        mpsc::Receiver<Observed>,
    ) {
        let (tx, rx) = mpsc::channel();
        let (duplex, events) = start(
            move || {
                Ok(ObservedPipe {
                    pipe: ClientOptions::new().open(&name)?,
                    observations: tx,
                })
            },
            soon(),
            formless_protocol::MAX_FRAME_BYTES,
        )
        .unwrap();
        (duplex, events, rx)
    }

    fn observe_until(rx: &mpsc::Receiver<Observed>, mut done: impl FnMut(Observed) -> bool) {
        let deadline = Instant::now() + IO_BOUND;
        loop {
            let left = deadline
                .checked_duration_since(Instant::now())
                .expect("pipe observation timed out");
            if done(rx.recv_timeout(left).expect("pipe observation missing")) {
                return;
            }
        }
    }

    fn pipe_name() -> String {
        format!(r"\\.\pipe\fl-duplex-test-{}", uuid::Uuid::new_v4())
    }

    fn server_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    fn instance(rt: &tokio::runtime::Runtime, name: &str, first: bool) -> NamedPipeServer {
        let _entered = rt.enter();
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create(name)
            .unwrap()
    }

    fn event_line(session: &str) -> String {
        let mut line = serde_json::to_string(&ServerMessage::Event(Event::RunStarted {
            session_id: session.into(),
            run_id: "r".into(),
        }))
        .unwrap();
        line.push('\n');
        line
    }

    fn session_of(message: ServerMessage) -> String {
        match message {
            ServerMessage::Event(Event::RunStarted { session_id, .. }) => session_id,
            other => panic!("unexpected {other:?}"),
        }
    }

    async fn read_line_within(
        reader: &mut tokio::io::BufReader<NamedPipeServer>,
        bound: Duration,
    ) -> String {
        let mut line = String::new();
        tokio::time::timeout(bound, reader.read_line(&mut line))
            .await
            .expect("line did not arrive")
            .unwrap();
        line
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn pending_read_first_then_first_and_later_writes_reach_the_server() {
        let rt = server_runtime();
        let name = pipe_name();
        let server = instance(&rt, &name, true);
        let (duplex, events, observations) = observed_connect(name);
        observe_until(&observations, |event| {
            matches!(event, Observed::ReadPending)
        });
        duplex.enqueue("first".to_string()).unwrap();
        let mut reader = rt.block_on(async {
            bounded(server.connect()).await.unwrap();
            tokio::io::BufReader::new(server)
        });
        rt.block_on(async {
            assert_eq!(
                read_line_within(&mut reader, Duration::from_secs(5)).await,
                "first\n"
            );
        });
        // Consume a reply, then require a new pending read after those bytes.
        rt.block_on(bounded(
            reader.get_mut().write_all(event_line("between").as_bytes()),
        ))
        .unwrap();
        assert_eq!(
            session_of(events.recv_timeout(IO_BOUND).unwrap()),
            "between"
        );
        observe_until(
            &observations,
            |event| matches!(event, Observed::ReadBytes(n) if n > 0),
        );
        observe_until(&observations, |event| {
            matches!(event, Observed::ReadPending)
        });
        duplex.enqueue("later".to_string()).unwrap();
        rt.block_on(async {
            assert_eq!(
                read_line_within(&mut reader, Duration::from_secs(5)).await,
                "later\n"
            );
            let wire = format!("{}{}", event_line("a"), event_line("b"));
            bounded(reader.get_mut().write_all(wire.as_bytes()))
                .await
                .unwrap();
        });
        assert_eq!(
            session_of(events.recv_timeout(Duration::from_secs(5)).unwrap()),
            "a"
        );
        assert_eq!(
            session_of(events.recv_timeout(Duration::from_secs(5)).unwrap()),
            "b"
        );
        let started = Instant::now();
        assert_eq!(duplex.close(WORKER_TEARDOWN_BOUND), WorkerFinish::Joined);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn frames_bad_json_and_a_tail_then_eof_over_a_real_pipe() {
        let rt = server_runtime();
        let name = pipe_name();
        let mut server = instance(&rt, &name, true);
        let (duplex, events, observations) = observed_connect(name);
        let wire = format!(
            "not-json\n{}{}",
            event_line("a"),
            event_line("tail").trim_end()
        );
        rt.block_on(async {
            bounded(server.connect()).await.unwrap();
            bounded(server.write_all(wire.as_bytes())).await.unwrap();
        });
        // A completed write/flush is not a drain on Windows. Wait until the
        // actual client reader has consumed every byte before closing the handle.
        let mut delivered = 0;
        observe_until(&observations, |event| {
            if let Observed::ReadBytes(n) = event {
                delivered += n;
            }
            delivered == wire.len()
        });
        assert_eq!(session_of(events.recv_timeout(IO_BOUND).unwrap()), "a");
        // DisconnectNamedPipe forcibly severs an instance and may report a
        // hard pipe error. Closing the drained server handle tests EOF.
        drop(server);
        observe_until(&observations, |event| match event {
            Observed::ReadBytes(0) => true,
            Observed::ReadError(kind, code) => {
                panic!("expected EOF after server handle close, got {kind:?} ({code:?})")
            }
            _ => false,
        });
        assert_eq!(session_of(events.recv_timeout(IO_BOUND).unwrap()), "tail");
        assert!(matches!(
            events.recv_timeout(IO_BOUND),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
        drop(duplex);
    }

    #[test]
    fn an_oversized_frame_over_a_real_pipe_closes_the_events() {
        let rt = server_runtime();
        let name = pipe_name();
        let mut server = instance(&rt, &name, true);
        let open_name = name.clone();
        let (duplex, events) =
            start(move || ClientOptions::new().open(&open_name), soon(), 64).unwrap();
        rt.block_on(async {
            bounded(server.connect()).await.unwrap();
            bounded(server.write_all(&[b'x'; 65])).await.unwrap();
        });
        assert!(matches!(
            events.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
        drop(duplex);
        drop(server);
    }

    #[test]
    fn a_missing_pipe_is_not_retried() {
        let started = Instant::now();
        let error = connect(pipe_name(), Instant::now() + Duration::from_millis(150))
            .err()
            .unwrap();
        assert_eq!(error.kind(), ErrorKind::NotFound, "{error}");
        assert_eq!(error.raw_os_error(), Some(2));
        assert!(started.elapsed() < Duration::from_millis(150) + WORKER_TEARDOWN_BOUND);
    }

    #[test]
    fn an_already_due_deadline_does_not_open_a_real_pipe() {
        let rt = server_runtime();
        let name = pipe_name();
        let _server = instance(&rt, &name, true);
        let error = connect(name.clone(), Instant::now()).err().unwrap();
        assert_eq!(error.kind(), ErrorKind::TimedOut);
        // The only instance is still free for a real client.
        let (duplex, _events) = connect(name, soon()).unwrap();
        drop(duplex);
    }

    #[test]
    fn only_raw_231_is_retried_and_a_recreated_instance_is_opened() {
        let rt = server_runtime();
        let name = pipe_name();
        let _first = instance(&rt, &name, true);
        let (holder, _holder_events) = connect(name.clone(), soon()).unwrap();
        // No listening instance: busy until the short deadline.
        let started = Instant::now();
        let error = connect(name.clone(), Instant::now() + Duration::from_millis(60))
            .err()
            .unwrap();
        assert!(error.raw_os_error() == Some(231), "{error}");
        assert!(started.elapsed() < Duration::from_millis(60) + Duration::from_secs(1));
        // A replacement instance within the 150ms budget is opened.
        let later = name.clone();
        let creator = rt.spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            ServerOptions::new()
                .reject_remote_clients(true)
                .create(&later)
                .unwrap()
        });
        let (second, _second_events) =
            connect(name, Instant::now() + Duration::from_millis(150)).unwrap();
        let _replacement = rt.block_on(bounded(creator)).unwrap();
        drop(second);
        drop(holder);
    }

    #[test]
    fn a_silent_server_does_not_hold_teardown() {
        let rt = server_runtime();
        let name = pipe_name();
        let server = instance(&rt, &name, true);
        let (duplex, _events, observations) = observed_connect(name);
        rt.block_on(bounded(server.connect())).unwrap();
        observe_until(&observations, |event| {
            matches!(event, Observed::ReadPending)
        });
        let started = Instant::now();
        // Mio cancels the pending read on drop, so the worker finishes.
        assert_eq!(duplex.close(WORKER_TEARDOWN_BOUND), WorkerFinish::Joined);
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(server);
    }

    /// The server never reads. Teardown is bounded for the caller; the
    /// in-flight write is not canceled and its handle may stay open, so the
    /// fixture does not wait for EOF.
    #[test]
    fn a_non_reading_server_gets_bounded_teardown_and_no_backlog_after_stop() {
        let rt = server_runtime();
        let name = pipe_name();
        let mut server = instance(&rt, &name, true);
        let (duplex, _events, observations) = observed_connect(name);
        rt.block_on(bounded(server.connect())).unwrap();
        let head = "a".repeat(1 << 20);
        duplex.enqueue(head.clone()).unwrap();
        duplex.enqueue("second".to_string()).unwrap();
        duplex.enqueue("third".to_string()).unwrap();
        observe_until(&observations, |event| {
            matches!(event, Observed::WritePending)
        });
        let started = Instant::now();
        let finish = duplex.close(WORKER_TEARDOWN_BOUND);
        assert!(
            started.elapsed() < WORKER_TEARDOWN_BOUND + Duration::from_millis(500),
            "{finish:?}"
        );
        let got = rt.block_on(bounded(async {
            let mut got = Vec::new();
            let mut tmp = vec![0u8; 64 * 1024];
            loop {
                match tokio::time::timeout(Duration::from_millis(300), server.read(&mut tmp)).await
                {
                    Ok(Ok(0)) | Ok(Err(_)) | Err(_) => return got,
                    Ok(Ok(n)) => got.extend_from_slice(&tmp[..n]),
                }
            }
        }));
        let text = String::from_utf8_lossy(&got);
        assert!(text.starts_with("aaaa"), "head frame never started");
        assert!(!text.contains("second"), "backlog was written after stop");
        assert!(!text.contains("third"), "backlog was written after stop");
    }

    #[test]
    fn a_dropped_event_receiver_keeps_later_requests_flowing() {
        let rt = server_runtime();
        let name = pipe_name();
        let server = instance(&rt, &name, true);
        let (duplex, events, observations) = observed_connect(name);
        drop(events);
        let mut reader = rt.block_on(async {
            bounded(server.connect()).await.unwrap();
            let mut reader = tokio::io::BufReader::new(server);
            bounded(reader.get_mut().write_all(event_line("nobody").as_bytes()))
                .await
                .unwrap();
            reader
        });
        let mut delivered = 0;
        observe_until(&observations, |event| {
            if let Observed::ReadBytes(n) = event {
                delivered += n;
            }
            delivered == event_line("nobody").len()
        });
        duplex.enqueue("after".to_string()).unwrap();
        rt.block_on(async {
            assert_eq!(
                read_line_within(&mut reader, Duration::from_secs(5)).await,
                "after\n"
            );
        });
        assert_eq!(duplex.close(WORKER_TEARDOWN_BOUND), WorkerFinish::Joined);
    }

    #[test]
    fn repeated_connect_and_drop_against_a_recreating_server() {
        const CYCLES: usize = 20;
        let rt = server_runtime();
        let name = pipe_name();
        let first = instance(&rt, &name, true);
        let accept_name = name.clone();
        let accepted = rt.spawn(async move {
            let mut server = first;
            let mut seen = 0;
            for _ in 0..CYCLES {
                bounded(server.connect()).await.unwrap();
                let next = ServerOptions::new()
                    .reject_remote_clients(true)
                    .create(&accept_name)
                    .unwrap();
                let mut reader = tokio::io::BufReader::new(server);
                let mut line = String::new();
                bounded(reader.read_line(&mut line)).await.unwrap();
                assert_eq!(line, "ping\n");
                bounded(reader.get_mut().write_all(event_line("ack").as_bytes()))
                    .await
                    .unwrap();
                seen += 1;
                server = next;
            }
            seen
        });
        for _ in 0..CYCLES {
            let (duplex, events) = connect(name.clone(), soon()).unwrap();
            duplex.enqueue("ping".to_string()).unwrap();
            assert_eq!(
                session_of(events.recv_timeout(Duration::from_secs(5)).unwrap()),
                "ack"
            );
            assert_eq!(duplex.close(WORKER_TEARDOWN_BOUND), WorkerFinish::Joined);
        }
        let seen = rt
            .block_on(async { tokio::time::timeout(Duration::from_secs(10), accepted).await })
            .expect("server did not see every cycle")
            .unwrap();
        assert_eq!(seen, CYCLES);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connect_inside_an_existing_runtime_over_a_real_pipe() {
        let name = pipe_name();
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create(&name)
            .unwrap();
        let (duplex, _events) = connect(name, soon()).unwrap();
        duplex.enqueue("hi".to_string()).unwrap();
        bounded(server.connect()).await.unwrap();
        let mut got = [0u8; 3];
        tokio::time::timeout(Duration::from_secs(5), server.read_exact(&mut got))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&got, b"hi\n");
        drop(duplex);
    }
}
