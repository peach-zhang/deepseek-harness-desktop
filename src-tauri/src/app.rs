//! Tauri application builder — setup, window creation, and run loop.

use tauri::webview::WebviewBuilder;
use tauri::{LogicalPosition, LogicalSize, Manager, RunEvent, WindowBuilder, WindowEvent};

use crate::backend::{self, BackendManager};
use crate::commands;
use crate::db;
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
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .manage(manager.clone())
        .invoke_handler(tauri::generate_handler![
            backend::commands::backend_status,
            backend::commands::restart_backend,
            commands::get_desktop_info,
            commands::check_harness_update,
            commands::toggle_desktop_info,
            theme::get_harness_theme,
        ])
        .setup(move |app| {
            // 常驻本地壳层负责标题栏；Harness 在独立子 WebView 中加载，
            // 不再通过顶层导航替换壳层，也不再启用系统装饰。
            let window_config = app
                .config()
                .app
                .windows
                .first()
                .ok_or_else(|| "主窗口配置缺失".to_owned())?;
            let navigation_manager = manager_for_setup.clone();
            let page_load_manager = manager_for_setup.clone();
            let window = WindowBuilder::from_config(app.handle(), window_config)?.build()?;
            let size = window.inner_size()?.to_logical::<f64>(window.scale_factor()?);
            let bootstrap = window.add_child(
                WebviewBuilder::new("main", window_config.url.clone())
                    .on_navigation(move |url| navigation_manager.allows_bootstrap_navigation(url))
                    .on_page_load(move |_webview, payload| {
                        page_load_manager.capture_bootstrap_url(payload.url());
                    }),
                LogicalPosition::new(0.0, 0.0),
                LogicalSize::new(size.width, size.height),
            )?;
            if let Ok(url) = bootstrap.url() {
                manager_for_setup.capture_bootstrap_url(&url);
            }
            let layout_window = window.clone();
            window.on_window_event(move |event| {
                match event {
                    // 关闭只隐藏窗口:内置 Harness 继续在后台运行,由托盘
                    // 图标或再次启动应用唤回;真正退出走托盘菜单。
                    WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        let _ = layout_window.hide();
                    }
                    WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                        if let Err(error) = crate::window_shell::resize_webviews(&layout_window) {
                            log::warn!("无法调整窗口内容区域：{error}");
                        }
                    }
                    _ => {}
                }
            });

            // 托盘在主窗口创建之后注册,保证隐藏驻留期间有恢复入口。
            crate::tray::setup(app.handle())?;

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

            // 桌面更新检查与原生提示继续由 Rust 管理。
            crate::desktop_update::spawn_update_checks(
                app.handle().clone(),
                manager_for_setup.clone(),
            );

            // Watch the Harness settings file and forward theme changes so the
            // bootstrap titlebar matches the configured Harness appearance.
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
                        backend.restore_bootstrap(&handle);
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
