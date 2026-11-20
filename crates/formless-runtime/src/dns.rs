//! Deadline-aware admission to a fixed OS-resolver pool. OS lookups cannot
//! be cancelled portably; a stalled lookup occupies one of four workers,
//! never a new abandoned resolver thread for each fetch.

use crate::{sync::LockExt, FetchTimeouts};
use std::collections::VecDeque;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};
use std::time::Instant;

type LookupResult = Result<Vec<SocketAddr>, String>;
type Lookup = dyn Fn(&str, u16) -> LookupResult + Send + Sync + 'static;

struct Job {
    host: String,
    port: u16,
    deadline: Instant,
    reply: mpsc::SyncSender<LookupResult>,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    closed: bool,
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    changed: Condvar,
}

struct ResolverPool {
    shared: Arc<Shared>,
    capacity: usize,
}

fn timed_out(host: &str, timeouts: FetchTimeouts) -> String {
    format!(
        "fetch timed out: DNS resolution for {host} exceeded the connection/total deadline (configured {})",
        timeouts.settings()
    )
}

impl ResolverPool {
    fn new(workers: usize, capacity: usize, lookup: Arc<Lookup>) -> Result<Self, String> {
        assert!(workers > 0 && capacity > 0);
        let pool = Self {
            shared: Arc::default(),
            capacity,
        };
        for worker in 0..workers {
            let shared = pool.shared.clone();
            let lookup = lookup.clone();
            std::thread::Builder::new()
                .name(format!("formless-dns-{worker}"))
                .spawn(move || loop {
                    let job = {
                        let mut queue = shared.queue.locked();
                        while queue.jobs.is_empty() && !queue.closed {
                            queue = shared
                                .changed
                                .wait(queue)
                                .unwrap_or_else(|e| e.into_inner());
                        }
                        if queue.closed {
                            return;
                        }
                        let job = queue.jobs.pop_front().unwrap();
                        shared.changed.notify_all();
                        job
                    };
                    // Expired queued work must not consume an OS lookup after
                    // its fetch has already left. Late replies are discarded.
                    if Instant::now() < job.deadline {
                        let result = lookup(&job.host, job.port);
                        if Instant::now() < job.deadline {
                            let _ = job.reply.send(result);
                        }
                    }
                })
                .map_err(|e| format!("cannot start bounded DNS resolver: {e}"))?;
        }
        Ok(pool)
    }

    fn resolve(
        &self,
        host: &str,
        port: u16,
        deadline: Instant,
        timeouts: FetchTimeouts,
    ) -> LookupResult {
        self.resolve_until(host, port, deadline, || timed_out(host, timeouts))
    }

    fn resolve_until(
        &self,
        host: &str,
        port: u16,
        deadline: Instant,
        timeout_error: impl Fn() -> String,
    ) -> LookupResult {
        let (reply, result) = mpsc::sync_channel(1);
        let mut queue = self.shared.queue.locked();
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(timeout_error());
            }
            if queue.closed {
                return Err("fetch DNS resolver is unavailable".into());
            }
            // Reclaim expired queue slots even if every worker is in the OS.
            queue.jobs.retain(|job| job.deadline > now);
            if queue.jobs.len() < self.capacity {
                queue.jobs.push_back(Job {
                    host: host.into(),
                    port,
                    deadline,
                    reply,
                });
                self.shared.changed.notify_all();
                break;
            }
            (queue, _) = self
                .shared
                .changed
                .wait_timeout(queue, deadline.saturating_duration_since(now))
                .unwrap_or_else(|e| e.into_inner());
        }
        drop(queue);
        match result.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(_) if Instant::now() >= deadline => Err(timeout_error()),
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(timeout_error()),
            Err(mpsc::RecvTimeoutError::Disconnected) if Instant::now() >= deadline => {
                Err(timeout_error())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("fetch DNS resolver worker failed".into())
            }
        }
    }
}

impl Drop for ResolverPool {
    fn drop(&mut self) {
        let mut queue = self.shared.queue.locked();
        queue.closed = true;
        queue.jobs.clear();
        self.shared.changed.notify_all();
        // Do not join a worker that may be blocked inside the OS. Production's
        // one shared pool lives for the process; its worker count stays fixed.
    }
}

pub(super) fn resolve(
    host: &str,
    port: u16,
    deadline: Instant,
    timeouts: FetchTimeouts,
) -> LookupResult {
    shared_pool()?.resolve(host, port, deadline, timeouts)
}

pub(super) fn resolve_host_until(host: &str, port: u16, deadline: Instant) -> LookupResult {
    shared_pool()?.resolve_until(host, port, deadline, || {
        format!("timed out resolving {host}")
    })
}

fn shared_pool() -> Result<&'static ResolverPool, String> {
    static POOL: OnceLock<Result<ResolverPool, String>> = OnceLock::new();
    POOL.get_or_init(|| {
        ResolverPool::new(
            4,
            16,
            Arc::new(|host, port| {
                (host, port)
                    .to_socket_addrs()
                    .map(|addresses| addresses.collect())
                    .map_err(|e| format!("cannot resolve {host}: {e}"))
            }),
        )
    })
    .as_ref()
    .map_err(Clone::clone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn addresses() -> Vec<SocketAddr> {
        vec!["1.1.1.1:80".parse().unwrap()]
    }

    #[test]
    fn stalled_os_lookup_does_not_strand_http_send_worker_and_pool_recovers() {
        // DNS receives the smaller connection/total budget in either order.
        for (connect_ms, total_ms) in [(80, 200), (200, 80)] {
            let timeouts = FetchTimeouts {
                connect_ms,
                total_ms,
            };
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let release_rx = Mutex::new(release_rx);
            let (finished_tx, finished_rx) = mpsc::channel();
            let pool = Arc::new(
                ResolverPool::new(
                    1,
                    1,
                    Arc::new(move |host, _| {
                        entered_tx.send(host.to_string()).unwrap();
                        if host == "stalled.example" {
                            release_rx.locked().recv().unwrap();
                            finished_tx.send(()).unwrap();
                        }
                        Ok(addresses())
                    }),
                )
                .unwrap(),
            );
            let (completed_tx, completed_rx) = mpsc::channel();
            let caller_pool = pool.clone();
            let caller = std::thread::spawn(move || {
                let request = crate::FetchRequest {
                    method: reqwest::Method::GET,
                    url: reqwest::Url::parse("http://stalled.example/").unwrap(),
                    headers: Vec::new(),
                    body: None,
                    scope: vec!["stalled.example".into()],
                    timeouts,
                };
                let result = crate::send_request_with_resolver(request, &|host, port, deadline| {
                    assert!(
                        deadline.saturating_duration_since(Instant::now())
                            <= Duration::from_millis(connect_ms.min(total_ms))
                    );
                    caller_pool.resolve(host, port, deadline, timeouts)
                })
                .map(|_| ());
                // This is the HTTP worker's own completion, while the OS
                // lookup remains held by a separate, fixed pool worker.
                completed_tx.send(result).unwrap();
            });
            assert_eq!(
                entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                "stalled.example"
            );
            let error = completed_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("HTTP worker remained blocked in OS DNS")
                .unwrap_err();
            assert!(
                error.starts_with("fetch timed out: DNS resolution"),
                "{error}"
            );
            assert!(
                error.ends_with(&format!("(configured {})", timeouts.settings())),
                "{error}"
            );
            caller.join().unwrap();
            assert!(
                finished_rx.try_recv().is_err(),
                "OS gate unexpectedly released"
            );
            release_tx.send(()).unwrap();
            finished_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(
                pool.resolve(
                    "fresh.example",
                    80,
                    Instant::now() + Duration::from_secs(2),
                    timeouts
                )
                .unwrap(),
                addresses()
            );
            assert_eq!(
                entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                "fresh.example"
            );
        }
    }

    #[test]
    fn fixed_pool_bounds_queue_and_skips_expired_work_before_reuse() {
        const WORKERS: usize = 2;
        const CAPACITY: usize = 3;
        let timeouts = FetchTimeouts {
            connect_ms: 300,
            total_ms: 5000,
        };
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let pool = Arc::new(
            ResolverPool::new(WORKERS, CAPACITY, {
                let active = active.clone();
                let peak = peak.clone();
                Arc::new(move |host, _| {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(current, Ordering::SeqCst);
                    entered_tx.send(host.to_string()).unwrap();
                    if host.starts_with("blocked") {
                        release_rx.locked().recv().unwrap();
                    }
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(addresses())
                })
            })
            .unwrap(),
        );
        let mut blockers = Vec::new();
        for index in 0..WORKERS {
            let pool = pool.clone();
            blockers.push(std::thread::spawn(move || {
                pool.resolve(
                    &format!("blocked-{index}"),
                    80,
                    Instant::now() + Duration::from_secs(5),
                    timeouts,
                )
            }));
        }
        for _ in 0..WORKERS {
            assert!(entered_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .starts_with("blocked"));
        }
        let (expired_tx, expired_rx) = mpsc::channel();
        let mut waiters = Vec::new();
        for index in 0..CAPACITY {
            let pool = pool.clone();
            let expired_tx = expired_tx.clone();
            waiters.push(std::thread::spawn(move || {
                expired_tx
                    .send(pool.resolve(
                        &format!("queued-{index}"),
                        80,
                        Instant::now() + Duration::from_millis(300),
                        timeouts,
                    ))
                    .unwrap();
            }));
        }
        // Observe actual queue admission before testing saturation; no sleep
        // assumes a caller has reached the resolver.
        let until = Instant::now() + Duration::from_secs(2);
        let mut queue = pool.shared.queue.locked();
        while queue.jobs.len() < CAPACITY {
            let remaining = until.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "resolver queue was never filled");
            (queue, _) = pool.shared.changed.wait_timeout(queue, remaining).unwrap();
        }
        assert_eq!(queue.jobs.len(), CAPACITY);
        drop(queue);
        for index in 0..10 {
            let pool = pool.clone();
            let expired_tx = expired_tx.clone();
            waiters.push(std::thread::spawn(move || {
                expired_tx
                    .send(pool.resolve(
                        &format!("overflow-{index}"),
                        80,
                        Instant::now() + Duration::from_millis(300),
                        timeouts,
                    ))
                    .unwrap();
            }));
        }
        for _ in 0..CAPACITY + 10 {
            let error = expired_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("full-queue caller did not respect its deadline")
                .unwrap_err();
            assert!(
                error.starts_with("fetch timed out: DNS resolution"),
                "{error}"
            );
            assert!(pool.shared.queue.locked().jobs.len() <= CAPACITY);
        }
        for waiter in waiters {
            waiter.join().unwrap();
        }
        assert_eq!(active.load(Ordering::SeqCst), WORKERS);
        assert_eq!(peak.load(Ordering::SeqCst), WORKERS);
        assert!(
            entered_rx.try_recv().is_err(),
            "queued work started without capacity"
        );
        for _ in 0..WORKERS {
            release_tx.send(()).unwrap();
        }
        for blocker in blockers {
            assert_eq!(blocker.join().unwrap().unwrap(), addresses());
        }
        let until = Instant::now() + Duration::from_secs(2);
        let mut queue = pool.shared.queue.locked();
        while !queue.jobs.is_empty() {
            let remaining = until.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "expired queue was never drained");
            (queue, _) = pool.shared.changed.wait_timeout(queue, remaining).unwrap();
        }
        drop(queue);
        assert!(
            entered_rx.try_recv().is_err(),
            "expired queued lookup executed"
        );
        assert_eq!(
            pool.resolve(
                "fresh",
                80,
                Instant::now() + Duration::from_secs(2),
                timeouts
            )
            .unwrap(),
            addresses()
        );
        assert_eq!(
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            "fresh"
        );
        assert_eq!(peak.load(Ordering::SeqCst), WORKERS);
    }
}
