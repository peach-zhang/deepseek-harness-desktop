//! Bundled Harness runtime extraction.
//!
//! On first launch (or when the app data directory is missing), the bundled
//! `dsh-runtime.tar.gz` is unpacked into `<data_dir>/runtime/<version>-<arch>/`.
//! Archive entries are validated against path traversal to keep the extraction
//! sandboxed.

use std::{
    fs::{self, File},
    path::{Component, Path, PathBuf},
};

use flate2::read::GzDecoder;

pub(crate) fn ensure_harness_runtime(resource_dir: &Path, data_dir: &Path) -> Result<PathBuf, String> {
    let runtime_root = data_dir.join("runtime");
    let runtime_id = format!("{}-{}", crate::HARNESS_VERSION, std::env::consts::ARCH);
    let destination = runtime_root.join(&runtime_id);
    let entry = destination
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("lib")
        .join("bin.js");
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

    fs::create_dir_all(&runtime_root).map_err(|error| format!("无法创建运行时目录：{error}"))?;
    let staging = runtime_root.join(format!(".{runtime_id}.staging"));
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|error| format!("无法清理未完成的运行时：{error}"))?;
    }
    fs::create_dir_all(&staging).map_err(|error| format!("无法创建运行时暂存目录：{error}"))?;

    let unpack_result = extract_guarded_archive(&archive_path, &staging);

    if let Err(error) = unpack_result {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    if !staging
        .join("node_modules")
        .join("@deepseek-ai")
        .join("dsh")
        .join("lib")
        .join("bin.js")
        .is_file()
    {
        let _ = fs::remove_dir_all(&staging);
        return Err("Harness 运行时归档不完整。".into());
    }
    if destination.exists() {
        fs::remove_dir_all(&destination).map_err(|error| format!("无法替换旧运行时：{error}"))?;
    }
    fs::rename(&staging, &destination)
        .map_err(|error| format!("无法启用 Harness 运行时：{error}"))?;
    Ok(entry)
}

/// Unpack a guarded gzip tarball into `staging`, validating every path and
/// link against traversal so extraction can never escape the staging
/// directory. Shared by the bundled Harness runtime and bundled plugins.
pub(crate) fn extract_guarded_archive(archive_path: &Path, staging: &Path) -> Result<(), String> {
    let archive_file =
        File::open(archive_path).map_err(|error| format!("无法读取归档：{error}"))?;
    let decoder = GzDecoder::new(archive_file);
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|error| format!("无法读取归档目录：{error}"))?;
    for entry_result in entries {
        let mut archive_entry =
            entry_result.map_err(|error| format!("无法读取归档条目：{error}"))?;
        let entry_path = archive_entry
            .path()
            .map_err(|error| format!("归档包含无效路径：{error}"))?
            .into_owned();
        if !safe_archive_path(&entry_path) {
            return Err(format!("归档包含越界路径：{}", entry_path.display()));
        }
        let entry_type = archive_entry.header().entry_type();
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            let link = archive_entry
                .link_name()
                .map_err(|error| format!("归档包含无效链接：{error}"))?
                .ok_or_else(|| "归档包含空链接。".to_owned())?;
            if !archive_link_stays_inside(&entry_path, &link) {
                return Err(format!("归档链接越界：{}", entry_path.display()));
            }
        }
        let unpacked = archive_entry
            .unpack_in(staging)
            .map_err(|error| format!("无法解压归档：{error}"))?;
        if !unpacked {
            return Err(format!("归档条目越界：{}", entry_path.display()));
        }
    }
    Ok(())
}

pub(crate) fn safe_archive_path(path: &Path) -> bool {
    path.components()
        .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
}

fn archive_link_stays_inside(entry_path: &Path, link: &Path) -> bool {
    if link.is_absolute() || !safe_archive_path(entry_path) {
        return false;
    }
    let mut depth = entry_path.parent().map_or(0, |parent| {
        parent
            .components()
            .filter(|part| matches!(part, Component::Normal(_)))
            .count()
    });
    for part in link.components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use super::*;

    #[test]
    fn rejects_archive_path_traversal() {
        assert!(safe_archive_path(Path::new("node_modules/pkg/index.js")));
        assert!(!safe_archive_path(Path::new("../outside")));
        assert!(archive_link_stays_inside(
            Path::new("node_modules/pkg/node_modules/dep"),
            Path::new("../../../dep/node_modules/dep")
        ));
        assert!(!archive_link_stays_inside(
            Path::new("node_modules/pkg/link"),
            Path::new("../../../outside")
        ));
    }

    #[test]
    fn extracts_prepared_runtime_archive_when_present() {
        let resource_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        if !resource_dir.join("runtime/dsh-runtime.tar.gz").is_file() {
            return;
        }
        let data_dir =
            std::env::temp_dir().join(format!("dsh-desktop-runtime-test-{}", std::process::id()));
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
                    .is_some_and(|name| name.starts_with("node-") && !name.ends_with(".json"))
            })
            .expect("prepared Node.js sidecar should exist");
        let version = Command::new(bundled_node)
            .arg(&entry)
            .arg("--version")
            .output()
            .expect("extracted Harness should run with bundled Node.js");
        assert!(version.status.success());
        assert_eq!(
            String::from_utf8_lossy(&version.stdout).trim(),
            "0.1.0-rc.6"
        );
        fs::remove_dir_all(&data_dir).expect("temporary runtime should be removable");
    }
}
