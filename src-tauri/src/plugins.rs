//! Bundled Cordis plugin installation for the Harness `web` profile.
//!
//! Every npm package under the repo's `src-tauri/plugins/` directory is a
//! Cordis plugin: its `package.json` declares `"dsh": { "bundle": { "patch":
//! "./cordis.patch.yml" } }`, and that patch file inserts the plugin's
//! entries into the profile composition. `scripts/prepare-runtime.mjs` packs
//! those directories into `plugins.tar.gz`, which ships inside the installer
//! next to the Harness runtime.
//!
//! On launch we install the bundled plugins into
//! `<data_dir>/harness/profiles/web` (the profile `dsh web` boots): each
//! package is copied into the profile's `node_modules`, recorded as a
//! dependency, and — when it declares a bundle patch — appended to the
//! profile's `dsh.profile.bundles` layer list so it is enabled by default,
//! exactly like `dsh plugin --profile web add <package>` would do.
//!
//! Synchronization is idempotent: a marker file
//! (`.dsh-desktop-plugin-sync.json`) records the installed `name -> version`
//! set, and a launch only re-syncs when the bundled set changed or an
//! installed package directory went missing. Sync failures are reported to
//! the caller, which logs them and keeps booting — the Harness simply runs
//! without the bundled plugins.
//!
//! Environment override for development builds (which do not bundle the
//! archive): `DSH_DESKTOP_PLUGINS_DIR=<path>` points at a directory holding
//! the same package subdirectories.

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use serde_json::{json, Map, Value};

const BUNDLED_ARCHIVE_NAME: &str = "plugins.tar.gz";
const PLUGINS_DIR_ENV: &str = "DSH_DESKTOP_PLUGINS_DIR";
const PROFILE_NAME: &str = "web";
const MARKER_FILENAME: &str = ".dsh-desktop-plugin-sync.json";
const PROFILE_PATCH_TEMPLATE: &str = "# Managed by DSH Desktop: bundled plugins ship their own patch layers.\n# Add your own profile entries below.\n[]\n";
const PROFILE_PNPM_WORKSPACE: &str = "packages:\n  - .\n\nnodeLinker: hoisted\nautoInstallPeers: false\n";
const WEB_PROFILE_BUNDLES: &[&str] = &["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app"];

/// One bundled plugin package, validated enough to install safely.
struct PluginPackage {
    name: String,
    version: String,
    dir: PathBuf,
    /// Relative path of the bundle patch inside the package, when declared.
    bundle_patch: Option<PathBuf>,
}

/// Where the bundled plugin packages come from this launch.
enum BundledSource {
    /// No bundled plugins available (no archive, no env override).
    None,
    /// Loose package directories (dev override via `DSH_DESKTOP_PLUGINS_DIR`).
    Directory(PathBuf),
    /// Archive extracted into a staging directory to remove after the sync.
    Staging(PathBuf),
}

/// Install (or refresh) the bundled plugins for the Harness `web` profile.
///
/// `resource_dir` is the Tauri resource directory holding the bundled
/// `runtime/plugins.tar.gz`; `data_dir` is the per-user app data directory;
/// `dsh_home` is the Harness home the backend boots (`<data_dir>/harness`).
pub(crate) fn sync_bundled_plugins(
    resource_dir: &Path,
    data_dir: &Path,
    dsh_home: &Path,
) -> Result<(), String> {
    let source = match bundled_source(resource_dir, data_dir) {
        Ok(source) => source,
        Err(error) => {
            // The archive is optional; a broken override must not hide a
            // working install, but nothing here is fatal.
            log::warn!("bundled plugin source unavailable: {error}");
            return Ok(());
        }
    };
    let result = match &source {
        BundledSource::None => return Ok(()),
        BundledSource::Directory(dir) => sync_from_directory(dir, dsh_home),
        BundledSource::Staging(dir) => sync_from_directory(dir, dsh_home),
    };
    if let BundledSource::Staging(dir) = &source {
        let _ = fs::remove_dir_all(dir);
    }
    result
}

fn bundled_source(resource_dir: &Path, data_dir: &Path) -> Result<BundledSource, String> {
    let archive_path = resource_dir.join("runtime").join(BUNDLED_ARCHIVE_NAME);
    if archive_path.is_file() {
        let staging = data_dir.join("plugins-bundled");
        if staging.exists() {
            fs::remove_dir_all(&staging)
                .map_err(|error| format!("无法清理插件暂存目录:{error}"))?;
        }
        fs::create_dir_all(&staging)
            .map_err(|error| format!("无法创建插件暂存目录:{error}"))?;
        crate::runtime::extract_guarded_archive(&archive_path, &staging)?;
        return Ok(BundledSource::Staging(staging));
    }
    if let Ok(dir) = std::env::var(PLUGINS_DIR_ENV) {
        let dir = PathBuf::from(dir);
        if dir.is_dir() {
            log::info!(
                "using bundled plugins from {PLUGINS_DIR_ENV}={}",
                dir.display()
            );
            return Ok(BundledSource::Directory(dir));
        }
        log::warn!(
            "{PLUGINS_DIR_ENV} points to a missing directory: {}",
            dir.display()
        );
    }
    Ok(BundledSource::None)
}

/// The core sync: read the package set under `plugins_dir`, compare it with
/// the recorded marker, and install when anything changed or is missing.
fn sync_from_directory(plugins_dir: &Path, dsh_home: &Path) -> Result<(), String> {
    let packages = read_packages(plugins_dir)?;
    let expected = expected_marker(&packages);

    let profile_dir = dsh_home.join("profiles").join(PROFILE_NAME);
    let node_modules = profile_dir.join("node_modules");
    let marker_path = profile_dir.join(MARKER_FILENAME);

    let marker_matches = read_marker(&marker_path)
        .map(|stored| stored == expected)
        .unwrap_or(false);
    let all_present = packages
        .iter()
        .all(|pkg| package_destination(&node_modules, &pkg.name).is_dir());
    if marker_matches && all_present {
        return Ok(());
    }

    let mut manifest = ensure_profile(&profile_dir)?;

    for pkg in &packages {
        install_package(pkg, &node_modules)?;
        record_dependency(&mut manifest, pkg);
        match &pkg.bundle_patch {
            Some(patch) if !pkg.dir.join(patch).is_file() => {
                log::warn!(
                    "bundled plugin {} declares dsh.bundle.patch {} but the file is missing; \
                     installed as a plain dependency, not enabled",
                    pkg.name,
                    patch.display()
                );
            }
            Some(_) => enable_bundle(&mut manifest, &pkg.name),
            None => log::warn!(
                "bundled plugin {} declares no dsh.bundle.patch; \
                 installed as a plain dependency, not enabled",
                pkg.name
            ),
        }
    }

    write_json_file(&profile_dir.join("package.json"), &manifest)?;
    write_json_file(&marker_path, &expected)?;
    log::info!(
        "installed {} bundled plugin(s) into the {PROFILE_NAME} profile",
        packages.len()
    );
    Ok(())
}

/// Read every plugin package under `plugins_dir`, validating names, versions
/// and bundle-patch declarations. Sorted by name for deterministic markers.
fn read_packages(plugins_dir: &Path) -> Result<Vec<PluginPackage>, String> {
    let entries = fs::read_dir(plugins_dir)
        .map_err(|error| format!("无法读取插件目录 {}:{error}", plugins_dir.display()))?;
    let mut packages = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let manifest_path = dir.join("package.json");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest = parse_json_file(&manifest_path)?;
        let name = manifest
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| format!("插件 {} 缺少有效的 name 字段", dir.display()))?;
        validate_package_name(name)?;
        if packages.iter().any(|pkg: &PluginPackage| pkg.name == name) {
            return Err(format!("插件重名:{name}"));
        }
        let version = manifest
            .get("version")
            .and_then(Value::as_str)
            .filter(|version| !version.is_empty())
            .ok_or_else(|| format!("插件 {name} 缺少有效的 version 字段"))?;
        let bundle_patch = manifest
            .get("dsh")
            .and_then(|dsh| dsh.get("bundle"))
            .and_then(|bundle| bundle.get("patch"))
            .and_then(Value::as_str)
            .map(|patch| validate_patch_path(name, patch))
            .transpose()?;
        packages.push(PluginPackage {
            name: name.to_owned(),
            version: version.to_owned(),
            dir,
            bundle_patch,
        });
    }
    packages.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(packages)
}

/// Reject npm names that could escape the profile's `node_modules` or clash
/// with our staging layout (dot-prefixed first segment).
fn validate_package_name(name: &str) -> Result<(), String> {
    for segment in name.split('/') {
        if segment.is_empty()
            || segment == "."
            || segment == ".."
            || segment.contains('\\')
            || segment.starts_with('.')
        {
            return Err(format!("插件名非法:{name}"));
        }
    }
    Ok(())
}

/// The bundle patch must stay inside the package: a relative, non-escaping path.
fn validate_patch_path(name: &str, patch: &str) -> Result<PathBuf, String> {
    let path = Path::new(patch);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "插件 {name} 的 dsh.bundle.patch 必须是包内相对路径:{patch}"
        ));
    }
    Ok(path.to_path_buf())
}

/// Create the profile directory and its missing files (mirroring the
/// Harness's own `initProfile`), returning the existing or fresh manifest.
fn ensure_profile(profile_dir: &Path) -> Result<Value, String> {
    fs::create_dir_all(profile_dir)
        .map_err(|error| format!("无法创建 profile 目录 {}:{error}", profile_dir.display()))?;

    let manifest_path = profile_dir.join("package.json");
    let manifest = if manifest_path.is_file() {
        parse_json_file(&manifest_path)?
    } else {
        let manifest = json!({
            "name": format!("dsh-profile-{PROFILE_NAME}"),
            "private": true,
            "dependencies": {},
            "dsh": { "profile": { "bundles": WEB_PROFILE_BUNDLES } }
        });
        write_json_file(&manifest_path, &manifest)?;
        manifest
    };

    let patch_path = profile_dir.join("cordis.patch.yml");
    if !patch_path.is_file() {
        fs::write(&patch_path, PROFILE_PATCH_TEMPLATE)
            .map_err(|error| format!("无法创建 profile 补丁文件:{error}"))?;
    }
    let workspace_path = profile_dir.join("pnpm-workspace.yaml");
    if !workspace_path.is_file() {
        fs::write(&workspace_path, PROFILE_PNPM_WORKSPACE)
            .map_err(|error| format!("无法创建 pnpm 工作区文件:{error}"))?;
    }
    Ok(manifest)
}

/// Copy one plugin package into the profile's `node_modules`, replacing any
/// previous copy atomically (stage, then rename).
fn install_package(pkg: &PluginPackage, node_modules: &Path) -> Result<(), String> {
    let destination = package_destination(node_modules, &pkg.name);
    let staging = package_destination(node_modules, &format!(".{}.staging", pkg.name));
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .map_err(|error| format!("无法清理插件暂存目录:{error}"))?;
    }
    if let Some(parent) = staging.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("无法创建插件目录 {}:{error}", parent.display()))?;
    }
    copy_dir(&pkg.dir, &staging)
        .map_err(|error| format!("无法复制插件 {}:{error}", pkg.name))?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("无法创建插件目录 {}:{error}", parent.display()))?;
    }
    if destination.exists() {
        fs::remove_dir_all(&destination)
            .map_err(|error| format!("无法替换插件 {}:{error}", pkg.name))?;
    }
    fs::rename(&staging, &destination)
        .map_err(|error| format!("无法启用插件 {}:{error}", pkg.name))?;
    Ok(())
}

/// `node_modules/<name>` for an npm name, splitting scoped names into segments.
fn package_destination(node_modules: &Path, name: &str) -> PathBuf {
    name.split('/')
        .fold(node_modules.to_path_buf(), |path, segment| path.join(segment))
}

/// Recursive directory copy preserving files and (best-effort) symlinks.
fn copy_dir(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination)
        .map_err(|error| format!("无法创建目录 {}:{error}", destination.display()))?;
    for entry in fs::read_dir(source)
        .map_err(|error| format!("无法读取 {}:{error}", source.display()))?
        .flatten()
    {
        let target = destination.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|error| format!("无法读取条目类型:{error}"))?;
        if file_type.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target)
                .map_err(|error| format!("无法复制 {}:{error}", entry.path().display()))?;
        } else if file_type.is_symlink() {
            let link = fs::read_link(entry.path())
                .map_err(|error| format!("无法读取符号链接 {}:{error}", entry.path().display()))?;
            #[cfg(windows)]
            std::os::windows::fs::symlink_dir(&link, &target)
                .or_else(|_| std::os::windows::fs::symlink_file(&link, &target))
                .map_err(|error| format!("无法创建符号链接 {}:{error}", target.display()))?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&link, &target)
                .map_err(|error| format!("无法创建符号链接 {}:{error}", target.display()))?;
        }
    }
    Ok(())
}

/// Record the package as a profile dependency. The bundled set is
/// authoritative, so a version change overwrites the recorded version.
fn record_dependency(manifest: &mut Value, pkg: &PluginPackage) {
    let Some(object) = manifest.as_object_mut() else {
        log::warn!("profile 清单不是对象，跳过依赖记录");
        return;
    };
    match object.get_mut("dependencies") {
        None => {
            object.insert(
                "dependencies".into(),
                json!({ pkg.name.clone(): pkg.version.clone() }),
            );
        }
        Some(Value::Object(map)) => {
            map.insert(pkg.name.clone(), Value::String(pkg.version.clone()));
        }
        Some(_) => log::warn!(
            "profile 的 dependencies 不是对象，未记录插件 {}",
            pkg.name
        ),
    }
}

/// Append the plugin to `dsh.profile.bundles` so its patch layer is enabled.
fn enable_bundle(manifest: &mut Value, name: &str) {
    let Some(object) = manifest.as_object_mut() else {
        return;
    };
    let bundles = object
        .get_mut("dsh")
        .and_then(|dsh| dsh.as_object_mut())
        .and_then(|dsh| dsh.get_mut("profile"))
        .and_then(|profile| profile.as_object_mut())
        .and_then(|profile| profile.get_mut("bundles"));
    match bundles {
        Some(Value::Array(list)) => {
            if !list.iter().any(|item| item.as_str() == Some(name)) {
                list.push(Value::String(name.into()));
            }
        }
        _ => log::warn!(
            "profile 的 dsh.profile.bundles 缺失或不是数组，无法启用插件 {}",
            name
        ),
    }
}

/// The marker value for a package set: `{ "plugins": { name: version, ... } }`.
fn expected_marker(packages: &[PluginPackage]) -> Value {
    let mut plugins = Map::new();
    for pkg in packages {
        plugins.insert(pkg.name.clone(), Value::String(pkg.version.clone()));
    }
    json!({ "plugins": Value::Object(plugins) })
}

fn read_marker(path: &Path) -> Option<Value> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn parse_json_file(path: &Path) -> Result<Value, String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("无法读取 {}:{error}", path.display()))?;
    serde_json::from_str(&raw).map_err(|error| format!("{} 不是有效 JSON:{error}", path.display()))
}

fn write_json_file(path: &Path, value: &Value) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(value)
        .map_err(|error| format!("无法序列化 {}:{error}", path.display()))?;
    fs::write(path, format!("{raw}\n"))
        .map_err(|error| format!("无法写入 {}:{error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn sample_plugin(dir: &Path, name: &str, version: &str, with_bundle: bool) {
        let bundle = if with_bundle {
            r#","dsh": {"bundle": {"patch": "./cordis.patch.yml"}}"#
        } else {
            ""
        };
        write(
            &dir.join("package.json"),
            &format!(r#"{{"name":"{name}","version":"{version}"{bundle}}}"#),
        );
        if with_bundle {
            write(
                &dir.join("cordis.patch.yml"),
                "# bundled plugin layer\n- id: demo\n  config: {}\n",
            );
        }
        write(&dir.join("lib.js"), "export default {}\n");
    }

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dsh-desktop-plugins-{label}-{}", std::process::id()))
    }

    #[test]
    fn validates_package_names() {
        assert!(validate_package_name("demo-plugin").is_ok());
        assert!(validate_package_name("@scope/name").is_ok());
        assert!(validate_package_name("").is_err());
        assert!(validate_package_name("..").is_err());
        assert!(validate_package_name("a/../b").is_err());
        assert!(validate_package_name("a\\b").is_err());
        assert!(validate_package_name(".hidden").is_err());
    }

    #[test]
    fn validates_patch_paths() {
        assert!(validate_patch_path("p", "./cordis.patch.yml").is_ok());
        assert!(validate_patch_path("p", "cordis.patch.yml").is_ok());
        assert!(validate_patch_path("p", "/abs/path.yml").is_err());
        assert!(validate_patch_path("p", "../escape.yml").is_err());
        assert!(validate_patch_path("p", "C:\\abs.yml").is_err());
    }

    #[test]
    fn installs_scoped_packages_into_nested_node_modules() {
        let temp = temp_dir("scoped");
        let _ = fs::remove_dir_all(&temp);
        let plugins = temp.join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        sample_plugin(
            &plugins.join("deepseek-usage-plugin"),
            "@deepseek-ai/dsh-plugin-deepseek-usage",
            "0.1.0",
            true,
        );
        let dsh_home = temp.join("harness");

        sync_from_directory(&plugins, &dsh_home).expect("sync should succeed");

        let profile = dsh_home.join("profiles").join("web");
        assert!(profile
            .join("node_modules/@deepseek-ai/dsh-plugin-deepseek-usage/package.json")
            .is_file());
        assert!(profile
            .join("node_modules/@deepseek-ai/dsh-plugin-deepseek-usage/cordis.patch.yml")
            .is_file());
        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(profile.join("package.json")).unwrap())
                .unwrap();
        assert_eq!(
            manifest["dependencies"]["@deepseek-ai/dsh-plugin-deepseek-usage"],
            "0.1.0"
        );
        let bundles = manifest["dsh"]["profile"]["bundles"].as_array().unwrap();
        assert!(bundles
            .iter()
            .any(|bundle| bundle == "@deepseek-ai/dsh-plugin-deepseek-usage"));

        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn installs_bundled_plugins_into_web_profile() {
        let temp = temp_dir("sync");
        let _ = fs::remove_dir_all(&temp);
        let plugins = temp.join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        sample_plugin(&plugins.join("demo-plugin"), "demo-plugin", "1.0.0", true);
        sample_plugin(&plugins.join("plain-lib"), "plain-lib", "2.1.0", false);
        let dsh_home = temp.join("harness");

        sync_from_directory(&plugins, &dsh_home).expect("sync should succeed");

        let profile = dsh_home.join("profiles").join("web");
        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(profile.join("package.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["dependencies"]["demo-plugin"], "1.0.0");
        assert_eq!(manifest["dependencies"]["plain-lib"], "2.1.0");
        let bundles = manifest["dsh"]["profile"]["bundles"].as_array().unwrap();
        assert!(bundles.iter().any(|bundle| bundle == "demo-plugin"));
        assert!(!bundles.iter().any(|bundle| bundle == "plain-lib"));
        assert!(profile
            .join("node_modules/demo-plugin/cordis.patch.yml")
            .is_file());
        assert!(profile.join("node_modules/plain-lib/lib.js").is_file());
        assert!(profile.join(MARKER_FILENAME).is_file());

        // Idempotent: a second sync keeps the marker byte-identical.
        let before = fs::read_to_string(profile.join(MARKER_FILENAME)).unwrap();
        sync_from_directory(&plugins, &dsh_home).expect("second sync should be a no-op");
        assert_eq!(
            fs::read_to_string(profile.join(MARKER_FILENAME)).unwrap(),
            before
        );

        // A version bump triggers a re-sync that updates the dependency.
        sample_plugin(&plugins.join("demo-plugin"), "demo-plugin", "1.1.0", true);
        sync_from_directory(&plugins, &dsh_home).expect("bumped sync should succeed");
        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(profile.join("package.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["dependencies"]["demo-plugin"], "1.1.0");
        let marker: Value =
            serde_json::from_str(&fs::read_to_string(profile.join(MARKER_FILENAME)).unwrap())
                .unwrap();
        assert_eq!(marker["plugins"]["demo-plugin"], "1.1.0");

        // Self-healing: a deleted package directory is restored on the next sync.
        fs::remove_dir_all(profile.join("node_modules/demo-plugin")).unwrap();
        sync_from_directory(&plugins, &dsh_home).expect("healing sync should succeed");
        assert!(profile
            .join("node_modules/demo-plugin/package.json")
            .is_file());

        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn preserves_existing_profile_manifest() {
        let temp = temp_dir("preserve");
        let _ = fs::remove_dir_all(&temp);
        let plugins = temp.join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        sample_plugin(&plugins.join("extra"), "extra", "3.0.0", true);

        let dsh_home = temp.join("harness");
        let profile = dsh_home.join("profiles").join("web");
        write(
            &profile.join("package.json"),
            r#"{
  "name": "dsh-profile-web",
  "private": true,
  "dependencies": { "user-dep": "9.9.9" },
  "dsh": { "profile": { "bundles": ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app", "user-bundle"] } }
}
"#,
        );
        write(&profile.join("cordis.patch.yml"), "- id: mine\n  config: {}\n");

        sync_from_directory(&plugins, &dsh_home).expect("sync should succeed");

        let manifest: Value =
            serde_json::from_str(&fs::read_to_string(profile.join("package.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["dependencies"]["user-dep"], "9.9.9");
        assert_eq!(manifest["dependencies"]["extra"], "3.0.0");
        let bundles = manifest["dsh"]["profile"]["bundles"].as_array().unwrap();
        let names: Vec<&str> = bundles.iter().filter_map(Value::as_str).collect();
        assert_eq!(
            names,
            vec![
                "@deepseek-ai/dsh-base",
                "@deepseek-ai/dsh-web-app",
                "user-bundle",
                "extra"
            ]
        );
        // The user's own patch layer is untouched.
        assert_eq!(
            fs::read_to_string(profile.join("cordis.patch.yml")).unwrap(),
            "- id: mine\n  config: {}\n"
        );

        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn rejects_duplicate_plugin_names() {
        let temp = temp_dir("dupe");
        let _ = fs::remove_dir_all(&temp);
        let plugins = temp.join("plugins");
        sample_plugin(&plugins.join("one"), "same", "1.0.0", true);
        sample_plugin(&plugins.join("two"), "same", "2.0.0", true);
        let error = sync_from_directory(&plugins, &temp.join("harness")).unwrap_err();
        assert!(error.contains("重名"), "unexpected error: {error}");
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn syncs_from_bundled_archive() {
        let temp = temp_dir("archive");
        let _ = fs::remove_dir_all(&temp);
        let plugins = temp.join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        sample_plugin(&plugins.join("demo-plugin"), "demo-plugin", "1.0.0", true);

        let archive_path = temp.join("runtime").join(BUNDLED_ARCHIVE_NAME);
        fs::create_dir_all(archive_path.parent().unwrap()).unwrap();
        let archive = fs::File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(archive, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        builder
            .append_dir_all("demo-plugin", plugins.join("demo-plugin"))
            .unwrap();
        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();

        let dsh_home = temp.join("harness");
        sync_bundled_plugins(&temp, &temp, &dsh_home).expect("archive sync should succeed");
        assert!(dsh_home
            .join("profiles/web/node_modules/demo-plugin/package.json")
            .is_file());
        // The extraction staging directory is cleaned up.
        assert!(!temp.join("plugins-bundled").exists());

        // Idempotent across launches while the marker matches.
        sync_bundled_plugins(&temp, &temp, &dsh_home).expect("second sync should be a no-op");
        assert!(!temp.join("plugins-bundled").exists());

        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn missing_source_is_a_no_op() {
        let temp = temp_dir("none");
        let _ = fs::remove_dir_all(&temp);
        sync_bundled_plugins(&temp, &temp, &temp.join("harness")).expect("no source should be fine");
        assert!(!temp.join("harness").exists());
        let _ = fs::remove_dir_all(&temp);
    }
}
