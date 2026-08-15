use tauri::{AppHandle, State};

use super::{BackendManager, emit_status};
use super::status::BackendStatus;

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
