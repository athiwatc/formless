//! Bounded nonblocking I/O for private Unix-socket test daemons.
//!
//! On Darwin, setting SO_RCVTIMEO/SO_SNDTIMEO can fail with EINVAL after
//! the reachability probe has already closed its peer. Do not configure
//! kernel timeouts: poll the owned nonblocking socket with an absolute
//! deadline and stop flag instead. EOF remains EOF, not a fixture failure.
use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

pub struct FixtureStream {
    stream: UnixStream,
    stop: Arc<AtomicBool>,
    deadline: Instant,
    read_timeout: Duration,
}
impl FixtureStream {
    pub fn new(
        stream: UnixStream,
        stop: Arc<AtomicBool>,
        deadline: Instant,
        read_timeout: Duration,
    ) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            stop,
            deadline,
            read_timeout,
        })
    }
    // Only the buffered-line fixture needs a second handle.
    #[allow(dead_code)]
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            stream: self.stream.try_clone()?,
            stop: self.stop.clone(),
            deadline: self.deadline,
            read_timeout: self.read_timeout,
        })
    }
    fn expired(&self, until: Instant) -> bool {
        Instant::now() >= until.min(self.deadline)
    }
}
impl Read for FixtureStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let until = Instant::now() + self.read_timeout;
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return Ok(0);
            }
            if self.expired(until) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "private fixture read deadline",
                ));
            }
            match self.stream.read(bytes) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::park_timeout(Duration::from_millis(2))
                }
                result => return result,
            }
        }
    }
}
impl Write for FixtureStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "private fixture stopped",
                ));
            }
            if self.expired(until) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "private fixture write deadline",
                ));
            }
            match self.stream.write(bytes) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::park_timeout(Duration::from_millis(2))
                }
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
