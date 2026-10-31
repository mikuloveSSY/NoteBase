//! Explicit opt-in: uses a disposable local WebDAV share and Windows Credential Manager.
#![cfg(windows)]
use notebase_core::data_services::{
    sync::{CredentialStore, OsCredentials, SyncBackend, WebDav},
    DataService, LocalDataService,
};
use serde_json::json;
use std::fs;

struct CredentialCleanup(String);
impl Drop for CredentialCleanup {
    fn drop(&mut self) {
        let _ =
            keyring::Entry::new("NoteBase", &self.0).and_then(|entry| entry.delete_credential());
    }
}

#[test]
#[ignore = "requires disposable NOTEBASE_ACCEPTANCE_DAV and Windows Credential Manager"]
fn independent_webdav_and_windows_credentials() {
    let endpoint =
        std::env::var("NOTEBASE_ACCEPTANCE_DAV").expect("dedicated disposable WebDAV URL required");
    assert!(
        endpoint.starts_with("http://127.0.0.1:"),
        "acceptance only permits disposable loopback service"
    );
    let password =
        std::env::var("NOTEBASE_ACCEPTANCE_PASSWORD").expect("temporary server password required");
    let temp = tempfile::tempdir().unwrap();
    let id = format!(
        "acceptance-{}-{}",
        std::process::id(),
        notebase_core::index::now_ms()
    );
    let key = format!("notebase/webdav/{id}");
    let cleanup = CredentialCleanup(key.clone());
    OsCredentials.set(&key, &password).unwrap();
    assert!(
        OsCredentials.get(&key).unwrap() == password,
        "credential round trip failed"
    );
    let vault = temp.path().join("vault");
    fs::create_dir(&vault).unwrap();
    fs::create_dir(vault.join("课程")).unwrap();
    fs::write(vault.join("课程/中文.md"), "本机上传").unwrap();
    let service =
        LocalDataService::open(vault.to_str().unwrap(), &temp.path().join("data"), &id).unwrap();
    service.sync_configure(&json!({"backend":"webdav","endpoint":endpoint,"username":"acceptance","password":password})).unwrap();
    let backend = WebDav::new(&endpoint, "acceptance", &password).unwrap();
    assert!(backend.list().unwrap().is_empty(), "server must be empty");
    assert_eq!(service.sync_now(&json!({})).unwrap()["completed"], true);
    let listed = backend.list().unwrap();
    assert!(WebDav::new(&endpoint, "acceptance", "wrong-test-password")
        .unwrap()
        .list()
        .is_err());
    assert!(backend
        .put("课程/中文.md", b"stale write", Some("\"stale-etag\""))
        .is_err());
    assert_eq!(
        backend
            .get("课程/中文.md", &listed["课程/中文.md"])
            .unwrap(),
        "本机上传".as_bytes()
    );
    backend.put("远端.md", "远端下载".as_bytes(), None).unwrap();
    service.sync_now(&json!({})).unwrap();
    assert_eq!(
        fs::read_to_string(vault.join("远端.md")).unwrap(),
        "远端下载"
    );
    let listed = backend.list().unwrap();
    backend
        .put(
            "课程/中文.md",
            "远端冲突".as_bytes(),
            Some(&listed["课程/中文.md"]),
        )
        .unwrap();
    fs::write(vault.join("课程/中文.md"), "本地冲突").unwrap();
    let result = service.sync_now(&json!({})).unwrap();
    assert!(result["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p == "课程/中文.md"));
    assert_eq!(
        fs::read_to_string(vault.join("课程/中文.md")).unwrap(),
        "本地冲突"
    );
    service
        .sync_now(&json!({"resolvedPaths":["课程/中文.md"]}))
        .unwrap();
    fs::remove_file(vault.join("远端.md")).unwrap();
    service.sync_now(&json!({})).unwrap();
    assert!(!backend.list().unwrap().contains_key("远端.md"));
    // A real refused TCP connection must not discard local edits or the baseline.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let unavailable = format!("http://{}/", listener.local_addr().unwrap());
    drop(listener);
    fs::write(vault.join("offline.md"), "断网期间编辑").unwrap();
    let offline = WebDav::new(&unavailable, "acceptance", &password).unwrap();
    assert!(service.sync_with(&offline, &json!({})).is_err());
    assert_eq!(
        fs::read_to_string(vault.join("offline.md")).unwrap(),
        "断网期间编辑"
    );
    assert_eq!(service.sync_now(&json!({})).unwrap()["completed"], true);
    assert!(backend.list().unwrap().contains_key("offline.md"));
    // A second instance reads the persisted config and real OS credential.
    let reopened =
        LocalDataService::open(vault.to_str().unwrap(), &temp.path().join("data"), &id).unwrap();
    assert_eq!(reopened.sync_now(&json!({})).unwrap()["completed"], true);
    for entry in walkdir::WalkDir::new(temp.path())
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let bytes = fs::read(entry.path()).unwrap();
        assert!(
            !bytes
                .windows(password.len())
                .any(|p| p == password.as_bytes()),
            "password leaked to application files"
        );
    }
    drop(cleanup);
    assert!(
        OsCredentials.get(&key).is_err(),
        "test credential must be deleted"
    );
}
