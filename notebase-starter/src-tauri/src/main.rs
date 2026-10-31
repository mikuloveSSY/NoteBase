#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use notebase_core::{App, Request, Response};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, State};
static SYNC_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
struct SyncGuard;
impl Drop for SyncGuard {
    fn drop(&mut self) {
        SYNC_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
    }
}
type Shared = Arc<Mutex<App>>;
async fn dispatch(expected: &str, request: Request, app: Shared) -> Response {
    if request.command != expected {
        return Response::Err {
            id: request.id,
            ok: false,
            error: notebase_core::domain::AppError::new("E_INTERNAL", "请求信封与传输命令不一致"),
        };
    }
    let id = request.id.clone();
    let _sync_guard = if expected == "sync_now" {
        if SYNC_ACTIVE.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return Response::Err {
                id,
                ok: false,
                error: notebase_core::domain::AppError::new("E_SYNC_BUSY", "同步任务已在执行"),
            };
        }
        Some(SyncGuard)
    } else {
        None
    };
    match tauri::async_runtime::spawn_blocking(move || match app.lock() {
        Ok(mut app) => app.dispatch(request),
        Err(_) => Response::Err {
            id: request.id,
            ok: false,
            error: notebase_core::domain::AppError::new("E_INTERNAL", "应用状态锁异常"),
        },
    })
    .await
    {
        Ok(response) => response,
        Err(_) => Response::Err {
            id,
            ok: false,
            error: notebase_core::domain::AppError::new("E_INTERNAL", "后台任务执行失败"),
        },
    }
}
// 23 个传输入口与设计保持同名，业务统一由应用层处理。
macro_rules! command {
    ($name:ident) => {
        #[tauri::command]
        async fn $name(request: Request, state: State<'_, Shared>) -> Result<Response, String> {
            // Tauri 要求带 State 引用的异步命令返回 Result；Ok 内仍是既有业务信封。
            let app = state.inner().clone();
            Ok(dispatch(stringify!($name), request, app).await)
        }
    };
}
command!(vault_open);
command!(vault_info);
command!(note_list);
command!(note_read);
command!(note_save);
command!(note_create);
command!(note_rename);
command!(note_delete);
command!(note_restore);
command!(search);
command!(link_backlinks);
command!(link_mentions);
command!(link_graph);
command!(index_status);
command!(index_rebuild);
command!(export_note);
command!(backup_create);
command!(backup_restore);
command!(sync_configure);
command!(sync_now);
command!(sync_status);
command!(settings_get);
command!(settings_set);
fn main() {
    tauri::Builder::default()
        .setup(|app| {
            // Tauri creates its default WebView directory before the WebView2 env
            // override takes effect. Apply the explicit profile at construction.
            // https://docs.rs/tauri/2.12.5/tauri/struct.WebviewWindowBuilder.html#method.data_directory
            let window_config = app
                .config()
                .app
                .windows
                .first()
                .ok_or("missing main window configuration")?;
            let mut window = tauri::WebviewWindowBuilder::from_config(app.handle(), window_config)?;
            if let Some(profile) = std::env::var_os("WEBVIEW2_USER_DATA_FOLDER") {
                let profile = std::path::PathBuf::from(profile);
                if !profile.is_absolute() {
                    return Err("WebView profile must be an absolute path".into());
                }
                window = window.data_directory(profile);
            }
            window.build()?;
            // 可重定位缓存以支持独立测试/便携开发；后台仍强制校验绝对路径与库外位置。
            let data_root = match std::env::var_os("NOTEBASE_DATA_DIR") {
                Some(path) => std::path::PathBuf::from(path),
                None => app.path().app_data_dir()?,
            };
            let handle = app.handle().clone();
            let events = Arc::new(move |name: &str, value: serde_json::Value| {
                let _ = handle.emit(&notebase_core::domain::transport_event_name(name), value);
            });
            let maintenance_events=events.clone();
            let shared=Arc::new(Mutex::new(notebase_core::new_indexed_app(data_root, events)));
            app.manage(shared.clone());
            // A weak reference avoids keeping the application alive after shell teardown.
            let weak=Arc::downgrade(&shared);
            std::thread::spawn(move||loop{
                std::thread::sleep(std::time::Duration::from_secs(60));
                let Some(app)=weak.upgrade() else {break};
                if let Ok(mut guard)=app.try_lock(){
                    let now=notebase_core::index::now_ms();
                    if let Err(error)=guard.maintenance(now){maintenance_events("error.raised",serde_json::json!({"code":error.code,"message":error.message,"source":"backup-svc"}));}
                    if SYNC_ACTIVE.compare_exchange(false,true,std::sync::atomic::Ordering::AcqRel,std::sync::atomic::Ordering::Acquire).is_ok(){
                        let _lease=SyncGuard;
                        if let Err(error)=guard.background_sync(now){maintenance_events("error.raised",serde_json::json!({"code":error.code,"message":error.message,"source":"sync-svc"}));}
                    }
                };
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            vault_open,
            vault_info,
            note_list,
            note_read,
            note_save,
            note_create,
            note_rename,
            note_delete,
            note_restore,
            search,
            link_backlinks,
            link_mentions,
            link_graph,
            index_status,
            index_rebuild,
            export_note,
            backup_create,
            backup_restore,
            sync_configure,
            sync_now,
            sync_status,
            settings_get,
            settings_set
        ])
        .run(tauri::generate_context!())
        .expect("启动 NoteBase 失败");
}
