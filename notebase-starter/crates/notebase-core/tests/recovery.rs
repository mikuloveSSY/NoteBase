use notebase_core::{
    domain::RelPath,
    index::{
        recovery,
        sqlite::SqliteIndexStore,
        worker::{BackgroundIndex, WorkerOptions},
        IndexSession, IndexState, IndexStore, NoteRecord,
    },
    migration::{copy_verified, LocalVaultLocation, VaultLocation},
    new_indexed_app, Request,
};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    fs,
    sync::Arc,
    time::{Duration, Instant},
};
const ID: &str = "0123456789abcdef";
fn record(path: &str, body: &str) -> NoteRecord {
    NoteRecord {
        path: RelPath::new(path).unwrap(),
        title: path.into(),
        body: body.into(),
        size_bytes: body.len() as u64,
        mtime_ms: 1,
        content_hash: "a".repeat(64),
        created_ms: None,
        updated_ms: None,
        has_attachment: false,
        aliases: vec![],
        tags: vec![],
    }
}
fn seed_business(path: &std::path::Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("INSERT INTO snapshot_index VALUES('A.md','v1','snapshots/a',3,1); INSERT INTO sync_state VALUES('A.md','base','local','etag','dev',1,'dirty'); INSERT INTO sync_tombstone VALUES('B.md',1,'hash'); INSERT INTO rename_journal VALUES(1,'A.md','C.md','[]','[]',1,'running');").unwrap();
}
fn assert_business(index: &SqliteIndexStore) {
    for table in [
        "snapshot_index",
        "sync_state",
        "sync_tombstone",
        "rename_journal",
    ] {
        assert_eq!(
            index
                .read(
                    |c| c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                        .get::<_, i64>(0))
                )
                .unwrap(),
            1
        );
    }
}
#[test]
fn candidate_publication_rolls_back_on_failure_and_preserves_business() {
    let dir = tempfile::tempdir().unwrap();
    let old_path = dir.path().join("old.db");
    let old = SqliteIndexStore::open(&old_path, ID).unwrap();
    old.upsert_batch(&[record("A.md", "旧数据标记")]).unwrap();
    old.finish_build().unwrap();
    seed_business(&old_path);
    let candidate_path = dir.path().join("new.db");
    let candidate = SqliteIndexStore::open(&candidate_path, ID).unwrap();
    candidate
        .upsert_batch(&[record("B.md", "新数据标记")])
        .unwrap();
    candidate.finish_build().unwrap();
    let fault = Connection::open(&old_path).unwrap();
    fault.execute_batch("CREATE TRIGGER fail_insert BEFORE INSERT ON notes BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    assert!(old.publish_from(&candidate_path).is_err());
    assert!(old.get(&RelPath::new("A.md").unwrap()).unwrap().is_some());
    assert_business(&old);
    fault.execute_batch("DROP TRIGGER fail_insert;").unwrap();
    old.publish_from(&candidate_path).unwrap();
    assert!(old.get(&RelPath::new("A.md").unwrap()).unwrap().is_none());
    assert_eq!(
        old.search(&json!({"query":"新数据"})).unwrap()["totalApprox"],
        1
    );
    assert_business(&old);
}
#[test]
fn corrupt_database_is_archived_without_overwriting_original_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    fs::write(&path, b"damaged sqlite bytes").unwrap();
    let recovered = recovery::open(&path, ID).unwrap();
    let archive = recovered.archived.unwrap();
    assert_eq!(
        fs::read(archive.join("index.db")).unwrap(),
        b"damaged sqlite bytes"
    );
    assert!(!recovered.business_restored);
    assert_eq!(recovered.store.status().unwrap().note_count, 0);
}
#[test]
fn unknown_schema_preserves_original_and_restores_four_known_business_tables() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let store = SqliteIndexStore::open(&path, ID).unwrap();
    drop(store);
    seed_business(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("ALTER TABLE sync_state ADD COLUMN future_value TEXT; UPDATE sync_state SET future_value='preserve extra data'; UPDATE meta SET value='future' WHERE key='schema_version';").unwrap();
    drop(conn);
    let result = recovery::open(&path, ID).unwrap();
    assert!(result.business_restored);
    assert_business(&result.store);
    let backup = Connection::open(result.archived.unwrap().join("index.db")).unwrap();
    assert_eq!(
        backup
            .query_row("SELECT future_value FROM sync_state", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "preserve extra data"
    );
}
#[test]
fn identity_mismatch_and_active_writer_are_not_treated_as_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let store = SqliteIndexStore::open(&path, ID).unwrap();
    assert!(recovery::open(&path, ID).err().unwrap().retryable);
    drop(store);
    assert!(recovery::open(&path, "1111111111111111").is_err());
    assert!(!fs::read_dir(dir.path()).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("recovery-")));
}
fn wait_ready(worker: &BackgroundIndex) {
    let start = Instant::now();
    while worker.status().state != IndexState::Ready {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "{:?}",
            worker.status()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(250));
}
#[test]
fn background_rebuild_returns_job_and_retains_old_query_while_accepting_save() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    for n in 0..500 {
        fs::write(
            vault.join(format!("{n:04}.md")),
            format!("旧索引标记 {n} {}", "正文".repeat(512)),
        )
        .unwrap();
    }
    let worker = BackgroundIndex::start_with_options(
        dir.path().join("data"),
        vault.to_str().unwrap().into(),
        ID.into(),
        Arc::new(|_, _| {}),
        WorkerOptions {
            native_watch: false,
            ..WorkerOptions::default()
        },
    );
    wait_ready(&worker);
    let start = Instant::now();
    let job = worker.rebuild().unwrap();
    assert!(job.starts_with("rebuild-"));
    assert!(start.elapsed() < Duration::from_millis(100));
    assert!(worker.rebuild().is_err());
    let page = worker.search(&json!({"query":"旧索引","limit":1})).unwrap();
    assert_eq!(page["degraded"], true);
    assert_eq!(page["degradeReason"], "index_rebuilding");
    fs::write(vault.join("0000.md"), "重建期间保存的内容").unwrap();
    assert!(worker.enqueue(&RelPath::new("0000.md").unwrap()));
    let start = Instant::now();
    loop {
        if let Ok(page) = worker.search(&json!({"query":"重建期间保存"})) {
            if page["totalApprox"] == 1 && !page["degraded"].as_bool().unwrap() {
                break;
            }
        }
        assert!(start.elapsed() < Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(10));
    }
    worker.shutdown();
}
#[test]
fn startup_recovers_corruption_and_rebuilds_real_notes() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("A.md"), "损坏恢复标记").unwrap();
    let data = dir.path().join("data");
    let folder = data.join("vaults").join(ID);
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("index.db"), b"broken").unwrap();
    let worker = BackgroundIndex::start(
        data,
        vault.to_str().unwrap().into(),
        ID.into(),
        Arc::new(|_, _| {}),
    );
    wait_ready(&worker);
    assert_eq!(
        worker.search(&json!({"query":"损坏恢复"})).unwrap()["totalApprox"],
        1
    );
    worker.shutdown();
}
#[test]
fn migration_copies_notes_attachments_trash_and_empty_directories_and_keeps_source() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    fs::create_dir_all(source.join("attachments/empty")).unwrap();
    fs::create_dir(source.join("trash")).unwrap();
    fs::write(source.join("A.md"), "笔记\r\n").unwrap();
    fs::write(source.join("attachments/image.bin"), [0, 255, 1]).unwrap();
    fs::write(source.join("trash/old.md"), "旧笔记").unwrap();
    let target = dir.path().join("target");
    copy_verified(&source, &target).unwrap();
    for file in ["A.md", "attachments/image.bin", "trash/old.md"] {
        assert_eq!(
            fs::read(source.join(file)).unwrap(),
            fs::read(target.join(file)).unwrap()
        );
    }
    assert!(target.join("attachments/empty").is_dir());
    assert!(source.join("A.md").exists());
    assert!(copy_verified(&source, &target).is_err());
    assert!(copy_verified(&source, &source.join("nested")).is_err());
}
#[test]
fn settings_persistence_keeps_other_members_fields_and_refuses_bad_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(
        &path,
        r#"{"appearance":{"theme":"dark"},"vaultPath":"old"}"#,
    )
    .unwrap();
    let store = LocalVaultLocation {
        settings_path: path.clone(),
    };
    store.save_path("new").unwrap();
    assert_eq!(store.configured_path().unwrap().as_deref(), Some("new"));
    let settings: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(settings["appearance"]["theme"], "dark");
    fs::write(&path, b"bad json").unwrap();
    assert!(store.save_path("overwrite").is_err());
    assert_eq!(fs::read(path).unwrap(), b"bad json");
}
fn call(app: &mut notebase_core::App, command: &str, payload: Value) -> Value {
    serde_json::to_value(app.dispatch(Request {
        id: "test".into(),
        command: command.into(),
        payload,
        client_seq: 1,
    }))
    .unwrap()
}
#[test]
fn ipc_migration_switches_only_after_verified_copy_and_recalculates_id() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("A.md"), "原始笔记").unwrap();
    let data = dir.path().join("data");
    let target = dir.path().join("target");
    let mut app = new_indexed_app(data.clone(), Arc::new(|_, _| {}));
    let before = call(&mut app, "vault_open", json!({"path":source}));
    assert_eq!(
        call(
            &mut app,
            "settings_set",
            json!({"values":{"vaultPath":target}})
        )["ok"],
        true
    );
    let values = call(&mut app, "settings_get", json!({"keys":["vaultPath"]}));
    assert!(values["data"]["values"]["vaultPath"]
        .as_str()
        .unwrap()
        .ends_with("target"));
    let after = call(&mut app, "vault_open", json!({"path":target}));
    assert_ne!(before["data"]["vaultId"], after["data"]["vaultId"]);
    assert!(source.join("A.md").exists());
    assert!(data.join("settings.json").exists());
}
#[test]
fn failed_settings_commit_keeps_active_source_and_original_config() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("A.md"), "原笔记").unwrap();
    let data = dir.path().join("data");
    fs::create_dir(&data).unwrap();
    fs::write(data.join("settings.json"), b"invalid").unwrap();
    let mut app = new_indexed_app(data.clone(), Arc::new(|_, _| {}));
    call(&mut app, "vault_open", json!({"path":source}));
    let target = dir.path().join("target");
    assert_eq!(
        call(
            &mut app,
            "settings_set",
            json!({"values":{"vaultPath":target}})
        )["ok"],
        false
    );
    assert!(call(&mut app, "vault_info", json!({}))["data"]["vaultPath"]
        .as_str()
        .unwrap()
        .ends_with("source"));
    assert!(source.join("A.md").exists());
    assert!(target.join("A.md").exists());
    assert_eq!(fs::read(data.join("settings.json")).unwrap(), b"invalid");
}

#[test]
fn migration_carries_snapshot_blobs_and_known_business_state_to_new_vault_id() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("A.md"), "原始内容").unwrap();
    let data = dir.path().join("data");
    let id = |p: &std::path::Path| {
        format!(
            "{:x}",
            Sha256::digest(fs::canonicalize(p).unwrap().to_string_lossy().as_bytes())
        )[..16]
            .to_owned()
    };
    let cache = data.join("vaults").join(id(&source));
    fs::create_dir_all(cache.join("snapshots")).unwrap();
    fs::write(cache.join("snapshots/a"), "快照原文").unwrap();
    let db = SqliteIndexStore::open(&cache.join("index.db"), &id(&source)).unwrap();
    seed_business(&cache.join("index.db"));
    drop(db);
    let target = dir.path().join("target");
    LocalVaultLocation {
        settings_path: data.join("settings.json"),
    }
    .copy_verified(source.to_str().unwrap(), target.to_str().unwrap())
    .unwrap();
    let new = data.join("vaults").join(id(&target));
    assert_eq!(
        fs::read_to_string(new.join("snapshots/a")).unwrap(),
        "快照原文"
    );
    assert_eq!(
        fs::read_to_string(cache.join("snapshots/a")).unwrap(),
        "快照原文"
    );
    let restored = SqliteIndexStore::open(&new.join("index.db"), &id(&target)).unwrap();
    assert_business(&restored);
}
#[test]
fn interrupted_background_rebuild_leaves_a_complete_main_index() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    for n in 0..300 {
        fs::write(
            vault.join(format!("{n}.md")),
            "中断重建仍完整 ".repeat(1024),
        )
        .unwrap();
    }
    let data = dir.path().join("data");
    let worker = BackgroundIndex::start_with_options(
        data.clone(),
        vault.to_str().unwrap().into(),
        ID.into(),
        Arc::new(|_, _| {}),
        WorkerOptions {
            native_watch: false,
            ..WorkerOptions::default()
        },
    );
    wait_ready(&worker);
    worker.rebuild().unwrap();
    worker.shutdown();
    let folder = data.join("vaults").join(ID);
    let index = SqliteIndexStore::open(&folder.join("index.db"), ID).unwrap();
    assert_eq!(index.status().unwrap().note_count, 300);
    assert_eq!(
        index.search(&json!({"query":"中断重建"})).unwrap()["totalApprox"],
        300
    );
    assert!(!fs::read_dir(folder).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("rebuild-")));
}

#[test]
fn unrecoverable_business_state_keeps_archive_and_warning_on_next_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("index.db");
    let index = SqliteIndexStore::open(&path, ID).unwrap();
    drop(index);
    seed_business(&path);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "PRAGMA ignore_check_constraints=ON; UPDATE sync_state SET state='future-state';",
    )
    .unwrap();
    drop(conn);
    assert!(recovery::open(&path, ID).is_err());
    let recovered = recovery::open(&path, ID).unwrap();
    assert!(!recovered.business_restored);
    let archive = recovered.archived.unwrap();
    let original = Connection::open(archive.join("index.db")).unwrap();
    assert_eq!(
        original
            .query_row("SELECT state FROM sync_state", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "future-state"
    );
    assert!(path.with_extension("recovery-info.json").exists());
}
#[test]
#[cfg(windows)]
fn migration_refuses_directory_junction_without_touching_original() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let outside = dir.path().join("outside");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.md"), "原数据").unwrap();
    let link = source.join("link");
    let result = std::process::Command::new("cmd.exe")
        .args(["/c", "mklink", "/J"])
        .arg(&link)
        .arg(&outside)
        .output()
        .unwrap();
    assert!(result.status.success());
    let target = dir.path().join("target");
    let error = copy_verified(&source, &target).unwrap_err();
    fs::remove_dir(&link).unwrap();
    assert_eq!(error.code, "E_PATH_INVALID");
    assert!(!target.exists());
    assert_eq!(
        fs::read_to_string(outside.join("secret.md")).unwrap(),
        "原数据"
    );
}
