//! Invocation-owned processes. Pipes are backpressured in bounded native queues;
//! the supervisor remains able to enforce deadlines while JavaScript is idle.
use super::{encode, utf8_cut, Enc, LockExt};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub(super) const MAX_HANDLES: usize = 16;
const QUEUED_PER_STREAM: usize = 256 * 1024;
const PIPE_CHUNK: usize = 64 * 1024;

pub(super) struct RunOutput {
    cap: usize,
    stdout_enc: Enc,
    stdout: CapturedStream,
    stderr: CapturedStream,
}

impl RunOutput {
    pub(super) fn new(cap: usize, stdout_enc: Enc) -> Self {
        Self {
            cap,
            stdout_enc,
            stdout: CapturedStream::default(),
            stderr: CapturedStream::default(),
        }
    }

    pub(super) fn push(&mut self, stdout: bool, chunk: &[u8]) {
        if stdout {
            self.stdout.push(chunk, self.cap, self.stdout_enc);
        } else {
            self.stderr.push(chunk, self.cap, Enc::Text);
        }
    }

    pub(super) fn finish(self, mut result: Value) -> Value {
        result["stdout_cap_truncated"] = json!(self.stdout.truncated);
        result["stderr_cap_truncated"] = json!(self.stderr.truncated);
        result["truncated"] =
            json!(self.stdout.truncated || self.stderr.truncated || result["truncated"] == true);
        result["stdout"] = self.stdout.encode(self.stdout_enc);
        result["stderr"] = self.stderr.encode(Enc::Text);
        result
    }
}

#[derive(Default)]
struct CapturedStream {
    bytes: Vec<u8>,
    truncated: bool,
    lookahead: [u8; 3],
    lookahead_len: usize,
}

impl CapturedStream {
    fn push(&mut self, chunk: &[u8], cap: usize, enc: Enc) {
        let retained = chunk.len().min(cap - self.bytes.len());
        let (prefix, omitted) = chunk.split_at(retained);
        self.bytes.extend_from_slice(prefix);
        self.truncated |= !omitted.is_empty();
        if enc == Enc::Text && !omitted.is_empty() {
            let room = &mut self.lookahead[self.lookahead_len..];
            let len = omitted.len().min(room.len());
            room[..len].copy_from_slice(&omitted[..len]);
            self.lookahead_len += len;
        }
    }

    fn encode(mut self, enc: Enc) -> Value {
        if enc == Enc::Text && self.truncated {
            // Trim only a valid character split by the cap. The first omitted
            // bytes distinguish that case from malformed or incomplete input.
            let end = utf8_cut(&self.bytes);
            let suffix = &self.bytes[end..];
            if !suffix.is_empty() {
                let mut boundary = [0; 4];
                let len = suffix.len() + self.lookahead_len.min(4 - suffix.len());
                boundary[..suffix.len()].copy_from_slice(suffix);
                boundary[suffix.len()..len].copy_from_slice(&self.lookahead[..len - suffix.len()]);
                let valid = std::str::from_utf8(&boundary[..len])
                    .map_or_else(|error| error.valid_up_to(), str::len);
                if valid > suffix.len() {
                    self.bytes.truncate(end);
                }
            }
        }
        encode(&self.bytes, enc)
    }
}

#[derive(Clone, Copy)]
enum Channel {
    Stdout,
    Stderr,
}
impl Channel {
    fn index(self) -> usize {
        match self {
            Self::Stdout => 0,
            Self::Stderr => 1,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}
struct Chunk {
    channel: Channel,
    bytes: Vec<u8>,
}
#[derive(Clone)]
struct Completion {
    code: i32,
    signal: Option<i32>,
    timed_out: bool,
    killed: bool,
}
#[derive(Default)]
struct Output {
    chunks: VecDeque<Chunk>,
    queued: [usize; 2],
    readers: usize,
    drain: bool,
    stop: bool,
    truncated: bool,
    io_error: Option<String>,
    result: Option<Result<Completion, String>>,
}
#[derive(Default)]
struct Shared {
    output: Mutex<Output>,
    changed: Condvar,
}

pub(super) struct Process {
    shared: Arc<Shared>,
    supervisor: Mutex<Option<JoinHandle<()>>>,
    cancel: Arc<AtomicBool>,
    enc: Enc,
}
impl Process {
    pub(super) fn start(
        mut command: Command,
        stdin: Option<Vec<u8>>,
        timeout: Option<Duration>,
        cancel: Arc<AtomicBool>,
        enc: Enc,
    ) -> Result<Self, String> {
        platform::prepare(&mut command)?;
        let child = command.spawn().map_err(|e| e.to_string())?;
        let mut owned = ChildOwner {
            child,
            group: None,
            terminated: false,
        };
        owned.group = Some(platform::Group::attach(&owned.child)?);
        let shared = Arc::new(Shared::default());
        let watched = shared.clone();
        let cancelled = cancel.clone();
        let deadline = timeout.map(|duration| Instant::now() + duration);
        let supervisor = std::thread::Builder::new()
            .name("formless-process".into())
            .spawn(move || supervise(owned, stdin, watched, cancelled, deadline, enc))
            .map_err(|e| format!("start process supervisor: {e}"))?;
        Ok(Self {
            shared,
            supervisor: Mutex::new(Some(supervisor)),
            cancel,
            enc,
        })
    }

    pub(super) fn read(&self, size: usize) -> Result<Value, String> {
        match self.read_bytes(size)? {
            Some((stdout, bytes)) => Ok(json!({"stream": if stdout { "stdout" } else { "stderr" },
                "data": encode(&bytes, if stdout { self.enc } else { Enc::Text }), "eof": false})),
            None => Ok(json!({"stream": null, "data": "", "eof": true})),
        }
    }

    pub(super) fn read_bytes(&self, size: usize) -> Result<Option<(bool, Vec<u8>)>, String> {
        let mut output = self.shared.output.locked();
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                return Err("interrupted".into());
            }
            if let Some(mut chunk) = output.chunks.pop_front() {
                let enc = if matches!(chunk.channel, Channel::Stdout) {
                    self.enc
                } else {
                    Enc::Text
                };
                let mut cut = size.min(chunk.bytes.len());
                if enc == Enc::Text && cut < chunk.bytes.len() {
                    cut = utf8_cut(&chunk.bytes[..cut]).max(1);
                }
                let rest = chunk.bytes.split_off(cut);
                let channel = chunk.channel;
                output.queued[channel.index()] -= chunk.bytes.len();
                if !rest.is_empty() {
                    output.chunks.push_front(Chunk {
                        channel,
                        bytes: rest,
                    });
                }
                self.shared.changed.notify_all();
                return Ok(Some((matches!(channel, Channel::Stdout), chunk.bytes)));
            }
            if let Some(error) = &output.io_error {
                return Err(error.clone());
            }
            if output.readers == 0 && output.result.is_some() {
                if let Some(Err(error)) = &output.result {
                    return Err(error.clone());
                }
                return Ok(None);
            }
            output = self
                .shared
                .changed
                .wait_timeout(output, Duration::from_millis(20))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    pub(super) fn request_stop(&self) {
        let mut output = self.shared.output.locked();
        output.stop = true;
        output.drain = true;
        output.truncated |= !output.chunks.is_empty();
        output.chunks.clear();
        output.queued = [0; 2];
        self.shared.changed.notify_all();
    }

    /// Waiting consumes unread chunks. This prevents a caller that waits
    /// without reading from deadlocking against its own backpressure.
    pub(super) fn wait(&self) -> Result<Value, String> {
        {
            let mut output = self.shared.output.locked();
            output.drain = true;
            output.truncated |= !output.chunks.is_empty();
            output.chunks.clear();
            output.queued = [0; 2];
            self.shared.changed.notify_all();
        }
        if let Some(thread) = self.supervisor.locked().take() {
            thread.join().map_err(|_| "process supervisor failed")?;
        }
        let output = self.shared.output.locked();
        if let Some(error) = &output.io_error {
            return Err(error.clone());
        }
        let result = output
            .result
            .as_ref()
            .ok_or("process supervisor did not finish")?
            .as_ref()
            .map_err(Clone::clone)?;
        Ok(json!({"code": result.code, "signal": result.signal,
            "timed_out": result.timed_out, "killed": result.killed,
            "truncated": output.truncated}))
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        self.request_stop();
        let _ = self.wait();
    }
}

struct ChildOwner {
    child: Child,
    group: Option<platform::Group>,
    terminated: bool,
}
impl ChildOwner {
    fn terminate(&mut self) {
        if !self.terminated {
            self.terminated = true;
            if let Some(group) = &self.group {
                group.kill(&mut self.child);
            } else {
                let _ = self.child.kill();
            }
        }
    }
}
impl Drop for ChildOwner {
    fn drop(&mut self) {
        self.terminate();
        let _ = self.child.wait();
    }
}

fn pipe_reader(
    mut pipe: impl Read + Send + 'static,
    shared: Arc<Shared>,
    channel: Channel,
    enc: Enc,
) -> std::io::Result<JoinHandle<()>> {
    {
        shared.output.locked().readers += 1;
    }
    let failed = shared.clone();
    let result = std::thread::Builder::new()
        .name(format!("formless-{}", channel.name()))
        .spawn(move || {
            let mut carry = Vec::new();
            loop {
                let mut buffer = [0u8; PIPE_CHUNK];
                let n = match pipe.read(&mut buffer) {
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        shared.output.locked().io_error =
                            Some(format!("read process {}: {error}", channel.name()));
                        break;
                    }
                };
                carry.extend_from_slice(&buffer[..n]);
                let cut = if enc == Enc::Text && n != 0 {
                    utf8_cut(&carry)
                } else {
                    carry.len()
                };
                if cut > 0 {
                    let rest = carry.split_off(cut);
                    let bytes = std::mem::replace(&mut carry, rest);
                    let mut output = shared.output.locked();
                    while !output.drain
                        && output.queued[channel.index()] + bytes.len() > QUEUED_PER_STREAM
                    {
                        output = shared
                            .changed
                            .wait(output)
                            .unwrap_or_else(|e| e.into_inner());
                    }
                    if output.drain {
                        output.truncated = true;
                    } else {
                        output.queued[channel.index()] += bytes.len();
                        output.chunks.push_back(Chunk { channel, bytes });
                    }
                    shared.changed.notify_all();
                }
                if n == 0 {
                    break;
                }
            }
            shared.output.locked().readers -= 1;
            shared.changed.notify_all();
        });
    if result.is_err() {
        failed.output.locked().readers -= 1;
    }
    result
}

fn supervise(
    mut owned: ChildOwner,
    stdin: Option<Vec<u8>>,
    shared: Arc<Shared>,
    cancel: Arc<AtomicBool>,
    deadline: Option<Instant>,
    enc: Enc,
) {
    let mut workers = Vec::new();
    let result = (|| -> Result<Completion, String> {
        for (pipe, channel, encoding) in [
            (
                owned
                    .child
                    .stdout
                    .take()
                    .map(|p| Box::new(p) as Box<dyn Read + Send>),
                Channel::Stdout,
                enc,
            ),
            (
                owned
                    .child
                    .stderr
                    .take()
                    .map(|p| Box::new(p) as Box<dyn Read + Send>),
                Channel::Stderr,
                Enc::Text,
            ),
        ] {
            let pipe = pipe.ok_or("process output pipe is missing")?;
            workers.push(
                pipe_reader(pipe, shared.clone(), channel, encoding).map_err(|e| e.to_string())?,
            );
        }
        if let (Some(data), Some(mut pipe)) = (stdin, owned.child.stdin.take()) {
            workers.push(
                std::thread::Builder::new()
                    .name("formless-stdin".into())
                    .spawn(move || {
                        let _ = pipe.write_all(&data);
                    })
                    .map_err(|e| e.to_string())?,
            );
        }
        let (status, timed_out, killed) = loop {
            let stopped = shared.output.locked().stop || cancel.load(Ordering::SeqCst);
            let timed_out = !stopped && deadline.is_some_and(|at| Instant::now() >= at);
            if stopped || timed_out {
                owned.terminate();
                break (
                    owned.child.wait().map_err(|e| e.to_string())?,
                    timed_out,
                    true,
                );
            }
            let exited = match platform::has_exited(&mut owned.child) {
                Ok(exited) => exited,
                Err(error) => {
                    // An external SIGCHLD reaper may have consumed the child.
                    // Never use a released PID/PGID as authority to kill others.
                    #[cfg(unix)]
                    if error.raw_os_error() == Some(libc::ECHILD) {
                        owned.terminated = true;
                    }
                    return Err(error.to_string());
                }
            };
            if exited {
                // Keep an exited Unix leader waitable until the group is killed:
                // reaping first could free its PID/PGID for an unrelated process.
                owned.terminate();
                break (owned.child.wait().map_err(|e| e.to_string())?, false, false);
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        Ok(completion(status, timed_out, killed))
    })();
    if result.is_err() {
        owned.terminate();
        let mut output = shared.output.locked();
        output.drain = true;
        output.truncated |= !output.chunks.is_empty();
        output.chunks.clear();
        output.queued = [0; 2];
        shared.changed.notify_all();
    }
    for worker in workers {
        if worker.join().is_err() {
            shared.output.locked().io_error = Some("process pipe worker failed".into());
        }
    }
    shared.output.locked().result = Some(result);
    shared.changed.notify_all();
}
fn completion(status: ExitStatus, timed_out: bool, killed: bool) -> Completion {
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&status);
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    Completion {
        code: status.code().unwrap_or(-1),
        signal,
        timed_out,
        killed,
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    pub(super) fn prepare(command: &mut Command) -> Result<(), String> {
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        // Read the parent policy without changing a global signal handler.
        // Auto-reaping makes it impossible to retain the leader's PID safely.
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if action.sa_sigaction == libc::SIG_IGN || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
            return Err("process supervision requires waitable children; SIGCHLD auto-reaping is enabled, so no child was started".into());
        }
        std::os::unix::process::CommandExt::process_group(command, 0);
        Ok(())
    }
    pub(super) fn has_exited(child: &mut Child) -> std::io::Result<bool> {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: valid initialized native output, our own still-waitable child,
        // and WNOWAIT deliberately retains its identity until group termination.
        if unsafe {
            libc::waitid(
                libc::P_PID,
                child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { info.si_pid() } != 0)
    }
    pub(super) struct Group(u32);
    impl Group {
        pub(super) fn attach(child: &Child) -> Result<Self, String> {
            Ok(Self(child.id()))
        }
        pub(super) fn kill(&self, child: &mut Child) {
            // SAFETY: this Invocation created the group with its own child PID.
            unsafe {
                libc::killpg(self.0 as libc::pid_t, libc::SIGKILL);
            }
            let _ = child.kill();
        }
    }
}
#[cfg(not(any(unix, windows)))]
mod platform {
    use super::*;
    pub(super) fn prepare(_: &mut Command) -> Result<(), String> {
        Err("supervised processes are unsupported on this platform".into())
    }
    pub(super) fn has_exited(child: &mut Child) -> std::io::Result<bool> {
        child.try_wait().map(|status| status.is_some())
    }
    pub(super) struct Group;
    impl Group {
        pub(super) fn attach(_: &Child) -> Result<Self, String> {
            Err("supervised processes are unsupported on this platform".into())
        }
        pub(super) fn kill(&self, child: &mut Child) {
            let _ = child.kill();
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    };
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD,
                THREADENTRY32,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Threading::{OpenThread, ResumeThread, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME},
        },
    };
    pub(super) fn prepare(command: &mut Command) -> Result<(), String> {
        command.creation_flags(CREATE_SUSPENDED);
        Ok(())
    }
    pub(super) fn has_exited(child: &mut Child) -> std::io::Result<bool> {
        // The private Job handle retains group identity after the process exits.
        child.try_wait().map(|status| status.is_some())
    }
    pub(super) struct Group(OwnedHandle);
    impl Group {
        pub(super) fn attach(child: &Child) -> Result<Self, String> {
            // Start suspended: no descendant can escape before the Job owns it.
            // Nested jobs are supported on Windows 8+ (the supported baseline).
            // https://learn.microsoft.com/windows/win32/procthread/job-objects
            let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if raw.is_null() {
                return Err(std::io::Error::last_os_error().to_string());
            }
            // SAFETY: successful native creation transfers one owned HANDLE.
            let group = Self(unsafe { OwnedHandle::from_raw_handle(raw) });
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: live owned job/child handles and an initialized native
            // structure of exactly the size passed to the native function.
            if unsafe {
                SetInformationJobObject(
                    group.0.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            } == 0
                || unsafe {
                    AssignProcessToJobObject(group.0.as_raw_handle(), child.as_raw_handle())
                } == 0
            {
                return Err(format!(
                    "isolate child process: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // std::process preserves Windows argument quoting and environment
            // handling but does not expose the primary thread handle. The
            // suspended child has not run; enumerate its thread and resume only
            // after assignment. If isolation/resumption fails, ChildOwner kills
            // and reaps the still-owned child rather than running uncontained.
            let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
            if raw == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error().to_string());
            }
            let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
            let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
            entry.dwSize = std::mem::size_of_val(&entry) as u32;
            let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } != 0;
            while more {
                if entry.th32OwnerProcessID == child.id() {
                    let raw = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                    if raw.is_null() {
                        return Err(std::io::Error::last_os_error().to_string());
                    }
                    let thread = unsafe { OwnedHandle::from_raw_handle(raw) };
                    if unsafe { ResumeThread(thread.as_raw_handle()) } != 1 {
                        return Err("cannot resume the isolated child thread".into());
                    }
                    return Ok(group);
                }
                more = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } != 0;
            }
            Err("cannot find the suspended child thread".into())
        }
        pub(super) fn kill(&self, child: &mut Child) {
            // SAFETY: a private owned Job handle; all processes in this Job
            // belong to this Invocation, with breakaway flags left disabled.
            unsafe {
                TerminateJobObject(self.0.as_raw_handle(), 1);
            }
            let _ = child.kill();
        }
    }
}

#[cfg(test)]
mod run_output_tests {
    use super::*;

    #[test]
    fn text_cap_keeps_the_prefix_across_chunks() {
        for character in ['¢', '€', '🙂'] {
            let text = format!("a{character}");
            for char_bytes_kept in 1..character.len_utf8() {
                for split in 1..text.len() {
                    let mut output = RunOutput::new(1 + char_bytes_kept, Enc::Text);
                    let (first, rest) = text.as_bytes().split_at(split);
                    output.push(true, first);
                    output.push(false, b"ok");
                    output.push(true, rest);
                    output.push(true, b"x");

                    let result = output.finish(json!({"truncated": false}));
                    assert_eq!(
                        result,
                        json!({
                            "stdout": "a",
                            "stderr": "ok",
                            "stdout_cap_truncated": true,
                            "stderr_cap_truncated": false,
                            "truncated": true
                        }),
                        "{character:?}, cap after {char_bytes_kept} character bytes, split {split}"
                    );
                }
            }
        }
    }

    #[test]
    fn binary_stdout_keeps_the_exact_byte_prefix_and_stderr_stays_text() {
        let mut output = RunOutput::new(2, Enc::Base64);
        output.push(true, b"\xc2");
        output.push(false, "a€".as_bytes());
        output.push(true, b"\xa2\xff");
        output.push(false, b"b");
        let result = output.finish(json!({"truncated": false}));
        assert_eq!(result["stdout"], "wqI=");
        assert_eq!(result["stderr"], "a");
        assert_eq!(result["stdout_cap_truncated"], true);
        assert_eq!(result["stderr_cap_truncated"], true);
        assert_eq!(result["truncated"], true);

        let mut output = RunOutput::new(1, Enc::Base64);
        output.push(true, b"\xc2");
        output.push(false, "€".as_bytes());
        let result = output.finish(json!({"truncated": false}));
        assert_eq!(result["stdout"], "wg==");
        assert_eq!(result["stderr"], "");
        assert_eq!(result["stdout_cap_truncated"], false);
        assert_eq!(result["stderr_cap_truncated"], true);
        assert_eq!(result["truncated"], true);

        let mut output = RunOutput::new(1, Enc::Base64);
        output.push(true, "¢".as_bytes());
        let result = output.finish(json!({"truncated": false}));
        assert_eq!(result["stdout"], "wg==");
        assert_eq!(result["stdout_cap_truncated"], true);
        assert_eq!(result["stderr_cap_truncated"], false);
    }

    #[test]
    fn malformed_and_incomplete_input_remains_lossy() {
        let cases: &[(&[u8], usize, &str, bool)] = &[
            (b"a\xffb", 2, "a�", true),
            (b"a\xe2\x82x", 3, "a�", true),
            (b"a\xed\xa0\x80", 3, "a��", true),
            (b"a\xe2\x82", 2, "a�", true),
            (b"a\xe2\x82", 3, "a�", false),
            (b"\xff\xf0\x9f\x98\x80", 3, "�", true),
        ];
        for &(bytes, cap, expected, truncated) in cases {
            let mut output = RunOutput::new(cap, Enc::Text);
            for byte in bytes {
                output.push(false, std::slice::from_ref(byte));
            }
            assert_eq!(output.stderr.bytes.len(), cap.min(bytes.len()));
            let result = output.finish(json!({"truncated": false}));
            assert_eq!(result["stderr"], expected, "{bytes:?} at cap {cap}");
            assert_eq!(result["stderr_cap_truncated"], truncated);
            assert_eq!(result["truncated"], truncated);
        }
    }

    #[test]
    fn empty_and_exact_cap_output_and_upstream_truncation() {
        for (cap, bytes) in [(0, b"".as_slice()), (2, "é".as_bytes())] {
            for upstream in [false, true] {
                let mut output = RunOutput::new(cap, Enc::Text);
                output.push(true, bytes);
                output.push(false, b"");
                let result = output.finish(json!({"code": 42, "truncated": upstream}));
                assert_eq!(result["stdout"], std::str::from_utf8(bytes).unwrap());
                assert_eq!(result["stderr"], "");
                assert_eq!(result["stdout_cap_truncated"], false);
                assert_eq!(result["stderr_cap_truncated"], false);
                assert_eq!(result["truncated"], upstream);
                assert_eq!(result["code"], 42);
            }
        }

        let mut output = RunOutput::new(0, Enc::Text);
        output.push(true, b"x");
        let result = output.finish(json!({"truncated": false}));
        assert_eq!(result["stdout"], "");
        assert_eq!(result["stdout_cap_truncated"], true);
        assert_eq!(result["stderr_cap_truncated"], false);
        assert_eq!(result["truncated"], true);
    }
}
