use crate::domain::NOTE_SIZE_LIMIT as LIMIT;
use crate::domain::{
    self, field, AppError, NoteStore, RelPath, Request, Response, Result, StoreError,
};
use crate::index::{IndexSession, IndexStatus};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

type StoreFactory = Box<dyn Fn(&str) -> Result<Box<dyn NoteStore>> + Send + Sync>;
pub type IndexFactory = Box<dyn Fn(&str, &str) -> Box<dyn IndexSession> + Send + Sync>;
pub struct App {
    store: Option<Box<dyn NoteStore>>,
    known: HashMap<String, String>,
    factory: StoreFactory,
    index_factory: Option<IndexFactory>,
    index: Option<Box<dyn IndexSession>>,
}
fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
fn title(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".md")
        .to_string()
}
impl App {
    pub fn new(factory: StoreFactory) -> Self {
        Self {
            store: None,
            known: HashMap::new(),
            factory,
            index_factory: None,
            index: None,
        }
    }
    pub fn with_index(mut self, factory: IndexFactory) -> Self {
        self.index_factory = Some(factory);
        self
    }
    pub fn dispatch(&mut self, r: Request) -> Response {
        match self.handle(&r.command, &r.payload) {
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
    fn store(&self) -> Result<&dyn NoteStore> {
        self.store
            .as_deref()
            .ok_or_else(|| AppError::new("E_VAULT_NOT_FOUND", "请先打开笔记库"))
    }
    fn handle(&mut self, command: &str, p: &Value) -> Result<Value> {
        match command {
            "vault_open" => {
                let store = (self.factory)(field(p, "path")?)?;
                let count = store.list(&RelPath::root())?.len();
                let id = hash(store.vault_path().as_bytes())[7..23].to_string();
                let same_vault = self
                    .store
                    .as_ref()
                    .is_some_and(|old| old.vault_path() == store.vault_path());
                let reused = self
                    .index
                    .as_ref()
                    .is_some_and(|index| same_vault && index.refresh());
                if !reused {
                    // 旧会话取消后不再发事件；后台任务始终绑定自己的库路径。
                    self.index = None;
                    self.index = self
                        .index_factory
                        .as_ref()
                        .map(|factory| factory(&store.vault_path(), &id));
                }
                self.store = Some(store);
                self.known.clear();
                Ok(json!({"vaultId":id,"noteCount":count}))
            }
            "vault_info" => Ok(
                json!({"vaultPath":self.store()?.vault_path(),"indexState":self.index_status().state,"syncState":"disabled"}),
            ),
            "index_status" => {
                self.store()?;
                Ok(serde_json::to_value(self.index_status()).expect("serializable index status"))
            }
            "note_list" => {
                let prefix = p.get("dirPrefix").and_then(Value::as_str).unwrap_or("");
                domain::validate_rel(prefix, true)?;
                let limit = p
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(500)
                    .clamp(1, 500) as usize;
                let after = p.get("cursor").and_then(Value::as_str).unwrap_or("");
                let root = if prefix.is_empty() {
                    RelPath::root()
                } else {
                    RelPath::new(prefix)?
                };
                let list = self.store()?.list(&root)?;
                let items:Vec<Value>=list.into_iter().filter(|e| e.path.as_str()>after).map(|e|json!({"title":title(&e.path),"path":e.path,"mtimeMs":e.mtime_ms,"hasAttachment":false})).collect();
                let next = if items.len() > limit {
                    items[limit - 1]["path"].clone()
                } else {
                    Value::Null
                };
                Ok(
                    json!({"items":items.into_iter().take(limit).collect::<Vec<_>>(),"nextCursor":next}),
                )
            }
            "note_create" => {
                let dir = p.get("dir").and_then(Value::as_str).unwrap_or("");
                domain::validate_rel(dir, true)?;
                let name = domain::sanitize_title(field(p, "title")?)?;
                for n in 0..10000 {
                    let filename = if n == 0 {
                        format!("{name}.md")
                    } else {
                        format!("{name} ({n}).md")
                    };
                    let path = if dir.is_empty() {
                        filename
                    } else {
                        format!("{dir}/{filename}")
                    };
                    match self.store()?.create_new(&RelPath::new(&path)?) {
                        Ok(()) => {
                            if let Some(index) = &self.index {
                                index.invalidate();
                            }
                            return Ok(json!({"path":path}));
                        }
                        Err(StoreError::AlreadyExists) => continue,
                        Err(e) => return Err(e.into()),
                    }
                }
                Err(AppError::new("E_PATH_INVALID", "同名笔记过多"))
            }
            "note_read" => {
                let path = field(p, "path")?;
                let stored = self.store()?.read(&RelPath::new(path)?)?;
                let mut preview = stored.raw.as_slice();
                // 预览截断点可能落在 UTF-8 字符内部，去掉尾部不完整字符，不误报编码错误。
                if stored.truncated {
                    if let Err(error) = std::str::from_utf8(preview) {
                        if error.error_len().is_none() {
                            preview = &preview[..error.valid_up_to()];
                        }
                    }
                }
                let valid_encoding = std::str::from_utf8(preview).is_ok();
                let raw_text = String::from_utf8_lossy(preview);
                let read_only = stored.truncated || !valid_encoding || stored.read_only;
                let body = raw_text
                    .strip_prefix('\u{feff}')
                    .unwrap_or(raw_text.as_ref());
                let (frontmatter, frontmatter_error) = domain::parse_frontmatter(body);
                // 截断预览没有全文件哈希；空值不能作为保存凭证。
                let content_hash = if stored.truncated {
                    String::new()
                } else {
                    hash(&stored.raw)
                };
                if read_only {
                    self.known.remove(path);
                } else {
                    self.known.insert(path.into(), content_hash.clone());
                }
                Ok(
                    json!({"path":path,"title":title(path),"body":body,"frontmatter":frontmatter,"frontmatterError":frontmatter_error,"contentHash":content_hash,"mtimeMs":stored.mtime_ms,"sizeBytes":stored.size_bytes,"encoding":if valid_encoding {"utf-8"} else {"unknown"},"lineEnding":if body.contains("\r\n"){"crlf"}else{"lf"},"readOnly":read_only}),
                )
            }
            "note_save" => {
                let path = field(p, "path")?;
                let body = field(p, "body")?;
                let base = field(p, "baseHash")?;
                let relative = RelPath::new(path)?;
                let stored = self.store()?.read(&relative)?;
                if stored.truncated || stored.size_bytes > LIMIT as u64 {
                    return Err(AppError::new(
                        "E_FILE_TOO_LARGE",
                        "文件超过 10 MB，不能通过基础版保存",
                    ));
                }
                let old = std::str::from_utf8(&stored.raw).map_err(|_| {
                    AppError::new(
                        "E_ENCODING_INVALID",
                        "源文件不是 UTF-8，只读预览不能直接保存",
                    )
                })?;
                if stored.read_only {
                    return Err(AppError::new("E_IO_PERMISSION", "文件是只读的，不能保存"));
                }
                let current = hash(&stored.raw);
                let known = self
                    .known
                    .get(path)
                    .ok_or_else(|| AppError::new("E_STALE_CLIENT", "请先读取笔记再保存"))?;
                if &current != known {
                    let mut e = AppError::new(
                        "E_EXTERNAL_MODIFIED",
                        "磁盘文件已修改，请重新打开并人工处理冲突",
                    );
                    e.detail = json!({"concurrent":base!=known});
                    return Err(e);
                }
                if base != known {
                    return Err(AppError::new(
                        "E_STALE_CLIENT",
                        "请求基于旧版本，请重新读取",
                    ));
                }
                let normalized = body.trim_start_matches('\u{feff}').replace("\r\n", "\n");
                let output = if old.contains("\r\n") {
                    normalized.replace('\n', "\r\n")
                } else {
                    normalized
                };
                if output.len() > LIMIT {
                    return Err(AppError::new("E_FILE_TOO_LARGE", "保存内容超过 10 MB"));
                }
                let written = self.store()?.write_atomic(&relative, output.as_bytes())?;
                self.known.insert(path.into(), written.content_hash.clone());
                if let Some(index) = &self.index {
                    index.invalidate();
                }
                Ok(
                    json!({"path":path,"contentHash":written.content_hash,"mtimeMs":written.mtime_ms,"bytesWritten":written.bytes_written,"snapshotCreated":false,"indexQueued":false,"warnings":["索引队列与版本快照尚未接入"]}),
                )
            }
            other => Err(AppError::new(
                "E_INTERNAL",
                if domain::COMMANDS.contains(&other) {
                    format!("{other} 尚未实现，基础工程仅注册接口名称")
                } else {
                    format!("未知命令 {other}")
                },
            )),
        }
    }
    fn index_status(&self) -> IndexStatus {
        self.index
            .as_ref()
            .map(|index| index.status())
            .unwrap_or_default()
    }
}
