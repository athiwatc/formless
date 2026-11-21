//! Isolate descendants before any user command can run, and retain authority
//! over the process group until the direct child has been collected.

use std::{
    io,
    process::{Child, Command, ExitStatus},
};
use tokio::process::{Child as AsyncChild, ChildStderr, ChildStdin, ChildStdout};

pub(crate) use platform::{has_exited, prepare, Group};

pub(crate) fn attach(child: &Child) -> Result<Group, String> {
    #[cfg(unix)]
    return Ok(Group(child.id()));
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        return Group::attach(child.id(), child.as_raw_handle());
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = child;
        Err("supervised processes are unsupported on this platform".into())
    }
}

/// An asynchronous child whose whole process group belongs to the caller.
/// Dropping it terminates the group, including on cancellation.
pub struct SupervisedChild {
    child: AsyncChild,
    group: Option<Group>,
}

impl SupervisedChild {
    pub fn spawn(mut command: tokio::process::Command) -> Result<Self, String> {
        prepare(command.as_std_mut())?;
        // On Windows, drop the direct child on an assignment failure too.
        // Unix termination below first verifies that the PID is still waitable.
        command.kill_on_drop(cfg!(windows));
        let child = command.spawn().map_err(|error| error.to_string())?;
        let pid = child.id().ok_or("isolated child has no process ID")?;
        #[cfg(unix)]
        let group = Some(Group(pid));
        #[cfg(windows)]
        let group = Some(Group::attach(
            pid,
            child
                .raw_handle()
                .ok_or("isolated child has no process handle")?,
        )?);
        #[cfg(not(any(unix, windows)))]
        let group = None;
        Ok(Self { child, group })
    }

    pub fn take_pipes(&mut self) -> (Option<ChildStdin>, Option<ChildStdout>, Option<ChildStderr>) {
        (
            self.child.stdin.take(),
            self.child.stdout.take(),
            self.child.stderr.take(),
        )
    }

    /// Wait for the direct child without releasing the Unix process group ID.
    pub async fn exited(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            let pid = self
                .child
                .id()
                .expect("the supervised child has not been reaped");
            while !platform::leader_exited(pid)? {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Ok(())
        }
        #[cfg(not(unix))]
        self.child.wait().await.map(|_| ())
    }

    /// Terminate leftover descendants and collect the child's actual status.
    pub async fn finish(mut self) -> io::Result<ExitStatus> {
        self.terminate();
        self.child.wait().await
    }

    fn terminate(&mut self) {
        if let Some(group) = self.group.take() {
            #[cfg(unix)]
            if platform::leader_exited(group.0).is_err() {
                return;
            }
            group.kill();
            let _ = self.child.start_kill();
        }
    }
}
impl Drop for SupervisedChild {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// A blocking child whose whole process group belongs to the caller.
/// Dropping it terminates the group, then collects the direct child.
pub struct SupervisedProcess {
    child: Child,
    group: Option<Group>,
}

impl SupervisedProcess {
    pub fn spawn(mut command: Command) -> Result<Self, String> {
        prepare(&mut command)?;
        let mut child = command.spawn().map_err(|error| error.to_string())?;
        match attach(&child) {
            Ok(group) => Ok(Self {
                child,
                group: Some(group),
            }),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
    }

    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Drop for SupervisedProcess {
    fn drop(&mut self) {
        if let Some(group) = self.group.take() {
            #[cfg(unix)]
            let live = platform::leader_exited(group.0).is_ok();
            #[cfg(not(unix))]
            let live = true;
            if live {
                group.kill();
                let _ = self.child.kill();
            }
        }
        let _ = self.child.wait();
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    pub(crate) fn prepare(command: &mut Command) -> Result<(), String> {
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
    pub(crate) fn has_exited(child: &mut Child) -> std::io::Result<bool> {
        leader_exited(child.id())
    }
    pub(crate) fn leader_exited(pid: u32) -> std::io::Result<bool> {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: valid initialized native output, our own still-waitable child,
        // and WNOWAIT deliberately retains its identity until group termination.
        if unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(unsafe { info.si_pid() } != 0)
    }
    pub(crate) struct Group(pub(crate) u32);
    impl Group {
        pub(crate) fn kill(&self) {
            // SAFETY: this Invocation created the group with its own child PID.
            unsafe {
                libc::killpg(self.0 as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}
#[cfg(not(any(unix, windows)))]
mod platform {
    use super::*;
    pub(crate) fn prepare(_: &mut Command) -> Result<(), String> {
        Err("supervised processes are unsupported on this platform".into())
    }
    pub(crate) fn has_exited(child: &mut Child) -> std::io::Result<bool> {
        child.try_wait().map(|status| status.is_some())
    }
    pub(crate) struct Group;
    impl Group {
        pub(crate) fn kill(&self) {}
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
        Foundation::{HANDLE, INVALID_HANDLE_VALUE},
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
    pub(crate) fn prepare(command: &mut Command) -> Result<(), String> {
        command.creation_flags(CREATE_SUSPENDED);
        Ok(())
    }
    pub(crate) fn has_exited(child: &mut Child) -> std::io::Result<bool> {
        // The private Job handle retains group identity after the process exits.
        child.try_wait().map(|status| status.is_some())
    }
    pub(crate) struct Group(OwnedHandle);
    impl Group {
        pub(crate) fn attach(pid: u32, child_handle: HANDLE) -> Result<Self, String> {
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
                || unsafe { AssignProcessToJobObject(group.0.as_raw_handle(), child_handle) } == 0
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
                if entry.th32OwnerProcessID == pid {
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
        pub(crate) fn kill(&self) {
            // SAFETY: a private owned Job handle; all processes in this Job
            // belong to this Invocation, with breakaway flags left disabled.
            unsafe {
                TerminateJobObject(self.0.as_raw_handle(), 1);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    #[test]
    fn dropping_a_supervised_process_kills_its_descendants() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 & echo $!; wait"])
            .stdout(std::process::Stdio::piped());
        let mut process = SupervisedProcess::spawn(command).unwrap();
        let mut line = String::new();
        BufReader::new(process.child_mut().stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let descendant: libc::pid_t = line.trim().parse().unwrap();
        assert_eq!(
            unsafe { libc::kill(descendant, 0) },
            0,
            "descendant not started"
        );
        drop(process);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let alive = |pid: libc::pid_t| {
            (unsafe { libc::kill(pid, 0) }) == 0
                && std::fs::read_to_string(format!("/proc/{pid}/stat"))
                    .map_or(true, |stat| !stat.contains(") Z "))
        };
        while alive(descendant) {
            assert!(std::time::Instant::now() < deadline, "descendant survived");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}
