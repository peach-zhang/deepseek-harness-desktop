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

use std::{fs, path::Path};

use semver::Version;

use crate::runtime::ensure_harness_runtime;
use install::runtime_paths;
use registry::{
    http_agent, latest_candidate, registry_base, updates_disabled, fetch_json,
    CHECK_TIMEOUT, DSH_METADATA_PATH,
};

pub(crate) mod install;
pub(crate) mod registry;

pub(crate) use install::{UpdateStage, UPDATE_STAGE_TOTAL};

pub(crate) struct RuntimeSelection {
    pub entry: std::path::PathBuf,
    pub version: String,
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

    let download_agent = http_agent(install::DOWNLOAD_TIMEOUT);
    let updated = node_sidecar_path().and_then(|node| {
        install::install_updated_runtime(
            &node,
            data_dir,
            &registry,
            &download_agent,
            &target,
            notify,
        )
    });
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

fn node_sidecar_path() -> Result<std::path::PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("无法定位应用可执行文件:{error}"))?;
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
fn best_installed_runtime(
    data_dir: &Path,
    bundled_version: &str,
) -> Option<RuntimeSelection> {
    let runtime_root = data_dir.join("runtime");
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

    /// Exercises the real download + install path against the live registry.
    /// Run explicitly with: cargo test -- --ignored
    #[test]
    #[ignore = "downloads the npm CLI and installs from the live registry"]
    fn installs_runtime_from_live_registry() {
        let node = std::env::var("DSH_TEST_NODE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(if cfg!(windows) { "node.exe" } else { "node" }));
        let data_dir = std::env::temp_dir().join(format!(
            "dsh-desktop-update-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&data_dir);
        fs::create_dir_all(&data_dir).expect("test data dir should be creatable");

        let registry = registry_base();
        let agent = http_agent(install::DOWNLOAD_TIMEOUT);
        let cli = install::ensure_npm_cli(&data_dir, &registry, &agent)
            .expect("npm CLI should bootstrap");
        assert!(cli.is_file());

        let version = Version::parse(crate::HARNESS_VERSION).unwrap();
        let selection = install::install_updated_runtime(
            &node, &data_dir, &registry, &agent, &version, &mut |_| {},
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
