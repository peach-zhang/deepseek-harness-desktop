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

use crate::archive::safe_archive_path;
use super::registry::fetch_json;
use super::RuntimeSelection;

/// Windows-only: prevents a visible console window from flashing when spawning
/// the bundled Node.js sidecar from the GUI process.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const NPM_PACKAGE: &str = "npm";
const NPM_CLI_VERSION: &str = "11.16.0";
pub(crate) const UPDATE_STAGE_TOTAL: usize = 4;
pub(crate) const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
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
    verify_sha512(&bytes, integrity)
        .map_err(|error| format!("npm CLI 包校验失败:{error}"))?;

    let staging = data_dir
        .join("npm-cli")
        .join(format!(".{NPM_CLI_VERSION}.staging"));
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .map_err(|error| format!("无法清理 npm CLI 暂存目录:{error}"))?;
    }
    fs::create_dir_all(&staging)
        .map_err(|error| format!("无法创建 npm CLI 暂存目录:{error}"))?;

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
        fs::remove_dir_all(&root)
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
    notify: &mut dyn FnMut(super::UpdateNotice),
) -> Result<RuntimeSelection, String> {
    use super::UpdateNotice;

    let version = target.to_string();

    notify(UpdateNotice::Staging {
        stage: UpdateStage::DOWNLOADING_NPM,
        target: version.clone(),
    });
    let npm_cli = ensure_npm_cli(data_dir, registry, agent)?;

    let runtime_root = data_dir.join("runtime");
    fs::create_dir_all(&runtime_root)
        .map_err(|error| format!("无法创建运行时目录:{error}"))?;
    let staging = runtime_root.join(format!(".{version}-{}.staging", std::env::consts::ARCH));
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .map_err(|error| format!("无法清理更新暂存目录:{error}"))?;
    }
    fs::create_dir_all(&staging)
        .map_err(|error| format!("无法创建更新暂存目录:{error}"))?;

    let result = (|| -> Result<RuntimeSelection, String> {
        let manifest = format!(
            "{{\"name\":\"dsh-desktop-updated-runtime\",\"private\":true,\"dependencies\":{{\"@deepseek-ai/dsh\":\"{version}\"}}}}\n"
        );
        fs::write(staging.join("package.json"), manifest)
            .map_err(|error| format!("无法写入更新清单:{error}"))?;

        let cache = data_dir.join("npm-cache");
        fs::create_dir_all(&cache)
            .map_err(|error| format!("无法创建 npm 缓存目录:{error}"))?;

        notify(UpdateNotice::Staging {
            stage: UpdateStage::INSTALLING_HARNESS,
            target: version.clone(),
        });
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

        notify(UpdateNotice::Staging {
            stage: UpdateStage::FINALIZING,
            target: version.clone(),
        });
        let (destination, entry) = runtime_paths(data_dir, &version);
        if destination.exists() {
            fs::remove_dir_all(&destination)
                .map_err(|error| format!("无法替换旧版本运行时:{error}"))?;
        }
        fs::rename(&staging, &destination)
            .map_err(|error| format!("无法启用新运行时:{error}"))?;
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
    let mut cmd = Command::new(node);
    cmd.arg(npm_cli)
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
        .stderr(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let mut child = cmd
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

pub(crate) fn runtime_paths(data_dir: &Path, version: &str) -> (PathBuf, PathBuf) {
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
            return Err(format!(
                "归档包含非 package 路径：{}",
                raw_path.display()
            ));
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
            fs::create_dir_all(&target)
                .map_err(|error| format!("无法创建目录:{error}"))?;
        } else if entry_type.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("无法创建目录:{error}"))?;
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
}
