use crate::domain::{self, field, AppError, NoteStore, RelPath, Request, Response, Result};
use crate::index::{IndexSession, IndexStatus};
use crate::migration::VaultLocation;
use crate::note_files::NoteFiles;
use crate::note_service::{content_hash, NoteService};
use crate::note_state::NoteStateStore;
use serde_json::{json, Value};
use std::sync::Arc;

type StoreFactory = Box<dyn Fn(&str) -> Result<Box<dyn NoteStore>> + Send + Sync>;
pub type IndexFactory = Box<dyn Fn(&str, &str) -> Box<dyn IndexSession> + Send + Sync>;
pub type SupportFactory =
    Box<dyn Fn(&str, &str) -> Result<(Box<dyn NoteFiles>, Box<dyn NoteStateStore>)> + Send + Sync>;
pub type NoteEvents = Arc<dyn Fn(&str, Value) + Send + Sync>;
pub type DataFactory =
    Box<dyn Fn(&str, &str) -> Result<Box<dyn crate::data_services::DataService>> + Send + Sync>;
pub struct App {
    last_sync_attempt: u64,
    data_factory: Option<DataFactory>,
    data: Option<Box<dyn crate::data_services::DataService>>,
    notes: Option<NoteService>,
    factory: StoreFactory,
    index_factory: Option<IndexFactory>,
    index: Option<Box<dyn IndexSession>>,
    support: Option<SupportFactory>,
    events: Option<NoteEvents>,
    location: Option<Box<dyn VaultLocation>>,
}
impl App {
    pub fn new(factory: StoreFactory) -> Self {
        Self {
            last_sync_attempt: 0,
            data_factory: None,
            data: None,
            notes: None,
            factory,
            index_factory: None,
            index: None,
            support: None,
            events: None,
            location: None,
        }
    }
    pub fn with_index(mut self, factory: IndexFactory) -> Self {
        self.index_factory = Some(factory);
        self
    }
    pub fn with_data(mut self, factory: DataFactory) -> Self {
        self.data_factory = Some(factory);
        self
    }
    fn data(&self) -> Result<&dyn crate::data_services::DataService> {
        self.data
            .as_deref()
            .ok_or_else(|| AppError::new("E_INTERNAL", "数据保障端口尚未装配"))
    }
    /// Called by the shell's maintenance timer; no extra IPC command is introduced.
    pub fn maintenance(&mut self, now: u64) -> Result<Value> {
        if self.data.is_none() {
            return Ok(json!({"created":false}));
        }
        let pruned = self.data()?.prune_versions(now)?;
        if !self.data()?.maintenance_due(now)? {
            return Ok(json!({"created":false,"prunedVersions":pruned}));
        }
        self.with_index_paused(|service| service.maintenance(now))
    }
    pub fn background_sync(&mut self, now: u64) -> Result<Value> {
        if self.data.is_none() || now.saturating_sub(self.last_sync_attempt) < 300000 {
            return Ok(json!({"started":false}));
        }
        let status = self.data()?.sync_status()?;
        if status["state"] == "unconfigured" || status["authRequired"] == true {
            return Ok(json!({"started":false}));
        }
        self.last_sync_attempt = now;
        self.handle("sync_now", &json!({}))
    }
    fn with_index_paused(
        &mut self,
        f: impl FnOnce(&dyn crate::data_services::DataService) -> Result<Value>,
    ) -> Result<Value> {
        self.data()?;
        let vault = self.notes()?.store.vault_path();
        let id = content_hash(vault.as_bytes())[7..23].to_owned();
        if let Some(index) = self.index.as_mut() {
            index.suspend()?;
        }
        self.index = None;
        let result = f(self.data()?);
        self.index = self
            .index_factory
            .as_ref()
            .map(|factory| factory(&vault, &id));
        result
    }
    pub fn with_location(mut self, location: Box<dyn VaultLocation>) -> Self {
        self.location = Some(location);
        self
    }
    pub fn with_support(mut self, factory: SupportFactory) -> Self {
        self.support = Some(factory);
        self
    }
    pub fn with_events(mut self, events: NoteEvents) -> Self {
        self.events = Some(events);
        self
    }
    pub fn dispatch(&mut self, mut r: Request) -> Response {
        if r.payload.is_object() && r.command == "note_save" && r.payload.get("clientSeq").is_none()
        {
            r.payload["clientSeq"] = json!(r.client_seq);
        }
        let result = self.handle(&r.command, &r.payload);
        if let (Some(events), Err(error)) = (&self.events, &result) {
            if error.code == "E_EXTERNAL_MODIFIED" {
                events(
                    "note.conflict",
                    json!({"path":r.payload["path"],"clientId":r.payload["clientId"],"detail":error.detail}),
                );
            }
        }
        match result {
            Ok(data) => Response::Ok {
                id: r.id,
                ok: true,
                data,
            },
            Err(error) => Response::Err {
                id: r.id,
                ok: false,
                error,
            },
        }
    }
    fn notes(&mut self) -> Result<&mut NoteService> {
        self.notes
            .as_mut()
            .ok_or_else(|| AppError::new("E_VAULT_NOT_FOUND", "请先打开笔记库"))
    }
    fn assemble_notes(&self, store: Box<dyn NoteStore>, id: &str) -> Result<NoteService> {
        let support = self
            .support
            .as_ref()
            .ok_or_else(|| AppError::new("E_INTERNAL", "笔记持久化端口尚未装配"))?;
        let (files, state) = support(&store.vault_path(), id)?;
        Ok(NoteService::new(store, files, state))
    }
    fn settings_get(&self, p: &Value) -> Result<Value> {
        if p["keys"].as_array().is_some_and(|keys| {
            !keys.is_empty() && keys.iter().all(|k| k.as_str() == Some("backup"))
        }) {
            return Ok(json!({"values":{"backup":self.data()?.settings()?}}));
        }
        if let Some(keys) = p.get("keys") {
            if keys
                .as_array()
                .is_none_or(|keys| keys.iter().any(|key| key.as_str() != Some("vaultPath")))
            {
                return Err(AppError::new("E_PATH_INVALID", "当前只支持 vaultPath 设置"));
            }
        }
        let path = match &self.notes {
            Some(notes) => Some(notes.store.vault_path()),
            None => self
                .location
                .as_ref()
                .map(|location| location.configured_path())
                .transpose()?
                .flatten(),
        };
        Ok(json!({"values":{"vaultPath":path}}))
    }
    fn migrate(&mut self, p: &Value) -> Result<Value> {
        let values = p
            .get("values")
            .and_then(Value::as_object)
            .ok_or_else(|| AppError::new("E_PATH_INVALID", "values 必须是对象"))?;
        if values.len() != 1 || !values.contains_key("vaultPath") {
            return Err(AppError::new(
                "E_PATH_INVALID",
                "当前只实现 vaultPath 迁移，其他设置尚未实现",
            ));
        }
        let target = field(&p["values"], "vaultPath")?;
        let source = self.notes()?.store.vault_path();
        let location = self
            .location
            .as_ref()
            .ok_or_else(|| AppError::new("E_INTERNAL", "未配置库迁移适配器"))?;
        let copied = location.copy_verified(&source, target)?;
        let store = (self.factory)(&copied)?;
        store.list(&RelPath::root())?;
        let vault = store.vault_path();
        let id = content_hash(vault.as_bytes())[7..23].to_owned();
        let notes = self.assemble_notes(store, &id)?;
        let data = self
            .data_factory
            .as_ref()
            .map(|factory| factory(&vault, &id))
            .transpose()?;
        // Verify copied private state before committing the new configured path.
        notes.list(&json!({"scope":"drafts"}))?;
        notes.list(&json!({"scope":"renames"}))?;
        location.save_path(&copied)?;
        self.index = None;
        self.index = self
            .index_factory
            .as_ref()
            .map(|factory| factory(&vault, &id));
        self.notes = Some(notes);
        self.data = data;
        Ok(json!({"ok":true}))
    }
    fn handle(&mut self, command: &str, p: &Value) -> Result<Value> {
        match command {
            "settings_get" => return self.settings_get(p),
            "settings_set" => {
                if p["values"]
                    .as_object()
                    .is_some_and(|o| o.len() == 1 && o.contains_key("backup"))
                {
                    return self.data()?.configure(&p["values"]["backup"]);
                }
                return self.migrate(p);
            }
            "vault_open" => {
                let store = (self.factory)(field(p, "path")?)?;
                let vault = store.vault_path();
                let count = store.list(&RelPath::root())?.len();
                let id = content_hash(vault.as_bytes())[7..23].to_owned();
                let notes = self.assemble_notes(store, &id)?;
                let data = self
                    .data_factory
                    .as_ref()
                    .map(|factory| factory(&vault, &id))
                    .transpose()?;
                let same = self
                    .notes
                    .as_ref()
                    .is_some_and(|old| old.store.vault_path() == vault);
                let reused = self
                    .index
                    .as_ref()
                    .is_some_and(|index| same && index.refresh());
                if !reused {
                    self.index = None;
                    self.index = self
                        .index_factory
                        .as_ref()
                        .map(|factory| factory(&vault, &id));
                }
                let cleanup = notes.expire_trash(crate::index::now_ms());
                if let (Some(events), Ok(count)) = (&self.events, &cleanup) {
                    if *count > 0 {
                        events(
                            "note.changed",
                            json!({"kind":"trash.cleaned","count":count}),
                        );
                    }
                }
                self.notes = Some(notes);
                self.data = data;
                return Ok(
                    json!({"vaultId":id,"noteCount":count,"warnings":cleanup.err().map(|e|vec![format!("回收站清理未完成：{}",e.message)]).unwrap_or_default()}),
                );
            }
            "vault_info" => {
                let vault = self.notes()?.store.vault_path();
                return Ok(
                    json!({"vaultPath":vault,"indexState":self.index_status().state,"syncState":"disabled"}),
                );
            }
            "index_status" => {
                self.notes()?;
                return Ok(
                    serde_json::to_value(self.index_status()).expect("serializable index status")
                );
            }
            "search" => {
                self.notes()?;
                return self
                    .index
                    .as_ref()
                    .ok_or_else(|| crate::index::unavailable("未配置索引"))?
                    .search(p);
            }
            "index_rebuild" => {
                self.notes()?;
                if p.get("mode").and_then(Value::as_str) != Some("full") {
                    return Err(AppError::new("E_QUERY_UNSUPPORTED", "仅支持 mode=full"));
                }
                let id = self
                    .index
                    .as_ref()
                    .ok_or_else(|| crate::index::unavailable("未配置索引"))?
                    .rebuild()?;
                return Ok(json!({"jobId":id}));
            }
            "export_note" => return self.data()?.export(p),
            "sync_configure" => return self.data()?.sync_configure(p),
            "sync_status" => return self.data()?.sync_status(),
            "sync_now" => {
                if let Some(events) = &self.events {
                    events("sync.state", json!({"state":"pending"}));
                }
                let result = self.data()?.sync_now(p);
                if let Some(events) = &self.events {
                    match &result {
                        Ok(r) => {
                            events("sync.state", json!({"state":r["state"]}));
                            if r["conflicts"].as_array().is_some_and(|v| !v.is_empty()) {
                                events("conflict.detected", json!({"paths":r["conflicts"]}));
                            }
                        }
                        Err(e) => events("sync.state", json!({"state":"dirty","lastError":e.code})),
                    }
                }
                if let Ok(r) = &result {
                    if let Some(paths) = r["changedPaths"].as_array() {
                        for path in paths.iter().filter_map(Value::as_str) {
                            if let Some(index) = &self.index {
                                if let Ok(rel) = RelPath::new(path) {
                                    index.enqueue(&rel);
                                }
                            }
                            if let Some(events) = &self.events {
                                events(
                                    "note.changed",
                                    json!({"path":path,"source":"external","kind":"sync"}),
                                );
                            }
                        }
                    }
                }
                return result;
            }
            "backup_create" => {
                if p["scope"] == "note" {
                    return self.data()?.backup_create(p);
                }
                return self.with_index_paused(|service| service.backup_create(p));
            }
            "backup_restore" => {
                if p["verifyOnly"] == true {
                    return self.data()?.backup_restore(p);
                }
                let result = self.with_index_paused(|service| service.backup_restore(p));
                // Existing editor baselines intentionally remain stale: a following save must detect the restored disk hash.
                if result.is_ok() {
                    if let Some(events) = &self.events {
                        events(
                            "note.changed",
                            json!({"kind":"backup.restored","source":"local"}),
                        );
                    }
                }
                return result;
            }
            "note_list" if p["scope"] == "versions" => return self.data()?.versions(p),
            "note_read" if p.get("versionId").is_some() => {
                return Ok(
                    json!({"body":self.data()?.version_body(p)?,"versionId":p["versionId"],"path":p["path"]}),
                )
            }
            _ => {}
        }
        let version = if command == "note_restore" && p["kind"] == "version" {
            Some(self.data()?.version_body(p)?)
        } else {
            None
        };
        let mut effective = p.clone();
        if let Some(body) = version {
            effective["body"] = json!(body);
            effective["action"] = json!("save");
        }
        let p = &effective;
        let old_snapshot = if (command == "note_save"
            && (p["action"].is_null() || p["action"] == "save" || p["action"] == "metadata"))
            || (command == "note_restore" && p["kind"] == "version")
        {
            p["path"]
                .as_str()
                .and_then(|path| RelPath::new(path).ok())
                .and_then(|path| self.notes.as_ref()?.store.read(&path).ok())
                .filter(|s| !s.truncated)
                .map(|s| s.raw)
        } else {
            None
        };
        let now = crate::index::now_ms();
        let mut result = match command {
            "note_list" => self.notes()?.list(p),
            "note_read" => self.notes()?.read(p),
            "note_create" => self.notes()?.create(p, now),
            "note_save" => self.notes()?.save(p, now),
            "note_delete" => self.notes()?.delete(p, now),
            "note_restore" if p["kind"] == "version" => self.notes()?.save(p, now),
            "note_restore" => self.notes()?.restore(p, now),
            "note_rename" => self.notes()?.rename(p, now),
            "link_backlinks" | "link_mentions" | "link_graph" => {
                self.notes()?.link_query(command, p)
            }
            other => Err(AppError::new(
                "E_INTERNAL",
                if domain::COMMANDS.contains(&other) {
                    format!("{other} 尚未实现，基础工程仅注册接口名称")
                } else {
                    format!("未知命令 {other}")
                },
            )),
        };
        // A partial rename can commit some writes before returning an error. Drain
        // actual mutations independently of the command's success response.
        let changed = self
            .notes
            .as_mut()
            .map(NoteService::take_changes)
            .unwrap_or_default();
        let mut queued = !changed.is_empty();
        for path in &changed {
            let accepted = self.index.as_ref().is_some_and(|index| index.enqueue(path));
            queued &= accepted;
            if !accepted {
                if let Some(index) = &self.index {
                    index.invalidate();
                }
            }
            if let Some(events) = &self.events {
                events(
                    "note.changed",
                    json!({"path":path.as_str(),"kind":command,"source":"local","clientId":p["clientId"]}),
                );
            }
        }
        if let (Some(service), Ok(data)) = (&self.data, &mut result) {
            if data.get("snapshotCreated").is_some() {
                let attempt = (|| -> Result<Value> {
                    let path = data
                        .get("path")
                        .and_then(Value::as_str)
                        .or_else(|| p["path"].as_str())
                        .ok_or_else(|| AppError::new("E_INTERNAL", "保存未返回笔记路径"))?;
                    if let Some(old) = &old_snapshot {
                        service.snapshot(path, old, "")?;
                    }
                    let stored = self
                        .notes
                        .as_ref()
                        .ok_or_else(|| AppError::new("E_VAULT_NOT_FOUND", "未打开库"))?
                        .store
                        .read(&RelPath::new(path)?)?;
                    service.snapshot(path, &stored.raw, "")
                })();
                data["snapshotCreated"] = json!(attempt.is_ok());
                if let Some(warnings) = data.get_mut("warnings").and_then(Value::as_array_mut) {
                    warnings.retain(|w| w.as_str() != Some("版本快照尚未接入"));
                    if let Err(e) = attempt {
                        warnings.push(json!(format!("文件已保存，历史快照失败：{}", e.message)));
                    }
                }
            }
        }
        if let Ok(data) = &mut result {
            if data.get("indexQueued").is_some() {
                data["indexQueued"] = json!(queued);
                if !changed.is_empty() && !queued {
                    if let Some(warnings) = data.get_mut("warnings").and_then(Value::as_array_mut) {
                        warnings.push(json!("索引增量更新未入队，请稍后重新打开笔记库"));
                    }
                }
            }
        }
        result
    }
    fn index_status(&self) -> IndexStatus {
        self.index
            .as_ref()
            .map(|index| index.status())
            .unwrap_or_default()
    }
}
