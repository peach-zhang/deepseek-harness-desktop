//! 系统托盘:关闭主窗口只是隐藏驻留,由托盘图标负责唤回与真正退出。
//!
//! 后台 Harness(本地 Node.js 服务)在窗口隐藏期间保持运行,托盘成为
//! 唯一的恢复入口之一;另一个入口是再次启动应用时的单实例回调。

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

pub(crate) fn setup(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let show = MenuItem::with_id(app, "tray-show", "显示主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "tray-quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    let tray = TrayIconBuilder::with_id("dsh-tray")
        .icon(
            app.default_window_icon()
                .ok_or_else(|| "缺少应用图标,无法创建托盘".to_owned())?
                .clone(),
        )
        .tooltip("DSH Desktop")
        .menu(&menu)
        // 左键直接唤起窗口,菜单只在右键时出现。
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "tray-show" => show_main_window(app),
            "tray-quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    // TrayIcon 被回收时会连同托盘图标一起移除,交给应用状态保管生命周期。
    app.manage(tray);
    Ok(())
}

pub(crate) fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_window("main") {
        // 窗口可能在隐藏前处于最小化状态,先还原再显示聚焦。
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// 首次因关闭而驻留时,用系统通知告知窗口去向,之后不再打扰。
pub(crate) fn notify_hidden_once(app: &AppHandle) {
    use tauri_plugin_notification::NotificationExt;

    const HINT_KEY: &str = "tray_hint_shown";
    let db = app.state::<crate::db::DesktopDb>();
    if db.get_meta(HINT_KEY).ok().flatten().as_deref() == Some("1") {
        return;
    }
    let _ = db.set_meta(HINT_KEY, "1");
    let _ = app
        .notification()
        .builder()
        .title("DSH Desktop 仍在运行")
        .body("窗口已最小化到系统托盘，点击托盘图标可随时恢复。")
        .show();
}
