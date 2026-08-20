//! Tauri application builder — setup, window creation, and run loop.

use tauri::{Manager, RunEvent};
use tauri::webview::{DownloadEvent, WebviewWindowBuilder};

use crate::backend::{self, BackendManager};
use crate::commands;
use crate::db;
use crate::platform::open_containing_folder;
use crate::theme;
use crate::HARNESS_VERSION;

pub fn run() {
    let manager = BackendManager::default();
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
            backend::commands::prepare_for_update,
            commands::get_desktop_info,
            commands::set_update_check_time,
            theme::get_harness_theme,
        ])
        .setup(move |app| {
            // Manually create the main window with a download handler so files
            // saved from the Harness iframe automatically reveal in Explorer /
            // Finder. The window is marked `"create": false` in tauri.conf.json
            // to prevent Tauri from building it before this handler is attached.
            let window_config = app
                .config()
                .app
                .windows
                .first()
                .ok_or_else(|| "主窗口配置缺失".to_owned())?;
            let _webview_window =
                WebviewWindowBuilder::from_config(app.handle(), window_config)?
                    .on_download(|_webview, event| match event {
                        DownloadEvent::Requested { .. } => {
                            // Allow the download to proceed with its default path.
                            true
                        }
                        DownloadEvent::Finished {
                            url: _,
                            path,
                            success,
                        } => {
                            if let Some(file_path) = path {
                                if success {
                                    open_containing_folder(&file_path);
                                }
                            }
                            true
                        }
                        _ => true,
                    })
                    .build()?;

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
                        let source =
                            if status.harness_version == HARNESS_VERSION {
                                "bundled"
                            } else {
                                "update"
                            };
                        let _ =
                            db.set_meta("last_update_check", &db::now_iso());
                        let _ = db.record_harness_version(
                            &status.harness_version,
                            source,
                        );
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
        if matches!(event, RunEvent::Exit | RunEvent::ExitRequested { .. }) {
            tauri::async_runtime::block_on(manager.stop());
        }
    });
}
