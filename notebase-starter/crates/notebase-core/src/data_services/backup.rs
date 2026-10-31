use super::{
    files::{self, ASSET_LIMIT, TOTAL_LIMIT},
    LocalDataService,
};
use crate::domain::{field, AppError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Manifest {
    format_version: u32,
    created_at: String,
    note_count: usize,
    attachment_count: usize,
    checksum: String,
    #[serde(default)]
    description: String,
    files: Vec<Item>,
    vault_id: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Item {
    path: String,
    size: u64,
    sha256: String,
}
fn zip_error(e: impl std::fmt::Display) -> AppError {
    AppError::new("E_BACKUP_CHECKSUM", format!("备份包无效：{e}"))
}
fn stamp() -> String {
    static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{}-{}-{:06}",
        crate::index::now_ms(),
        std::process::id(),
        SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}
fn archive_path(path: &str) -> String {
    if path.starts_with("attachments/") {
        path.to_owned()
    } else {
        format!("notes/{path}")
    }
}
fn restore_path(path: &str) -> Result<String> {
    let rel = if let Some(p) = path.strip_prefix("notes/") {
        p
    } else if path.starts_with("attachments/") {
        path
    } else {
        return Err(zip_error("未识别的备份路径"));
    };
    crate::domain::validate_rel(rel, false)?;
    if !rel.starts_with("attachments/") && !rel.ends_with(".md") {
        return Err(zip_error("notes 内存在非 Markdown 文件"));
    }
    if rel.starts_with(".git/") {
        return Err(zip_error("不允许恢复 Git 元数据"));
    }
    Ok(rel.into())
}
pub fn create(s: &LocalDataService, p: &Value) -> Result<Value> {
    s.check_cancelled()?;
    let target = if let Some(dir) = p.get("targetDir").and_then(Value::as_str) {
        s.files.external(dir)?
    } else {
        let config = settings(s)?;
        s.files.external(
            config["dir"]
                .as_str()
                .ok_or_else(|| AppError::new("E_PATH_INVALID", "无效备份目录"))?,
        )?
    };
    files::directory(&target)?;
    let entries = s.files.entries()?;
    let mut writer =
        ZipWriter::new(tempfile::NamedTempFile::new_in(&target).map_err(AppError::io)?);
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o600);
    let mut manifest = Manifest {
        format_version: 1,
        created_at: chrono::DateTime::from_timestamp_millis(crate::index::now_ms() as i64)
            .unwrap()
            .to_rfc3339(),
        note_count: 0,
        attachment_count: 0,
        checksum: String::new(),
        description: p
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(4096)
            .collect(),
        files: vec![],
        vault_id: s
            .private
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into(),
    };
    let mut combined = Sha256::new();
    let mut total = 0u64;
    // Keep bytes out of memory while compressing, while calculating both content and path+content hashes.
    for rel in &entries {
        s.check_cancelled()?;
        let source = s.files.path(rel)?;
        let mut file = fs::File::open(&source).map_err(AppError::io)?;
        let before = file.metadata().map_err(AppError::io)?;
        if before.len() > ASSET_LIMIT {
            return Err(AppError::new(
                "E_FILE_TOO_LARGE",
                "单个备份资产超过 100 MiB",
            ));
        }
        writer
            .start_file(archive_path(rel), opts)
            .map_err(zip_error)?;
        let mut content = Sha256::new();
        let mut with_path = Sha256::new();
        with_path.update(rel.as_bytes());
        with_path.update([0]);
        let mut size = 0u64;
        let mut buf = [0; 65536];
        loop {
            let n = file.read(&mut buf).map_err(AppError::io)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            total += n as u64;
            if size > ASSET_LIMIT || total > TOTAL_LIMIT {
                return Err(AppError::new(
                    "E_FILE_TOO_LARGE",
                    "备份总量超过 2 GiB 基础上限",
                ));
            }
            content.update(&buf[..n]);
            with_path.update(&buf[..n]);
            writer.write_all(&buf[..n]).map_err(AppError::io)?;
        }
        if before.len() != size
            || before.modified().ok()
                != fs::metadata(&source).map_err(AppError::io)?.modified().ok()
        {
            return Err(AppError::new(
                "E_EXTERNAL_MODIFIED",
                "备份期间文件变化，请重试",
            ));
        }
        let path_hash = format!("{:x}", with_path.finalize());
        combined.update(path_hash.as_bytes());
        manifest.files.push(Item {
            path: rel.clone(),
            size,
            sha256: format!("{:x}", content.finalize()),
        });
        if rel.starts_with("attachments/") {
            manifest.attachment_count += 1;
        } else {
            manifest.note_count += 1;
        }
    }
    if entries != s.files.entries()? {
        return Err(AppError::new(
            "E_EXTERNAL_MODIFIED",
            "备份期间库文件清单变化，请重试",
        ));
    }
    // Per-file stat checks alone cannot detect a file changed after its turn.
    // Recheck the source content before publishing the completed archive.
    for (rel, item) in entries.iter().zip(&manifest.files) {
        s.check_cancelled()?;
        let mut input = fs::File::open(s.files.path(rel)?).map_err(AppError::io)?;
        let mut digest = Sha256::new();
        let mut size = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            let count = input.read(&mut buffer).map_err(AppError::io)?;
            if count == 0 {
                break;
            }
            size += count as u64;
            if size > item.size {
                break;
            }
            digest.update(&buffer[..count]);
        }
        if size != item.size || format!("{:x}", digest.finalize()) != item.sha256 {
            return Err(AppError::new(
                "E_EXTERNAL_MODIFIED",
                "备份期间源文件内容变化，未发布备份",
            ));
        }
    }
    manifest.checksum = format!("{:x}", combined.finalize());
    let encoded = serde_json::to_vec_pretty(&manifest).unwrap();
    if encoded.len() > 32 * 1024 * 1024 {
        return Err(AppError::new(
            "E_FILE_TOO_LARGE",
            "备份清单超过 32 MiB，未发布备份",
        ));
    }
    writer
        .start_file("manifest.json", opts)
        .map_err(zip_error)?;
    writer.write_all(&encoded).map_err(AppError::io)?;
    let temp = writer.finish().map_err(zip_error)?;
    temp.as_file().sync_all().map_err(AppError::io)?;
    let path = target.join(format!("backup-{}.zip", stamp()));
    s.check_cancelled()?;
    temp.persist_noclobber(&path)
        .map_err(|e| AppError::io(e.error))?;
    Ok(
        json!({"filePath":path,"bytes":fs::metadata(&path).map_err(AppError::io)?.len(),"checksum":manifest.checksum,"noteCount":manifest.note_count,"attachmentCount":manifest.attachment_count}),
    )
}
fn verified(file_path: &Path, stage: &Path) -> Result<Manifest> {
    files::no_links(file_path)?;
    let file = fs::File::open(file_path).map_err(AppError::io)?;
    let mut zip = ZipArchive::new(file).map_err(zip_error)?;
    if zip.len() > 200001 {
        return Err(zip_error("文件数量过多"));
    }
    let manifest: Manifest = {
        let f = zip.by_name("manifest.json").map_err(zip_error)?;
        if f.size() > 32 * 1024 * 1024 {
            return Err(zip_error("manifest 过大"));
        }
        serde_json::from_reader(f.take(32 * 1024 * 1024)).map_err(zip_error)?
    };
    if manifest.format_version != 1 {
        return Err(AppError::new("E_FORMAT_UNSUPPORTED", "未知备份格式版本"));
    }
    let mut expected = BTreeMap::new();
    for item in &manifest.files {
        crate::domain::validate_rel(&item.path, false)?;
        restore_path(&archive_path(&item.path))?;
        if item.size > ASSET_LIMIT
            || item.sha256.len() != 64
            || expected.insert(item.path.clone(), item).is_some()
        {
            return Err(zip_error("manifest 文件项无效或重复"));
        }
    }
    if zip.len() != manifest.files.len() + 1 {
        return Err(zip_error("清单数量与 ZIP 不符"));
    }
    let mut seen = BTreeSet::new();
    let mut path_hashes = BTreeMap::<String, String>::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(zip_error)?;
        let name = f.name().to_owned();
        if name == "manifest.json" {
            continue;
        }
        if f.is_dir()
            || f.enclosed_name().is_none()
            || f.unix_mode()
                .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(zip_error("拒绝目录、链接或越界 ZIP 条目"));
        }
        let rel = restore_path(&name)?;
        if archive_path(&rel) != name {
            return Err(zip_error("不规范的备份条目路径"));
        }
        let item = expected
            .get(&rel)
            .ok_or_else(|| zip_error("ZIP 存在清单外文件"))?;
        if !seen.insert(rel.clone()) || f.size() != item.size {
            return Err(zip_error("重复条目或大小不符"));
        }
        let target = stage.join(&rel);
        files::directory(target.parent().unwrap())?;
        let mut out = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&target)
            .map_err(AppError::io)?;
        let mut content = Sha256::new();
        let mut with_path = Sha256::new();
        with_path.update(rel.as_bytes());
        with_path.update([0]);
        let mut size = 0u64;
        let mut buf = [0; 65536];
        loop {
            let n = f.read(&mut buf).map_err(zip_error)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            total += n as u64;
            if size > item.size || size > ASSET_LIMIT || total > TOTAL_LIMIT {
                return Err(zip_error("解压大小超限"));
            }
            content.update(&buf[..n]);
            with_path.update(&buf[..n]);
            out.write_all(&buf[..n]).map_err(AppError::io)?;
        }
        if size != item.size || format!("{:x}", content.finalize()) != item.sha256 {
            return Err(zip_error("文件 SHA-256 不匹配"));
        }
        out.sync_all().map_err(AppError::io)?;
        path_hashes.insert(rel, format!("{:x}", with_path.finalize()));
    }
    let mut hash = Sha256::new();
    for v in path_hashes.values() {
        hash.update(v.as_bytes());
    }
    if seen.len() != expected.len()
        || format!("{:x}", hash.finalize()) != manifest.checksum
        || manifest.note_count
            != seen
                .iter()
                .filter(|p| !p.starts_with("attachments/"))
                .count()
        || manifest.attachment_count
            != seen
                .iter()
                .filter(|p| p.starts_with("attachments/"))
                .count()
    {
        return Err(zip_error("整库 checksum 或文件计数不符"));
    }
    Ok(manifest)
}
pub fn restore(s: &LocalDataService, p: &Value) -> Result<Value> {
    restore_with_move(s, p, &mut |from, to| {
        #[cfg(windows)]
        {
            crate::migration::publish(from, to)
        }
        #[cfg(not(windows))]
        {
            fs::rename(from, to).map_err(AppError::io)
        }
    })
}
fn restore_with_move(
    s: &LocalDataService,
    p: &Value,
    move_dir: &mut dyn FnMut(&Path, &Path) -> Result<()>,
) -> Result<Value> {
    s.check_cancelled()?;
    let source = PathBuf::from(field(p, "filePath")?);
    if !source.is_absolute() {
        return Err(AppError::new("E_PATH_INVALID", "备份包必须使用绝对路径"));
    }
    if p["verifyOnly"] == true {
        let stage = tempfile::tempdir().map_err(AppError::io)?;
        let manifest = verified(&source, stage.path())?;
        return Ok(
            json!({"verified":true,"noteCount":manifest.note_count,"attachmentCount":manifest.attachment_count,"checksum":manifest.checksum}),
        );
    }
    let mode = p.get("mode").and_then(Value::as_str).unwrap_or("new");
    if mode != "original" && mode != "new" {
        return Err(AppError::new(
            "E_PATH_INVALID",
            "恢复模式需要 original 或 new",
        ));
    }
    let target = if mode == "original" {
        s.files.root.clone()
    } else {
        s.files.external(field(p, "targetPath")?)?
    };
    if mode == "new" && target.exists() {
        return Err(AppError::new(
            "E_PATH_INVALID",
            "新位置必须不存在，避免覆盖",
        ));
    }
    let parent = target
        .parent()
        .ok_or_else(|| AppError::new("E_PATH_INVALID", "目标无父目录"))?;
    files::directory(parent)?;
    let stage = tempfile::Builder::new()
        .prefix(".notebase-restore-")
        .tempdir_in(parent)
        .map_err(AppError::io)?;
    let manifest = verified(&source, stage.path())?;
    if p.get("verifyOnly") == Some(&Value::Bool(true)) {
        return Ok(
            json!({"verified":true,"noteCount":manifest.note_count,"attachmentCount":manifest.attachment_count,"checksum":manifest.checksum}),
        );
    }
    // Always protect the current vault, including when restoring to a new location.
    let snapshot = create(s, &json!({"description":"恢复前强制整库快照"}))?;
    let mut retained = Value::Null;
    // Do not observe cancellation between the directory swap and rollback.
    s.check_cancelled()?;
    if mode == "original" {
        let old = parent.join(format!("notebase-before-restore-{}", stamp()));
        files::no_links(&old)?;
        if old.exists() {
            return Err(AppError::new("E_PATH_INVALID", "恢复保留目录已存在"));
        }
        move_dir(&s.files.root, &old)?;
        if let Err(mut e) = move_dir(stage.path(), &target) {
            if let Err(rollback) = move_dir(&old, &s.files.root) {
                let mut error = AppError::new(
                    "E_IO_PERMISSION",
                    format!(
                        "恢复发布与自动回退失败；原库保存在 {}。请保留原件并重新打开该目录。{}; {}",
                        old.display(),
                        e.message,
                        rollback.message
                    ),
                );
                error.detail = json!({"retainedOriginalDir":old,"snapshotPath":snapshot["filePath"],"restoredTo":target});
                return Err(error);
            }
            e.detail =
                json!({"rolledBack":true,"snapshotPath":snapshot["filePath"],"restoredTo":target});
            return Err(e);
        }
        // Retain the entire former directory: even files outside the backup asset scope survive.
        retained = json!(old);
    } else {
        if target.exists() {
            return Err(AppError::new("E_PATH_INVALID", "恢复目标被其他程序创建"));
        }
        move_dir(stage.path(), &target)?;
    }
    Ok(
        json!({"restoredTo":target,"snapshotPath":snapshot["filePath"],"retainedOriginalDir":retained,"noteCount":manifest.note_count,"attachmentCount":manifest.attachment_count}),
    )
}
pub fn settings(s: &LocalDataService) -> Result<Value> {
    let p = s.private.join("backup-settings.json");
    let defaults = json!({"enabled":true,"intervalHours":24,"keepCount":30,"dir":s.backups,"versionRetentionDays":30});
    if !p.exists() {
        return Ok(defaults);
    }
    let mut stored: Value = serde_json::from_slice(&files::read_limit(&p, 65536)?)
        .map_err(|_| AppError::new("E_FORMAT_UNSUPPORTED", "备份配置损坏，原件已保留"))?;
    if !stored.is_object() {
        return Err(AppError::new(
            "E_FORMAT_UNSUPPORTED",
            "备份配置必须为对象，原件已保留",
        ));
    }
    for (key, default) in defaults.as_object().unwrap() {
        if let Some(value) = stored.get(key) {
            validate_setting(s, key, value).map_err(|_| {
                AppError::new(
                    "E_FORMAT_UNSUPPORTED",
                    format!("备份配置字段{key}无效，原件已保留"),
                )
            })?;
        } else {
            stored[key] = default.clone();
        }
    }
    Ok(stored)
}
fn validate_setting(s: &LocalDataService, key: &str, v: &Value) -> Result<()> {
    match key {
        "enabled" if v.is_boolean() => {}
        "intervalHours" if v.as_u64().is_some_and(|n| (1..=8760).contains(&n)) => {}
        "keepCount" if v.as_u64().is_some_and(|n| (1..=365).contains(&n)) => {}
        "versionRetentionDays" if v.as_u64().is_some_and(|n| n <= 36500) => {}
        "dir" => {
            s.files.external(
                v.as_str()
                    .ok_or_else(|| AppError::new("E_PATH_INVALID", "备份目录必须为字符串"))?,
            )?;
        }
        _ => return Err(AppError::new("E_PATH_INVALID", "无效备份或版本保留设置")),
    }
    Ok(())
}
pub fn configure(s: &LocalDataService, p: &Value) -> Result<Value> {
    let mut config = settings(s)?;
    let o = p
        .as_object()
        .ok_or_else(|| AppError::new("E_PATH_INVALID", "backup 设置必须为对象"))?;
    for (key, v) in o {
        validate_setting(s, key, v)?;
        config[key] = v.clone();
    }
    files::atomic(
        &s.private.join("backup-settings.json"),
        &serde_json::to_vec_pretty(&config).unwrap(),
        true,
    )?;
    Ok(json!({"ok":true}))
}
pub fn maintenance(s: &LocalDataService, now: u64) -> Result<Value> {
    let config = settings(s)?;
    let state = s.private.join("backup-schedule.json");
    let last = if state.exists() {
        let v: Value = serde_json::from_slice(&files::read_limit(&state, 65536)?)
            .map_err(|_| AppError::new("E_FORMAT_UNSUPPORTED", "备份调度状态损坏"))?;
        v["lastBackupMs"].as_u64().unwrap_or(0)
    } else {
        0
    };
    let pruned = super::versions::prune(s, now)?;
    if config["enabled"] != true
        || now.saturating_sub(last) < config["intervalHours"].as_u64().unwrap_or(24) * 3600000
    {
        return Ok(json!({"created":false,"prunedVersions":pruned}));
    }
    let backup = create(
        s,
        &json!({"targetDir":config["dir"],"description":"定时备份"}),
    )?;
    files::atomic(
        &state,
        &serde_json::to_vec(&json!({"lastBackupMs":now})).unwrap(),
        true,
    )?;
    // Never prune arbitrary ZIPs: verify each package's manifest before considering it owned.
    let dir = s.files.external(
        config["dir"]
            .as_str()
            .ok_or_else(|| AppError::new("E_PATH_INVALID", "无效备份目录"))?,
    )?;
    let mut owned = Vec::new();
    for e in fs::read_dir(&dir).map_err(AppError::io)? {
        let e = e.map_err(AppError::io)?;
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("backup-")
            && name.ends_with(".zip")
            && e.path() != Path::new(backup["filePath"].as_str().unwrap_or(""))
        {
            let scratch = tempfile::tempdir().map_err(AppError::io)?;
            if verified(&e.path(), scratch.path()).is_ok_and(|m| {
                m.vault_id == s.private.file_name().unwrap_or_default().to_string_lossy()
            }) {
                owned.push(e.path());
            }
        }
    }
    owned.sort();
    let remove = owned
        .len()
        .saturating_add(1)
        .saturating_sub(config["keepCount"].as_u64().unwrap_or(30) as usize);
    for path in owned.into_iter().take(remove) {
        files::no_links(&path)?;
        fs::remove_file(path).map_err(AppError::io)?;
    }
    Ok(json!({"created":true,"backup":backup,"prunedVersions":pruned}))
}
pub fn due(s: &LocalDataService, now: u64) -> Result<bool> {
    let c = settings(s)?;
    let state = s.private.join("backup-schedule.json");
    let last = if state.exists() {
        let v: Value = serde_json::from_slice(&files::read_limit(&state, 65536)?)
            .map_err(|_| AppError::new("E_FORMAT_UNSUPPORTED", "备份调度状态损坏"))?;
        v["lastBackupMs"].as_u64().unwrap_or(0)
    } else {
        0
    };
    Ok(c["enabled"] == true
        && now.saturating_sub(last) >= c["intervalHours"].as_u64().unwrap_or(24) * 3600000)
}

#[cfg(test)]
mod restore_faults {
    use super::*;
    fn exercise(rollback_fails: bool) {
        let root = tempfile::tempdir().unwrap();
        let vault = root.path().join("vault");
        fs::create_dir(&vault).unwrap();
        fs::write(vault.join("A.md"), b"archive").unwrap();
        let s = LocalDataService::open(vault.to_str().unwrap(), &root.path().join("data"), "fault")
            .unwrap();
        let archive = create(&s, &json!({})).unwrap();
        fs::write(vault.join("A.md"), b"current original").unwrap();
        let mut calls = 0;
        let mut retained = None;
        let error = restore_with_move(
            &s,
            &json!({"filePath":archive["filePath"],"mode":"original"}),
            &mut |from, to| {
                calls += 1;
                if calls == 2 {
                    return Err(AppError::new("E_IO_LOCKED", "injected publication failure"));
                }
                if calls == 3 && rollback_fails {
                    return Err(AppError::new("E_IO_LOCKED", "injected rollback failure"));
                }
                if calls == 1 {
                    retained = Some(to.to_path_buf());
                }
                fs::rename(from, to).map_err(AppError::io)
            },
        )
        .unwrap_err();
        assert_eq!(calls, 3);
        if rollback_fails {
            let original = retained.unwrap();
            assert_eq!(
                fs::read(original.join("A.md")).unwrap(),
                b"current original"
            );
            assert_eq!(error.detail["retainedOriginalDir"], json!(original));
            assert!(Path::new(error.detail["snapshotPath"].as_str().unwrap()).exists());
            assert!(!vault.exists());
        } else {
            assert_eq!(error.code, "E_IO_LOCKED");
            assert_eq!(error.detail["rolledBack"], true);
            assert_eq!(fs::read(vault.join("A.md")).unwrap(), b"current original");
        }
        let mut zip =
            ZipArchive::new(fs::File::open(archive["filePath"].as_str().unwrap()).unwrap())
                .unwrap();
        let mut body = String::new();
        zip.by_name("notes/A.md")
            .unwrap()
            .read_to_string(&mut body)
            .unwrap();
        assert_eq!(body, "archive");
    }
    #[test]
    fn publication_failure_restores_original_directory() {
        exercise(false);
    }
    #[test]
    fn rollback_failure_retains_original_and_reports_recovery_paths() {
        exercise(true);
    }
}
