use tauri::{AppHandle, State};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

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

/// Stops the Harness backend and kills any orphaned Node.js processes before
/// a desktop app update is installed.
///
/// On Windows the NSIS installer needs to replace `node.exe` (the bundled
/// sidecar), which is locked while the process is alive. Killing the backend
/// and any orphaned node.exe processes ensures those file handles are
/// released so the installer can succeed.
///
/// On macOS / Unix the OS uses inode-based semantics: a running binary can be
/// replaced without affecting the executing process, so only the backend
/// needs to be stopped — no blanket process kill is necessary.
#[tauri::command]
pub(crate) async fn prepare_for_update(
    manager: State<'_, BackendManager>,
) -> Result<(), String> {
    // Stop the running Harness backend.
    manager.stop().await;

    // On Windows, kill any remaining node.exe processes that might still hold
    // file locks. This covers orphaned children from a previous backend that
    // didn't exit cleanly, or stale processes from an earlier crash.
    #[cfg(windows)]
    {
        // Give the process a moment to release file handles after stop().
        std::thread::sleep(std::time::Duration::from_millis(300));

        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/IM", "node.exe"])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output();

        // Brief pause to let the OS fully release file handles.
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    Ok(())
}
