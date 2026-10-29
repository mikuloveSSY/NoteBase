use crate::domain::{AppError, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File, Metadata};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use tempfile::NamedTempFile;

const MAX_STATE_BYTES: u64 = 64 * 1024 * 1024;
const JSON_SUFFIX: &str = ".json";

/// Private, per-vault state used to resume recoverable note operations.
///
/// Implementations deliberately expose only the two state buckets used by the
/// note service. The port is synchronous because callers already serialize
/// mutations at the application boundary.
pub trait NoteStateStore: Send {
    fn read(&self, bucket: &str, key: &str) -> crate::domain::Result<Option<serde_json::Value>>;
    fn write(&self, bucket: &str, key: &str, value: &Value) -> crate::domain::Result<()>;
    fn remove(&self, bucket: &str, key: &str) -> crate::domain::Result<()>;
    fn list(&self, bucket: &str) -> crate::domain::Result<Vec<Value>>;
}

/// JSON state persisted below a private directory outside the vault.
#[derive(Debug)]
pub struct FsNoteStateStore {
    root: PathBuf,
}

impl FsNoteStateStore {
    /// Opens (and, when needed, creates) the private state directory.
    ///
    /// Every directory component is inspected with symlink_metadata before it
    /// is used. This matters on Windows because directory junctions are
    /// reparse points even though file_type().is_symlink() is false.
    pub fn open(root: &Path) -> Result<Self> {
        if root.as_os_str().is_empty() {
            return Err(invalid_path("恢复状态根目录不能为空"));
        }
        ensure_directory_chain(root)?;
        let store = Self {
            root: root.to_path_buf(),
        };
        store.ensure_bucket("drafts")?;
        store.ensure_bucket("renames")?;
        Ok(store)
    }

    fn ensure_bucket(&self, bucket: &str) -> Result<PathBuf> {
        validate_bucket(bucket)?;
        ensure_directory_chain(&self.root)?;
        let path = self.root.join(bucket);
        ensure_directory_chain(&path)?;
        Ok(path)
    }

    fn target_path(&self, bucket: &str, key: &str) -> Result<PathBuf> {
        validate_key(key)?;
        let directory = self.ensure_bucket(bucket)?;
        Ok(directory.join(format!("{key}{JSON_SUFFIX}")))
    }
}

impl NoteStateStore for FsNoteStateStore {
    fn read(&self, bucket: &str, key: &str) -> Result<Option<Value>> {
        let path = self.target_path(bucket, key)?;
        let Some(metadata) = state_metadata(&path)? else {
            return Ok(None);
        };
        Ok(Some(read_value(&path, metadata)?))
    }

    fn write(&self, bucket: &str, key: &str, value: &Value) -> Result<()> {
        validate_key(key)?;
        let bytes = serde_json::to_vec(value)
            .map_err(|error| AppError::new("E_INTERNAL", format!("恢复记录序列化失败：{error}")))?;
        ensure_size(bytes.len() as u64)?;

        let path = self.target_path(bucket, key)?;
        // Never replace a link, directory, corrupt record, or oversized record.
        // Keeping the old record visible lets recovery surface the problem.
        if let Some(metadata) = state_metadata(&path)? {
            let _ = read_value(&path, metadata)?;
        }
        let parent = path
            .parent()
            .ok_or_else(|| invalid_path("恢复记录没有父目录"))?;
        let mut temporary = NamedTempFile::new_in(parent).map_err(AppError::io)?;
        temporary
            .as_file_mut()
            .write_all(&bytes)
            .map_err(AppError::io)?;
        temporary.as_file().sync_all().map_err(AppError::io)?;
        temporary
            .persist(&path)
            .map_err(|error| AppError::io(error.error))?;
        sync_directory(parent).map_err(AppError::io)?;
        Ok(())
    }

    fn remove(&self, bucket: &str, key: &str) -> Result<()> {
        let path = self.target_path(bucket, key)?;
        let Some(metadata) = state_metadata(&path)? else {
            return Ok(());
        };
        let _ = read_value(&path, metadata)?;
        fs::remove_file(&path).map_err(AppError::io)?;
        let parent = path
            .parent()
            .ok_or_else(|| invalid_path("恢复记录没有父目录"))?;
        sync_directory(parent).map_err(AppError::io)?;
        Ok(())
    }

    fn list(&self, bucket: &str) -> Result<Vec<Value>> {
        let directory = self.ensure_bucket(bucket)?;
        let mut entries = Vec::new();
        let directory_entries = fs::read_dir(&directory).map_err(AppError::io)?;
        for entry in directory_entries {
            let entry = entry.map_err(AppError::io)?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(AppError::io)?;
            if is_link(&metadata) {
                return Err(invalid_path("恢复状态目录不能包含符号链接或目录联接"));
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| invalid_path("恢复记录文件名必须是UTF-8"))?;
            if !name.ends_with(JSON_SUFFIX) {
                continue;
            }
            if !metadata.is_file() {
                return Err(invalid_path("恢复记录目标不是普通文件"));
            }
            let key = name
                .strip_suffix(JSON_SUFFIX)
                .ok_or_else(|| invalid_path("恢复记录扩展名无效"))?;
            validate_key(key)?;
            entries.push((key.to_owned(), path, metadata));
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));

        let mut values = Vec::with_capacity(entries.len());
        for (_, path, metadata) in entries {
            values.push(read_value(&path, metadata)?);
        }
        Ok(values)
    }
}

/// In-memory state for pure/local application tests and ephemeral callers.
#[derive(Debug, Default)]
pub struct MemoryNoteStateStore {
    values: Mutex<BTreeMap<(String, String), Value>>,
}

impl NoteStateStore for MemoryNoteStateStore {
    fn read(&self, bucket: &str, key: &str) -> Result<Option<Value>> {
        validate_bucket(bucket)?;
        validate_key(key)?;
        let values = self
            .values
            .lock()
            .map_err(|_| AppError::new("E_INTERNAL", "恢复状态锁已损坏"))?;
        Ok(values.get(&(bucket.to_owned(), key.to_owned())).cloned())
    }

    fn write(&self, bucket: &str, key: &str, value: &Value) -> Result<()> {
        validate_bucket(bucket)?;
        validate_key(key)?;
        let bytes = serde_json::to_vec(value)
            .map_err(|error| AppError::new("E_INTERNAL", format!("恢复记录序列化失败：{error}")))?;
        ensure_size(bytes.len() as u64)?;
        let mut values = self
            .values
            .lock()
            .map_err(|_| AppError::new("E_INTERNAL", "恢复状态锁已损坏"))?;
        values.insert((bucket.to_owned(), key.to_owned()), value.clone());
        Ok(())
    }

    fn remove(&self, bucket: &str, key: &str) -> Result<()> {
        validate_bucket(bucket)?;
        validate_key(key)?;
        let mut values = self
            .values
            .lock()
            .map_err(|_| AppError::new("E_INTERNAL", "恢复状态锁已损坏"))?;
        values.remove(&(bucket.to_owned(), key.to_owned()));
        Ok(())
    }

    fn list(&self, bucket: &str) -> Result<Vec<Value>> {
        validate_bucket(bucket)?;
        let values = self
            .values
            .lock()
            .map_err(|_| AppError::new("E_INTERNAL", "恢复状态锁已损坏"))?;
        Ok(values
            .iter()
            .filter(|((stored_bucket, _), _)| stored_bucket == bucket)
            .map(|(_, value)| value.clone())
            .collect())
    }
}

fn validate_bucket(bucket: &str) -> Result<()> {
    if matches!(bucket, "drafts" | "renames") {
        Ok(())
    } else {
        Err(invalid_path("恢复状态分区无效"))
    }
}

fn validate_key(key: &str) -> Result<()> {
    // Restrict keys to a portable filename alphabet. In particular, do not
    // accept separators, control bytes, Windows device names, or . and ...
    if key.is_empty()
        || key.len() > 250
        || !key.is_ascii()
        || key == "."
        || key == ".."
        || key.ends_with('.')
        || key.ends_with(' ')
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(invalid_path("恢复记录键必须是安全ASCII文件名"));
    }
    let device = key.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (device.len() == 4
            && (device.starts_with("COM") || device.starts_with("LPT"))
            && matches!(device.as_bytes()[3], b'1'..=b'9'))
    {
        return Err(invalid_path("恢复记录键不能使用系统设备名称"));
    }
    Ok(())
}

fn ensure_size(size: u64) -> Result<()> {
    if size > MAX_STATE_BYTES {
        Err(AppError::new("E_FILE_TOO_LARGE", "恢复记录超过64 MiB"))
    } else {
        Ok(())
    }
}

fn state_metadata(path: &Path) -> Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if is_link(&metadata) {
                return Err(invalid_path("恢复状态不能通过符号链接或目录联接访问"));
            }
            if !metadata.is_file() {
                return Err(invalid_path("恢复记录目标不是普通文件"));
            }
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AppError::io(error)),
    }
}

fn read_value(path: &Path, metadata: Metadata) -> Result<Value> {
    ensure_size(metadata.len())?;
    let file = File::open(path).map_err(AppError::io)?;
    let file_metadata = file.metadata().map_err(AppError::io)?;
    if !file_metadata.is_file() {
        return Err(invalid_path("恢复记录目标不是普通文件"));
    }
    ensure_size(file_metadata.len())?;

    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(AppError::io)?;
    ensure_size(bytes.len() as u64)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AppError::new("E_FORMAT_UNSUPPORTED", format!("恢复记录JSON损坏：{error}"))
    })
}

fn ensure_directory_chain(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Err(invalid_path("恢复状态目录不能为空"));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(invalid_path("恢复状态目录不能包含父级路径"));
    }

    let mut ancestors: Vec<PathBuf> = path
        .ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .collect();
    ancestors.reverse();
    for current in ancestors {
        match fs::symlink_metadata(&current) {
            Ok(metadata) => check_directory(&current, &metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(AppError::io)?;
                let metadata = fs::symlink_metadata(&current).map_err(AppError::io)?;
                check_directory(&current, &metadata)?;
            }
            Err(error) => return Err(AppError::io(error)),
        }
    }
    Ok(())
}

fn check_directory(path: &Path, metadata: &Metadata) -> Result<()> {
    if is_link(metadata) {
        return Err(invalid_path(format!(
            "恢复状态目录包含符号链接或目录联接：{}",
            path.display()
        )));
    }
    if !metadata.is_dir() {
        return Err(invalid_path("恢复状态路径中的父级不是目录"));
    }
    Ok(())
}

fn invalid_path(message: impl Into<String>) -> AppError {
    AppError::new("E_PATH_INVALID", message)
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT also covers directory junctions.
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
