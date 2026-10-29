//! Local JSONL diagnostics. Callers cannot supply arbitrary messages or payloads.
use crate::domain::{AppError, Result};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::Mutex,
};
const DAY: u64 = 86_400_000;
const MAX_FILE: u64 = 1024 * 1024;
pub struct Logger {
    directory: PathBuf,
    guard: Mutex<()>,
}
impl Logger {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            guard: Mutex::new(()),
        }
    }
    pub fn record(&self, module: &str, event: &str, code: Option<&str>) -> Result<()> {
        self.record_at(crate::index::now_ms(), module, event, code)
    }
    pub fn record_at(&self, now: u64, module: &str, event: &str, code: Option<&str>) -> Result<()> {
        let _guard = self
            .guard
            .lock()
            .map_err(|_| AppError::new("E_INTERNAL", "日志锁异常"))?;
        fs::create_dir_all(&self.directory).map_err(AppError::io)?;
        for entry in fs::read_dir(&self.directory).map_err(AppError::io)? {
            let entry = entry.map_err(AppError::io)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(day) = name
                .strip_prefix("notebase-")
                .and_then(|s| s.split('.').next())
                .and_then(|s| s.parse::<u64>().ok())
            {
                if now / DAY >= day + 7 && entry.file_type().map_err(AppError::io)?.is_file() {
                    fs::remove_file(entry.path()).map_err(AppError::io)?;
                }
            }
        }
        let safe_module = if matches!(module, "app-shell" | "ui" | "logger") {
            module
        } else {
            "app-shell"
        };
        let safe_event = if crate::domain::COMMANDS.contains(&event)
            || matches!(event, "startup" | "shutdown" | "shortcut" | "window")
        {
            event
        } else {
            "unknown"
        };
        let safe_code = code.filter(|code| {
            matches!(
                *code,
                "E_IO_DISK_FULL"
                    | "E_IO_PERMISSION"
                    | "E_IO_LOCKED"
                    | "E_NOT_FOUND"
                    | "E_EXTERNAL_MODIFIED"
                    | "E_STALE_CLIENT"
                    | "E_FILE_TOO_LARGE"
                    | "E_ENCODING_INVALID"
                    | "E_PATH_INVALID"
                    | "E_INDEX_UNAVAILABLE"
                    | "E_QUERY_SYNTAX"
                    | "E_QUERY_UNSUPPORTED"
                    | "E_SYNC_AUTH"
                    | "E_SYNC_NETWORK"
                    | "E_SYNC_BUSY"
                    | "E_PARTIAL_RENAME"
                    | "E_BACKUP_CHECKSUM"
                    | "E_FORMAT_UNSUPPORTED"
                    | "E_VAULT_NOT_FOUND"
                    | "E_PDF_UNSUPPORTED"
                    | "E_KEYMAP_CONFLICT"
                    | "E_CANCELLED"
                    | "E_INTERNAL"
            )
        });
        let path = self.directory.join(format!("notebase-{}.jsonl", now / DAY));
        if fs::metadata(&path).is_ok_and(|m| m.len() >= MAX_FILE) {
            let rotated = path.with_extension("previous.jsonl");
            if rotated.exists() {
                fs::remove_file(&rotated).map_err(AppError::io)?;
            }
            fs::rename(&path, &rotated).map_err(AppError::io)?;
        }
        let value = serde_json::json!({"ts":now,"module":safe_module,"level":if safe_code.is_some(){"error"}else{"info"},"event":safe_event,"code":safe_code,"path":null});
        let mut output = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(AppError::io)?;
        writeln!(output, "{value}").map_err(AppError::io)
    }
}
