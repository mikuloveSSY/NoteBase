use notebase_core::{new_local_app, App, Request};
use serde_json::{json, Value};
use std::fs;
fn call(app: &mut App, command: &str, payload: Value) -> Value {
    serde_json::to_value(app.dispatch(Request {
        id: "member2".into(),
        command: command.into(),
        payload,
        client_seq: 0,
    }))
    .unwrap()
}
fn setup() -> (tempfile::TempDir, App) {
    let d = tempfile::tempdir().unwrap();
    let mut app = new_local_app();
    assert_eq!(
        call(&mut app, "vault_open", json!({"path":d.path()}))["ok"],
        true
    );
    (d, app)
}
#[test]
fn draft_acknowledgement_does_not_overwrite_authoritative_note() {
    let (d, mut a) = setup();
    fs::write(d.path().join("A.md"), "disk").unwrap();
    let n = call(&mut a, "note_read", json!({"path":"A.md","clientId":"one"}));
    let r = call(
        &mut a,
        "note_save",
        json!({"path":"A.md","body":"unsaved","baseHash":n["data"]["contentHash"],"action":"draft","clientId":"one","clientSeq":2}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(fs::read_to_string(d.path().join("A.md")).unwrap(), "disk");
    let drafts = call(&mut a, "note_list", json!({"scope":"drafts"}));
    assert_eq!(drafts["data"]["items"][0]["body"], "unsaved");
}
#[test]
fn soft_delete_restores_without_overwriting_a_new_same_name_note() {
    let (d, mut a) = setup();
    fs::write(d.path().join("A.md"), "original").unwrap();
    let r = call(&mut a, "note_delete", json!({"path":"A.md"}));
    assert_eq!(r["ok"], true, "{r}");
    assert!(!d.path().join("A.md").exists());
    fs::write(d.path().join("A.md"), "new").unwrap();
    let restored = call(
        &mut a,
        "note_restore",
        json!({"trashPath":r["data"]["trashPath"]}),
    );
    assert_eq!(restored["ok"], true, "{restored}");
    assert_eq!(
        fs::read_to_string(d.path().join(restored["data"]["path"].as_str().unwrap())).unwrap(),
        "original"
    );
    assert_eq!(fs::read_to_string(d.path().join("A.md")).unwrap(), "new");
}
#[test]
fn metadata_patch_retains_body_and_custom_fields() {
    let (d, mut a) = setup();
    fs::write(
        d.path().join("A.md"),
        "---\ncustom: keep\n---\n# unchanged\n",
    )
    .unwrap();
    let n = call(&mut a, "note_read", json!({"path":"A.md"}));
    let r = call(
        &mut a,
        "note_save",
        json!({"path":"A.md","baseHash":n["data"]["contentHash"],"action":"metadata","updates":{"tags":["课程/算法"],"favorite":true}}),
    );
    assert_eq!(r["ok"], true, "{r}");
    let n = call(&mut a, "note_read", json!({"path":"A.md"}));
    assert_eq!(n["data"]["frontmatter"]["custom"], "keep");
    assert_eq!(n["data"]["frontmatter"]["tags"][0], "课程/算法");
    assert!(n["data"]["body"]
        .as_str()
        .unwrap()
        .ends_with("# unchanged\n"));
}
#[test]
fn rename_preview_is_readonly_and_execution_rewrites_only_real_links() {
    let (d, mut a) = setup();
    fs::write(d.path().join("A.md"), "# A").unwrap();
    fs::write(d.path().join("B.md"), "[[A|显示]] and `[[A]]`").unwrap();
    let plan = call(
        &mut a,
        "note_rename",
        json!({"path":"A.md","newName":"C","planOnly":true}),
    );
    assert_eq!(plan["ok"], true, "{plan}");
    assert!(d.path().join("A.md").exists());
    assert_eq!(
        fs::read_to_string(d.path().join("B.md")).unwrap(),
        "[[A|显示]] and `[[A]]`"
    );
    let r = call(
        &mut a,
        "note_rename",
        json!({"path":"A.md","newName":"C","planOnly":false,"planId":plan["data"]["plan"]["id"]}),
    );
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(
        fs::read_to_string(d.path().join("B.md")).unwrap(),
        "[[C|显示]] and `[[A]]`"
    );
    assert!(d.path().join("C.md").exists());
}
#[test]
fn backlinks_and_mentions_are_real_current_file_data() {
    let (d, mut a) = setup();
    fs::write(d.path().join("A.md"), "# A").unwrap();
    fs::write(d.path().join("B.md"), "[[A]] and A").unwrap();
    let b = call(&mut a, "link_backlinks", json!({"path":"A.md"}));
    assert_eq!(b["ok"], true, "{b}");
    assert_eq!(b["data"]["items"].as_array().unwrap().len(), 1);
    let m = call(&mut a, "link_mentions", json!({"path":"A.md"}));
    assert_eq!(m["ok"], true, "{m}");
    assert!(!m["data"]["items"].as_array().unwrap().is_empty());
}
