use crate::domain::{validate_rel, AppError, Result, NOTE_SIZE_LIMIT};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;
pub const ASSET_LIMIT: u64 = 100 * 1024 * 1024;
pub const TOTAL_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn linked(m: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        m.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        m.file_type().is_symlink()
    }
}
pub fn no_links(path: &Path) -> Result<()> {
    let mut cur = PathBuf::new();
    for part in path.components() {
        cur.push(part);
        // Windows verbatim prefixes (e.g. \\?\C:) are not standalone filesystem
        // objects. Check entries only after the volume/root has been assembled.
        if matches!(
            part,
            std::path::Component::Prefix(_) | std::path::Component::RootDir
        ) {
            continue;
        }
        match fs::symlink_metadata(&cur) {
            Ok(m) if linked(&m) => {
                return Err(AppError::new("E_PATH_INVALID", "不允许符号链接或目录联接"))
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(AppError::io(e)),
        }
    }
    Ok(())
}
pub fn directory(path: &Path) -> Result<()> {
    no_links(path)?;
    fs::create_dir_all(path).map_err(AppError::io)?;
    no_links(path)
}
pub fn read_limit(path: &Path, max: u64) -> Result<Vec<u8>> {
    no_links(path)?;
    let f = fs::File::open(path).map_err(AppError::io)?;
    if !f.metadata().map_err(AppError::io)?.is_file() {
        return Err(AppError::new("E_PATH_INVALID", "目标不是普通文件"));
    }
    let mut out = Vec::new();
    f.take(max + 1)
        .read_to_end(&mut out)
        .map_err(AppError::io)?;
    if out.len() as u64 > max {
        return Err(AppError::new("E_FILE_TOO_LARGE", "文件超过本功能大小上限"));
    }
    Ok(out)
}
pub fn atomic(path: &Path, bytes: &[u8], overwrite: bool) -> Result<()> {
    no_links(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| AppError::new("E_PATH_INVALID", "目标没有父目录"))?;
    directory(parent)?;
    let mut temp = NamedTempFile::new_in(parent).map_err(AppError::io)?;
    temp.write_all(bytes).map_err(AppError::io)?;
    temp.as_file().sync_all().map_err(AppError::io)?;
    if overwrite {
        temp.persist(path).map_err(|e| AppError::io(e.error))?;
    } else {
        temp.persist_noclobber(path).map_err(|e| {
            if e.error.kind() == std::io::ErrorKind::AlreadyExists {
                AppError::new("E_PATH_INVALID", "目标已存在，请选择新路径")
            } else {
                AppError::io(e.error)
            }
        })?;
    }
    #[cfg(unix)]
    fs::File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(AppError::io)?;
    Ok(())
}
pub struct Files {
    pub root: PathBuf,
}
impl Files {
    pub fn open(vault: &str) -> Result<Self> {
        let root = fs::canonicalize(vault).map_err(AppError::io)?;
        no_links(&root)?;
        Ok(Self { root })
    }
    pub fn path(&self, relative: &str) -> Result<PathBuf> {
        validate_rel(relative, false)?;
        let p = self.root.join(relative);
        no_links(&p)?;
        Ok(p)
    }
    pub fn read_note(&self, path: &str) -> Result<Vec<u8>> {
        if !path.ends_with(".md") || path.starts_with("trash/") || path.starts_with("attachments/")
        {
            return Err(AppError::new("E_PATH_INVALID", "需要普通笔记路径"));
        }
        let b = read_limit(&self.path(path)?, NOTE_SIZE_LIMIT as u64)?;
        std::str::from_utf8(&b)
            .map_err(|_| AppError::new("E_ENCODING_INVALID", "笔记不是合法 UTF-8"))?;
        Ok(b)
    }
    pub fn entries(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| e.depth() == 0 || e.file_name() != ".git")
        {
            let e = entry.map_err(|e| AppError::new("E_IO_PERMISSION", e.to_string()))?;
            if linked(&fs::symlink_metadata(e.path()).map_err(AppError::io)?) {
                return Err(AppError::new(
                    "E_PATH_INVALID",
                    "库中存在链接，无法生成完整备份",
                ));
            }
            if !e.file_type().is_file() {
                continue;
            }
            let rel = e
                .path()
                .strip_prefix(&self.root)
                .map_err(|_| AppError::new("E_PATH_INVALID", "越界路径"))?
                .to_string_lossy()
                .replace('\\', "/");
            if rel.ends_with(".md") || rel.starts_with("attachments/") {
                self.path(&rel)?;
                out.push(rel);
            }
        }
        out.sort();
        if out.len() > 200000 {
            return Err(AppError::new("E_FILE_TOO_LARGE", "备份文件数量超过 200000"));
        }
        Ok(out)
    }
    pub fn external(&self, path: &str) -> Result<PathBuf> {
        let p = PathBuf::from(path);
        if !p.is_absolute() {
            return Err(AppError::new("E_PATH_INVALID", "输出必须使用绝对路径"));
        }
        no_links(&p)?;
        // Resolve non-existing leaf paths against their nearest existing ancestor.
        let mut parent = p.as_path();
        let mut tail = Vec::new();
        while !parent.exists() {
            tail.push(
                parent
                    .file_name()
                    .ok_or_else(|| AppError::new("E_PATH_INVALID", "无效输出路径"))?
                    .to_os_string(),
            );
            parent = parent
                .parent()
                .ok_or_else(|| AppError::new("E_PATH_INVALID", "无效输出路径"))?;
        }
        let mut canonical = fs::canonicalize(parent).map_err(AppError::io)?;
        for s in tail.into_iter().rev() {
            if s == ".." {
                canonical.pop();
            } else if s != "." {
                canonical.push(s);
            }
        }
        if canonical.starts_with(&self.root) || self.root.starts_with(&canonical) {
            return Err(AppError::new(
                "E_PATH_INVALID",
                "输出位置必须在笔记库及其父级之外",
            ));
        }
        Ok(canonical)
    }
}
