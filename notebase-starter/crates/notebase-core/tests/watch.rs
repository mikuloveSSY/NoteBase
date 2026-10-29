type Events = std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>;
use notebase_core::{
    domain::RelPath,
    index::{
        worker::{BackgroundIndex, EventSink, WorkerOptions},
        IndexSession, IndexState,
    },
};
use serde_json::{json, Value};
use std::{
    fs,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
fn setup(options: WorkerOptions) -> (tempfile::TempDir, BackgroundIndex, Events) {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("A.md"), "初始正文").unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let capture = events.clone();
    let sink: EventSink =
        Arc::new(move |name, value| capture.lock().unwrap().push((name.to_owned(), value)));
    let worker = BackgroundIndex::start_with_options(
        dir.path().join("data"),
        vault.to_str().unwrap().into(),
        "0123456789abcdef".into(),
        sink,
        options,
    );
    wait(&worker, "初始", &["A.md"]);
    std::thread::sleep(Duration::from_millis(250));
    (dir, worker, events)
}
fn wait(worker: &BackgroundIndex, query: &str, expected: &[&str]) -> Value {
    let start = Instant::now();
    loop {
        if let Ok(page) = worker.search(&json!({"query":query,"sort":"mtime"})) {
            let mut actual: Vec<_> = page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["path"].as_str().unwrap().to_owned())
                .collect();
            actual.sort();
            let mut expected = expected.to_vec();
            expected.sort();
            if actual == expected {
                return page;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "query={query},state={:?}",
            worker.status()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn native_events_cover_create_modify_atomic_replace_delete_and_rename() {
    let (dir, worker, events) = setup(WorkerOptions::default());
    let vault = dir.path().join("vault");
    let start = Instant::now();
    fs::write(vault.join("B.md"), "新增标记").unwrap();
    wait(&worker, "新增", &["B.md"]);
    let elapsed = start.elapsed();
    eprintln!("native create-to-query: {}ms", elapsed.as_millis());
    assert!(elapsed < Duration::from_secs(1));
    fs::write(vault.join("B.md"), "修改标记").unwrap();
    wait(&worker, "修改", &["B.md"]);
    wait(&worker, "新增", &[]);
    // 模拟编辑器同目录临时文件替换，磁盘变化由 watcher 捕获。
    let tmp = tempfile::NamedTempFile::new_in(&vault).unwrap();
    fs::write(tmp.path(), "原子替换标记").unwrap();
    tmp.persist(vault.join("B.md")).unwrap();
    wait(&worker, "原子替换", &["B.md"]);
    fs::rename(vault.join("B.md"), vault.join("C.md")).unwrap();
    wait(&worker, "原子替换", &["C.md"]);
    fs::remove_file(vault.join("C.md")).unwrap();
    wait(&worker, "原子替换", &[]);
    fs::create_dir(vault.join("旧目录")).unwrap();
    fs::write(vault.join("旧目录/D.md"), "目录重命名标记").unwrap();
    wait(&worker, "目录重命名", &["旧目录/D.md"]);
    fs::rename(vault.join("旧目录"), vault.join("新目录")).unwrap();
    wait(&worker, "目录重命名", &["新目录/D.md"]);
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|(n, p)| n == "note.changed" && p["path"] == "B.md" && p["source"] == "external"));
    worker.shutdown();
}
#[test]
fn local_queue_is_deduplicated_and_updates_without_reopen_or_duplicate_external_event() {
    let (dir, worker, events) = setup(WorkerOptions::default());
    let file = dir.path().join("vault/A.md");
    events.lock().unwrap().clear();
    fs::write(&file, "本地保存终值").unwrap();
    for _ in 0..40 {
        assert!(worker.enqueue(&RelPath::new("A.md").unwrap()));
    }
    wait(&worker, "本地保存", &["A.md"]);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!events
        .lock()
        .unwrap()
        .iter()
        .any(|(n, p)| n == "note.changed" && p["path"] == "A.md"));
    worker.shutdown();
}
#[test]
fn polling_fallback_reconciles_missed_events_and_directory_rename() {
    let options = WorkerOptions {
        native_watch: false,
        reconcile_interval: Duration::from_millis(100),
        ..WorkerOptions::default()
    };
    let (dir, worker, events) = setup(options);
    let vault = dir.path().join("vault");
    fs::create_dir(vault.join("目录")).unwrap();
    fs::write(vault.join("目录/B.md"), "轮询标记").unwrap();
    wait(&worker, "轮询", &["目录/B.md"]);
    fs::rename(vault.join("目录"), vault.join("新目录")).unwrap();
    wait(&worker, "轮询", &["新目录/B.md"]);
    fs::remove_file(vault.join("新目录/B.md")).unwrap();
    wait(&worker, "轮询", &[]);
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|(n, p)| n == "error.raised"
            && p["message"].as_str().unwrap_or("").contains("周期对账")));
    worker.shutdown();
}
#[test]
fn overflow_reconciles_all_changes_and_increment_invalidates_old_search_cursor() {
    let options = WorkerOptions {
        native_watch: false,
        reconcile_interval: Duration::from_secs(300),
        queue_capacity: 2,
        ..WorkerOptions::default()
    };
    let (dir, worker, events) = setup(options);
    let vault = dir.path().join("vault");
    for i in 0..8 {
        let name = format!("笔记{i}.md");
        fs::write(vault.join(&name), "共同标记").unwrap();
        assert!(worker.enqueue(&RelPath::new(&name).unwrap()));
    }
    let expected: Vec<_> = (0..8).map(|i| format!("笔记{i}.md")).collect();
    let refs: Vec<_> = expected.iter().map(String::as_str).collect();
    wait(&worker, "共同", &refs);
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|(n, p)| n == "error.raised" && p["message"].as_str().unwrap_or("").contains("溢出")));
    let first = worker.search(&json!({"query":"共同","limit":1})).unwrap();
    fs::write(vault.join("笔记0.md"), "共同更新").unwrap();
    worker.enqueue(&RelPath::new("笔记0.md").unwrap());
    wait(&worker, "共同更新", &["笔记0.md"]);
    let error = worker
        .search(&json!({"query":"共同","limit":1,"cursor":first["nextCursor"]}))
        .unwrap_err();
    assert_eq!(error.code, "E_QUERY_SYNTAX");
    worker.shutdown();
}
#[test]
fn invalid_external_content_removes_stale_hit_and_recovers_when_fixed() {
    let (dir, worker, _) = setup(WorkerOptions::default());
    let file = dir.path().join("vault/A.md");
    fs::write(&file, [0xff]).unwrap();
    worker.enqueue(&RelPath::new("A.md").unwrap());
    let start = Instant::now();
    loop {
        if worker.status().state == IndexState::Offline && worker.status().note_count == 0 {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::write(&file, "修复编码标记").unwrap();
    worker.enqueue(&RelPath::new("A.md").unwrap());
    wait(&worker, "修复编码", &["A.md"]);
    worker.shutdown();
}

#[test]
fn initially_invalid_vault_becomes_searchable_after_repair() {
    let dir = tempfile::tempdir().unwrap();
    let vault = dir.path().join("vault");
    fs::create_dir(&vault).unwrap();
    fs::write(vault.join("A.md"), [0xff]).unwrap();
    let worker = BackgroundIndex::start_with_options(
        dir.path().join("data"),
        vault.to_str().unwrap().into(),
        "0123456789abcdef".into(),
        Arc::new(|_, _| {}),
        WorkerOptions {
            native_watch: false,
            ..WorkerOptions::default()
        },
    );
    let start = Instant::now();
    while worker.status().state != IndexState::Offline {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::write(vault.join("A.md"), "首次修复可检索").unwrap();
    worker.enqueue(&RelPath::new("A.md").unwrap());
    wait(&worker, "首次修复", &["A.md"]);
    worker.shutdown();
}

#[test]
fn event_rescan_and_ignored_paths_do_not_lose_reconciliation_request() {
    use notebase_core::index::{queue::IndexQueue, watcher::route};
    use notify::{event::Flag, Event, EventKind};
    let queue = IndexQueue::new(10);
    let root = std::path::Path::new("vault");
    route(
        root,
        Event::new(EventKind::Any).add_path(root.join("trash/hidden.md")),
        &queue,
    );
    assert!(!queue.pending());
    route(
        root,
        Event::new(EventKind::Other).set_flag(Flag::Rescan),
        &queue,
    );
    let work = queue.take(Duration::ZERO).unwrap();
    assert!(work.reconcile);
    assert!(work.warning.is_some());
}
