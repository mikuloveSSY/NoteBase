//! WebDAV adapter and conservative file-level three-way sync. Never merges text.
use super::{
    files::{self, ASSET_LIMIT, TOTAL_LIMIT},
    DataService, LocalDataService,
};
use crate::domain::{field, AppError, Result};
use reqwest::{
    blocking::{Client, Response},
    header::{IF_MATCH, IF_NONE_MATCH},
    Method, StatusCode, Url,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[cfg(not(any(windows, target_os = "macos")))]
use std::io::Write;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Clean,
    Upload,
    Download,
    DeleteLocal,
    DeleteRemote,
    Conflict,
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use std::{io::Write, net::TcpListener, sync::atomic::Ordering, sync::mpsc};

    fn cancel_http_retry(during_request: bool) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let cancelled = flag.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
            }
            entered_tx.send(()).unwrap();
            if during_request {
                let deadline = Instant::now() + Duration::from_secs(5);
                while !cancelled.load(Ordering::Acquire) {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            listener
        });
        let mut backend =
            WebDav::new(&format!("http://{address}/notes/"), "fixture", "fixture").unwrap();
        backend.cancelled = flag.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || done_tx.send(backend.list()).unwrap());
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        if !during_request {
            std::thread::sleep(Duration::from_millis(150));
        }
        flag.store(true, Ordering::Release);
        assert_eq!(
            done_rx
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap_err()
                .code,
            "E_CANCELLED"
        );
        worker.join().unwrap();
        let listener = server.join().unwrap();
        listener.set_nonblocking(true).unwrap();
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "cancel must prevent another HTTP request"
        );
    }

    #[test]
    fn cancellation_after_failed_http_request_prevents_retries() {
        cancel_http_retry(true);
    }

    #[test]
    fn cancellation_interrupts_http_retry_backoff() {
        cancel_http_retry(false);
    }
}
pub fn plan(base: Option<&str>, local: Option<&str>, remote: Option<&str>) -> Action {
    if local == remote {
        return Action::Clean;
    }
    match base {
        None => match (local, remote) {
            (Some(_), None) => Action::Upload,
            (None, Some(_)) => Action::Download,
            _ => Action::Conflict,
        },
        Some(base) => {
            if local == Some(base) {
                if remote.is_none() {
                    Action::DeleteLocal
                } else {
                    Action::Download
                }
            } else if remote == Some(base) {
                if local.is_none() {
                    Action::DeleteRemote
                } else {
                    Action::Upload
                }
            } else {
                Action::Conflict
            }
        }
    }
}
pub trait CredentialStore: Send + Sync {
    fn set(&self, key: &str, password: &str) -> Result<()>;
    fn get(&self, key: &str) -> Result<String>;
}
pub struct OsCredentials;
fn auth(message: &str) -> AppError {
    AppError::new("E_SYNC_AUTH", message)
}
impl CredentialStore for OsCredentials {
    fn set(&self, key: &str, password: &str) -> Result<()> {
        #[cfg(any(windows, target_os = "macos"))]
        {
            keyring::Entry::new("NoteBase", key)
                .and_then(|e| e.set_password(password))
                .map_err(|_| auth("系统凭据库不可用，未保存密码"))
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            use std::process::{Command, Stdio};
            let mut child = Command::new("secret-tool")
                .args([
                    "store",
                    "--label=NoteBase WebDAV",
                    "service",
                    "NoteBase",
                    "vault",
                    key,
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| {
                    auth("系统凭据库不可用；需要 secret-tool 与已解锁的 Secret Service")
                })?;
            child
                .stdin
                .take()
                .ok_or_else(|| auth("无法访问凭据库输入"))?
                .write_all(password.as_bytes())
                .map_err(|_| auth("无法保存系统凭据"))?;
            if child.wait().map_err(|_| auth("凭据库请求失败"))?.success() {
                Ok(())
            } else {
                Err(auth("凭据库拒绝保存，密码未写入应用文件"))
            }
        }
    }
    fn get(&self, key: &str) -> Result<String> {
        #[cfg(any(windows, target_os = "macos"))]
        {
            keyring::Entry::new("NoteBase", key)
                .and_then(|e| e.get_password())
                .map_err(|_| auth("无法读取系统凭据，请重新配置同步"))
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let out = std::process::Command::new("secret-tool")
                .args(["lookup", "service", "NoteBase", "vault", key])
                .output()
                .map_err(|_| auth("系统凭据库不可用，无法同步"))?;
            if !out.status.success() {
                return Err(auth("未找到系统凭据，请重新配置同步"));
            }
            String::from_utf8(out.stdout)
                .map(|s| s.trim_end_matches('\n').to_owned())
                .map_err(|_| auth("系统凭据格式无效"))
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    backend: String,
    endpoint: String,
    username: String,
    credential_ref: String,
    exclude_globs: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Entry {
    base_hash: Option<String>,
    state: String,
    conflict_local: Option<String>,
    conflict_remote: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SyncState {
    endpoint: String,
    files: BTreeMap<String, Entry>,
    last_synced_ms: Option<u64>,
    last_error: Option<String>,
    pending: usize,
}

pub trait SyncBackend {
    fn list(&self) -> Result<BTreeMap<String, String>>;
    fn get(&self, path: &str, etag: &str) -> Result<Vec<u8>>;
    fn put(&self, path: &str, body: &[u8], etag: Option<&str>) -> Result<()>;
    fn delete(&self, path: &str, etag: &str) -> Result<()>;
}
pub struct WebDav {
    client: Client,
    base: Url,
    user: String,
    secret: String,
    cancelled: Arc<AtomicBool>,
}
fn network() -> AppError {
    let mut e = AppError::new(
        "E_SYNC_NETWORK",
        "WebDAV 请求失败或超时，请检查网络和服务配置",
    );
    e.retryable = true;
    e
}
fn endpoint(text: &str) -> Result<Url> {
    let mut url =
        Url::parse(text).map_err(|_| AppError::new("E_PATH_INVALID", "无效 WebDAV 地址"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::new(
            "E_PATH_INVALID",
            "地址不得嵌入凭据、查询或片段",
        ));
    }
    if url.scheme() != "https"
        && !(url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
    {
        return Err(AppError::new(
            "E_PATH_INVALID",
            "同步需要 HTTPS；仅本机测试允许 HTTP",
        ));
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}
impl WebDav {
    pub fn new(url: &str, user: &str, secret: &str) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| network())?,
            base: endpoint(url)?,
            user: user.into(),
            secret: secret.into(),
            cancelled: Arc::new(AtomicBool::new(false)),
        })
    }
    fn check_cancelled(&self) -> Result<()> {
        if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Err(AppError::new(
                "E_CANCELLED",
                "同步已取消，已完成的文件操作保留并核对",
            ));
        }
        Ok(())
    }
    fn backoff(&self, delay: Duration) -> Result<()> {
        let deadline = Instant::now() + delay;
        loop {
            self.check_cancelled()?;
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(());
            };
            std::thread::sleep(remaining.min(Duration::from_millis(50)));
        }
    }
    fn url(&self, path: &str) -> Result<Url> {
        let mut url = self.base.clone();
        if !path.is_empty() {
            crate::domain::validate_rel(path, false)?;
            let mut parts = url
                .path_segments_mut()
                .map_err(|_| AppError::new("E_PATH_INVALID", "同步地址无路径"))?;
            parts.pop_if_empty();
            for segment in path.split('/') {
                parts.push(segment);
            }
        }
        Ok(url)
    }
    fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&[u8]>,
        condition: Option<(&str, &str)>,
    ) -> Result<Response> {
        let url = self.url(path)?;
        let mut last = network();
        for attempt in 0..=5 {
            self.check_cancelled()?;
            let mut req = self
                .client
                .request(method.clone(), url.clone())
                .basic_auth(&self.user, Some(&self.secret));
            if method.as_str() == "PROPFIND" {
                req = req
                    .header("Depth", "1")
                    .header("Content-Type", "application/xml");
            }
            if let Some((key, value)) = condition {
                req = req.header(key, value);
            }
            if let Some(body) = body {
                req = req.body(body.to_vec());
            }
            let response = req.send();
            // A blocking request keeps its existing 30 s timeout. Cancellation
            // must prevent a fresh request or backoff after that request returns.
            self.check_cancelled()?;
            match response {
                Ok(r) if matches!(r.status(), StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => {
                    return Err(auth("WebDAV 凭据失效或权限不足"))
                }
                Ok(r) if r.status() == StatusCode::PRECONDITION_FAILED => {
                    return Err(AppError::new(
                        "E_EXTERNAL_MODIFIED",
                        "远端在同步期间变化，条件写入已拒绝",
                    ))
                }
                Ok(r) if !r.status().is_server_error() => return Ok(r),
                _ => {
                    last = network();
                    if attempt < 5 {
                        self.backoff(Duration::from_secs(1 << attempt))?;
                    }
                }
            }
        }
        Err(last)
    }
    fn success(r: Response) -> Result<Response> {
        if r.status().is_success() {
            Ok(r)
        } else {
            Err(network())
        }
    }
}
impl SyncBackend for WebDav {
    fn list(&self) -> Result<BTreeMap<String, String>> {
        let mut found = BTreeMap::new();
        let mut dirs = vec![String::new()];
        let mut visited = BTreeSet::new();
        while let Some(dir) = dirs.pop() {
            if !visited.insert(dir.clone()) {
                continue;
            }
            if visited.len() + found.len() > 200000 || dir.split('/').count() > 64 {
                return Err(AppError::new("E_FILE_TOO_LARGE", "同步远端清单超过上限"));
            }
            let r=Self::success(self.send(Method::from_bytes(b"PROPFIND").unwrap(),&dir,Some(b"<?xml version=\"1.0\"?><d:propfind xmlns:d=\"DAV:\"><d:prop><d:resourcetype/><d:getetag/></d:prop></d:propfind>"),None)?)?;
            let mut xml = String::new();
            r.take(8 * 1024 * 1024 + 1)
                .read_to_string(&mut xml)
                .map_err(|_| network())?;
            if xml.len() > 8 * 1024 * 1024 {
                return Err(network());
            }
            let doc = roxmltree::Document::parse(&xml).map_err(|_| network())?;
            for item in doc
                .descendants()
                .filter(|n| n.tag_name().name() == "response")
            {
                let Some(href) = item
                    .descendants()
                    .find(|n| n.tag_name().name() == "href")
                    .and_then(|n| n.text())
                else {
                    continue;
                };
                let url = self.base.join(href).map_err(|_| network())?;
                if url.origin() != self.base.origin() || !url.path().starts_with(self.base.path()) {
                    return Err(AppError::new("E_PATH_INVALID", "WebDAV 返回库外地址"));
                }
                let rel =
                    percent_encoding::percent_decode_str(&url.path()[self.base.path().len()..])
                        .decode_utf8()
                        .map_err(|_| network())?
                        .trim_end_matches('/')
                        .to_owned();
                if rel.is_empty() || rel == dir {
                    continue;
                }
                crate::domain::validate_rel(&rel, false)?;
                let collection = item
                    .descendants()
                    .any(|n| n.tag_name().name() == "collection");
                if collection {
                    dirs.push(rel);
                } else {
                    let etag = item
                        .descendants()
                        .find(|n| n.tag_name().name() == "getetag")
                        .and_then(|n| n.text())
                        .unwrap_or("")
                        .to_owned();
                    found.insert(rel, etag);
                }
            }
        }
        Ok(found)
    }
    fn get(&self, path: &str, etag: &str) -> Result<Vec<u8>> {
        if etag.is_empty() {
            return Err(AppError::new(
                "E_SYNC_NETWORK",
                "远端未提供 ETag，拒绝不受保护的同步",
            ));
        }
        let r =
            Self::success(self.send(Method::GET, path, None, Some((IF_MATCH.as_str(), etag)))?)?;
        let mut out = Vec::new();
        r.take(ASSET_LIMIT + 1)
            .read_to_end(&mut out)
            .map_err(|_| network())?;
        if out.len() as u64 > ASSET_LIMIT {
            return Err(AppError::new("E_FILE_TOO_LARGE", "远端文件超过 100 MiB"));
        }
        Ok(out)
    }
    fn put(&self, path: &str, body: &[u8], etag: Option<&str>) -> Result<()> {
        if let Some((parent, _)) = path.rsplit_once('/') {
            let mut parts = Vec::new();
            for segment in parent.split('/') {
                parts.push(segment);
                let r = self.send(
                    Method::from_bytes(b"MKCOL").unwrap(),
                    &parts.join("/"),
                    None,
                    None,
                )?;
                if !(r.status().is_success() || r.status() == StatusCode::METHOD_NOT_ALLOWED) {
                    return Err(network());
                }
            }
        }
        let condition = match etag {
            Some("") => return Err(AppError::new("E_SYNC_NETWORK", "远端缺少 ETag，禁止覆盖")),
            Some(e) => (IF_MATCH.as_str(), e),
            None => (IF_NONE_MATCH.as_str(), "*"),
        };
        Self::success(self.send(Method::PUT, path, Some(body), Some(condition))?)?;
        Ok(())
    }
    fn delete(&self, path: &str, etag: &str) -> Result<()> {
        if etag.is_empty() {
            return Err(network());
        }
        Self::success(self.send(Method::DELETE, path, None, Some((IF_MATCH.as_str(), etag)))?)?;
        Ok(())
    }
}
impl LocalDataService {
    fn sync_config(&self) -> Result<Config> {
        serde_json::from_slice(
            &files::read_limit(&self.private.join("sync-config.json"), 65536)
                .map_err(|_| auth("同步未配置或配置不可读"))?,
        )
        .map_err(|_| auth("同步未配置或配置无效"))
    }
    pub fn sync_configure_with(
        &self,
        p: &Value,
        credentials: &dyn CredentialStore,
    ) -> Result<Value> {
        if field(p, "backend")? != "webdav" {
            return Err(AppError::new("E_FORMAT_UNSUPPORTED", "仅支持 WebDAV"));
        }
        let url = endpoint(field(p, "endpoint")?)?;
        let id = self
            .private
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let key = format!("notebase/webdav/{id}");
        if p.get("secretRef")
            .and_then(Value::as_str)
            .is_some_and(|v| v != key)
        {
            return Err(auth("secretRef 必须指向当前库的凭据项"));
        }
        let exclude_globs = if let Some(v) = p.get("excludeGlobs") {
            let items = v
                .as_array()
                .ok_or_else(|| AppError::new("E_PATH_INVALID", "excludeGlobs 必须为数组"))?;
            items
                .iter()
                .map(|x| {
                    let pattern = x
                        .as_str()
                        .ok_or_else(|| AppError::new("E_PATH_INVALID", "排除项必须为字符串"))?;
                    globset::Glob::new(pattern)
                        .map_err(|_| AppError::new("E_PATH_INVALID", "无效排除模式"))?;
                    Ok(pattern.to_owned())
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            vec!["attachments/**".into(), "trash/**".into()]
        };
        let config = Config {
            backend: "webdav".into(),
            endpoint: url.to_string(),
            username: field(p, "username")?.into(),
            credential_ref: key,
            exclude_globs,
        };
        if let Some(password) = p.get("password").and_then(Value::as_str) {
            if password.is_empty() {
                return Err(auth("密码不能为空"));
            }
            credentials.set(&config.credential_ref, password)?;
        } else {
            credentials.get(&config.credential_ref)?;
        }
        files::atomic(
            &self.private.join("sync-config.json"),
            &serde_json::to_vec_pretty(&config).unwrap(),
            true,
        )?;
        Ok(json!({"ok":true,"secretRef":config.credential_ref}))
    }
    pub fn sync_status(&self) -> Result<Value> {
        let config = self.sync_config();
        let state = self.read_sync_state()?;
        let mut dirty = state.pending;
        if let Ok(cfg) = &config {
            let mut globs = globset::GlobSetBuilder::new();
            for pattern in &cfg.exclude_globs {
                globs.add(
                    globset::Glob::new(pattern)
                        .map_err(|_| AppError::new("E_PATH_INVALID", "无效同步排除模式"))?,
                );
            }
            let excluded = globs
                .build()
                .map_err(|_| AppError::new("E_PATH_INVALID", "无效同步排除模式"))?;
            let paths: BTreeSet<_> = self
                .files
                .entries()?
                .into_iter()
                .chain(state.files.keys().cloned())
                .filter(|p| !p.contains(" (conflict remote ") && !excluded.is_match(p))
                .collect();
            let mut bytes = 0u64;
            for path in paths {
                let file = self.files.path(&path)?;
                let local = if file.exists() {
                    let data = files::read_limit(&file, ASSET_LIMIT)?;
                    bytes += data.len() as u64;
                    if bytes > TOTAL_LIMIT {
                        return Err(AppError::new("E_FILE_TOO_LARGE", "同步状态扫描超过 2 GiB"));
                    }
                    Some(files::digest(&data))
                } else {
                    None
                };
                if state.files.get(&path).and_then(|v| v.base_hash.as_ref()) != local.as_ref() {
                    dirty += 1;
                }
            }
        }
        let conflicts: Vec<_> = state
            .files
            .iter()
            .filter(|(_, v)| v.state == "conflict")
            .map(|(p, _)| p)
            .collect();
        Ok(
            json!({"state":if config.is_err(){"unconfigured"}else if !conflicts.is_empty(){"conflict"}else if state.last_error.is_some()||dirty>0||state.last_synced_ms.is_none(){"dirty"}else{"clean"},"pending":dirty,"conflicts":conflicts,"lastError":state.last_error,"authRequired":config.is_err()||state.last_error.as_deref()==Some("E_SYNC_AUTH"),"lastSyncedMs":state.last_synced_ms}),
        )
    }
    fn read_sync_state(&self) -> Result<SyncState> {
        let path = self.private.join("sync-state.json");
        if !path.exists() {
            return Ok(SyncState::default());
        }
        serde_json::from_slice(&files::read_limit(&path, 32 * 1024 * 1024)?).map_err(|_| {
            AppError::new(
                "E_FORMAT_UNSUPPORTED",
                "同步基准损坏，保留原件并拒绝推断删除",
            )
        })
    }
    fn write_sync_state(&self, state: &SyncState) -> Result<()> {
        let encoded = serde_json::to_vec_pretty(state).unwrap();
        if encoded.len() > 32 * 1024 * 1024 {
            return Err(AppError::new("E_FILE_TOO_LARGE", "同步基准超过 32 MiB"));
        }
        files::atomic(&self.private.join("sync-state.json"), &encoded, true)
    }
    pub fn sync_run(&self, p: &Value) -> Result<Value> {
        let result = (|| {
            self.check_cancelled()?;
            let cfg = self.sync_config()?;
            let password = OsCredentials.get(&cfg.credential_ref)?;
            let mut backend = WebDav::new(&cfg.endpoint, &cfg.username, &password)?;
            backend.cancelled = self.cancelled.clone();
            self.sync_with(&backend, p)
        })();
        if let Err(e) = &result {
            if let Ok(mut state) = self.read_sync_state() {
                state.last_error = Some(e.code.clone());
                let _ = self.write_sync_state(&state);
            }
        }
        result
    }
    /// Testable transport boundary; the same planner is used by the real WebDAV adapter.
    pub fn sync_with(&self, backend: &dyn SyncBackend, p: &Value) -> Result<Value> {
        let cfg = self.sync_config()?;
        let mut state = self.read_sync_state()?;
        if state.endpoint != cfg.endpoint {
            state = SyncState {
                endpoint: cfg.endpoint.clone(),
                ..Default::default()
            };
        }
        let mut exclusions = globset::GlobSetBuilder::new();
        for glob in cfg.exclude_globs {
            exclusions.add(
                globset::Glob::new(&glob)
                    .map_err(|_| AppError::new("E_PATH_INVALID", "排除配置无效"))?,
            );
        }
        let exclusions = exclusions
            .build()
            .map_err(|_| AppError::new("E_PATH_INVALID", "排除配置无效"))?;
        let eligible = |path: &str| {
            (path.ends_with(".md") || path.starts_with("attachments/"))
                && !path.starts_with(".git/")
                && !path.contains(" (conflict remote ")
                && !exclusions.is_match(path)
        };
        let result = (|| -> Result<Value> {
            self.check_cancelled()?;
            let remote = backend.list()?;
            let local_paths = self.files.entries()?;
            let union: BTreeSet<_> = local_paths
                .into_iter()
                .chain(remote.keys().cloned())
                .chain(state.files.keys().cloned())
                .filter(|p| eligible(p))
                .collect();
            if union.len() > 200000 {
                return Err(AppError::new("E_FILE_TOO_LARGE", "同步文件数量超过 200000"));
            }
            state.pending = union.len();
            self.write_sync_state(&state)?;
            let mut changed = Vec::new();
            let mut conflicts = Vec::new();
            let mut transferred = 0u64;
            for path in union {
                self.check_cancelled()?;
                let local_file = self.files.path(&path)?;
                let local = if local_file.exists() {
                    Some(files::read_limit(&local_file, ASSET_LIMIT)?)
                } else {
                    None
                };
                let lh = local.as_ref().map(|b| files::digest(b));
                let remote_bytes = match remote.get(&path) {
                    Some(etag) => Some(backend.get(&path, etag)?),
                    None => None,
                };
                let rh = remote_bytes.as_ref().map(|b| files::digest(b));
                self.check_cancelled()?;
                transferred += local.as_ref().map_or(0, |b| b.len() as u64)
                    + remote_bytes.as_ref().map_or(0, |b| b.len() as u64);
                if transferred > TOTAL_LIMIT {
                    return Err(AppError::new("E_FILE_TOO_LARGE", "单轮同步扫描超过 2 GiB"));
                }
                let previous = state.files.get(&path).cloned().unwrap_or_default();
                let resolved = p["resolvedPaths"]
                    .as_array()
                    .is_some_and(|v| v.iter().any(|v| v.as_str() == Some(&path)));
                // A fresh ETag only protects the upcoming PUT. It does not prove
                // that the user has reviewed this remote revision as a conflict.
                // Local bytes may differ because manual merging is intentional.
                if resolved && (previous.state != "conflict" || previous.conflict_remote != rh) {
                    return Err(AppError::new(
                        "E_EXTERNAL_MODIFIED",
                        "冲突尚未确认或远端已再次变化，请先重新同步并检查双份内容",
                    ));
                }
                let base = if resolved {
                    rh.as_deref()
                } else {
                    previous.base_hash.as_deref()
                };
                let action = plan(base, lh.as_deref(), rh.as_deref());
                let mut entry = Entry {
                    base_hash: previous.base_hash.clone(),
                    state: "clean".into(),
                    ..Default::default()
                };
                // A changed local/remote pair gets exactly one conflict copy, even across restarts.
                if previous.state == "conflict"
                    && !resolved
                    && previous.conflict_local == lh
                    && previous.conflict_remote == rh
                {
                    conflicts.push(path.clone());
                    state.pending = state.pending.saturating_sub(1);
                    continue;
                }
                match action {
                    Action::Clean => entry.base_hash = lh.clone(),
                    Action::Upload => {
                        backend.put(
                            &path,
                            local.as_deref().unwrap_or(&[]),
                            remote.get(&path).map(String::as_str),
                        )?;
                        entry.base_hash = lh.clone();
                    }
                    Action::DeleteRemote => {
                        backend.delete(&path, remote.get(&path).ok_or_else(network)?)?;
                        entry.base_hash = None;
                    }
                    Action::Download | Action::DeleteLocal => {
                        if local.is_some()
                            && fs::metadata(&local_file)
                                .map_err(AppError::io)?
                                .permissions()
                                .readonly()
                        {
                            return Err(AppError::new(
                                "E_IO_PERMISSION",
                                "本地文件只读，拒绝同步覆盖",
                            ));
                        }
                        if action == Action::Download && path.ends_with(".md") {
                            let bytes = remote_bytes.as_deref().unwrap_or(&[]);
                            if bytes.len() > crate::domain::NOTE_SIZE_LIMIT {
                                return Err(AppError::new(
                                    "E_FILE_TOO_LARGE",
                                    "远端笔记超过 10 MiB",
                                ));
                            }
                            std::str::from_utf8(bytes).map_err(|_| {
                                AppError::new(
                                    "E_ENCODING_INVALID",
                                    "远端笔记不是 UTF-8，拒绝覆盖本地",
                                )
                            })?;
                        }
                        let current = if local_file.exists() {
                            Some(files::digest(&files::read_limit(&local_file, ASSET_LIMIT)?))
                        } else {
                            None
                        };
                        if current != lh {
                            return Err(AppError::new(
                                "E_EXTERNAL_MODIFIED",
                                "同步期间本地文件已变化，拒绝覆盖",
                            ));
                        }
                        // Protect the original bytes before any local mutation.
                        if let Some(bytes) = &local {
                            if path.ends_with(".md") && !path.starts_with("attachments/") {
                                self.snapshot(&path, bytes, "同步前版本")?;
                            }
                        }
                        if action == Action::Download {
                            files::atomic(
                                &local_file,
                                remote_bytes.as_deref().unwrap_or(&[]),
                                local.is_some(),
                            )?;
                            entry.base_hash = rh.clone();
                        } else {
                            let trash = self.private.join("sync-deleted").join(format!(
                                "{}-{}",
                                crate::index::now_ms(),
                                files::digest(path.as_bytes())
                            ));
                            files::directory(&trash)?;
                            let target = trash.join(local_file.file_name().unwrap_or_default());
                            fs::rename(&local_file, target).map_err(AppError::io)?;
                            entry.base_hash = None;
                        }
                        changed.push(path.clone());
                    }
                    Action::Conflict => {
                        if let Some(bytes) = &remote_bytes {
                            let stem = local_file.file_stem().unwrap_or_default().to_string_lossy();
                            let extension =
                                local_file.extension().unwrap_or_default().to_string_lossy();
                            let copy = local_file.with_file_name(format!(
                                "{stem} (conflict remote {}).{}",
                                &rh.as_deref().unwrap_or("")[..16],
                                extension
                            ));
                            if copy.exists() {
                                if files::read_limit(&copy, ASSET_LIMIT)? != *bytes {
                                    return Err(AppError::new(
                                        "E_PATH_INVALID",
                                        "冲突副本名称已被其他内容占用",
                                    ));
                                }
                            } else {
                                files::atomic(&copy, bytes, false)?;
                                changed.push(
                                    copy.strip_prefix(&self.files.root)
                                        .unwrap()
                                        .to_string_lossy()
                                        .replace('\\', "/"),
                                );
                            }
                        }
                        entry.state = "conflict".into();
                        entry.conflict_local = lh;
                        entry.conflict_remote = rh;
                        conflicts.push(path.clone());
                    }
                }
                state.files.insert(path, entry);
                state.pending = state.pending.saturating_sub(1);
                self.write_sync_state(&state)?;
            }
            state.last_error = None;
            state.last_synced_ms = Some(crate::index::now_ms());
            self.write_sync_state(&state)?;
            Ok(
                json!({"started":true,"completed":true,"changedPaths":changed,"conflicts":conflicts,"state":if conflicts.is_empty(){"clean"}else{"conflict"}}),
            )
        })();
        if let Err(error) = &result {
            state.last_error = Some(error.code.clone());
            self.write_sync_state(&state)?;
        }
        result
    }
}
