use notebase_core::{new_stateful_app, App, Request};
use serde_json::{json, Value};
use std::fs;
fn call(app: &mut App, command: &str, payload: Value) -> Value {
    serde_json::to_value(app.dispatch(Request {
        id: "lifecycle".into(),
        command: command.into(),
        payload,
        client_seq: 0,
    }))
    .unwrap()
}
fn open(data: &std::path::Path, vault: &std::path::Path) -> App {
    let mut a = new_stateful_app(data.into());
    let r = call(&mut a, "vault_open", json!({"path":vault}));
    assert_eq!(r["ok"], true, "{r}");
    a
}
#[test]
fn restart_recovers_draft_as_copy_when_disk_changed() {
    let v = tempfile::tempdir().unwrap();
    let d = tempfile::tempdir().unwrap();
    fs::write(v.path().join("A.md"), "original").unwrap();
    let mut a = open(d.path(), v.path());
    let n = call(&mut a, "note_read", json!({"path":"A.md","clientId":"win"}));
    let r = call(
        &mut a,
        "note_save",
        json!({"action":"draft","path":"A.md","body":"my unsaved text","baseHash":n["data"]["contentHash"],"clientId":"win","clientSeq":7}),
    );
    assert_eq!(r["ok"], true, "{r}");
    drop(a);
    fs::write(v.path().join("A.md"), "external edit").unwrap();
    let mut b = open(d.path(), v.path());
    let drafts = call(&mut b, "note_list", json!({"scope":"drafts"}));
    assert_eq!(drafts["data"]["items"].as_array().unwrap().len(), 1);
    let r = call(
        &mut b,
        "note_restore",
        json!({"kind":"draft","draftId":drafts["data"]["items"][0]["id"]}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["data"]["copied"], true);
    assert_eq!(
        fs::read_to_string(v.path().join("A.md")).unwrap(),
        "external edit"
    );
    assert_eq!(
        fs::read_to_string(v.path().join(r["data"]["path"].as_str().unwrap())).unwrap(),
        "my unsaved text"
    );
}
#[test]
fn explicit_conflict_choice_rechecks_disk_version() {
    let v = tempfile::tempdir().unwrap();
    let d = tempfile::tempdir().unwrap();
    fs::write(v.path().join("A.md"), "old").unwrap();
    let mut a = open(d.path(), v.path());
    let n = call(&mut a, "note_read", json!({"path":"A.md"}));
    fs::write(v.path().join("A.md"), "external1").unwrap();
    let fail = call(
        &mut a,
        "note_save",
        json!({"path":"A.md","body":"mine","baseHash":n["data"]["contentHash"]}),
    );
    assert_eq!(fail["error"]["code"], "E_EXTERNAL_MODIFIED");
    let hash = fail["error"]["detail"]["diskHash"].clone();
    fs::write(v.path().join("A.md"), "external2").unwrap();
    let stale = call(
        &mut a,
        "note_save",
        json!({"path":"A.md","action":"conflict","choice":"local","body":"mine","diskHash":hash}),
    );
    assert_eq!(stale["error"]["code"], "E_EXTERNAL_MODIFIED");
    assert_eq!(
        fs::read_to_string(v.path().join("A.md")).unwrap(),
        "external2"
    );
    let now = call(&mut a, "note_read", json!({"path":"A.md","probe":true}));
    let copy = call(
        &mut a,
        "note_save",
        json!({"path":"A.md","action":"conflict","choice":"copy","body":"mine","diskHash":now["data"]["contentHash"]}),
    );
    assert_eq!(copy["ok"], true, "{copy}");
    assert_eq!(
        fs::read_to_string(v.path().join(copy["data"]["path"].as_str().unwrap())).unwrap(),
        "mine"
    );
}
#[test]
fn a_saved_revision_cannot_delete_a_newer_draft() {
    let v = tempfile::tempdir().unwrap();
    let d = tempfile::tempdir().unwrap();
    fs::write(v.path().join("A.md"), "old").unwrap();
    let mut a = open(d.path(), v.path());
    let n = call(&mut a, "note_read", json!({"path":"A.md","clientId":"w"}));
    let h = n["data"]["contentHash"].clone();
    assert_eq!(
        call(
            &mut a,
            "note_save",
            json!({"action":"draft","path":"A.md","body":"newer","baseHash":h,"clientId":"w","clientSeq":5})
        )["ok"],
        true
    );
    assert_eq!(
        call(
            &mut a,
            "note_save",
            json!({"path":"A.md","body":"older save","baseHash":h,"clientId":"w","clientSeq":4})
        )["ok"],
        true
    );
    let drafts = call(&mut a, "note_list", json!({"scope":"drafts"}));
    assert_eq!(drafts["data"]["items"][0]["body"], "newer");
}
#[test]
fn attachment_copy_is_relative_and_collision_safe() {
    let v = tempfile::tempdir().unwrap();
    let d = tempfile::tempdir().unwrap();
    fs::create_dir(v.path().join("course")).unwrap();
    fs::write(v.path().join("course/A.md"), "").unwrap();
    let mut a = open(d.path(), v.path());
    let p = json!({"action":"attachment","path":"course/A.md","name":"a b.png","bytes":[0,1,2,255],"image":true});
    let first = call(&mut a, "note_save", p.clone());
    assert_eq!(first["ok"], true, "{first}");
    assert!(first["data"]["relativePath"]
        .as_str()
        .unwrap()
        .starts_with("../attachments/"));
    assert!(first["data"]["markdown"]
        .as_str()
        .unwrap()
        .contains("a%20b.png"));
    let second = call(&mut a, "note_save", p);
    assert_eq!(second["ok"], true, "{second}");
    assert_ne!(
        first["data"]["attachmentPath"],
        second["data"]["attachmentPath"]
    );
    assert_eq!(
        fs::read(
            v.path()
                .join(first["data"]["attachmentPath"].as_str().unwrap())
        )
        .unwrap(),
        vec![0, 1, 2, 255]
    );
}
#[test]
fn invalid_metadata_patch_and_stale_rename_leave_files_intact() {
    let v = tempfile::tempdir().unwrap();
    let d = tempfile::tempdir().unwrap();
    fs::write(v.path().join("A.md"), "---\ntags: [broken\n---\ntext").unwrap();
    let mut a = open(d.path(), v.path());
    let n = call(&mut a, "note_read", json!({"path":"A.md"}));
    let r = call(
        &mut a,
        "note_save",
        json!({"path":"A.md","baseHash":n["data"]["contentHash"],"action":"metadata","updates":{"favorite":true}}),
    );
    assert_eq!(r["ok"], false);
    assert_eq!(
        fs::read_to_string(v.path().join("A.md")).unwrap(),
        "---\ntags: [broken\n---\ntext"
    );
    fs::write(v.path().join("A.md"), "# A").unwrap();
    fs::write(v.path().join("B.md"), "[[A]]").unwrap();
    let plan = call(
        &mut a,
        "note_rename",
        json!({"path":"A.md","newName":"C","planOnly":true}),
    );
    fs::write(v.path().join("B.md"), "changed [[A]]").unwrap();
    let r = call(
        &mut a,
        "note_rename",
        json!({"path":"A.md","newName":"C","planOnly":false,"planId":plan["data"]["plan"]["id"]}),
    );
    assert_eq!(r["error"]["code"], "E_STALE_CLIENT");
    assert!(v.path().join("A.md").exists());
    assert!(!v.path().join("C.md").exists());
    assert!(
        call(&mut a, "note_list", json!({"scope":"renames"}))["data"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn private_state_may_not_be_configured_inside_the_vault() {
    let v = tempfile::tempdir().unwrap();
    let mut a = new_stateful_app(v.path().join("private"));
    let r = call(&mut a, "vault_open", json!({"path":v.path()}));
    assert_eq!(r["error"]["code"], "E_PATH_INVALID");
    assert!(!v.path().join("private").exists());
}
#[test]
fn malformed_rename_state_is_rejected_without_poisoning_app() {
    let v = tempfile::tempdir().unwrap();
    let d = tempfile::tempdir().unwrap();
    fs::write(v.path().join("A.md"), "original").unwrap();
    let mut a = open(d.path(), v.path());
    let vault_data = fs::read_dir(d.path().join("vaults"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::create_dir_all(vault_data.join("renames")).unwrap();
    fs::write(
        vault_data.join("renames/bad.json"),
        r#"{"id":"bad","from":"A.md","to":"B.md","moved":false,"done":null}"#,
    )
    .unwrap();
    let r = call(&mut a, "note_rename", json!({"resumeId":"bad"}));
    assert_eq!(r["error"]["code"], "E_FORMAT_UNSUPPORTED");
    assert_eq!(
        fs::read_to_string(v.path().join("A.md")).unwrap(),
        "original"
    );
    assert!(!v.path().join("B.md").exists());
    assert_eq!(
        call(&mut a, "note_read", json!({"path":"A.md"}))["ok"],
        true
    );
}
#[test]
fn renaming_to_the_existing_name_is_a_confirmed_noop() {
    let v = tempfile::tempdir().unwrap();
    let d = tempfile::tempdir().unwrap();
    fs::write(v.path().join("A.md"), "[[A]]").unwrap();
    let mut a = open(d.path(), v.path());
    let plan = call(
        &mut a,
        "note_rename",
        json!({"path":"A.md","newName":"A","planOnly":true}),
    );
    let r = call(
        &mut a,
        "note_rename",
        json!({"path":"A.md","newName":"A","planOnly":false,"planId":plan["data"]["plan"]["id"]}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["data"]["renamed"], false);
    assert_eq!(fs::read_dir(v.path()).unwrap().count(), 1);
}
