//! Desktop-level Tauri commands (info queries and metadata updates).
//!
//! Backend lifecycle commands live in `backend::commands`.

use tauri::{AppHandle, Emitter, Manager, State};

use crate::backend::BackendManager;
use crate::db;
use crate::window_shell;

/// Name of the child WebView that renders the version/history panel.
pub(crate) const INFO_WEBVIEW: &str = "info";
/// Local page served by the bundled frontend for the panel.
const INFO_PAGE: &str = "info.html";
/// Emitted whenever the panel opens or closes, so the bootstrap title bar can
/// keep its button in sync with the authoritative Rust-side state.
pub(crate) const INFO_EVENT: &str = "desktop-info";

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopInfo {
    pub app_version: String,
    pub harness_version: String,
    pub last_update_check: Option<String>,
    pub first_launch: Option<String>,
    pub harness_history: Vec<db::HarnessHistoryEntry>,
}

#[tauri::command]
pub(crate) async fn get_desktop_info(
    db: State<'_, db::DesktopDb>,
    manager: State<'_, BackendManager>,
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

pub(crate) fn is_info_open(app: &AppHandle) -> bool {
    app.get_webview(INFO_WEBVIEW).is_some()
}

fn emit_info_state(app: &AppHandle, open: bool) {
    if let Err(error) = app.emit(INFO_EVENT, open) {
        log::warn!("failed to emit desktop info state: {error}");
    }
}

/// Closes the panel when it is open and reports whether it is gone afterwards.
///
/// Used by [`toggle_desktop_info`] and when the window layout is restored to
/// the bootstrap page, where the panel has no meaning.
pub(crate) fn close_info_panel(app: &AppHandle) -> bool {
    let Some(webview) = app.get_webview(INFO_WEBVIEW) else {
        return true;
    };
    match webview.close() {
        Ok(()) => {
            emit_info_state(app, false);
            true
        }
        Err(error) => {
            log::warn!("无法关闭版本信息面板：{error}");
            false
        }
    }
}

/// Opens or closes the version/history panel.
///
/// The panel is a child WebView rather than DOM inside the bootstrap page: once
/// the Harness page is showing, the bootstrap WebView is clipped to the title
/// bar, so a document-level popup could never render over the Harness UI.
///
/// This command is `async` on purpose. Tauri documents that creating a WebView
/// from a *synchronous* command deadlocks on Windows (WebView2 issue), so the
/// builder must never run on the main thread's synchronous path.
///
/// Returns `true` when the panel is open after the call.
#[tauri::command]
pub(crate) async fn toggle_desktop_info(app: AppHandle) -> Result<bool, String> {
    // A panel that cannot be closed must not be re-created under the same
    // label; surface the failure instead.
    if !close_info_panel(&app) {
        return Err("无法关闭版本信息面板。".to_owned());
    }
    if app.get_webview(INFO_WEBVIEW).is_some() {
        // It was already open and has now been closed.
        return Ok(false);
    }

    let window = app
        .get_window("main")
        .ok_or_else(|| "无法定位主窗口。".to_owned())?;
    let size = window
        .inner_size()
        .map_err(|error| format!("无法读取窗口尺寸：{error}"))?
        .to_logical::<f64>(window.scale_factor().map_err(|error| error.to_string())?);
    let bounds = window_shell::info_panel_bounds(size);
    // `WebviewUrl::App` resolves the path against whichever origin serves the
    // bundled frontend (the dev server or the packaged assets), so the panel
    // works identically in development and in a release build.
    let url = tauri::WebviewUrl::App(INFO_PAGE.into());

    let builder = tauri::webview::WebviewBuilder::new(INFO_WEBVIEW, url);
    let webview = window
        .add_child(builder, bounds.position, bounds.size)
        .map_err(|error| format!("无法打开版本信息面板：{error}"))?;
    // 窗口可能在创建期间缩放，用最新尺寸再同步一次。
    window_shell::resize_webviews(&window).map_err(|error| error.to_string())?;
    let _ = webview.set_focus();
    emit_info_state(&app, true);
    Ok(true)
}

/// Re-creates the panel so it stacks above a freshly created Harness WebView.
///
/// Native child WebViews are z-ordered by creation time, so a panel opened
/// before the Harness page would end up hidden behind it. Re-creating the panel
/// after the Harness WebView exists is the portable way to bring it back to the
/// front; the panel is stateless, so the cost is one local page load.
///
/// Runs on the async runtime because it creates a WebView, which must not
/// happen on a synchronous path on Windows.
pub(crate) fn reopen_info_panel_above_harness(app: &AppHandle) {
    if !is_info_open(app) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        close_info_panel(&app);
        if let Err(error) = toggle_desktop_info(app.clone()).await {
            log::warn!("failed to restore version panel above Harness: {error}");
        }
    });
}
