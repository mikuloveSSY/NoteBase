use super::{
    extract::extract, sqlite::SqliteIndexStore, unavailable, IndexProgress, IndexSession,
    IndexState, IndexStatus, IndexStore, BATCH_SIZE,
};
use crate::{
    domain::{AppError, NoteStore, RelPath, Result},
    infra::FsStore,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

pub type EventSink = Arc<dyn Fn(&str, Value) + Send + Sync>;
struct State {
    status: IndexStatus,
    generation: u64,
    active: bool,
}
pub struct BackgroundIndex {
    state: Arc<Mutex<State>>,
    cancelled: Arc<AtomicBool>,
    refresh: SyncSender<()>,
    events: EventSink,
    worker: Option<thread::JoinHandle<()>>,
}

/// 检查既有父目录的真实位置后再创建缓存；拒绝把应用数据放入当前笔记库。
pub fn cache_path(data_root: &Path, vault: &Path, vault_id: &str) -> Result<PathBuf> {
    if !data_root.is_absolute()
        || vault_id.len() != 16
        || !vault_id.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(unavailable("应用数据目录或库标识不合法"));
    }
    let vault = fs::canonicalize(vault).map_err(AppError::io)?;
    let mut ancestor = data_root;
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| unavailable("应用数据目录没有可用父目录"))?;
    }
    let physical = fs::canonicalize(ancestor).map_err(AppError::io)?.join(
        data_root
            .strip_prefix(ancestor)
            .map_err(|_| unavailable("应用数据路径不合法"))?,
    );
    if physical
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
        || physical.starts_with(&vault)
    {
        return Err(unavailable("索引必须位于笔记库之外"));
    }
    fs::create_dir_all(data_root).map_err(AppError::io)?;
    let data_root = fs::canonicalize(data_root).map_err(AppError::io)?;
    if data_root.starts_with(&vault) {
        return Err(unavailable("索引必须位于笔记库之外"));
    }
    // 不跟随缓存层级中的 junction/symlink；库根目录本身可以由用户明确选择。
    let mut folder = data_root;
    for name in ["vaults", vault_id] {
        folder.push(name);
        if let Ok(meta) = fs::symlink_metadata(&folder) {
            #[cfg(windows)]
            let is_link = {
                use std::os::windows::fs::MetadataExt;
                meta.file_attributes() & 0x400 != 0
            };
            #[cfg(not(windows))]
            let is_link = meta.file_type().is_symlink();
            if is_link {
                return Err(unavailable("索引目录不能经过符号链接或目录联接"));
            }
        } else {
            fs::create_dir(&folder).map_err(AppError::io)?;
        }
    }
    Ok(folder.join("index.db"))
}

impl BackgroundIndex {
    pub fn start(data_root: PathBuf, vault: String, vault_id: String, events: EventSink) -> Self {
        let state = Arc::new(Mutex::new(State {
            status: IndexStatus {
                state: IndexState::Rebuilding,
                ..IndexStatus::default()
            },
            generation: 0,
            active: true,
        }));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (refresh, receiver) = mpsc::sync_channel(1);
        let state2 = state.clone();
        let cancel2 = cancelled.clone();
        let events2 = events.clone();
        let worker = thread::Builder::new()
            .name(format!("index-{vault_id}"))
            .spawn(move || {
                let result = (|| -> Result<()> {
                    let path = cache_path(&data_root, Path::new(&vault), &vault_id)?;
                    let start = Instant::now();
                    let index = loop {
                        if cancel2.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        match SqliteIndexStore::open(&path, &vault_id) {
                            Ok(index) => break index,
                            Err(error)
                                if error.retryable && start.elapsed() < Duration::from_secs(5) =>
                            {
                                thread::sleep(Duration::from_millis(25))
                            }
                            Err(error) => return Err(error),
                        }
                    };
                    let store = FsStore::open(&vault)?;
                    loop {
                        if cancel2.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        if let Err(error) = build(&index, &store, &state2, &cancel2, &events2) {
                            let _ = index.mark_offline();
                            report_error(&state2, &events2, error);
                        }
                        match receiver.recv() {
                            Ok(()) => {}
                            Err(_) => return Ok(()),
                        }
                    }
                })();
                // 先关闭刷新入口，再公布初始化失败，避免重开库把请求发给将退出的线程。
                drop(receiver);
                if let Err(error) = result {
                    report_error(&state2, &events2, error);
                }
            });
        let worker = match worker {
            Ok(worker) => Some(worker),
            Err(error) => {
                report_error(
                    &state,
                    &events,
                    unavailable(format!("无法启动索引线程：{error}")),
                );
                None
            }
        };
        Self {
            state,
            cancelled,
            refresh,
            events,
            worker,
        }
    }
    /// 测试/显式关闭入口；普通切库只发取消信号，不在 App 全局锁内等待扫描结束。
    pub fn shutdown(mut self) {
        self.deactivate();
        let (dummy, _) = mpsc::sync_channel(1);
        drop(std::mem::replace(&mut self.refresh, dummy));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
    fn deactivate(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Ok(mut state) = self.state.lock() {
            state.active = false;
        }
    }
}
impl Drop for BackgroundIndex {
    fn drop(&mut self) {
        self.deactivate();
    }
}
impl IndexSession for BackgroundIndex {
    fn status(&self) -> IndexStatus {
        self.state
            .lock()
            .map(|s| s.status.clone())
            .unwrap_or_default()
    }
    fn invalidate(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.generation += 1;
            if state.status.state != IndexState::Rebuilding {
                state.status.state = IndexState::Offline;
            }
            emit_state(&state, &self.events);
        }
    }
    fn refresh(&self) -> bool {
        if let Ok(mut state) = self.state.lock() {
            match self.refresh.try_send(()) {
                Ok(()) | Err(mpsc::TrySendError::Full(())) => {
                    state.generation += 1;
                    state.status.state = IndexState::Rebuilding;
                    emit_state(&state, &self.events);
                    true
                }
                Err(mpsc::TrySendError::Disconnected(())) => false,
            }
        } else {
            false
        }
    }
}
fn emit_state(state: &State, events: &EventSink) {
    if state.active {
        events(
            "index.state",
            json!({"state":state.status.state,"noteCount":state.status.note_count}),
        );
    }
}
fn report_error(state: &Mutex<State>, events: &EventSink, error: AppError) {
    if let Ok(mut state) = state.lock() {
        state.status.state = IndexState::Offline;
        state.status.progress = None;
        emit_state(&state, events);
        if state.active {
            events(
                "error.raised",
                json!({"code":error.code,"message":error.message,"detail":error.detail}),
            );
        }
    }
}
fn progress(
    state: &Mutex<State>,
    events: &EventSink,
    value: IndexProgress,
    index: &SqliteIndexStore,
) -> Result<()> {
    let status = index.status()?;
    if let Ok(mut state) = state.lock() {
        state.status = IndexStatus {
            progress: Some(value.clone()),
            state: IndexState::Rebuilding,
            ..status
        };
        if state.active {
            events("index.progress", serde_json::to_value(value).unwrap());
        }
    }
    Ok(())
}
fn build(
    index: &SqliteIndexStore,
    store: &dyn NoteStore,
    state: &Mutex<State>,
    cancelled: &AtomicBool,
    events: &EventSink,
) -> Result<()> {
    let start = Instant::now();
    let generation = {
        let mut state = state.lock().map_err(|_| unavailable("索引状态锁异常"))?;
        state.status.state = IndexState::Rebuilding;
        state.status.progress = Some(IndexProgress {
            phase: "scan".into(),
            done: 0,
            total: 0,
            elapsed_ms: 0,
        });
        emit_state(&state, events);
        state.generation
    };
    let entries = store.list(&RelPath::root())?;
    if cancelled.load(Ordering::Acquire) {
        return Err(AppError::new("E_CANCELLED", "索引构建已取消"));
    }
    index.begin_build()?;
    let total = entries.len();
    let mut done = 0;
    let mut rejected = 0;
    for entries in entries.chunks(BATCH_SIZE) {
        let mut records = Vec::with_capacity(entries.len());
        let mut batch_bytes = 0;
        for entry in entries {
            if cancelled.load(Ordering::Acquire) {
                return Err(AppError::new("E_CANCELLED", "索引构建已取消"));
            }
            let path = RelPath::new(&entry.path)?;
            let result = store
                .read(&path)
                .map_err(AppError::from)
                .and_then(|note| extract(path, note));
            match result {
                Ok(note) => {
                    batch_bytes += note.size_bytes;
                    records.push(note);
                }
                Err(error)
                    if matches!(
                        error.code.as_str(),
                        "E_ENCODING_INVALID" | "E_FILE_TOO_LARGE" | "E_NOT_FOUND"
                    ) =>
                {
                    rejected += 1;
                }
                Err(error) => return Err(error),
            }
            // 同时限制条数和字节量，避免 200 篇接近 10 MiB 的笔记堆积在内存。
            if batch_bytes >= 8 * 1024 * 1024 {
                index.upsert_batch(&records)?;
                records.clear();
                batch_bytes = 0;
            }
        }
        index.upsert_batch(&records)?;
        done += entries.len();
        progress(
            state,
            events,
            IndexProgress {
                phase: "write".into(),
                done,
                total,
                elapsed_ms: start.elapsed().as_millis() as u64,
            },
            index,
        )?;
    }
    // 整个扫描期的新建/保存都使此次快照过期；没有增量队列时不可宣称已追平。
    // optimize 不持有会话状态锁；读写笔记与状态查询仍能继续。
    progress(
        state,
        events,
        IndexProgress {
            phase: "optimize".into(),
            done,
            total,
            elapsed_ms: start.elapsed().as_millis() as u64,
        },
        index,
    )?;
    if cancelled.load(Ordering::Acquire) {
        return Err(AppError::new("E_CANCELLED", "索引构建已取消"));
    }
    index.optimize()?;
    let mut state = state.lock().map_err(|_| unavailable("索引状态锁异常"))?;
    if cancelled.load(Ordering::Acquire) || !state.active {
        return Err(AppError::new("E_CANCELLED", "索引构建已取消"));
    }
    if rejected > 0 || state.generation != generation {
        index.mark_offline()?;
        state.status = index.status()?;
        emit_state(&state, events);
        events(
            "error.raised",
            json!({"code":"E_INDEX_UNAVAILABLE","message":"索引不完整或构建期间文件已变化；修复文件后重新打开库以刷新索引","detail":{"skipped":rejected,"changedDuringBuild":state.generation!=generation}}),
        );
    } else {
        index.finish_build()?;
        state.status = index.status()?;
        emit_state(&state, events);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Entry, FileStat, StoreResult, StoredNote, WriteOutcome};
    struct InterleavedStore {
        inner: FsStore,
        on_read: Box<dyn Fn() + Send>,
    }
    impl NoteStore for InterleavedStore {
        fn read(&self, p: &RelPath) -> StoreResult<StoredNote> {
            (self.on_read)();
            self.inner.read(p)
        }
        fn write_atomic(&self, p: &RelPath, b: &[u8]) -> StoreResult<WriteOutcome> {
            self.inner.write_atomic(p, b)
        }
        fn create_new(&self, p: &RelPath) -> StoreResult<()> {
            self.inner.create_new(p)
        }
        fn rename(&self, p: &RelPath, q: &RelPath) -> StoreResult<()> {
            self.inner.rename(p, q)
        }
        fn delete(&self, p: &RelPath) -> StoreResult<()> {
            self.inner.delete(p)
        }
        fn list(&self, p: &RelPath) -> StoreResult<Vec<Entry>> {
            self.inner.list(p)
        }
        fn stat(&self, p: &RelPath) -> StoreResult<FileStat> {
            self.inner.stat(p)
        }
        fn vault_path(&self) -> String {
            self.inner.vault_path()
        }
    }
    #[test]
    fn local_save_during_build_does_not_publish_ready() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        fs::create_dir(&vault).unwrap();
        fs::write(vault.join("A.md"), "content").unwrap();
        let state = Arc::new(Mutex::new(State {
            status: IndexStatus::default(),
            generation: 0,
            active: true,
        }));
        let changing = state.clone();
        let store = InterleavedStore {
            inner: FsStore::open(vault.to_str().unwrap()).unwrap(),
            on_read: Box::new(move || {
                changing.lock().unwrap().generation += 1;
            }),
        };
        let index =
            SqliteIndexStore::open(&dir.path().join("index.db"), "0123456789abcdef").unwrap();
        build(
            &index,
            &store,
            &state,
            &AtomicBool::new(false),
            &(Arc::new(|_: &str, _: Value| {}) as EventSink),
        )
        .unwrap();
        assert_eq!(state.lock().unwrap().status.state, IndexState::Offline);
        assert!(index.status().unwrap().last_full_build_ms.is_none());
    }
    #[test]
    fn cancellation_between_file_reads_never_commits_a_ready_index() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        fs::create_dir(&vault).unwrap();
        fs::write(vault.join("A.md"), "content").unwrap();
        fs::write(vault.join("B.md"), "content").unwrap();
        let state = Mutex::new(State {
            status: IndexStatus::default(),
            generation: 0,
            active: true,
        });
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = cancelled.clone();
        let store = InterleavedStore {
            inner: FsStore::open(vault.to_str().unwrap()).unwrap(),
            on_read: Box::new(move || cancel.store(true, Ordering::Release)),
        };
        let index =
            SqliteIndexStore::open(&dir.path().join("index.db"), "0123456789abcdef").unwrap();
        assert_eq!(
            build(
                &index,
                &store,
                &state,
                &cancelled,
                &(Arc::new(|_: &str, _: Value| {}) as EventSink)
            )
            .unwrap_err()
            .code,
            "E_CANCELLED"
        );
        assert_ne!(index.status().unwrap().state, IndexState::Ready);
        assert_eq!(index.status().unwrap().note_count, 0);
    }
}
