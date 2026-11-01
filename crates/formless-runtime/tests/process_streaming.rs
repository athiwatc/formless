//! Actual private children, never a shell, OS manager, or external program.
use formless_runtime::{
    execute, Capabilities, ExecErrorKind, ExecRequest, HostMode, HostOps, NoHostOps,
};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

const HELPER_ARGS: &[&str] = &[
    "--exact",
    "process_fixture_helper",
    "--ignored",
    "--nocapture",
];
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("fl-process612-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }
    fn input(&self, mode: &str) -> Value {
        json!({"cmd": std::env::current_exe().unwrap(), "args": HELPER_ARGS,
            "cwd": self.0, "env": {"FL_FIXTURE_MODE": mode,
            "FL_PID_FILE": self.0.join("parent.pid"), "FL_CHILD_PID_FILE": self.0.join("child.pid"),
            "FL_ACK_FILE": self.0.join("ack")}, "ack": self.0.join("ack")})
    }
    fn request(&self, code: &str, mode: &str) -> ExecRequest {
        ExecRequest {
            code: code.into(),
            input: self.input(mode),
            output_schema: None,
            caps: Capabilities::parse(&[
                format!("process:{}", std::env::current_exe().unwrap().display()),
                format!("fs:read:{}", self.0.display()),
                format!("fs:write:{}", self.0.display()),
                "env:FORMLESS_API_KEY".into(),
            ])
            .unwrap(),
            memory_mb: 128,
            mode: HostMode::Live,
            cancel: Arc::new(AtomicBool::new(false)),
            overlay: None,
            activity: None,
            modules: Arc::new(Vec::new()),
        }
    }
    fn pid(&self, file: &str) -> u32 {
        std::fs::read_to_string(self.0.join(file))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn wait_file(file: &Path) {
    let end = Instant::now() + Duration::from_secs(5);
    while !file.is_file() {
        assert!(
            Instant::now() < end,
            "private process fixture did not become ready"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn hold() {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "private subprocess helper; invoked explicitly by the parent tests"]
fn process_fixture_helper() {
    let Ok(mode) = std::env::var("FL_FIXTURE_MODE") else {
        return;
    };
    if let Some(file) = std::env::var_os("FL_PID_FILE") {
        std::fs::write(file, std::process::id().to_string()).unwrap();
    }
    match mode.as_str() {
        "handshake" => {
            print!("UTF8:A🦀B\n");
            std::io::stdout().flush().unwrap();
            eprint!("ERR:é🧭\n");
            std::io::stderr().flush().unwrap();
            wait_file(Path::new(&std::env::var_os("FL_ACK_FILE").unwrap()));
            println!("AFTER_ACK");
            std::io::stdout().flush().unwrap();
            std::process::exit(7);
        }
        "duplex" => {
            std::io::stdout()
                .write_all(&vec![b'Z'; 1024 * 1024])
                .unwrap();
            std::io::stdout().flush().unwrap();
            std::io::stderr()
                .write_all(&vec![b'y'; 1024 * 1024])
                .unwrap();
            std::io::stderr().flush().unwrap();
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
            println!("\nSTDIN_BYTES={}", input.len());
            std::io::stdout().flush().unwrap();
        }
        "tree" | "exit_tree" => {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(HELPER_ARGS)
                .env("FL_FIXTURE_MODE", "hold")
                .env(
                    "FL_PID_FILE",
                    std::env::var_os("FL_CHILD_PID_FILE").unwrap(),
                )
                .spawn()
                .unwrap();
            wait_file(Path::new(&std::env::var_os("FL_CHILD_PID_FILE").unwrap()));
            println!("TREE_READY");
            std::io::stdout().flush().unwrap();
            if mode == "exit_tree" {
                std::process::exit(0);
            }
            hold();
            let _ = child.kill();
            let _ = child.wait();
        }
        "hold" => {
            println!("READY");
            std::io::stdout().flush().unwrap();
            hold();
        }
        "environment" => {
            let private_present = std::env::vars_os().any(|(k, _)| {
                k.to_str()
                    .is_some_and(|k| k.to_ascii_uppercase().starts_with("FORMLESS_"))
            });
            println!("PRIVATE_PRESENT={private_present}");
            println!(
                "EXPLICIT={}",
                std::env::var("FL_EXPLICIT").unwrap_or_default()
            );
        }
        _ => panic!("unknown private fixture mode"),
    }
    std::process::exit(0);
}

#[cfg(unix)]
fn is_running(pid: u32) -> bool {
    // A killed orphan can await its host init's reap; it cannot run or keep
    // pipes open. The runtime's own direct children are checked separately.
    #[cfg(target_os = "linux")]
    if std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|text| {
            text.rsplit_once(") ")
                .map(|(_, rest)| rest.starts_with('Z'))
        })
        == Some(true)
    {
        return false;
    }
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}
#[cfg(windows)]
fn is_running(pid: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE,
        },
    };
    let process = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    if process.is_null() {
        return false;
    }
    let running = unsafe { WaitForSingleObject(process, 0) } == 258;
    unsafe {
        CloseHandle(process);
    }
    running
}
#[cfg(not(any(unix, windows)))]
fn is_running(_: u32) -> bool {
    false
}
fn assert_reaped(pid: u32) {
    assert!(!is_running(pid), "owned fixture child is still running");
    #[cfg(unix)]
    {
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) },
            -1,
            "the runtime left a direct child for its caller to reap"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}

#[test]
fn spawn_delivers_distinct_utf8_chunks_before_exit_and_wait_is_idempotent() {
    let f = Fixture::new();
    let req = f.request(r#"function run(i) {
      const p = process.spawn(i.cmd, i.args, {cwd:i.cwd, env:i.env, timeout_ms:10000});
      let out='', err='', chunks=[];
      while (!out.includes('UTF8:A🦀B\n') || !err.includes('ERR:é🧭\n')) {
        const c=p.read(4); if(c.eof) throw Error('missing live output');
        if(c.stream==='stdout') out+=c.data; else if(c.stream==='stderr') err+=c.data; else throw Error('no stream');
        chunks.push(c.data);
      }
      fs.write(i.ack, 'observed before completion');
      while(true) { const c=p.read(4); if(c.eof) break; if(c.stream==='stdout')out+=c.data; else err+=c.data; chunks.push(c.data); }
      const status=p.wait();
      return {out,err,status,again:p.wait(),eof:p.read(),replacement:chunks.some(x=>x.includes('\ufffd'))};
    }"#, "handshake");
    let v = execute(&req, Arc::new(NoHostOps)).unwrap().output;
    assert!(v["out"].as_str().unwrap().contains("UTF8:A🦀B\nAFTER_ACK"));
    assert!(v["err"].as_str().unwrap().contains("ERR:é🧭"));
    assert_eq!(v["replacement"], false);
    assert_eq!(v["status"]["code"], 7);
    assert_eq!(v["status"]["timed_out"], false);
    assert_eq!(v["status"]["truncated"], false);
    assert_eq!(v["again"], v["status"]);
    assert_eq!(v["eof"]["eof"], true);
    assert_reaped(f.pid("parent.pid"));
}

#[test]
fn both_pipes_and_stdin_progress_and_wait_without_reads_cannot_deadlock() {
    let f = Fixture::new();
    let req = f.request(r#"function run(i) {
      const opts={cwd:i.cwd,env:i.env,stdin:'s'.repeat(1048576),timeout_ms:10000};
      const run=process.run(i.cmd,i.args,opts);
      const p=process.spawn(i.cmd,i.args,opts);
      let out='',err='';
      while(true) {const c=p.read();if(c.eof)break;if(c.stream==='stdout')out+=c.data;else err+=c.data;}
      const streamed=p.wait();
      const unread=process.spawn(i.cmd,i.args,opts).wait();
      return {run:{code:run.code,z:run.stdout.split('Z').length-1,y:run.stderr.split('y').length-1,stdin:run.stdout.includes('STDIN_BYTES=1048576')},
        streamed,z:out.split('Z').length-1,y:err.split('y').length-1,stdin:out.includes('STDIN_BYTES=1048576'),unread};
    }"#, "duplex");
    let v = execute(&req, Arc::new(NoHostOps)).unwrap().output;
    assert_eq!(v["run"]["code"], 0);
    for result in [&v, &v["run"]] {
        assert_eq!(result["z"], 1048576);
        assert_eq!(result["y"], 1048576);
        assert_eq!(result["stdin"], true);
    }
    assert_eq!(v["streamed"]["truncated"], false);
    assert_eq!(v["unread"]["code"], 0);
    assert_eq!(v["unread"]["truncated"], true);
    assert_reaped(f.pid("parent.pid"));
}

#[test]
fn run_and_spawn_deadlines_terminate_the_group_and_preserve_timeout_status() {
    for api in ["run", "spawn"] {
        let f = Fixture::new();
        let call = if api == "run" {
            "process.run(i.cmd,i.args,o)"
        } else {
            "process.spawn(i.cmd,i.args,o).wait()"
        };
        let code = format!(
            "function run(i) {{const o={{cwd:i.cwd,env:i.env,timeout_ms:1000}};return {call};}}"
        );
        let started = Instant::now();
        let value = execute(&f.request(&code, "tree"), Arc::new(NoHostOps))
            .unwrap()
            .output;
        assert_eq!(value["timed_out"], true);
        assert_eq!(value["killed"], true);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "deadline did not bound fixture runtime"
        );
        assert_reaped(f.pid("parent.pid"));
        assert!(
            !is_running(f.pid("child.pid")),
            "a timed-out group member survived"
        );
    }
}

#[test]
fn normal_return_js_error_and_explicit_kill_reap_abandoned_handles() {
    for ending in [
        "return 17;",
        "throw Error('expected private fixture failure');",
        "return p.kill();",
    ] {
        let f = Fixture::new();
        let code = format!(
            r#"function run(i) {{
          const p=process.spawn(i.cmd,i.args,{{cwd:i.cwd,env:i.env}});
          let seen='';while(!seen.includes('READY')){{const c=p.read();if(c.eof)throw Error('not ready');seen+=c.data;}}
          {ending}
        }}"#
        );
        let result = execute(&f.request(&code, "hold"), Arc::new(NoHostOps));
        if ending.starts_with("throw") {
            assert!(result.is_err());
        } else if ending.contains("kill") {
            assert_eq!(result.unwrap().output["killed"], true);
        } else {
            assert_eq!(result.unwrap().output, 17);
        }
        assert_reaped(f.pid("parent.pid"));
    }
}
struct Emit(mpsc::Sender<String>);
impl HostOps for Emit {
    fn emit(&self, chunk: &str) -> Result<(), String> {
        self.0.send(chunk.into()).map_err(|e| e.to_string())
    }
}
#[test]
fn cancellation_interrupts_read_and_reaps_its_child() {
    let f = Fixture::new();
    let req = f.request(
        r#"function run(i){const p=process.spawn(i.cmd,i.args,{cwd:i.cwd,env:i.env});let s='';
      while(!s.includes('READY')){s+=p.read().data;}emit('ready');while(true)p.read();}"#,
        "hold",
    );
    let cancel = req.cancel.clone();
    let (tx, rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let _ = done_tx.send(execute(&req, Arc::new(Emit(tx))));
    });
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "ready");
    cancel.store(true, Ordering::SeqCst);
    let result = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    thread.join().unwrap();
    assert_eq!(result.unwrap_err().kind, ExecErrorKind::Cancelled);
    assert_reaped(f.pid("parent.pid"));
}

#[test]
fn handles_are_invocation_local_even_while_the_owner_is_running() {
    let f = Fixture::new();
    let req=f.request(r#"function run(i){const h=__call('process_spawn',{cmd:i.cmd,args:i.args,cwd:i.cwd,env:i.env});
      let s='';while(!s.includes('UTF8:A🦀B\n'))s+=__call('process_read',{id:h.id}).data;
      emit(String(h.id));while(!__call('process_read',{id:h.id}).eof){}return __call('process_wait',{id:h.id});}"#,"handshake");
    let (tx, rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let _ = done_tx.send(execute(&req, Arc::new(Emit(tx))));
    });
    let id: u64 = rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .parse()
        .unwrap();
    let mut foreign = f.request(
        "function run(i){return __call('process_kill',{id:i.id});}",
        "hold",
    );
    foreign.input["id"] = json!(id);
    let error = execute(&foreign, Arc::new(NoHostOps)).unwrap_err();
    assert!(error.message.contains("unknown to this Invocation"));
    assert!(
        is_running(f.pid("parent.pid")),
        "foreign Invocation killed its sibling's child"
    );
    std::fs::write(f.0.join("ack"), "owner may finish").unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    thread.join().unwrap();
    assert_eq!(result.output["code"], 7);
    assert_eq!(result.output["killed"], false);
    assert_reaped(f.pid("parent.pid"));
}

#[test]
fn environment_scopes_and_non_live_modes_match_run() {
    let f = Fixture::new();
    let mut req=f.request(r#"function run(i){i.env.FL_EXPLICIT='allowed';const p=process.spawn(i.cmd,i.args,{cwd:i.cwd,env:i.env});let out='';
      while(true){const c=p.read();if(c.eof)break;out+=c.data;}return {out,status:p.wait()};}"#,"environment");
    let value = execute(&req, Arc::new(NoHostOps)).unwrap().output;
    assert!(value["out"]
        .as_str()
        .unwrap()
        .contains("PRIVATE_PRESENT=false"));
    assert!(value["out"].as_str().unwrap().contains("EXPLICIT=allowed"));
    req.input["env"]["PATH"] = json!("unapproved-loader-path");
    let error = execute(&req, Arc::new(NoHostOps)).unwrap_err();
    assert_eq!(error.kind, ExecErrorKind::Denied);
    assert!(error.message.contains("env:PATH"));
    req.input["env"].as_object_mut().unwrap().remove("PATH");
    req.code="function run(i){const p=process.spawn(i.cmd,i.args,{cwd:i.cwd,env:i.env});return {chunk:p.read(),done:p.wait()};}".into();
    for mode in [HostMode::Smoke, HostMode::DryRun] {
        req.mode = mode;
        let v = execute(&req, Arc::new(NoHostOps)).unwrap().output;
        assert_eq!(v["chunk"]["eof"], true);
        assert_eq!(v["done"]["code"], 0);
    }
    req.mode = HostMode::Replay;
    assert_eq!(
        execute(&req, Arc::new(NoHostOps)).unwrap_err().kind,
        ExecErrorKind::Denied
    );
}

#[test]
fn normal_leader_exit_closes_background_group_pipes_before_reaping() {
    let f = Fixture::new();
    let value=execute(&f.request(r#"function run(i){const p=process.spawn(i.cmd,i.args,{cwd:i.cwd,env:i.env,timeout_ms:5000});
      let out='';while(true){const c=p.read();if(c.eof)break;out+=c.data;}return {out,status:p.wait()};}"#,"exit_tree"),Arc::new(NoHostOps)).unwrap().output;
    assert!(value["out"].as_str().unwrap().contains("TREE_READY"));
    assert_eq!(value["status"]["code"], 0);
    assert_eq!(value["status"]["timed_out"], false);
    assert_reaped(f.pid("parent.pid"));
    assert!(
        !is_running(f.pid("child.pid")),
        "an exited leader left a running background group member"
    );
}

#[test]
fn deadline_still_fires_when_javascript_does_not_drain_full_pipes() {
    let f = Fixture::new();
    let req=f.request(r#"function run(i){const p=process.spawn(i.cmd,i.args,{cwd:i.cwd,env:i.env,timeout_ms:100});
      sleep(500);let n=0;while(true){const c=p.read();if(c.eof)break;n+=c.data.length;}return {n,status:p.wait()};}"#,"duplex");
    let value = execute(&req, Arc::new(NoHostOps)).unwrap().output;
    assert_eq!(value["status"]["timed_out"], true);
    assert!(value["n"].as_u64().unwrap() > 0);
    assert!(
        value["n"].as_u64().unwrap() < 1048576,
        "unread stdout was not backpressured"
    );
    assert_reaped(f.pid("parent.pid"));
}

#[test]
fn open_handle_limit_fails_before_launch_and_kill_releases_the_slot() {
    let f = Fixture::new();
    let req=f.request(r#"function run(i){const options={cwd:i.cwd,env:i.env,timeout_ms:10000};let ps=[];
      for(let n=0;n<16;n++)ps.push(process.spawn(i.cmd,i.args,options));
      let denied=false;try{process.spawn(i.cmd,i.args,options);}catch(e){denied=String(e).includes('resource limit');}
      ps.shift().kill();const replacement=process.spawn(i.cmd,i.args,options);replacement.kill();
      for(const p of ps)p.kill();return {denied,reused:true};}"#,"hold");
    let value = execute(&req, Arc::new(NoHostOps)).unwrap().output;
    assert_eq!(value, json!({"denied":true,"reused":true}));
}
