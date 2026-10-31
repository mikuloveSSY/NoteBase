use super::{files, LocalDataService};
use crate::domain::{field, AppError, Result, NOTE_SIZE_LIMIT};
use serde_json::{json, Value};
use std::fs;
const RETENTION: u64 = 30 * 24 * 60 * 60 * 1000;
fn folder(s: &LocalDataService, path: &str) -> Result<std::path::PathBuf> {
    crate::domain::RelPath::new(path)?;
    Ok(s.private
        .join("snapshots")
        .join(files::digest(path.as_bytes())))
}
pub fn snapshot(
    s: &LocalDataService,
    path: &str,
    bytes: &[u8],
    description: &str,
) -> Result<Value> {
    if description.chars().count() > 4096 {
        return Err(AppError::new("E_PATH_INVALID", "快照说明不能超过 4096 字"));
    }
    if bytes.len() > NOTE_SIZE_LIMIT {
        return Err(AppError::new("E_FILE_TOO_LARGE", "历史版本超过大小上限"));
    }
    let hash = files::digest(bytes);
    let dir = folder(s, path)?;
    files::directory(&dir)?;
    let blob = dir.join(format!("{hash}.md"));
    let record = dir.join(format!("{hash}.json"));
    if !blob.exists() {
        files::atomic(&blob, bytes, false)?;
    }
    if !record.exists() {
        let entry = json!({"path":path,"versionId":hash,"contentHash":format!("sha256:{hash}"),"createdMs":crate::index::now_ms(),"sizeBytes":bytes.len(),"description":description});
        files::atomic(&record, &serde_json::to_vec_pretty(&entry).unwrap(), false)?;
    }
    // Keep the original creation time, but retain a deduplicated version for
    // thirty days after its most recent use. Old records need no migration.
    {
        let mut v: Value = serde_json::from_slice(&files::read_limit(&record, 65536)?)
            .map_err(|_| AppError::new("E_BACKUP_CHECKSUM", "版本元数据无效"))?;
        if v["path"] != path
            || v["versionId"] != hash
            || files::digest(&files::read_limit(&blob, NOTE_SIZE_LIMIT as u64)?) != hash
        {
            return Err(AppError::new(
                "E_BACKUP_CHECKSUM",
                "已有快照内容或元数据损坏，保留原件",
            ));
        }
        if !description.is_empty() {
            v["description"] = json!(description);
        }
        v["lastUsedMs"] = json!(crate::index::now_ms());
        files::atomic(&record, &serde_json::to_vec_pretty(&v).unwrap(), true)?;
    }
    Ok(json!({"path":path,"versionId":hash,"snapshotCreated":true}))
}
pub fn list(s: &LocalDataService, p: &Value) -> Result<Value> {
    let path = field(p, "path")?;
    let dir = folder(s, path)?;
    let mut items = Vec::<Value>::new();
    if dir.exists() {
        files::no_links(&dir)?;
        for f in fs::read_dir(&dir).map_err(AppError::io)? {
            let f = f.map_err(AppError::io)?;
            if f.path().extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let mut v: Value = serde_json::from_slice(&files::read_limit(&f.path(), 65536)?)
                .map_err(|_| AppError::new("E_BACKUP_CHECKSUM", "历史记录损坏，保留原件"))?;
            if v["path"] != path {
                return Err(AppError::new("E_BACKUP_CHECKSUM", "版本笔记路径不匹配"));
            }
            if crate::index::now_ms().saturating_sub(
                v["lastUsedMs"]
                    .as_u64()
                    .or_else(|| v["createdMs"].as_u64())
                    .unwrap_or(0),
            ) <= RETENTION
            {
                v.as_object_mut().unwrap().remove("lastUsedMs");
                items.push(v);
            }
        }
    }
    items.sort_by_key(|v| std::cmp::Reverse(v["createdMs"].as_u64().unwrap_or(0)));
    Ok(json!({"items":items}))
}
pub fn body(s: &LocalDataService, p: &Value) -> Result<String> {
    let path = field(p, "path")?;
    let version = field(p, "versionId")?;
    if version.len() != 64 || !version.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AppError::new("E_PATH_INVALID", "无效版本标识"));
    }
    let dir = folder(s, path)?;
    let record: Value = serde_json::from_slice(&files::read_limit(
        &dir.join(format!("{version}.json")),
        65536,
    )?)
    .map_err(|_| AppError::new("E_BACKUP_CHECKSUM", "版本记录损坏"))?;
    if record["path"] != path || record["versionId"] != version {
        return Err(AppError::new("E_BACKUP_CHECKSUM", "版本记录不匹配"));
    }
    let bytes = files::read_limit(&dir.join(format!("{version}.md")), NOTE_SIZE_LIMIT as u64)?;
    if files::digest(&bytes) != version {
        return Err(AppError::new("E_BACKUP_CHECKSUM", "版本内容校验失败"));
    }
    String::from_utf8(bytes).map_err(|_| AppError::new("E_ENCODING_INVALID", "历史版本不是 UTF-8"))
}
pub fn prune(s: &LocalDataService, now: u64) -> Result<usize> {
    let root = s.private.join("snapshots");
    if !root.exists() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in walkdir::WalkDir::new(&root).follow_links(false) {
        s.check_cancelled()?;
        let e = entry.map_err(|e| AppError::new("E_IO_PERMISSION", e.to_string()))?;
        files::no_links(e.path())?;
        if !e.file_type().is_file() || e.path().extension().and_then(|x| x.to_str()) != Some("json")
        {
            continue;
        }
        let v: Value = serde_json::from_slice(&files::read_limit(e.path(), 65536)?)
            .map_err(|_| AppError::new("E_BACKUP_CHECKSUM", "版本记录损坏，停止清理"))?;
        if now.saturating_sub(
            v["lastUsedMs"]
                .as_u64()
                .or_else(|| v["createdMs"].as_u64())
                .unwrap_or(now),
        ) > RETENTION
        {
            let blob = e.path().with_extension("md");
            files::no_links(&blob)?;
            fs::remove_file(e.path()).map_err(AppError::io)?;
            if blob.exists() {
                fs::remove_file(blob).map_err(AppError::io)?;
            }
            count += 1;
        }
    }
    Ok(count)
}
