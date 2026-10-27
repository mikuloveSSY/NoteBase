use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

pub const COMMANDS: &[&str] = &[
    "vault_open",
    "vault_info",
    "note_list",
    "note_read",
    "note_save",
    "note_create",
    "note_rename",
    "note_delete",
    "note_restore",
    "search",
    "link_backlinks",
    "link_mentions",
    "link_graph",
    "index_status",
    "index_rebuild",
    "export_note",
    "backup_create",
    "backup_restore",
    "sync_configure",
    "sync_now",
    "sync_status",
    "settings_get",
    "settings_set",
];
pub const NOTE_SIZE_LIMIT: usize = 10 * 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: String,
    pub command: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub client_seq: u64,
}
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum Response {
    Ok {
        id: String,
        ok: bool,
        data: Value,
    },
    Err {
        id: String,
        ok: bool,
        error: AppError,
    },
}
#[derive(Debug, Serialize)]
pub struct AppError {
    pub code: String,
    pub message: String,
    pub detail: Value,
    pub retryable: bool,
}
impl AppError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            detail: json!({}),
            retryable: false,
        }
    }
    pub fn io(e: std::io::Error) -> Self {
        let code = match e.kind() {
            std::io::ErrorKind::NotFound => "E_NOT_FOUND",
            std::io::ErrorKind::PermissionDenied => "E_IO_PERMISSION",
            std::io::ErrorKind::WouldBlock => "E_IO_LOCKED",
            _ if matches!(e.raw_os_error(), Some(28) | Some(112)) => "E_IO_DISK_FULL",
            _ => "E_INTERNAL",
        };
        Self::new(code, e.to_string())
    }
}
pub type Result<T> = std::result::Result<T, AppError>;

// 领域层只处理值和规则，不访问磁盘。
pub fn validate_rel(path: &str, allow_empty: bool) -> Result<()> {
    if path.is_empty() && allow_empty {
        return Ok(());
    }
    if path.is_empty() || Path::new(path).is_absolute() || path.contains('\\') {
        return Err(AppError::new(
            "E_PATH_INVALID",
            "必须使用库内相对路径和 / 分隔符",
        ));
    }
    for part in path.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with('.')
            || part.ends_with(' ')
            || part
                .chars()
                .any(|c| c.is_control() || "<>:\"|?*".contains(c))
        {
            return Err(AppError::new("E_PATH_INVALID", "路径包含非法字符或片段"));
        }
        let name = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        if matches!(name.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (name.len() == 4
                && (name.starts_with("COM") || name.starts_with("LPT"))
                && matches!(name.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(AppError::new("E_PATH_INVALID", "不能使用系统保留名称"));
        }
    }
    Ok(())
}
pub fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::new("E_INTERNAL", format!("请求字段 {key} 缺失或类型错误")))
}

pub struct StoredNote {
    pub raw: Vec<u8>,
    pub mtime_ms: u64,
}
pub struct Entry {
    pub path: String,
    pub mtime_ms: u64,
}
// 基础工程仅接通 NoteStore 的读、写、新建、枚举和 stat 所需能力；后续补齐原设计端口。
pub trait NoteStore: Send {
    fn read(&self, path: &str) -> Result<StoredNote>;
    fn write_atomic(&self, path: &str, body: &[u8]) -> Result<()>;
    fn create_new(&self, path: &str) -> std::io::Result<()>;
    fn list(&self) -> Result<Vec<Entry>>;
    fn vault_path(&self) -> String;
}
