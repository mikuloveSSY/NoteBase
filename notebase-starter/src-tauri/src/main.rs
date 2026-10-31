#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use notebase_core::{App, Request, Response};
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, State};
mod shortcut;
struct Shell {
    shutting_down: std::sync::atomic::AtomicBool,
    scan_cancelled: Arc<std::sync::atomic::AtomicBool>,
    app: Mutex<App>,
    logger: notebase_core::logger::Logger,
    shortcut: shortcut::GlobalShortcut,
    handle: tauri::AppHandle,
}
type Shared = Arc<Shell>;
fn validate_vault_location(
    path: &str,
    executable: &std::path::Path,
) -> notebase_core::domain::Result<()> {
    // Invalid/missing paths are still diagnosed by the original core validator.
    if let Ok(root) = std::fs::canonicalize(path) {
        let contains_executable =
            std::fs::canonicalize(executable).is_ok_and(|exe| exe.starts_with(&root));
        // Also recognize our source workspace when the running app is installed elsewhere.
        let own_project = [
            "src-tauri/tauri.conf.json",
            "notebase-starter/src-tauri/tauri.conf.json",
            "repository/notebase-starter/src-tauri/tauri.conf.json",
        ]
        .iter()
        .any(|relative| {
            use std::io::Read;
            let Ok(file) = std::fs::File::open(root.join(relative)) else {
                return false;
            };
            let mut text = String::new();
            if file.take(65536).read_to_string(&mut text).is_err() {
                return false;
            }
            serde_json::from_str::<serde_json::Value>(&text)
                .is_ok_and(|config| config["identifier"] == "edu.notebase.desktop")
        });
        if contains_executable || own_project {
            return Err(notebase_core::domain::AppError::new(
                "E_PATH_INVALID",
                "不能将软件运行目录、开发目录或其上级目录作为笔记库，请选择专用笔记文件夹",
            ));
        }
    }
    Ok(())
}
fn close_guard(core: &Mutex<App>, scan: &std::sync::atomic::AtomicBool) -> Option<&'static str> {
    match core.try_lock() {
        Ok(core) if core.data_task_active() => {
            core.request_data_task_cancel();
            Some("正在取消后台任务；完成文件核对后可再次关闭窗口")
        }
        Ok(_) => None,
        Err(std::sync::TryLockError::WouldBlock) => {
            scan.store(true, std::sync::atomic::Ordering::Release);
            Some("后台操作尚未结束，已请求中止目录扫描；请稍后再次关闭窗口")
        }
        Err(std::sync::TryLockError::Poisoned(_)) => Some("应用状态异常，暂时无法安全关闭窗口"),
    }
}
fn defer_windowless_exit(handle: &tauri::AppHandle, shared: Shared) {
    if !handle.webview_windows().is_empty() {
        return;
    }
    let handle = handle.clone();
    // Wait off the UI thread if the last window disappeared during an IPC race.
    tauri::async_runtime::spawn_blocking(move || loop {
        let ready = match shared.app.lock() {
            Ok(core) if core.data_task_active() => {
                core.request_data_task_cancel();
                false
            }
            Ok(_) => {
                shared
                    .shutting_down
                    .store(true, std::sync::atomic::Ordering::Release);
                true
            }
            Err(_) => return,
        };
        if ready {
            handle.exit(0);
            return;
        }
        if !handle.webview_windows().is_empty() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    });
}
fn same_vault_path(current: &serde_json::Value, requested: &serde_json::Value) -> bool {
    let (Some(current), Some(requested)) = (current.as_str(), requested.as_str()) else {
        return false;
    };
    // The native picker returns a normal path; the active vault may use \\?\.
    // Compare filesystem identities before applying the multi-window switch guard.
    match (
        std::fs::canonicalize(current),
        std::fs::canonicalize(requested),
    ) {
        (Ok(current), Ok(requested)) => current == requested,
        _ => false,
    }
}
fn execute_data_task(
    app: &Shared,
    mut task: notebase_core::application::DataTask,
) -> notebase_core::domain::Result<serde_json::Value> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task.run()))
        .unwrap_or_else(|_| {
            Err(notebase_core::domain::AppError::new(
                "E_INTERNAL",
                "数据任务异常结束",
            ))
        });
    app.app
        .lock()
        .map_err(|_| notebase_core::domain::AppError::new("E_INTERNAL", "应用状态锁异常"))?
        .finish_data_task(task, result)
}
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
        if request.command == "vault_open" {
            if let (Some(path), Ok(exe)) = (request.payload["path"].as_str(), std::env::current_exe()) {
                if let Err(error) = validate_vault_location(path, &exe) {
                    let _ = app.logger.record("app-shell", "vault_open", Some(&error.code));
                    return Response::Err { id:request.id, ok:false, error };
                }
            }
        }
        if matches!(request.command.as_str(), "export_note" | "backup_restore" | "sync_now") || (request.command == "backup_create" && request.payload["scope"] != "note") {
            let prepared = match app.app.lock() {
                Ok(mut core) => {
                    if app.shutting_down.load(std::sync::atomic::Ordering::Acquire) {
                        return Response::Err { id:request.id,ok:false,error:notebase_core::domain::AppError::new("E_CANCELLED","应用正在退出") };
                    }
                    if request.command == "backup_restore" && request.payload["verifyOnly"] != true && app.handle.webview_windows().len() > 1 {
                        Err(notebase_core::domain::AppError::new("E_CANCELLED", "恢复前请先保存并关闭其他窗口"))
                    } else { core.begin_data_task(&request.command, &request.payload) }
                }
                Err(_) => Err(notebase_core::domain::AppError::new("E_INTERNAL", "应用状态锁异常")),
            };
            let result = prepared.and_then(|task| execute_data_task(&app, task));
            let code = result.as_ref().err().map(|error| error.code.as_str());
            let _ = app.logger.record("app-shell", &request.command, code);
            return match result { Ok(data) => Response::Ok { id:request.id,ok:true,data }, Err(error) => Response::Err { id:request.id,ok:false,error } };
        }
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
            if app.shutting_down.load(std::sync::atomic::Ordering::Acquire) { return failure("E_CANCELLED", "应用正在退出"); }
            if core.data_task_active() && request.command == "settings_set" { return failure("E_CANCELLED", "后台任务执行中，请稍后修改设置"); }
            if app.handle.webview_windows().len()>1 && (request.command=="vault_open" || (request.command=="settings_set" && request.payload["values"].get("vaultPath").is_some())) {
                let info=core.dispatch(Request{id:"shell-vault".into(),command:"vault_info".into(),payload:serde_json::json!({}),client_seq:0});
                let same=matches!(info,Response::Ok{data,..} if request.command=="vault_open" && same_vault_path(&data["vaultPath"],&request.payload["path"]));
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
        .plugin(tauri_plugin_dialog::init())
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
            let shared = Arc::new(Shell {
                shutting_down: std::sync::atomic::AtomicBool::new(false),
                scan_cancelled: core.scan_cancel_handle(),
                app: Mutex::new(core),
                logger,
                shortcut,
                handle: app.handle().clone(),
            });
            let weak = Arc::downgrade(&shared);
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
                let Some(shared) = weak.upgrade() else {
                    break;
                };
                for sync in [false, true] {
                    let prepared = match shared.app.try_lock() {
                        Ok(mut core) => {
                            if shared
                                .shutting_down
                                .load(std::sync::atomic::Ordering::Acquire)
                            {
                                break;
                            }
                            core.next_background_task(notebase_core::index::now_ms(), sync)
                        }
                        Err(_) => continue,
                    };
                    let result = prepared.and_then(|task| {
                        task.map(|task| execute_data_task(&shared, task))
                            .transpose()
                    });
                    if let Err(error) = result {
                        let _ = shared.handle.emit(
                            "notebase:error_raised",
                            serde_json::json!({"code":error.code,"message":error.message}),
                        );
                    }
                }
            });
            app.manage(shared);
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
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if let Some(shared) = window.app_handle().try_state::<Shared>() {
                    if let Some(message) = close_guard(&shared.app, &shared.scan_cancelled) {
                        api.prevent_close();
                        let _ = window.emit(
                            "notebase:error_raised",
                            serde_json::json!({"code":"E_CANCELLED","message":message}),
                        );
                    }
                }
            }
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
        .build(tauri::generate_context!())
        .expect("启动 NoteBase 失败")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                let shared = app.state::<Shared>();
                if close_guard(&shared.app, &shared.scan_cancelled).is_some() {
                    api.prevent_exit();
                    defer_windowless_exit(app, shared.inner().clone());
                } else {
                    shared
                        .shutting_down
                        .store(true, std::sync::atomic::Ordering::Release);
                }
            }
        });
}

#[cfg(test)]
mod responsiveness_tests {
    use super::*;
    #[test]
    fn app_directory_and_ancestors_are_rejected_but_note_subfolders_are_allowed() {
        let root = std::env::temp_dir().join(format!(
            "notebase-location-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir_all(root.join("app/notes")).unwrap();
        let exe = root.join("app/NoteBase.exe");
        std::fs::write(&exe, b"test fixture").unwrap();
        assert_eq!(
            validate_vault_location(root.to_str().unwrap(), &exe)
                .unwrap_err()
                .code,
            "E_PATH_INVALID"
        );
        assert!(validate_vault_location(root.join("app").to_str().unwrap(), &exe).is_err());
        assert!(validate_vault_location(root.join("app/notes").to_str().unwrap(), &exe).is_ok());
        let source = root.join("workspace/repository/notebase-starter/src-tauri");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("tauri.conf.json"),
            br#"{"identifier":"edu.notebase.desktop"}"#,
        )
        .unwrap();
        assert!(validate_vault_location(root.join("workspace").to_str().unwrap(), &exe).is_err());
        // The executable is outside workspace: this covers an installed app selecting its source folder.
        std::fs::remove_file(source.join("tauri.conf.json")).unwrap();
        for path in [
            &source,
            &root.join("workspace/repository/notebase-starter"),
            &root.join("workspace/repository"),
            &root.join("workspace"),
        ] {
            std::fs::remove_dir(path).unwrap();
        }
        // Exact, test-owned leaf files only; no recursive cleanup of a computed path.
        std::fs::remove_file(exe).unwrap();
        std::fs::remove_dir(root.join("app/notes")).unwrap();
        std::fs::remove_dir(root.join("app")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn closing_during_a_locked_operation_never_waits_on_the_ui_thread() {
        let core = Arc::new(Mutex::new(notebase_core::new_local_app()));
        let scan = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let held = core.lock().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let other = core.clone();
        let cancelled = scan.clone();
        let task = std::thread::spawn(move || {
            sender
                .send(close_guard(&other, &cancelled).is_some())
                .unwrap();
        });
        assert!(receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap());
        assert!(scan.load(std::sync::atomic::Ordering::Acquire));
        drop(held);
        task.join().unwrap();
        assert!(close_guard(&core, &scan).is_none());
    }
}
