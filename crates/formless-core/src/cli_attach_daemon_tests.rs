use super::*;

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[tokio::test]
async fn applied_approval_reports_audit_failure_without_replaying_the_decision() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let fixture = Fixture(std::env::temp_dir().join(format!("cli-audit-{}", uuid::Uuid::new_v4())));
    let paths = Paths::from_dirs(fixture.0.join("d"), fixture.0.join("c")).unwrap();
    Config::default().save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths.clone()).unwrap());
    let conn = rusqlite::Connection::open(paths.db_file()).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_cli_audit BEFORE INSERT ON audit WHEN NEW.action='approval.decision' BEGIN SELECT RAISE(FAIL,'private audit failure'); END").unwrap();
    let (tx, rx) = oneshot::channel();
    daemon
        .approvals
        .locked()
        .insert("private-pending".into(), tx);
    let request = || Request::ApprovalDecision {
        approval_id: "private-pending".into(),
        approved: true,
        scope: ApprovalScope::Once,
        paths: Default::default(),
        remember_denial: false,
    };
    let result = CLIENT
        .scope((42, "CLI".into()), daemon.handle(request()))
        .await
        .unwrap();
    assert_eq!(result["resolved"], true);
    assert_eq!(result["audit_recorded"], false);
    assert!(rx.await.unwrap().0);
    let repeated = CLIENT
        .scope((42, "CLI".into()), daemon.handle(request()))
        .await
        .unwrap();
    assert_eq!(repeated["resolved"], false);
    assert!(repeated["audit_recorded"].is_null());
}
