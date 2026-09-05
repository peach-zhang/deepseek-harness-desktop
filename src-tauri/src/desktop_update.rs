//! Desktop application update checks that survive WebView document navigation.

use std::time::Duration;

use tauri::{
    window::{ProgressBarState, ProgressBarStatus},
    AppHandle, Manager,
};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::UpdaterExt;

use crate::{
    backend::{emit_status, BackendManager, BackendStatus},
    db::DesktopDb,
};

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(5);
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);
const DISMISSED_UPDATE_KEY: &str = "dismissed_desktop_update";

pub(crate) fn spawn_update_checks(app: AppHandle, backend: BackendManager) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        while matches!(
            backend.status().await.phase.as_str(),
            "starting" | "checking" | "updating"
        ) {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        loop {
            if let Err(error) = check_once(&app, &backend).await {
                log::warn!("Desktop update check failed: {error}");
            }
            tokio::time::sleep(UPDATE_CHECK_INTERVAL).await;
        }
    });
}

async fn check_once(app: &AppHandle, backend: &BackendManager) -> Result<(), String> {
    let updater = app
        .updater()
        .map_err(|error| format!("无法初始化桌面更新器：{error}"))?;
    let Some(update) = updater
        .check()
        .await
        .map_err(|error| format!("检查桌面更新失败：{error}"))?
    else {
        return Ok(());
    };

    if app
        .state::<DesktopDb>()
        .get_meta(DISMISSED_UPDATE_KEY)?
        .as_deref()
        == Some(update.version.as_str())
    {
        return Ok(());
    }

    let current_version = update.current_version.clone();
    let target_version = update.version.clone();
    let prompt = app
        .dialog()
        .message(format!(
            "发现 DSH Desktop v{target_version}，当前版本为 v{current_version}。是否立即下载并安装？"
        ))
        .title("DSH Desktop 更新")
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "立即更新".into(),
            "稍后".into(),
        ));
    let prompt = if let Some(window) = app.get_webview_window("main") {
        prompt.parent(&window)
    } else {
        prompt
    };
    let accepted = tauri::async_runtime::spawn_blocking(move || prompt.blocking_show())
        .await
        .map_err(|error| format!("更新确认窗口异常退出：{error}"))?;
    if !accepted {
        app.state::<DesktopDb>()
            .set_meta(DISMISSED_UPDATE_KEY, &target_version)?;
        return Ok(());
    }

    let window = app.get_webview_window("main");
    if let Some(window) = &window {
        let _ = window.set_progress_bar(ProgressBarState {
            status: Some(ProgressBarStatus::Indeterminate),
            progress: None,
        });
    }

    let progress_window = window.clone();
    let mut downloaded = 0_u64;
    let bytes = update
        .download(
            move |chunk_length, total| {
                downloaded = downloaded.saturating_add(chunk_length as u64);
                let Some(total) = total.filter(|total| *total > 0) else {
                    return;
                };
                let percent = downloaded
                    .saturating_mul(100)
                    .saturating_div(total)
                    .min(100);
                if let Some(window) = &progress_window {
                    let _ = window.set_progress_bar(ProgressBarState {
                        status: Some(ProgressBarStatus::Normal),
                        progress: Some(percent),
                    });
                }
            },
            || log::info!("Desktop update download completed"),
        )
        .await
        .map_err(|error| format!("下载桌面更新失败：{error}"));

    if let Some(window) = &window {
        let _ = window.set_progress_bar(ProgressBarState {
            status: Some(ProgressBarStatus::None),
            progress: None,
        });
    }

    let bytes = match bytes {
        Ok(bytes) => bytes,
        Err(error) => {
            show_error(app, &error);
            return Err(error);
        }
    };

    // On Windows the updater exits the process from install(), bypassing the
    // normal Tauri run events. Serialize against restart and keep the guard
    // through installation so no new Node child can appear after cleanup.
    let update_guard = backend.stop_for_update().await;
    let install = match tauri::async_runtime::spawn_blocking(move || update.install(bytes)).await {
        Ok(result) => result.map_err(|error| format!("安装桌面更新失败：{error}")),
        Err(error) => Err(format!("桌面更新安装任务异常退出：{error}")),
    };
    drop(update_guard);

    match install {
        Ok(()) => {
            app.request_restart();
            Ok(())
        }
        Err(error) => {
            backend.restore_bootstrap(app);
            if let Err(restart_error) = backend.start(app.clone()).await {
                let version = backend.current_version().await;
                let status = BackendStatus::failed(
                    format!("桌面更新安装失败，且 DeepSeek Harness 无法重新启动：{restart_error}"),
                    &version,
                );
                backend.set_status(status.clone()).await;
                emit_status(app, status);
                backend.restore_bootstrap(app);
                log::error!(
                    "failed to restart Harness after update installation error: {restart_error}"
                );
            }
            show_error(app, &error);
            Err(error)
        }
    }
}

fn show_error(app: &AppHandle, message: &str) {
    app.dialog()
        .message(message)
        .title("DSH Desktop 更新失败")
        .kind(MessageDialogKind::Error)
        .buttons(MessageDialogButtons::Ok)
        .show(|_| {});
}
