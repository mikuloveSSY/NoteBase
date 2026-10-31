//! 库目录迁移：先复制到同级临时目录，逐文件校验后发布，永不删除原库。
use crate::domain::{AppError, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub trait VaultLocation: Send + Sync {
    fn configured_path(&self) -> Result<Option<String>>;
    fn copy_verified(&self, source: &str, target: &str) -> Result<String>;
    fn save_path(&self, path: &str) -> Result<()>;
}
pub struct LocalVaultLocation {
    pub settings_path: PathBuf,
}
#[derive(Debug, PartialEq, Eq)]
struct Manifest {
    files: BTreeMap<PathBuf, (u64, String)>,
    dirs: BTreeSet<PathBuf>,
}
fn path_error(message: &str) -> AppError {
    AppError::new("E_PATH_INVALID", message)
}
fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
fn manifest(root: &Path, cache: bool) -> Result<Manifest> {
    let mut result = Manifest {
        files: BTreeMap::new(),
        dirs: BTreeSet::new(),
    };
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            if !cache || entry.depth() != 1 {
                return true;
            }
            let name = entry.file_name().to_string_lossy();
            !matches!(
                name.as_ref(),
                "index.db" | "index.db-wal" | "index.db-shm" | "index.writer.lock"
            ) && !name.starts_with("rebuild-")
                && !name.starts_with("scan-")
        })
    {
        let entry = entry.map_err(|e| AppError::new("E_IO_PERMISSION", e.to_string()))?;
        let meta = fs::symlink_metadata(entry.path()).map_err(AppError::io)?;
        if is_link(&meta) {
            return Err(path_error("迁移不支持符号链接或目录联接；原库保持不变"));
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| path_error("文件超出源库"))?
            .to_owned();
        if meta.is_dir() {
            if !relative.as_os_str().is_empty() {
                result.dirs.insert(relative);
            }
            continue;
        }
        if !meta.is_file() {
            return Err(path_error("迁移只支持普通文件与目录"));
        }
        let mut input = File::open(entry.path()).map_err(AppError::io)?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let n = input.read(&mut buffer).map_err(AppError::io)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
            size += n as u64;
        }
        result
            .files
            .insert(relative, (size, format!("{:x}", hash.finalize())));
    }
    Ok(result)
}
pub fn copy_verified(source: &Path, target: &Path) -> Result<PathBuf> {
    copy_checked(source, target, || {})
}
fn copy_checked(source: &Path, target: &Path, after_copy: impl FnOnce()) -> Result<PathBuf> {
    copy_filtered(source, target, after_copy, false)
}
fn copy_filtered(
    source: &Path,
    target: &Path,
    after_copy: impl FnOnce(),
    cache: bool,
) -> Result<PathBuf> {
    if !source.is_absolute() || !target.is_absolute() {
        return Err(path_error("迁移源与目标必须是绝对路径"));
    }
    if is_link(&fs::symlink_metadata(source).map_err(AppError::io)?) {
        return Err(path_error("迁移源不能是符号链接或目录联接"));
    }
    let source = fs::canonicalize(source).map_err(AppError::io)?;
    if !source.is_dir() {
        return Err(path_error("源库必须是目录"));
    }
    let parent = fs::canonicalize(
        target
            .parent()
            .ok_or_else(|| path_error("目标缺少父目录"))?,
    )
    .map_err(AppError::io)?;
    let target = parent.join(
        target
            .file_name()
            .ok_or_else(|| path_error("目标目录名不合法"))?,
    );
    if target.exists() || fs::symlink_metadata(&target).is_ok() {
        return Err(path_error("迁移目标必须不存在，避免覆盖已有文件"));
    }
    if target.starts_with(&source) || source.starts_with(&target) {
        return Err(path_error("源库和目标不能相同或相互嵌套"));
    }
    let before = manifest(&source, cache)?;
    let bytes = before
        .files
        .values()
        .try_fold(0u64, |sum, (size, _)| sum.checked_add(*size))
        .ok_or_else(|| path_error("文件总大小溢出"))?;
    if fs2::available_space(&parent).map_err(AppError::io)? < bytes.saturating_add(1024 * 1024) {
        return Err(AppError::new("E_IO_DISK_FULL", "目标磁盘可用空间不足"));
    }
    let staging = tempfile::Builder::new()
        .prefix(".notebase-migrate-")
        .tempdir_in(&parent)
        .map_err(AppError::io)?;
    for dir in &before.dirs {
        fs::create_dir_all(staging.path().join(dir)).map_err(AppError::io)?;
    }
    for relative in before.files.keys() {
        let from = source.join(relative);
        let to = staging.path().join(relative);
        if is_link(&fs::symlink_metadata(&from).map_err(AppError::io)?) {
            return Err(path_error("复制期间源路径变成了链接，迁移已中止"));
        }
        let mut input = File::open(&from).map_err(AppError::io)?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&to)
            .map_err(AppError::io)?;
        std::io::copy(&mut input, &mut output).map_err(AppError::io)?;
        let meta = input.metadata().map_err(AppError::io)?;
        if let Ok(time) = meta.modified() {
            output
                .set_times(fs::FileTimes::new().set_modified(time))
                .map_err(AppError::io)?;
        }
        output
            .set_permissions(meta.permissions())
            .map_err(AppError::io)?;
        output.sync_all().map_err(AppError::io)?;
    }
    after_copy();
    if manifest(staging.path(), cache)? != before || manifest(&source, cache)? != before {
        return Err(AppError::new(
            "E_EXTERNAL_MODIFIED",
            "复制或校验期间源库发生变化；未切换，原库完整保留",
        ));
    }
    publish(staging.path(), &target)?;
    // 已发布目录不在 TempDir 原路径；销毁空的清理句柄即可。
    Ok(target)
}

#[cfg(windows)]
pub(crate) fn publish(from: &Path, to: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};
    let source: Vec<_> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let target: Vec<_> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: 两缓冲区均以 NUL 结束；未指定 REPLACE_EXISTING，不覆盖竞态中新建的目标。
    if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        return Err(AppError::io(std::io::Error::last_os_error()));
    }
    Ok(())
}
#[cfg(not(windows))]
pub(crate) fn publish(_from: &Path, _to: &Path) -> Result<()> {
    Err(AppError::new(
        "E_FORMAT_UNSUPPORTED",
        "目录迁移的无覆盖发布当前仅支持 Windows",
    ))
}

impl LocalVaultLocation {
    fn settings(&self) -> Result<serde_json::Value> {
        match fs::read(&self.settings_path) {
            Ok(bytes) => {
                if bytes.len() > 256 * 1024 {
                    return Err(path_error("设置文件超限"));
                }
                let value: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|_| path_error("设置文件不是合法 JSON，拒绝覆盖"))?;
                if !value.is_object() {
                    return Err(path_error("设置文件必须是对象"));
                }
                Ok(value)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::json!({})),
            Err(error) => Err(AppError::io(error)),
        }
    }
}
impl VaultLocation for LocalVaultLocation {
    fn configured_path(&self) -> Result<Option<String>> {
        Ok(self
            .settings()?
            .get("vaultPath")
            .and_then(|v| v.as_str())
            .map(str::to_owned))
    }
    fn copy_verified(&self, source: &str, target: &str) -> Result<String> {
        let copied = copy_verified(Path::new(source), Path::new(target))?;
        let id = |path: &Path| -> Result<String> {
            let canonical = fs::canonicalize(path).map_err(AppError::io)?;
            Ok(format!(
                "{:x}",
                Sha256::digest(canonical.to_string_lossy().as_bytes())
            )[..16]
                .to_owned())
        };
        let root = self
            .settings_path
            .parent()
            .ok_or_else(|| path_error("没有应用数据目录"))?
            .join("vaults");
        let from = root.join(id(Path::new(source))?);
        let new_id = id(&copied)?;
        let to = root.join(&new_id);
        if from.exists() {
            let read_business = || -> Result<Option<crate::index::recovery::Business>> {
                let db = from.join("index.db");
                if !db.exists() {
                    return Ok(None);
                }
                let conn = rusqlite::Connection::open_with_flags(
                    db,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                        | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
                )
                .map_err(|e| path_error(&e.to_string()))?;
                Ok(Some(crate::index::recovery::extract_business(&conn)?))
            };
            let business = read_business()?;
            copy_filtered(&from, &to, || {}, true)?;
            if read_business()? != business {
                return Err(AppError::new(
                    "E_EXTERNAL_MODIFIED",
                    "复制期间历史或同步状态变化；未切换库，原数据已保留",
                ));
            }
            let database = to.join("index.db");
            let index = crate::index::sqlite::SqliteIndexStore::open(&database, &new_id)?;
            if let Some(business) = business {
                crate::index::recovery::restore_business(&database, business)?;
            }
            drop(index);
        }
        Ok(copied.to_string_lossy().into_owned())
    }
    fn save_path(&self, path: &str) -> Result<()> {
        let mut settings = self.settings()?;
        settings["vaultPath"] = serde_json::json!(path);
        let parent = self
            .settings_path
            .parent()
            .ok_or_else(|| path_error("设置目录不合法"))?;
        fs::create_dir_all(parent).map_err(AppError::io)?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(AppError::io)?;
        tmp.write_all(
            &serde_json::to_vec_pretty(&settings).map_err(|e| path_error(&e.to_string()))?,
        )
        .map_err(AppError::io)?;
        tmp.as_file().sync_all().map_err(AppError::io)?;
        tmp.persist(&self.settings_path)
            .map_err(|e| AppError::io(e.error))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn equal_file_count_and_size_do_not_hide_changed_content() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("A.md"), "old").unwrap();
        let target = dir.path().join("target");
        let error = copy_checked(&source, &target, || {
            fs::write(source.join("A.md"), "new").unwrap()
        })
        .unwrap_err();
        assert_eq!(error.code, "E_EXTERNAL_MODIFIED");
        assert!(!target.exists());
        assert_eq!(fs::read_to_string(source.join("A.md")).unwrap(), "new");
    }
}
