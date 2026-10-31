use super::{files, LocalDataService};
use crate::domain::{field, AppError, Result, NOTE_SIZE_LIMIT};
use serde_json::{json, Value};
use std::fs;
fn retention(s: &LocalDataService) -> Result<Option<u64>> {
    let config = super::backup::settings(s)?;
    let days = config["versionRetentionDays"]
        .as_u64()
        .ok_or_else(|| AppError::new("E_FORMAT_UNSUPPORTED", "版本保留配置无效"))?;
    Ok((days != 0).then_some(days * 86_400_000))
}
const SEQUENCE: &str = ".notebase-history-sequence";
fn sequence(s: &LocalDataService, dir: &std::path::Path, now: u64) -> Result<u64> {
    let path = dir.join(SEQUENCE);
    let mut state: Value = if path.exists() {
        let state: Value = serde_json::from_slice(&files::read_limit(&path, 65536)?)
            .map_err(|_| AppError::new("E_BACKUP_CHECKSUM", "版本序列损坏，保留原件"))?;
        if !state.is_object()
            || state["lastSequence"].as_u64().is_none()
            || state["lastObservedMs"].as_u64().is_none()
        {
            return Err(AppError::new(
                "E_BACKUP_CHECKSUM",
                "版本序列结构无效，保留原件",
            ));
        }
        state
    } else {
        let mut last = 0;
        for file in fs::read_dir(dir).map_err(AppError::io)? {
            let file = file.map_err(AppError::io)?.path();
            if file.extension().and_then(|v| v.to_str()) == Some("json") {
                let record: Value = serde_json::from_slice(&files::read_limit(&file, 65536)?)
                    .map_err(|_| AppError::new("E_BACKUP_CHECKSUM", "版本元数据无效，保留原件"))?;
                last = last.max(
                    record["sequence"]
                        .as_u64()
                        .or_else(|| record["createdMs"].as_u64())
                        .unwrap_or(0),
                );
            }
        }
        json!({"lastSequence":last,"lastObservedMs":now,"rollbackCount":0,"rollbackActive":false})
    };
    let previous = state["lastObservedMs"].as_u64().unwrap();
    let rollback = now.saturating_add(5000) < previous;
    if rollback && state["rollbackActive"] != true {
        let data_root = s
            .private
            .parent()
            .and_then(|p| p.parent())
            .ok_or_else(|| AppError::new("E_PATH_INVALID", "私有目录无效"))?;
        crate::logger::Logger::new(data_root.join("logs")).record(
            "data-services",
            "clock_rollback",
            None,
        )?;
        state["rollbackCount"] = json!(state["rollbackCount"]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(1));
    }
    let next = state["lastSequence"]
        .as_u64()
        .unwrap()
        .checked_add(1)
        .ok_or_else(|| AppError::new("E_BACKUP_CHECKSUM", "版本序列溢出"))?
        .max(now);
    state["lastSequence"] = json!(next);
    state["lastObservedMs"] = json!(previous.max(now));
    state["rollbackActive"] = json!(rollback);
    files::atomic(&path, &serde_json::to_vec(&state).unwrap(), true)?;
    Ok(next)
}
fn folder(s: &LocalDataService, path: &str) -> Result<std::path::PathBuf> {
    crate::domain::RelPath::new(path)?;
    Ok(s.private
        .join("snapshots")
        .join(files::digest(path.as_bytes())))
}
/// Reuse the durable note rename journal instead of guessing ownership from file content.
pub fn rename(
    s: &LocalDataService,
    from: &str,
    to: &str,
    id: &str,
    check_only: bool,
) -> Result<()> {
    if from == to || id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AppError::new("E_PATH_INVALID", "无效历史重命名事务"));
    }
    let source = folder(s, from)?;
    let target = folder(s, to)?;
    files::no_links(&source)?;
    files::no_links(&target)?;
    let marker = ".notebase-history-move";
    let ownership = json!({"journalId":id,"from":from,"to":to});
    let directory = if target.exists() {
        let owned = files::read_limit(&target.join(marker), 65536)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        if source.exists() || owned.as_ref() != Some(&ownership) {
            return Err(AppError::new(
                "E_PATH_INVALID",
                "目标笔记已有其他历史，拒绝合并或覆盖",
            ));
        }
        &target
    } else if source.exists() {
        &source
    } else {
        return Ok(());
    };
    // Validate all records and blobs before moving the directory or rewriting any record.
    let mut records = Vec::new();
    for entry in fs::read_dir(directory).map_err(AppError::io)? {
        let entry = entry.map_err(AppError::io)?;
        files::no_links(&entry.path())?;
        if entry.file_name() == marker || entry.file_name() == SEQUENCE {
            continue;
        }
        let file = entry.path();
        let ext = file.extension().and_then(|v| v.to_str());
        let version = file.file_stem().and_then(|v| v.to_str()).unwrap_or("");
        if !entry.file_type().map_err(AppError::io)?.is_file()
            || !matches!(ext, Some("json" | "md"))
            || version.len() != 64
            || !version.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(AppError::new(
                "E_BACKUP_CHECKSUM",
                "历史目录含未知条目，保留原件",
            ));
        }
        if ext != Some("json") {
            continue;
        }
        let mut record: Value = serde_json::from_slice(&files::read_limit(&file, 65536)?)
            .map_err(|_| AppError::new("E_BACKUP_CHECKSUM", "历史元数据损坏，保留原件"))?;
        if (record["path"] != from && record["path"] != to)
            || record["versionId"] != version
            || record["contentHash"] != format!("sha256:{version}")
            || files::digest(&files::read_limit(
                &file.with_extension("md"),
                NOTE_SIZE_LIMIT as u64,
            )?) != version
        {
            return Err(AppError::new(
                "E_BACKUP_CHECKSUM",
                "历史内容或归属不匹配，保留原件",
            ));
        }
        record["path"] = json!(to);
        records.push((entry.file_name(), record));
    }
    if check_only {
        return Ok(());
    }
    if directory == &source {
        files::atomic(
            &source.join(marker),
            &serde_json::to_vec(&ownership).unwrap(),
            true,
        )?;
        crate::migration::publish(&source, &target)?;
    }
    for (name, record) in records {
        files::atomic(
            &target.join(name),
            &serde_json::to_vec_pretty(&record).unwrap(),
            true,
        )?;
    }
    Ok(())
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
    if retention(s)?.is_none() {
        return Ok(json!({"path":path,"versionId":hash,"snapshotCreated":false}));
    }
    let dir = folder(s, path)?;
    files::directory(&dir)?;
    let now = crate::index::now_ms();
    let sequence = sequence(s, &dir, now)?;
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
        v["lastUsedMs"] = json!(v["lastUsedMs"].as_u64().unwrap_or(0).max(now));
        if v["sequence"].as_u64().is_none() {
            v["sequence"] = json!(sequence);
        }
        files::atomic(&record, &serde_json::to_vec_pretty(&v).unwrap(), true)?;
    }
    Ok(json!({"path":path,"versionId":hash,"snapshotCreated":true}))
}
pub fn list(s: &LocalDataService, p: &Value) -> Result<Value> {
    let path = field(p, "path")?;
    let dir = folder(s, path)?;
    let Some(retention) = retention(s)? else {
        return Ok(json!({"items":[]}));
    };
    let mut items = Vec::<Value>::new();
    if dir.exists() {
        files::no_links(&dir)?;
        for f in fs::read_dir(&dir).map_err(AppError::io)? {
            let f = f.map_err(AppError::io)?;
            if f.path().extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let v: Value = serde_json::from_slice(&files::read_limit(&f.path(), 65536)?)
                .map_err(|_| AppError::new("E_BACKUP_CHECKSUM", "历史记录损坏，保留原件"))?;
            if v["path"] != path {
                return Err(AppError::new("E_BACKUP_CHECKSUM", "版本笔记路径不匹配"));
            }
            if crate::index::now_ms().saturating_sub(
                v["lastUsedMs"]
                    .as_u64()
                    .or_else(|| v["createdMs"].as_u64())
                    .unwrap_or(0),
            ) <= retention
            {
                items.push(v);
            }
        }
    }
    items.sort_by_key(|v| {
        std::cmp::Reverse(
            v["sequence"]
                .as_u64()
                .or_else(|| v["createdMs"].as_u64())
                .unwrap_or(0),
        )
    });
    for item in &mut items {
        let fields = item
            .as_object_mut()
            .ok_or_else(|| AppError::new("E_BACKUP_CHECKSUM", "版本记录不是对象"))?;
        fields.remove("lastUsedMs");
        fields.remove("sequence");
    }
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
    let Some(retention) = retention(s)? else {
        return Ok(0);
    };
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
        ) > retention
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
