use crate::domain::{
    AppError, Entry, FileStat, NoteStore, RelPath, Result, StoreError, StoreResult, StoredNote,
    WriteOutcome, MAX_PATH_UNITS, NOTE_SIZE_LIMIT, PREVIEW_SIZE_LIMIT,
};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use tempfile::NamedTempFile;
use walkdir::WalkDir;

pub struct FsStore {
    root: PathBuf,
    scan_cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl FsStore {
    pub fn open(path: &str) -> Result<Self> {
        if !Path::new(path).is_absolute() {
            return Err(AppError::new(
                "E_PATH_INVALID",
                "请输入已有文件夹的绝对路径",
            ));
        }
        let root = fs::canonicalize(path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AppError::new("E_VAULT_NOT_FOUND", "笔记库目录不存在，请先建立文件夹")
            } else {
                let mut error = AppError::io(e);
                error.message = format!("无法访问所选笔记库文件夹：{}", error.message);
                error
            }
        })?;
        if !root.is_dir() {
            return Err(AppError::new("E_VAULT_NOT_FOUND", "笔记库必须是文件夹"));
        }
        Ok(Self {
            root,
            scan_cancelled: None,
        })
    }

    fn resolve(&self, path: &RelPath) -> StoreResult<PathBuf> {
        let joined = self.root.join(path.as_str());
        // MAX_PATH 兼容档；canonicalize 的 Windows 扩展前缀不计入用户路径。
        let display = joined.to_string_lossy();
        let logical = display
            .strip_prefix(r"\\?\UNC\")
            .map(|p| format!(r"\\{p}"))
            .unwrap_or_else(|| display.strip_prefix(r"\\?\").unwrap_or(&display).to_owned());
        if logical.encode_utf16().count() >= MAX_PATH_UNITS {
            return Err(AppError::new("E_PATH_INVALID", "完整路径超过 259 个 UTF-16 单元").into());
        }
        let mut current = self.root.clone();
        for segment in path.as_str().split('/').filter(|s| !s.is_empty()) {
            current.push(segment);
            match fs::symlink_metadata(&current) {
                Ok(meta) => {
                    if is_link(&meta) {
                        return Err(
                            AppError::new("E_PATH_INVALID", "不操作符号链接或目录联接").into()
                        );
                    }
                    if !fs::canonicalize(&current)?.starts_with(&self.root) {
                        return Err(AppError::new("E_PATH_INVALID", "路径超出笔记库").into());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(joined)
    }

    fn resolve_note(&self, path: &RelPath) -> StoreResult<PathBuf> {
        if path.as_str().is_empty() || !path.as_str().to_ascii_lowercase().ends_with(".md") {
            return Err(AppError::new("E_PATH_INVALID", "笔记必须是 .md 文件").into());
        }
        let resolved = self.resolve(path)?;
        if let Ok(meta) = fs::metadata(&resolved) {
            require_file(&meta)?;
        }
        Ok(resolved)
    }

    fn replace_with(
        &self,
        path: &RelPath,
        write: impl FnOnce(&mut File) -> std::io::Result<()>,
    ) -> StoreResult<()> {
        let path = self.resolve_note(path)?;
        let parent = path
            .parent()
            .ok_or_else(|| AppError::new("E_PATH_INVALID", "无父目录"))?;
        let meta = fs::metadata(&path)?;
        require_file(&meta)?;
        if meta.permissions().readonly() {
            return Err(AppError::new("E_IO_PERMISSION", "文件是只读的，不能保存").into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_GENERIC_WRITE};
            // 替换前探测写入/删除共享权限，区分锁冲突与 ACL 拒绝，不修改原文件。
            drop(
                fs::OpenOptions::new()
                    .access_mode(FILE_GENERIC_WRITE | DELETE)
                    .open(&path)?,
            );
        }
        let mut tmp = NamedTempFile::new_in(parent)?;
        write(tmp.as_file_mut())?;
        tmp.as_file().set_permissions(meta.permissions())?;
        tmp.as_file().sync_all()?;
        tmp.persist(&path).map_err(|e| StoreError::from(e.error))?;
        sync_directory(parent)?;
        Ok(())
    }
}

fn require_file(meta: &fs::Metadata) -> Result<()> {
    if !meta.is_file() {
        return Err(AppError::new("E_PATH_INVALID", "目标不是普通文件"));
    }
    Ok(())
}

fn is_link(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Windows junction 也是重解析点，不能只检查 file_type().is_symlink()。
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

fn mtime(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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

impl NoteStore for FsStore {
    fn set_scan_cancel(&mut self, cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.scan_cancelled = Some(cancelled);
    }
    fn vault_path(&self) -> String {
        self.root.to_string_lossy().into()
    }

    fn read(&self, path: &RelPath) -> StoreResult<StoredNote> {
        let file = File::open(self.resolve_note(path)?)?;
        let meta = file.metadata()?;
        require_file(&meta)?;
        let large = meta.len() > NOTE_SIZE_LIMIT as u64;
        let limit = if large {
            PREVIEW_SIZE_LIMIT
        } else {
            NOTE_SIZE_LIMIT + 1
        };
        let mut raw = Vec::new();
        file.take(limit as u64).read_to_end(&mut raw)?;
        // 文件可在 stat 之后增长，仍必须限制读取内存，并标成只读预览。
        let truncated = large || raw.len() > NOTE_SIZE_LIMIT;
        let size_bytes = meta.len().max(raw.len() as u64);
        if truncated {
            raw.truncate(PREVIEW_SIZE_LIMIT);
        }
        Ok(StoredNote {
            raw,
            mtime_ms: mtime(&meta),
            size_bytes,
            truncated,
            read_only: meta.permissions().readonly(),
        })
    }

    fn create_new(&self, path: &RelPath) -> StoreResult<()> {
        let path = self.resolve_note(path)?;
        // 临时文件在失败时自动清理；persist_noclobber 原子拒绝同名。
        let parent = path
            .parent()
            .ok_or_else(|| AppError::new("E_PATH_INVALID", "无父目录"))?;
        let tmp = NamedTempFile::new_in(parent)?;
        tmp.as_file().sync_all()?;
        tmp.persist_noclobber(&path)
            .map_err(|e| StoreError::from(e.error))?;
        sync_directory(parent)?;
        Ok(())
    }

    fn write_atomic(&self, path: &RelPath, body: &[u8]) -> StoreResult<WriteOutcome> {
        if body.len() > NOTE_SIZE_LIMIT {
            return Err(AppError::new("E_FILE_TOO_LARGE", "保存内容超过 10 MB").into());
        }
        if self.stat(path)?.size_bytes > NOTE_SIZE_LIMIT as u64 {
            return Err(AppError::new("E_FILE_TOO_LARGE", "超大文件只能预览，不能保存").into());
        }
        self.replace_with(path, |file| file.write_all(body))?;
        Ok(WriteOutcome {
            // 返回本次写入的哈希，不把保存后外部改写的数据误报成我们写入的内容。
            content_hash: format!("sha256:{:x}", Sha256::digest(body)),
            bytes_written: body.len(),
            mtime_ms: self.stat(path)?.mtime_ms,
        })
    }

    fn rename(&self, from: &RelPath, to: &RelPath) -> StoreResult<()> {
        let source = self.resolve_note(from)?;
        require_file(&fs::metadata(&source)?)?;
        if from == to {
            return Ok(());
        }
        let target = self.resolve_note(to)?;
        rename_no_replace(&source, &target)?;
        sync_directory(source.parent().unwrap())?;
        if source.parent() != target.parent() {
            sync_directory(target.parent().unwrap())?;
        }
        Ok(())
    }

    fn delete(&self, path: &RelPath) -> StoreResult<()> {
        let path = self.resolve_note(path)?;
        require_file(&fs::metadata(&path)?)?;
        fs::remove_file(&path)?;
        sync_directory(path.parent().unwrap())?;
        Ok(())
    }

    fn list(&self, root: &RelPath) -> StoreResult<Vec<Entry>> {
        // trash 与 attachments 只在库根目录具有系统目录含义。
        if matches!(
            root.as_str().split('/').next(),
            Some("trash" | "attachments" | ".git")
        ) {
            return Ok(Vec::new());
        }
        let scan_root = self.resolve(root)?;
        if !fs::metadata(&scan_root)?.is_dir() {
            return Err(AppError::new("E_PATH_INVALID", "列表根路径必须是文件夹").into());
        }
        let mut entries = Vec::new();
        let mut walk = WalkDir::new(&scan_root).follow_links(false).into_iter();
        loop {
            // Check even between ignored files, not just between Markdown notes.
            if self
                .scan_cancelled
                .as_ref()
                .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
            {
                return Err(AppError::new("E_CANCELLED", "目录扫描已取消，原文件未修改").into());
            }
            let Some(entry) = walk.next() else {
                break;
            };
            let entry = entry.map_err(|e| {
                let location = e
                    .path()
                    .and_then(|path| path.strip_prefix(&self.root).ok())
                    .filter(|path| !path.as_os_str().is_empty())
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "所选文件夹".to_owned());
                let mut error = e
                    .into_io_error()
                    .map(AppError::io)
                    .unwrap_or_else(|| AppError::new("E_IO_PERMISSION", "无法枚举库目录"));
                error.message = format!("无法扫描笔记库中的「{location}」：{}", error.message);
                if error.code == "E_IO_PERMISSION" {
                    error
                        .message
                        .push_str("。请选择有读取权限的专用笔记文件夹，或检查此目录的读取权限");
                }
                error
            })?;
            let markdown = entry
                .path()
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("md"));
            // Non-note files need no metadata probing, canonicalization or reads.
            if !entry.file_type().is_dir() && !markdown {
                continue;
            }
            if entry.depth() > 0
                && (fs::symlink_metadata(entry.path())
                    .map(|m| is_link(&m))
                    .unwrap_or(false)
                    || (entry.path().parent() == Some(self.root.as_path())
                        && matches!(
                            entry.file_name().to_str(),
                            Some("trash" | "attachments" | ".git")
                        )))
            {
                if entry.file_type().is_dir() {
                    walk.skip_current_dir();
                }
                continue;
            }
            if !entry.file_type().is_file()
                || !entry
                    .path()
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.eq_ignore_ascii_case("md"))
            {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&self.root)
                .map_err(|_| AppError::new("E_PATH_INVALID", "路径超出笔记库"))?
                .to_string_lossy()
                .replace('\\', "/");
            // 忽略不能安全操作的现有路径，但不改写或删除它们。
            if let Ok(path) = RelPath::new(&relative) {
                match self.stat(&path) {
                    Ok(meta) => entries.push(Entry {
                        path: relative,
                        mtime_ms: meta.mtime_ms,
                    }),
                    Err(StoreError::Failure(error))
                        if error.code == "E_PATH_INVALID" || error.code == "E_NOT_FOUND" => {}
                    Err(error) => return Err(error),
                }
            }
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }

    fn stat(&self, path: &RelPath) -> StoreResult<FileStat> {
        let meta = fs::metadata(self.resolve_note(path)?)?;
        require_file(&meta)?;
        Ok(FileStat {
            size_bytes: meta.len(),
            mtime_ms: mtime(&meta),
            read_only: meta.permissions().readonly(),
        })
    }
}

#[cfg(windows)]
fn rename_no_replace(from: &Path, to: &Path) -> StoreResult<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: 两缓冲区以 NUL 结尾，生命周期覆盖调用。没有 REPLACE_EXISTING 标记。
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn rename_no_replace(from: &Path, to: &Path) -> StoreResult<()> {
    // 同文件系统硬链接原子拒绝同名；移除源名失败时保留两份入口供业务日志恢复。
    // 此降级不保证两个目录项的原子切换，不代表 macOS/Linux 已验收。
    fs::hard_link(from, to)?;
    fs::remove_file(from).map_err(|e| {
        let mut error = AppError::new(
            "E_PARTIAL_RENAME",
            format!("目标已建立，源路径移除失败：{e}"),
        );
        error.detail = serde_json::json!({"from":from,"to":to,"sourceRetained":true});
        StoreError::from(error)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_atomic_write_preserves_original_and_cleans_temp() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("A.md"), "原内容").unwrap();
        let store = FsStore::open(dir.path().to_str().unwrap()).unwrap();
        let error = store
            .replace_with(&RelPath::new("A.md").unwrap(), |file| {
                file.write_all(b"half-written")?;
                Err(std::io::Error::from(std::io::ErrorKind::StorageFull))
            })
            .unwrap_err();
        assert_eq!(AppError::from(error).code, "E_IO_DISK_FULL");
        assert_eq!(
            fs::read_to_string(dir.path().join("A.md")).unwrap(),
            "原内容"
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
