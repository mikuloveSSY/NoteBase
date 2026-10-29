//! Member-two note use cases. Filesystem and private state are injected ports.
use crate::domain::{self, AppError, NoteStore, RelPath, Result, StoreError, NOTE_SIZE_LIMIT};
use crate::links::{self, LinkNote};
use crate::note_files::{original_from_trash, NoteFiles};
use crate::note_state::NoteStateStore;
use crate::organization;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};

pub fn content_hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
fn key(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn string<'a>(p: &'a Value, name: &str) -> Result<&'a str> {
    domain::field(p, name)
}
fn client(p: &Value) -> &str {
    p.get("clientId")
        .and_then(Value::as_str)
        .unwrap_or("legacy")
}
fn seq(p: &Value) -> u64 {
    p.get("clientSeq").and_then(Value::as_u64).unwrap_or(0)
}
fn fallback_title(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map(|v| v.0).unwrap_or(name).into()
}
pub struct NoteService {
    pub store: Box<dyn NoteStore>,
    files: Box<dyn NoteFiles>,
    state: Box<dyn NoteStateStore>,
    known: HashMap<String, String>,
    saved_seq: HashMap<String, u64>,
    paused: HashSet<String>,
    changed_paths: BTreeSet<String>,
}
impl NoteService {
    pub fn new(
        store: Box<dyn NoteStore>,
        files: Box<dyn NoteFiles>,
        state: Box<dyn NoteStateStore>,
    ) -> Self {
        Self {
            store,
            files,
            state,
            known: HashMap::new(),
            saved_seq: HashMap::new(),
            paused: HashSet::new(),
            changed_paths: BTreeSet::new(),
        }
    }
    /// Drain paths whose authoritative files actually changed, including partial failures.
    pub fn take_changes(&mut self) -> Vec<RelPath> {
        std::mem::take(&mut self.changed_paths)
            .into_iter()
            .map(|path| RelPath::new(&path).expect("only validated note paths enter mutation set"))
            .collect()
    }
    pub fn expire_trash(&self, now: u64) -> Result<usize> {
        let mut cleaned = 0;
        for entry in self.files.trash_entries()? {
            if now.saturating_sub(entry.deleted_at_ms) > 30 * 24 * 60 * 60 * 1000
                && now > entry.deleted_at_ms
            {
                self.store.delete(&RelPath::new(&entry.trash_path)?)?;
                cleaned += 1;
            }
        }
        Ok(cleaned)
    }
    fn identity(path: &str, p: &Value) -> String {
        format!("{}\0{}", client(p), path)
    }
    fn ordinary(path: &str) -> Result<RelPath> {
        let p = RelPath::new(path)?;
        if path.starts_with("trash/")
            || path.starts_with("attachments/")
            || path.starts_with(".git/")
        {
            return Err(AppError::new(
                "E_PATH_INVALID",
                "该操作不接受系统目录中的笔记",
            ));
        }
        Ok(p)
    }
    fn writable_text(&self, path: &RelPath) -> Result<(String, String, u64)> {
        let s = self.store.read(path)?;
        if s.truncated || s.size_bytes > NOTE_SIZE_LIMIT as u64 {
            return Err(AppError::new(
                "E_FILE_TOO_LARGE",
                "超过10 MiB的笔记只能只读预览",
            ));
        }
        if s.read_only {
            return Err(AppError::new("E_IO_PERMISSION", "笔记是只读文件"));
        }
        let text = std::str::from_utf8(&s.raw)
            .map_err(|_| AppError::new("E_ENCODING_INVALID", "非UTF-8文件不可直接改写"))?
            .to_owned();
        Ok((text, content_hash(&s.raw), s.mtime_ms))
    }
    pub fn read(&mut self, p: &Value) -> Result<Value> {
        let path = string(p, "path")?;
        let s = self.store.read(&Self::ordinary(path)?)?;
        let mut bytes = s.raw.as_slice();
        if s.truncated {
            if let Err(e) = std::str::from_utf8(bytes) {
                if e.error_len().is_none() {
                    bytes = &bytes[..e.valid_up_to()];
                }
            }
        }
        let valid = std::str::from_utf8(bytes).is_ok();
        let raw = String::from_utf8_lossy(bytes);
        let body = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
        let meta = organization::inspect(body);
        let hash = if s.truncated {
            String::new()
        } else {
            content_hash(&s.raw)
        };
        let read_only = s.truncated || !valid || s.read_only;
        let id = Self::identity(path, p);
        if !p.get("probe").and_then(Value::as_bool).unwrap_or(false) {
            if read_only {
                self.known.remove(&id);
            } else {
                self.known.insert(id, hash.clone());
            }
        }
        let title = meta
            .fields
            .get("title")
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| fallback_title(path));
        Ok(
            json!({"path":path,"title":title,"body":body,"frontmatter":meta.fields,"frontmatterError":meta.error,"contentHash":hash,"mtimeMs":s.mtime_ms,"sizeBytes":s.size_bytes,"encoding":if valid{"utf-8"}else{"unknown"},"lineEnding":if body.contains("\r\n"){"crlf"}else{"lf"},"readOnly":read_only,"conflictPaused":self.paused.contains(path),"links":links::parse(body)}),
        )
    }
    fn normalize(body: &str, old: &str) -> String {
        let b = body.trim_start_matches('\u{feff}').replace("\r\n", "\n");
        if old.contains("\r\n") {
            b.replace('\n', "\r\n")
        } else {
            b
        }
    }
    fn check_base(&mut self, path: &str, p: &Value, current: &str) -> Result<()> {
        let id = Self::identity(path, p);
        let base = string(p, "baseHash")?;
        let known = self
            .known
            .get(&id)
            .ok_or_else(|| AppError::new("E_STALE_CLIENT", "请先读取笔记"))?;
        if current != known {
            self.paused.insert(path.into());
            let mut e = AppError::new("E_EXTERNAL_MODIFIED", "检测到磁盘修改，自动保存已暂停");
            e.detail = json!({"concurrent":base!=known,"diskHash":current,"choices":["local","disk","copy"]});
            return Err(e);
        }
        if base != known {
            return Err(AppError::new(
                "E_STALE_CLIENT",
                "请求基于旧版本，请重新读取",
            ));
        }
        if p.get("clientId").is_some()
            && seq(p) > 0
            && self.saved_seq.get(&id).is_some_and(|n| seq(p) <= *n)
        {
            return Err(AppError::new("E_STALE_CLIENT", "保存序号过期"));
        }
        Ok(())
    }
    pub fn save(&mut self, p: &Value, now: u64) -> Result<Value> {
        let action = p.get("action").and_then(Value::as_str).unwrap_or("save");
        match action {
            "draft" => return self.draft(p, now),
            "conflict" => return self.resolve_conflict(p, now),
            "attachment" => return self.attachment(p, now),
            "save" | "metadata" => {}
            _ => return Err(AppError::new("E_INTERNAL", "未知笔记保存操作")),
        }
        let path = string(p, "path")?;
        let rp = Self::ordinary(path)?;
        let (old, hash, mtime) = self.writable_text(&rp)?;
        self.check_base(path, p, &hash)?;
        if p.get("expectedMtimeMs")
            .and_then(Value::as_u64)
            .is_some_and(|expected| expected != mtime)
        {
            return Err(AppError::new(
                "E_STALE_CLIENT",
                "文件时间版本已变化，请重新读取",
            ));
        }
        if self.paused.contains(path) {
            let mut e = AppError::new("E_EXTERNAL_MODIFIED", "请显式处理冲突后再保存");
            e.detail = json!({"diskHash":hash,"choices":["local","disk","copy"]});
            return Err(e);
        }
        let body = if action == "metadata" {
            let updates = p
                .get("updates")
                .and_then(Value::as_object)
                .ok_or_else(|| AppError::new("E_INTERNAL", "属性更新必须为对象"))?;
            organization::patch(&old, updates)?
        } else {
            string(p, "body")?.to_owned()
        };
        self.commit(path, p, &old, &body)
    }
    fn commit(&mut self, path: &str, p: &Value, old: &str, body: &str) -> Result<Value> {
        let output = Self::normalize(body, old);
        if output.len() > NOTE_SIZE_LIMIT {
            return Err(AppError::new("E_FILE_TOO_LARGE", "保存内容超过10 MiB"));
        }
        let written = self
            .store
            .write_atomic(&RelPath::new(path)?, output.as_bytes())?;
        self.changed_paths.insert(path.to_owned());
        let id = Self::identity(path, p);
        self.known.insert(id.clone(), written.content_hash.clone());
        if seq(p) > 0 {
            self.saved_seq.insert(id.clone(), seq(p));
        }
        let mut warnings = vec!["版本快照尚未接入".to_owned()];
        let draft_key = key(&id);
        match self.state.read("drafts", &draft_key) {
            Ok(Some(d))
                if d["clientSeq"].as_u64().unwrap_or(0) <= seq(p)
                    || p.get("clientId").is_none() =>
            {
                if let Err(e) = self.state.remove("drafts", &draft_key) {
                    warnings.push(format!("文件已保存但草稿清理失败：{}", e.message));
                }
            }
            Err(e) => warnings.push(format!("草稿状态检查失败：{}", e.message)),
            _ => {}
        }
        Ok(
            json!({"path":path,"contentHash":written.content_hash,"mtimeMs":written.mtime_ms,"bytesWritten":written.bytes_written,"snapshotCreated":false,"indexQueued":false,"warnings":warnings,"clientSeq":seq(p)}),
        )
    }
    fn draft(&mut self, p: &Value, now: u64) -> Result<Value> {
        let path = string(p, "path")?;
        Self::ordinary(path)?;
        let body = string(p, "body")?;
        if body.len() > NOTE_SIZE_LIMIT {
            return Err(AppError::new("E_FILE_TOO_LARGE", "草稿超过10 MiB"));
        }
        let id = Self::identity(path, p);
        let draft_key = key(&id);
        if self
            .saved_seq
            .get(&id)
            .is_some_and(|n| seq(p) > 0 && seq(p) <= *n)
        {
            return Ok(json!({"draftSaved":false,"superseded":true}));
        }
        let known = self
            .known
            .get(&id)
            .ok_or_else(|| AppError::new("E_STALE_CLIENT", "请先读取笔记"))?;
        if string(p, "baseHash")? != known {
            return Err(AppError::new("E_STALE_CLIENT", "草稿基于旧版本"));
        }
        if let Some(prev) = self.state.read("drafts", &draft_key)? {
            if prev["clientSeq"].as_u64().unwrap_or(0) > seq(p) {
                return Err(AppError::new("E_STALE_CLIENT", "草稿序号过期"));
            }
        }
        self.state.write("drafts",&draft_key,&json!({"id":draft_key,"path":path,"body":body,"baseHash":known,"clientId":client(p),"clientSeq":seq(p),"savedAtMs":now}))?;
        Ok(json!({"draftSaved":true,"draftId":draft_key,"clientSeq":seq(p)}))
    }
    fn resolve_conflict(&mut self, p: &Value, now: u64) -> Result<Value> {
        let path = string(p, "path")?;
        let rp = Self::ordinary(path)?;
        let (old, current, _) = self.writable_text(&rp)?;
        if p.get("diskHash").and_then(Value::as_str) != Some(&current) {
            let mut e = AppError::new("E_EXTERNAL_MODIFIED", "磁盘再次变化，请重新确认");
            e.detail = json!({"diskHash":current});
            return Err(e);
        }
        match string(p, "choice")? {
            "disk" => {
                self.paused.remove(path);
                self.known.insert(Self::identity(path, p), current);
                self.state
                    .remove("drafts", &key(&Self::identity(path, p)))?;
                self.read(p)
            }
            "local" => {
                let body = string(p, "body")?;
                let r = self.commit(path, p, &old, body)?;
                self.paused.remove(path);
                Ok(r)
            }
            "copy" => {
                let new = self.create_copy(path, string(p, "body")?, "conflict", now)?;
                self.paused.remove(path);
                self.state
                    .remove("drafts", &key(&Self::identity(path, p)))?;
                Ok(json!({"path":new,"copied":true}))
            }
            _ => Err(AppError::new("E_INTERNAL", "未知冲突处理选择")),
        }
    }
    fn vacant_path(&self, dir: &str, name: &str) -> Result<String> {
        for n in 0..10000 {
            let suffix = if n == 0 {
                String::new()
            } else {
                format!(" ({})", n + 1)
            };
            let file = format!("{name}{suffix}.md");
            let path = if dir.is_empty() {
                file
            } else {
                format!("{dir}/{file}")
            };
            match self.store.stat(&RelPath::new(&path)?) {
                Err(StoreError::Failure(e)) if e.code == "E_NOT_FOUND" => return Ok(path),
                Ok(_) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Err(AppError::new("E_PATH_INVALID", "同名笔记过多"))
    }
    pub fn create(&mut self, p: &Value, now: u64) -> Result<Value> {
        let dir = p.get("dir").and_then(Value::as_str).unwrap_or("");
        domain::validate_rel(dir, true)?;
        if dir == "trash"
            || dir.starts_with("trash/")
            || dir == "attachments"
            || dir.starts_with("attachments/")
        {
            return Err(AppError::new("E_PATH_INVALID", "不能在系统目录创建笔记"));
        }
        let title = string(p, "title")?;
        let name = domain::sanitize_title(title)?;
        let stamp = timestamp(now);
        let mut body = p
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if let Some(t) = p.get("templatePath").and_then(Value::as_str) {
            if !t.starts_with("templates/") {
                return Err(AppError::new("E_PATH_INVALID", "模板必须来自templates目录"));
            }
            let template = self.store.read(&RelPath::new(t)?)?;
            if template.truncated {
                return Err(AppError::new("E_FILE_TOO_LARGE", "模板超过大小上限"));
            }
            let raw = std::str::from_utf8(&template.raw)
                .map_err(|_| AppError::new("E_ENCODING_INVALID", "模板不是UTF-8"))?;
            body = organization::render_template(raw, title, &stamp[..10], &stamp[11..19]);
        }
        if body.len() > NOTE_SIZE_LIMIT {
            return Err(AppError::new("E_FILE_TOO_LARGE", "模板内容超过10 MiB"));
        }
        for _ in 0..10000 {
            let path = self.vacant_path(dir, &name)?;
            let rp = RelPath::new(&path)?;
            self.files.prepare_parent(&rp)?;
            match self.files.create_note(&rp, body.as_bytes()) {
                Ok(()) => {
                    self.changed_paths.insert(path.clone());
                    return Ok(json!({"path":path}));
                }
                Err(StoreError::AlreadyExists) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(AppError::new("E_PATH_INVALID", "同名笔记过多"))
    }
    fn create_copy(&mut self, path: &str, body: &str, label: &str, now: u64) -> Result<String> {
        let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
        let title = format!("{} ({label} {})", fallback_title(file), compact_stamp(now));
        let r = self.create(&json!({"dir":dir,"title":title,"body":body}), now)?;
        Ok(r["path"].as_str().unwrap().into())
    }
    pub fn list(&self, p: &Value) -> Result<Value> {
        let scope = p.get("scope").and_then(Value::as_str).unwrap_or("notes");
        if scope == "drafts" || scope == "renames" {
            return Ok(
                json!({"items":self.state.list(if scope=="drafts"{"drafts"}else{"renames"})?,"nextCursor":null}),
            );
        }
        if scope == "trash" {
            return Ok(json!({"items":self.files.trash_entries()?,"nextCursor":null}));
        }
        if scope != "notes" && scope != "templates" {
            return Err(AppError::new("E_INTERNAL", "未知列表范围"));
        }
        let prefix = if scope == "templates" {
            "templates"
        } else {
            p.get("dirPrefix").and_then(Value::as_str).unwrap_or("")
        };
        domain::validate_rel(prefix, true)?;
        let root = if prefix.is_empty() {
            RelPath::root()
        } else {
            RelPath::new(prefix)?
        };
        let entries = match self.store.list(&root) {
            Ok(v) => v,
            Err(StoreError::Failure(e)) if e.code == "E_NOT_FOUND" && scope == "templates" => {
                vec![]
            }
            Err(e) => return Err(e.into()),
        };
        let after = p.get("cursor").and_then(Value::as_str).unwrap_or("");
        let limit = p
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(500)
            .clamp(1, 500) as usize;
        let mut items = vec![];
        for e in entries {
            if e.path.as_str() <= after || (scope == "notes" && e.path.starts_with("templates/")) {
                continue;
            }
            let stored = self.store.read(&RelPath::new(&e.path)?)?;
            let text = String::from_utf8_lossy(&stored.raw);
            let meta = organization::inspect(&text);
            let fields = &meta.fields;
            let tags: Vec<String> = fields["tags"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            if let Some(tag) = p.get("tag").and_then(Value::as_str) {
                if !organization::tag_matches(&tags, tag) {
                    continue;
                }
            }
            if p.get("favorite").and_then(Value::as_bool) == Some(true)
                && fields["favorite"] != true
            {
                continue;
            }
            items.push(json!({"path":e.path,"title":fields["title"].as_str().unwrap_or(&fallback_title(&e.path)),"mtimeMs":e.mtime_ms,"hasAttachment":text.contains("attachments/"),"tags":tags,"favorite":fields["favorite"].as_bool().unwrap_or(false),"pinned":fields["pinned"].as_bool().unwrap_or(false)}));
        }
        let next = if items.len() > limit {
            items[limit - 1]["path"].clone()
        } else {
            Value::Null
        };
        items.truncate(limit);
        Ok(json!({"items":items,"nextCursor":next}))
    }
    pub fn delete(&mut self, p: &Value, now: u64) -> Result<Value> {
        let path = string(p, "path")?;
        let rp = RelPath::new(path)?;
        if p.get("permanent").and_then(Value::as_bool) == Some(true) {
            if p.get("confirmed").and_then(Value::as_bool) != Some(true) {
                return Err(AppError::new("E_CANCELLED", "彻底删除需要二次确认"));
            }
            self.store.delete(&rp)?;
            self.changed_paths.insert(path.to_owned());
            return Ok(json!({"deleted":true,"restorable":false}));
        }
        Self::ordinary(path)?;
        let base = path
            .strip_suffix(".md")
            .or_else(|| path.strip_suffix(".MD"))
            .ok_or_else(|| AppError::new("E_PATH_INVALID", "笔记必须为.md"))?;
        let stamp = compact_stamp(now);
        for n in 0..10000 {
            let suffix = if n == 0 {
                String::new()
            } else {
                format!(" ({})", n + 1)
            };
            let trash = format!("trash/{base} (deleted {stamp}){suffix}.md");
            let tp = RelPath::new(&trash)?;
            self.files.prepare_parent(&tp)?;
            match self.store.rename(&rp, &tp) {
                Ok(()) => {
                    self.changed_paths.insert(path.to_owned());
                    self.known
                        .retain(|id, _| !id.ends_with(&format!("\0{path}")));
                    return Ok(json!({"trashPath":trash,"restorable":true}));
                }
                Err(StoreError::AlreadyExists) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(AppError::new("E_PATH_INVALID", "回收站重名过多"))
    }
    pub fn restore(&mut self, p: &Value, now: u64) -> Result<Value> {
        if p.get("kind").and_then(Value::as_str) == Some("draft") {
            return self.restore_draft(p, now);
        }
        let trash = string(p, "trashPath")?;
        let original = original_from_trash(trash)
            .ok_or_else(|| AppError::new("E_PATH_INVALID", "不是可恢复的回收站路径"))?;
        if !self
            .files
            .trash_entries()?
            .iter()
            .any(|t| t.trash_path == trash)
        {
            return Err(AppError::new("E_NOT_FOUND", "回收记录不存在"));
        }
        let (dir, file) = original.rsplit_once('/').unwrap_or(("", &original));
        for _ in 0..10000 {
            let target = self.vacant_path(dir, &fallback_title(file))?;
            let rp = RelPath::new(&target)?;
            self.files.prepare_parent(&rp)?;
            match self.store.rename(&RelPath::new(trash)?, &rp) {
                Ok(()) => {
                    self.changed_paths.insert(target.clone());
                    return Ok(json!({"path":target}));
                }
                Err(StoreError::AlreadyExists) => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(AppError::new("E_PATH_INVALID", "恢复路径重名过多"))
    }
    fn restore_draft(&mut self, p: &Value, now: u64) -> Result<Value> {
        let id = string(p, "draftId")?;
        let d = self
            .state
            .read("drafts", id)?
            .ok_or_else(|| AppError::new("E_NOT_FOUND", "草稿不存在"))?;
        let path = string(&d, "path")?;
        Self::ordinary(path)?;
        if p.get("choice").and_then(Value::as_str) == Some("discard") {
            self.state.remove("drafts", id)?;
            return Ok(json!({"discarded":true}));
        }
        let body = string(&d, "body")?;
        let target = match self.writable_text(&RelPath::new(path)?) {
            Ok((old, hash, _))
                if d["baseHash"].as_str() == Some(&hash)
                    && p.get("choice").and_then(Value::as_str) != Some("copy") =>
            {
                self.store
                    .write_atomic(&RelPath::new(path)?, Self::normalize(body, &old).as_bytes())?;
                self.changed_paths.insert(path.to_owned());
                path.to_owned()
            }
            Ok(_) => self.create_copy(path, body, "recovered", now)?,
            Err(e) if e.code == "E_NOT_FOUND" => self.create_copy(path, body, "recovered", now)?,
            Err(e) => return Err(e),
        };
        self.state.remove("drafts", id)?;
        Ok(json!({"path":target,"recovered":true,"copied":target!=path}))
    }
    fn attachment(&self, p: &Value, now: u64) -> Result<Value> {
        let path = string(p, "path")?;
        self.writable_text(&Self::ordinary(path)?)?;
        let name = string(p, "name")?;
        let values = p
            .get("bytes")
            .and_then(Value::as_array)
            .ok_or_else(|| AppError::new("E_INTERNAL", "附件数据必须是字节数组"))?;
        if values.len() > 32 * 1024 * 1024 {
            return Err(AppError::new("E_FILE_TOO_LARGE", "附件上传超过32 MiB"));
        }
        let bytes: Result<Vec<u8>> = values
            .iter()
            .map(|v| {
                v.as_u64()
                    .filter(|n| *n < 256)
                    .map(|n| n as u8)
                    .ok_or_else(|| AppError::new("E_INTERNAL", "非法附件字节"))
            })
            .collect();
        let directory = p
            .get("directory")
            .and_then(Value::as_str)
            .unwrap_or("attachments");
        let target = self
            .files
            .add_attachment(name, &bytes?, directory, &compact_stamp(now))?;
        let source_dir = path.rsplit_once('/').map(|v| v.0).unwrap_or("");
        let relative = relative_path(source_dir, &target);
        let escaped = relative
            .replace(' ', "%20")
            .replace('(', "%28")
            .replace(')', "%29")
            .replace('#', "%23");
        let alt = name.replace(['[', ']'], "_");
        let image = p.get("image").and_then(Value::as_bool).unwrap_or(false);
        Ok(
            json!({"attachmentPath":target,"relativePath":relative,"markdown":format!("{}[{alt}]({escaped})",if image{"!"}else{""})}),
        )
    }
    pub fn documents(&self) -> Result<Vec<LinkNote>> {
        let mut docs = vec![];
        for e in self.store.list(&RelPath::root())? {
            if e.path.starts_with("templates/") {
                continue;
            }
            let s = self.store.read(&RelPath::new(&e.path)?)?;
            if s.truncated {
                return Err(AppError::new(
                    "E_FILE_TOO_LARGE",
                    "链接扫描包含超大笔记，请缩小库范围",
                ));
            }
            let body = String::from_utf8(s.raw).map_err(|_| {
                AppError::new(
                    "E_ENCODING_INVALID",
                    "链接扫描包含非法编码，不能声称引用完整",
                )
            })?;
            let m = organization::inspect(&body);
            let title = m.fields["title"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| fallback_title(&e.path));
            let aliases = m.fields["aliases"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            docs.push(LinkNote {
                path: e.path,
                title,
                aliases,
                body,
            });
        }
        Ok(docs)
    }
    pub fn link_query(&self, command: &str, p: &Value) -> Result<Value> {
        let docs = self.documents()?;
        if let Some(target) = p.get("resolveTarget").and_then(Value::as_str) {
            return serde_json::to_value(links::resolve(string(p, "path")?, target, &docs))
                .map_err(|e| AppError::new("E_INTERNAL", e.to_string()));
        }
        if command == "link_graph" {
            let prefix = p.get("pathPrefix").and_then(Value::as_str).unwrap_or("");
            let docs: Vec<_> = docs
                .into_iter()
                .filter(|d| d.path.starts_with(prefix))
                .collect();
            return Ok(links::graph(
                p.get("path").and_then(Value::as_str),
                p.get("maxNodes")
                    .and_then(Value::as_u64)
                    .unwrap_or(500)
                    .clamp(1, 500) as usize,
                &docs,
            ));
        }
        let path = string(p, "path")?;
        if !docs.iter().any(|d| d.path == path) {
            return Err(AppError::new("E_NOT_FOUND", "链接目标笔记不存在"));
        }
        Ok(if command == "link_backlinks" {
            links::backlinks(path, &docs)
        } else {
            links::mentions(path, &docs)
        })
    }
    pub fn rename(&mut self, p: &Value, now: u64) -> Result<Value> {
        if let Some(id) = p.get("resumeId").and_then(Value::as_str) {
            return self.resume_rename(id);
        }
        let from = string(p, "path")?;
        Self::ordinary(from)?;
        let (dir, _) = from.rsplit_once('/').unwrap_or(("", from));
        let name = domain::sanitize_title(string(p, "newName")?)?;
        let requested = if dir.is_empty() {
            format!("{name}.md")
        } else {
            format!("{dir}/{name}.md")
        };
        let to = if requested == from {
            from.to_owned()
        } else {
            self.vacant_path(dir, &name)?
        };
        let docs = self.documents()?;
        let source = docs
            .iter()
            .find(|d| d.path == from)
            .ok_or_else(|| AppError::new("E_NOT_FOUND", "待重命名笔记不存在"))?;
        let source_hash = content_hash(source.body.as_bytes());
        let mut edits = vec![];
        for d in &docs {
            let new = links::rename_body(d, from, &to, &docs);
            if new != d.body {
                edits.push(json!({"path":d.path,"beforeHash":content_hash(d.body.as_bytes()),"afterHash":content_hash(new.as_bytes()),"body":new}));
            }
        }
        let mut journal = json!({"from":from,"to":to,"sourceHash":source_hash,"edits":edits,"createdMs":now,"moved":false,"done":[],"state":"running"});
        let id = key(&serde_json::to_string(
            &json!({"from":from,"to":to,"sourceHash":source_hash,"edits":edits}),
        )
        .unwrap());
        journal["id"] = json!(id);
        if p.get("planOnly").and_then(Value::as_bool).unwrap_or(true) {
            return Ok(
                json!({"plan":{"id":id,"from":from,"to":to,"affectedFiles":edits.iter().map(|e|json!({"path":e["path"],"preview":e["body"].as_str().unwrap_or("").chars().take(120).collect::<String>()})).collect::<Vec<_>>(),"totalReplacements":edits.len()}}),
            );
        }
        if p.get("planId").and_then(Value::as_str) != Some(&id) {
            return Err(AppError::new(
                "E_STALE_CLIENT",
                "重命名预演已过期，请重新预演并确认",
            ));
        }
        if from == to {
            return Ok(json!({"renamed":false,"path":from,"updatedFiles":0,"failedFiles":[]}));
        }
        self.state.write("renames", &id, &journal)?;
        self.resume_rename(&id)
    }
    fn resume_rename(&mut self, id: &str) -> Result<Value> {
        let mut j = self
            .state
            .read("renames", id)?
            .ok_or_else(|| AppError::new("E_NOT_FOUND", "重命名恢复记录不存在"))?;
        validate_rename_journal(&j, id)?;
        let from = string(&j, "from")?.to_owned();
        let to = string(&j, "to")?.to_owned();
        let result = (|| -> Result<usize> {
            let source = RelPath::new(&from)?;
            let target = RelPath::new(&to)?;
            if j["moved"] != true {
                match self.store.read(&source) {
                    Ok(s) => {
                        if content_hash(&s.raw) != j["sourceHash"].as_str().unwrap_or("") {
                            return Err(AppError::new(
                                "E_EXTERNAL_MODIFIED",
                                "源文件在预演后被修改",
                            ));
                        }
                        self.files.prepare_parent(&target)?;
                        self.store.rename(&source, &target)?;
                        self.changed_paths.insert(from.clone());
                        self.changed_paths.insert(to.clone());
                    }
                    Err(StoreError::Failure(e)) if e.code == "E_NOT_FOUND" => {
                        let s = self.store.read(&target)?;
                        let h = content_hash(&s.raw);
                        let accepted = j["edits"]
                            .as_array()
                            .and_then(|a| a.iter().find(|e| e["path"] == from))
                            .and_then(|e| e["afterHash"].as_str());
                        if h != j["sourceHash"].as_str().unwrap_or("")
                            && Some(h.as_str()) != accepted
                        {
                            return Err(AppError::new(
                                "E_EXTERNAL_MODIFIED",
                                "恢复目标与日志哈希不一致",
                            ));
                        }
                    }
                    Err(e) => return Err(e.into()),
                }
                self.changed_paths.insert(from.clone());
                self.changed_paths.insert(to.clone());
                j["moved"] = json!(true);
                self.state.write("renames", id, &j)?;
            }
            if self.store.stat(&source).is_ok() {
                return Err(AppError::new(
                    "E_EXTERNAL_MODIFIED",
                    "原路径在重命名后重新出现，请人工检查",
                ));
            }
            let current_target = self.store.read(&target)?;
            let target_hash = content_hash(&current_target.raw);
            let rewritten_source = j["edits"]
                .as_array()
                .and_then(|a| a.iter().find(|e| e["path"] == from))
                .and_then(|e| e["afterHash"].as_str());
            let source_pending = j["edits"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["path"] == from)
                && !j["done"].as_array().unwrap().contains(&json!(from));
            if source_pending
                && target_hash != j["sourceHash"].as_str().unwrap_or("")
                && Some(target_hash.as_str()) != rewritten_source
            {
                return Err(AppError::new("E_EXTERNAL_MODIFIED", "重命名目标被外部修改"));
            }
            let edits = j["edits"].as_array().cloned().unwrap_or_default();
            for edit in &edits {
                let original = string(edit, "path")?;
                // A durable done marker proves this edit was committed. Preserve any
                // later user changes and resume only the remaining writes.
                if j["done"].as_array().unwrap().contains(&json!(original)) {
                    continue;
                }
                let path = if original == from { &to } else { original };
                let rp = RelPath::new(path)?;
                let s = self.store.read(&rp)?;
                let hash = content_hash(&s.raw);
                if Some(hash.as_str()) != edit["afterHash"].as_str() {
                    if Some(hash.as_str()) != edit["beforeHash"].as_str() {
                        return Err(AppError::new(
                            "E_EXTERNAL_MODIFIED",
                            format!("引用文件已变化：{path}"),
                        ));
                    }
                    self.store
                        .write_atomic(&rp, string(edit, "body")?.as_bytes())?;
                    self.changed_paths.insert(path.to_owned());
                }
                if !j["done"].as_array().unwrap().contains(&json!(original)) {
                    j["done"].as_array_mut().unwrap().push(json!(original));
                    self.state.write("renames", id, &j)?;
                }
            }
            self.state.remove("renames", id)?;
            self.known.clear();
            Ok(edits.len())
        })();
        match result {
            Ok(count) => {
                Ok(json!({"renamed":true,"path":to,"updatedFiles":count,"failedFiles":[]}))
            }
            Err(cause) => {
                let mut e = AppError::new(
                    "E_PARTIAL_RENAME",
                    format!("重命名待恢复：{}", cause.message),
                );
                e.detail = json!({"journalId":id,"from":from,"to":to,"updated":j["done"],"pending":j["edits"],"cause":cause.code});
                Err(e)
            }
        }
    }
}
fn timestamp(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms.min(i64::MAX as u64) as i64)
        .unwrap_or_default()
        .to_rfc3339()
}
fn compact_stamp(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms.min(i64::MAX as u64) as i64)
        .unwrap_or_default()
        .format("%Y%m%d-%H%M%S")
        .to_string()
}
fn relative_path(source_dir: &str, target: &str) -> String {
    let a: Vec<_> = source_dir.split('/').filter(|p| !p.is_empty()).collect();
    let b: Vec<_> = target.split('/').collect();
    let common = a.iter().zip(&b).take_while(|(a, b)| a == b).count();
    let mut parts = vec![".."; a.len() - common];
    parts.extend_from_slice(&b[common..]);
    parts.join("/")
}
fn validate_rename_journal(j: &Value, id: &str) -> Result<()> {
    let invalid = || {
        AppError::new(
            "E_FORMAT_UNSUPPORTED",
            "重命名恢复日志结构或哈希无效；原记录已保留",
        )
    };
    if j["id"].as_str() != Some(id) || !j["moved"].is_boolean() {
        return Err(invalid());
    }
    let from = j["from"].as_str().ok_or_else(invalid)?;
    let to = j["to"].as_str().ok_or_else(invalid)?;
    NoteService::ordinary(from)?;
    NoteService::ordinary(to)?;
    if from == to {
        return Err(invalid());
    }
    let valid_hash = |v: &Value| {
        v.as_str().is_some_and(|s| {
            s.len() == 71
                && s.starts_with("sha256:")
                && s[7..].bytes().all(|b| b.is_ascii_hexdigit())
        })
    };
    if !valid_hash(&j["sourceHash"]) {
        return Err(invalid());
    }
    if !j["done"]
        .as_array()
        .is_some_and(|a| a.iter().all(Value::is_string))
    {
        return Err(invalid());
    }
    let edits = j["edits"].as_array().ok_or_else(invalid)?;
    let mut paths = HashSet::new();
    for edit in edits {
        let path = edit["path"].as_str().ok_or_else(invalid)?;
        NoteService::ordinary(path)?;
        let body = edit["body"].as_str().ok_or_else(invalid)?;
        if !paths.insert(path)
            || body.len() > NOTE_SIZE_LIMIT
            || !valid_hash(&edit["beforeHash"])
            || Some(content_hash(body.as_bytes()).as_str()) != edit["afterHash"].as_str()
        {
            return Err(invalid());
        }
    }
    Ok(())
}
