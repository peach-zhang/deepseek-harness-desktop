//! Harness runtime installation — npm CLI bootstrap, npm install, integrity
//! verification, and npm package extraction.

use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
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
/// Prefer cached tarballs when available (e.g. repeated launches or prior
/// partial installs). npm falls back to the network automatically on a cache
/// miss, and revalidates expired metadata, so this is a pure optimisation on
/// the first attempt. The retry drops it in favour of `--prefer-online` to make
/// sure metadata is refetched (see `run_npm_install_with_retry`).
const PREFER_OFFLINE_ARGS: &[&str] = &["--prefer-offline"];
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
/// Only [`NpmInstallKind::TargetNotFound`] is worth a retry: it means npm
/// resolved *some* packument for the package that did not contain the version
/// we asked for. Any other failure — a network outage, a disk error — is not
/// fixed by re-resolving metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NpmInstallKind {
    /// npm exited with its `ETARGET`/"No matching version found" error.
    TargetNotFound,
    /// Anything else (non-zero exit without the ETARGET marker).
    Other,
}

/// The signature npm prints for a package version it could not resolve. It
/// appears verbatim in stderr at `--loglevel=verbose`.
const ETARGET_MARKER: &str = "No matching version found for";

fn npm_failure_kind(stderr: &str) -> NpmInstallKind {
    if stderr.contains("ETARGET") || stderr.contains(ETARGET_MARKER) {
        NpmInstallKind::TargetNotFound
    } else {
        NpmInstallKind::Other
    }
}

/// Builds npm's cacache key for a package's packument.
///
/// npm keys registry metadata as `make-fetch-happen:request-cache:<url>` with
/// the scope separator percent-encoded (`@scope/name` → `@scope%2Fname`).
pub(crate) fn packument_cache_key(registry: &str, name: &str) -> String {
    let encoded = name.replace('/', "%2F");
    format!(
        "make-fetch-happen:request-cache:{}/{}",
        registry.trim_end_matches('/'),
        encoded
    )
}

/// npm has written the scope separator both uppercased (`%2F`) and lowercased
/// (`%2f`) across versions, and `npm cache clean` matches the key byte for
/// byte, so both spellings of the package name must be purged. Note that
/// `str::to_ascii_lowercase` is *not* enough here: it would leave `%2F` intact.
const PURGE_NAME_VARIANTS: [&str; 2] = [NPM_PACKAGE_TARGET, "@deepseek-ai%2fdsh"];

/// Drops npm's cached packument for `name` so the next install must refetch
/// metadata from the registry.
///
/// This is the repair step for a stale-metadata failure. `npm cache clean`
/// rewrites the cacache index while holding npm's own lock, so it is safe to
/// run against the shared cache directory; failures are logged and ignored
/// because the retry is still worth attempting without the purge.
fn purge_packument_cache(node: &Path, npm_cli: &Path, cache: &Path, registry: &str) {
    for name in PURGE_NAME_VARIANTS {
        let key = packument_cache_key(registry, name);
        log::debug!("purging cached packument: {key}");
        let mut cmd = Command::new(node);
        cmd.arg(npm_cli)
            .args(["cache", "clean", &key, "--force"])
            .arg(format!("--cache={}", cache.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        match cmd.output() {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let deleted = stdout.lines().any(|line| line.starts_with("Deleted:"));
                if deleted {
                    log::debug!("npm cache purge removed the stale packument");
                } else {
                    log::debug!("npm cache purge found no matching packument entry");
                }
            }
            Ok(output) => log::warn!(
                "npm cache purge exited with {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Err(error) => log::warn!("failed to run npm cache purge: {error}"),
        }
    }
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
/// available, and it retries once with a purged packument cache because the
/// stale metadata can also live in npm's own cache.
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

/// Runs `npm install` for `spec`, and retries once without the local metadata
/// cache when npm reports that the requested version does not exist.
///
/// `spec` is either `@deepseek-ai/dsh@<version>` or a direct tarball URL; only
/// the former depends on packument resolution, so only it can produce an
/// `ETARGET` worth repairing here.
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
        Err((NpmInstallKind::TargetNotFound, error)) => {
            log::warn!(
                "npm could not resolve {spec} from {registry} ({error}); \
                 discarding the cached packument and retrying with --prefer-online"
            );
            purge_packument_cache(node, npm_cli, cache, registry);
            clear_staging(staging)?;

            match run_npm_install(node, npm_cli, staging, cache, registry, spec, true) {
                Ok(()) => {
                    log::info!(
                        "Harness {label} installed via {registry} after discarding stale metadata"
                    );
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
        .args(PREFER_OFFLINE_ARGS)
        .args([
            // Retry transient network failures instead of hanging silently.
            "--fetch-retries=3",
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
    if prefer_online {
        cmd.env("npm_config_prefer_online", "true");
    }
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
                        "npm install 失败（退出码 {}，耗时 {:.1}s）。",
                        status
                            .code()
                            .map_or("未知".to_owned(), |code| code.to_string()),
                        elapsed.as_secs_f64(),
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
    fn builds_packument_cache_keys() {
        assert_eq!(
            packument_cache_key("https://registry.npmmirror.com", NPM_PACKAGE_TARGET),
            "make-fetch-happen:request-cache:https://registry.npmmirror.com/@deepseek-ai%2Fdsh"
        );
        assert_eq!(
            packument_cache_key("https://registry.npmjs.org", NPM_PACKAGE_TARGET),
            "make-fetch-happen:request-cache:https://registry.npmjs.org/@deepseek-ai%2Fdsh"
        );
        // A trailing slash on the registry must not double up.
        assert_eq!(
            packument_cache_key("https://registry.npmjs.org/", NPM_PACKAGE_TARGET),
            "make-fetch-happen:request-cache:https://registry.npmjs.org/@deepseek-ai%2Fdsh"
        );
    }

    #[test]
    fn builds_packument_cache_keys_without_duplicated_slashes() {
        assert_eq!(
            packument_cache_key("https://registry.npmjs.org", NPM_PACKAGE_TARGET),
            "make-fetch-happen:request-cache:https://registry.npmjs.org/@deepseek-ai%2Fdsh"
        );
        // A trailing slash on the registry must not double up.
        assert_eq!(
            packument_cache_key("https://registry.npmjs.org/", NPM_PACKAGE_TARGET),
            "make-fetch-happen:request-cache:https://registry.npmjs.org/@deepseek-ai%2Fdsh"
        );
    }

    #[test]
    fn purge_covers_both_scope_encodings() {
        // The two keys npm has used for the same packument. The second entry
        // must be spelled out, because `to_ascii_lowercase` would leave `%2F`
        // intact and silently purge the same key twice.
        assert_ne!(PURGE_NAME_VARIANTS[0], PURGE_NAME_VARIANTS[1]);
        assert_eq!(
            packument_cache_key("https://registry.npmjs.org", PURGE_NAME_VARIANTS[1]),
            "make-fetch-happen:request-cache:https://registry.npmjs.org/@deepseek-ai%2fdsh"
        );
        let lowercased = PURGE_NAME_VARIANTS[0].to_ascii_lowercase();
        assert_eq!(
            lowercased, PURGE_NAME_VARIANTS[0],
            "the guard case: lowercasing the name does not change the escape"
        );
    }
}
