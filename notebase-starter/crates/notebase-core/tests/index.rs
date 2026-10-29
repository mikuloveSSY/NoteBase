use notebase_core::{
    domain::{NoteStore, RelPath},
    index::{
        extract::{extract, markdown_options},
        sqlite::SqliteIndexStore,
        worker::{cache_path, BackgroundIndex, EventSink},
        IndexSession, IndexState, IndexStore, NoteRecord, BATCH_SIZE,
    },
    infra::FsStore,
};
use rusqlite::Connection;
use serde_json::Value;
use std::{
    fs,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const ID: &str = "0123456789abcdef";
fn path(p: &str) -> RelPath {
    RelPath::new(p).unwrap()
}
fn record(p: &str, body: &str) -> NoteRecord {
    NoteRecord {
        path: path(p),
        title: p.into(),
        body: body.into(),
        size_bytes: body.len() as u64,
        mtime_ms: 123,
        content_hash: "a".repeat(64),
        created_ms: None,
        updated_ms: None,
        has_attachment: false,
        aliases: vec!["别名".into()],
        tags: vec!["课程/数据库".into()],
    }
}
fn setup() -> (tempfile::TempDir, SqliteIndexStore) {
    let dir = tempfile::tempdir().unwrap();
    let index = SqliteIndexStore::open(&dir.path().join("index.db"), ID).unwrap();
    (dir, index)
}
fn count(index: &SqliteIndexStore, table: &str) -> i64 {
    index
        .read(|c| c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)))
        .unwrap()
}

#[test]
fn schema_has_ten_logical_tables_trigram_and_readonly_wal_connections() {
    let (_dir, index) = setup();
    let count=index.read(|c|c.query_row("SELECT count(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'note_fts_%'",[],|r|r.get::<_,i64>(0))).unwrap();
    assert_eq!(count, 10);
    for _ in 0..2 {
        assert_eq!(
            index
                .read(|c| c.query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0)))
                .unwrap(),
            "wal"
        );
        assert_eq!(
            index
                .read(|c| c.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0)))
                .unwrap(),
            1
        );
        assert_eq!(
            index
                .read(|c| c.query_row("PRAGMA cache_size", [], |r| r.get::<_, i64>(0)))
                .unwrap(),
            -65536
        );
        assert!(index.read(|c| c.execute("DELETE FROM meta", [])).is_err());
    }
    index
        .upsert_batch(&[record("中文.md", "数据库索引")])
        .unwrap();
    let matches = index
        .read(|c| {
            c.query_row(
                "SELECT count(*) FROM note_fts WHERE note_fts MATCH '据库索'",
                [],
                |r| r.get::<_, i64>(0),
            )
        })
        .unwrap();
    assert_eq!(matches, 1);
    let shadow: String = index
        .read(|c| c.query_row("SELECT c2 FROM note_fts_content", [], |r| r.get(0)))
        .unwrap();
    assert_eq!(shadow, "数据库索引");
    assert!(index.needs_rebuild);
}

#[test]
fn batch_failure_rolls_back_notes_fts_aliases_and_tags() {
    let (_dir, index) = setup();
    index.upsert_batch(&[record("A.md", "原始正文")]).unwrap();
    let mut bad = record("B.md", "坏记录");
    bad.content_hash = "not-a-hash".into();
    assert!(index
        .upsert_batch(&[record("A.md", "替换正文"), bad])
        .is_err());
    assert_eq!(index.get(&path("A.md")).unwrap().unwrap().body, "原始正文");
    for table in ["notes", "note_fts", "aliases", "tags"] {
        assert_eq!(count(&index, table), 1);
    }
    assert!(index
        .upsert_batch(&vec![record("C.md", "x"); BATCH_SIZE + 1])
        .is_err());
    assert_eq!(count(&index, "notes"), 1);
}

#[test]
fn update_is_idempotent_and_delete_clears_related_fts_and_link_resolution() {
    let (dir, index) = setup();
    index
        .upsert_batch(&[record("A.md", "旧正文"), record("B.md", "来源")])
        .unwrap();
    let external = Connection::open(dir.path().join("index.db")).unwrap();
    external.execute("INSERT INTO links(source_path,target_text,target_path,line,col,resolved) VALUES('B.md','A','A.md',1,1,1)",[]).unwrap();
    let mut updated = record("A.md", "新正文");
    updated.tags = vec!["新标签".into()];
    updated.aliases.clear();
    index.upsert_batch(&[updated.clone()]).unwrap();
    index.upsert_batch(&[updated]).unwrap();
    assert_eq!(count(&index, "note_fts"), 2);
    assert_eq!(
        index.get(&path("A.md")).unwrap().unwrap().tags,
        vec!["新标签"]
    );
    index.remove(&[path("A.md")]).unwrap();
    assert!(index.get(&path("A.md")).unwrap().is_none());
    assert_eq!(count(&index, "note_fts"), 1);
    let link: (Option<String>, i64) = index
        .read(|c| {
            c.query_row("SELECT target_path,resolved FROM links", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
        })
        .unwrap();
    assert_eq!(link, (None, 0));
    assert_eq!(count(&index, "tags"), 1);
}

#[test]
fn version_rebuild_preserves_all_four_business_state_tables() {
    let (dir, index) = setup();
    index.upsert_batch(&[record("A.md", "旧正文")]).unwrap();
    index.finish_build().unwrap();
    drop(index);
    let db = Connection::open(dir.path().join("index.db")).unwrap();
    db.execute_batch("INSERT INTO snapshot_index VALUES('A.md','v1','snapshots/a',3,1); INSERT INTO sync_state VALUES('A.md','base','local','etag','dev',1,'dirty'); INSERT INTO sync_tombstone VALUES('B.md',1,'hash'); INSERT INTO rename_journal VALUES(1,'A.md','C.md','[]','[]',1,'running'); UPDATE meta SET value='old' WHERE key IN ('schema_version','tokenizer_version');").unwrap();
    drop(db);
    let index = SqliteIndexStore::open(&dir.path().join("index.db"), ID).unwrap();
    assert!(index.needs_rebuild);
    index.begin_build().unwrap();
    index.upsert_batch(&[record("C.md", "新正文")]).unwrap();
    index.finish_build().unwrap();
    for table in [
        "snapshot_index",
        "sync_state",
        "sync_tombstone",
        "rename_journal",
    ] {
        assert_eq!(count(&index, table), 1);
    }
    assert!(index.get(&path("A.md")).unwrap().is_none());
    assert_eq!(index.status().unwrap().state, IndexState::Ready);
    drop(index);
    assert!(
        !SqliteIndexStore::open(&dir.path().join("index.db"), ID)
            .unwrap()
            .needs_rebuild
    );
}

#[test]
fn unknown_schema_identity_and_corruption_are_not_silently_replaced() {
    let (dir, index) = setup();
    drop(index);
    assert!(SqliteIndexStore::open(&dir.path().join("index.db"), "1111111111111111").is_err());
    let db = Connection::open(dir.path().join("index.db")).unwrap();
    db.execute("ALTER TABLE sync_state ADD COLUMN future_data TEXT", [])
        .unwrap();
    drop(db);
    assert!(SqliteIndexStore::open(&dir.path().join("index.db"), ID).is_err());
    let db = Connection::open(dir.path().join("index.db")).unwrap();
    db.execute("SELECT future_data FROM sync_state", [])
        .unwrap();
    drop(db);
    let broken = dir.path().join("broken.db");
    fs::write(&broken, b"not a sqlite database").unwrap();
    assert!(SqliteIndexStore::open(&broken, ID).is_err());
    assert_eq!(fs::read(broken).unwrap(), b"not a sqlite database");
}

#[test]
fn exclusive_writer_lease_prevents_two_store_writers() {
    let (dir, index) = setup();
    assert!(SqliteIndexStore::open(&dir.path().join("index.db"), ID).is_err());
    drop(index);
    assert!(SqliteIndexStore::open(&dir.path().join("index.db"), ID).is_ok());
}

#[test]
fn extraction_uses_frontmatter_and_shared_markdown_semantics_without_mutating_source() {
    let dir = tempfile::tempdir().unwrap();
    let raw="\u{feff}---\r\ntitle: 自定义标题\r\ntags: [知识/数据库, 知识/数据库]\r\naliases: [别名]\r\ncreated: '2026-10-27T08:00:00+08:00'\r\nupdated: bad-time\r\n---\r\n# 标题\r\n中**文**连续字\r\n\r\n![图说明](attachments/a.png)\r\n\r\n```rust\r\nlet test = 1;\r\n```\r\n";
    fs::write(dir.path().join("A.md"), raw).unwrap();
    let store = FsStore::open(dir.path().to_str().unwrap()).unwrap();
    let note = extract(path("A.md"), store.read(&path("A.md")).unwrap()).unwrap();
    assert_eq!(note.title, "自定义标题");
    assert_eq!(note.tags, vec!["知识/数据库"]);
    assert_eq!(note.aliases, vec!["别名"]);
    assert_eq!(note.created_ms, Some(1793059200000));
    assert_eq!(note.updated_ms, None);
    assert!(note.has_attachment);
    assert!(note.body.contains("中文连续字"));
    assert!(note.body.contains("let test = 1;"));
    assert!(!note.body.contains("created:"));
    assert_eq!(note.content_hash.len(), 64);
    assert_eq!(fs::read_to_string(dir.path().join("A.md")).unwrap(), raw);
    let options = markdown_options();
    assert!(
        options.extension.table
            && options.extension.tasklist
            && options.extension.autolink
            && options.extension.strikethrough
    );
    assert!(
        !options.extension.footnotes
            && !options.extension.description_lists
            && !options.extension.superscript
            && !options.render.r#unsafe
    );
}

#[test]
fn invalid_yaml_is_indexed_as_body_but_invalid_encoding_and_large_files_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = FsStore::open(dir.path().to_str().unwrap()).unwrap();
    fs::write(dir.path().join("A.md"), "---\ntitle: [broken\n---\n正文").unwrap();
    assert!(extract(path("A.md"), store.read(&path("A.md")).unwrap())
        .unwrap()
        .body
        .contains("broken"));
    fs::write(dir.path().join("A.md"), [0xff]).unwrap();
    assert_eq!(
        extract(path("A.md"), store.read(&path("A.md")).unwrap())
            .unwrap_err()
            .code,
        "E_ENCODING_INVALID"
    );
    fs::File::create(dir.path().join("A.md"))
        .unwrap()
        .set_len(10 * 1024 * 1024 + 1)
        .unwrap();
    assert_eq!(
        extract(path("A.md"), store.read(&path("A.md")).unwrap())
            .unwrap_err()
            .code,
        "E_FILE_TOO_LARGE"
    );
}

fn wait_finished(index: &BackgroundIndex) -> notebase_core::index::IndexStatus {
    let start = Instant::now();
    loop {
        let status = index.status();
        if status.state != IndexState::Rebuilding {
            return status;
        }
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn real_vault_background_build_batches_and_refreshes_without_changing_files() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    for i in 0..205 {
        fs::write(
            vault.join(format!("笔记{i}.md")),
            format!("# 标题{i}\n数据库索引"),
        )
        .unwrap();
    }
    let events = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
    let capture = events.clone();
    let sink: EventSink =
        Arc::new(move |name, value| capture.lock().unwrap().push((name.into(), value)));
    let index = BackgroundIndex::start(
        dir.path().join("data"),
        vault.to_str().unwrap().into(),
        ID.into(),
        sink,
    );
    let status = wait_finished(&index);
    assert_eq!(status.state, IndexState::Ready);
    assert_eq!(status.note_count, 205);
    assert!(status.index_bytes > 0);
    assert!(status.last_full_build_ms.is_some());
    assert_eq!(fs::read_dir(&vault).unwrap().count(), 205);
    assert!(dir
        .path()
        .join("data/vaults")
        .join(ID)
        .join("index.db")
        .exists());
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|(name, p)| name == "index.progress" && p["done"] == 200));
    fs::remove_file(vault.join("笔记0.md")).unwrap();
    index.invalidate();
    assert_eq!(index.status().state, IndexState::Offline);
    index.refresh();
    let start = Instant::now();
    while index.status().state != IndexState::Ready || index.status().note_count != 204 {
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
    }
    index.shutdown();
    let db = SqliteIndexStore::open(
        &dir.path().join("data/vaults").join(ID).join("index.db"),
        ID,
    )
    .unwrap();
    assert_eq!(count(&db, "notes"), 204);
    assert_eq!(count(&db, "note_fts"), 204);
}

#[test]
fn partial_or_unwritable_index_never_claims_ready_and_cache_cannot_be_in_vault() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("A.md"), "good").unwrap();
    fs::write(vault.join("B.md"), [0xff]).unwrap();
    assert!(cache_path(&vault.join("cache"), &vault, ID).is_err());
    assert!(!vault.join("cache").exists());
    let index = BackgroundIndex::start(
        dir.path().join("data"),
        vault.to_str().unwrap().into(),
        ID.into(),
        Arc::new(|_, _| {}),
    );
    assert_eq!(wait_finished(&index).state, IndexState::Offline);
    // 候选含非法文件时不发布；后续增量对账可提交其余合法记录，但仍明确 offline。
    let start = Instant::now();
    while index.status().note_count != 1 {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(index.status().note_count, 1);
    index.shutdown();
    let index = BackgroundIndex::start(
        vault.clone(),
        vault.to_str().unwrap().into(),
        ID.into(),
        Arc::new(|_, _| {}),
    );
    assert_eq!(wait_finished(&index).state, IndexState::Offline);
    index.shutdown();
}

#[test]
fn unavailable_index_does_not_block_file_editing_and_reopen_retries_initialization() {
    use notebase_core::{new_indexed_app, Request};
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    let data = dir.path().join("data");
    fs::write(&data, "blocked by a file").unwrap();
    let mut app = new_indexed_app(data.clone(), Arc::new(|_, _| {}));
    let invoke = |app: &mut notebase_core::App, command: &str, payload: Value| {
        serde_json::to_value(app.dispatch(Request {
            id: "index-test".into(),
            command: command.into(),
            payload,
            client_seq: 1,
        }))
        .unwrap()
    };
    assert_eq!(
        invoke(&mut app, "vault_open", json!({"path":vault}))["ok"],
        true
    );
    let start = Instant::now();
    while invoke(&mut app, "index_status", json!({}))["data"]["state"] == "rebuilding" {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        invoke(&mut app, "note_create", json!({"title":"A"}))["ok"],
        true
    );
    let read = invoke(&mut app, "note_read", json!({"path":"A.md"}));
    assert_eq!(
        invoke(
            &mut app,
            "note_save",
            json!({"path":"A.md","body":"可编辑","baseHash":read["data"]["contentHash"]})
        )["ok"],
        true
    );
    fs::remove_file(&data).unwrap();
    assert_eq!(
        invoke(&mut app, "vault_open", json!({"path":vault}))["ok"],
        true
    );
    let start = Instant::now();
    while invoke(&mut app, "index_status", json!({}))["data"]["state"] != "ready" {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        invoke(&mut app, "index_status", json!({}))["data"]["noteCount"],
        1
    );
}

#[test]
fn empty_vault_build_is_ready_and_shutdown_stops_events_and_releases_writer() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let capture = events.clone();
    let worker = BackgroundIndex::start(
        dir.path().join("data"),
        vault.to_str().unwrap().into(),
        ID.into(),
        Arc::new(move |name, _| capture.lock().unwrap().push(name.to_owned())),
    );
    let status = wait_finished(&worker);
    assert_eq!(status.state, IndexState::Ready);
    assert_eq!(status.note_count, 0);
    worker.shutdown();
    let last = events.lock().unwrap().len();
    let index = SqliteIndexStore::open(
        &dir.path().join("data/vaults").join(ID).join("index.db"),
        ID,
    )
    .unwrap();
    assert_eq!(index.status().unwrap().state, IndexState::Ready);
    assert_eq!(events.lock().unwrap().len(), last);
}
