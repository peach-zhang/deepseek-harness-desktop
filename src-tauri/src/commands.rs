//! Desktop-level Tauri commands (info queries and metadata updates).
//!
//! Backend lifecycle commands live in `backend::commands`.

use tauri::State;

use crate::backend::BackendManager;
use crate::db;

#[derive(serde::Serialize)]
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

#[tauri::command]
pub(crate) fn set_update_check_time(db: State<'_, db::DesktopDb>) -> Result<(), String> {
    db.set_meta("last_update_check", &db::now_iso())
}
