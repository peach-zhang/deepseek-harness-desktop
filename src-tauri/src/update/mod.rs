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
//! - `DSH_DESKTOP_REGISTRY=<url>`     use another registry as the primary
//!   source instead of the default npmmirror.com mirror
//!
//! The official npm registry is automatically tried after a primary registry
//! failure so a temporary mirror sync gap does not strand runtime updates.

use std::{fs, path::Path};

use tauri::Manager;

#[cfg(windows)]
use std::time::Duration;

use semver::Version;

use crate::runtime::ensure_harness_runtime;
use install::runtime_paths;
use registry::{
    fetch_json, http_agent, latest_candidate, registry_candidates, tarball_url, updates_disabled,
    CHECK_TIMEOUT, DSH_METADATA_PATH,
};

pub(crate) mod install;
pub(crate) mod registry;

pub(crate) use install::{UpdateStage, UPDATE_STAGE_TOTAL};

/// Removes a directory tree, retrying with exponential back-off on Windows.
///
/// Windows frequently fails with OS error 32 (sharing violation) when another
/// process — antivirus, Windows Search indexer, or an orphaned Node.js child —
/// still holds a handle on a file inside the tree. A short retry loop is
/// usually enough to let the competing handle drain.
///
/// The retry loop only waits; it never terminates other processes. Orphaned
/// Node.js children are the backend's responsibility (`backend::stop_child`
/// kills the whole process tree), so a leaked handle here surfaces as a logged
/// warning rather than being resolved by force.
///
/// On non-Windows platforms this is a thin wrapper around [`fs::remove_dir_all`]
/// (no retries needed because Unix uses inode-based semantics).
pub(crate) fn remove_dir_all_retried(path: &Path) -> std::io::Result<()> {
    let result = fs::remove_dir_all(path);
    if result.is_ok() {
        return Ok(());
    }

    #[cfg(windows)]
    {
        const MAX_ATTEMPTS: u32 = 6;
        const BASE_DELAY: Duration = Duration::from_millis(300);

        if let Some(err) = result.as_ref().err() {
            if err.raw_os_error() != Some(32) {
                return result;
            }
        }
        log::debug!(
            "remove_dir_all contention on {}, retrying with back-off",
            path.display()
        );

        let mut attempt = 0u32;
        loop {
            std::thread::sleep(BASE_DELAY * (1 << attempt));

            if let Err(error) = fs::remove_dir_all(path) {
                if error.raw_os_error() == Some(32) && attempt + 1 < MAX_ATTEMPTS {
                    attempt += 1;
                    continue;
                }
                return Err(error);
            }
            return Ok(());
        }
    }

    #[cfg(not(windows))]
    {
        return result;
    }
}

pub(crate) struct RuntimeSelection {
    pub entry: std::path::PathBuf,
    pub version: String,
}

const FAILED_RUNTIME_FILE: &str = "failed-runtime";

pub(crate) fn mark_runtime_failed(app: &tauri::AppHandle, version: &str) {
    let Ok(data_dir) = app.path().app_data_dir() else {
        return;
    };
    let marker = format!("{}\n{version}\n", env!("CARGO_PKG_VERSION"));
    if let Err(error) = fs::write(data_dir.join(FAILED_RUNTIME_FILE), marker) {
        log::warn!("failed to quarantine Harness runtime {version}: {error}");
    } else {
        log::warn!("quarantined Harness runtime {version} after startup failure");
    }
}

fn failed_runtime(data_dir: &Path) -> Option<String> {
    let raw = fs::read_to_string(data_dir.join(FAILED_RUNTIME_FILE)).ok()?;
    let mut lines = raw.lines();
    if lines.next()? != env!("CARGO_PKG_VERSION") {
        return None;
    }
    lines.next().map(str::to_owned).filter(|value| !value.is_empty())
}

pub(crate) enum UpdateNotice {
    Checking { current: String },
    Staging { stage: UpdateStage, target: String },
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
        log::info!("Harness update check disabled via DSH_DESKTOP_UPDATE_DISABLED");
        return Ok(current);
    }

    notify(UpdateNotice::Checking {
        current: current.version.clone(),
    });
    notify(UpdateNotice::Staging {
        stage: UpdateStage::CHECKING_REGISTRY,
        target: current.version.clone(),
    });

    let registries = registry_candidates();
    let check_agent = http_agent(CHECK_TIMEOUT);
    let mut candidate = None;
    for (index, registry) in registries.iter().enumerate() {
        let metadata_url = format!("{registry}/{DSH_METADATA_PATH}");
        match fetch_json(&check_agent, &metadata_url) {
            Ok(metadata) => {
                if let Some(version) = latest_candidate(&metadata) {
                    // Keep the packument: the install step uses this registry's
                    // own tarball URL for `version` rather than making npm
                    // resolve the version a second time.
                    candidate = Some((index, version, metadata));
                    break;
                }
                log::warn!("Harness update metadata from {registry} contains no valid version");
            }
            Err(error) => log::warn!("Harness update check via {registry} failed: {error}"),
        }
    }

    let Some((registry_index, candidate, metadata)) = candidate else {
        log::warn!("Harness update check failed via every configured registry");
        return Ok(current);
    };

    let Ok(current_version) = Version::parse(&current.version) else {
        log::warn!(
            "installed Harness version is not semver: {}",
            current.version
        );
        return Ok(current);
    };
    if candidate <= current_version {
        log::info!("Harness {} is up to date.", current.version);
        return Ok(current);
    }
    let target = candidate;
    if failed_runtime(data_dir).as_deref() == Some(target.to_string().as_str()) {
        log::warn!("Harness {target} is quarantined after a previous startup failure");
        return Ok(current);
    }

    notify(UpdateNotice::Updating {
        target: target.to_string(),
    });

    let updated = (|| -> Result<RuntimeSelection, String> {
        let node = node_sidecar_path()?;
        let mut failures = Vec::new();
        for (index, registry) in registries.iter().enumerate().skip(registry_index) {
            // Only the registry whose metadata produced the candidate can
            // supply its tarball URL. A fallback registry has to resolve the
            // version through npm's normal packument lookup.
            let tarball = (index == registry_index)
                .then(|| tarball_url(&metadata, &target))
                .flatten();
            let download_agent = http_agent(install::DOWNLOAD_TIMEOUT);
            match install::install_updated_runtime(
                &node,
                data_dir,
                registry,
                &download_agent,
                &target,
                tarball.as_deref(),
                notify,
            ) {
                Ok(selection) => return Ok(selection),
                Err(error) => {
                    log::warn!("Harness update via {registry} failed: {error}");
                    failures.push(format!("{registry}: {error}"));
                }
            }
        }
        Err(format!("所有更新源均失败：{}", failures.join("；")))
    })();
    match updated {
        Ok(selection) => {
            log::info!("Harness updated to {}.", selection.version);
            cleanup_stale_runtimes(data_dir, bundled_version, &selection.version);
            Ok(selection)
        }
        Err(error) => {
            log::warn!(
                "Harness update to {target} failed, keeping {}: {error}",
                current.version
            );
            Ok(current)
        }
    }
}

/// Manual update check: resolves the newest published Harness version from the
/// configured registries without touching the installed runtime. Mirrors the
/// lookup order (and fallback) of [`select_harness_runtime`].
pub(crate) fn registry_latest_version() -> Result<Version, String> {
    let registries = registry_candidates();
    let agent = http_agent(CHECK_TIMEOUT);
    let mut last_error: Option<String> = None;
    for registry in &registries {
        let metadata_url = format!("{registry}/{DSH_METADATA_PATH}");
        match fetch_json(&agent, &metadata_url) {
            Ok(metadata) => {
                if let Some(version) = latest_candidate(&metadata) {
                    return Ok(version);
                }
                last_error = Some(format!("registry 元数据中没有可用版本（{registry}）"));
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| "没有可用的 registry 配置".to_owned()))
}

fn node_sidecar_path() -> Result<std::path::PathBuf, String> {
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

/// Version encoded in a `runtime/<version>-<arch>` directory name.
fn installed_dir_version(name: &str) -> Option<Version> {
    let stem = name.strip_suffix(&format!("-{}", std::env::consts::ARCH))?;
    Version::parse(stem).ok()
}

/// Newest complete runtime already on disk; the bundled version is the floor.
fn best_installed_runtime(data_dir: &Path, bundled_version: &str) -> Option<RuntimeSelection> {
    let runtime_root = data_dir.join("runtime");
    let failed = failed_runtime(data_dir);
    let mut best = Version::parse(bundled_version).ok()?;
    let mut best_version = bundled_version.to_owned();
    if let Ok(entries) = fs::read_dir(&runtime_root) {
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let Some(version) = installed_dir_version(name) else {
                continue;
            };
            if failed.as_deref() == Some(version.to_string().as_str()) {
                continue;
            }
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
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with('.') || keep.iter().any(|kept| kept == name) {
                continue;
            }
            if let Err(error) = remove_dir_all_retried(&entry.path()) {
                log::warn!("failed to remove stale Harness runtime {name}: {error}");
            } else {
                log::info!("removed stale Harness runtime {name}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, process::Command};

    use super::*;

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
    fn skips_quarantined_runtime() {
        let data_dir = std::env::temp_dir().join(format!(
            "dsh-desktop-quarantine-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&data_dir);
        for version in ["0.1.0-rc.7", "0.1.0-rc.8"] {
            let (_, entry) = runtime_paths(&data_dir, version);
            fs::create_dir_all(entry.parent().unwrap()).unwrap();
            fs::write(entry, "entry").unwrap();
        }
        fs::write(
            data_dir.join(FAILED_RUNTIME_FILE),
            format!("{}\n0.1.0-rc.8\n", env!("CARGO_PKG_VERSION")),
        )
        .unwrap();

        let selected = best_installed_runtime(&data_dir, "0.1.0-rc.7").unwrap();
        assert_eq!(selected.version, "0.1.0-rc.7");

        fs::remove_dir_all(&data_dir).unwrap();
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

        let registry = registry_candidates()
            .into_iter()
            .next()
            .expect("at least one registry should be configured");
        let agent = http_agent(install::DOWNLOAD_TIMEOUT);
        let cli = install::ensure_npm_cli(&data_dir, &registry, &agent)
            .expect("npm CLI should bootstrap");
        assert!(cli.is_file());

        let version = Version::parse(crate::HARNESS_VERSION).unwrap();
        // Resolve the tarball URL exactly as the real update path does, so this
        // test covers the preferred tarball-URL install rather than only the
        // version-spec fallback.
        let metadata = fetch_json(&agent, &format!("{registry}/{DSH_METADATA_PATH}"))
            .expect("registry metadata should be fetchable");
        let tarball = tarball_url(&metadata, &version);
        assert!(tarball.is_some(), "registry should advertise a tarball URL");

        // `DSH_TEST_VERSION_SPEC=1` exercises the version-spec fallback (the
        // path npm can fail as ETARGET) instead of the default tarball path.
        let tarball = if std::env::var("DSH_TEST_VERSION_SPEC").is_ok() {
            None
        } else {
            tarball
        };

        let selection = install::install_updated_runtime(
            &node,
            &data_dir,
            &registry,
            &agent,
            &version,
            tarball.as_deref(),
            &mut |_| {},
        )
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
