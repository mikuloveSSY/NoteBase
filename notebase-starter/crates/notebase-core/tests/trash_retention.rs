use notebase_core::{new_stateful_app, Request};
use serde_json::{json, Value};
use std::fs;
#[test]
fn thirty_day_cleanup_uses_deletion_time_not_old_note_mtime() {
    let v = tempfile::tempdir().unwrap();
    let d = tempfile::tempdir().unwrap();
    fs::create_dir(v.path().join("trash")).unwrap();
    fs::write(
        v.path().join("trash/A (deleted 20000101-000000).md"),
        "expired",
    )
    .unwrap();
    fs::write(
        v.path().join("trash/B (deleted 29990101-000000).md"),
        "future",
    )
    .unwrap();
    fs::write(v.path().join("trash/manual.md"), "no trusted deletion time").unwrap();
    let mut a = new_stateful_app(d.path().into());
    let r = serde_json::to_value(a.dispatch(Request {
        id: "retention".into(),
        command: "vault_open".into(),
        payload: json!({"path":v.path()}),
        client_seq: 0,
    }))
    .unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert!(!v
        .path()
        .join("trash/A (deleted 20000101-000000).md")
        .exists());
    assert_eq!(
        fs::read_to_string(v.path().join("trash/B (deleted 29990101-000000).md")).unwrap(),
        "future"
    );
    assert!(v.path().join("trash/manual.md").exists());
    let list: Value = serde_json::to_value(a.dispatch(Request {
        id: "list".into(),
        command: "note_list".into(),
        payload: json!({"scope":"trash"}),
        client_seq: 0,
    }))
    .unwrap();
    assert_eq!(list["data"]["items"].as_array().unwrap().len(), 1);
}
