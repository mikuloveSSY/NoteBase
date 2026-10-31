use notebase_core::{
    data_services::{DataService, LocalDataService},
    new_indexed_app, App, Request, Response,
};
use serde_json::{json, Value};
use std::{
    fs,
    sync::{Arc, Barrier, Mutex},
    time::{Duration, Instant},
};

fn call(app: &mut App, command: &str, payload: Value) -> Value {
    match app.dispatch(Request {
        id: "team-all".into(),
        command: command.into(),
        payload,
        client_seq: 1,
    }) {
        Response::Ok { data, .. } => data,
        Response::Err { error, .. } => panic!("{command}: {error:?}"),
    }
}
fn wait_ready(app: &mut App) {
    let start = Instant::now();
    while call(app, "index_status", json!({}))["state"] != "ready" {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(10));
    }
}
struct BlockingData {
    inner: LocalDataService,
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
}
macro_rules! forward {
    ($($name:ident($($arg:ident:$ty:ty),*) -> $out:ty;)*) => {$(fn $name(&self,$($arg:$ty),*) -> notebase_core::domain::Result<$out> { self.inner.$name($($arg),*) })*};
}
impl DataService for BlockingData {
    fn export(&self, _: &Value) -> notebase_core::domain::Result<Value> {
        self.entered.wait();
        self.release.wait();
        Ok(json!({"html":"fixture"}))
    }
    forward! {
        backup_create(p:&Value)->Value; backup_restore(p:&Value)->Value;
        versions(p:&Value)->Value; version_body(p:&Value)->String;
        snapshot(path:&str,bytes:&[u8],note:&str)->Value;
        settings()->Value; configure(p:&Value)->Value; maintenance(now:u64)->Value;
        maintenance_due(now:u64)->bool; prune_versions(now:u64)->usize;
        sync_configure(p:&Value)->Value; sync_now(p:&Value)->Value; sync_status()->Value;
    }
}
#[test]
fn long_job_releases_app_lock_rejects_writes_and_always_restores_service() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("A.md"), "original").unwrap();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let (e, r) = (entered.clone(), release.clone());
    let root = dir.path().join("data");
    let data = root.clone();
    let mut app =
        new_indexed_app(root, Arc::new(|_, _| {})).with_data(Box::new(move |vault, id| {
            Ok(Box::new(BlockingData {
                inner: LocalDataService::open(vault, &data, id)?,
                entered: e.clone(),
                release: r.clone(),
            }))
        }));
    call(&mut app, "vault_open", json!({"path":vault}));
    wait_ready(&mut app);
    let mut task = app
        .begin_data_task("export_note", &json!({"path":"A.md","format":"html"}))
        .unwrap();
    let app = Arc::new(Mutex::new(app));
    let shared = app.clone();
    let worker = std::thread::spawn(move || {
        let result = task.run();
        shared.lock().unwrap().finish_data_task(task, result)
    });
    entered.wait();
    {
        let mut app = app
            .try_lock()
            .expect("network/print wait must not hold application lock");
        assert!(app.data_task_active());
        let note = call(&mut app, "note_read", json!({"path":"A.md"}));
        assert_eq!(note["body"], "original");
        let blocked = app.dispatch(Request {
            id: "blocked".into(),
            command: "vault_open".into(),
            payload: json!({"path":vault}),
            client_seq: 2,
        });
        assert!(matches!(blocked,Response::Err{error,..} if error.code=="E_CANCELLED"));
        assert!(app.begin_data_task("sync_now", &json!({})).is_err());
        call(
            &mut app,
            "note_save",
            json!({"action":"draft","path":"A.md","body":"pending draft","baseHash":note["contentHash"]}),
        );
    }
    release.wait();
    assert!(worker.join().unwrap().is_ok());
    let mut core = app.lock().unwrap();
    assert!(!core.data_task_active());
    assert_eq!(
        call(&mut core, "settings_get", json!({"keys":["backup"]}))["values"]["backup"]
            ["keepCount"],
        30
    );
    assert_eq!(fs::read_to_string(vault.join("A.md")).unwrap(), "original");
    let mut preview = core
        .begin_data_task(
            "export_note",
            &json!({"path":"A.md","format":"html","preview":true}),
        )
        .unwrap();
    drop(core);
    let shared = app.clone();
    let worker = std::thread::spawn(move || {
        let result = preview.run();
        shared.lock().unwrap().finish_data_task(preview, result)
    });
    entered.wait();
    let other = dir.path().join("other");
    fs::create_dir(&other).unwrap();
    call(
        &mut app.lock().unwrap(),
        "vault_open",
        json!({"path":other}),
    );
    release.wait();
    assert_eq!(worker.join().unwrap().unwrap_err().code, "E_CANCELLED");
}

#[test]
fn failed_restore_releases_maintenance_and_reopens_index_without_touching_notes() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("A.md"), "keep original").unwrap();
    let corrupt = dir.path().join("bad.zip");
    fs::write(&corrupt, "not a zip").unwrap();
    let mut app = new_indexed_app(dir.path().join("data"), Arc::new(|_, _| {}));
    call(&mut app, "vault_open", json!({"path":vault}));
    wait_ready(&mut app);
    let mut task = app
        .begin_data_task(
            "backup_restore",
            &json!({"filePath":corrupt,"mode":"original"}),
        )
        .unwrap();
    let result = task.run();
    assert!(result.is_err());
    assert!(app.finish_data_task(task, result).is_err());
    assert!(!app.data_task_active());
    wait_ready(&mut app);
    assert_eq!(
        fs::read_to_string(vault.join("A.md")).unwrap(),
        "keep original"
    );
    assert!(call(
        &mut app,
        "backup_create",
        json!({"scope":"note","path":"A.md"})
    )["snapshotCreated"]
        .as_bool()
        .unwrap());
    assert_eq!(
        call(&mut app, "search", json!({"query":"original"}))["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let mut task = app.begin_data_task("backup_create", &json!({})).unwrap();
    app.request_data_task_cancel();
    assert!(app.data_task_active());
    let result = task.run();
    assert_eq!(result.as_ref().unwrap_err().code, "E_CANCELLED");
    assert!(app.finish_data_task(task, result).is_err());
    assert!(!app.data_task_active());
    wait_ready(&mut app);
}

#[test]
fn deduplicated_version_keeps_description_creation_time_and_renews_retention() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    let service =
        LocalDataService::open(vault.to_str().unwrap(), &dir.path().join("data"), "repeat")
            .unwrap();
    let snapshot = service
        .snapshot("A.md", b"reused", "manual description")
        .unwrap();
    let record = dir
        .path()
        .join("data/vaults/repeat/snapshots")
        .join(notebase_core::data_services::files::digest(b"A.md"))
        .join(format!("{}.json", snapshot["versionId"].as_str().unwrap()));
    let mut meta: Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    meta["createdMs"] = json!(1);
    meta["lastUsedMs"] = json!(1);
    fs::write(&record, serde_json::to_vec(&meta).unwrap()).unwrap();
    service.snapshot("A.md", b"reused", "").unwrap();
    let listed = service.versions(&json!({"path":"A.md"})).unwrap();
    assert_eq!(listed["items"][0]["description"], "manual description");
    assert_eq!(listed["items"][0]["createdMs"], 1);
    assert!(listed["items"][0].get("lastUsedMs").is_none());
    let now = notebase_core::index::now_ms();
    assert_eq!(service.prune_versions(now + 86_400_000).unwrap(), 0);
    assert_eq!(service.prune_versions(now + 31 * 86_400_000).unwrap(), 1);
}

#[test]
fn backup_and_original_restore_stop_old_arc_restart_search_and_clear_old_baselines() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("A.md"), "restore original").unwrap();
    let mut app = new_indexed_app(dir.path().join("data"), Arc::new(|_, _| {}));
    call(&mut app, "vault_open", json!({"path":vault}));
    wait_ready(&mut app);
    let old = app.search_session().unwrap();
    let mut task = app.begin_data_task("backup_create", &json!({})).unwrap();
    let result = task.run();
    assert!(old.search(&json!({"query":"original"})).is_err());
    let archive = app.finish_data_task(task, result).unwrap();
    wait_ready(&mut app);
    fs::write(vault.join("A.md"), "before restore").unwrap();
    call(
        &mut app,
        "note_read",
        json!({"path":"A.md","clientId":"old-window"}),
    );
    let mut task = app
        .begin_data_task(
            "backup_restore",
            &json!({"filePath":archive["filePath"],"mode":"original"}),
        )
        .unwrap();
    let result = task.run();
    app.finish_data_task(task, result).unwrap();
    wait_ready(&mut app);
    assert_eq!(
        call(&mut app, "note_read", json!({"path":"A.md"}))["body"],
        "restore original"
    );
    assert_eq!(
        call(&mut app, "search", json!({"query":"original"}))["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        call(&mut app, "note_list", json!({"scope":"drafts"}))["items"],
        json!([])
    );
}

#[test]
fn mixed_settings_and_migration_preserve_backup_history_and_sync_references() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("A.md"), "history").unwrap();
    let data = dir.path().join("data");
    let mut app = new_indexed_app(data.clone(), Arc::new(|_, _| {}));
    let opened = call(&mut app, "vault_open", json!({"path":vault}));
    wait_ready(&mut app);
    call(&mut app, "settings_set", json!({"values":{"theme":"dark"}}));
    call(
        &mut app,
        "settings_set",
        json!({"values":{"backup":{"keepCount":17,"enabled":false}}}),
    );
    call(
        &mut app,
        "backup_create",
        json!({"scope":"note","path":"A.md"}),
    );
    let private = data
        .join("vaults")
        .join(opened["vaultId"].as_str().unwrap());
    fs::write(
        private.join("sync-config.json"),
        r#"{"endpoint":"https://fixture.invalid/","credentialRef":"fixture-reference"}"#,
    )
    .unwrap();
    fs::write(
        private.join("sync-state.json"),
        r#"{"fixture":"baseline preserved"}"#,
    )
    .unwrap();
    let before = fs::read(data.join("settings.json")).unwrap();
    let rejected = app.dispatch(Request {
        id: "mixed".into(),
        command: "settings_set".into(),
        payload: json!({"values":{"theme":"light","backup":{"keepCount":1}}}),
        client_seq: 1,
    });
    assert!(matches!(rejected,Response::Err{error,..} if error.code=="E_PATH_INVALID"));
    assert_eq!(fs::read(data.join("settings.json")).unwrap(), before);
    let target = dir.path().join("migrated");
    call(
        &mut app,
        "settings_set",
        json!({"values":{"vaultPath":target}}),
    );
    wait_ready(&mut app);
    let values = call(
        &mut app,
        "settings_get",
        json!({"keys":["theme","backup","vaultPath"]}),
    )["values"]
        .clone();
    assert_eq!(values["theme"], "dark");
    assert_eq!(values["backup"]["keepCount"], 17);
    assert_eq!(
        call(
            &mut app,
            "note_list",
            json!({"scope":"versions","path":"A.md"})
        )["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let reopened = call(&mut app, "vault_open", json!({"path":target}));
    let new_private = data
        .join("vaults")
        .join(reopened["vaultId"].as_str().unwrap());
    for file in ["sync-config.json", "sync-state.json"] {
        assert_eq!(
            fs::read(private.join(file)).unwrap(),
            fs::read(new_private.join(file)).unwrap()
        );
    }
}
