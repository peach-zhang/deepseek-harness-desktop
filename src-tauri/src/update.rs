//! Startup self-update for the bundled DeepSeek Harness runtime.
//!
//! The desktop app ships a pinned Harness build (`HARNESS_VERSION` in `lib.rs`),
//! but on every launch we ask the npm registry for the newest published version
//! and, when one exists, install it into the app data directory before starting
//! the backend. The bundled runtime remains the offline fallback: any failure
//! in the update path only logs a warning and never blocks startup.
//!
//! Layout under the app data directory:
//! - `runtime/<version>-<arch>/`    extracted Harness runtime trees
//! - `runtime/.<version>-<arch>.staging/`  in-progress installs (atomic rename)
//! - `npm-cli/<version>/`           cached standalone npm CLI used for installs
//! - `npm-cache/`                   shared npm download cache
//!
//! Installs run with `--ignore-scripts` on purpose: the bundled runtime is
//! built the same way (see `scripts/prepare-runtime.mjs`, where lifecycle
//! scripts stay unapproved), which proves the Harness works without them, and
//! skipping them avoids executing arbitrary install scripts at update time.
//!
//! Environment overrides:
//! - `DSH_DESKTOP_UPDATE_DISABLED=1`  skip the update check entirely
//! - `DSH_DESKTOP_REGISTRY=<url>`     use another registry instead of the
//!   default npmmirror.com mirror (for example the official
//!   https://registry.npmjs.org)

use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use flate2::read::GzDecoder;
use semver::Version;
use serde_json::Value;
use sha2::{Digest, Sha512};

use crate::runtime::{ensure_harness_runtime, safe_archive_path};

// The npmmirror sync serves byte-identical packages (same SHA-512 integrity
// hashes as npmjs) and is dramatically faster for the primary user base.
const DEFAULT_REGISTRY: &str = "https://registry.npmmirror.com";
const REGISTRY_ENV: &str = "DSH_DESKTOP_REGISTRY";
const DISABLE_ENV: &str = "DSH_DESKTOP_UPDATE_DISABLED";
const DSH_METADATA_PATH: &str = "@deepseek-ai%2Fdsh";
const NPM_PACKAGE: &str = "npm";
const NPM_CLI_VERSION: &str = "11.16.0";
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(900);
const MAX_METADATA_BYTES: u64 = 32 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 96 * 1024 * 1024;

pub(crate) struct RuntimeSelection {
    pub entry: PathBuf,
    pub version: String,
}

pub(crate) enum UpdateNotice {
    Checking { current: String },
    Updating { target: String },
}

/// Picks the Harness entry point to run: the newest already-installed runtime,
/// upgraded in place when the registry publishes a newer version.
///
/// Only failures of the *bundled* extraction are fatal; every update-step
/// failure falls back to the best installed runtime.
pub(crate) fn select_harness_runtime(
    resource_dir: &Path,
    data_dir: &Path,
    bundled_version: &str,
    notify: &mut dyn FnMut(UpdateNotice),
) -> Result<RuntimeSelection, String> {
    // Fatal if broken: the offline fallback must exist before anything else.
    ensure_harness_runtime(resource_dir, data_dir)?;

    let current = best_installed_runtime(data_dir, bundled_version)
        .expect("bundled runtime was just extracted and must be discoverable");

    if updates_disabled() {
        log::info!("Harness update check disabled via {DISABLE_ENV}");
        return Ok(current);
    }

    notify(UpdateNotice::Checking {
        current: current.version.clone(),
    });

    let registry = registry_base();
    let check_agent = http_agent(CHECK_TIMEOUT);
    let metadata_url = format!("{registry}/{DSH_METADATA_PATH}");
    let candidate = match fetch_json(&check_agent, &metadata_url) {
        Ok(metadata) => latest_candidate(&metadata),
        Err(error) => {
            log::warn!("Harness update check failed: {error}");
            return Ok(current);
        }
    };

    let Ok(current_version) = Version::parse(&current.version) else {
        log::warn!("installed Harness version is not semver: {}", current.version);
        return Ok(current);
    };
    let Some(target) = candidate.filter(|target| *target > current_version) else {
        log::info!("Harness {} is up to date.", current.version);
        return Ok(current);
    };

    notify(UpdateNotice::Updating {
        target: target.to_string(),
    });

    let download_agent = http_agent(DOWNLOAD_TIMEOUT);
    let updated = node_sidecar_path()
        .and_then(|node| install_updated_runtime(&node, data_dir, &registry, &download_agent, &target));
    match updated {
        Ok(selection) => {
            log::info!("Harness updated to {}.", selection.version);
            cleanup_stale_runtimes(data_dir, bundled_version, &selection.version);
            Ok(selection)
        }
        Err(error) => {
            log::warn!("Harness update to {target} failed, keeping {}: {error}", current.version);
            Ok(current)
        }
    }
}

fn updates_disabled() -> bool {
    std::env::var(DISABLE_ENV)
        .map(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

fn registry_base() -> String {
    std::env::var(REGISTRY_ENV)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_REGISTRY.to_owned())
}

fn http_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .build()
        .into()
}

fn fetch_json(agent: &ureq::Agent, url: &str) -> Result<Value, String> {
    let mut response = agent
        .get(url)
        .header("Accept", "application/vnd.npm.install-v1+json")
        .call()
        .map_err(|error| format!("请求失败（{url}）：{error}"))?;
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_METADATA_BYTES)
        .read_to_string()
        .map_err(|error| format!("读取响应失败（{url}）：{error}"))?;
    serde_json::from_str(&body).map_err(|error| format!("响应不是有效 JSON（{url}）：{error}"))
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

/// The version we track: the publisher's `latest` dist-tag, falling back to
/// the highest published version when the tag is absent or unparseable.
fn latest_candidate(metadata: &Value) -> Option<Version> {
    let tagged = metadata
        .get("dist-tags")
        .and_then(|tags| tags.get("latest"))
        .and_then(Value::as_str)
        .and_then(|version| Version::parse(version).ok());
    if tagged.is_some() {
        return tagged;
    }
    metadata
        .get("versions")
        .and_then(Value::as_object)
        .and_then(|versions| {
            versions
                .keys()
                .filter_map(|key| Version::parse(key).ok())
                .max()
        })
}

fn runtime_paths(data_dir: &Path, version: &str) -> (PathBuf, PathBuf) {
    let directory = data_dir
        .join("runtime")
        .join(format!("{version}-{}", std::env::consts::ARCH));
    let entry = directory
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("lib")
        .join("bin.js");
    (directory, entry)
}

/// Version encoded in a `runtime/<version>-<arch>` directory name.
fn installed_dir_version(name: &str) -> Option<Version> {
    let stem = name.strip_suffix(&format!("-{}", std::env::consts::ARCH))?;
    Version::parse(stem).ok()
}

/// Newest complete runtime already on disk; the bundled version is the floor.
fn best_installed_runtime(data_dir: &Path, bundled_version: &str) -> Option<RuntimeSelection> {
    let runtime_root = data_dir.join("runtime");
    let mut best = Version::parse(bundled_version).ok()?;
    let mut best_version = bundled_version.to_owned();
    if let Ok(entries) = fs::read_dir(&runtime_root) {
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else { continue };
            if !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.') {
                continue;
            }
            let Some(version) = installed_dir_version(name) else { continue };
            let (_, entry_path) = runtime_paths(data_dir, &version.to_string());
            if !entry_path.is_file() {
                continue;
            }
            if version > best {
                best = version;
                best_version = best.to_string();
            }
        }
    }
    let (_, entry) = runtime_paths(data_dir, &best_version);
    if !entry.is_file() {
        return None;
    }
    Some(RuntimeSelection {
        entry,
        version: best_version,
    })
}

fn node_sidecar_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|error| format!("无法定位应用可执行文件:{error}"))?;
    let directory = exe
        .parent()
        .ok_or_else(|| "无法定位应用安装目录。".to_owned())?;
    let path = directory.join(if cfg!(windows) { "node.exe" } else { "node" });
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!("内置 Node.js 缺失：{}", path.display()))
    }
}

/// Downloads and caches the standalone npm CLI used to install updates.
/// Returns the path to `bin/npm-cli.js`.
fn ensure_npm_cli(
    data_dir: &Path,
    registry: &str,
    agent: &ureq::Agent,
) -> Result<PathBuf, String> {
    let root = data_dir.join("npm-cli").join(NPM_CLI_VERSION);
    let cli = root.join("bin").join("npm-cli.js");
    if cli.is_file() {
        return Ok(cli);
    }

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
        fs::remove_dir_all(&staging).map_err(|error| format!("无法清理 npm CLI 暂存目录:{error}"))?;
    }
    fs::create_dir_all(&staging).map_err(|error| format!("无法创建 npm CLI 暂存目录:{error}"))?;

    let extract_result = extract_npm_package(&bytes, &staging);
    if let Err(error) = extract_result {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    let staged_cli = staging.join("bin").join("npm-cli.js");
    if !staged_cli.is_file() {
        let _ = fs::remove_dir_all(&staging);
        return Err("npm CLI 包内容不完整。".into());
    }
    if root.exists() {
        fs::remove_dir_all(&root).map_err(|error| format!("无法替换旧 npm CLI:{error}"))?;
    }
    fs::rename(&staging, &root).map_err(|error| format!("无法启用 npm CLI:{error}"))?;
    Ok(cli)
}

fn install_updated_runtime(
    node: &Path,
    data_dir: &Path,
    registry: &str,
    agent: &ureq::Agent,
    target: &Version,
) -> Result<RuntimeSelection, String> {
    let npm_cli = ensure_npm_cli(data_dir, registry, agent)?;

    let version = target.to_string();
    let runtime_root = data_dir.join("runtime");
    fs::create_dir_all(&runtime_root).map_err(|error| format!("无法创建运行时目录:{error}"))?;
    let staging = runtime_root.join(format!(".{version}-{}.staging", std::env::consts::ARCH));
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|error| format!("无法清理更新暂存目录:{error}"))?;
    }
    fs::create_dir_all(&staging).map_err(|error| format!("无法创建更新暂存目录:{error}"))?;

    let result = (|| -> Result<RuntimeSelection, String> {
        let manifest = format!(
            "{{\"name\":\"dsh-desktop-updated-runtime\",\"private\":true,\"dependencies\":{{\"@deepseek-ai/dsh\":\"{version}\"}}}}\n"
        );
        fs::write(staging.join("package.json"), manifest)
            .map_err(|error| format!("无法写入更新清单:{error}"))?;

        let cache = data_dir.join("npm-cache");
        fs::create_dir_all(&cache).map_err(|error| format!("无法创建 npm 缓存目录:{error}"))?;
        run_npm_install(node, &npm_cli, &staging, &cache, registry)?;

        let staged_entry = staging
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh")
            .join("lib")
            .join("bin.js");
        if !staged_entry.is_file() {
            return Err("更新安装结果不完整，缺少 Harness 入口。".into());
        }

        let (destination, entry) = runtime_paths(data_dir, &version);
        if destination.exists() {
            fs::remove_dir_all(&destination)
                .map_err(|error| format!("无法替换旧版本运行时:{error}"))?;
        }
        fs::rename(&staging, &destination).map_err(|error| format!("无法启用新运行时:{error}"))?;
        Ok(RuntimeSelection { entry, version })
    })();

    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn run_npm_install(
    node: &Path,
    npm_cli: &Path,
    staging: &Path,
    cache: &Path,
    registry: &str,
) -> Result<(), String> {
    let mut child = Command::new(node)
        .arg(npm_cli)
        .args([
            "install",
            "--omit=dev",
            "--no-audit",
            "--no-fund",
            "--ignore-scripts",
            "--loglevel=warn",
            "--prefix",
        ])
        .arg(staging)
        .arg("--registry")
        .arg(registry)
        .current_dir(staging)
        .env("npm_config_cache", cache)
        .env("DSH_TELEMETRY_DISABLED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("无法启动 npm 安装进程:{error}"))?;

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(format!(
                    "npm install 失败（退出码 {}）。",
                    status.code().map_or("未知".to_owned(), |code| code.to_string())
                ))
            }
            Ok(None) => {
                if started.elapsed() > INSTALL_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("npm install 超时。".into());
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(error) => return Err(format!("无法等待 npm 安装进程:{error}")),
        }
    }
}

fn verify_sha512(bytes: &[u8], integrity: &str) -> Result<(), String> {
    let expected = integrity
        .strip_prefix("sha512-")
        .ok_or_else(|| format!("不支持的校验算法:{integrity}"))?;
    let actual = BASE64.encode(Sha512::digest(bytes));
    if actual != expected {
        return Err("校验和不匹配。".into());
    }
    Ok(())
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
            return Err(format!(
                "归档包含不支持的条目类型：{}",
                stripped.display()
            ));
        }
    }
    Ok(())
}

/// Removes downloaded runtimes that are neither the bundled fallback nor the
/// version we are about to run.
fn cleanup_stale_runtimes(data_dir: &Path, bundled_version: &str, keep_version: &str) {
    let runtime_root = data_dir.join("runtime");
    let keep = [
        format!("{bundled_version}-{}", std::env::consts::ARCH),
        format!("{keep_version}-{}", std::env::consts::ARCH),
    ];
    if let Ok(entries) = fs::read_dir(&runtime_root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.') || keep.iter().any(|kept| kept == name) {
                continue;
            }
            if let Err(error) = fs::remove_dir_all(entry.path()) {
                log::warn!("failed to remove stale Harness runtime {name}: {error}");
            } else {
                log::info!("removed stale Harness runtime {name}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prefers_latest_dist_tag() {
        let metadata = json!({
            "dist-tags": { "latest": "0.1.0-rc.8", "next": "0.1.0-rc.7" },
            "versions": { "0.1.0-rc.8": {}, "0.1.0-rc.7": {} }
        });
        assert_eq!(
            latest_candidate(&metadata),
            Some(Version::parse("0.1.0-rc.8").unwrap())
        );
    }

    #[test]
    fn falls_back_to_highest_version_without_latest_tag() {
        let metadata = json!({
            "versions": { "0.1.0-rc.8": {}, "0.1.0-rc.7": {}, "0.0.1-rc.5": {} }
        });
        assert_eq!(
            latest_candidate(&metadata),
            Some(Version::parse("0.1.0-rc.7").unwrap())
        );
    }

    #[test]
    fn stable_release_outranks_prerelease() {
        let metadata = json!({
            "versions": { "0.1.0-rc.7": {}, "0.1.0": {} }
        });
        assert_eq!(
            latest_candidate(&metadata),
            Some(Version::parse("0.1.0").unwrap())
        );
        assert!(Version::parse("0.1.0").unwrap() > Version::parse("0.1.0-rc.7").unwrap());
        assert!(Version::parse("0.1.0-rc.7").unwrap() > Version::parse("0.1.0-rc.8").unwrap());
    }

    #[test]
    fn verifies_sha512_integrity() {
        let bytes = b"deepseek-harness-update-test";
        let integrity = "sha512-r4rPPYHQWLOq1LQ/U+O4RMuU4MZxYKi0DLYNMidK9EiaNLTswp4Tmovt79NSifBTOewtXikG0lIjocl5h+gWUg==";
        assert!(verify_sha512(bytes, integrity).is_ok());
        assert!(verify_sha512(b"tampered", integrity).is_err());
        assert!(verify_sha512(bytes, "sha1-deadbeef").is_err());
    }

    #[test]
    fn parses_installed_dir_names() {
        let arch = std::env::consts::ARCH;
        assert_eq!(
            installed_dir_version(&format!("0.1.0-rc.8-{arch}")),
            Some(Version::parse("0.1.0-rc.8").unwrap())
        );
        assert_eq!(installed_dir_version(&format!("node-{arch}")), None);
        assert_eq!(installed_dir_version("0.1.0-rc.8"), None);
    }

    #[test]
    fn rejects_traversal_in_npm_archives() {
        assert!(safe_archive_path(Path::new("bin/npm-cli.js")));
        assert!(!safe_archive_path(Path::new("../escape")));
    }

    /// Exercises the real download + install path against the live registry.
    /// Run explicitly with: cargo test -- --ignored
    #[test]
    #[ignore = "downloads the npm CLI and installs from the live registry"]
    fn installs_runtime_from_live_registry() {
        let node = std::env::var("DSH_TEST_NODE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(if cfg!(windows) { "node.exe" } else { "node" }));
        let data_dir =
            std::env::temp_dir().join(format!("dsh-desktop-update-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&data_dir);
        fs::create_dir_all(&data_dir).expect("test data dir should be creatable");

        let registry = registry_base();
        let agent = http_agent(DOWNLOAD_TIMEOUT);
        let cli = ensure_npm_cli(&data_dir, &registry, &agent).expect("npm CLI should bootstrap");
        assert!(cli.is_file());

        let version = Version::parse(crate::HARNESS_VERSION).unwrap();
        let selection = install_updated_runtime(&node, &data_dir, &registry, &agent, &version)
            .expect("registry install should succeed");
        assert!(selection.entry.is_file());

        let output = Command::new(node)
            .arg(&selection.entry)
            .arg("--version")
            .output()
            .expect("installed Harness should run");
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            crate::HARNESS_VERSION
        );

        fs::remove_dir_all(&data_dir).expect("test data dir should be removable");
    }
}
