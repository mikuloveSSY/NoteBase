use super::{
    extract::extract,
    queue::{Source, Work},
    sqlite::SqliteIndexStore,
    IndexStore, BATCH_SIZE,
};
use crate::domain::{AppError, NoteStore, RelPath, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicBool, Ordering},
};
pub struct Change {
    pub path: String,
    pub hash: String,
    pub source: Source,
}
/// 单写线程调用；全库对账与增量更新共用相同的读取、比较、删除规则。
pub fn apply(
    index: &SqliteIndexStore,
    store: &dyn NoteStore,
    work: Work,
    invalid: &mut BTreeSet<String>,
    cancelled: &AtomicBool,
) -> Result<Vec<Change>> {
    let fingerprints = if work.reconcile {
        index.fingerprints()?
    } else {
        index.fingerprints_for(work.paths.keys().cloned())?
    };
    let mut paths = work.paths;
    if work.reconcile {
        let entries = store.list(&RelPath::root())?;
        let live: BTreeSet<_> = entries.into_iter().map(|e| e.path).collect();
        invalid.retain(|p| live.contains(p));
        for p in live.iter().chain(fingerprints.keys()) {
            paths.entry(p.clone()).or_insert(Source::External);
        }
    }
    let mut changes = Vec::new();
    let mut records = Vec::new();
    let mut removes = Vec::new();
    let mut bytes = 0;
    let mut pending_changes = Vec::new();
    for (name, source) in paths {
        if cancelled.load(Ordering::Acquire) {
            return Err(AppError::new("E_CANCELLED", "增量索引已取消"));
        }
        let path = RelPath::new(&name)?;
        let result = store.read(&path).map_err(AppError::from).and_then(|note| {
            let unchanged = !note.truncated
                && fingerprints.get(&name).is_some_and(|(hash, mtime, size)| {
                    *mtime == note.mtime_ms
                        && *size == note.size_bytes
                        && *hash == format!("{:x}", Sha256::digest(&note.raw))
                });
            if unchanged {
                Ok(None)
            } else {
                extract(path.clone(), note).map(Some)
            }
        });
        match result {
            Ok(None) => {
                invalid.remove(&name);
            }
            Ok(Some(record)) => {
                invalid.remove(&name);
                let old = fingerprints.get(&name);
                if old.is_none_or(|(hash, mtime, size)| {
                    hash != &record.content_hash
                        || *mtime != record.mtime_ms
                        || *size != record.size_bytes
                }) {
                    if old.is_none_or(|(hash, _, _)| hash != &record.content_hash) {
                        pending_changes.push(Change {
                            path: name,
                            hash: format!("sha256:{}", record.content_hash),
                            source,
                        });
                    }
                    bytes += record.size_bytes;
                    records.push(record);
                }
            }
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "E_NOT_FOUND" | "E_ENCODING_INVALID" | "E_FILE_TOO_LARGE" | "E_PATH_INVALID"
                ) =>
            {
                if error.code == "E_NOT_FOUND" {
                    invalid.remove(&name);
                } else {
                    invalid.insert(name.clone());
                }
                if fingerprints.contains_key(&name) {
                    removes.push(path);
                    pending_changes.push(Change {
                        path: name,
                        hash: String::new(),
                        source,
                    });
                }
            }
            Err(error) => return Err(error),
        }
        if records.len() + removes.len() >= BATCH_SIZE || bytes >= 8 * 1024 * 1024 {
            flush(index, &mut records, &mut removes)?;
            changes.append(&mut pending_changes);
            bytes = 0;
        }
    }
    flush(index, &mut records, &mut removes)?;
    changes.append(&mut pending_changes);
    Ok(changes)
}
fn flush(
    index: &SqliteIndexStore,
    records: &mut Vec<super::NoteRecord>,
    removes: &mut Vec<RelPath>,
) -> Result<()> {
    if !records.is_empty() {
        index.upsert_batch(records)?;
        records.clear();
    }
    if !removes.is_empty() {
        index.remove(removes)?;
        removes.clear();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::FsStore;
    use std::fs;
    #[test]
    fn reconcile_preserves_unchanged_records_but_checks_hash_when_stat_is_equal() {
        let temp = tempfile::tempdir().unwrap();
        let vault = temp.path().join("vault");
        fs::create_dir(&vault).unwrap();
        let file = vault.join("a.md");
        fs::write(&file, "---\ntitle: 标题甲\n---\n正文甲").unwrap();
        let modified = fs::metadata(&file).unwrap().modified().unwrap();
        let store = FsStore::open(vault.to_str().unwrap()).unwrap();
        let index =
            SqliteIndexStore::open(&temp.path().join("index.db"), "0123456789abcdef").unwrap();
        index.begin_build().unwrap();
        let mut invalid = BTreeSet::new();
        let full = || Work {
            paths: Default::default(),
            reconcile: true,
            warning: None,
        };
        let cancel = AtomicBool::new(false);
        assert_eq!(
            apply(&index, &store, full(), &mut invalid, &cancel)
                .unwrap()
                .len(),
            1
        );
        let path = RelPath::new("a.md").unwrap();
        let before = index.get(&path).unwrap().unwrap();
        assert!(apply(&index, &store, full(), &mut invalid, &cancel)
            .unwrap()
            .is_empty());
        assert_eq!(
            index.get(&path).unwrap().unwrap().content_hash,
            before.content_hash
        );
        fs::write(&file, "---\ntitle: 标题乙\n---\n正文乙").unwrap();
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let stat = store.stat(&path).unwrap();
        assert_eq!(
            (stat.mtime_ms, stat.size_bytes),
            (before.mtime_ms, before.size_bytes)
        );
        assert_eq!(
            apply(&index, &store, full(), &mut invalid, &cancel)
                .unwrap()
                .len(),
            1
        );
        let after = index.get(&path).unwrap().unwrap();
        assert_ne!(after.content_hash, before.content_hash);
        assert_eq!(after.title, "标题乙");
        assert!(after.body.contains("正文乙"));
    }
}
