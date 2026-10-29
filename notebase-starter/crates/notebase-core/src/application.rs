use crate::domain::{self, field, AppError, NoteStore, RelPath, Request, Response, Result};
use crate::index::{IndexSession, IndexStatus};
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
pub struct App {
    notes: Option<NoteService>,
    factory: StoreFactory,
    index_factory: Option<IndexFactory>,
    index: Option<Box<dyn IndexSession>>,
    support: Option<SupportFactory>,
    events: Option<NoteEvents>,
    location: Option<Box<dyn crate::migration::VaultLocation>>,
    settings: Option<Box<dyn crate::settings::SettingsStore>>,
}
impl App {
    pub fn new(factory: StoreFactory) -> Self {
        Self {
            notes: None,
            factory,
            index_factory: None,
            index: None,
            support: None,
            events: None,
            location: None,
            settings: None,
        }
    }
    pub fn with_index(mut self, factory: IndexFactory) -> Self {
        self.index_factory = Some(factory);
        self
    }
    pub fn with_location(mut self, location: Box<dyn crate::migration::VaultLocation>) -> Self {
        self.location = Some(location);
        self
    }
    pub fn with_settings(mut self, settings: Box<dyn crate::settings::SettingsStore>) -> Self {
        self.settings = Some(settings);
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
        if let Some(events) = &self.events {
            if let Err(e) = &result {
                if e.code == "E_EXTERNAL_MODIFIED" {
                    events(
                        "note.conflict",
                        json!({"path":r.payload["path"],"clientId":r.payload["clientId"],"detail":e.detail}),
                    );
                }
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
    fn handle(&mut self, c: &str, p: &Value) -> Result<Value> {
        if c == "settings_get" {
            let mut values = self
                .settings
                .as_ref()
                .map(|s| s.read())
                .transpose()?
                .unwrap_or_else(crate::settings::defaults);
            values["vaultPath"] = json!(if let Some(notes) = &self.notes {
                Some(notes.store.vault_path())
            } else {
                self.location
                    .as_ref()
                    .map(|l| l.configured_path())
                    .transpose()?
                    .flatten()
            });
            if let Some(keys) = p.get("keys") {
                let keys = keys
                    .as_array()
                    .ok_or_else(|| AppError::new("E_PATH_INVALID", "keys 必须是数组"))?;
                let mut selected = serde_json::Map::new();
                for key in keys {
                    let key = key
                        .as_str()
                        .ok_or_else(|| AppError::new("E_PATH_INVALID", "设置键必须是字符串"))?;
                    selected.insert(
                        key.to_owned(),
                        values.get(key).cloned().unwrap_or(Value::Null),
                    );
                }
                values = Value::Object(selected);
            }
            return Ok(json!({"values":values}));
        }
        if c == "settings_set" {
            let values = p
                .get("values")
                .and_then(Value::as_object)
                .ok_or_else(|| AppError::new("E_PATH_INVALID", "values 必须是对象"))?;
            if values.contains_key("vaultPath") {
                if values.len() != 1 {
                    return Err(AppError::new("E_PATH_INVALID", "目录迁移必须单独提交"));
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
                let (files, state) = self
                    .support
                    .as_ref()
                    .ok_or_else(|| AppError::new("E_INTERNAL", "未配置恢复存储"))?(
                    &vault, &id,
                )?;
                location.save_path(&copied)?;
                self.index = None;
                self.index = self.index_factory.as_ref().map(|f| f(&vault, &id));
                self.notes = Some(NoteService::new(store, files, state));
            } else {
                self.settings
                    .as_ref()
                    .ok_or_else(|| AppError::new("E_INTERNAL", "未配置设置存储"))?
                    .update(&p["values"])?;
            }
            return Ok(json!({"ok":true}));
        }
        if c == "vault_open" {
            let store = (self.factory)(field(p, "path")?)?;
            let vault = store.vault_path();
            let count = store.list(&RelPath::root())?.len();
            let id = content_hash(vault.as_bytes())[7..23].to_owned();
            let support = self
                .support
                .as_ref()
                .ok_or_else(|| AppError::new("E_INTERNAL", "笔记持久化端口尚未装配"))?;
            let (files, state) = support(&vault, &id)?;
            let same = self
                .notes
                .as_ref()
                .is_some_and(|n| n.store.vault_path() == vault);
            // Additional windows must retain existing per-client save baselines.
            if same {
                if !self.index.as_ref().is_some_and(|index| index.refresh()) {
                    self.index = None;
                    self.index = self
                        .index_factory
                        .as_ref()
                        .map(|factory| factory(&vault, &id));
                }
                return Ok(json!({"vaultId":id,"noteCount":count,"warnings":[]}));
            }
            let reused = self.index.as_ref().is_some_and(|idx| same && idx.refresh());
            if !reused {
                self.index = None;
                self.index = self.index_factory.as_ref().map(|f| f(&vault, &id));
            }
            let notes = NoteService::new(store, files, state);
            let trash_cleanup = notes.expire_trash(crate::index::now_ms());
            if let (Some(events), Ok(count)) = (&self.events, &trash_cleanup) {
                if *count > 0 {
                    events(
                        "note.changed",
                        json!({"kind":"trash.cleaned","count":count}),
                    );
                }
            }
            self.notes = Some(notes);
            return Ok(
                json!({"vaultId":id,"noteCount":count,"warnings":trash_cleanup.err().map(|e|vec![format!("回收站清理未完成：{}",e.message)]).unwrap_or_default()}),
            );
        }
        if c == "vault_info" {
            let path = self.notes()?.store.vault_path();
            return Ok(
                json!({"vaultPath":path,"indexState":self.index_status().state,"syncState":"disabled"}),
            );
        }
        if c == "index_status" {
            self.notes()?;
            return Ok(serde_json::to_value(self.index_status()).expect("index status serializes"));
        }
        if c == "search" {
            self.notes()?;
            return self
                .index
                .as_ref()
                .ok_or_else(|| crate::index::unavailable("未配置索引"))?
                .search(p);
        }
        if c == "index_rebuild" {
            self.notes()?;
            if p["mode"] != "full" {
                return Err(AppError::new("E_QUERY_UNSUPPORTED", "仅支持 mode=full"));
            }
            let job = self
                .index
                .as_ref()
                .ok_or_else(|| crate::index::unavailable("未配置索引"))?
                .rebuild()?;
            return Ok(json!({"jobId":job}));
        }
        let now = crate::index::now_ms();
        let mut result = match c {
            "note_list" => self.notes()?.list(p),
            "note_read" => self.notes()?.read(p),
            "note_create" => self.notes()?.create(p, now),
            "note_save" => self.notes()?.save(p, now),
            "note_delete" => self.notes()?.delete(p, now),
            "note_restore" => self.notes()?.restore(p, now),
            "note_rename" => self.notes()?.rename(p, now),
            "link_backlinks" | "link_mentions" | "link_graph" => self.notes()?.link_query(c, p),
            other => Err(AppError::new(
                "E_INTERNAL",
                if domain::COMMANDS.contains(&other) {
                    format!("{other} 尚未实现，基础工程仅注册接口名称")
                } else {
                    format!("未知命令 {other}")
                },
            )),
        };
        let writes = match c {
            "note_create" | "note_delete" | "note_restore" => true,
            "note_save" => !matches!(p["action"].as_str(), Some("draft" | "attachment")),
            "note_rename" => p["planOnly"].as_bool() == Some(false) || p.get("resumeId").is_some(),
            _ => false,
        };
        if writes
            && (result.is_ok()
                || result
                    .as_ref()
                    .err()
                    .is_some_and(|e| e.code == "E_PARTIAL_RENAME"))
        {
            let queued = if let Some(index) = &self.index {
                if c == "note_rename" {
                    // All referenced files may have changed, including a partial rename.
                    index.refresh()
                } else {
                    let path = result
                        .as_ref()
                        .ok()
                        .and_then(|v| v["path"].as_str())
                        .or_else(|| p["path"].as_str());
                    path.and_then(|path| RelPath::new(path).ok())
                        .is_some_and(|path| index.enqueue(&path))
                }
            } else {
                false
            };
            if let Ok(value) = &mut result {
                if value.get("indexQueued").is_some() {
                    value["indexQueued"] = json!(queued);
                    value["warnings"] = if queued {
                        json!(["版本快照尚未接入"])
                    } else {
                        json!(["索引队列未接收任务；版本快照尚未接入"])
                    };
                }
            }
            if let Some(events) = &self.events {
                events(
                    "note.changed",
                    json!({"path":result.as_ref().ok().and_then(|v|v["path"].as_str()).or_else(||p["path"].as_str()),"sourcePath":p["path"],"kind":c,"clientId":p["clientId"]}),
                );
            }
        }
        result
    }
    fn index_status(&self) -> IndexStatus {
        self.index.as_ref().map(|i| i.status()).unwrap_or_default()
    }
}
