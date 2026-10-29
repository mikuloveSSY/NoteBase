//! Filesystem additions used by note-svc; existing NoteStore remains unchanged.
use crate::domain::{AppError, RelPath, Result, StoreResult};
use serde::Serialize;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashEntry {
    pub trash_path: String,
    pub original_path: String,
    pub deleted_at_ms: u64,
}
pub trait NoteFiles: Send {
    fn prepare_parent(&self, path: &RelPath) -> Result<()>;
    fn create_note(&self, path: &RelPath, body: &[u8]) -> StoreResult<()>;
    fn trash_entries(&self) -> Result<Vec<TrashEntry>>;
    fn add_attachment(
        &self,
        name: &str,
        bytes: &[u8],
        directory: &str,
        stamp: &str,
    ) -> Result<String>;
}
pub struct FsNoteFiles {
    root: PathBuf,
}
fn linked(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}
impl FsNoteFiles {
    pub fn open(root: &str) -> Result<Self> {
        let root = fs::canonicalize(root).map_err(AppError::io)?;
        if !root.is_dir() {
            return Err(AppError::new("E_PATH_INVALID", "笔记库必须是目录"));
        }
        Ok(Self { root })
    }
    fn resolve(&self, path: &RelPath) -> Result<PathBuf> {
        let mut current = self.root.clone();
        for segment in path.as_str().split('/').filter(|p| !p.is_empty()) {
            current.push(segment);
            match fs::symlink_metadata(&current) {
                Ok(meta) => {
                    if linked(&meta)
                        || !fs::canonicalize(&current)
                            .map_err(AppError::io)?
                            .starts_with(&self.root)
                    {
                        return Err(AppError::new(
                            "E_PATH_INVALID",
                            "拒绝符号链接、目录联接或越界路径",
                        ));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(AppError::io(e)),
            }
        }
        Ok(current)
    }
}
impl NoteFiles for FsNoteFiles {
    fn create_note(&self, path: &RelPath, body: &[u8]) -> StoreResult<()> {
        if !path.as_str().to_ascii_lowercase().ends_with(".md")
            || body.len() > crate::domain::NOTE_SIZE_LIMIT
        {
            return Err(AppError::new("E_PATH_INVALID", "新笔记格式或大小无效").into());
        }
        self.prepare_parent(path)?;
        let target = self.resolve(path)?;
        let mut temp = NamedTempFile::new_in(target.parent().unwrap())?;
        temp.write_all(body)?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(&target)
            .map_err(|e| crate::domain::StoreError::from(e.error))?;
        sync_parent(&target)?;
        Ok(())
    }
    fn prepare_parent(&self, path: &RelPath) -> Result<()> {
        let parts: Vec<_> = path.as_str().split('/').collect();
        let mut rel = String::new();
        for part in parts.iter().take(parts.len().saturating_sub(1)) {
            if !rel.is_empty() {
                rel.push('/');
            }
            rel.push_str(part);
            let p = self.resolve(&RelPath::new(&rel)?)?;
            if !p.exists() {
                match fs::create_dir(&p) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(AppError::io(e)),
                }
            }
            let meta = fs::symlink_metadata(&p).map_err(AppError::io)?;
            if linked(&meta) || !meta.is_dir() {
                return Err(AppError::new("E_PATH_INVALID", "父目录不可安全使用"));
            }
        }
        self.resolve(path)?;
        Ok(())
    }
    fn trash_entries(&self) -> Result<Vec<TrashEntry>> {
        let root = self.resolve(&RelPath::new("trash")?)?;
        if !root.exists() {
            return Ok(vec![]);
        }
        if !root.is_dir() {
            return Err(AppError::new("E_PATH_INVALID", "回收站不是目录"));
        }
        let mut entries = vec![];
        for e in WalkDir::new(&root).follow_links(false) {
            let e = e.map_err(|e| AppError::new("E_IO_PERMISSION", e.to_string()))?;
            let m = fs::symlink_metadata(e.path()).map_err(AppError::io)?;
            if linked(&m) {
                return Err(AppError::new("E_PATH_INVALID", "回收站含不安全链接"));
            }
            if !m.is_file() {
                continue;
            }
            let rel = e
                .path()
                .strip_prefix(&self.root)
                .map_err(|_| AppError::new("E_PATH_INVALID", "回收站越界"))?
                .to_string_lossy()
                .replace(char::from(92), "/");
            let Some(original) = original_from_trash(&rel) else {
                continue;
            };
            let deleted = deletion_timestamp(&rel).unwrap_or(u64::MAX);
            entries.push(TrashEntry {
                trash_path: rel,
                original_path: original,
                deleted_at_ms: deleted,
            });
        }
        entries.sort_by(|a, b| a.trash_path.cmp(&b.trash_path));
        Ok(entries)
    }
    fn add_attachment(
        &self,
        name: &str,
        bytes: &[u8],
        directory: &str,
        stamp: &str,
    ) -> Result<String> {
        // A bounded IPC upload; the vault-wide data budget follows CON-06.
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(AppError::new("E_FILE_TOO_LARGE", "单次附件上传上限32 MiB"));
        }
        if directory == "trash"
            || directory.starts_with("trash/")
            || directory == ".git"
            || directory.starts_with(".git/")
        {
            return Err(AppError::new("E_PATH_INVALID", "不能写入系统目录"));
        }
        crate::domain::validate_rel(directory, true)?;
        let safe = crate::domain::sanitize_title(name)?;
        let attachments = self.resolve(&RelPath::new("attachments")?)?;
        let mut total = 0u64;
        if attachments.exists() {
            for e in WalkDir::new(&attachments).follow_links(false) {
                let e = e.map_err(|e| AppError::new("E_IO_PERMISSION", e.to_string()))?;
                let m = fs::symlink_metadata(e.path()).map_err(AppError::io)?;
                if linked(&m) {
                    return Err(AppError::new("E_PATH_INVALID", "附件目录含不安全链接"));
                }
                if m.is_file() {
                    total = total.saturating_add(m.len());
                }
            }
        }
        if total.saturating_add(bytes.len() as u64) > 20 * 1024 * 1024 * 1024 {
            return Err(AppError::new("E_IO_DISK_FULL", "附件总量超过20 GiB约束"));
        }
        for n in 0..10000 {
            let suffix = if n == 0 {
                String::new()
            } else {
                format!(" ({})", n + 1)
            };
            let filename = match safe.rsplit_once('.') {
                Some((base, ext)) => format!("{stamp}-{base}{suffix}.{ext}"),
                None => format!("{stamp}-{safe}{suffix}"),
            };
            let rel = if directory.is_empty() {
                filename
            } else {
                format!("{directory}/{filename}")
            };
            let rp = RelPath::new(&rel)?;
            self.prepare_parent(&rp)?;
            let target = self.resolve(&rp)?;
            let mut temp = NamedTempFile::new_in(target.parent().unwrap()).map_err(AppError::io)?;
            temp.write_all(bytes).map_err(AppError::io)?;
            temp.as_file().sync_all().map_err(AppError::io)?;
            // https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html#method.persist_noclobber
            match temp.persist_noclobber(&target) {
                Ok(_) => {
                    sync_parent(&target)?;
                    return Ok(rel);
                }
                Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(AppError::io(e.error)),
            }
        }
        Err(AppError::new("E_PATH_INVALID", "附件重名过多"))
    }
}
/// Keep recovery state outside the authoritative vault, including non-existing roots.
pub fn private_state_root(base: &Path, vault: &str, id: &str) -> Result<PathBuf> {
    if !base.is_absolute() {
        return Err(AppError::new(
            "E_PATH_INVALID",
            "应用数据目录必须是绝对路径",
        ));
    }
    let vault = fs::canonicalize(vault).map_err(AppError::io)?;
    let mut ancestor = base.to_path_buf();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        let name = ancestor
            .file_name()
            .ok_or_else(|| AppError::new("E_PATH_INVALID", "无法定位数据目录父级"))?
            .to_os_string();
        suffix.push(name);
        ancestor.pop();
    }
    let mut resolved = fs::canonicalize(&ancestor).map_err(AppError::io)?;
    for part in suffix.into_iter().rev() {
        resolved.push(part);
    }
    if resolved.starts_with(&vault) {
        return Err(AppError::new(
            "E_PATH_INVALID",
            "草稿与恢复日志必须存储在库外",
        ));
    }
    Ok(resolved.join("vaults").join(id))
}
pub fn original_from_trash(path: &str) -> Option<String> {
    let tail = path.strip_prefix("trash/")?;
    let (original, suffix) = tail.rsplit_once(" (deleted ")?;
    let (stamp, ending) = suffix.split_once(')')?;
    if stamp.len() != 15 || !ending.ends_with(".md") || deletion_timestamp(path).is_none() {
        return None;
    }
    Some(format!("{original}.md"))
}
fn deletion_timestamp(path: &str) -> Option<u64> {
    let (_, suffix) = path.rsplit_once(" (deleted ")?;
    let stamp = suffix.split(')').next()?;
    let date = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d-%H%M%S").ok()?;
    u64::try_from(date.and_utc().timestamp_millis()).ok()
}
fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(path.parent().unwrap())
            .map_err(AppError::io)?
            .sync_all()
            .map_err(AppError::io)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
