use crate::domain::{self, AppError, Entry, NoteStore, Result, StoredNote};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use tempfile::NamedTempFile;
use walkdir::WalkDir;
pub const LIMIT: usize = 10 * 1024 * 1024;
pub struct FsStore {
    root: PathBuf,
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
                AppError::io(e)
            }
        })?;
        if !root.is_dir() {
            return Err(AppError::new("E_VAULT_NOT_FOUND", "笔记库必须是文件夹"));
        }
        Ok(Self { root })
    }
    fn resolve(&self, path: &str) -> Result<PathBuf> {
        domain::validate_rel(path, false)?;
        if !path.ends_with(".md") {
            return Err(AppError::new("E_PATH_INVALID", "基础版仅操作 .md 文件"));
        }
        let joined = self.root.join(path);
        let mut current = self.root.clone();
        for segment in path.split('/') {
            current.push(segment);
            match fs::symlink_metadata(&current) {
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(AppError::new("E_PATH_INVALID", "基础版不操作符号链接"))
                }
                Ok(_) => {
                    let canonical = fs::canonicalize(&current).map_err(AppError::io)?;
                    if !canonical.starts_with(&self.root) {
                        return Err(AppError::new("E_PATH_INVALID", "路径超出笔记库"));
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(AppError::io(e)),
            }
        }
        Ok(joined)
    }
}
fn mtime(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
impl NoteStore for FsStore {
    fn vault_path(&self) -> String {
        self.root.to_string_lossy().into()
    }
    fn read(&self, path: &str) -> Result<StoredNote> {
        let file = fs::File::open(self.resolve(path)?).map_err(AppError::io)?;
        let meta = file.metadata().map_err(AppError::io)?;
        // 仅支持最多 10 MB 的读取；超限只读内容由读取上限控制，避免一次载入任意大小文件。
        let mut raw = Vec::new();
        file.take((LIMIT + 1) as u64)
            .read_to_end(&mut raw)
            .map_err(AppError::io)?;
        Ok(StoredNote {
            raw,
            mtime_ms: mtime(&meta),
        })
    }
    fn create_new(&self, path: &str) -> std::io::Result<()> {
        let path = self
            .resolve(path)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e.message))?;
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        file.sync_all()?;
        Ok(())
    }
    fn write_atomic(&self, path: &str, body: &[u8]) -> Result<()> {
        let path = self.resolve(path)?;
        let parent = path
            .parent()
            .ok_or_else(|| AppError::new("E_PATH_INVALID", "无父目录"))?;
        let perms = fs::metadata(&path).map_err(AppError::io)?.permissions();
        let mut tmp = NamedTempFile::new_in(parent).map_err(AppError::io)?;
        tmp.write_all(body).map_err(AppError::io)?;
        tmp.as_file().set_permissions(perms).map_err(AppError::io)?;
        tmp.as_file().sync_all().map_err(AppError::io)?;
        // tempfile 跨平台 persist 使用同目录替换；失败时临时文件由 RAII 清理。
        tmp.persist(&path).map_err(|e| AppError::io(e.error))?;
        #[cfg(unix)]
        {
            fs::File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(AppError::io)?;
        }
        Ok(())
    }
    fn list(&self) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for e in WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                e.depth() == 0
                    || !matches!(
                        e.file_name().to_str(),
                        Some("trash" | "attachments" | ".git")
                    )
            })
        {
            let e = e.map_err(|e| AppError::new("E_IO_PERMISSION", e.to_string()))?;
            if e.file_type().is_file()
                && e.path().extension().and_then(|e| e.to_str()) == Some("md")
            {
                let relative = e
                    .path()
                    .strip_prefix(&self.root)
                    .map_err(|_| AppError::new("E_PATH_INVALID", "路径超出笔记库"))?
                    .to_string_lossy()
                    .replace('\\', "/");
                if self.resolve(&relative).is_ok() {
                    entries.push(Entry {
                        path: relative,
                        mtime_ms: mtime(&fs::metadata(e.path()).map_err(AppError::io)?),
                    });
                }
            }
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }
}
