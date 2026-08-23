//! Bundled Harness runtime extraction.
//!
//! On first launch (or when the app data directory is missing), the bundled
//! `dsh-runtime.tar.gz` is unpacked into `<data_dir>/runtime/<version>-<arch>/`.
//! Archive entries are validated against path traversal to keep the extraction
//! sandboxed.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::archive::extract_guarded_archive;
use crate::update::install::runtime_entry;

pub(crate) fn ensure_harness_runtime(
    resource_dir: &Path,
    data_dir: &Path,
) -> Result<PathBuf, String> {
    let runtime_root = data_dir.join("runtime");
    let runtime_id = format!("{}-{}", crate::HARNESS_VERSION, std::env::consts::ARCH);
    let destination = runtime_root.join(&runtime_id);
    let entry = runtime_entry(&destination);
    if entry.is_file() {
        return Ok(entry);
    }

    let archive_path = resource_dir.join("runtime").join("dsh-runtime.tar.gz");
    if !archive_path.is_file() {
        return Err(format!(
            "安装包缺少 Harness 运行时：{}",
            archive_path.display()
        ));
    }

    fs::create_dir_all(&runtime_root)
        .map_err(|error| format!("无法创建运行时目录：{error}"))?;
    let staging = runtime_root.join(format!(".{runtime_id}.staging"));
    if staging.exists() {
        crate::update::remove_dir_all_retried(&staging)
            .map_err(|error| format!("无法清理未完成的运行时：{error}"))?;
    }
    fs::create_dir_all(&staging)
        .map_err(|error| format!("无法创建运行时暂存目录：{error}"))?;

    let unpack_result = extract_guarded_archive(&archive_path, &staging);

    if let Err(error) = unpack_result {
        let _ = crate::update::remove_dir_all_retried(&staging);
        return Err(error);
    }
    if !runtime_entry(&staging).is_file() {
        let _ = crate::update::remove_dir_all_retried(&staging);
        return Err("Harness 运行时归档不完整。".into());
    }
    if destination.exists() {
        crate::update::remove_dir_all_retried(&destination)
            .map_err(|error| format!("无法替换旧运行时：{error}"))?;
    }
    fs::rename(&staging, &destination)
        .map_err(|error| format!("无法启用 Harness 运行时：{error}"))?;
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use super::*;

    #[test]
    fn extracts_prepared_runtime_archive_when_present() {
        let resource_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        if !resource_dir.join("runtime/dsh-runtime.tar.gz").is_file() {
            return;
        }
        let data_dir = std::env::temp_dir()
            .join(format!("dsh-desktop-runtime-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&data_dir);
        let entry = ensure_harness_runtime(resource_dir, &data_dir)
            .expect("prepared runtime archive should extract safely");
        assert!(entry.is_file());
        let bundled_node = fs::read_dir(resource_dir.join("runtime"))
            .expect("prepared runtime directory should be readable")
            .filter_map(Result::ok)
            .map(|item| item.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("node-") && !name.ends_with(".json")
                    })
            })
            .expect("prepared Node.js sidecar should exist");
        let version = Command::new(bundled_node)
            .arg(&entry)
            .arg("--version")
            .output()
            .expect("extracted Harness should run with bundled Node.js");
        assert!(version.status.success());
        // Read the expected version from the manifest rather than hardcoding
        // it, so the test survives archive regeneration without code changes.
        let manifest_path = resource_dir.join("runtime").join("runtime-manifest.json");
        let manifest: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&manifest_path).expect("manifest should exist"))
                .expect("manifest should be valid JSON");
        let expected = manifest["harness"].as_str().expect("manifest should have harness version");
        assert_eq!(String::from_utf8_lossy(&version.stdout).trim(), expected);
        fs::remove_dir_all(&data_dir)
            .expect("temporary runtime should be removable");
    }
}
