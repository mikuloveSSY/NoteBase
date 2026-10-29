use notebase_core::domain::{
    AppError, Entry, FileStat, NoteStore, RelPath, StoreError, StoreResult, StoredNote,
    WriteOutcome,
};
use notebase_core::infra::FsStore;
use notebase_core::note_files::FsNoteFiles;
use notebase_core::note_state::FsNoteStateStore;
use notebase_core::{App, Request};
use serde_json::{json, Value};
use std::{
    fs,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
struct FailingStore {
    inner: FsStore,
    fail: Arc<AtomicBool>,
}
impl NoteStore for FailingStore {
    fn read(&self, p: &RelPath) -> StoreResult<StoredNote> {
        self.inner.read(p)
    }
    fn write_atomic(&self, p: &RelPath, b: &[u8]) -> StoreResult<WriteOutcome> {
        if p.as_str() == "B.md" && self.fail.swap(false, Ordering::SeqCst) {
            return Err(StoreError::Failure(AppError::new(
                "E_IO_DISK_FULL",
                "injected interrupted rename",
            )));
        }
        self.inner.write_atomic(p, b)
    }
    fn create_new(&self, p: &RelPath) -> StoreResult<()> {
        self.inner.create_new(p)
    }
    fn rename(&self, a: &RelPath, b: &RelPath) -> StoreResult<()> {
        self.inner.rename(a, b)
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
fn call(a: &mut App, c: &str, p: Value) -> Value {
    serde_json::to_value(a.dispatch(Request {
        id: "rename".into(),
        command: c.into(),
        payload: p,
        client_seq: 0,
    }))
    .unwrap()
}
fn app(vault: &std::path::Path, data: &std::path::Path, fail: Arc<AtomicBool>) -> App {
    let data = data.to_path_buf();
    let mut a = App::new(Box::new(move |path| {
        Ok(Box::new(FailingStore {
            inner: FsStore::open(path)?,
            fail: fail.clone(),
        }))
    }))
    .with_support(Box::new(move |vault, id| {
        Ok((
            Box::new(FsNoteFiles::open(vault)?),
            Box::new(FsNoteStateStore::open(&data.join(id))?),
        ))
    }));
    assert_eq!(
        call(&mut a, "vault_open", json!({"path":vault}))["ok"],
        true
    );
    a
}
#[test]
fn interrupted_rename_survives_restart_and_is_idempotent() {
    let v = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    fs::write(v.path().join("A.md"), "# A").unwrap();
    fs::write(v.path().join("B.md"), "[[A]]").unwrap();
    fs::write(v.path().join("D.md"), "[[A|显示]]").unwrap();
    let fail = Arc::new(AtomicBool::new(true));
    let mut a = app(v.path(), data.path(), fail.clone());
    let p = call(
        &mut a,
        "note_rename",
        json!({"path":"A.md","newName":"C","planOnly":true}),
    );
    let interrupted = call(
        &mut a,
        "note_rename",
        json!({"path":"A.md","newName":"C","planOnly":false,"planId":p["data"]["plan"]["id"]}),
    );
    assert_eq!(
        interrupted["error"]["code"], "E_PARTIAL_RENAME",
        "{interrupted}"
    );
    assert!(v.path().join("C.md").exists());
    assert_eq!(fs::read_to_string(v.path().join("B.md")).unwrap(), "[[A]]");
    let id = interrupted["error"]["detail"]["journalId"].clone();
    drop(a);
    let mut a = app(v.path(), data.path(), fail);
    let records = call(&mut a, "note_list", json!({"scope":"renames"}));
    assert_eq!(records["data"]["items"].as_array().unwrap().len(), 1);
    let done = call(&mut a, "note_rename", json!({"resumeId":id}));
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(fs::read_to_string(v.path().join("B.md")).unwrap(), "[[C]]");
    assert_eq!(
        fs::read_to_string(v.path().join("D.md")).unwrap(),
        "[[C|显示]]"
    );
    assert!(
        call(&mut a, "note_list", json!({"scope":"renames"}))["data"]["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn externally_changed_pending_reference_is_preserved_during_resume() {
    let v = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    fs::write(v.path().join("A.md"), "# A").unwrap();
    fs::write(v.path().join("B.md"), "[[A]]").unwrap();
    let fail = Arc::new(AtomicBool::new(true));
    let mut a = app(v.path(), data.path(), fail);
    let p = call(
        &mut a,
        "note_rename",
        json!({"path":"A.md","newName":"C","planOnly":true}),
    );
    let r = call(
        &mut a,
        "note_rename",
        json!({"path":"A.md","newName":"C","planOnly":false,"planId":p["data"]["plan"]["id"]}),
    );
    fs::write(v.path().join("B.md"), "user modified while recovering").unwrap();
    let resumed = call(
        &mut a,
        "note_rename",
        json!({"resumeId":r["error"]["detail"]["journalId"]}),
    );
    assert_eq!(resumed["error"]["code"], "E_PARTIAL_RENAME");
    assert_eq!(
        fs::read_to_string(v.path().join("B.md")).unwrap(),
        "user modified while recovering"
    );
}
#[test]
fn user_edits_to_completed_references_do_not_block_remaining_recovery() {
    let v = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    fs::write(v.path().join("A.md"), "# A").unwrap();
    fs::write(v.path().join("0.md"), "[[A]]").unwrap();
    fs::write(v.path().join("B.md"), "[[A]]").unwrap();
    let fail = Arc::new(AtomicBool::new(true));
    let mut a = app(v.path(), data.path(), fail);
    let plan = call(
        &mut a,
        "note_rename",
        json!({"path":"A.md","newName":"C","planOnly":true}),
    );
    let interrupted = call(
        &mut a,
        "note_rename",
        json!({"path":"A.md","newName":"C","planOnly":false,"planId":plan["data"]["plan"]["id"]}),
    );
    assert_eq!(interrupted["error"]["code"], "E_PARTIAL_RENAME");
    assert_eq!(fs::read_to_string(v.path().join("0.md")).unwrap(), "[[C]]");
    fs::write(
        v.path().join("0.md"),
        "user edits the already-completed reference",
    )
    .unwrap();
    let resumed = call(
        &mut a,
        "note_rename",
        json!({"resumeId":interrupted["error"]["detail"]["journalId"]}),
    );
    assert_eq!(resumed["ok"], true, "{resumed}");
    assert_eq!(
        fs::read_to_string(v.path().join("0.md")).unwrap(),
        "user edits the already-completed reference"
    );
    assert_eq!(fs::read_to_string(v.path().join("B.md")).unwrap(), "[[C]]");
}
#[test]
fn completed_source_self_link_may_be_edited_before_remaining_recovery() {
    let v = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    fs::write(v.path().join("A.md"), "# A\n[[A]]").unwrap();
    fs::write(v.path().join("B.md"), "[[A]]").unwrap();
    let mut a = app(v.path(), data.path(), Arc::new(AtomicBool::new(true)));
    let p = call(
        &mut a,
        "note_rename",
        json!({"path":"A.md","newName":"C","planOnly":true}),
    );
    let r = call(
        &mut a,
        "note_rename",
        json!({"path":"A.md","newName":"C","planOnly":false,"planId":p["data"]["plan"]["id"]}),
    );
    assert_eq!(r["error"]["code"], "E_PARTIAL_RENAME");
    assert_eq!(
        fs::read_to_string(v.path().join("C.md")).unwrap(),
        "# A\n[[C]]"
    );
    fs::write(v.path().join("C.md"), "# A\n[[C]]\nuser appended").unwrap();
    let done = call(
        &mut a,
        "note_rename",
        json!({"resumeId":r["error"]["detail"]["journalId"]}),
    );
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(
        fs::read_to_string(v.path().join("C.md")).unwrap(),
        "# A\n[[C]]\nuser appended"
    );
    assert_eq!(fs::read_to_string(v.path().join("B.md")).unwrap(), "[[C]]");
}
