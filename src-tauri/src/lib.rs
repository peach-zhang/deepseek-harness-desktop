mod backend;
mod db;
mod plugins;
mod runtime;
mod theme;
mod update;

use tauri::{Manager, State};

pub(crate) const HARNESS_VERSION: &str = "0.1.0-rc.6";
pub(crate) const MAX_DIAGNOSTIC_LINES: usize = 12;

// ── Desktop info Tauri commands ─

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopInfo {
    app_version: String,
    harness_version: String,
    last_update_check: Option<String>,
    first_launch: Option<String>,
    harness_history: Vec<db::HarnessHistoryEntry>,
}

#[tauri::command]
async fn get_desktop_info(
    db: State<'_, db::DesktopDb>,
    manager: State<'_, backend::BackendManager>,
) -> Result<DesktopInfo, String> {
    let history = db.harness_history()?;
    let last_update_check = db.get_meta("last_update_check")?;
    let first_launch = db.get_meta("first_launch")?;
    Ok(DesktopInfo {
        app_version: env!("CARGO_PKG_VERSION").into(),
        harness_version: manager.current_version().await,
        last_update_check,
        first_launch,
        harness_history: history,
    })
}

#[tauri::command]
fn set_update_check_time(db: State<'_, db::DesktopDb>) -> Result<(), String> {
    db.set_meta("last_update_check", &db::now_iso())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let manager = backend::BackendManager::default();
    let manager_for_setup = manager.clone();

    let app = tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                // The update HTTP client is very chatty at dev log levels.
                .level_for("ureq", log::LevelFilter::Warn)
                .level_for("ureq_proto", log::LevelFilter::Warn)
                .build(),
        )
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .manage(manager.clone())
        .invoke_handler(tauri::generate_handler![
            backend::commands::backend_status,
            backend::commands::restart_backend,
            get_desktop_info,
            set_update_check_time,
            theme::get_harness_theme,
        ])
        .setup(move |app| {
            // Initialize SQLite database
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|error| format!("无法定位应用数据目录：{error}"))?;
            let db = db::DesktopDb::open(&data_dir)?;
            // Record bundled version before handing ownership to Tauri
            let _ = db.record_harness_version(HARNESS_VERSION, "bundled");
            if db.get_meta("first_launch").ok().flatten().is_none() {
                let _ = db.set_meta("first_launch", &db::now_iso());
            }
            app.manage(db);

            // Watch the Harness settings file and forward theme changes so the
            // custom titlebar can match the iframe's appearance.
            theme::spawn_theme_watcher(app.handle().clone());

            #[cfg(target_os = "macos")]
            {
                use tauri::menu::{MenuBuilder, SubmenuBuilder};

                let app_menu = SubmenuBuilder::new(app, "DSH Desktop")
                    .about(None)
                    .separator()
                    .hide()
                    .hide_others()
                    .show_all()
                    .separator()
                    .quit()
                    .build()?;
                let edit_menu = SubmenuBuilder::new(app, "Edit")
                    .undo()
                    .redo()
                    .separator()
                    .cut()
                    .copy()
                    .paste()
                    .select_all()
                    .build()?;
                let menu = MenuBuilder::new(app)
                    .item(&app_menu)
                    .item(&edit_menu)
                    .build()?;
                app.set_menu(menu)?;
            }

            let handle = app.handle().clone();
            let backend = manager_for_setup.clone();
            tauri::async_runtime::spawn(async move {
                match backend.start(handle.clone()).await {
                    Ok(status) => {
                        // The start cycle ran the registry update check; record
                        // when it happened and which version ended up running.
                        let db = handle.state::<db::DesktopDb>();
                        let source = if status.harness_version == HARNESS_VERSION {
                            "bundled"
                        } else {
                            "update"
                        };
                        let _ = db.set_meta("last_update_check", &db::now_iso());
                        let _ = db.record_harness_version(&status.harness_version, source);
                    }
                    Err(error) => {
                        let version = backend.current_version().await;
                        let status = backend::BackendStatus::failed(error, &version);
                        backend.set_status(status.clone()).await;
                        backend::emit_status(&handle, status);
                    }
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build DSH Desktop");

    app.run(move |_handle, event| {
        if matches!(
            event,
            tauri::RunEvent::Exit | tauri::RunEvent::ExitRequested { .. }
        ) {
            tauri::async_runtime::block_on(manager.stop());
        }
    });
}
