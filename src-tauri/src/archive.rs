//! Safe tar.gz archive extraction with path-traversal guards.
//!
//! Shared by the bundled Harness runtime extraction, npm package unpacking,
//! and bundled plugin installation. Every entry is validated against directory
//! traversal so extraction can never escape the target staging directory.

use std::{
    fs::File,
    path::{Component, Path},
};

use flate2::read::GzDecoder;

/// Unpack a gzip tarball into `staging`, validating every path and link
/// against traversal so extraction can never escape the staging directory.
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

/// Returns true when every component of `path` is `Normal` or `CurDir` —
/// i.e. no `..`, absolute roots, or drive prefixes.
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
    use std::path::Path;

    use super::*;

    #[test]
    fn accepts_safe_archive_paths() {
        assert!(safe_archive_path(Path::new("node_modules/pkg/index.js")));
        assert!(safe_archive_path(Path::new("./relative/file.txt")));
    }

    #[test]
    fn rejects_archive_path_traversal() {
        assert!(!safe_archive_path(Path::new("../outside")));
        assert!(!safe_archive_path(Path::new("a/../../escape")));
    }

    #[test]
    fn accepts_link_staying_inside() {
        assert!(archive_link_stays_inside(
            Path::new("node_modules/pkg/node_modules/dep"),
            Path::new("../../../dep/node_modules/dep")
        ));
    }

    #[test]
    fn rejects_link_escaping() {
        assert!(!archive_link_stays_inside(
            Path::new("node_modules/pkg/link"),
            Path::new("../../../outside")
        ));
    }
}
