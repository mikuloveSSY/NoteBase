//! Member-three ports. Business dispatch never accesses disk directly.
pub mod backup;
pub mod files;
mod pdf;
pub mod render;
pub mod sync;
pub mod versions;
use crate::domain::Result;
use serde_json::Value;

pub trait DataService: Send {
    /// Cooperative only: the owner must retain its lease until the job returns.
    fn set_cancel_flag(&mut self, _flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {}
    fn export(&self, payload: &Value) -> Result<Value>;
    fn backup_create(&self, payload: &Value) -> Result<Value>;
    fn backup_restore(&self, payload: &Value) -> Result<Value>;
    fn versions(&self, payload: &Value) -> Result<Value>;
    fn version_body(&self, payload: &Value) -> Result<String>;
    fn snapshot(&self, path: &str, bytes: &[u8], note: &str) -> Result<Value>;
    fn settings(&self) -> Result<Value>;
    fn configure(&self, values: &Value) -> Result<Value>;
    fn maintenance(&self, now: u64) -> Result<Value>;
    fn maintenance_due(&self, now: u64) -> Result<bool>;
    fn prune_versions(&self, now: u64) -> Result<usize>;
    fn sync_configure(&self, p: &Value) -> Result<Value>;
    fn sync_now(&self, p: &Value) -> Result<Value>;
    fn sync_status(&self) -> Result<Value>;
}
pub struct LocalDataService {
    pub(crate) cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) files: files::Files,
    pub(crate) private: std::path::PathBuf,
    pub(crate) backups: std::path::PathBuf,
}
impl LocalDataService {
    pub fn open(vault: &str, data_root: &std::path::Path, id: &str) -> Result<Self> {
        let private = crate::note_files::private_state_root(data_root, vault, id)?;
        files::directory(&private)?;
        let backups = data_root.join("backups").join(id);
        Ok(Self {
            cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            files: files::Files::open(vault)?,
            private,
            backups,
        })
    }
}
impl LocalDataService {
    pub(crate) fn check_cancelled(&self) -> Result<()> {
        if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Err(crate::domain::AppError::new(
                "E_CANCELLED",
                "数据任务已取消，已完成的文件操作保留并核对",
            ));
        }
        Ok(())
    }
}
impl DataService for LocalDataService {
    fn set_cancel_flag(&mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.cancelled = flag;
    }
    fn export(&self, p: &Value) -> Result<Value> {
        render::export(self, p)
    }
    fn backup_create(&self, p: &Value) -> Result<Value> {
        if p.get("scope").and_then(Value::as_str) == Some("note") {
            let path = crate::domain::field(p, "path")?;
            return self.snapshot(
                path,
                &self.files.read_note(path)?,
                p.get("description").and_then(Value::as_str).unwrap_or(""),
            );
        }
        backup::create(self, p)
    }
    fn backup_restore(&self, p: &Value) -> Result<Value> {
        backup::restore(self, p)
    }
    fn versions(&self, p: &Value) -> Result<Value> {
        versions::list(self, p)
    }
    fn version_body(&self, p: &Value) -> Result<String> {
        versions::body(self, p)
    }
    fn snapshot(&self, path: &str, bytes: &[u8], note: &str) -> Result<Value> {
        versions::snapshot(self, path, bytes, note)
    }
    fn settings(&self) -> Result<Value> {
        backup::settings(self)
    }
    fn configure(&self, p: &Value) -> Result<Value> {
        backup::configure(self, p)
    }
    fn maintenance(&self, now: u64) -> Result<Value> {
        backup::maintenance(self, now)
    }
    fn maintenance_due(&self, now: u64) -> Result<bool> {
        backup::due(self, now)
    }
    fn prune_versions(&self, now: u64) -> Result<usize> {
        versions::prune(self, now)
    }
    fn sync_configure(&self, p: &Value) -> Result<Value> {
        self.sync_configure_with(p, &sync::OsCredentials)
    }
    fn sync_now(&self, p: &Value) -> Result<Value> {
        self.sync_run(p)
    }
    fn sync_status(&self) -> Result<Value> {
        LocalDataService::sync_status(self)
    }
}
