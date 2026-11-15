//! Summary consumers use private Skill files and records without executing them.
use super::*;

#[tokio::test]
async fn summary_consumers_keep_search_harness_and_bulk_export_membership() {
    let _env = crate::credential_env::EnvGuard::isolated();
    let root = std::env::temp_dir().join(format!("fl-skill-projection-{}", uuid::Uuid::new_v4()));
    let paths = Paths::from_dirs(root.join("data"), root.join("config")).unwrap();
    let mut cfg = Config::default();
    cfg.naming.enabled = false;
    cfg.provider.base_url = "http://127.0.0.1:9/v1".into();
    cfg.provider.model = "private-unused".into();
    cfg.save(&paths).unwrap();
    let daemon = Arc::new(Daemon::new(paths).unwrap());
    let session=daemon.with_store(|st| {
        for (index,name) in ["00-disabled","01-retired","active","日本語"].into_iter().enumerate() {
            let folder=root.join(format!("skill-{index}"));std::fs::create_dir_all(&folder)?;
            std::fs::write(folder.join("SKILL.md"),format!("---\nname: {name}\ndescription: needle reference\n---\nPrivate ไทย notes\n"))?;
            st.add_skill_version(name,"needle reference ไทย",1,&folder.to_string_lossy(),"local","user",false)?;
        }
        st.set_skill_enabled("active",true)?;
        st.set_skill_enabled("日本語",true)?;
        st.set_skill_enabled("01-retired",true)?;
        st.set_skill_enabled("00-disabled",false)?;
        let conn=rusqlite::Connection::open(daemon.paths.db_file())?;
        conn.execute("UPDATE skills SET retired=1 WHERE name='01-retired'",[])?;
        Ok(st.create_session(None)?.id)
    }).unwrap();
    let original = daemon.with_reader(|st| st.list_skills()).unwrap();
    let search: Value = serde_json::from_str(
        &daemon
            .find_tools_impl(&session, &json!({"query":"needle","limit":1}))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        search["skills"],
        json!([{"name":"active","description":"needle reference ไทย","load_with":"load_skill"}])
    );
    let harness = daemon.harness_read().await.unwrap();
    assert_eq!(
        harness["skills"],
        json!(original
            .iter()
            .map(|s| json!({"name":s.name,"enabled":s.enabled,"description":s.description}))
            .collect::<Vec<_>>())
    );
    let expected = daemon
        .with_reader(|st| {
            original
                .iter()
                .map(|s| crate::skills::export(st, &s.name))
                .collect::<Result<Vec<_>>>()
        })
        .unwrap();
    let export = daemon.handle_inner(Request::ExportSkills).await.unwrap();
    assert_eq!(
        export,
        json!({"format":"formless.skills/1","skills":expected})
    );
    assert_eq!(daemon.export_all(false).unwrap()["skills"], json!(expected));
    assert_eq!(daemon.with_reader(|st| st.list_skills()).unwrap(), original);
    drop(daemon);
    std::fs::remove_dir_all(root).unwrap();
}
