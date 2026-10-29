use super::{
    now_ms, unavailable, IndexState, IndexStatus, IndexStore, NoteRecord, BATCH_SIZE,
    SCHEMA_VERSION, TOKENIZER_VERSION,
};
use crate::domain::{RelPath, Result};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex, MutexGuard,
    },
    time::Duration,
};

pub const SCHEMA: &str = include_str!("schema.sql");

pub struct SqliteIndexStore {
    writer: Mutex<Connection>,
    readers: [Mutex<Connection>; 2],
    next_reader: AtomicUsize,
    path: PathBuf,
    // 保证遵守端口的多个进程/会话也不能同时持有写连接。
    _writer_lease: File,
    pub needs_rebuild: bool,
}

fn db<T>(value: rusqlite::Result<T>) -> Result<T> {
    value.map_err(|e| unavailable(format!("索引数据库错误：{e}")))
}
fn lock(connection: &Mutex<Connection>) -> Result<MutexGuard<'_, Connection>> {
    connection.lock().map_err(|_| unavailable("索引连接锁异常"))
}
fn meta(connection: &Connection, key: &str) -> Result<Option<String>> {
    db(connection
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .optional())
}
fn set_meta(connection: &Connection, key: &str, value: &str) -> Result<()> {
    db(connection.execute("INSERT INTO meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value]))?;
    Ok(())
}
fn configure(connection: &Connection, writer: bool) -> Result<()> {
    db(connection.busy_timeout(Duration::from_millis(5000)))?;
    db(connection.execute_batch("PRAGMA foreign_keys=ON; PRAGMA cache_size=-65536;"))?;
    if writer {
        let mode: String =
            db(connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0)))?;
        if mode.to_lowercase() != "wal" {
            return Err(unavailable("索引数据库无法启用 WAL"));
        }
        db(connection.execute_batch("PRAGMA synchronous=NORMAL;"))?;
    } else {
        db(connection.execute_batch("PRAGMA query_only=ON;"))?;
    }
    Ok(())
}

impl SqliteIndexStore {
    pub fn open(path: &Path, vault_id: &str) -> Result<Self> {
        if fs::symlink_metadata(path).is_ok_and(|m| {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                m.file_attributes() & 0x400 != 0
            }
            #[cfg(not(windows))]
            {
                m.file_type().is_symlink()
            }
        }) {
            return Err(unavailable("索引文件不能是符号链接"));
        }
        let lease_path = path.with_extension("writer.lock");
        let writer_lease = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lease_path)
            .map_err(crate::domain::AppError::io)?;
        writer_lease.try_lock().map_err(|e| {
            let mut error = unavailable(format!("此库已有索引写入会话：{e}"));
            error.retryable = true;
            error
        })?;
        let mut writer = db(Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        ))?;
        configure(&writer, true)?;
        let tables: i64 = db(writer.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        ))?;
        if tables == 0 {
            let tx = db(writer.transaction())?;
            // 连接级 PRAGMA 已设置，模式中的 PRAGMA 不在事务内重复执行。
            db(tx.execute_batch(
                &SCHEMA
                    .lines()
                    .filter(|line| !line.starts_with("PRAGMA "))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ))?;
            set_meta(&tx, "schema_version", SCHEMA_VERSION)?;
            set_meta(&tx, "tokenizer_version", TOKENIZER_VERSION)?;
            set_meta(&tx, "vault_id", vault_id)?;
            set_meta(&tx, "build_state", "offline")?;
            db(tx.commit())?;
        }
        // 未知模式不尝试删除数据库，避免连带丢失同步/快照/重命名状态。
        validate_schema(&writer)?;
        if meta(&writer, "vault_id")?.as_deref() != Some(vault_id) {
            return Err(unavailable("索引库身份不匹配，原数据库已保留"));
        }
        let integrity: String = db(writer.query_row("PRAGMA quick_check", [], |row| row.get(0)))?;
        if integrity != "ok" {
            return Err(unavailable("索引校验失败，原数据库已保留"));
        }
        let needs_rebuild = meta(&writer, "schema_version")?.as_deref() != Some(SCHEMA_VERSION)
            || meta(&writer, "tokenizer_version")?.as_deref() != Some(TOKENIZER_VERSION)
            || meta(&writer, "build_state")?.as_deref() != Some("ready");
        let read = || -> Result<Mutex<Connection>> {
            let conn = db(Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            ))?;
            configure(&conn, false)?;
            Ok(Mutex::new(conn))
        };
        Ok(Self {
            readers: [read()?, read()?],
            writer: Mutex::new(writer),
            next_reader: AtomicUsize::new(0),
            path: path.into(),
            _writer_lease: writer_lease,
            needs_rebuild,
        })
    }

    pub fn read<T>(&self, query: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let index = self.next_reader.fetch_add(1, Ordering::Relaxed) % 2;
        db(query(&*lock(&self.readers[index])?))
    }

    pub fn begin_build(&self) -> Result<()> {
        let mut writer = lock(&self.writer)?;
        let tx = db(writer.transaction())?;
        db(tx.execute_batch("DELETE FROM links; DELETE FROM aliases; DELETE FROM tags; DELETE FROM note_fts; DELETE FROM notes;"))?;
        set_meta(&tx, "schema_version", SCHEMA_VERSION)?;
        set_meta(&tx, "tokenizer_version", TOKENIZER_VERSION)?;
        set_meta(&tx, "build_state", "rebuilding")?;
        set_meta(&tx, "rebuild_cursor", "")?;
        db(tx.commit())
    }
    pub fn optimize(&self) -> Result<()> {
        db(lock(&self.writer)?.execute("INSERT INTO note_fts(note_fts) VALUES('optimize')", []))?;
        Ok(())
    }
    pub fn finish_build(&self) -> Result<()> {
        let mut writer = lock(&self.writer)?;
        let tx = db(writer.transaction())?;
        set_meta(&tx, "build_state", "ready")?;
        set_meta(&tx, "last_full_build_ms", &now_ms().to_string())?;
        set_meta(&tx, "rebuild_cursor", "")?;
        db(tx.commit())
    }
    pub fn mark_offline(&self) -> Result<()> {
        set_meta(&*lock(&self.writer)?, "build_state", "offline")
    }
    pub fn fingerprints(&self) -> Result<std::collections::BTreeMap<String, (String, u64, u64)>> {
        self.read(|conn| {
            let mut stmt =
                conn.prepare("SELECT path,content_hash,mtime_ms,size_bytes FROM notes")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    (
                        r.get(1)?,
                        r.get::<_, i64>(2)? as u64,
                        r.get::<_, i64>(3)? as u64,
                    ),
                ))
            })?;
            rows.collect()
        })
    }
    pub fn finish_increment(&self, full: bool) -> Result<()> {
        let mut writer = lock(&self.writer)?;
        let tx = db(writer.transaction())?;
        let revision = meta(&tx, "index_revision")?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            .saturating_add(1);
        set_meta(&tx, "index_revision", &revision.to_string())?;
        set_meta(&tx, "build_state", "ready")?;
        if full {
            set_meta(&tx, "last_reconcile_ms", &now_ms().to_string())?;
        }
        if meta(&tx, "last_full_build_ms")?.is_none() {
            set_meta(&tx, "last_full_build_ms", &now_ms().to_string())?;
        }
        db(tx.commit())
    }
    pub fn fingerprints_for(
        &self,
        paths: impl Iterator<Item = String>,
    ) -> Result<std::collections::BTreeMap<String, (String, u64, u64)>> {
        self.read(|conn| {
            let tx = conn.unchecked_transaction()?;
            let mut stmt =
                tx.prepare("SELECT content_hash,mtime_ms,size_bytes FROM notes WHERE path=?1")?;
            let mut values = std::collections::BTreeMap::new();
            for path in paths {
                if let Some(value) = stmt
                    .query_row([&path], |r| {
                        Ok((
                            r.get(0)?,
                            r.get::<_, i64>(1)? as u64,
                            r.get::<_, i64>(2)? as u64,
                        ))
                    })
                    .optional()?
                {
                    values.insert(path, value);
                }
            }
            Ok(values)
        })
    }
}

fn validate_schema(connection: &Connection) -> Result<()> {
    let expected = db(Connection::open_in_memory())?;
    db(expected.execute_batch(SCHEMA))?;
    let catalog = |conn: &Connection| -> Result<Vec<(String, String)>> {
        let mut statement=db(conn.prepare("SELECT name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' AND sql IS NOT NULL ORDER BY name"))?;
        let rows = db(statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?
                    .split_whitespace()
                    .collect::<String>(),
            ))
        }))?;
        db(rows.collect())
    };
    if catalog(connection)? != catalog(&expected)? {
        return Err(unavailable(
            "未知表、索引或触发器结构，原数据库已保留，需专门迁移",
        ));
    }
    Ok(())
}

fn upsert(tx: &Transaction<'_>, record: &NoteRecord) -> Result<()> {
    if !record.path.as_str().to_ascii_lowercase().ends_with(".md") {
        return Err(unavailable("索引记录必须对应 Markdown 文件"));
    }
    if record.content_hash.len() != 64
        || !record.content_hash.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(unavailable("索引 content_hash 必须为 64 位十六进制"));
    }
    // UPDATE 不使用 REPLACE，保留指向同一路径的外键。
    db(tx.execute("INSERT INTO notes(path,title,size_bytes,mtime_ms,content_hash,created_ms,updated_ms,has_attachment,indexed_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(path) DO UPDATE SET title=excluded.title,size_bytes=excluded.size_bytes,mtime_ms=excluded.mtime_ms,content_hash=excluded.content_hash,created_ms=excluded.created_ms,updated_ms=excluded.updated_ms,has_attachment=excluded.has_attachment,indexed_at_ms=excluded.indexed_at_ms",params![record.path.as_str(),record.title,i64::try_from(record.size_bytes).map_err(|_|unavailable("文件大小超过 SQLite 整数范围"))?,i64::try_from(record.mtime_ms).map_err(|_|unavailable("修改时间超过 SQLite 整数范围"))?,record.content_hash,record.created_ms,record.updated_ms,record.has_attachment,now_ms() as i64]))?;
    let rowid: i64 = db(tx.query_row(
        "SELECT rowid FROM notes WHERE path=?1",
        [record.path.as_str()],
        |r| r.get(0),
    ))?;
    // 共享 rowid 避免每次更新扫描 UNINDEXED path 列，路径仍是唯一业务身份。
    db(tx.execute("DELETE FROM note_fts WHERE rowid=?1", [rowid]))?;
    db(tx.execute(
        "INSERT INTO note_fts(rowid,path,title,body) VALUES(?1,?2,?3,?4)",
        params![rowid, record.path.as_str(), record.title, record.body],
    ))?;
    for (table, column, values) in [
        ("aliases", "alias", &record.aliases),
        ("tags", "tag", &record.tags),
    ] {
        db(tx.execute(
            &format!("DELETE FROM {table} WHERE note_path=?1"),
            [record.path.as_str()],
        ))?;
        for value in values {
            db(tx.execute(
                &format!("INSERT OR IGNORE INTO {table}(note_path,{column}) VALUES(?1,?2)"),
                params![record.path.as_str(), value],
            ))?;
        }
    }
    Ok(())
}

impl IndexStore for SqliteIndexStore {
    fn search(&self, query: &serde_json::Value) -> Result<serde_json::Value> {
        self.read(|conn| Ok(super::search::search(conn, query)))?
    }
    fn upsert_batch(&self, notes: &[NoteRecord]) -> Result<()> {
        if notes.len() > BATCH_SIZE {
            return Err(unavailable("单次索引事务不得超过 200 篇"));
        }
        let mut writer = lock(&self.writer)?;
        let tx = db(writer.transaction())?;
        for record in notes {
            upsert(&tx, record)?;
        }
        if let Some(last) = notes.last() {
            set_meta(&tx, "rebuild_cursor", last.path.as_str())?;
        }
        db(tx.commit())
    }
    fn remove(&self, paths: &[RelPath]) -> Result<()> {
        if paths.len() > BATCH_SIZE {
            return Err(unavailable("单次索引事务不得超过 200 篇"));
        }
        let mut writer = lock(&self.writer)?;
        let tx = db(writer.transaction())?;
        for path in paths {
            db(tx.execute(
                "UPDATE links SET resolved=0,ambiguous=0 WHERE target_path=?1",
                [path.as_str()],
            ))?;
            db(tx.execute(
                "DELETE FROM note_fts WHERE rowid IN (SELECT rowid FROM notes WHERE path=?1)",
                [path.as_str()],
            ))?;
            db(tx.execute("DELETE FROM notes WHERE path=?1", [path.as_str()]))?;
        }
        db(tx.commit())
    }
    fn get(&self, path: &RelPath) -> Result<Option<NoteRecord>> {
        self.read(|conn| {
            let tx=conn.unchecked_transaction()?;
            let record=tx.query_row("SELECT n.title,f.body,n.size_bytes,n.mtime_ms,n.content_hash,n.created_ms,n.updated_ms,n.has_attachment FROM notes n JOIN note_fts f ON n.path=f.path WHERE n.path=?1",[path.as_str()],|row|Ok(NoteRecord { path:path.clone(),title:row.get(0)?,body:row.get(1)?,size_bytes:row.get::<_,i64>(2)? as u64,mtime_ms:row.get::<_,i64>(3)? as u64,content_hash:row.get(4)?,created_ms:row.get(5)?,updated_ms:row.get(6)?,has_attachment:row.get(7)?,aliases:Vec::new(),tags:Vec::new() })).optional()?;
            if let Some(mut record)=record {
                for (table,column,values) in [("aliases","alias",&mut record.aliases),("tags","tag",&mut record.tags)] {
                    let mut stmt=tx.prepare(&format!("SELECT {column} FROM {table} WHERE note_path=?1 ORDER BY {column}"))?;
                    *values=stmt.query_map([path.as_str()],|row|row.get(0))?.collect::<rusqlite::Result<_>>()?;
                }
                Ok(Some(record))
            } else { Ok(None) }
        })
    }
    fn status(&self) -> Result<IndexStatus> {
        let (note_count, state, last): (i64, String, Option<String>) = self.read(|conn| {
            let tx = conn.unchecked_transaction()?;
            Ok((
                tx.query_row("SELECT count(*) FROM notes", [], |r| r.get(0))?,
                tx.query_row("SELECT value FROM meta WHERE key='build_state'", [], |r| {
                    r.get(0)
                })?,
                tx.query_row(
                    "SELECT value FROM meta WHERE key='last_full_build_ms'",
                    [],
                    |r| r.get(0),
                )
                .optional()?,
            ))
        })?;
        let index_bytes = [
            self.path.clone(),
            PathBuf::from(format!("{}-wal", self.path.display())),
            PathBuf::from(format!("{}-shm", self.path.display())),
        ]
        .iter()
        .filter_map(|p| fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
        Ok(IndexStatus {
            state: match state.as_str() {
                "ready" => IndexState::Ready,
                "rebuilding" => IndexState::Rebuilding,
                _ => IndexState::Offline,
            },
            note_count: note_count as u64,
            index_bytes,
            last_full_build_ms: last.and_then(|v| v.parse().ok()),
            progress: None,
        })
    }
}
