pub(crate) mod commands;
mod status;

pub(crate) use status::BackendStatus;

use std::{
    collections::VecDeque,
    fs,
    sync::{Arc, Mutex},
    time::Duration,
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest, Sha256};
use tauri::{webview::WebviewWindow, AppHandle, Emitter, Manager};
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
    navigating: bool,
    status: BackendStatus,
    diagnostics: VecDeque<String>,
    harness_version: String,
}

#[derive(Clone)]
struct HarnessNavigation {
    host: String,
    port: u16,
    generation: u64,
    requires_cookie: bool,
}

impl HarnessNavigation {
    fn contains(&self, url: &Url) -> bool {
        url.scheme() == "http"
            && url.host_str() == Some(self.host.as_str())
            && url.port() == Some(self.port)
    }

    fn is_clean_root(&self, url: &Url) -> bool {
        self.contains(url) && url.path() == "/" && url.query().is_none() && url.fragment().is_none()
    }
}

impl Default for BackendRuntime {
    fn default() -> Self {
        Self {
            child: None,
            generation: 0,
            navigating: false,
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
    bootstrap_url: Arc<Mutex<Option<Url>>>,
    harness_origin: Arc<Mutex<Option<HarnessNavigation>>>,
}

impl BackendManager {
    pub(crate) fn capture_bootstrap_url(&self, url: &Url) {
        if !is_bootstrap_url(url) {
            return;
        }
        if let Ok(mut bootstrap_url) = self.bootstrap_url.lock() {
            *bootstrap_url = Some(url.clone());
        }
    }

    fn bootstrap_url(&self) -> Option<Url> {
        self.bootstrap_url.lock().ok()?.clone()
    }

    pub(crate) fn allows_navigation(&self, url: &Url) -> bool {
        if self
            .bootstrap_url()
            .is_some_and(|bootstrap| bootstrap.origin() == url.origin())
            || is_bootstrap_url(url)
        {
            return true;
        }

        self.harness_origin
            .lock()
            .ok()
            .and_then(|navigation| navigation.clone())
            .is_some_and(|navigation| navigation.contains(url))
    }

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
        runtime.navigating = false;
        if let Some(child) = runtime.child.take() {
            stop_child(child);
        }
    }

    pub(crate) async fn stop_for_update(&self) -> tokio::sync::OwnedMutexGuard<()> {
        let guard = self.start_lock.clone().lock_owned().await;
        self.stop().await;
        guard
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
            runtime.navigating = false;
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
            runtime.generation = runtime.generation.wrapping_add(1);
            let navigation_started = runtime.navigating;
            runtime.navigating = false;
            if let Some(child) = runtime.child.take() {
                stop_child(child);
            }
            let message = if navigation_started {
                "DeepSeek Harness 页面认证超时，未能建立有效的浏览器会话。"
            } else {
                "DeepSeek Harness 启动超时，未在 45 秒内报告就绪。"
            };
            let status = BackendStatus::failed(message, &timeout_version);
            runtime.status = status.clone();
            drop(runtime);
            if !navigation_started && timeout_version != HARNESS_VERSION {
                update::mark_runtime_failed(&timeout_app, &timeout_version);
            }
            emit_status(&timeout_app, status);
            timeout_manager.restore_bootstrap(&timeout_app);
        });

        let manager = self.clone();
        let app_for_events = app.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(event) = events.recv().await {
                match event {
                    CommandEvent::Stdout(bytes) => {
                        let output = String::from_utf8_lossy(&bytes);
                        for line in output
                            .lines()
                            .map(str::trim)
                            .filter(|line| !line.is_empty())
                        {
                            let ready_url = readiness_url(line);
                            log::info!(target: "dsh", "{}", safe_stdout_line(line, ready_url.as_ref()));
                            let Some(url) = ready_url else {
                                continue;
                            };

                            {
                                let mut runtime = manager.inner.lock().await;
                                if runtime.generation != generation || runtime.navigating {
                                    continue;
                                }
                                runtime.navigating = true;
                            }

                            if let Err(error) =
                                manager.show_harness(&app_for_events, generation, url)
                            {
                                let (status, child) = {
                                    let mut runtime = manager.inner.lock().await;
                                    if runtime.generation != generation {
                                        continue;
                                    }
                                    runtime.generation = runtime.generation.wrapping_add(1);
                                    runtime.navigating = false;
                                    let child = runtime.child.take();
                                    let status =
                                        BackendStatus::failed(error, &runtime.harness_version);
                                    runtime.status = status.clone();
                                    (status, child)
                                };
                                if let Some(child) = child {
                                    stop_child(child);
                                }
                                emit_status(&app_for_events, status);
                                manager.restore_bootstrap(&app_for_events);
                                break;
                            }
                        }
                    }
                    CommandEvent::Stderr(bytes) => {
                        let output = String::from_utf8_lossy(&bytes);
                        for line in output
                            .lines()
                            .map(str::trim)
                            .filter(|line| !line.is_empty())
                        {
                            let ready_url = readiness_url(line);
                            let safe_line = safe_stdout_line(line, ready_url.as_ref());
                            log::warn!(target: "dsh", "{safe_line}");
                            let mut runtime = manager.inner.lock().await;
                            if runtime.generation == generation {
                                if runtime.diagnostics.len() == MAX_DIAGNOSTIC_LINES {
                                    runtime.diagnostics.pop_front();
                                }
                                runtime.diagnostics.push_back(safe_line);
                            }
                        }
                    }
                    CommandEvent::Terminated(payload) => {
                        let mut runtime = manager.inner.lock().await;
                        if runtime.generation != generation {
                            break;
                        }
                        runtime.generation = runtime.generation.wrapping_add(1);
                        runtime.navigating = false;
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
                        manager.restore_bootstrap(&app_for_events);
                        break;
                    }
                    _ => {}
                }
            }
        });

        Ok(self.status().await)
    }

    fn show_harness(&self, app: &AppHandle, generation: u64, url: Url) -> Result<(), String> {
        let window = app
            .get_webview_window("main")
            .ok_or_else(|| "无法定位主窗口。".to_owned())?;
        let navigation = HarnessNavigation {
            host: url.host_str().unwrap_or("127.0.0.1").to_owned(),
            port: url
                .port()
                .ok_or_else(|| "Harness URL 缺少端口。".to_owned())?,
            generation,
            requires_cookie: url.query().is_some(),
        };
        *self
            .harness_origin
            .lock()
            .map_err(|_| "Harness origin 锁中毒".to_owned())? = Some(navigation);
        if let Err(error) = window.set_decorations(true) {
            if let Ok(mut origin) = self.harness_origin.lock() {
                *origin = None;
            }
            return Err(format!("无法启用系统标题栏：{error}"));
        }
        if let Err(error) = window.navigate(url) {
            if let Ok(mut origin) = self.harness_origin.lock() {
                *origin = None;
            }
            let _ = window.set_decorations(false);
            return Err(format!("无法打开 DeepSeek Harness：{error}"));
        }
        Ok(())
    }

    pub(crate) fn handle_page_load(&self, app: &AppHandle, window: &WebviewWindow, url: &Url) {
        self.capture_bootstrap_url(url);
        let Some(navigation) = self
            .harness_origin
            .lock()
            .ok()
            .and_then(|navigation| navigation.clone())
        else {
            return;
        };
        if !navigation.is_clean_root(url) {
            return;
        }
        let manager = self.clone();
        let app = app.clone();
        let window = window.clone();
        let url = url.clone();
        tauri::async_runtime::spawn(async move {
            if navigation.requires_cookie {
                let mut authenticated = false;
                for attempt in 0..20 {
                    if has_dsh_auth_cookie(&window, &url) {
                        authenticated = true;
                        break;
                    }
                    if attempt < 19 {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
                if !authenticated {
                    log::warn!("Harness root loaded without its authenticated browser cookie");
                    let (status, child) = {
                        let mut runtime = manager.inner.lock().await;
                        if runtime.generation != navigation.generation || !runtime.navigating {
                            return;
                        }
                        runtime.generation = runtime.generation.wrapping_add(1);
                        runtime.navigating = false;
                        let child = runtime.child.take();
                        let status = BackendStatus::failed(
                            "DeepSeek Harness 页面认证失败，未能建立有效的浏览器会话。",
                            &runtime.harness_version,
                        );
                        runtime.status = status.clone();
                        (status, child)
                    };
                    if let Some(child) = child {
                        stop_child(child);
                    }
                    emit_status(&app, status);
                    manager.restore_bootstrap(&app);
                    return;
                }
            }

            let status = {
                let mut runtime = manager.inner.lock().await;
                if runtime.generation != navigation.generation || !runtime.navigating {
                    return;
                }
                runtime.navigating = false;
                let status = BackendStatus::running(&runtime.harness_version);
                runtime.status = status.clone();
                status
            };
            emit_status(&app, status);
        });
    }

    pub(crate) fn restore_bootstrap(&self, app: &AppHandle) {
        if let Ok(mut origin) = self.harness_origin.lock() {
            *origin = None;
        }
        let Some(url) = self.bootstrap_url() else {
            log::warn!("cannot restore bootstrap because its URL was not recorded");
            return;
        };
        let Some(window) = app.get_webview_window("main") else {
            log::warn!("cannot restore bootstrap because the main window is missing");
            return;
        };
        if let Err(error) = window.set_decorations(false) {
            log::warn!("failed to restore bootstrap window decorations: {error}");
        }
        if let Err(error) = window.navigate(url) {
            log::warn!("failed to restore bootstrap page: {error}");
        }
    }
}

fn dsh_auth_cookie_name(url: &Url) -> Option<String> {
    let authority = format!("{}:{}", url.host_str()?, url.port()?);
    let digest = Sha256::digest(authority.as_bytes());
    Some(format!("dsh-auth-{}", URL_SAFE_NO_PAD.encode(digest)))
}

fn has_dsh_auth_cookie(window: &WebviewWindow, url: &Url) -> bool {
    let Some(expected_name) = dsh_auth_cookie_name(url) else {
        return false;
    };
    window.cookies_for_url(url.clone()).is_ok_and(|cookies| {
        cookies
            .iter()
            .any(|cookie| cookie.name() == expected_name.as_str())
    })
}

fn is_bootstrap_url(url: &Url) -> bool {
    matches!(
        (url.scheme(), url.host_str()),
        ("tauri", Some("localhost")) | ("http" | "https", Some("tauri.localhost"))
    ) || (cfg!(debug_assertions)
        && url.scheme() == "http"
        && url.host_str() == Some("localhost")
        && url.port() == Some(1420))
}

fn readiness_url(line: &str) -> Option<Url> {
    let candidate = line.strip_prefix("dsh web: ")?.split_whitespace().next()?;
    let parsed = Url::parse(candidate).ok()?;
    if parsed.scheme() != "http"
        || parsed.host_str() != Some("127.0.0.1")
        || parsed.port().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.fragment().is_some()
    {
        return None;
    }

    let query = parsed.query_pairs().collect::<Vec<_>>();
    if !query.is_empty()
        && (query.len() != 1
            || query[0].0 != "token"
            || query[0].1.is_empty()
            || !query[0]
                .1
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'))
    {
        return None;
    }
    Some(parsed)
}

fn safe_stdout_line(line: &str, ready_url: Option<&Url>) -> String {
    if let Some(url) = ready_url {
        let credential = if url.query().is_some() {
            "?token=[REDACTED]"
        } else {
            ""
        };
        return format!(
            "dsh web: {}://{}:{}/{}",
            url.scheme(),
            url.host_str().unwrap_or("127.0.0.1"),
            url.port().unwrap_or_default(),
            credential
        );
    }
    if line.starts_with("dsh web: ") {
        return "dsh web: [invalid readiness URL omitted]".to_owned();
    }
    line.to_owned()
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
            readiness_url("dsh web: http://127.0.0.1:49152").map(|url| url.to_string()),
            Some("http://127.0.0.1:49152/".into())
        );
        assert_eq!(
            readiness_url("dsh web: http://127.0.0.1:49152/?token=launch_token")
                .map(|url| url.to_string()),
            Some("http://127.0.0.1:49152/?token=launch_token".into())
        );
    }

    #[test]
    fn rejects_unsafe_or_malformed_readiness_line() {
        for line in [
            "dsh web: http://localhost:3080",
            "dsh web: https://127.0.0.1:3080",
            "dsh web: http://example.com:3080",
            "dsh web: http://user@127.0.0.1:3080",
            "dsh web: http://127.0.0.1:3080/session",
            "dsh web: http://127.0.0.1:3080/#fragment",
            "dsh web: http://127.0.0.1:3080/?token=",
            "dsh web: http://127.0.0.1:3080/?token=valid&extra=value",
            "dsh web: http://127.0.0.1:3080/?token=not%20base64url",
        ] {
            assert_eq!(readiness_url(line), None, "accepted {line}");
        }
    }

    #[test]
    fn navigation_is_limited_to_bootstrap_and_active_harness_origins() {
        let manager = BackendManager::default();
        manager.capture_bootstrap_url(&Url::parse("http://tauri.localhost/").unwrap());
        let navigation = HarnessNavigation {
            host: "127.0.0.1".into(),
            port: 49152,
            generation: 1,
            requires_cookie: true,
        };
        assert!(navigation.is_clean_root(&Url::parse("http://127.0.0.1:49152/").unwrap()));
        assert!(
            !navigation.is_clean_root(&Url::parse("http://127.0.0.1:49152/?token=secret").unwrap())
        );
        *manager.harness_origin.lock().unwrap() = Some(navigation);

        assert!(manager.allows_navigation(&Url::parse("http://tauri.localhost/").unwrap()));
        assert!(
            manager.allows_navigation(&Url::parse("http://127.0.0.1:49152/session/abc").unwrap())
        );
        assert!(!manager.allows_navigation(&Url::parse("http://127.0.0.1:49153/").unwrap()));
        assert!(!manager.allows_navigation(&Url::parse("https://example.com/").unwrap()));
        assert!(!manager.allows_navigation(&Url::parse("http://localhost:9999/").unwrap()));
    }

    #[test]
    fn browser_cookie_names_are_bound_to_the_exact_authority() {
        let first = Url::parse("http://127.0.0.1:49152/").unwrap();
        let second = Url::parse("http://127.0.0.1:49153/").unwrap();
        assert_eq!(
            dsh_auth_cookie_name(&first).as_deref(),
            Some("dsh-auth-rVQkdfvWOXZk-UCkN_TrO5eJBsE1jldRuAWZUSJJyrk")
        );
        assert_ne!(dsh_auth_cookie_name(&first), dsh_auth_cookie_name(&second));
    }

    #[test]
    fn redacts_launch_tokens_from_stdout() {
        let line = "dsh web: http://127.0.0.1:49152/?token=super_secret";
        let url = readiness_url(line);
        let safe = safe_stdout_line(line, url.as_ref());
        assert_eq!(safe, "dsh web: http://127.0.0.1:49152/?token=[REDACTED]");
        assert!(!safe.contains("super_secret"));

        let invalid = "dsh web: http://evil.example/?token=also_secret";
        let safe_invalid = safe_stdout_line(invalid, readiness_url(invalid).as_ref());
        assert_eq!(safe_invalid, "dsh web: [invalid readiness URL omitted]");
        assert!(!safe_invalid.contains("also_secret"));
    }
}
