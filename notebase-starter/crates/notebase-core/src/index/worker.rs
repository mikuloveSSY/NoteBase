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
#[derive(Clone, Copy)]
pub struct WorkerOptions {
    pub native_watch: bool,
    pub reconcile_interval: Duration,
    pub debounce: Duration,
    pub queue_capacity: usize,
    #[cfg(test)]
    pub reconcile_test_delay: Duration,
}
impl Default for WorkerOptions {
    fn default() -> Self {
        Self {
            native_watch: true,
            reconcile_interval: Duration::from_secs(300),
            debounce: Duration::from_millis(150),
            queue_capacity: super::queue::QUEUE_CAPACITY,
            #[cfg(test)]
            reconcile_test_delay: Duration::ZERO,
        }
    }
}
struct State {
    status: IndexStatus,
    generation: u64,
    active: bool,
}
pub struct BackgroundIndex {
    readers: Arc<Mutex<Option<std::sync::Weak<SqliteIndexStore>>>>,
    full_rebuild: Arc<AtomicBool>,
    rebuild_pending: Arc<Mutex<Option<String>>>,
    vault: String,
    queue: Arc<super::queue::IndexQueue>,
    db_path: PathBuf,
    state: Arc<Mutex<State>>,
    cancelled: Arc<AtomicBool>,
    refresh: SyncSender<()>,
    events: EventSink,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
    queries: std::sync::RwLock<()>,
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
        Self::start_with_options(data_root, vault, vault_id, events, WorkerOptions::default())
    }
    pub fn start_with_options(
        data_root: PathBuf,
        vault: String,
        vault_id: String,
        events: EventSink,
        options: WorkerOptions,
    ) -> Self {
        let vault_path = vault.clone();
        let readers = Arc::new(Mutex::new(None));
        let worker_readers = readers.clone();
        let full_rebuild = Arc::new(AtomicBool::new(true));
        let full_rebuild_worker = full_rebuild.clone();
        let rebuild_pending = Arc::new(Mutex::new(None));
        let rebuild_requests = rebuild_pending.clone();
        let db_path = data_root.join("vaults").join(&vault_id).join("index.db");
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
        let queue = Arc::new(super::queue::IndexQueue::new(options.queue_capacity));
        let work_queue = queue.clone();
        let wake = refresh.clone();
        let state2 = state.clone();
        let cancel2 = cancelled.clone();
        let events2 = events.clone();
        let worker = thread::Builder::new()
            .name(format!("index-{vault_id}"))
            .spawn(move || {
                let result = (|| -> Result<()> {
                    let path = cache_path(&data_root, Path::new(&vault), &vault_id)?;
                    let start = Instant::now();
                    let recovered = loop {
                        if cancel2.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        match super::recovery::open(&path, &vault_id) {
                            Ok(index) => break index,
                            Err(error)
                                if error.retryable && start.elapsed() < Duration::from_secs(5) =>
                            {
                                thread::sleep(Duration::from_millis(25))
                            }
                            Err(error) => return Err(error),
                        }
                    };
                    if let Some(archive)=&recovered.archived{if state2.lock().unwrap().active{events2("error.raised",json!({"code":"E_INDEX_UNAVAILABLE","message":"原索引已归档并重新构建；请核对历史业务状态","detail":{"recoveryDir":archive,"businessRestored":recovered.business_restored}}));}}
                    let index=Arc::new(recovered.store);*worker_readers.lock().unwrap()=Some(Arc::downgrade(&index));
                    let store = FsStore::open(&vault)?;
                    let watcher_failed=Arc::new(AtomicBool::new(false));
                    let mut watcher=if options.native_watch{
                        match super::watcher::start(Path::new(&store.vault_path()),work_queue.clone(),wake,watcher_failed.clone()){
                            Ok(watcher)=>Some(watcher),Err(error)=>{work_queue.reconcile(Some(format!("原生监听不可用，已启用周期对账：{error}")));None}
                        }
                    }else{work_queue.reconcile(Some("原生监听已禁用，使用周期对账".into()));None};
                    if index.needs_rebuild{
                        if let Err(error)=staged_build(&index,&path,&vault_id,&store,&state2,&cancel2,&events2){report_error(&state2,&events2,error);work_queue.reconcile(None);}
                    }else{let mut state=state2.lock().unwrap();state.status=index.status()?;emit_state(&state,&events2);work_queue.reconcile(None);}
                    full_rebuild_worker.store(false,Ordering::Release);
                    let mut invalid=std::collections::BTreeSet::new();let mut last_reconcile=Instant::now();
                    loop {
                        if cancel2.load(Ordering::Acquire) {
                            return Ok(());
                        }
                        let request=rebuild_requests.lock().unwrap().take();
                        if request.is_some(){
                            #[cfg(test)]
                            thread::sleep(options.reconcile_test_delay);
                            if let Err(error)=staged_build(&index,&path,&vault_id,&store,&state2,&cancel2,&events2){report_error(&state2,&events2,error);work_queue.reconcile(None);}
                            full_rebuild_worker.store(false,Ordering::Release);
                            last_reconcile=Instant::now();
                        }
                        if watcher_failed.swap(false,Ordering::AcqRel){watcher=None;}
                        if last_reconcile.elapsed()>=options.reconcile_interval{work_queue.reconcile(None);last_reconcile=Instant::now();}
                        if let Some(work)=work_queue.take(options.debounce){
                            let full=work.reconcile;
                            if let Some(warning)=&work.warning{let state=state2.lock().unwrap();if state.active{events2("error.raised",json!({"code":"E_INDEX_UNAVAILABLE","message":warning,"detail":{"pollIntervalMs":options.reconcile_interval.as_millis() as u64}}));}}
                            {let mut state=state2.lock().unwrap();state.status.state=IndexState::Rebuilding;state.generation+=1;emit_state(&state,&events2);}
                            #[cfg(test)]
                            if full{thread::sleep(options.reconcile_test_delay);}
                            match super::incremental::apply(&index,&store,work,&mut invalid,&cancel2){
                                Ok(changes)=>{
                                    let mut state=state2.lock().unwrap();
                                    if !state.active||cancel2.load(Ordering::Acquire){return Ok(());}
                                    if invalid.is_empty()&&!work_queue.pending(){index.finish_increment(full)?;}else{index.mark_offline()?;}
                                    state.status=index.status()?;if full{last_reconcile=Instant::now();}emit_state(&state,&events2);
                                    for change in changes{if change.source==super::queue::Source::External{events2("note.changed",json!({"path":change.path,"source":"external","contentHash":change.hash}));}}
                                },
                                Err(error)=>{let _=index.mark_offline();report_error(&state2,&events2,error);last_reconcile=Instant::now().checked_sub(options.reconcile_interval.saturating_sub(Duration::from_secs(1))).unwrap_or_else(Instant::now);},
                            }

                        }
                        // 保持 watcher 生命周期覆盖整个会话。
                        let _=&watcher;
                        match receiver.recv_timeout(Duration::from_millis(50)) {
                            Ok(()) => {}
                            Err(mpsc::RecvTimeoutError::Timeout)=>{},
                            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
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
            readers,
            full_rebuild,
            rebuild_pending,
            vault: vault_path,
            queue,
            db_path,
            state,
            cancelled,
            refresh,
            events,
            worker: Mutex::new(worker),
            queries: std::sync::RwLock::new(()),
        }
    }
    /// 测试/显式关闭入口；普通切库只发取消信号，不在 App 全局锁内等待扫描结束。
    pub fn shutdown(self) {
        let _ = self.suspend();
    }
    fn deactivate(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Ok(mut state) = self.state.lock() {
            state.active = false;
            state.status.state = IndexState::Unavailable;
            state.status.progress = None;
        }
    }
}
impl Drop for BackgroundIndex {
    fn drop(&mut self) {
        self.deactivate();
    }
}
impl IndexSession for BackgroundIndex {
    fn suspend(&self) -> Result<()> {
        self.deactivate();
        let _queries = self
            .queries
            .write()
            .map_err(|_| unavailable("查询生命周期锁异常"))?;
        let worker = self
            .worker
            .lock()
            .map_err(|_| unavailable("索引线程锁异常"))?
            .take();
        if let Some(worker) = worker {
            worker
                .join()
                .map_err(|_| unavailable("索引线程关闭失败，拒绝恢复"))?;
        }
        Ok(())
    }
    fn cancel(&self) {
        self.deactivate();
    }
    fn rebuild(&self) -> Result<String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| unavailable("索引状态锁异常"))?;
        if !state.active
            || self
                .worker
                .lock()
                .unwrap()
                .as_ref()
                .is_none_or(|worker| worker.is_finished())
        {
            return Err(unavailable("索引线程不可用，请重新打开库"));
        }
        if state.status.state == IndexState::Rebuilding
            || self.full_rebuild.swap(true, Ordering::AcqRel)
        {
            let mut error = unavailable("已有索引任务正在执行，请稍后重试");
            error.retryable = true;
            return Err(error);
        }
        static JOB: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = format!(
            "rebuild-{}-{}-{}",
            std::process::id(),
            super::now_ms(),
            JOB.fetch_add(1, Ordering::Relaxed)
        );
        *self.rebuild_pending.lock().unwrap() = Some(id.clone());
        state.status.state = IndexState::Rebuilding;
        state.generation += 1;
        emit_state(&state, &self.events);
        if matches!(
            self.refresh.try_send(()),
            Err(mpsc::TrySendError::Disconnected(()))
        ) {
            return Err(unavailable("索引线程已停止"));
        }
        Ok(id)
    }
    fn enqueue(&self, path: &RelPath) -> bool {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return false,
        };
        if !state.active
            || self
                .worker
                .lock()
                .unwrap()
                .as_ref()
                .is_none_or(|worker| worker.is_finished())
        {
            return false;
        }
        state.generation += 1;
        if state.status.state != IndexState::Rebuilding {
            state.status.state = IndexState::Offline;
        }
        self.queue.push(path, super::queue::Source::Local);
        if matches!(
            self.refresh.try_send(()),
            Err(mpsc::TrySendError::Disconnected(()))
        ) {
            return false;
        }
        emit_state(&state, &self.events);
        true
    }
    fn search(&self, query: &Value) -> Result<Value> {
        let _query = self
            .queries
            .read()
            .map_err(|_| unavailable("查询生命周期锁异常"))?;
        let generation = {
            let state = self
                .state
                .lock()
                .map_err(|_| unavailable("索引状态锁异常"))?;
            if !state.active
                || (state.status.state != IndexState::Ready
                    && state.status.state != IndexState::Rebuilding)
            {
                return Err(unavailable("索引未就绪或已过期，请重新打开库"));
            }
            state.generation
        };
        let rebuilding = self.status().state == IndexState::Rebuilding;
        let reader = self
            .readers
            .lock()
            .unwrap()
            .as_ref()
            .and_then(std::sync::Weak::upgrade);
        let read = reader
            .as_ref()
            .map(|index| index.search(query))
            .unwrap_or_else(|| Err(unavailable("索引尚未打开")));
        let mut result = match read {
            Ok(result) => result,
            Err(error) if rebuilding && error.code == "E_INDEX_UNAVAILABLE" => {
                scan_search(&self.db_path, &self.vault, query)?
            }
            Err(error) => return Err(error),
        };
        if rebuilding {
            result["degraded"] = json!(true);
            if result["degradeReason"] != "index_rebuilding_partial" {
                result["degradeReason"] = json!("index_rebuilding");
            }
        }
        let state = self
            .state
            .lock()
            .map_err(|_| unavailable("索引状态锁异常"))?;
        if !state.active
            || state.generation != generation
            || !matches!(
                state.status.state,
                IndexState::Ready | IndexState::Rebuilding
            )
        {
            return Err(unavailable("查询期间索引已变化，请重试"));
        }
        Ok(result)
    }
    fn status(&self) -> IndexStatus {
        let mut status = self
            .state
            .lock()
            .map(|s| s.status.clone())
            .unwrap_or_default();
        if self.full_rebuild.load(Ordering::Acquire)
            && self
                .worker
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|w| !w.is_finished())
        {
            status.state = IndexState::Rebuilding;
        }
        status
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
            self.queue.reconcile(None);
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

fn staged_build(
    index: &SqliteIndexStore,
    path: &Path,
    id: &str,
    store: &dyn NoteStore,
    state: &Arc<Mutex<State>>,
    cancelled: &AtomicBool,
    events: &EventSink,
) -> Result<()> {
    let folder = tempfile::Builder::new()
        .prefix("rebuild-")
        .tempdir_in(path.parent().unwrap())
        .map_err(AppError::io)?;
    let candidate_path = folder.path().join("index.db");
    let candidate = SqliteIndexStore::open(&candidate_path, id)?;
    let private = Mutex::new(State {
        status: IndexStatus::default(),
        generation: 0,
        active: true,
    });
    let real = state.clone();
    let forward = events.clone();
    let stage_events: EventSink = Arc::new(move |name, payload| {
        let mut state = real.lock().unwrap();
        if !state.active {
            return;
        }
        if name == "index.progress" {
            state.status.state = IndexState::Rebuilding;
            if let Ok(progress) = serde_json::from_value::<IndexProgress>(payload.clone()) {
                state.status.progress = Some(progress);
            }
            forward(name, payload);
        }
    });
    build(&candidate, store, &private, cancelled, &stage_events)?;
    // 扫描期间的保存先留在主队列；发布前再从权威文件校验候选。
    for _ in 0..3 {
        if cancelled.load(Ordering::Acquire) {
            return Err(AppError::new("E_CANCELLED", "重建已取消"));
        }
        let generation = state.lock().unwrap().generation;
        let mut invalid = std::collections::BTreeSet::new();
        super::incremental::apply(
            &candidate,
            store,
            super::queue::Work {
                paths: Default::default(),
                reconcile: true,
                warning: None,
            },
            &mut invalid,
            cancelled,
        )?;
        if !invalid.is_empty() {
            return Err(unavailable("存在无法索引的文件，旧索引保留，重建未发布"));
        }
        if state.lock().unwrap().generation != generation {
            continue;
        }
        index.publish_from(&candidate_path)?;
        let mut state = state.lock().unwrap();
        if !state.active || cancelled.load(Ordering::Acquire) {
            return Err(AppError::new("E_CANCELLED", "重建已取消"));
        }
        if state.generation != generation {
            index.mark_offline()?;
        }
        state.status = index.status()?;
        emit_state(&state, events);
        return Ok(());
    }
    Err(unavailable(
        "重建期间持续写入，候选未发布；后台增量会继续追平",
    ))
}
fn scan_search(path: &Path, vault: &str, query: &Value) -> Result<Value> {
    let dir = tempfile::Builder::new()
        .prefix("scan-")
        .tempdir_in(path.parent().unwrap())
        .map_err(AppError::io)?;
    let store = FsStore::open(vault)?;
    let index = SqliteIndexStore::open(&dir.path().join("index.db"), "0000000000000000")?;
    let mut invalid = std::collections::BTreeSet::new();
    super::incremental::apply(
        &index,
        &store,
        super::queue::Work {
            paths: Default::default(),
            reconcile: true,
            warning: None,
        },
        &mut invalid,
        &AtomicBool::new(false),
    )?;
    index.finish_build()?;
    index.stable_scan_epoch()?;
    let mut result = index.search(query)?;
    result["degraded"] = json!(true);
    result["degradeReason"] = json!(if invalid.is_empty() {
        "index_rebuilding"
    } else {
        "index_rebuilding_partial"
    });
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Entry, FileStat, StoreResult, StoredNote, WriteOutcome};
    #[test]
    fn missing_index_scan_fallback_has_stable_paging() {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        fs::create_dir(&vault).unwrap();
        for name in ["A.md", "B.md"] {
            fs::write(vault.join(name), "扫描查询标记").unwrap();
        }
        let path = dir.path().join("index.db");
        let first = scan_search(
            &path,
            vault.to_str().unwrap(),
            &json!({"query":"扫描查询","limit":1}),
        )
        .unwrap();
        assert_eq!(first["degraded"], true);
        let second = scan_search(
            &path,
            vault.to_str().unwrap(),
            &json!({"query":"扫描查询","limit":1,"cursor":first["nextCursor"]}),
        )
        .unwrap();
        assert_eq!(second["items"].as_array().unwrap().len(), 1);
        assert_ne!(first["items"][0]["path"], second["items"][0]["path"]);
    }
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

    #[test]
    fn slow_reconcile_waits_a_full_interval_after_completion() {
        let temp = tempfile::tempdir().unwrap();
        let vault = temp.path().join("vault");
        fs::create_dir(&vault).unwrap();
        fs::write(vault.join("test.md"), "对账内容").unwrap();
        let events = Arc::new(Mutex::new(Vec::<(String, Instant)>::new()));
        let sink = events.clone();
        let worker = BackgroundIndex::start_with_options(
            temp.path().join("data"),
            vault.to_string_lossy().into_owned(),
            "0123456789abcdef".into(),
            Arc::new(move |name, payload| {
                if name == "index.state" {
                    sink.lock()
                        .unwrap()
                        .push((payload["state"].as_str().unwrap().into(), Instant::now()));
                }
            }),
            WorkerOptions {
                native_watch: false,
                reconcile_interval: Duration::from_millis(120),
                reconcile_test_delay: Duration::from_millis(180),
                debounce: Duration::ZERO,
                ..WorkerOptions::default()
            },
        );
        let start = Instant::now();
        loop {
            if events
                .lock()
                .unwrap()
                .iter()
                .filter(|(s, _)| s == "ready")
                .count()
                >= 3
            {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(10));
            thread::sleep(Duration::from_millis(5));
        }
        worker.rebuild().unwrap();
        loop {
            if events
                .lock()
                .unwrap()
                .iter()
                .filter(|(s, _)| s == "ready")
                .count()
                >= 5
            {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(10));
            thread::sleep(Duration::from_millis(5));
        }
        worker.shutdown();
        let events = events.lock().unwrap();
        let second = events
            .iter()
            .enumerate()
            .filter(|(_, e)| e.0 == "ready")
            .nth(1)
            .unwrap()
            .0;
        let next = events
            .iter()
            .skip(second + 1)
            .find(|e| e.0 == "rebuilding")
            .unwrap();
        assert!(
            next.1.duration_since(events[second].1) >= Duration::from_millis(100),
            "reconcile must cool down after completion"
        );
        let manual = events
            .iter()
            .enumerate()
            .filter(|(_, e)| e.0 == "ready")
            .nth(3)
            .unwrap()
            .0;
        let next = events
            .iter()
            .skip(manual + 1)
            .find(|e| e.0 == "rebuilding")
            .unwrap();
        assert!(
            next.1.duration_since(events[manual].1) >= Duration::from_millis(100),
            "rebuild must reset periodic reconciliation cooldown"
        );
    }
}
