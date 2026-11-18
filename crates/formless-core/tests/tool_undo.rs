//! Undoing the newest Tool registry change (SPEC §7.2).

use formless_core::{Config, Paths, Store};
use formless_protocol::Request;
use formless_runtime::Capabilities;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread")]
async fn the_newest_registry_change_can_be_undone() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-undo-{short}"));
    let paths = Paths::from_dirs(dir.join("d"), dir.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.save(&paths).unwrap();
    let id = {
        let store = Store::open(&paths.db_file()).unwrap();
        let caps = Capabilities::parse(&[]).unwrap();
        store
            .create_tool_version(
                "echo_text",
                "d",
                r#"{"type":"object"}"#,
                "{}",
                "function run(i){ return {a: 1}; }",
                &caps,
                64,
                "agent",
            )
            .unwrap()
            .0
            .id
    };
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let undo = || {
        let d = daemon.clone();
        async move { d.handle(Request::UndoRegistryChange).await }
    };
    assert!(undo().await.is_err(), "nothing recorded yet");

    // An update (here the user's code edit) is rolled back.
    daemon
        .handle(Request::UpdateToolCode {
            id: id.clone(),
            code: "function run(i){ return {a: 2}; }".into(),
        })
        .await
        .unwrap();
    let r = undo().await.unwrap();
    assert_eq!(r["undone"], "user updated echo_text v2");
    assert_eq!(r["tool"]["current_version"], 1);
    assert!(undo().await.is_err(), "one undo per change");

    // A retire is reversed.
    daemon
        .handle(Request::RetireTool { id: id.clone() })
        .await
        .unwrap();
    let r = undo().await.unwrap();
    assert_eq!(r["tool"]["retired"], false);

    // A new Tool (here an import) is retired, not purged.
    let bundle = daemon.handle(Request::ExportTool { id }).await.unwrap();
    let imported = daemon.handle(Request::ImportTool { bundle }).await.unwrap();
    assert_eq!(imported["name"], "echo_text_imported");
    let r = undo().await.unwrap();
    assert_eq!(r["undone"], "import created echo_text_imported v1");
    assert_eq!(r["tool"]["retired"], true);
    let store = Store::open(&paths.db_file()).unwrap();
    assert!(
        store
            .get_tool_by_name("echo_text_imported")
            .unwrap()
            .is_some(),
        "kept"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_guarded_undo_cannot_revert_another_clients_change_or_be_repeated() {
    use formless_protocol::{
        Event, RegistryChange, RequestEnvelope, ServerMessage, PROTOCOL_VERSION,
    };
    use serde_json::Value;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    async fn exchange(
        client: &mut BufReader<UnixStream>,
        request: Request,
    ) -> (Result<Value, String>, Option<RegistryChange>) {
        let line = serde_json::to_string(&RequestEnvelope {
            id: 1,
            protocol: PROTOCOL_VERSION,
            request,
        })
        .unwrap();
        client
            .get_mut()
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut change = None;
            loop {
                let mut line = String::new();
                assert_ne!(client.read_line(&mut line).await.unwrap(), 0);
                match serde_json::from_str(&line).unwrap() {
                    ServerMessage::Event(Event::RegistryChanged { change: latest }) => {
                        change = Some(latest)
                    }
                    ServerMessage::Response {
                        id: 1,
                        ok,
                        result,
                        error,
                    } => {
                        return (
                            if ok {
                                Ok(result.unwrap_or_default())
                            } else {
                                Err(error.unwrap())
                            },
                            change,
                        );
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("daemon reply within deadline")
    }

    let uuid = uuid::Uuid::new_v4().simple().to_string();
    let dir = std::env::temp_dir().join(format!("fl-tg-{}", &uuid[..8]));
    let paths = Paths::from_dirs(dir.join("d"), dir.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "fixture".into();
    cfg.save(&paths).unwrap();
    let store = Store::open(&paths.db_file()).unwrap();
    let seed = |name| {
        store
            .create_tool_version(
                name,
                "fixture",
                "{}",
                "{}",
                "function run(){return {};}",
                &Capabilities::parse(&[]).unwrap(),
                64,
                "user",
            )
            .unwrap()
            .0
            .id
    };
    let a = seed("tool_a");
    let b = seed("tool_b");
    let check = |id: &str| store.get_tool_current(id).unwrap().unwrap().0.retired;
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let server = tokio::spawn(daemon.serve());
    let connect = || async {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(stream) = UnixStream::connect(paths.socket_path()).await {
                    break BufReader::new(stream);
                }
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
        })
        .await
        .unwrap()
    };
    let mut client_a = connect().await;
    let mut client_b = connect().await;
    let ping = exchange(&mut client_b, Request::Ping).await.0;
    assert!(ping.is_ok());

    let (result, change_a) = exchange(&mut client_a, Request::RetireTool { id: a.clone() }).await;
    assert!(result.is_ok());
    let change_a = change_a.unwrap();
    assert_eq!(change_a.tool_id, a);
    let (result, change_b) = exchange(&mut client_b, Request::RetireTool { id: b.clone() }).await;
    assert!(result.is_ok());
    let change_b = change_b.unwrap();
    assert_eq!(change_b.tool_id, b);
    assert_ne!(change_a.change_id, change_b.change_id);

    let stale = exchange(
        &mut client_a,
        Request::UndoRegistryChangeIfCurrent {
            change_id: change_a.change_id.unwrap(),
        },
    )
    .await
    .0
    .unwrap_err();
    assert!(stale.contains("no longer available"), "{stale}");
    assert!(check(&a) && check(&b), "stale Undo changes neither Tool");

    let change_id = change_b.change_id.unwrap();
    let undo = || Request::UndoRegistryChangeIfCurrent {
        change_id: change_id.clone(),
    };
    let (first, repeated) = tokio::join!(
        exchange(&mut client_a, undo()),
        exchange(&mut client_b, undo())
    );
    let mut results = [first.0, repeated.0].into_iter();
    let first = results.next().unwrap();
    let repeated = results.next().unwrap();
    let (success, rejected) = if first.is_ok() {
        (first, repeated)
    } else {
        (repeated, first)
    };
    assert_eq!(success.unwrap()["tool"]["id"], b);
    assert!(rejected.unwrap_err().contains("no longer available"));
    assert!(check(&a) && !check(&b), "only B was restored");
    assert!(exchange(&mut client_b, Request::Shutdown).await.0.is_ok());
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_is_reviewed_without_being_saved() {
    let short = &uuid::Uuid::new_v4().to_string()[..8];
    let dir = std::path::PathBuf::from(format!("/tmp/fl-prev-{short}"));
    let paths = Paths::from_dirs(dir.join("d"), dir.join("c")).unwrap();
    let mut cfg = Config::default();
    cfg.provider.base_url = "http://127.0.0.1:9".into();
    cfg.provider.model = "m".into();
    cfg.save(&paths).unwrap();
    let id = {
        let store = Store::open(&paths.db_file()).unwrap();
        let caps = Capabilities::parse(&[]).unwrap();
        store
            .create_tool_version(
                "echo_text",
                "d",
                r#"{"type":"object"}"#,
                "{}",
                "function run(i){\n  return {a: 1};\n}",
                &caps,
                64,
                "agent",
            )
            .unwrap()
            .0
            .id
    };
    let daemon = Arc::new(formless_core::daemon::Daemon::new(paths.clone()).unwrap());
    let preview = |code: &str| {
        let (d, id, code) = (daemon.clone(), id.clone(), code.to_string());
        async move {
            d.handle(Request::PreviewToolCode { id, code })
                .await
                .unwrap()
        }
    };
    let good = preview("function run(i){\n  return {a: 2};\n}").await;
    assert_eq!(good["ok"], true, "{good}");
    let diff = good["diff"].as_str().unwrap();
    assert!(
        diff.contains("-   return {a: 1};") && diff.contains("+   return {a: 2};"),
        "{diff}"
    );

    let broken = preview("function run(i){ return {a: ").await;
    assert_eq!(
        (broken["ok"].clone(), broken["stage"].clone()),
        (serde_json::json!(false), serde_json::json!("syntax"))
    );
    let throws = preview("function run(i){ throw new Error('nope'); }").await;
    assert_eq!(throws["stage"], "smoke", "{throws}");
    assert!(throws["error"].as_str().unwrap().contains("nope"));

    let store = Store::open(&paths.db_file()).unwrap();
    let (info, _) = store.get_tool_current(&id).unwrap().unwrap();
    assert_eq!(info.current_version, 1, "a preview saves nothing");
    let _ = std::fs::remove_dir_all(dir);
}
