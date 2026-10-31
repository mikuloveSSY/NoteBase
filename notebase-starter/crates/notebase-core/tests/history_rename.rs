use notebase_core::{
    data_services::{files::digest, DataService, LocalDataService},
    domain::{AppError, Result},
    new_stateful_app, App, Request, Response,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
fn response(app: &mut App, command: &str, payload: Value) -> Response {
    app.dispatch(Request {
        id: "history".into(),
        command: command.into(),
        payload,
        client_seq: 1,
    })
}
fn call(app: &mut App, command: &str, payload: Value) -> Value {
    match response(app, command, payload) {
        Response::Ok { data, .. } => data,
        other => panic!("{command}: {other:?}"),
    }
}
fn setup() -> (tempfile::TempDir, PathBuf, App) {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("A.md"), "original").unwrap();
    let mut app = new_stateful_app(dir.path().join("data"));
    call(&mut app, "vault_open", json!({"path":vault}));
    (dir, vault, app)
}
fn rename(app: &mut App, path: &str, new_name: &str) -> Value {
    let plan = call(
        app,
        "note_rename",
        json!({"path":path,"newName":new_name,"planOnly":true}),
    );
    call(
        app,
        "note_rename",
        json!({"path":path,"newName":new_name,"planOnly":false,"planId":plan["plan"]["id"]}),
    )
}

#[test]
fn chain_rename_preserves_versions_and_old_path_reuse_is_a_new_history() {
    let (_dir, vault, mut app) = setup();
    let saved = call(
        &mut app,
        "backup_create",
        json!({"scope":"note","path":"A.md","description":"keep description"}),
    );
    let before = call(
        &mut app,
        "note_list",
        json!({"scope":"versions","path":"A.md"}),
    )["items"]
        .clone();
    rename(&mut app, "A.md", "中文 B");
    rename(&mut app, "中文 B.md", "C");
    let after = call(
        &mut app,
        "note_list",
        json!({"scope":"versions","path":"C.md"}),
    )["items"]
        .clone();
    assert_eq!(after[0]["path"], "C.md");
    for key in ["versionId", "createdMs", "description", "contentHash"] {
        assert_eq!(after[0][key], before[0][key]);
    }
    assert_eq!(
        call(
            &mut app,
            "note_read",
            json!({"path":"C.md","versionId":saved["versionId"]})
        )["body"],
        "original"
    );
    call(
        &mut app,
        "note_create",
        json!({"title":"A","body":"new owner"}),
    );
    call(
        &mut app,
        "backup_create",
        json!({"scope":"note","path":"A.md"}),
    );
    let old = call(
        &mut app,
        "note_list",
        json!({"scope":"versions","path":"A.md"}),
    );
    assert_eq!(old["items"].as_array().unwrap().len(), 1);
    assert_ne!(old["items"][0]["versionId"], saved["versionId"]);
    assert_eq!(fs::read_to_string(vault.join("C.md")).unwrap(), "original");
}

#[test]
fn target_history_and_damaged_source_are_refused_before_note_is_moved() {
    let (dir, vault, mut app) = setup();
    call(
        &mut app,
        "backup_create",
        json!({"scope":"note","path":"A.md"}),
    );
    fs::write(vault.join("B.md"), "other owner").unwrap();
    call(
        &mut app,
        "backup_create",
        json!({"scope":"note","path":"B.md"}),
    );
    fs::remove_file(vault.join("B.md")).unwrap();
    let plan = call(
        &mut app,
        "note_rename",
        json!({"path":"A.md","newName":"B","planOnly":true}),
    );
    assert!(
        matches!(response(&mut app,"note_rename",json!({"path":"A.md","newName":"B","planOnly":false,"planId":plan["plan"]["id"]})),Response::Err{error,..} if error.code=="E_PATH_INVALID")
    );
    assert!(vault.join("A.md").exists());
    assert!(!vault.join("B.md").exists());
    // A separate service must also refuse a corrupt blob without altering its bytes.
    let svc = LocalDataService::open(
        vault.to_str().unwrap(),
        &dir.path().join("test-data"),
        "test",
    )
    .unwrap();
    let version = svc.snapshot("A.md", b"valid", "").unwrap();
    let blob = dir
        .path()
        .join("test-data/vaults/test/snapshots")
        .join(digest(b"A.md"))
        .join(format!("{}.md", version["versionId"].as_str().unwrap()));
    fs::write(&blob, b"broken").unwrap();
    assert_eq!(
        svc.rename_versions("A.md", "C.md", &"a".repeat(64), true)
            .unwrap_err()
            .code,
        "E_BACKUP_CHECKSUM"
    );
    assert_eq!(fs::read(&blob).unwrap(), b"broken");
}

#[test]
fn interrupted_metadata_transfer_is_idempotent_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    let svc =
        LocalDataService::open(vault.to_str().unwrap(), &dir.path().join("data"), "test").unwrap();
    let first = svc.snapshot("A.md", b"one", "first").unwrap();
    svc.snapshot("A.md", b"two", "second").unwrap();
    let root = dir.path().join("data/vaults/test/snapshots");
    let source = root.join(digest(b"A.md"));
    let target = root.join(digest(b"B.md"));
    let id = "b".repeat(64);
    fs::write(
        source.join(".notebase-history-move"),
        serde_json::to_vec(&json!({"journalId":id,"from":"A.md","to":"B.md"})).unwrap(),
    )
    .unwrap();
    fs::rename(&source, &target).unwrap();
    let record = target.join(format!("{}.json", first["versionId"].as_str().unwrap()));
    let mut meta: Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    meta["path"] = json!("B.md");
    fs::write(&record, serde_json::to_vec(&meta).unwrap()).unwrap();
    drop(svc);
    let reopened =
        LocalDataService::open(vault.to_str().unwrap(), &dir.path().join("data"), "test").unwrap();
    reopened
        .rename_versions("A.md", "B.md", &id, false)
        .unwrap();
    reopened
        .rename_versions("A.md", "B.md", &id, false)
        .unwrap();
    assert_eq!(
        reopened.versions(&json!({"path":"B.md"})).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(!source.exists());
}

struct FailHistory {
    inner: LocalDataService,
    fail: Arc<AtomicBool>,
}
macro_rules! forward {($($name:ident($($arg:ident:$ty:ty),*)->$out:ty;)*)=>{$(fn $name(&self,$($arg:$ty),*)->Result<$out>{self.inner.$name($($arg),*)})*};}
impl DataService for FailHistory {
    fn rename_versions(&self, from: &str, to: &str, id: &str, check_only: bool) -> Result<()> {
        if !check_only && self.fail.swap(false, Ordering::AcqRel) {
            return Err(AppError::new(
                "E_IO_DISK_FULL",
                "fixture metadata write interrupted",
            ));
        }
        self.inner.rename_versions(from, to, id, check_only)
    }
    forward! {export(p:&Value)->Value;backup_create(p:&Value)->Value;backup_restore(p:&Value)->Value;versions(p:&Value)->Value;version_body(p:&Value)->String;snapshot(path:&str,bytes:&[u8],note:&str)->Value;settings()->Value;configure(p:&Value)->Value;maintenance(now:u64)->Value;maintenance_due(now:u64)->bool;prune_versions(now:u64)->usize;sync_configure(p:&Value)->Value;sync_now(p:&Value)->Value;sync_status()->Value;}
}
fn failing_app(data: &Path, fail: Arc<AtomicBool>) -> App {
    let root = data.to_path_buf();
    new_stateful_app(root.clone()).with_data(Box::new(move |vault, id| {
        Ok(Box::new(FailHistory {
            inner: LocalDataService::open(vault, &root, id)?,
            fail: fail.clone(),
        }))
    }))
}
#[test]
fn failed_history_step_keeps_journal_blocks_reuse_and_resumes_after_restart() {
    let (dir, vault, _old) = setup();
    let root = dir.path().join("data");
    let mut app = failing_app(&root, Arc::new(AtomicBool::new(true)));
    call(&mut app, "vault_open", json!({"path":vault}));
    let old = call(&mut app, "note_read", json!({"path":"A.md"}));
    let snap = call(
        &mut app,
        "backup_create",
        json!({"scope":"note","path":"A.md"}),
    );
    let plan = call(
        &mut app,
        "note_rename",
        json!({"path":"A.md","newName":"B","planOnly":true}),
    );
    assert!(
        matches!(response(&mut app,"note_rename",json!({"path":"A.md","newName":"B","planOnly":false,"planId":plan["plan"]["id"]})),Response::Err{error,..} if error.code=="E_PARTIAL_RENAME")
    );
    assert!(vault.join("B.md").exists());
    assert!(!vault.join("A.md").exists());
    assert!(
        matches!(response(&mut app,"note_create",json!({"title":"A","body":"cannot mix"})),Response::Err{error,..} if error.code=="E_PARTIAL_RENAME")
    );
    call(
        &mut app,
        "note_save",
        json!({"action":"draft","path":"A.md","baseHash":old["contentHash"],"body":"pending editor text"}),
    );
    let current = call(&mut app, "note_read", json!({"path":"B.md"}));
    assert!(
        matches!(response(&mut app,"note_save",json!({"path":"B.md","baseHash":current["contentHash"],"body":"blocked"})),Response::Err{error,..} if error.code=="E_PARTIAL_RENAME")
    );
    drop(app);
    let mut app = new_stateful_app(root);
    call(&mut app, "vault_open", json!({"path":vault}));
    let journal = call(&mut app, "note_list", json!({"scope":"renames"}))["items"][0]["id"].clone();
    call(&mut app, "note_rename", json!({"resumeId":journal}));
    assert_eq!(
        call(
            &mut app,
            "note_read",
            json!({"path":"B.md","versionId":snap["versionId"]})
        )["body"],
        "original"
    );
    assert_eq!(
        call(&mut app, "note_list", json!({"scope":"drafts"}))["items"][0]["body"],
        "pending editor text"
    );
    assert_eq!(
        call(&mut app, "note_create", json!({"title":"A"}))["path"],
        "A.md"
    );
}

#[test]
fn version_retention_can_be_configured_disabled_and_reenabled_without_deleting_old_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    let service = LocalDataService::open(
        vault.to_str().unwrap(),
        &dir.path().join("data"),
        "retention",
    )
    .unwrap();
    service.snapshot("A.md", b"retained", "").unwrap();
    service
        .configure(&json!({"versionRetentionDays":0}))
        .unwrap();
    assert_eq!(
        service.snapshot("A.md", b"disabled", "").unwrap()["snapshotCreated"],
        false
    );
    assert_eq!(
        service
            .prune_versions(notebase_core::index::now_ms() + 365 * 86_400_000)
            .unwrap(),
        0
    );
    assert_eq!(
        service.versions(&json!({"path":"A.md"})).unwrap()["items"],
        json!([])
    );
    service
        .configure(&json!({"versionRetentionDays":1}))
        .unwrap();
    assert_eq!(
        service.versions(&json!({"path":"A.md"})).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        service
            .prune_versions(notebase_core::index::now_ms() + 2 * 86_400_000)
            .unwrap(),
        1
    );
}

#[test]
fn save_with_history_disabled_reports_file_success_without_fake_snapshot_success() {
    let (_dir, vault, mut app) = setup();
    call(
        &mut app,
        "settings_set",
        json!({"values":{"backup":{"versionRetentionDays":0}}}),
    );
    let note = call(&mut app, "note_read", json!({"path":"A.md"}));
    let saved = call(
        &mut app,
        "note_save",
        json!({"path":"A.md","baseHash":note["contentHash"],"body":"saved without history"}),
    );
    assert_eq!(saved["snapshotCreated"], false);
    assert_eq!(
        fs::read_to_string(vault.join("A.md")).unwrap(),
        "saved without history"
    );
    assert!(saved["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .all(|w| !w.as_str().unwrap().contains("快照失败")));
}

#[test]
fn clock_rollback_uses_monotonic_sequence_and_records_a_redacted_warning() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    let service =
        LocalDataService::open(vault.to_str().unwrap(), &dir.path().join("data"), "clock").unwrap();
    let old = service.snapshot("A.md", b"old-secret-body", "").unwrap();
    let folder = dir
        .path()
        .join("data/vaults/clock/snapshots")
        .join(digest(b"A.md"));
    let future = notebase_core::index::now_ms() + 86_400_000;
    fs::write(
        folder.join(".notebase-history-sequence"),
        serde_json::to_vec(
            &json!({"lastSequence":future,"lastObservedMs":future,"rollbackCount":0}),
        )
        .unwrap(),
    )
    .unwrap();
    let old_record = folder.join(format!("{}.json", old["versionId"].as_str().unwrap()));
    let mut meta: Value = serde_json::from_slice(&fs::read(&old_record).unwrap()).unwrap();
    meta["createdMs"] = json!(future);
    meta["sequence"] = json!(future);
    fs::write(old_record, serde_json::to_vec(&meta).unwrap()).unwrap();
    let new = service.snapshot("A.md", b"new-secret-body", "").unwrap();
    let list = service.versions(&json!({"path":"A.md"})).unwrap();
    assert_eq!(list["items"][0]["versionId"], new["versionId"]);
    assert!(list["items"][0].get("sequence").is_none());
    let state: Value =
        serde_json::from_slice(&fs::read(folder.join(".notebase-history-sequence")).unwrap())
            .unwrap();
    assert_eq!(state["rollbackCount"], 1);
    let log = fs::read_dir(dir.path().join("data/logs"))
        .unwrap()
        .map(|e| fs::read_to_string(e.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(log.contains("clock_rollback"));
    assert!(!log.contains("secret-body"));
}

#[test]
fn invalid_backup_settings_are_errors_and_legacy_settings_gain_defaults_without_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    let service = LocalDataService::open(
        vault.to_str().unwrap(),
        &dir.path().join("data"),
        "settings",
    )
    .unwrap();
    let file = dir.path().join("data/vaults/settings/backup-settings.json");
    for invalid in [
        "[]",
        r#"{"keepCount":"bad"}"#,
        r#"{"versionRetentionDays":-1}"#,
    ] {
        fs::write(&file, invalid).unwrap();
        assert_eq!(
            service
                .configure(&json!({"enabled":false}))
                .unwrap_err()
                .code,
            "E_FORMAT_UNSUPPORTED"
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), invalid);
    }
    fs::write(&file, r#"{"enabled":false,"futureKey":"preserved"}"#).unwrap();
    assert_eq!(service.settings().unwrap()["versionRetentionDays"], 30);
    service
        .configure(&json!({"versionRetentionDays":17}))
        .unwrap();
    assert_eq!(service.settings().unwrap()["futureKey"], "preserved");
}
