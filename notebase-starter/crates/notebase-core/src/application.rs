use crate::domain::NOTE_SIZE_LIMIT as LIMIT;
use crate::domain::{self, field, AppError, NoteStore, Request, Response, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

type StoreFactory = Box<dyn Fn(&str) -> Result<Box<dyn NoteStore>> + Send + Sync>;
pub struct App {
    store: Option<Box<dyn NoteStore>>,
    known: HashMap<String, String>,
    factory: StoreFactory,
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
        }
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
                let count = store.list()?.len();
                let id = hash(store.vault_path().as_bytes())[7..23].to_string();
                self.store = Some(store);
                self.known.clear();
                Ok(json!({"vaultId":id,"noteCount":count}))
            }
            "vault_info" => Ok(
                json!({"vaultPath":self.store()?.vault_path(),"indexState":"unavailable","syncState":"disabled"}),
            ),
            "note_list" => {
                let prefix = p.get("dirPrefix").and_then(Value::as_str).unwrap_or("");
                domain::validate_rel(prefix, true)?;
                let limit = p
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(500)
                    .clamp(1, 500) as usize;
                let after = p.get("cursor").and_then(Value::as_str).unwrap_or("");
                let list = self.store()?.list()?;
                let items:Vec<Value>=list.into_iter().filter(|e|(prefix.is_empty()||e.path.starts_with(&format!("{prefix}/")))&&e.path.as_str()>after).map(|e|json!({"title":title(&e.path),"path":e.path,"mtimeMs":e.mtime_ms,"hasAttachment":false})).collect();
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
                let name = field(p, "title")?.trim();
                if name.contains('/') || name.is_empty() {
                    return Err(AppError::new(
                        "E_PATH_INVALID",
                        "标题不能为空且不能含路径分隔符",
                    ));
                }
                domain::validate_rel(name, false)?;
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
                    match self.store()?.create_new(&path) {
                        Ok(()) => return Ok(json!({"path":path})),
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                        Err(e) => return Err(AppError::io(e)),
                    }
                }
                Err(AppError::new("E_PATH_INVALID", "同名笔记过多"))
            }
            "note_read" => {
                let path = field(p, "path")?;
                let stored = self.store()?.read(path)?;
                let read_only = stored.raw.len() > LIMIT;
                if read_only {
                    return Err(AppError::new(
                        "E_FILE_TOO_LARGE",
                        "基础版尚未接入大文件只读预览，请用外部编辑器打开",
                    ));
                }
                let raw_text = std::str::from_utf8(&stored.raw).map_err(|_| {
                    AppError::new(
                        "E_ENCODING_INVALID",
                        "基础版仅读取 UTF-8 文件，原文件未修改",
                    )
                })?;
                let body = raw_text.strip_prefix('\u{feff}').unwrap_or(raw_text);
                let (frontmatter, frontmatter_error) = frontmatter(body);
                let content_hash = hash(&stored.raw);
                self.known.insert(path.into(), content_hash.clone());
                Ok(
                    json!({"path":path,"title":title(path),"body":body,"frontmatter":frontmatter,"frontmatterError":frontmatter_error,"contentHash":content_hash,"mtimeMs":stored.mtime_ms,"sizeBytes":stored.raw.len(),"encoding":"utf-8","lineEnding":if body.contains("\r\n"){"crlf"}else{"lf"},"readOnly":false}),
                )
            }
            "note_save" => {
                let path = field(p, "path")?;
                let body = field(p, "body")?;
                let base = field(p, "baseHash")?;
                let stored = self.store()?.read(path)?;
                if stored.raw.len() > LIMIT {
                    return Err(AppError::new(
                        "E_FILE_TOO_LARGE",
                        "文件超过 10 MB，不能通过基础版保存",
                    ));
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
                let old = std::str::from_utf8(&stored.raw)
                    .map_err(|_| AppError::new("E_ENCODING_INVALID", "源文件不是 UTF-8"))?;
                let normalized = body.replace("\r\n", "\n");
                let mut output = if old.contains("\r\n") {
                    normalized.replace('\n', "\r\n")
                } else {
                    normalized
                };
                if old.starts_with('\u{feff}') && !output.starts_with('\u{feff}') {
                    output.insert(0, '\u{feff}');
                }
                if output.len() > LIMIT {
                    return Err(AppError::new("E_FILE_TOO_LARGE", "保存内容超过 10 MB"));
                }
                self.store()?.write_atomic(path, output.as_bytes())?;
                let after = self.store()?.read(path)?;
                let content_hash = hash(&after.raw);
                self.known.insert(path.into(), content_hash.clone());
                Ok(
                    json!({"path":path,"contentHash":content_hash,"mtimeMs":after.mtime_ms,"bytesWritten":output.len(),"snapshotCreated":false,"indexQueued":false,"warnings":["索引队列与版本快照尚未接入"]}),
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
}
// 编辑器始终接收并保存完整原文，元数据解析不会重写 YAML。
fn frontmatter(body: &str) -> (Value, Option<String>) {
    let lf = body.replace("\r\n", "\n");
    if !lf.starts_with("---\n") {
        return (json!({}), None);
    }
    let rest = &lf[4..];
    if let Some(end) = rest.find("\n---") {
        let tail = &rest[end + 4..];
        if !(tail.is_empty() || tail.starts_with('\n')) {
            return (json!({}), Some("frontmatter 分隔符不合法".into()));
        }
        match serde_yaml::from_str::<Value>(&rest[..end]) {
            Ok(v) if v.is_object() => (v, None),
            Ok(_) => (json!({}), Some("frontmatter 必须是对象".into())),
            Err(e) => (json!({}), Some(e.to_string())),
        }
    } else {
        (json!({}), Some("frontmatter 缺少结束分隔符".into()))
    }
}
