pub(crate) mod commands;
mod status;

pub(crate) use status::BackendStatus;

use std::{collections::VecDeque, fs, sync::Arc, time::Duration};

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_shell::{
    process::{CommandChild, CommandEvent},
    ShellExt,
};
use url::Url;

use crate::update::{self, UpdateNotice};
use crate::{HARNESS_VERSION, MAX_DIAGNOSTIC_LINES};

struct BackendRuntime {
    child: Option<CommandChild>,
    generation: u64,
    status: BackendStatus,
    diagnostics: VecDeque<String>,
    harness_version: String,
}

impl Default for BackendRuntime {
    fn default() -> Self {
        Self {
            child: None,
            generation: 0,
            status: BackendStatus::starting(HARNESS_VERSION),
            diagnostics: VecDeque::new(),
            harness_version: HARNESS_VERSION.into(),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct BackendManager {
    inner: Arc<tauri::async_runtime::Mutex<BackendRuntime>>,
    start_lock: Arc<tauri::async_runtime::Mutex<()>>,
}

impl BackendManager {
    pub(crate) async fn status(&self) -> BackendStatus {
        self.inner.lock().await.status.clone()
    }

    pub(crate) async fn current_version(&self) -> String {
        self.inner.lock().await.harness_version.clone()
    }

    pub(crate) async fn set_status(&self, status: BackendStatus) {
        self.inner.lock().await.status = status;
    }

    pub(crate) async fn stop(&self) {
        let mut runtime = self.inner.lock().await;
        runtime.generation = runtime.generation.wrapping_add(1);
        if let Some(child) = runtime.child.take() {
            stop_child(child);
        }
    }

    pub(crate) async fn start(&self, app: AppHandle) -> Result<BackendStatus, String> {
        // Serialize whole start cycles: update installs swap runtime directories
        // and must not race a concurrent restart.
        let _start_guard = self.start_lock.lock().await;
        self.stop().await;

        let resource_dir = app
            .path()
            .resource_dir()
            .map_err(|error| format!("无法定位应用资源目录：{error}"))?;
        let data_dir = app
            .path()
            .app_data_dir()
            .map_err(|error| format!("无法定位应用数据目录：{error}"))?;
        fs::create_dir_all(&data_dir).map_err(|error| format!("无法创建应用数据目录：{error}"))?;
        let working_dir = app.path().home_dir().unwrap_or_else(|_| data_dir.clone());

        let generation = {
            let mut runtime = self.inner.lock().await;
            runtime.generation = runtime.generation.wrapping_add(1);
            runtime.status = BackendStatus::starting(HARNESS_VERSION);
            runtime.diagnostics.clear();
            runtime.generation
        };

        emit_status(&app, BackendStatus::starting(HARNESS_VERSION));

        // Resolve (and, when a newer release exists, install) the Harness
        // runtime. Blocking network/disk work runs off the async runtime; the
        // bundled runtime remains the fallback on any update failure.
        let selection = {
            let app_for_update = app.clone();
            let resource_dir = resource_dir.clone();
            let data_dir = data_dir.clone();
            tauri::async_runtime::spawn_blocking(move || {
                update::select_harness_runtime(
                    &resource_dir,
                    &data_dir,
                    HARNESS_VERSION,
                    &mut |notice| {
                        let status = match notice {
                            UpdateNotice::Checking { current } => {
                                BackendStatus::checking_update(&current)
                            }
                            UpdateNotice::Staging { stage, target } => {
                                BackendStatus::updating_stage(stage, &target)
                            }
                            UpdateNotice::Updating { target } => BackendStatus::updating(&target),
                        };
                        emit_status(&app_for_update, status);
                    },
                )
            })
            .await
            .map_err(|error| format!("运行时准备任务中断：{error}"))??
        };

        let entry = selection.entry;
        let selected_version = selection.version;

        {
            let mut runtime = self.inner.lock().await;
            if runtime.generation != generation {
                return Ok(runtime.status.clone());
            }
            runtime.harness_version = selected_version.clone();
            let status = BackendStatus::starting(&selected_version);
            runtime.status = status.clone();
            drop(runtime);
            emit_status(&app, status);
        }

        let dsh_home = data_dir.join("harness");
        let agents_home = data_dir.join("agents");
        fs::create_dir_all(&dsh_home)
            .and_then(|_| fs::create_dir_all(&agents_home))
            .map_err(|error| format!("无法准备 Harness 数据目录：{error}"))?;

        // Install the registry plugins listed by the desktop JSON manifest
        // before the Harness web profile boots. Best-effort: an unavailable
        // registry must not prevent the desktop from starting.
        if let Err(error) = crate::plugins::sync_configured_plugins(
            &app,
            &resource_dir,
            &dsh_home,
            &working_dir,
            &entry,
        )
        .await
        {
            log::warn!("configured plugin installation failed: {error}");
        }

        let command = app
            .shell()
            .sidecar("node")
            .map_err(|error| format!("无法定位内置 Node.js：{error}"))?
            .args([
                entry.to_string_lossy().into_owned(),
                "web".into(),
                "--host".into(),
                "127.0.0.1".into(),
                "--port".into(),
                "0".into(),
                // The `dsh web` command opens the default browser by default;
                // the desktop shell already renders the UI in its own webview,
                // so suppress the automatic browser launch.
                "--no-open".into(),
            ])
            .env("DSH_HOME", dsh_home)
            .env("DSH_AGENTS_HOME", agents_home)
            .env("DSH_TELEMETRY_DISABLED", "1")
            .current_dir(working_dir);

        let (mut events, child) = command
            .spawn()
            .map_err(|error| format!("无法启动内置 Harness：{error}"))?;

        {
            let mut runtime = self.inner.lock().await;
            if runtime.generation != generation {
                stop_child(child);
                return Ok(runtime.status.clone());
            }
            runtime.child = Some(child);
        }

        let timeout_manager = self.clone();
        let timeout_app = app.clone();
        let timeout_version = selected_version.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(45)).await;
            let mut runtime = timeout_manager.inner.lock().await;
            if runtime.generation != generation || runtime.status.phase != "starting" {
                return;
            }
            if let Some(child) = runtime.child.take() {
                stop_child(child);
            }
            let status = BackendStatus::failed(
                "DeepSeek Harness 启动超时，未在 45 秒内报告就绪。",
                &timeout_version,
            );
            runtime.status = status.clone();
            drop(runtime);
            if timeout_version != HARNESS_VERSION {
                update::mark_runtime_failed(&timeout_app, &timeout_version);
            }
            emit_status(&timeout_app, status);
        });

        let manager = self.clone();
        let app_for_events = app.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(event) = events.recv().await {
                match event {
                    CommandEvent::Stdout(bytes) => {
                        let line = String::from_utf8_lossy(&bytes).trim().to_owned();
                        log::info!(target: "dsh", "{line}");
                        if let Some(url) = readiness_url(&line) {
                            let mut runtime = manager.inner.lock().await;
                            let status =
                                BackendStatus::running(url, &runtime.harness_version.clone());
                            if runtime.generation == generation {
                                runtime.status = status.clone();
                                drop(runtime);
                                emit_status(&app_for_events, status);
                            }
                        }
                    }
                    CommandEvent::Stderr(bytes) => {
                        let line = String::from_utf8_lossy(&bytes).trim().to_owned();
                        log::warn!(target: "dsh", "{line}");
                        let mut runtime = manager.inner.lock().await;
                        if runtime.generation == generation && !line.is_empty() {
                            if runtime.diagnostics.len() == MAX_DIAGNOSTIC_LINES {
                                runtime.diagnostics.pop_front();
                            }
                            runtime.diagnostics.push_back(line);
                        }
                    }
                    CommandEvent::Terminated(payload) => {
                        let mut runtime = manager.inner.lock().await;
                        if runtime.generation != generation {
                            break;
                        }
                        runtime.child = None;
                        let detail = runtime
                            .diagnostics
                            .iter()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" · ");
                        let suffix = payload.code.map_or_else(
                            || "进程已退出".to_owned(),
                            |code| format!("进程退出码 {code}"),
                        );
                        let message = if detail.is_empty() {
                            format!("DeepSeek Harness 意外停止（{suffix}）。")
                        } else {
                            format!("DeepSeek Harness 意外停止（{suffix}）：{detail}")
                        };
                        let failed_version = runtime.harness_version.clone();
                        let status = BackendStatus::failed(message, &failed_version);
                        runtime.status = status.clone();
                        drop(runtime);
                        if failed_version != HARNESS_VERSION {
                            update::mark_runtime_failed(&app_for_events, &failed_version);
                        }
                        emit_status(&app_for_events, status);
                        break;
                    }
                    _ => {}
                }
            }
        });

        Ok(self.status().await)
    }
}

fn readiness_url(line: &str) -> Option<String> {
    let candidate = line.strip_prefix("dsh web: ")?.split_whitespace().next()?;
    let parsed = Url::parse(candidate).ok()?;
    if parsed.scheme() != "http"
        || parsed.host_str() != Some("127.0.0.1")
        || parsed.port().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
    {
        return None;
    }
    Some(parsed.to_string())
}

fn stop_child(child: CommandChild) {
    let pid = child.pid();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let status = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        if status.is_ok_and(|status| status.success()) {
            return;
        }
    }
    if let Err(error) = child.kill() {
        log::warn!("failed to stop Harness sidecar {pid}: {error}");
    }
}

pub(crate) fn emit_status(app: &AppHandle, status: BackendStatus) {
    if let Err(error) = app.emit("backend-status", status) {
        log::warn!("failed to emit backend status: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_loopback_readiness_line() {
        assert_eq!(
            readiness_url("dsh web: http://127.0.0.1:49152"),
            Some("http://127.0.0.1:49152/".into())
        );
    }

    #[test]
    fn rejects_non_loopback_readiness_line() {
        assert_eq!(readiness_url("dsh web: http://localhost:3080"), None);
        assert_eq!(readiness_url("dsh web: https://127.0.0.1:3080"), None);
        assert_eq!(readiness_url("dsh web: http://example.com:3080"), None);
    }
}
