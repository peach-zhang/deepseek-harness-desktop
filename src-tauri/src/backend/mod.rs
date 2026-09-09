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
use tauri::{
    webview::{DownloadEvent, PageLoadEvent, WebviewBuilder},
    AppHandle, Emitter, Manager, Webview, WebviewUrl,
};
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
    /// True once a valid `dsh web: <url>` readiness line was seen for this
    /// generation. Used to explain a startup timeout: "the CLI never announced
    /// readiness" is a very different failure from "the page did not load".
    readiness_seen: bool,
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
            readiness_seen: false,
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

    pub(crate) fn allows_bootstrap_navigation(&self, url: &Url) -> bool {
        self.bootstrap_url()
            .is_some_and(|bootstrap| bootstrap.origin() == url.origin())
            || is_bootstrap_url(url)
    }

    pub(crate) fn allows_harness_navigation(&self, url: &Url) -> bool {
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
        self.restore_bootstrap(&app);

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
            runtime.readiness_seen = false;
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
            let readiness_seen = runtime.readiness_seen;
            runtime.navigating = false;
            if let Some(child) = runtime.child.take() {
                stop_child(child);
            }
            let message = if navigation_started {
                "DeepSeek Harness 页面认证超时，未能建立有效的浏览器会话。"
            } else if !readiness_seen {
                "DeepSeek Harness 启动超时：未在 45 秒内输出就绪地址（`dsh web: <url>`）。"
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
                            let readiness = parse_readiness_line(line);
                            if let ReadinessLine::Malformed(issue) = &readiness {
                                log::warn!(
                                    target: "dsh",
                                    "ignored malformed `{READINESS_PREFIX}` readiness line ({})",
                                    issue.description()
                                );
                            }
                            log::info!(target: "dsh", "{}", safe_stdout_line(line, &readiness));
                            let Some(url) = readiness.url() else {
                                continue;
                            };

                            {
                                let mut runtime = manager.inner.lock().await;
                                if runtime.generation != generation || runtime.navigating {
                                    continue;
                                }
                                runtime.navigating = true;
                                runtime.readiness_seen = true;
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
                            let readiness = parse_readiness_line(line);
                            let safe_line = safe_stdout_line(line, &readiness);
                            log::warn!(target: "dsh", "{safe_line}");
                            let mut runtime = manager.inner.lock().await;
                            if runtime.generation == generation {
                                if readiness.url().is_some() {
                                    runtime.readiness_seen = true;
                                }
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
                        // A process that died before announcing a readiness URL
                        // almost always means the CLI rejected its arguments or
                        // crashed during bootstrap; say so explicitly instead of
                        // leaving the user with only a bare exit code.
                        let message = if runtime.readiness_seen {
                            message
                        } else {
                            format!(
                                "{message}（进程在输出就绪地址 `{READINESS_PREFIX}<url>` 之前即退出）"
                            )
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
            .get_window("main")
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
        let navigation_manager = self.clone();
        let page_load_manager = self.clone();
        let page_load_app = app.clone();
        let builder = WebviewBuilder::new("harness", WebviewUrl::External(url))
            .on_navigation(move |url| navigation_manager.allows_harness_navigation(url))
            .on_page_load(move |webview, payload| {
                if matches!(payload.event(), PageLoadEvent::Finished) {
                    page_load_manager.handle_page_load(&page_load_app, &webview, payload.url());
                }
            })
            .on_download(|_webview, event| {
                if let DownloadEvent::Finished {
                    path: Some(path),
                    success: true,
                    ..
                } = event
                {
                    crate::platform::open_containing_folder(&path);
                }
                true
            });
        let size = window
            .inner_size()
            .map_err(|error| format!("无法读取窗口尺寸：{error}"))?
            .to_logical::<f64>(window.scale_factor().map_err(|error| error.to_string())?);
        let bounds = crate::window_shell::harness_bounds(size);
        let webview = window
            .add_child(builder, bounds.position, bounds.size)
            .map_err(|error| format!("无法打开 DeepSeek Harness：{error}"))?;
        // 创建期间窗口可能发生缩放，使用最新尺寸再次同步。
        crate::window_shell::resize_webviews(&window).map_err(|error| error.to_string())?;
        let _ = webview.set_focus();
        // The Harness WebView was created last, so it now stacks above the
        // version panel; re-create the panel to keep it visible.
        crate::commands::reopen_info_panel_above_harness(app);
        Ok(())
    }

    pub(crate) fn handle_page_load(&self, app: &AppHandle, window: &Webview, url: &Url) {
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
        // The panel only makes sense while the bootstrap page owns the window.
        crate::commands::close_info_panel(app);
        if let Some(harness) = app.get_webview("harness") {
            if let Err(error) = harness.close() {
                log::warn!("无法关闭 Harness 子 WebView：{error}");
            }
        }
        if let Some(window) = app.get_window("main") {
            if let Err(error) = crate::window_shell::resize_webviews(&window) {
                log::warn!("无法恢复启动页尺寸：{error}");
            }
        }
        if let Some(bootstrap) = app.get_webview("main") {
            let _ = bootstrap.set_focus();
        }
    }
}

fn dsh_auth_cookie_name(url: &Url) -> Option<String> {
    let authority = format!("{}:{}", url.host_str()?, url.port()?);
    let digest = Sha256::digest(authority.as_bytes());
    Some(format!("dsh-auth-{}", URL_SAFE_NO_PAD.encode(digest)))
}

/// True only when a *non-empty* session cookie named for this exact
/// `127.0.0.1:<port>` authority is present.
///
/// Checking the name alone would accept a cookie that exists but carries no
/// credential, which is precisely the broken state this guard is meant to
/// catch: the launch token exchange must have produced a usable session before
/// the Harness page can be considered loaded.
fn has_dsh_auth_cookie(window: &Webview, url: &Url) -> bool {
    let Some(expected_name) = dsh_auth_cookie_name(url) else {
        return false;
    };
    window.cookies_for_url(url.clone()).is_ok_and(|cookies| {
        cookies
            .iter()
            .any(|cookie| session_cookie_is_valid(cookie.name(), &expected_name, cookie.value()))
    })
}

/// A session cookie only counts when the name matches the authority-derived
/// name *and* it actually carries a credential.
fn session_cookie_is_valid(name: &str, expected_name: &str, value: &str) -> bool {
    name == expected_name && !value.is_empty()
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

const READINESS_PREFIX: &str = "dsh web: ";

/// Why a `dsh web: ` line could not be turned into a loadable Harness URL.
///
/// The distinction is diagnostic only: every variant is refused, but knowing
/// *which* check failed turns a silent 45-second startup timeout into an
/// actionable log line when the upstream CLI output format changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReadinessIssue {
    /// The URL token still contains a control character after the edges were
    /// trimmed, so it is dropped without echoing it into logs.
    Unprintable,
    /// No URL token at all after the prefix.
    MissingUrl,
    /// Trailing tokens after the URL, e.g. a CLI that appended a sentence.
    TrailingTokens,
    /// The URL token is not a parseable absolute URL.
    Unparsable,
    /// Parsed, but not an `http://127.0.0.1:<port>/` URL.
    NotLoopbackHttp,
    /// Loopback, but the query string is not a single valid `token` parameter.
    UnsafeQuery,
}

#[derive(Debug)]
enum ReadinessLine {
    /// The Harness readiness URL, ready to load in the child WebView.
    Ready(Url),
    /// A `dsh web: ` line that failed validation.
    Malformed(ReadinessIssue),
    /// An ordinary log line that is not a readiness signal.
    Unrelated,
}

impl ReadinessLine {
    fn url(&self) -> Option<&Url> {
        match self {
            Self::Ready(url) => Some(url),
            _ => None,
        }
    }
}

/// Parses one stdout/stderr line into a validated Harness readiness URL.
///
/// The sidecar contract is a single line: `dsh web: http://127.0.0.1:<port>`
/// optionally followed by `?token=<base64url>`. Only that exact shape is
/// accepted — anything else is reported as [`ReadinessIssue`] so the caller can
/// log *why* startup is not progressing.
fn parse_readiness_line(line: &str) -> ReadinessLine {
    let Some(rest) = line.strip_prefix(READINESS_PREFIX) else {
        return ReadinessLine::Unrelated;
    };
    // Trim only the edges. Interior whitespace still means "two tokens", which
    // keeps a CLI that appends prose to its readiness URL detectable, while a
    // trailing `\r\n` from the pipe is tolerated.
    let rest = rest.trim_matches(char::is_control).trim();
    let mut tokens = rest.split_whitespace();
    let Some(candidate) = tokens.next() else {
        return ReadinessLine::Malformed(ReadinessIssue::MissingUrl);
    };
    if tokens.next().is_some() {
        return ReadinessLine::Malformed(ReadinessIssue::TrailingTokens);
    }
    // A malformed URL may embed an unredacted launch token, so never echo the
    // raw candidate into logs.
    if candidate.chars().any(char::is_control) {
        return ReadinessLine::Malformed(ReadinessIssue::Unprintable);
    }
    let Ok(parsed) = Url::parse(candidate) else {
        return ReadinessLine::Malformed(ReadinessIssue::Unparsable);
    };
    if parsed.scheme() != "http"
        || parsed.host_str() != Some("127.0.0.1")
        || parsed.port().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.fragment().is_some()
    {
        return ReadinessLine::Malformed(ReadinessIssue::NotLoopbackHttp);
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
        return ReadinessLine::Malformed(ReadinessIssue::UnsafeQuery);
    }
    ReadinessLine::Ready(parsed)
}

impl ReadinessIssue {
    /// Operator-facing explanation. Deliberately generic: it must never leak a
    /// rejected URL that may still carry an unredacted launch token.
    fn description(self) -> &'static str {
        match self {
            Self::Unprintable => "输出包含不可打印字符",
            Self::MissingUrl => "前缀之后缺少 URL",
            Self::TrailingTokens => "URL 之后存在多余内容",
            Self::Unparsable => "URL 无法解析",
            Self::NotLoopbackHttp => "URL 不是 http://127.0.0.1:<port>/ 形式",
            Self::UnsafeQuery => "查询串不是单个合法的 token 参数",
        }
    }
}

fn safe_stdout_line(line: &str, readiness: &ReadinessLine) -> String {
    if let ReadinessLine::Ready(url) = readiness {
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
    if line.starts_with(READINESS_PREFIX) {
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
        for (line, expected) in [
            (
                "dsh web: http://127.0.0.1:49152",
                "http://127.0.0.1:49152/",
            ),
            (
                "dsh web: http://127.0.0.1:49152/?token=launch_token",
                "http://127.0.0.1:49152/?token=launch_token",
            ),
            // The CLI may pad the line; surrounding whitespace is not part of
            // the contract and must not turn a valid signal into a timeout.
            (
                "  dsh web: http://127.0.0.1:49152/  ",
                "http://127.0.0.1:49152/",
            ),
        ] {
            let readiness = parse_readiness_line(line);
            assert_eq!(
                readiness.url().map(Url::to_string).as_deref(),
                Some(expected),
                "rejected {line:?}"
            );
        }
    }

    #[test]
    fn reports_why_a_readiness_line_was_rejected() {
        for (line, expected) in [
            ("dsh web:", ReadinessIssue::MissingUrl),
            ("dsh web:   ", ReadinessIssue::MissingUrl),
            (
                "dsh web: http://127.0.0.1:3080 ready",
                ReadinessIssue::TrailingTokens,
            ),
            ("dsh web: \u{7f}", ReadinessIssue::MissingUrl),
            ("dsh web: not-a-url", ReadinessIssue::Unparsable),
            (
                "dsh web: http://127.0.0.1:3080/?token=sec\u{7f}ret",
                ReadinessIssue::Unprintable,
            ),
            (
                "dsh web: http://localhost:3080",
                ReadinessIssue::NotLoopbackHttp,
            ),
            (
                "dsh web: https://127.0.0.1:3080",
                ReadinessIssue::NotLoopbackHttp,
            ),
            (
                "dsh web: http://example.com:3080",
                ReadinessIssue::NotLoopbackHttp,
            ),
            (
                "dsh web: http://user@127.0.0.1:3080",
                ReadinessIssue::NotLoopbackHttp,
            ),
            (
                "dsh web: http://127.0.0.1:3080/session",
                ReadinessIssue::NotLoopbackHttp,
            ),
            (
                "dsh web: http://127.0.0.1:3080/#fragment",
                ReadinessIssue::NotLoopbackHttp,
            ),
            (
                "dsh web: http://127.0.0.1:3080/?token=",
                ReadinessIssue::UnsafeQuery,
            ),
            (
                "dsh web: http://127.0.0.1:3080/?token=valid&extra=value",
                ReadinessIssue::UnsafeQuery,
            ),
            (
                "dsh web: http://127.0.0.1:3080/?token=not%20base64url",
                ReadinessIssue::UnsafeQuery,
            ),
            (
                "dsh web: http://127.0.0.1:3080/?token=UPPER_not_allowed",
                ReadinessIssue::UnsafeQuery,
            ),
        ] {
            assert_eq!(
                parse_readiness_line(line),
                ReadinessLine::Malformed(expected),
                "misclassified {line:?}"
            );
            assert!(
                parse_readiness_line(line).url().is_none(),
                "exposed a URL for {line:?}"
            );
        }
    }

    #[test]
    fn ignores_lines_that_are_not_readiness_signals() {
        for line in [
            "starting dsh web server",
            "DSH web: http://127.0.0.1:3080",
            "dsh web server ready",
            "",
        ] {
            assert_eq!(parse_readiness_line(line), ReadinessLine::Unrelated);
        }
    }

    #[test]
    fn navigation_keeps_bootstrap_and_harness_in_separate_webviews() {
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

        let bootstrap = Url::parse("http://tauri.localhost/").unwrap();
        let harness = Url::parse("http://127.0.0.1:49152/").unwrap();
        assert!(manager.allows_bootstrap_navigation(&bootstrap));
        assert!(!manager.allows_bootstrap_navigation(&harness));
        assert!(!manager.allows_harness_navigation(&bootstrap));
        assert!(
            manager.allows_harness_navigation(&Url::parse("http://127.0.0.1:49152/session/abc").unwrap())
        );
        assert!(!manager.allows_harness_navigation(&Url::parse("http://127.0.0.1:49153/").unwrap()));
        assert!(!manager.allows_harness_navigation(&Url::parse("https://example.com/").unwrap()));
        assert!(!manager.allows_harness_navigation(&Url::parse("http://localhost:9999/").unwrap()));

        *manager.harness_origin.lock().unwrap() = None;
        assert!(!manager.allows_harness_navigation(&harness));
        assert!(manager.allows_bootstrap_navigation(&bootstrap));
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
        let readiness = parse_readiness_line(line);
        let safe = safe_stdout_line(line, &readiness);
        assert_eq!(safe, "dsh web: http://127.0.0.1:49152/?token=[REDACTED]");
        assert!(!safe.contains("super_secret"));

        let invalid = "dsh web: http://evil.example/?token=also_secret";
        let readiness = parse_readiness_line(invalid);
        let safe_invalid = safe_stdout_line(invalid, &readiness);
        assert_eq!(safe_invalid, "dsh web: [invalid readiness URL omitted]");
        assert!(!safe_invalid.contains("also_secret"));
        assert!(!safe_invalid.contains("evil.example"));

        // Rejection reasons are logged, so they must stay token-free too.
        for issue in [
            ReadinessIssue::Unprintable,
            ReadinessIssue::MissingUrl,
            ReadinessIssue::TrailingTokens,
            ReadinessIssue::Unparsable,
            ReadinessIssue::NotLoopbackHttp,
            ReadinessIssue::UnsafeQuery,
        ] {
            assert!(!issue.description().contains("token="));
        }
    }

    #[test]
    fn rejects_a_present_but_empty_session_cookie() {
        // The name check alone would treat an empty cookie as authenticated;
        // the non-empty value check is what makes this guard meaningful.
        assert!(session_cookie_is_valid("dsh-auth-test", "dsh-auth-test", "session-token"));
        assert!(!session_cookie_is_valid("dsh-auth-test", "dsh-auth-test", ""));
        assert!(!session_cookie_is_valid("other-cookie", "dsh-auth-test", "session-token"));
    }
}
