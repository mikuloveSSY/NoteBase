use notebase_core::{
    domain::{RelPath, Result},
    index::{IndexSession, IndexStatus},
    infra::FsStore,
    App, Request,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Barrier, Mutex,
};
struct Session {
    active: AtomicBool,
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
}
impl IndexSession for Session {
    fn cancel(&self) {
        self.active.store(false, Ordering::Release);
    }
    fn rebuild(&self) -> Result<String> {
        Ok("job".into())
    }
    fn enqueue(&self, _: &RelPath) -> bool {
        true
    }
    fn search(&self, _: &Value) -> Result<Value> {
        self.entered.wait();
        self.release.wait();
        if !self.active.load(Ordering::Acquire) {
            return Err(notebase_core::index::unavailable("会话已取消"));
        }
        Ok(json!({"items":[]}))
    }
    fn status(&self) -> IndexStatus {
        IndexStatus::default()
    }
    fn invalidate(&self) {}
    fn refresh(&self) -> bool {
        true
    }
}
#[test]
fn detached_query_allows_application_work_and_switch_cancels_old_session() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    let b = dir.path().join("b");
    std::fs::create_dir(&a).unwrap();
    std::fs::create_dir(&b).unwrap();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let (e, r) = (entered.clone(), release.clone());
    let app = App::new(Box::new(|p| Ok(Box::new(FsStore::open(p)?)))).with_index(Box::new(
        move |_, _| {
            Arc::new(Session {
                active: AtomicBool::new(true),
                entered: e.clone(),
                release: r.clone(),
            })
        },
    ));
    let app = Arc::new(Mutex::new(app));
    let open = |p: &std::path::Path| Request {
        id: "test".into(),
        client_seq: 0,
        command: "vault_open".into(),
        payload: json!({"path":p.to_str().unwrap()}),
    };
    app.lock().unwrap().dispatch(open(&a));
    let session = app.lock().unwrap().search_session().unwrap();
    let query = std::thread::spawn(move || session.search(&json!({"query":"查询"})));
    entered.wait();
    // 查询仍阻塞时应用锁可用，切库立即取消旧 Arc，迟到的结果不得成为成功响应。
    let mut locked = app
        .try_lock()
        .expect("search must release application lock");
    locked.dispatch(open(&b));
    drop(locked);
    release.wait();
    assert_eq!(
        query.join().unwrap().unwrap_err().code,
        "E_INDEX_UNAVAILABLE"
    );
}
