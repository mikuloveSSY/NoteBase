#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use notebase_core::{App, Request, Response};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, State};
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
            // 可重定位缓存以支持独立测试/便携开发；后台仍强制校验绝对路径与库外位置。
            let data_root = std::env::var_os("NOTEBASE_DATA_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or(app.path().app_data_dir()?);
            let handle = app.handle().clone();
            let events = Arc::new(move |name: &str, value: serde_json::Value| {
                let _ = handle.emit(name, value);
            });
            app.manage(Arc::new(Mutex::new(notebase_core::new_indexed_app(
                data_root, events,
            ))));
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
