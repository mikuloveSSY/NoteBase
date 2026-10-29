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
        }
    }
    pub fn with_index(mut self, factory: IndexFactory) -> Self {
        self.index_factory = Some(factory);
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
        let now = crate::index::now_ms();
        let result = match c {
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
            if let Some(index) = &self.index {
                index.invalidate();
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
