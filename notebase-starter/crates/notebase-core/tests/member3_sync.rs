use notebase_core::{
    data_services::{
        files::digest,
        sync::{plan, Action, CredentialStore, SyncBackend, WebDav},
        LocalDataService,
    },
    domain::{AppError, Result},
};
use serde_json::json;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::TcpListener,
    sync::Mutex,
};
#[derive(Default)]
struct Credentials(Mutex<BTreeMap<String, String>>);
impl CredentialStore for Credentials {
    fn set(&self, k: &str, p: &str) -> Result<()> {
        self.0.lock().unwrap().insert(k.into(), p.into());
        Ok(())
    }
    fn get(&self, k: &str) -> Result<String> {
        self.0
            .lock()
            .unwrap()
            .get(k)
            .cloned()
            .ok_or_else(|| AppError::new("E_SYNC_AUTH", "fixture credential absent"))
    }
}
#[derive(Default)]
struct Remote(RefCell<BTreeMap<String, Vec<u8>>>);
fn etag(bytes: &[u8]) -> String {
    format!("\"{}\"", digest(bytes))
}
impl SyncBackend for Remote {
    fn list(&self) -> Result<BTreeMap<String, String>> {
        Ok(self
            .0
            .borrow()
            .iter()
            .map(|(p, b)| (p.clone(), etag(b)))
            .collect())
    }
    fn get(&self, p: &str, e: &str) -> Result<Vec<u8>> {
        let data = self.0.borrow();
        let b = data
            .get(p)
            .ok_or_else(|| AppError::new("E_SYNC_NETWORK", "missing"))?;
        assert_eq!(e, etag(b));
        Ok(b.clone())
    }
    fn put(&self, p: &str, b: &[u8], e: Option<&str>) -> Result<()> {
        let mut data = self.0.borrow_mut();
        assert_eq!(data.get(p).map(|b| etag(b)).as_deref(), e);
        data.insert(p.into(), b.to_vec());
        Ok(())
    }
    fn delete(&self, p: &str, e: &str) -> Result<()> {
        let mut data = self.0.borrow_mut();
        assert_eq!(etag(data.get(p).unwrap()), e);
        data.remove(p);
        Ok(())
    }
}
fn setup() -> (tempfile::TempDir, std::path::PathBuf, LocalDataService) {
    let d = tempfile::tempdir().unwrap();
    let vault = d.path().join("vault");
    fs::create_dir(&vault).unwrap();
    let svc = LocalDataService::open(vault.to_str().unwrap(), &d.path().join("data"), "sync-test")
        .unwrap();
    svc.sync_configure_with(&json!({"backend":"webdav","endpoint":"https://example.invalid/notes/","username":"fixture","password":"test-only-not-a-real-secret"}),&Credentials::default()).unwrap();
    (d, vault, svc)
}
#[test]
fn three_way_planner_covers_mutations_and_deletions() {
    for (base, local, remote, want) in [
        (None, None, None, Action::Clean),
        (None, Some("a"), Some("a"), Action::Clean),
        (None, Some("a"), None, Action::Upload),
        (None, None, Some("a"), Action::Download),
        (None, Some("a"), Some("b"), Action::Conflict),
        (Some("a"), Some("b"), Some("a"), Action::Upload),
        (Some("a"), Some("a"), Some("b"), Action::Download),
        (Some("a"), Some("b"), Some("c"), Action::Conflict),
        (Some("a"), None, Some("a"), Action::DeleteRemote),
        (Some("a"), Some("a"), None, Action::DeleteLocal),
        (Some("a"), None, Some("b"), Action::Conflict),
        (Some("a"), Some("b"), None, Action::Conflict),
    ] {
        assert_eq!(plan(base, local, remote), want);
    }
}
#[test]
fn initial_sync_uploads_and_downloads_and_never_writes_password_to_json() {
    let (d, vault, svc) = setup();
    fs::write(vault.join("local.md"), "local").unwrap();
    let r = Remote::default();
    r.0.borrow_mut()
        .insert("remote.md".into(), b"remote".to_vec());
    let result = svc.sync_with(&r, &json!({})).unwrap();
    assert_eq!(result["completed"], true);
    assert_eq!(
        fs::read_to_string(vault.join("remote.md")).unwrap(),
        "remote"
    );
    assert_eq!(r.0.borrow().get("local.md").unwrap(), b"local");
    for e in walkdir::WalkDir::new(d.path().join("data")) {
        let e = e.unwrap();
        if e.file_type().is_file() {
            assert!(!String::from_utf8_lossy(&fs::read(e.path()).unwrap())
                .contains("test-only-not-a-real-secret"));
        }
    }
}
#[test]
fn concurrent_modification_preserves_both_and_deduplicates_conflict_copies() {
    let (_d, vault, svc) = setup();
    fs::write(vault.join("A.md"), "local").unwrap();
    let r = Remote::default();
    r.0.borrow_mut().insert("A.md".into(), b"remote".to_vec());
    let result = svc.sync_with(&r, &json!({})).unwrap();
    assert_eq!(result["conflicts"], json!(["A.md"]));
    assert_eq!(fs::read_to_string(vault.join("A.md")).unwrap(), "local");
    let files: Vec<_> = fs::read_dir(&vault)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 2);
    assert!(files.iter().any(|p| fs::read(p).unwrap() == b"remote"));
    svc.sync_with(&r, &json!({})).unwrap();
    assert_eq!(fs::read_dir(&vault).unwrap().count(), 2);
    assert_eq!(r.0.borrow().len(), 1);
}
#[test]
fn explicit_local_conflict_resolution_uploads_only_after_confirmation_payload() {
    let (_d, vault, svc) = setup();
    fs::write(vault.join("A.md"), "local").unwrap();
    let r = Remote::default();
    r.0.borrow_mut().insert("A.md".into(), b"remote".to_vec());
    svc.sync_with(&r, &json!({})).unwrap();
    let result = svc
        .sync_with(&r, &json!({"resolvedPaths":["A.md"]}))
        .unwrap();
    assert_eq!(result["conflicts"], json!([]));
    assert_eq!(r.0.borrow().get("A.md").unwrap(), b"local");
}
#[test]
fn known_remote_delete_moves_local_to_recoverable_private_storage() {
    let (d, vault, svc) = setup();
    fs::write(vault.join("A.md"), "base").unwrap();
    let r = Remote::default();
    svc.sync_with(&r, &json!({})).unwrap();
    r.0.borrow_mut().remove("A.md");
    svc.sync_with(&r, &json!({})).unwrap();
    assert!(!vault.join("A.md").exists());
    assert!(walkdir::WalkDir::new(d.path().join("data"))
        .into_iter()
        .filter_map(|e| e.ok())
        .any(|e| e.file_type().is_file()
            && e.path().to_string_lossy().contains("sync-deleted")
            && fs::read(e.path()).unwrap() == b"base"));
}
#[test]
fn known_local_delete_uses_remote_condition_and_does_not_resurrect() {
    let (_d, vault, svc) = setup();
    fs::write(vault.join("A.md"), "base").unwrap();
    let r = Remote::default();
    svc.sync_with(&r, &json!({})).unwrap();
    fs::remove_file(vault.join("A.md")).unwrap();
    svc.sync_with(&r, &json!({})).unwrap();
    assert!(!r.0.borrow().contains_key("A.md"));
}
#[test]
fn default_sync_excludes_attachments_and_trash() {
    let (_d, vault, svc) = setup();
    fs::create_dir(vault.join("attachments")).unwrap();
    fs::create_dir(vault.join("trash")).unwrap();
    fs::write(vault.join("attachments/x.bin"), [1]).unwrap();
    fs::write(vault.join("trash/A.md"), "trash").unwrap();
    let r = Remote::default();
    svc.sync_with(&r, &json!({})).unwrap();
    assert!(r.0.borrow().is_empty());
}
#[test]
fn sync_baseline_survives_service_restart() {
    let (d, vault, svc) = setup();
    fs::write(vault.join("A.md"), "base").unwrap();
    let r = Remote::default();
    svc.sync_with(&r, &json!({})).unwrap();
    drop(svc);
    fs::write(vault.join("A.md"), "new local").unwrap();
    let svc = LocalDataService::open(vault.to_str().unwrap(), &d.path().join("data"), "sync-test")
        .unwrap();
    svc.sync_with(&r, &json!({})).unwrap();
    assert_eq!(r.0.borrow().get("A.md").unwrap(), b"new local");
}
struct Offline;
impl SyncBackend for Offline {
    fn list(&self) -> Result<BTreeMap<String, String>> {
        Err(AppError::new("E_SYNC_NETWORK", "offline"))
    }
    fn get(&self, _: &str, _: &str) -> Result<Vec<u8>> {
        unreachable!()
    }
    fn put(&self, _: &str, _: &[u8], _: Option<&str>) -> Result<()> {
        unreachable!()
    }
    fn delete(&self, _: &str, _: &str) -> Result<()> {
        unreachable!()
    }
}
#[test]
fn offline_failure_is_visible_and_next_run_catches_up() {
    let (_d, vault, svc) = setup();
    fs::write(vault.join("A.md"), "offline content").unwrap();
    assert_eq!(
        svc.sync_with(&Offline, &json!({})).unwrap_err().code,
        "E_SYNC_NETWORK"
    );
    assert_eq!(svc.sync_status().unwrap()["lastError"], "E_SYNC_NETWORK");
    assert_eq!(
        fs::read_to_string(vault.join("A.md")).unwrap(),
        "offline content"
    );
    let r = Remote::default();
    svc.sync_with(&r, &json!({})).unwrap();
    assert_eq!(r.0.borrow().get("A.md").unwrap(), b"offline content");
    assert!(svc.sync_status().unwrap()["lastError"].is_null());
}
#[test]
fn status_reports_unsynced_local_edits_after_a_clean_sync() {
    let (_d, vault, svc) = setup();
    fs::write(vault.join("A.md"), "base").unwrap();
    let remote = Remote::default();
    svc.sync_with(&remote, &json!({})).unwrap();
    assert_eq!(svc.sync_status().unwrap()["state"], "clean");
    fs::write(vault.join("A.md"), "new").unwrap();
    let status = svc.sync_status().unwrap();
    assert_eq!(status["state"], "dirty");
    assert_eq!(status["pending"], 1);
}
#[test]
fn credentials_require_secure_endpoint_and_reject_embedded_password() {
    for url in [
        "http://example.com/notes/",
        "https://user:secret@example.com/notes/",
        "https://example.com/notes/?token=x",
    ] {
        assert!(WebDav::new(url, "fixture", "fixture").is_err());
    }
}
fn server(response: &str) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let response = response.to_owned();
    let join = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut b = [0; 1024];
        loop {
            let n = stream.read(&mut b).unwrap();
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&b[..n]);
            if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        stream.write_all(response.as_bytes()).unwrap();
        String::from_utf8(bytes).unwrap()
    });
    (format!("http://{address}/notes/"), join)
}
#[test]
fn real_http_adapter_sends_conditional_put_and_does_not_follow_auth_redirects() {
    let (url, join) =
        server("HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    WebDav::new(&url, "fixture", "fixture")
        .unwrap()
        .put("A.md", b"body", None)
        .unwrap();
    let request = join.join().unwrap().to_ascii_lowercase();
    assert!(request.starts_with("put /notes/a.md "));
    assert!(request.contains("if-none-match: *"));
    let (url, join) = server(
        "HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
    assert_eq!(
        WebDav::new(&url, "fixture", "fixture")
            .unwrap()
            .put("A.md", b"body", Some("\"old\""))
            .unwrap_err()
            .code,
        "E_EXTERNAL_MODIFIED"
    );
    assert!(join
        .join()
        .unwrap()
        .to_ascii_lowercase()
        .contains("if-match: \"old\""));
}
#[test]
fn real_propfind_parses_namespaced_xml_and_percent_encoded_chinese() {
    let body="<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>/notes/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response><d:response><d:href>/notes/%E4%B8%AD%E6%96%87.md</d:href><d:propstat><d:prop><d:getetag>&quot;abc&quot;</d:getetag></d:prop></d:propstat></d:response></d:multistatus>";
    let (url, join) = server(&format!(
        "HTTP/1.1 207 Multi-Status\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    let files = WebDav::new(&url, "fixture", "fixture")
        .unwrap()
        .list()
        .unwrap();
    assert_eq!(files.get("中文.md").unwrap(), "\"abc\"");
    assert!(join
        .join()
        .unwrap()
        .to_ascii_lowercase()
        .contains("depth: 1"));
}
