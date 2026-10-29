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
    assert_eq!(r["data"]["path"], "A (2).md");
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
fn saving_removes_bom_and_preserves_crlf() {
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
        "# B\r\n新内容\r\n"
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
    assert_eq!(r["ok"], true);
    assert_eq!(r["data"]["readOnly"], true);
    assert_eq!(r["data"]["encoding"], "unknown");
    assert!(r["data"]["body"].as_str().unwrap().contains('\u{fffd}'));
    let saved = call(
        &mut app,
        "note_save",
        json!({"path":"A.md","body":"replacement","baseHash":r["data"]["contentHash"]}),
    );
    assert_eq!(saved["error"]["code"], "E_ENCODING_INVALID");
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

#[test]
fn sanitized_creation_collisions_do_not_overwrite() {
    let (dir, mut app) = setup();
    for (title, expected) in [
        ("CON", "_CON.md"),
        ("COM¹", "_COM1.md"),
        ("a<b>c", "a_b_c.md"),
        ("name.", "name.md"),
        ("Ａ", "A.md"),
        ("A", "A (2).md"),
    ] {
        let created = call(&mut app, "note_create", json!({"title":title}));
        assert_eq!(created["data"]["path"], expected, "{created}");
        assert!(dir.path().join(expected).is_file());
    }
}

#[test]
fn large_read_returns_a_bounded_readonly_preview_without_a_save_hash() {
    use notebase_core::domain::{NOTE_SIZE_LIMIT, PREVIEW_SIZE_LIMIT};
    let (dir, mut app) = setup();
    let mut content = vec![b'a'; PREVIEW_SIZE_LIMIT - 1];
    content.extend_from_slice("中".as_bytes());
    content.resize(NOTE_SIZE_LIMIT + 1, b'a');
    fs::write(dir.path().join("large.md"), &content).unwrap();
    let n = read(&mut app, "large.md");
    assert_eq!(n["readOnly"], true);
    assert_eq!(n["encoding"], "utf-8");
    assert_eq!(n["sizeBytes"], content.len());
    assert_eq!(n["contentHash"], "");
    assert_eq!(n["body"].as_str().unwrap().len(), PREVIEW_SIZE_LIMIT - 1);
    let saved = call(
        &mut app,
        "note_save",
        json!({"path":"large.md","body":"short","baseHash":""}),
    );
    assert_eq!(saved["error"]["code"], "E_FILE_TOO_LARGE");
    assert_eq!(fs::read(dir.path().join("large.md")).unwrap(), content);
}

#[test]
fn exactly_ten_megabytes_is_not_truncated_and_small_saves_keep_lf() {
    use notebase_core::domain::NOTE_SIZE_LIMIT;
    let (dir, mut app) = setup();
    fs::write(dir.path().join("A.md"), vec![b'a'; NOTE_SIZE_LIMIT]).unwrap();
    let n = read(&mut app, "A.md");
    assert_eq!(n["readOnly"], false);
    assert_eq!(n["body"].as_str().unwrap().len(), NOTE_SIZE_LIMIT);
    let saved = call(
        &mut app,
        "note_save",
        json!({"path":"A.md","body":"\u{feff}# B\r\n正文\r\n","baseHash":n["contentHash"]}),
    );
    assert_eq!(saved["ok"], true);
    assert_eq!(
        fs::read_to_string(dir.path().join("A.md")).unwrap(),
        "# B\n正文\n"
    );
}

#[test]
fn readonly_file_preview_cannot_be_saved_through_ipc() {
    let (dir, mut app) = setup();
    let target = dir.path().join("A.md");
    fs::write(&target, "原文").unwrap();
    let original = fs::metadata(&target).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&target, readonly).unwrap();
    let n = read(&mut app, "A.md");
    let saved = call(
        &mut app,
        "note_save",
        json!({"path":"A.md","body":"new","baseHash":n["contentHash"]}),
    );
    fs::set_permissions(&target, original).unwrap();
    assert_eq!(n["readOnly"], true);
    assert_eq!(saved["error"]["code"], "E_IO_PERMISSION");
    assert_eq!(fs::read_to_string(target).unwrap(), "原文");
}
