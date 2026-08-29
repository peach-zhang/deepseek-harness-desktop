//! Installation of registry plugins configured by `plugins/plugins.json`.
//!
//! The desktop package ships only a small JSON manifest. On launch, each
//! listed package is installed into the Harness `web` profile through the
//! bundled DSH CLI. A marker avoids repeating successful installs while a
//! missing installed package triggers a repair on the next launch.

use std::{fs, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;

const CONFIG_PATH: &str = "plugins/plugins.json";
const PROFILE_NAME: &str = "web";
const MARKER_FILENAME: &str = ".dsh-desktop-plugin-sync.json";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PluginSpec {
    name: String,
    command: String,
}

/// Install the plugins declared by the desktop JSON manifest.
pub(crate) async fn sync_configured_plugins(
    app: &AppHandle,
    resource_dir: &Path,
    dsh_home: &Path,
    working_dir: &Path,
    entry: &Path,
) -> Result<(), String> {
    let plugins = read_config(&resource_dir.join(CONFIG_PATH))?;
    let profile_dir = dsh_home.join("profiles").join(PROFILE_NAME);
    let marker_path = profile_dir.join(MARKER_FILENAME);
    let expected = expected_marker(&plugins);

    if read_json(&marker_path).as_ref() == Some(&expected)
        && plugins
            .iter()
            .all(|plugin| installed_package_exists(&profile_dir, &plugin.name))
    {
        return Ok(());
    }

    for plugin in &plugins {
        install_plugin(app, dsh_home, working_dir, entry, plugin).await?;
    }

    fs::create_dir_all(&profile_dir).map_err(|error| {
        format!(
            "无法创建插件 profile 目录 {}：{error}",
            profile_dir.display()
        )
    })?;
    write_json(&marker_path, &expected)?;
    log::info!(
        "installed {} configured plugin(s) into the {PROFILE_NAME} profile",
        plugins.len()
    );
    Ok(())
}

fn read_config(path: &Path) -> Result<Vec<PluginSpec>, String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("无法读取插件配置 {}：{error}", path.display()))?;
    let plugins: Vec<PluginSpec> = serde_json::from_str(&raw)
        .map_err(|error| format!("插件配置 {} 不是有效 JSON：{error}", path.display()))?;

    for (index, plugin) in plugins.iter().enumerate() {
        validate_package_name(&plugin.name)
            .and_then(|_| plugin_command_args(plugin).map(|_| ()))
            .map_err(|error| format!("插件配置第 {} 项无效：{error}", index + 1))?;
        if plugins[..index]
            .iter()
            .any(|previous| previous.name == plugin.name)
        {
            return Err(format!("插件配置包含重复名称：{}", plugin.name));
        }
    }
    Ok(plugins)
}

fn validate_package_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.trim() != name
        || name.chars().any(char::is_whitespace)
        || name.starts_with('-')
        || name.contains('\\')
    {
        return Err(format!("插件名非法：{name}"));
    }

    let valid = if let Some(scoped) = name.strip_prefix('@') {
        let mut parts = scoped.split('/');
        matches!((parts.next(), parts.next(), parts.next()), (Some(scope), Some(package), None) if valid_name_part(scope) && valid_name_part(package))
    } else {
        !name.contains('/') && valid_name_part(name)
    };

    if valid {
        Ok(())
    } else {
        Err(format!("插件名非法：{name}"))
    }
}

fn valid_name_part(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('.')
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

fn plugin_command_args(plugin: &PluginSpec) -> Result<Vec<String>, String> {
    let tokens = plugin.command.split_whitespace().collect::<Vec<_>>();
    let expected = [
        "dsh",
        "plugin",
        "--profile",
        PROFILE_NAME,
        "add",
        plugin.name.as_str(),
    ];
    if tokens != expected {
        return Err(format!(
            "插件 {} 的 command 必须为 `dsh plugin --profile {PROFILE_NAME} add {}`",
            plugin.name, plugin.name
        ));
    }
    Ok(tokens.into_iter().skip(1).map(str::to_owned).collect())
}

async fn install_plugin(
    app: &AppHandle,
    dsh_home: &Path,
    working_dir: &Path,
    entry: &Path,
    plugin: &PluginSpec,
) -> Result<(), String> {
    log::info!("installing configured Harness plugin {}", plugin.name);
    let mut args = vec![entry.to_string_lossy().into_owned()];
    args.extend(plugin_command_args(plugin)?);
    let output = app
        .shell()
        .sidecar("node")
        .map_err(|error| format!("无法定位内置 Node.js：{error}"))?
        .args(args)
        .env("DSH_HOME", dsh_home)
        .current_dir(working_dir)
        .output()
        .await
        .map_err(|error| format!("无法安装插件 {}：{error}", plugin.name))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    let suffix = output.status.code().map_or_else(
        || "进程异常终止".to_owned(),
        |code| format!("退出码 {code}"),
    );
    if detail.is_empty() {
        Err(format!("插件 {} 安装失败（{suffix}）", plugin.name))
    } else {
        Err(format!(
            "插件 {} 安装失败（{suffix}）：{detail}",
            plugin.name
        ))
    }
}

fn installed_package_exists(profile_dir: &Path, name: &str) -> bool {
    name.split('/')
        .fold(profile_dir.join("node_modules"), |path, segment| {
            path.join(segment)
        })
        .join("package.json")
        .is_file()
}

fn expected_marker(plugins: &[PluginSpec]) -> Value {
    json!({ "plugins": plugins })
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let raw = serde_json::to_string_pretty(value)
        .map_err(|error| format!("无法序列化插件安装标记：{error}"))?;
    fs::write(path, format!("{raw}\n"))
        .map_err(|error| format!("无法写入插件安装标记 {}：{error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "dsh-desktop-plugin-config-{label}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn reads_valid_plugin_config() {
        let temp = temp_dir("valid");
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        let path = temp.join("plugins.json");
        fs::write(
            &path,
            r#"[
                {"name":"plain-plugin","command":"dsh plugin --profile web add plain-plugin"},
                {"name":"@scope/scoped-plugin","command":"dsh plugin --profile web add @scope/scoped-plugin"}
            ]"#,
        )
        .unwrap();

        let plugins = read_config(&path).unwrap();
        assert_eq!(plugins.len(), 2);
        assert_eq!(plugins[1].name, "@scope/scoped-plugin");
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn rejects_invalid_or_duplicate_names() {
        for name in ["", "../escape", "plugin name", "-option", "@scope"] {
            assert!(validate_package_name(name).is_err(), "accepted {name}");
        }
        assert!(validate_package_name("dsh-plugin").is_ok());
        assert!(validate_package_name("@deepseek-ai/dsh-plugin").is_ok());

        let temp = temp_dir("duplicate");
        let _ = fs::remove_dir_all(&temp);
        fs::create_dir_all(&temp).unwrap();
        let path = temp.join("plugins.json");
        fs::write(
            &path,
            r#"[
                {"name":"same","command":"dsh plugin --profile web add same"},
                {"name":"same","command":"dsh plugin --profile web add same"}
            ]"#,
        )
        .unwrap();
        assert!(read_config(&path).unwrap_err().contains("重复"));

        fs::write(
            &path,
            r#"[{"name":"safe","command":"dsh plugin --profile web add other"}]"#,
        )
        .unwrap();
        assert!(read_config(&path).unwrap_err().contains("command"));
        fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn detects_missing_installed_package() {
        let temp = temp_dir("installed");
        let _ = fs::remove_dir_all(&temp);
        let package = temp.join("node_modules/@scope/plugin/package.json");
        fs::create_dir_all(package.parent().unwrap()).unwrap();
        fs::write(&package, "{}").unwrap();

        assert!(installed_package_exists(&temp, "@scope/plugin"));
        assert!(!installed_package_exists(&temp, "missing"));
        fs::remove_dir_all(&temp).unwrap();
    }
}
