use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;
use unicode_normalization::UnicodeNormalization;

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
pub const PREVIEW_SIZE_LIMIT: usize = 256 * 1024;
pub const MAX_PATH_UNITS: usize = 260;

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
        #[cfg(windows)]
        if matches!(e.raw_os_error(), Some(32) | Some(33)) {
            let mut error = Self::new("E_IO_LOCKED", e.to_string());
            error.retryable = true;
            return error;
        }
        let code = match e.kind() {
            std::io::ErrorKind::NotFound => "E_NOT_FOUND",
            std::io::ErrorKind::PermissionDenied => "E_IO_PERMISSION",
            std::io::ErrorKind::WouldBlock => "E_IO_LOCKED",
            std::io::ErrorKind::StorageFull => "E_IO_DISK_FULL",
            _ => "E_INTERNAL",
        };
        let mut error = Self::new(code, e.to_string());
        error.retryable = code == "E_IO_LOCKED";
        error
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
    if path.encode_utf16().count() >= MAX_PATH_UNITS {
        return Err(AppError::new(
            "E_PATH_INVALID",
            "路径超过 259 个 UTF-16 单元",
        ));
    }
    for original in path.split('/') {
        // 校验 NFKC 视图，但保留磁盘原名；不能把一个已有文件悄悄映射到另一个文件。
        let part: String = original.nfkc().collect();
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with('.')
            || part.ends_with(' ')
            || original.encode_utf16().count() > 255
            || part
                .chars()
                .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
        {
            return Err(AppError::new("E_PATH_INVALID", "路径包含非法字符或片段"));
        }
        if is_reserved(&part) {
            return Err(AppError::new("E_PATH_INVALID", "不能使用系统保留名称"));
        }
    }
    Ok(())
}

fn is_reserved(part: &str) -> bool {
    let name = part
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    matches!(name.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (name.len() == 4
            && (name.starts_with("COM") || name.starts_with("LPT"))
            && matches!(name.as_bytes()[3], b'1'..=b'9'))
}

/// 仅用于用户新建标题。读取/保存已有路径始终严格校验，不能用此函数修复路径。
pub fn sanitize_title(input: &str) -> Result<String> {
    if input.trim().is_empty() {
        return Err(AppError::new("E_PATH_INVALID", "标题不能为空"));
    }
    let mut name: String = input
        .trim()
        .nfkc()
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    name = name.trim_end_matches(['.', ' ']).to_owned();
    if name.is_empty() {
        name = "未命名".into();
    }
    if is_reserved(&name) {
        name.insert(0, '_');
    }
    validate_rel(&name, false)?;
    Ok(name)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelPath(String);

impl RelPath {
    pub fn new(path: &str) -> Result<Self> {
        validate_rel(path, false)?;
        Ok(Self(path.into()))
    }

    pub fn root() -> Self {
        Self(String::new())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
pub fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::new("E_INTERNAL", format!("请求字段 {key} 缺失或类型错误")))
}

pub struct StoredNote {
    pub raw: Vec<u8>,
    pub mtime_ms: u64,
    pub size_bytes: u64,
    pub truncated: bool,
    pub read_only: bool,
}
#[derive(Debug)]
pub struct Entry {
    pub path: String,
    pub mtime_ms: u64,
}
#[derive(Debug)]
pub struct FileStat {
    pub size_bytes: u64,
    pub mtime_ms: u64,
    pub read_only: bool,
}

#[derive(Debug)]
pub struct WriteOutcome {
    pub content_hash: String,
    pub bytes_written: usize,
    pub mtime_ms: u64,
}

#[derive(Debug)]
pub enum StoreError {
    AlreadyExists,
    Failure(AppError),
}

impl From<AppError> for StoreError {
    fn from(error: AppError) -> Self {
        Self::Failure(error)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            Self::AlreadyExists
        } else {
            Self::Failure(AppError::io(error))
        }
    }
}

impl From<StoreError> for AppError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::AlreadyExists => Self::new("E_PATH_INVALID", "目标已存在，不能覆盖"),
            StoreError::Failure(error) => error,
        }
    }
}

pub type StoreResult<T> = std::result::Result<T, StoreError>;

// 字节保真读写边界：编码/换行和 frontmatter 的编辑语义由 application 编排。
// delete 是物理删除原语，普通笔记删除由 note-svc 使用 rename 移至 trash/。
pub trait NoteStore: Send {
    fn read(&self, path: &RelPath) -> StoreResult<StoredNote>;
    fn write_atomic(&self, path: &RelPath, body: &[u8]) -> StoreResult<WriteOutcome>;
    fn create_new(&self, path: &RelPath) -> StoreResult<()>;
    fn rename(&self, from: &RelPath, to: &RelPath) -> StoreResult<()>;
    fn delete(&self, path: &RelPath) -> StoreResult<()>;
    fn list(&self, root: &RelPath) -> StoreResult<Vec<Entry>>;
    fn stat(&self, path: &RelPath) -> StoreResult<FileStat>;
    fn vault_path(&self) -> String;
}
