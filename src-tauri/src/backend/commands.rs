use tauri::{AppHandle, State};

use super::status::BackendStatus;
use super::{emit_status, BackendManager};

#[tauri::command]
pub(crate) async fn backend_status(
    manager: State<'_, BackendManager>,
) -> Result<BackendStatus, String> {
    Ok(manager.status().await)
}

#[tauri::command]
pub(crate) async fn restart_backend(
    app: AppHandle,
    manager: State<'_, BackendManager>,
) -> Result<BackendStatus, String> {
    match manager.start(app.clone()).await {
        Ok(status) => Ok(status),
        Err(error) => {
            let version = manager.current_version().await;
            let status = BackendStatus::failed(error.clone(), &version);
            manager.set_status(status.clone()).await;
            emit_status(&app, status);
            Err(error)
        }
    }
}

/// Stops the Harness backend before a desktop app update is installed.
/// `BackendManager::stop` terminates only the process tree started by this app,
/// so unrelated Node.js workloads on the machine are never affected.
#[tauri::command]
pub(crate) async fn prepare_for_update(manager: State<'_, BackendManager>) -> Result<(), String> {
    manager.stop().await;
    Ok(())
}
