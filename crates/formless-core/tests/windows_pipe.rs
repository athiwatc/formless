//! Windows named pipe: owner-only security, and a client can talk to the
//! daemon over it (SPEC §2.3/§14).
#![cfg(windows)]

use formless_core::Paths;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[tokio::test]
async fn pipe_dacl_allows_only_current_user_and_system() {
    check_pipe_dacl(false).await;
}

#[tokio::test]
#[should_panic(expected = "expected user and SYSTEM allow ACEs")]
async fn pipe_dacl_oracle_rejects_an_empty_dacl() {
    check_pipe_dacl(true).await;
}

async fn check_pipe_dacl(empty_dacl: bool) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
    use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows_sys::Win32::Security::{
        CreateWellKnownSid, EqualSid, GetAce, GetSecurityDescriptorDacl, GetTokenInformation,
        InitializeAcl, TokenUser, WinLocalSystemSid, ACCESS_ALLOWED_ACE, ACL_REVISION,
        DACL_SECURITY_INFORMATION, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let sa = formless_core::win_pipe::OwnerOnly::new().unwrap();
    if empty_dacl {
        // SAFETY: the descriptor is owned by `sa`; reset its existing ACL buffer
        // to a valid empty DACL to verify the oracle rejects a real regression.
        unsafe {
            let attrs = &*sa.as_raw().cast::<SECURITY_ATTRIBUTES>();
            let mut present = 0;
            let mut defaulted = 0;
            let mut acl = std::ptr::null_mut();
            assert_ne!(
                GetSecurityDescriptorDacl(
                    attrs.lpSecurityDescriptor,
                    &mut present,
                    &mut acl,
                    &mut defaulted
                ),
                0
            );
            assert_ne!(present, 0);
            assert!(!acl.is_null());
            assert_ne!(InitializeAcl(acl, (*acl).AclSize.into(), ACL_REVISION), 0);
        }
    }
    let name = format!(r"\\.\pipe\fl-acl-{}", uuid::Uuid::new_v4());
    // SAFETY: the security attributes remain live until after pipe creation.
    let pipe = unsafe {
        tokio::net::windows::named_pipe::ServerOptions::new()
            .create_with_security_attributes_raw(name, sa.as_raw())
    }
    .unwrap();
    // SAFETY: Win32 fills the descriptor and token buffers below. ACE pointers
    // borrow the descriptor, which is freed only after all SID comparisons.
    unsafe {
        let mut sd = std::ptr::null_mut();
        let mut acl = std::ptr::null_mut();
        assert_eq!(
            GetSecurityInfo(
                pipe.as_raw_handle(),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut acl,
                std::ptr::null_mut(),
                &mut sd,
            ),
            0
        );
        assert!(!acl.is_null(), "a null DACL grants everyone access");
        let mut token = std::ptr::null_mut();
        assert_ne!(
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token),
            0
        );
        let mut len = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
        // u64 storage guarantees alignment for TOKEN_USER.
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
        CloseHandle(token);
        assert_ne!(ok, 0);
        let user = &*buf.as_ptr().cast::<TOKEN_USER>();
        let mut system = [0u64; 9];
        let mut system_len = std::mem::size_of_val(&system) as u32;
        assert_ne!(
            CreateWellKnownSid(
                WinLocalSystemSid,
                std::ptr::null_mut(),
                system.as_mut_ptr().cast(),
                &mut system_len
            ),
            0
        );
        assert_eq!((*acl).AceCount, 2, "expected user and SYSTEM allow ACEs");
        let mut seen_user = false;
        let mut seen_system = false;
        for index in 0..(*acl).AceCount {
            let mut raw = std::ptr::null_mut();
            assert_ne!(GetAce(acl, index.into(), &mut raw), 0);
            let ace = &*raw.cast::<ACCESS_ALLOWED_ACE>();
            assert_eq!(ace.Header.AceType, 0, "expected ACCESS_ALLOWED_ACE_TYPE");
            let sid = std::ptr::addr_of!(ace.SidStart).cast_mut().cast();
            if EqualSid(sid, user.User.Sid) != 0 {
                assert!(!seen_user, "duplicate user ACE");
                seen_user = true;
            } else {
                assert_ne!(
                    EqualSid(sid, system.as_mut_ptr().cast()),
                    0,
                    "unexpected trustee (Everyone/anonymous must fail)"
                );
                assert!(!seen_system, "duplicate SYSTEM ACE");
                seen_system = true;
            }
        }
        assert!(seen_user && seen_system);
        LocalFree(sd);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_answers_over_the_named_pipe() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let base = std::env::temp_dir().join(format!("fl-win-{short}"));
    let paths = Paths {
        data_dir: base.join("d"),
        config_dir: base.join("c"),
        profile: format!("test-win-{short}"),
    };
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    formless_core::Config::default().save(&paths).unwrap();
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    tokio::spawn({
        let d = daemon.clone();
        async move { d.serve().await }
    });
    let client = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(paths.pipe_name()) {
                Ok(c) => break c,
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
            }
        }
    })
    .await
    .expect("pipe never appeared within 10 seconds");
    let (r, mut w) = tokio::io::split(client);
    let env = formless_protocol::RequestEnvelope {
        id: 1,
        protocol: formless_protocol::PROTOCOL_VERSION,
        request: formless_protocol::Request::Ping,
    };
    w.write_all(format!("{}\n", serde_json::to_string(&env).unwrap()).as_bytes())
        .await
        .unwrap();
    let mut lines = tokio::io::BufReader::new(r).lines();
    let line = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
        .await
        .expect("Ping response never arrived")
        .unwrap()
        .unwrap();
    match serde_json::from_str::<formless_protocol::ServerMessage>(&line).unwrap() {
        formless_protocol::ServerMessage::Response {
            id,
            ok,
            result,
            error,
        } => {
            assert_eq!(id, 1);
            assert!(ok, "Ping failed: {error:?}");
            let result = result.expect("Ping omitted its payload");
            assert_eq!(result["min_protocol"], 1);
            assert!(result["features"].as_array().unwrap().len() > 1);
        }
        other => panic!("expected Ping response, got {other:?}"),
    }
}
