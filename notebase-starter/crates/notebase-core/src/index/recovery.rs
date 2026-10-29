//! 恢复前保留原数据库及 WAL/SHM；可读取的业务状态复制到新模式。
use super::{sqlite::SqliteIndexStore, unavailable};
use crate::domain::{AppError, Result};
use rusqlite::{params_from_iter, types::Value, Connection, OpenFlags, OptionalExtension};
use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
};
fn saved_note(path: &Path) -> Option<serde_json::Value> {
    fs::read(path.with_extension("recovery-info.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}
fn save_note(path: &Path, archive: &Path, restored: bool) -> Result<()> {
    use std::io::Write;
    let info = serde_json::json!({"recoveryDir":archive,"businessRestored":restored,"originalPreserved":true});
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap()).map_err(AppError::io)?;
    file.write_all(&serde_json::to_vec_pretty(&info).map_err(|e| unavailable(e.to_string()))?)
        .map_err(AppError::io)?;
    file.as_file().sync_all().map_err(AppError::io)?;
    file.persist(path.with_extension("recovery-info.json"))
        .map_err(|e| AppError::io(e.error))?;
    Ok(())
}
const TABLES: [(&str, &str); 4] = [
    (
        "snapshot_index",
        "note_path,version_id,blob_rel,size_bytes,created_ms",
    ),
    (
        "sync_state",
        "path,base_hash,local_hash,remote_etag,device_id,last_synced_ms,state",
    ),
    ("sync_tombstone", "path,deleted_ms,base_hash"),
    (
        "rename_journal",
        "id,from_path,to_path,affected,done,created_ms,state",
    ),
];
pub(crate) type Business = Vec<(&'static str, Vec<Vec<Value>>)>;
pub struct Recovery {
    pub store: SqliteIndexStore,
    pub archived: Option<PathBuf>,
    pub business_restored: bool,
}
pub fn open(path: &Path, id: &str) -> Result<Recovery> {
    if !path.exists() {
        let note = saved_note(path);
        return Ok(Recovery {
            store: SqliteIndexStore::open(path, id)?,
            archived: note
                .as_ref()
                .and_then(|n| n.get("recoveryDir"))
                .and_then(|v| v.as_str())
                .map(PathBuf::from),
            business_restored: note
                .as_ref()
                .and_then(|n| n.get("businessRestored"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
        });
    }
    let metadata = fs::symlink_metadata(path).map_err(AppError::io)?;
    #[cfg(windows)]
    let link = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let link = metadata.file_type().is_symlink();
    if link {
        return Err(unavailable("拒绝恢复符号链接或重解析点数据库"));
    }
    let lease = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path.with_extension("writer.lock"))
        .map_err(AppError::io)?;
    lease.try_lock().map_err(|e| {
        let mut error = unavailable(e.to_string());
        error.retryable = true;
        error
    })?;
    // 在任何可写初始化前提取业务状态。身份不符绝不能当作缓存损坏处理。
    let original = Some(
        Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|e| unavailable(format!("无法只读打开原库，拒绝自动替换：{e}")))?,
    );
    if let Some(conn) = &original {
        if let Err(error) = conn.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)) {
            let corrupt = matches!(&error,rusqlite::Error::SqliteFailure(f,_) if matches!(f.code,rusqlite::ErrorCode::DatabaseCorrupt|rusqlite::ErrorCode::NotADatabase));
            if !corrupt {
                let mut result = unavailable(format!("原库访问暂时失败，未自动替换：{error}"));
                result.retryable = matches!(&error,rusqlite::Error::SqliteFailure(f,_) if matches!(f.code,rusqlite::ErrorCode::DatabaseBusy|rusqlite::ErrorCode::DatabaseLocked));
                return Err(result);
            }
        }
        if let Ok(Some(vault)) = conn
            .query_row("SELECT value FROM meta WHERE key='vault_id'", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()
        {
            if vault != id {
                return Err(unavailable("数据库属于其他笔记库，拒绝自动恢复"));
            }
        }
    }
    let business = original.as_ref().map(extract_business).transpose();
    let structurally_sound = original.as_ref().is_some_and(|conn| {
        super::sqlite::validate_schema(conn).is_ok()
            && conn
                .query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0))
                .is_ok_and(|value| value == "ok")
    });
    drop(original);
    match SqliteIndexStore::open_locked(path, id, &lease) {
        Ok(store) => {
            let note = saved_note(path);
            return Ok(Recovery {
                store,
                archived: note
                    .as_ref()
                    .and_then(|n| n.get("recoveryDir"))
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from),
                business_restored: note
                    .as_ref()
                    .and_then(|n| n.get("businessRestored"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
            });
        }
        Err(error)
            if (structurally_sound && error.detail["reason"] != "integrity_failed")
                || error.code != "E_INDEX_UNAVAILABLE" =>
        {
            return Err(error)
        }
        Err(_) => {}
    }
    let parent = path.parent().ok_or_else(|| unavailable("无索引父目录"))?;
    let archive = tempfile::Builder::new()
        .prefix("recovery-")
        .tempdir_in(parent)
        .map_err(AppError::io)?
        .keep();
    let mut moved = Vec::new();
    for source in [
        path.to_path_buf(),
        PathBuf::from(format!("{}-wal", path.display())),
        PathBuf::from(format!("{}-shm", path.display())),
    ] {
        if source.exists() {
            let target = archive.join(source.file_name().unwrap());
            if let Err(error) = fs::rename(&source, &target) {
                for (from, to) in moved.iter().rev() {
                    let _ = fs::rename(to, from);
                }
                return Err(AppError::io(error));
            }
            moved.push((source, target));
        }
    }
    let restored = matches!(&business, Ok(Some(_)));
    save_note(path, &archive, false)?;
    let store = SqliteIndexStore::open_locked(path, id, &lease).map_err(|mut error| {
        error.detail = serde_json::json!({"recoveryDir":archive});
        error
    })?;
    if let Ok(Some(business)) = business {
        restore_business(path, business).map_err(|mut error| {
            error.detail = serde_json::json!({"recoveryDir":archive,"originalPreserved":true});
            error
        })?;
    }
    save_note(path, &archive, restored)?;
    Ok(Recovery {
        store,
        archived: Some(archive),
        business_restored: restored,
    })
}
pub(crate) fn extract_business(conn: &Connection) -> Result<Business> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| unavailable(e.to_string()))?;
    let mut result = Vec::new();
    for (table, columns) in TABLES {
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
                [table],
                |r| r.get(0),
            )
            .map_err(|e| unavailable(e.to_string()))?;
        let mut rows = Vec::new();
        if exists {
            let mut statement = tx
                .prepare(&format!("SELECT {columns} FROM {table} ORDER BY rowid"))
                .map_err(|e| unavailable(e.to_string()))?;
            let count = columns.split(',').count();
            let values = statement
                .query_map([], |r| {
                    (0..count)
                        .map(|i| r.get(i))
                        .collect::<rusqlite::Result<Vec<Value>>>()
                })
                .map_err(|e| unavailable(e.to_string()))?;
            for row in values {
                rows.push(row.map_err(|e| unavailable(e.to_string()))?);
            }
        }
        result.push((table, rows));
    }
    Ok(result)
}
pub(crate) fn restore_business(path: &Path, business: Business) -> Result<()> {
    let mut conn = Connection::open(path).map_err(|e| unavailable(e.to_string()))?;
    let tx = conn.transaction().map_err(|e| unavailable(e.to_string()))?;
    for (table, rows) in business {
        let columns = TABLES.iter().find(|(name, _)| *name == table).unwrap().1;
        for row in rows {
            let parameters = vec!["?"; row.len()].join(",");
            tx.execute(
                &format!("INSERT INTO {table}({columns}) VALUES({parameters})"),
                params_from_iter(row.iter()),
            )
            .map_err(|e| unavailable(e.to_string()))?;
        }
    }
    tx.commit().map_err(|e| unavailable(e.to_string()))
}
