use notebase_core::{new_local_app, App, Request};
use serde_json::{json, Value};
use std::fs;
fn call(app: &mut App, command: &str, payload: Value) -> Value {
    serde_json::to_value(app.dispatch(Request {
        id: "test".into(),
        command: command.into(),
        payload,
        client_seq: 1,
    }))
    .unwrap()
}
fn setup() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let mut app = new_local_app();
    let r = call(&mut app, "vault_open", json!({"path":dir.path()}));
    assert_eq!(r["ok"], true);
    (dir, app)
}
fn read(app: &mut App, path: &str) -> Value {
    call(app, "note_read", json!({"path":path}))["data"].clone()
}
#[test]
fn create_read_save_hash_and_no_temporary_files() {
    let (dir, mut app) = setup();
    assert_eq!(
        call(
            &mut app,
            "note_create",
            json!({"dir":"","title":"中文笔记"})
        )["data"]["path"],
        "中文笔记.md"
    );
    let n = read(&mut app, "中文笔记.md");
    let saved = call(
        &mut app,
        "note_save",
        json!({"path":"中文笔记.md","body":"# 内容\n","baseHash":n["contentHash"]}),
    );
    assert_eq!(saved["ok"], true);
    assert_eq!(
        fs::read_to_string(dir.path().join("中文笔记.md")).unwrap(),
        "# 内容\n"
    );
    assert_eq!(
        read(&mut app, "中文笔记.md")["contentHash"],
        saved["data"]["contentHash"]
    );
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    assert_eq!(saved["data"]["indexQueued"], false);
    assert_eq!(saved["data"]["snapshotCreated"], false);
}
#[test]
fn duplicate_create_never_overwrites() {
    let (dir, mut app) = setup();
    fs::write(dir.path().join("A.md"), "保留").unwrap();
    let r = call(&mut app, "note_create", json!({"title":"A"}));
    assert_eq!(r["data"]["path"], "A (1).md");
    assert_eq!(fs::read_to_string(dir.path().join("A.md")).unwrap(), "保留");
}
#[test]
fn rejects_traversal_and_reserved_names() {
    let (_, mut app) = setup();
    for path in [
        "../secret.md",
        "/tmp/a.md",
        "a\\b.md",
        "CON.md",
        "x/../../a.md",
    ] {
        let r = call(&mut app, "note_read", json!({"path":path}));
        assert_eq!(r["error"]["code"], "E_PATH_INVALID", "{path}");
    }
}
#[test]
fn rejects_stale_client_without_writing() {
    let (dir, mut app) = setup();
    fs::write(dir.path().join("A.md"), "原文").unwrap();
    read(&mut app, "A.md");
    let r = call(
        &mut app,
        "note_save",
        json!({"path":"A.md","body":"新文","baseHash":"old"}),
    );
    assert_eq!(r["error"]["code"], "E_STALE_CLIENT");
    assert_eq!(fs::read_to_string(dir.path().join("A.md")).unwrap(), "原文");
}
#[test]
fn external_modification_preserved() {
    let (dir, mut app) = setup();
    fs::write(dir.path().join("A.md"), "原文").unwrap();
    let n = read(&mut app, "A.md");
    fs::write(dir.path().join("A.md"), "外部写入").unwrap();
    let r = call(
        &mut app,
        "note_save",
        json!({"path":"A.md","body":"UI内容","baseHash":n["contentHash"]}),
    );
    assert_eq!(r["error"]["code"], "E_EXTERNAL_MODIFIED");
    assert_eq!(
        fs::read_to_string(dir.path().join("A.md")).unwrap(),
        "外部写入"
    );
}
#[test]
fn utf8_bom_and_crlf_preserved() {
    let (dir, mut app) = setup();
    fs::write(dir.path().join("A.md"), "\u{feff}# A\r\n正文\r\n").unwrap();
    let n = read(&mut app, "A.md");
    let r = call(
        &mut app,
        "note_save",
        json!({"path":"A.md","body":"# B\n新内容\n","baseHash":n["contentHash"]}),
    );
    assert_eq!(r["ok"], true);
    assert_eq!(
        fs::read_to_string(dir.path().join("A.md")).unwrap(),
        "\u{feff}# B\r\n新内容\r\n"
    );
}
#[test]
fn bad_yaml_remains_intact() {
    let (dir, mut app) = setup();
    let raw = "---\ntags: [broken\n---\n正文";
    fs::write(dir.path().join("A.md"), raw).unwrap();
    let n = read(&mut app, "A.md");
    assert!(n["frontmatterError"].is_string());
    assert_eq!(n["body"], raw);
}
#[test]
fn yaml_parses_without_removing_original() {
    let (dir, mut app) = setup();
    let raw = "---\ntags: [课程]\n---\n正文";
    fs::write(dir.path().join("A.md"), raw).unwrap();
    let n = read(&mut app, "A.md");
    assert_eq!(n["frontmatter"]["tags"][0], "课程");
    assert_eq!(n["body"], raw);
}
#[test]
fn unimplemented_does_not_report_success() {
    let (_, mut app) = setup();
    for c in ["search", "index_rebuild", "backup_create", "sync_now"] {
        let r = call(&mut app, c, json!({}));
        assert_eq!(r["ok"], false);
        assert_eq!(r["error"]["code"], "E_INTERNAL");
    }
}
#[test]
fn pagination_and_trash_exclusion() {
    let (dir, mut app) = setup();
    fs::create_dir(dir.path().join("trash")).unwrap();
    fs::write(dir.path().join("trash/hidden.md"), "").unwrap();
    for p in ["A.md", "B.md", "C.md"] {
        fs::write(dir.path().join(p), "").unwrap();
    }
    let a = call(&mut app, "note_list", json!({"limit":2}));
    assert_eq!(a["data"]["items"].as_array().unwrap().len(), 2);
    let b = call(
        &mut app,
        "note_list",
        json!({"limit":2,"cursor":a["data"]["nextCursor"]}),
    );
    assert_eq!(b["data"]["items"][0]["path"], "C.md");
}
#[test]
fn invalid_utf8_is_not_overwritten() {
    let (dir, mut app) = setup();
    fs::write(dir.path().join("A.md"), [255, 254, 0]).unwrap();
    let r = call(&mut app, "note_read", json!({"path":"A.md"}));
    assert_eq!(r["error"]["code"], "E_ENCODING_INVALID");
    assert_eq!(fs::read(dir.path().join("A.md")).unwrap(), [255, 254, 0]);
}
#[cfg(unix)]
#[test]
fn symlink_cannot_escape_vault() {
    let (dir, mut app) = setup();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.md"), "secret").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
    let r = call(&mut app, "note_read", json!({"path":"link/secret.md"}));
    assert_eq!(r["error"]["code"], "E_PATH_INVALID");
}
