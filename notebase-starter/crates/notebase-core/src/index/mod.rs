//! 索引基础端口；查询语法、链接业务和增量队列在后续迭代接入。
pub mod extract;
pub mod incremental;
pub mod queue;
pub mod recovery;
pub mod search;
pub mod sqlite;
pub mod watcher;
pub mod worker;

use crate::domain::{AppError, RelPath, Result};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SCHEMA_VERSION: &str = "1";
pub const TOKENIZER_VERSION: &str = "trigram-comrak-1";
pub const BATCH_SIZE: usize = 200;

#[derive(Debug, Clone)]
pub struct NoteRecord {
    pub path: RelPath,
    pub title: String,
    pub body: String,
    pub size_bytes: u64,
    pub mtime_ms: u64,
    pub content_hash: String,
    pub created_ms: Option<i64>,
    pub updated_ms: Option<i64>,
    pub has_attachment: bool,
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IndexState {
    Ready,
    Rebuilding,
    Offline,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexProgress {
    pub phase: String,
    pub done: usize,
    pub total: usize,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub state: IndexState,
    pub note_count: u64,
    pub index_bytes: u64,
    pub last_full_build_ms: Option<u64>,
    pub progress: Option<IndexProgress>,
}
impl Default for IndexStatus {
    fn default() -> Self {
        Self {
            state: IndexState::Unavailable,
            note_count: 0,
            index_bytes: 0,
            last_full_build_ms: None,
            progress: None,
        }
    }
}

// 本版落实写入、删除、读取和状态；不为尚未实现的检索/链接提供假结果。
pub trait IndexStore {
    fn search(&self, query: &serde_json::Value) -> Result<serde_json::Value>;
    fn upsert_batch(&self, notes: &[NoteRecord]) -> Result<()>;
    fn remove(&self, paths: &[RelPath]) -> Result<()>;
    fn get(&self, path: &RelPath) -> Result<Option<NoteRecord>>;
    fn status(&self) -> Result<IndexStatus>;
}

// 应用层仅依赖会话端口；后台任务不借用 App 的全局锁。
pub trait IndexSession: Send {
    /// Stop and join the sole writer before a backup/restore mutates the library.
    /// Implementations without a quiescence guarantee must fail rather than pretend to pause.
    fn suspend(&mut self) -> Result<()> {
        Err(unavailable("索引实现未提供安全暂停端口"))
    }
    fn rebuild(&self) -> Result<String>;
    fn enqueue(&self, path: &RelPath) -> bool;
    fn search(&self, query: &serde_json::Value) -> Result<serde_json::Value>;
    fn status(&self) -> IndexStatus;
    fn invalidate(&self);
    fn refresh(&self) -> bool;
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn unavailable(message: impl Into<String>) -> AppError {
    AppError::new("E_INDEX_UNAVAILABLE", message)
}
