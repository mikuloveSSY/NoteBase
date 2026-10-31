//! Faults after an actual file mutation must remain recoverable at the App boundary.
use notebase_core::{
    data_services::{
        sync::{CredentialStore, SyncBackend},
        DataService, LocalDataService,
    },
    domain::{AppError, Result},
    new_indexed_app, App, Request, Response,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

fn response(app: &mut App, command: &str, payload: Value) -> Response {
    app.dispatch(Request {
        id: "release-safety".into(),
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
fn wait_search(app: &mut App, query: &str, expected: &[&str]) {
    let start = Instant::now();
    loop {
        if let Response::Ok { data, .. } = response(app, "search", json!({"query":query})) {
            let mut paths: Vec<_> = data["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["path"].as_str().unwrap())
                .collect();
            paths.sort();
            if paths == expected {
                return;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "search did not converge: {query}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
struct FixtureCredentials;
impl CredentialStore for FixtureCredentials {
    fn set(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
    fn get(&self, _: &str) -> Result<String> {
        Ok("fixture".into())
    }
}
struct FaultRemote {
    // 0 = fail on B, 1 = cancel on B, 2 = healthy.
    mode: AtomicUsize,
    changed: AtomicBool,
    cancelled: Mutex<Arc<AtomicBool>>,
}
impl SyncBackend for FaultRemote {
    fn list(&self) -> Result<BTreeMap<String, String>> {
        Ok([("A.md".into(), "a".into()), ("B.md".into(), "b".into())].into())
    }
    fn get(&self, path: &str, _: &str) -> Result<Vec<u8>> {
        if path == "B.md" {
            match self.mode.load(Ordering::Acquire) {
                0 => {
                    return Err(AppError::new(
                        "E_SYNC_NETWORK",
                        "fixture fails after A was downloaded",
                    ))
                }
                1 => self
                    .cancelled
                    .lock()
                    .unwrap()
                    .store(true, Ordering::Release),
                _ => {}
            }
        }
        Ok(if path == "A.md" {
            if self.changed.load(Ordering::Acquire) {
                b"downloaded A.md".to_vec()
            } else {
                b"baseline".to_vec()
            }
        } else {
            b"stable B.md".to_vec()
        })
    }
    fn put(&self, _: &str, _: &[u8], _: Option<&str>) -> Result<()> {
        panic!("no upload expected")
    }
    fn delete(&self, _: &str, _: &str) -> Result<()> {
        panic!("no delete expected")
    }
}
struct FaultData {
    inner: LocalDataService,
    remote: Arc<FaultRemote>,
}
macro_rules! forward {
    ($($name:ident($($arg:ident:$ty:ty),*) -> $out:ty;)*) => {$(fn $name(&self,$($arg:$ty),*) -> Result<$out> { self.inner.$name($($arg),*) })*};
}
impl DataService for FaultData {
    fn set_cancel_flag(&mut self, flag: Arc<AtomicBool>) {
        self.inner.set_cancel_flag(flag.clone());
        *self.remote.cancelled.lock().unwrap() = flag;
    }
    fn sync_now(&self, p: &Value) -> Result<Value> {
        self.inner.sync_with(self.remote.as_ref(), p)
    }
    forward! {
        export(p:&Value)->Value; backup_create(p:&Value)->Value; backup_restore(p:&Value)->Value;
        versions(p:&Value)->Value; version_body(p:&Value)->String; snapshot(path:&str,bytes:&[u8],note:&str)->Value;
        settings()->Value; configure(p:&Value)->Value; maintenance(now:u64)->Value;
        maintenance_due(now:u64)->bool; prune_versions(now:u64)->usize;
        sync_configure(p:&Value)->Value; sync_status()->Value;
    }
}

fn partial_sync(mode: usize, error_code: &str) {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("A.md"), "baseline").unwrap();
    let remote = Arc::new(FaultRemote {
        mode: AtomicUsize::new(2),
        changed: AtomicBool::new(false),
        cancelled: Mutex::new(Arc::new(AtomicBool::new(false))),
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let event_log = events.clone();
    let data_root = dir.path().join("data");
    let root = data_root.clone();
    let transport = remote.clone();
    let mut app = new_indexed_app(data_root, Arc::new(move |name,payload| event_log.lock().unwrap().push((name.to_owned(),payload))))
        .with_data(Box::new(move |vault,id| {
            let inner = LocalDataService::open(vault, &root, id)?;
            inner.sync_configure_with(&json!({"backend":"webdav","endpoint":"https://fixture.invalid/","username":"fixture","password":"fixture"}), &FixtureCredentials)?;
            Ok(Box::new(FaultData { inner, remote: transport.clone() }))
        }));
    call(&mut app, "vault_open", json!({"path":vault}));
    // Establish a real three-way baseline without reading any OS credential.
    let mut task = app.begin_data_task("sync_now", &json!({})).unwrap();
    let result = task.run();
    app.finish_data_task(task, result).unwrap();
    let base = call(
        &mut app,
        "note_read",
        json!({"path":"A.md","clientId":"editing"}),
    );
    call(
        &mut app,
        "note_save",
        json!({"action":"draft","path":"A.md","body":"unsaved local text","baseHash":base["contentHash"],"clientId":"editing"}),
    );
    wait_search(&mut app, "baseline", &["A.md"]);
    remote.changed.store(true, Ordering::Release);
    remote.mode.store(mode, Ordering::Release);
    events.lock().unwrap().clear();
    let mut task = app.begin_data_task("sync_now", &json!({})).unwrap();
    let result = task.run();
    assert_eq!(result.as_ref().unwrap_err().code, error_code);
    assert!(app.data_task_active());
    assert_eq!(
        app.finish_data_task(task, result).unwrap_err().code,
        error_code
    );
    assert!(!app.data_task_active());
    wait_search(&mut app, "downloaded", &["A.md"]);
    wait_search(&mut app, "baseline", &[]);
    assert_eq!(
        fs::read_to_string(vault.join("A.md")).unwrap(),
        "downloaded A.md"
    );
    assert_eq!(
        call(&mut app, "note_list", json!({"scope":"drafts"}))["items"][0]["body"],
        "unsaved local text"
    );
    assert!(
        matches!(response(&mut app,"note_save",json!({"path":"A.md","body":"unsaved local text","baseHash":base["contentHash"],"clientId":"editing","clientSeq":3})), Response::Err{error,..} if error.code == "E_EXTERNAL_MODIFIED")
    );
    let log = events.lock().unwrap();
    assert!(log.iter().any(|(name, p)| name == "note.changed"
        && p["kind"] == "sync"
        && p["source"] == "external"));
    assert!(log
        .iter()
        .any(|(name, p)| name == "sync.state" && p["lastError"] == error_code));
    drop(log);
    remote.mode.store(2, Ordering::Release);
    let mut task = app.begin_data_task("sync_now", &json!({})).unwrap();
    let result = task.run();
    assert_eq!(
        app.finish_data_task(task, result).unwrap()["state"],
        "clean"
    );
    assert_eq!(call(&mut app, "sync_status", json!({}))["pending"], 0);
}
#[test]
fn partial_sync_network_failure_reconciles_search_and_preserves_draft_then_retries() {
    partial_sync(0, "E_SYNC_NETWORK");
}
#[test]
fn partial_sync_cancel_reconciles_search_and_preserves_draft_then_retries() {
    partial_sync(1, "E_CANCELLED");
}

#[test]
fn failed_migration_settings_commit_restarts_source_search_and_allows_next_save() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("A.md"), "original").unwrap();
    let data = dir.path().join("data");
    let mut app = new_indexed_app(data.clone(), Arc::new(|_, _| {}));
    call(&mut app, "vault_open", json!({"path":source}));
    wait_search(&mut app, "original", &["A.md"]);
    let old_session = app.search_session().unwrap();
    fs::write(data.join("settings.json"), "invalid settings preserved").unwrap();
    let target = dir.path().join("target");
    assert!(matches!(
        response(
            &mut app,
            "settings_set",
            json!({"values":{"vaultPath":target}})
        ),
        Response::Err { .. }
    ));
    assert!(old_session.search(&json!({"query":"original"})).is_err());
    wait_search(&mut app, "original", &["A.md"]);
    let note = call(&mut app, "note_read", json!({"path":"A.md"}));
    call(
        &mut app,
        "note_save",
        json!({"path":"A.md","body":"afterfailure","baseHash":note["contentHash"]}),
    );
    wait_search(&mut app, "afterfailure", &["A.md"]);
    assert_eq!(
        fs::read_to_string(source.join("A.md")).unwrap(),
        "afterfailure"
    );
    assert_eq!(fs::read_to_string(target.join("A.md")).unwrap(), "original");
    assert_eq!(
        fs::read_to_string(data.join("settings.json")).unwrap(),
        "invalid settings preserved"
    );
}
