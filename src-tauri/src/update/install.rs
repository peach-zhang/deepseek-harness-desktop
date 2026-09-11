//! Harness runtime installation — npm CLI bootstrap, npm install, integrity
//! verification, and npm package extraction.

use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use flate2::read::GzDecoder;
use semver::Version;
use serde_json::Value;
use sha2::{Digest, Sha512};

use super::registry::fetch_json;
use super::RuntimeSelection;
use crate::archive::safe_archive_path;

/// Windows-only: prevents a visible console window from flashing when spawning
/// the bundled Node.js sidecar from the GUI process.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const NPM_PACKAGE: &str = "npm";
const NPM_CLI_VERSION: &str = "11.17.0";
/// The Harness package this app installs and updates.
const NPM_PACKAGE_TARGET: &str = "@deepseek-ai/dsh";
/// Reuse cached downloads on the first attempt; recovery must explicitly
/// override offline settings inherited from npmrc or the parent environment.
fn npm_cache_args(prefer_online: bool) -> &'static [&'static str] {
    if prefer_online {
        &[
            "--prefer-online",
            "--prefer-offline=false",
            "--offline=false",
        ]
    } else {
        &["--prefer-offline"]
    }
}
pub(crate) const UPDATE_STAGE_TOTAL: usize = 4;
pub(crate) const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
/// Maximum wall-clock time for `npm install`.  The Harness package has 120+
/// transitive dependencies whose tarballs must all be downloaded and extracted;
/// on cold-cache launches (especially with Windows Defender real-time scanning
/// the extracted files) this routinely takes 5–10 minutes.  15 minutes keeps
/// a comfortable margin without blocking startup indefinitely.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(900);
pub(crate) const MAX_ARCHIVE_BYTES: u64 = 96 * 1024 * 1024;

/// A single labelled step within the Harness update flow. The `description`
/// is shown directly in the UI, so it stays in Chinese to match the rest of
/// the desktop shell.
#[derive(Clone, Copy, Debug)]
pub(crate) struct UpdateStage {
    pub index: usize,
    pub description: &'static str,
}

impl UpdateStage {
    pub const CHECKING_REGISTRY: Self = Self {
        index: 1,
        description: "检查更新源",
    };
    pub const DOWNLOADING_NPM: Self = Self {
        index: 2,
        description: "下载 npm CLI",
    };
    pub const INSTALLING_HARNESS: Self = Self {
        index: 3,
        description: "安装 Harness",
    };
    pub const FINALIZING: Self = Self {
        index: 4,
        description: "应用更新",
    };
}

/// Downloads and caches the standalone npm CLI used to install updates.
/// Returns the path to `bin/npm-cli.js`.
pub(crate) fn ensure_npm_cli(
    data_dir: &Path,
    registry: &str,
    agent: &ureq::Agent,
) -> Result<PathBuf, String> {
    let root = data_dir.join("npm-cli").join(NPM_CLI_VERSION);
    let cli = root.join("bin").join("npm-cli.js");
    if cli.is_file() {
        log::debug!("npm CLI already cached at {}", cli.display());
        return Ok(cli);
    }

    log::debug!("npm CLI not cached, downloading from registry");

    let metadata = fetch_json(agent, &format!("{registry}/{NPM_PACKAGE}"))?;
    let dist = metadata
        .get("versions")
        .and_then(|versions| versions.get(NPM_CLI_VERSION))
        .and_then(|version| version.get("dist"))
        .ok_or_else(|| format!("registry 中不存在 npm@{NPM_CLI_VERSION}"))?;
    let tarball = dist
        .get("tarball")
        .and_then(Value::as_str)
        .ok_or_else(|| "npm 元数据缺少 tarball 地址。".to_owned())?;
    let integrity = dist
        .get("integrity")
        .and_then(Value::as_str)
        .ok_or_else(|| "npm 元数据缺少完整性校验值。".to_owned())?;

    let bytes = fetch_archive(agent, tarball)?;
    verify_sha512(&bytes, integrity).map_err(|error| format!("npm CLI 包校验失败:{error}"))?;

    let staging = data_dir
        .join("npm-cli")
        .join(format!(".{NPM_CLI_VERSION}.staging"));
    if staging.exists() {
        super::remove_dir_all_retried(&staging)
            .map_err(|error| format!("无法清理 npm CLI 暂存目录:{error}"))?;
    }
    fs::create_dir_all(&staging).map_err(|error| format!("无法创建 npm CLI 暂存目录:{error}"))?;

    let extract_result = extract_npm_package(&bytes, &staging);
    if let Err(error) = extract_result {
        let _ = super::remove_dir_all_retried(&staging);
        return Err(error);
    }
    let staged_cli = staging.join("bin").join("npm-cli.js");
    if !staged_cli.is_file() {
        let _ = super::remove_dir_all_retried(&staging);
        return Err("npm CLI 包内容不完整。".into());
    }
    if root.exists() {
        super::remove_dir_all_retried(&root)
            .map_err(|error| format!("无法替换旧 npm CLI:{error}"))?;
    }
    fs::rename(&staging, &root).map_err(|error| format!("无法启用 npm CLI:{error}"))?;
    Ok(cli)
}

pub(crate) fn install_updated_runtime(
    node: &Path,
    data_dir: &Path,
    registry: &str,
    agent: &ureq::Agent,
    target: &Version,
    tarball: Option<&str>,
    notify: &mut dyn FnMut(super::UpdateNotice),
) -> Result<RuntimeSelection, String> {
    use super::UpdateNotice;

    let version = target.to_string();

    notify(UpdateNotice::Staging {
        stage: UpdateStage::DOWNLOADING_NPM,
        target: version.clone(),
    });
    log::debug!("ensuring npm CLI is available");
    let npm_cli = ensure_npm_cli(data_dir, registry, agent)?;
    log::debug!("npm CLI ready: {}", npm_cli.display());

    let runtime_root = data_dir.join("runtime");
    fs::create_dir_all(&runtime_root).map_err(|error| format!("无法创建运行时目录:{error}"))?;
    let staging = runtime_root.join(format!(".{version}-{}.staging", std::env::consts::ARCH));
    if staging.exists() {
        super::remove_dir_all_retried(&staging)
            .map_err(|error| format!("无法清理更新暂存目录:{error}"))?;
    }
    fs::create_dir_all(&staging).map_err(|error| format!("无法创建更新暂存目录:{error}"))?;

    let result = (|| -> Result<RuntimeSelection, String> {
        let cache = data_dir.join("npm-cache");
        fs::create_dir_all(&cache).map_err(|error| format!("无法创建 npm 缓存目录:{error}"))?;

        notify(UpdateNotice::Staging {
            stage: UpdateStage::INSTALLING_HARNESS,
            target: version.clone(),
        });
        log::debug!("starting npm install -g for Harness {version}");
        install_harness_package(
            node, &npm_cli, &staging, &cache, registry, &version, tarball,
        )?;
        log::debug!("npm install completed, verifying staged entry");

        let staged_entry = runtime_entry(&staging);
        if !staged_entry.is_file() {
            return Err("更新安装结果不完整，缺少 Harness 入口。".into());
        }

        // Read the actually installed version from the package's own package.json.
        let installed_pkg_json = runtime_package_dir(&staging).join("package.json");
        let installed_version = fs::read_to_string(&installed_pkg_json)
            .ok()
            .and_then(|contents| {
                serde_json::from_str::<Value>(&contents)
                    .ok()
                    .and_then(|pkg| pkg.get("version")?.as_str().map(String::from))
            })
            .ok_or_else(|| "更新安装结果缺少有效的 Harness 版本。".to_owned())?;
        if installed_version != version {
            return Err(format!(
                "更新版本校验失败：请求 {version}，实际安装 {installed_version}。"
            ));
        }
        log::debug!("Harness {installed_version} installed and verified");

        notify(UpdateNotice::Staging {
            stage: UpdateStage::FINALIZING,
            target: installed_version.clone(),
        });
        let (destination, entry) = runtime_paths(data_dir, &installed_version);
        if destination.exists() {
            super::remove_dir_all_retried(&destination)
                .map_err(|error| format!("无法替换旧版本运行时:{error}"))?;
        }
        fs::rename(&staging, &destination).map_err(|error| format!("无法启用新运行时:{error}"))?;
        Ok(RuntimeSelection {
            entry,
            version: installed_version,
        })
    })();

    if result.is_err() {
        let _ = super::remove_dir_all_retried(&staging);
    }
    result
}

/// Why an `npm install` attempt failed, as far as the app is concerned.
///
/// Stale metadata and missing/corrupt cached content can be recovered by
/// retrying with a fresh cache. Unrelated filesystem/network errors are left
/// to the caller's registry fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NpmInstallKind {
    /// npm exited with its `ETARGET`/"No matching version found" error.
    TargetNotFound,
    /// Missing cached content or an integrity mismatch worth one fresh download.
    CacheCorrupt,
    /// Anything else (permissions, network errors, missing install files, etc.).
    Other,
}

/// The signature npm prints for a package version it could not resolve. It
/// appears verbatim in stderr at `--loglevel=verbose`.
const ETARGET_MARKER: &str = "No matching version found for";

fn npm_failure_kind(stderr: &str) -> NpmInstallKind {
    let normalized = stderr.replace('\\', "/");
    if normalized.lines().any(|line| {
        let Some(error) = line
            .strip_prefix("npm error ")
            .or_else(|| line.strip_prefix("npm ERR! "))
        else {
            return false;
        };
        // Integrity failures often omit the cache path altogether. One fresh
        // download is safe; a persistent upstream mismatch still fails.
        error.trim() == "code EINTEGRITY"
            || (error.to_ascii_lowercase().contains("/_cacache/") && error.contains("ENOENT"))
    }) {
        NpmInstallKind::CacheCorrupt
    } else if stderr.contains("ETARGET") || stderr.contains(ETARGET_MARKER) {
        NpmInstallKind::TargetNotFound
    } else {
        NpmInstallKind::Other
    }
}

/// Never delete individual entries from the shared cache: npm 11's `cache
/// clean <key>` also deletes the content blob, which other keys may reference.
/// Keep recovery downloads isolated, and retain their npm logs for diagnosis.
fn fresh_retry_cache(cache: &Path) -> Result<PathBuf, String> {
    let root = cache.join("_retries");
    fs::create_dir_all(&root).map_err(|error| format!("无法创建 npm 重试缓存目录:{error}"))?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = root.join(format!("{}-{timestamp}", std::process::id()));
    // create_dir fails on collision rather than reusing a potentially bad cache.
    fs::create_dir(&path).map_err(|error| format!("无法创建 npm 独立缓存目录:{error}"))?;
    Ok(path)
}

fn cleanup_retry_cache(cache: &Path) {
    let content = cache.join("_cacache");
    if content.exists() {
        if let Err(error) = super::remove_dir_all_retried(&content) {
            log::warn!(
                "failed to remove retry cache {}: {error}",
                content.display()
            );
        }
    }
}

/// Preserve npm's error code, reason, and debug-log location in the update
/// failure, instead of only reporting an exit code and elapsed time.
fn npm_error_details(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|line| line.starts_with("npm error ") || line.starts_with("npm ERR! "))
        .take(12)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Installs the Harness package into `staging`, preferring the exact tarball
/// URL carried by the packument the update check already fetched.
///
/// Installing from the tarball URL is the primary strategy because it never
/// asks the registry to resolve a *name@version* — the packument lookup that
/// failed in the field. The registry (and any CDN in front of it) can keep
/// serving a packument that predates a just-published release for tens of
/// minutes even while the tarball itself is already downloadable, which leaves
/// `npm install @deepseek-ai/dsh@<new>` failing with `ETARGET` while the app's
/// own metadata fetch correctly reports the version as available. Going
/// straight to the tarball sidesteps that inconsistency entirely.
///
/// The version-spec path remains as the fallback for when no tarball URL is
/// available. Both paths retry once with a fresh cache for stale metadata or
/// corrupt cached downloads, including failures in transitive dependencies.
#[allow(clippy::too_many_arguments)]
fn install_harness_package(
    node: &Path,
    npm_cli: &Path,
    staging: &Path,
    cache: &Path,
    registry: &str,
    version: &str,
    tarball: Option<&str>,
) -> Result<(), String> {
    if let Some(url) = tarball {
        log::debug!("installing Harness {version} from its tarball URL: {url}");
        match run_npm_install_with_retry(node, npm_cli, staging, cache, registry, url, "tarball") {
            Ok(()) => return Ok(()),
            Err(error) => {
                log::warn!(
                    "installing Harness {version} from its tarball URL failed ({error}); \
                     falling back to resolving {NPM_PACKAGE_TARGET}@{version}"
                );
                clear_staging(staging)?;
            }
        }
    }

    let spec = format!("{NPM_PACKAGE_TARGET}@{version}");
    run_npm_install_with_retry(
        node,
        npm_cli,
        staging,
        cache,
        registry,
        &spec,
        "version spec",
    )
}

/// Empties the staging tree so the next install attempt starts clean. A failed
/// attempt can leave a half-built `node_modules` behind.
fn clear_staging(staging: &Path) -> Result<(), String> {
    if staging.exists() {
        super::remove_dir_all_retried(staging)
            .map_err(|error| format!("无法清理更新暂存目录:{error}"))?;
    }
    fs::create_dir_all(staging).map_err(|error| format!("无法创建更新暂存目录:{error}"))
}

/// Runs `npm install` for `spec`, retrying once in an empty cache on stale
/// metadata or cache corruption. Direct tarballs can also hit these errors
/// while npm resolves their dependencies.
#[allow(clippy::too_many_arguments)]
fn run_npm_install_with_retry(
    node: &Path,
    npm_cli: &Path,
    staging: &Path,
    cache: &Path,
    registry: &str,
    spec: &str,
    label: &str,
) -> Result<(), String> {
    match run_npm_install(node, npm_cli, staging, cache, registry, spec, false) {
        Ok(()) => Ok(()),
        Err((NpmInstallKind::Other, error)) => Err(error),
        Err((kind, error)) => {
            log::warn!(
                "npm install {spec} from {registry} failed ({kind:?}: {error}); \
                 retrying online with a fresh cache"
            );
            clear_staging(staging)?;
            let retry_cache =
                fresh_retry_cache(cache).map_err(|cache_error| format!("{error} {cache_error}"))?;
            let result =
                run_npm_install(node, npm_cli, staging, &retry_cache, registry, spec, true);
            cleanup_retry_cache(&retry_cache);
            match result {
                Ok(()) => {
                    log::info!("Harness {label} installed via {registry} using a fresh cache");
                    Ok(())
                }
                Err((_, retry_error)) => Err(format!("{error} 重试后仍失败：{retry_error}")),
            }
        }
    }
}

/// Runs a single `npm install` attempt.
///
/// Returns the classified failure kind alongside a display message so the
/// caller can decide whether a metadata refresh is worth retrying.
#[allow(clippy::too_many_arguments)]
fn run_npm_install(
    node: &Path,
    npm_cli: &Path,
    staging: &Path,
    cache: &Path,
    registry: &str,
    spec: &str,
    prefer_online: bool,
) -> Result<(), (NpmInstallKind, String)> {
    log::debug!(
        "npm install: node={} npm_cli={} staging={} registry={} spec={} prefer_online={}",
        node.display(),
        npm_cli.display(),
        staging.display(),
        registry,
        spec,
        prefer_online,
    );

    let mut cmd = Command::new(node);
    cmd.arg(npm_cli)
        .args([
            "install",
            // Use global install mode: npm's local install mode (resolving a
            // package.json dependency tree) hangs in the `reify` phase on
            // Windows with npm 11.x when the tree has 120+ packages.  Global
            // mode bypasses the local idealTree finalisation and installs the
            // package directly into --prefix/node_modules, which works
            // reliably.
            "-g",
            "--omit=dev",
            "--no-audit",
            "--no-fund",
            "--no-progress",
            "--ignore-scripts",
            // All value-carrying flags MUST use `=` syntax. npm's arg parser
            // consumes the next argument as the value for non-boolean flags
            // like `--loglevel`/`--prefix`/`--registry`, so passing them as
            // separate array entries misparses the entire argument chain.
            "--loglevel=verbose",
        ])
        .args(npm_cache_args(prefer_online))
        .args([
            // Retry transient network failures instead of hanging silently.
            "--fetch-retries=3",
            &format!("--cache={}", cache.display()),
            &format!("--prefix={}", staging.display()),
            &format!("--registry={}", registry),
            // The install target: either `@deepseek-ai/dsh@<version>` or a
            // direct tarball URL. Passing it as a positional argument is
            // required — global mode doesn't read package.json for the
            // install target list.
            spec,
        ])
        .current_dir(staging)
        .env("npm_config_cache", cache)
        .env("DSH_TELEMETRY_DISABLED", "1")
        // Tell npm (and any child scripts) this is a non-interactive CI
        // environment. Prevents npm from blocking on prompts like the
        // `allow-scripts` approval dialog that would hang a headless child.
        .env("CI", "true")
        // Increase concurrent download slots.  npm defaults to 12; with
        // 120+ packages to fetch, raising this to 50 roughly halves the
        // network-bound portion of the install on fast connections.
        .env("npm_config_maxsockets", "50")
        // Cap individual fetch timeouts so a single slow/stalled request
        // doesn't block the whole install indefinitely.  120s per request
        // is generous enough for large tarballs on slow connections.
        .env("npm_config_fetch_timeout", "120000")
        .env("npm_config_fetch_retry_mintimeout", "20000");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let mut child = cmd.spawn().map_err(|error| {
        (
            NpmInstallKind::Other,
            format!("无法启动 npm 安装进程:{error}"),
        )
    })?;

    // Drain npm's stdout in a background thread. npm writes its summary
    // ("added N packages in Xs") to stdout; we log it so we can see what
    // happened when the install hangs or fails.
    let stdout_handle = child.stdout.take();
    let stdout_thread = std::thread::spawn(move || {
        if let Some(stdout) = stdout_handle {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                log::debug!(target: "dsh", "[npm:out] {line}");
            }
        }
    });

    // Drain npm's stderr in a background thread. npm writes warnings,
    // progress information *and* its fatal errors to stderr, so the text is
    // both logged and kept on hand to classify the failure once the child
    // exits (see `npm_failure_kind`).
    let stderr_handle = child.stderr.take();
    let stderr_thread = std::thread::spawn(move || {
        let mut collected = String::new();
        if let Some(stderr) = stderr_handle {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                log::debug!(target: "dsh", "[npm:err] {line}");
                if !collected.is_empty() {
                    collected.push('\n');
                }
                collected.push_str(&line);
            }
        }
        collected
    });

    let started = Instant::now();
    let mut last_progress = Instant::now();
    let progress_interval = Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let elapsed = started.elapsed();
                log::debug!("npm install succeeded in {:.1}s", elapsed.as_secs_f64());
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Ok(());
            }
            Ok(Some(status)) => {
                let elapsed = started.elapsed();
                let _ = stdout_thread.join();
                let stderr = stderr_thread.join().unwrap_or_default();
                let kind = npm_failure_kind(&stderr);
                if kind == NpmInstallKind::TargetNotFound {
                    log::warn!(
                        "npm reported the requested version as missing; last stderr line: {}",
                        stderr.lines().last().unwrap_or("(no stderr)")
                    );
                }
                return Err((
                    kind,
                    format!(
                        "npm install 失败（退出码 {}，耗时 {:.1}s）。\n{}",
                        status
                            .code()
                            .map_or("未知".to_owned(), |code| code.to_string()),
                        elapsed.as_secs_f64(),
                        npm_error_details(&stderr),
                    ),
                ));
            }
            Ok(None) => {
                if started.elapsed() > INSTALL_TIMEOUT {
                    log::warn!(
                        "npm install timed out after {:.0}s, killing process",
                        started.elapsed().as_secs_f64()
                    );
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_thread.join();
                    let _ = stderr_thread.join();
                    return Err((NpmInstallKind::Other, "npm install 超时。".into()));
                }
                if last_progress.elapsed() >= progress_interval {
                    log::debug!(
                        "npm install still running ({:.0}s elapsed)",
                        started.elapsed().as_secs_f64()
                    );
                    last_progress = Instant::now();
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(error) => {
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err((
                    NpmInstallKind::Other,
                    format!("无法等待 npm 安装进程:{error}"),
                ));
            }
        }
    }
}

pub(crate) fn verify_sha512(bytes: &[u8], integrity: &str) -> Result<(), String> {
    let expected = integrity
        .strip_prefix("sha512-")
        .ok_or_else(|| format!("不支持的校验算法:{integrity}"))?;
    let actual = BASE64.encode(Sha512::digest(bytes));
    if actual != expected {
        return Err("校验和不匹配。".into());
    }
    Ok(())
}

fn fetch_archive(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>, String> {
    let mut response = agent
        .get(url)
        .call()
        .map_err(|error| format!("下载失败（{url}）：{error}"))?;
    response
        .body_mut()
        .with_config()
        .limit(MAX_ARCHIVE_BYTES)
        .read_to_vec()
        .map_err(|error| format!("读取下载内容失败（{url}）：{error}"))
}

/// The `node_modules/@deepseek-ai/dsh` directory inside an extracted runtime.
pub(crate) fn runtime_package_dir(base: &Path) -> PathBuf {
    base.join("node_modules").join("@deepseek-ai").join("dsh")
}

/// The `bin.js` entry point inside an extracted Harness runtime tree.
pub(crate) fn runtime_entry(base: &Path) -> PathBuf {
    runtime_package_dir(base).join("lib").join("bin.js")
}

pub(crate) fn runtime_paths(data_dir: &Path, version: &str) -> (PathBuf, PathBuf) {
    let directory = data_dir
        .join("runtime")
        .join(format!("{version}-{}", std::env::consts::ARCH));
    let entry = runtime_entry(&directory);
    (directory, entry)
}

/// Extracts an npm package tarball (`package/...` entries) stripping the
/// leading `package/` component, with the same traversal guards as the
/// bundled-runtime extraction.
fn extract_npm_package(bytes: &[u8], destination: &Path) -> Result<(), String> {
    let decoder = GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| format!("无法读取下载的归档:{error}"))?;
    for result in entries {
        let mut entry = result.map_err(|error| format!("无法读取归档条目:{error}"))?;
        let raw_path = entry
            .path()
            .map_err(|error| format!("归档包含无效路径:{error}"))?
            .into_owned();
        let mut components = raw_path.components();
        if components.next() != Some(Component::Normal("package".as_ref())) {
            return Err(format!("归档包含非 package 路径：{}", raw_path.display()));
        }
        let stripped: PathBuf = components.collect();
        if stripped.as_os_str().is_empty() {
            continue; // the `package/` root directory itself
        }
        if !safe_archive_path(&stripped) {
            return Err(format!("归档包含越界路径：{}", stripped.display()));
        }
        let target = destination.join(&stripped);
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            fs::create_dir_all(&target).map_err(|error| format!("无法创建目录:{error}"))?;
        } else if entry_type.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|error| format!("无法创建目录:{error}"))?;
            }
            entry
                .unpack(&target)
                .map_err(|error| format!("无法解包文件:{error}"))?;
        } else {
            return Err(format!("归档包含不支持的条目类型：{}", stripped.display()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_sha512_integrity() {
        let bytes = b"deepseek-harness-update-test";
        let integrity =
            "sha512-r4rPPYHQWLOq1LQ/U+O4RMuU4MZxYKi0DLYNMidK9EiaNLTswp4Tmovt79NSifBTOewtXikG0lIjocl5h+gWUg==";
        assert!(verify_sha512(bytes, integrity).is_ok());
        assert!(verify_sha512(b"tampered", integrity).is_err());
        assert!(verify_sha512(bytes, "sha1-deadbeef").is_err());
    }

    #[test]
    fn rejects_traversal_in_npm_archives() {
        assert!(safe_archive_path(Path::new("bin/npm-cli.js")));
        assert!(!safe_archive_path(Path::new("../escape")));
    }

    #[test]
    fn classifies_npm_failures() {
        // The exact stderr npm produced when a stale packument omitted the
        // requested version (npm 11.17.0, --loglevel=verbose).
        let etarget = "npm http fetch GET 200 https://registry.npmjs.org/@deepseek-ai%2fdsh 15ms (cache stale)\n\
             npm error code ETARGET\n\
             npm error notarget No matching version found for @deepseek-ai/dsh@0.1.5-rc.1.";
        assert_eq!(
            npm_failure_kind(etarget),
            NpmInstallKind::TargetNotFound,
            "a stale packument must be recognised so the install is retried"
        );
        // The marker alone is enough (npm prints it without the `ETARGET`
        // code line when the error is formatted differently).
        assert_eq!(
            npm_failure_kind("No matching version found for @deepseek-ai/dsh@1.0.0"),
            NpmInstallKind::TargetNotFound
        );

        // Unrelated failures must not trigger a metadata purge + retry.
        assert_eq!(
            npm_failure_kind("npm error code EAI_AGAIN\nnpm error network request failed"),
            NpmInstallKind::Other
        );
        assert_eq!(npm_failure_kind(""), NpmInstallKind::Other);
    }

    #[test]
    fn distinguishes_cache_corruption_from_other_filesystem_errors() {
        for error in [
            r"npm error enoent Invalid response body: ENOENT: stat 'C:\Users\test\npm-cache\_cacache\content-v2\sha512\f2\17\blob'",
            "npm error ENOENT: open '/tmp/npm-cache/_cacache/index-v5/entry'",
            "npm error code EINTEGRITY\nnpm error Integrity verification failed for sha512-test (/tmp/npm-cache/_cacache)",
            "npm error code EINTEGRITY\nnpm error sha512-test integrity checksum failed when using sha512: wanted sha512-test but got sha512-other. (42 bytes)",
        ] {
            assert_eq!(npm_failure_kind(error), NpmInstallKind::CacheCorrupt);
        }
        for error in [
            "npm warn ENOENT: open '/tmp/npm-cache/_cacache/content-v2/blob'\nnpm error code EACCES",
            "npm error ENOENT: open '/tmp/staging/package.json'",
            "npm error EACCES: open '/tmp/npm-cache/_cacache/content-v2/blob'",
            "npm error EINTEGRITY: downloaded tarball checksum mismatch",
        ] {
            assert_eq!(npm_failure_kind(error), NpmInstallKind::Other);
        }
    }

    #[test]
    fn retains_actionable_npm_errors() {
        let stderr = "npm verbose stack internal details\nnpm error code ENOENT\n\
            npm error enoent missing cache content\n\
            npm error A complete log of this run can be found in: /tmp/debug.log";
        let details = npm_error_details(stderr);
        assert!(details.contains("code ENOENT"));
        assert!(details.contains("missing cache content"));
        assert!(details.contains("/tmp/debug.log"));
        assert!(!details.contains("internal details"));
        assert_eq!(
            npm_error_details("npm ERR! code ETARGET"),
            "npm ERR! code ETARGET"
        );
    }

    /// Runs the real subprocess/retry code with a deterministic npm stand-in.
    /// No registry downloads; Node.js is the only external requirement.
    #[test]
    #[ignore = "requires Node.js; run explicitly with DSH_TEST_NODE if needed"]
    fn retries_with_fresh_cache_and_online_flags() {
        let node = std::env::var_os("DSH_TEST_NODE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("node"));
        let root = std::env::temp_dir().join(format!(
            "dsh-npm-retry-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let cli = root.join("npm.cjs");
        fs::write(
            &cli,
            r#"
const fs = require('node:fs');
const path = require('node:path');
const args = process.argv.slice(2);
const cache = args.find(a => a.startsWith('--cache=')).slice(8);
const root = __dirname;
const marker = path.join(root, 'attempt.json');
const retry = fs.existsSync(marker);
if (!retry) {
  fs.writeFileSync(marker, JSON.stringify({cache, args}));
  fs.writeFileSync('partial-install', 'incomplete');
  console.error(fs.readFileSync(path.join(root, 'failure.txt'), 'utf8'));
  process.exit(1);
}
const first = JSON.parse(fs.readFileSync(marker, 'utf8'));
const valid = cache !== first.cache
  && fs.readdirSync(cache).length === 0
  && args.includes('--prefer-online')
  && args.includes('--prefer-offline=false')
  && args.includes('--offline=false')
  && !args.includes('--prefer-offline')
  && first.args.includes('--prefer-offline')
  && !fs.existsSync('partial-install');
if (!valid) { console.error('npm error invalid retry configuration'); process.exit(2); }
fs.mkdirSync(path.join(cache, '_cacache'));
fs.mkdirSync(path.join(cache, '_logs'));
fs.writeFileSync(path.join(cache, '_logs', 'debug.log'), 'retry log');
fs.writeFileSync(path.join(root, 'retry-cache.txt'), cache);
const failure = fs.readFileSync(path.join(root, 'failure.txt'), 'utf8');
if (failure.includes('fail-twice')) { console.error(failure); process.exit(1); }
"#,
        )
        .unwrap();
        let staging = root.join("staging");
        let cache = root.join("shared-cache");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("keep"), "shared cache remains intact").unwrap();
        for (failure, retried, succeeds) in [
            (r"npm error ENOENT: stat 'C:\cache\_cacache\content-v2\blob'", true, true),
            ("npm error code ETARGET\nnpm error No matching version found for @deepseek-ai/dependency@1.0.0", true, true),
            ("npm error code EINTEGRITY\nnpm error integrity checksum failed when using sha512", true, true),
            ("npm error code EINTEGRITY\nnpm error fail-twice", true, false),
            ("npm error code ETARGET\nnpm error fail-twice", true, false),
            ("npm error code EAI_AGAIN", false, false),
        ] {
            clear_staging(&staging).unwrap();
            let _ = fs::remove_file(root.join("attempt.json"));
            let _ = fs::remove_file(root.join("retry-cache.txt"));
            fs::write(root.join("failure.txt"), failure).unwrap();
            let result = run_npm_install_with_retry(
                &node, &cli, &staging, &cache, "https://registry.invalid",
                "https://registry.invalid/dsh.tgz", "test tarball",
            );
            assert_eq!(result.is_ok(), succeeds, "{result:?}");
            assert_eq!(root.join("retry-cache.txt").exists(), retried);
            if retried {
                let retry_cache = PathBuf::from(fs::read_to_string(root.join("retry-cache.txt")).unwrap());
                assert!(!retry_cache.join("_cacache").exists());
                assert!(retry_cache.join("_logs/debug.log").is_file());
            }
            if !succeeds {
                let error = result.unwrap_err();
                assert!(error.contains(failure));
                assert_eq!(error.contains("重试后仍失败"), retried);
            }
            assert_eq!(fs::read_to_string(cache.join("keep")).unwrap(), "shared cache remains intact");
        }
        fs::remove_dir_all(root).unwrap();
    }
}
