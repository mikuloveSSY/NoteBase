#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use notebase_core::{App, Request, Response};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, State};
mod shortcut;
struct Shell {
    app: Mutex<App>,
    logger: notebase_core::logger::Logger,
    shortcut: shortcut::GlobalShortcut,
    handle: tauri::AppHandle,
}
type Shared = Arc<Shell>;
async fn dispatch(expected: &str, request: Request, app: Shared) -> Response {
    if request.command != expected {
        return Response::Err {
            id: request.id,
            ok: false,
            error: notebase_core::domain::AppError::new("E_INTERNAL", "请求信封与传输命令不一致"),
        };
    }
    let id = request.id.clone();
    match tauri::async_runtime::spawn_blocking(move || {
        if request.command == "search" {
            let session = match app.app.lock() {
                Ok(core) => core.search_session(),
                Err(_) => Err(notebase_core::domain::AppError::new("E_INTERNAL", "应用状态锁异常")),
            };
            // The Arc remains valid outside the application lock; switching cancels it.
            let result = session.and_then(|session| session.search(&request.payload));
            let code = result.as_ref().err().map(|error| error.code.as_str());
            if let Err(error) = app.logger.record("app-shell", "search", code) {
                let _ = app.handle.emit("notebase:error_raised",serde_json::json!({"code":error.code,"message":"本地诊断日志写入失败；笔记操作结果不受影响"}));
            }
            return match result {
                Ok(data) => Response::Ok { id: request.id, ok: true, data },
                Err(error) => Response::Err { id: request.id, ok: false, error },
            };
        }
        match app.app.lock() {
        Ok(mut core) => {
            let failure = |code: &str, message: &str| Response::Err { id: request.id.clone(), ok: false, error: notebase_core::domain::AppError::new(code, message) };
            if app.handle.webview_windows().len()>1 && (request.command=="vault_open" || (request.command=="settings_set" && request.payload["values"].get("vaultPath").is_some())) {
                let info=core.dispatch(Request{id:"shell-vault".into(),command:"vault_info".into(),payload:serde_json::json!({}),client_seq:0});
                let same=matches!(info,Response::Ok{data,..} if request.command=="vault_open" && data["vaultPath"]==request.payload["path"]);
                if !same { return failure("E_CANCELLED","切换或迁移笔记库前请先关闭其他窗口，避免丢失未保存内容"); }
            }
            let new_shortcut=request.payload["values"]["keymap"]["globalQuickNote"].as_str().map(str::to_owned);
            let mut previous=None;
            if let Some(new_shortcut)=new_shortcut.filter(|_| request.command=="settings_set") {
                if let Response::Ok{data,..}=core.dispatch(Request{id:"shell-settings".into(),command:"settings_get".into(),payload:serde_json::json!({}),client_seq:0}) {
                    if let Err(error)=notebase_core::settings::merged(&data["values"],&request.payload["values"]) { return failure(&error.code,&error.message); }
                    previous=data["values"]["keymap"]["globalQuickNote"].as_str().map(str::to_owned);
                }
                if let Err(error)=app.shortcut.configure(&new_shortcut) { return failure("E_KEYMAP_CONFLICT",&error); }
            }
            let command=request.command.clone();
            let response=core.dispatch(request);
            let code=match &response {Response::Err{error,..}=>Some(error.code.as_str()),_=>None};
            if code.is_some() { if let Some(previous)=previous { let _=app.shortcut.configure(&previous); } }
            if let Err(error)=app.logger.record("app-shell",&command,code) {
                let _=app.handle.emit("notebase:error_raised",serde_json::json!({"code":error.code,"message":"本地诊断日志写入失败；笔记操作结果不受影响"}));
            }
            response
        },
        Err(_) => Response::Err {
            id: request.id,
            ok: false,
            error: notebase_core::domain::AppError::new("E_INTERNAL", "应用状态锁异常"),
        },
    }})
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
            let mut core = notebase_core::new_indexed_app(data_root.clone(), events);
            let handle = app.handle().clone();
            let shortcut = shortcut::GlobalShortcut::start(move || {
                let handle2 = handle.clone();
                let _ = handle.run_on_main_thread(move || {
                    let window = handle2
                        .get_webview_window("main")
                        .or_else(|| handle2.webview_windows().into_values().next());
                    if let Some(window) = window {
                        let _ = window.unminimize();
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                });
            });
            let preferences = core.dispatch(Request {
                id: "startup".into(),
                command: "settings_get".into(),
                payload: serde_json::json!({}),
                client_seq: 0,
            });
            let logger = notebase_core::logger::Logger::new(data_root.join("logs"));
            let _ = logger.record("app-shell", "startup", None);
            if let Response::Ok { data, .. } = preferences {
                if shortcut
                    .configure(
                        data["values"]["keymap"]["globalQuickNote"]
                            .as_str()
                            .unwrap_or("Ctrl+Shift+Space"),
                    )
                    .is_err()
                {
                    let _ = logger.record("app-shell", "shortcut", Some("E_KEYMAP_CONFLICT"));
                }
            }
            app.manage(Arc::new(Shell {
                app: Mutex::new(core),
                logger,
                shortcut,
                handle: app.handle().clone(),
            }));
            use tauri::menu::{MenuBuilder, SubmenuBuilder};
            let file = SubmenuBuilder::new(app, "文件")
                .text("new", "新建笔记")
                .text("search", "搜索笔记")
                .text("save", "保存")
                .separator()
                .text("newWindow", "新建窗口")
                .text("settings", "设置")
                .build()?;
            let view = SubmenuBuilder::new(app, "视图")
                .text("commands", "命令面板")
                .text("focus", "专注模式")
                .build()?;
            app.set_menu(MenuBuilder::new(app).item(&file).item(&view).build()?)?;
            Ok(())
        })
        .on_menu_event(|app, event| {
            if let Some(window) = app
                .webview_windows()
                .into_values()
                .find(|w| w.is_focused().unwrap_or(false))
            {
                // Native shell-only event; not an addition to the frozen business event set.
                let _ = window.emit("notebase:shell_action", event.id().as_ref());
            }
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
