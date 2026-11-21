//! Accept owned fake-daemon connections independently; a one-shot client's
//! reader can keep the preceding socket open until our next read deadline.
use std::{
    io,
    os::unix::net::{UnixListener, UnixStream},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    thread::{self, ScopedJoinHandle},
    time::Duration,
};

const MAX_CONNECTIONS: usize = 16;
const POLL_INTERVAL: Duration = Duration::from_millis(2);

pub(super) fn serve(
    listener: &UnixListener,
    stop: &AtomicBool,
    accepted: &AtomicUsize,
    handle: impl Fn(UnixStream) + Sync,
) {
    thread::scope(|scope| {
        let handle = &handle;
        let mut workers: Vec<ScopedJoinHandle<'_, ()>> = Vec::new();
        while !stop.load(Ordering::SeqCst) {
            let mut i = 0;
            while i < workers.len() {
                if workers[i].is_finished() {
                    // Don't silently discard a failed fixture assertion.
                    workers.swap_remove(i).join().unwrap();
                } else {
                    i += 1;
                }
            }
            if workers.len() == MAX_CONNECTIONS {
                thread::sleep(POLL_INTERVAL);
                continue;
            }
            let (socket, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(POLL_INTERVAL);
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            accepted.fetch_add(1, Ordering::SeqCst);
            workers.push(scope.spawn(move || handle(socket)));
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
}
