use notebase_core::{
    logger::Logger,
    settings::{FileSettings, SettingsStore},
    App, Request, Response,
};
use serde_json::{json, Value};
use std::{
    fs,
    sync::Arc,
    time::{Duration, Instant},
};
fn call(app: &mut App, command: &str, payload: Value) -> Value {
    match app.dispatch(Request {
        id: "member4".into(),
        command: command.into(),
        payload,
        client_seq: 1,
    }) {
        Response::Ok { data, .. } => data,
        Response::Err { error, .. } => panic!("{}: {}", error.code, error.message),
    }
}
#[test]
fn preferences_preserve_vault_and_other_members_keys_and_reject_conflicts_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(
        &path,
        r#"{"vaultPath":"D:/notes","sync":{"enabled":false}}"#,
    )
    .unwrap();
    let store = FileSettings(path.clone());
    store
        .update(&json!({"theme":"dark","editor":{"fontSize":18}}))
        .unwrap();
    let before = fs::read(&path).unwrap();
    assert_eq!(store.read().unwrap()["sync"]["enabled"], false);
    assert_eq!(store.read().unwrap()["vaultPath"], "D:/notes");
    assert_eq!(
        store
            .update(&json!({"keymap":{"new":"Shift+Ctrl+P"}}))
            .unwrap_err()
            .code,
        "E_KEYMAP_CONFLICT"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}
#[test]
fn malformed_settings_are_retained_without_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    fs::write(&path, b"{broken").unwrap();
    assert!(FileSettings(path.clone())
        .update(&json!({"theme":"dark"}))
        .is_err());
    assert_eq!(fs::read(path).unwrap(), b"{broken");
}
#[test]
fn logs_are_bounded_redacted_and_expire_only_owned_files() {
    let dir = tempfile::tempdir().unwrap();
    let logger = Logger::new(dir.path().into());
    let day = 86_400_000;
    fs::write(dir.path().join("keep.txt"), "unrelated").unwrap();
    logger
        .record_at(
            day,
            "private content",
            "secret note body",
            Some("secret token"),
        )
        .unwrap();
    let first = fs::read_to_string(dir.path().join("notebase-1.jsonl")).unwrap();
    assert!(!first.contains("secret"));
    assert!(!first.contains("private"));
    fs::write(dir.path().join("notebase-8.jsonl"), vec![b' '; 1024 * 1024]).unwrap();
    logger
        .record_at(
            8 * day,
            "app-shell",
            "note_save",
            Some("E_EXTERNAL_MODIFIED"),
        )
        .unwrap();
    assert!(!dir.path().join("notebase-1.jsonl").exists());
    assert!(dir.path().join("keep.txt").exists());
    assert!(dir.path().join("notebase-8.previous.jsonl").exists());
}
#[test]
fn integrated_member_two_save_is_queued_and_searchable_and_reopen_preserves_clients() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("A.md"), "初始正文").unwrap();
    let mut app = notebase_core::new_indexed_app(dir.path().join("data"), Arc::new(|_, _| {}));
    call(&mut app, "vault_open", json!({"path":vault}));
    let a = call(
        &mut app,
        "note_read",
        json!({"path":"A.md","clientId":"one"}),
    );
    call(&mut app, "vault_open", json!({"path":vault}));
    call(
        &mut app,
        "note_read",
        json!({"path":"A.md","clientId":"two"}),
    );
    let saved = call(
        &mut app,
        "note_save",
        json!({"path":"A.md","body":"成员四集成关键词","baseHash":a["contentHash"],"clientId":"one","clientSeq":1}),
    );
    assert_eq!(saved["indexQueued"], true);
    let start = Instant::now();
    loop {
        if let Response::Ok { data, .. } = app.dispatch(Request {
            id: "search".into(),
            command: "search".into(),
            payload: json!({"query":"集成关键词"}),
            client_seq: 0,
        }) {
            if data["items"].as_array().is_some_and(|v| v.len() == 1) {
                break;
            }
        }
        assert!(start.elapsed() < Duration::from_secs(8));
        std::thread::sleep(Duration::from_millis(20));
    }
    let stale=app.dispatch(Request{id:"stale".into(),command:"note_save".into(),payload:json!({"path":"A.md","body":"覆盖","baseHash":a["contentHash"],"clientId":"two","clientSeq":1}),client_seq:1});
    assert!(matches!(stale, Response::Err { .. }));
    assert_eq!(
        fs::read_to_string(vault.join("A.md")).unwrap(),
        "成员四集成关键词"
    );
}
