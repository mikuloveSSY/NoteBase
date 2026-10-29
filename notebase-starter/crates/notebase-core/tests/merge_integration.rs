use notebase_core::{
    migration::{LocalVaultLocation, VaultLocation},
    new_indexed_app,
    note_service::content_hash,
    note_state::{FsNoteStateStore, NoteStateStore},
    App, Request,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

fn response(app: &mut App, command: &str, payload: Value) -> Value {
    serde_json::to_value(app.dispatch(Request {
        id: "merge-integration".into(),
        command: command.into(),
        payload,
        client_seq: 0,
    }))
    .unwrap()
}

fn call(app: &mut App, command: &str, payload: Value) -> Value {
    let result = response(app, command, payload);
    assert_eq!(result["ok"], true, "{command}: {result}");
    result["data"].clone()
}

fn wait_index(app: &mut App, note_count: u64) -> Value {
    let started = Instant::now();
    loop {
        let status = call(app, "index_status", json!({}));
        if status["state"] == "ready" && status["noteCount"] == note_count {
            return status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "index did not catch up to {note_count} notes: {status}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn search_paths(app: &mut App, query: &str) -> Vec<String> {
    let result = call(app, "search", json!({"query":query}));
    let mut paths: Vec<_> = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["path"].as_str().unwrap().to_owned())
        .collect();
    paths.sort();
    paths
}

fn wait_search(app: &mut App, query: &str, expected: &[&str]) {
    let started = Instant::now();
    loop {
        let result = response(app, "search", json!({"query":query}));
        if result["ok"] == true {
            let mut paths: Vec<_> = result["data"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["path"].as_str().unwrap())
                .collect();
            paths.sort();
            if paths == expected {
                return;
            }
        } else {
            assert_eq!(result["error"]["code"], "E_INDEX_UNAVAILABLE", "{result}");
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "search {query:?} did not catch up to {expected:?}: {result}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn state_directory(data: &Path, opened: &Value) -> std::path::PathBuf {
    data.join("vaults")
        .join(opened["vaultId"].as_str().unwrap())
}

#[test]
fn indexed_member2_draft_save_rename_delete_restore_catches_up_without_full_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    let data = dir.path().join("app-data");
    fs::create_dir(&vault).unwrap();
    let original = "# A\n原文基线";
    let changed = "# A\n队列提交内容";
    fs::write(vault.join("A.md"), original).unwrap();
    fs::write(vault.join("B.md"), "[[A|显示]] and `[[A]]`").unwrap();
    let mut app = new_indexed_app(data, Arc::new(|_, _| {}));
    call(&mut app, "vault_open", json!({"path":vault}));
    let initial = wait_index(&mut app, 2);
    let full_build_ms = initial["lastFullBuildMs"].clone();
    assert!(full_build_ms.is_u64(), "{initial}");
    assert_eq!(search_paths(&mut app, "原文基线"), ["A.md"]);
    assert!(search_paths(&mut app, "队列提交").is_empty());

    let note = call(
        &mut app,
        "note_read",
        json!({"path":"A.md","clientId":"merge-editor"}),
    );
    let draft = call(
        &mut app,
        "note_save",
        json!({"action":"draft","path":"A.md","body":changed,"baseHash":note["contentHash"],"clientId":"merge-editor","clientSeq":1}),
    );
    assert_eq!(draft["draftSaved"], true);
    assert_eq!(fs::read_to_string(vault.join("A.md")).unwrap(), original);
    assert_eq!(search_paths(&mut app, "原文基线"), ["A.md"]);
    assert!(search_paths(&mut app, "队列提交").is_empty());
    assert_eq!(
        call(&mut app, "note_list", json!({"scope":"drafts"}))["items"][0]["body"],
        changed
    );

    let saved = call(
        &mut app,
        "note_save",
        json!({"action":"save","path":"A.md","body":changed,"baseHash":note["contentHash"],"clientId":"merge-editor","clientSeq":2}),
    );
    assert_eq!(saved["indexQueued"], true, "{saved}");
    assert_eq!(fs::read_to_string(vault.join("A.md")).unwrap(), changed);
    wait_search(&mut app, "队列提交", &["A.md"]);
    wait_search(&mut app, "原文基线", &[]);
    assert_eq!(wait_index(&mut app, 2)["lastFullBuildMs"], full_build_ms);
    assert!(
        call(&mut app, "note_list", json!({"scope":"drafts"}))["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let plan = call(
        &mut app,
        "note_rename",
        json!({"path":"A.md","newName":"C","planOnly":true}),
    );
    assert!(vault.join("A.md").exists());
    let renamed = call(
        &mut app,
        "note_rename",
        json!({"path":"A.md","newName":"C","planOnly":false,"planId":plan["plan"]["id"]}),
    );
    assert_eq!(renamed["path"], "C.md");
    assert!(!vault.join("A.md").exists());
    assert_eq!(fs::read_to_string(vault.join("C.md")).unwrap(), changed);
    assert_eq!(
        fs::read_to_string(vault.join("B.md")).unwrap(),
        "[[C|显示]] and `[[A]]`"
    );
    assert_eq!(
        response(&mut app, "note_read", json!({"path":"A.md"}))["error"]["code"],
        "E_NOT_FOUND"
    );
    wait_search(&mut app, "队列提交", &["C.md"]);
    wait_index(&mut app, 2);

    let deleted = call(&mut app, "note_delete", json!({"path":"C.md"}));
    assert_eq!(deleted["restorable"], true);
    assert!(!vault.join("C.md").exists());
    let trash_path = deleted["trashPath"].as_str().unwrap();
    assert_eq!(fs::read_to_string(vault.join(trash_path)).unwrap(), changed);
    wait_search(&mut app, "队列提交", &[]);
    wait_index(&mut app, 1);

    let restored = call(&mut app, "note_restore", json!({"trashPath":trash_path}));
    assert_eq!(restored["path"], "C.md");
    assert_eq!(fs::read_to_string(vault.join("C.md")).unwrap(), changed);
    assert!(!vault.join(trash_path).exists());
    wait_search(&mut app, "队列提交", &["C.md"]);
    assert_eq!(wait_index(&mut app, 2)["lastFullBuildMs"], full_build_ms);
}

#[test]
fn settings_migration_preserves_private_drafts_and_resumes_relative_rename_in_new_vault() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let target = dir.path().join("target");
    let data = dir.path().join("app-data");
    fs::create_dir(&source).unwrap();
    let disk_body = "磁盘原文";
    let draft_body = "迁移恢复的草稿全文";
    let source_body = "# A\n重命名恢复内容";
    let reference_before = "[[A|显示]] and `[[A]]`";
    let reference_after = "[[C|显示]] and `[[A]]`";
    fs::write(source.join("Draft.md"), disk_body).unwrap();
    fs::write(source.join("A.md"), source_body).unwrap();
    fs::write(source.join("B.md"), reference_before).unwrap();
    let mut app = new_indexed_app(data.clone(), Arc::new(|_, _| {}));
    let opened = call(&mut app, "vault_open", json!({"path":source}));
    wait_index(&mut app, 3);
    let old_state = state_directory(&data, &opened);
    assert!(!fs::canonicalize(&old_state)
        .unwrap()
        .starts_with(fs::canonicalize(&source).unwrap()));

    let note = call(
        &mut app,
        "note_read",
        json!({"path":"Draft.md","clientId":"migrating-editor"}),
    );
    let draft = call(
        &mut app,
        "note_save",
        json!({"action":"draft","path":"Draft.md","body":draft_body,"baseHash":note["contentHash"],"clientId":"migrating-editor","clientSeq":1}),
    );
    assert_eq!(draft["draftSaved"], true);
    let draft_id = draft["draftId"].as_str().unwrap();
    let draft_file = old_state.join("drafts").join(format!("{draft_id}.json"));
    let original_draft_record = fs::read(&draft_file).unwrap();

    let plan = call(
        &mut app,
        "note_rename",
        json!({"path":"A.md","newName":"C","planOnly":true}),
    );
    let journal_id = plan["plan"]["id"].as_str().unwrap();
    // Real crash fixture: the source move reached disk, but the pending backlink
    // write did not. Persist the service's relative-path journal in its real store.
    fs::rename(source.join("A.md"), source.join("C.md")).unwrap();
    let journal = json!({
        "id":journal_id,"from":"A.md","to":"C.md",
        "sourceHash":content_hash(source_body.as_bytes()),
        "edits":[{"path":"B.md","beforeHash":content_hash(reference_before.as_bytes()),
            "afterHash":content_hash(reference_after.as_bytes()),"body":reference_after}],
        "createdMs":1,"moved":true,"done":[],"state":"running"
    });
    FsNoteStateStore::open(&old_state)
        .unwrap()
        .write("renames", journal_id, &journal)
        .unwrap();
    let journal_file = old_state.join("renames").join(format!("{journal_id}.json"));
    let original_journal_record = fs::read(&journal_file).unwrap();
    assert_eq!(
        call(&mut app, "note_list", json!({"scope":"renames"}))["items"][0]["id"],
        journal_id
    );
    assert_eq!(
        fs::read_to_string(source.join("Draft.md")).unwrap(),
        disk_body
    );

    call(
        &mut app,
        "settings_set",
        json!({"values":{"vaultPath":target}}),
    );
    // Verify the active support was rebound by settings_set itself, before a reopen.
    assert_eq!(
        call(&mut app, "note_list", json!({"scope":"drafts"}))["items"][0]["id"],
        draft_id
    );
    assert_eq!(
        call(&mut app, "note_list", json!({"scope":"renames"}))["items"][0],
        journal
    );
    let location = LocalVaultLocation {
        settings_path: data.join("settings.json"),
    };
    let configured = location.configured_path().unwrap().unwrap();
    assert_eq!(
        fs::canonicalize(configured).unwrap(),
        fs::canonicalize(&target).unwrap()
    );
    assert_eq!(
        fs::canonicalize(
            call(&mut app, "vault_info", json!({}))["vaultPath"]
                .as_str()
                .unwrap()
        )
        .unwrap(),
        fs::canonicalize(&target).unwrap()
    );
    let migrated = call(&mut app, "vault_open", json!({"path":target}));
    assert_ne!(opened["vaultId"], migrated["vaultId"]);
    let new_state = state_directory(&data, &migrated);
    assert_eq!(
        fs::read(new_state.join("drafts").join(format!("{draft_id}.json"))).unwrap(),
        original_draft_record
    );
    assert_eq!(
        fs::read(new_state.join("renames").join(format!("{journal_id}.json"))).unwrap(),
        original_journal_record
    );
    assert!(!target.join("drafts").exists());
    assert!(!target.join("renames").exists());
    assert_eq!(
        fs::read_to_string(target.join("Draft.md")).unwrap(),
        disk_body
    );
    assert_eq!(
        fs::read_to_string(target.join("B.md")).unwrap(),
        reference_before
    );
    drop(app);

    let mut app = new_indexed_app(data, Arc::new(|_, _| {}));
    call(&mut app, "vault_open", json!({"path":target}));
    wait_index(&mut app, 3);
    let drafts = call(&mut app, "note_list", json!({"scope":"drafts"}));
    assert_eq!(drafts["items"].as_array().unwrap().len(), 1);
    assert_eq!(drafts["items"][0]["id"], draft_id);
    assert_eq!(drafts["items"][0]["body"], draft_body);
    let renames = call(&mut app, "note_list", json!({"scope":"renames"}));
    assert_eq!(renames["items"].as_array().unwrap().len(), 1);
    assert_eq!(renames["items"][0], journal);
    let recovered = call(
        &mut app,
        "note_restore",
        json!({"kind":"draft","draftId":draft_id,"choice":"original"}),
    );
    assert_eq!(recovered["path"], "Draft.md");
    assert_eq!(recovered["copied"], false);
    assert_eq!(
        fs::read_to_string(target.join("Draft.md")).unwrap(),
        draft_body
    );
    let resumed = call(&mut app, "note_rename", json!({"resumeId":journal_id}));
    assert_eq!(resumed["path"], "C.md");
    assert_eq!(
        fs::read_to_string(target.join("B.md")).unwrap(),
        reference_after
    );
    assert_eq!(
        fs::read_to_string(target.join("C.md")).unwrap(),
        source_body
    );
    assert!(!target.join("A.md").exists());
    assert!(
        call(&mut app, "note_list", json!({"scope":"drafts"}))["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        call(&mut app, "note_list", json!({"scope":"renames"}))["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    wait_search(&mut app, "迁移恢复", &["Draft.md"]);
    wait_search(&mut app, "重命名恢复", &["C.md"]);
    wait_index(&mut app, 3);

    // Recovery in the migrated vault must leave the original notes and state intact.
    assert_eq!(
        fs::read_to_string(source.join("Draft.md")).unwrap(),
        disk_body
    );
    assert_eq!(
        fs::read_to_string(source.join("B.md")).unwrap(),
        reference_before
    );
    assert_eq!(
        fs::read_to_string(source.join("C.md")).unwrap(),
        source_body
    );
    assert!(!source.join("A.md").exists());
    assert_eq!(fs::read(draft_file).unwrap(), original_draft_record);
    assert_eq!(fs::read(journal_file).unwrap(), original_journal_record);
}
