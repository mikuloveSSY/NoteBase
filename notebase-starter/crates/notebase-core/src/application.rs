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
pub struct App {
    notes: Option<NoteService>,
    factory: StoreFactory,
    index_factory: Option<IndexFactory>,
    index: Option<Box<dyn IndexSession>>,
    support: Option<SupportFactory>,
    events: Option<NoteEvents>,
    location: Option<Box<dyn VaultLocation>>,
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
    pub fn with_location(mut self, location: Box<dyn VaultLocation>) -> Self {
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
        Ok(json!({"values":values}))
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
        Ok(json!({"ok":true}))
    }
    fn handle(&mut self, command: &str, p: &Value) -> Result<Value> {
        match command {
            "settings_get" => return self.settings_get(p),
            "settings_set" => {
                let values = p
                    .get("values")
                    .and_then(Value::as_object)
                    .ok_or_else(|| AppError::new("E_PATH_INVALID", "values 必须是对象"))?;
                if values.contains_key("vaultPath") {
                    return self.migrate(p);
                }
                self.settings
                    .as_ref()
                    .ok_or_else(|| AppError::new("E_INTERNAL", "未配置设置存储"))?
                    .update(&p["values"])?;
                return Ok(json!({"ok":true}));
            }
            "vault_open" => {
                let store = (self.factory)(field(p, "path")?)?;
                let vault = store.vault_path();
                let count = store.list(&RelPath::root())?.len();
                let id = content_hash(vault.as_bytes())[7..23].to_owned();

                let same = self
                    .notes
                    .as_ref()
                    .is_some_and(|old| old.store.vault_path() == vault);
                // Reopening a shared vault must retain every client's save baseline.
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
                let notes = self.assemble_notes(store, &id)?;
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
            _ => {}
        }
        let now = crate::index::now_ms();
        let mut result = match command {
            "note_list" => self.notes()?.list(p),
            "note_read" => self.notes()?.read(p),
            "note_create" => self.notes()?.create(p, now),
            "note_save" => self.notes()?.save(p, now),
            "note_delete" => self.notes()?.delete(p, now),
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
