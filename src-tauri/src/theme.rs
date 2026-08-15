//! Theme preference detection for the embedded Harness UI.
//!
//! The Harness web UI stores its appearance preference server-side in
//! `$DSH_HOME/settings.yaml` (`ui-theme.preference`: light | dark | system).
//! The desktop shell cannot read the cross-origin iframe DOM, so instead it
//! watches that file and forwards changes to the frontend, which re-skins the
//! custom titlebar to match. `system` is resolved in the frontend via
//! `prefers-color-scheme`.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;
use tauri::{AppHandle, Emitter, Manager};

const POLL_INTERVAL: Duration = Duration::from_micros(200);

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HarnessTheme {
    pub preference: String,
}

#[derive(Deserialize)]
struct SettingsFile {
    #[serde(rename = "ui-theme")]
    ui_theme: Option<UiThemeSection>,
}

#[derive(Deserialize)]
struct UiThemeSection {
    preference: Option<String>,
}

/// Reads the theme preference from `<dsh_home>/settings.yaml`. A missing or
/// unparsable file (or a missing/unknown value) resolves to `system`, which
/// matches the Harness default.
pub(crate) fn read_theme_preference(dsh_home: &Path) -> String {
    let path = dsh_home.join("settings.yaml");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return "system".to_owned();
    };
    parse_theme_preference(&content)
}

fn parse_theme_preference(content: &str) -> String {
    match serde_yaml::from_str::<SettingsFile>(content) {
        Ok(file) => file
            .ui_theme
            .and_then(|section| section.preference)
            .filter(|value| matches!(value.as_str(), "light" | "dark" | "system"))
            .unwrap_or_else(|| "system".to_owned()),
        Err(error) => {
            log::warn!("无法解析 Harness 设置文件：{error}");
            "system".to_owned()
        }
    }
}

fn dsh_home(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_data_dir()
        .ok()
        .map(|dir| dir.join("harness"))
}

#[tauri::command]
pub(crate) fn get_harness_theme(app: AppHandle) -> HarnessTheme {
    let preference = dsh_home(&app)
        .map(|home| read_theme_preference(&home))
        .unwrap_or_else(|| "system".to_owned());
    HarnessTheme { preference }
}

/// Polls `settings.yaml` on a dedicated thread and emits `harness-theme`
/// whenever the preference changes. The file is tiny and the interval
/// generous, so polling stays cheaper than a filesystem watcher dependency.
pub(crate) fn spawn_theme_watcher(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last: Option<String> = None;
        loop {
            if let Some(home) = dsh_home(&app) {
                let preference = read_theme_preference(&home);
                if last.as_deref() != Some(preference.as_str()) {
                    last = Some(preference.clone());
                    if let Err(error) = app.emit("harness-theme", HarnessTheme { preference }) {
                        log::warn!("failed to emit harness theme: {error}");
                    }
                }
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dark_preference() {
        let yaml = "locale:\n  language: zh-CN\nui-theme:\n  preference: dark\n";
        assert_eq!(parse_theme_preference(yaml), "dark");
    }

    #[test]
    fn parses_quoted_and_indented_variants() {
        assert_eq!(parse_theme_preference("ui-theme:\n    preference: \"light\""), "light");
        assert_eq!(parse_theme_preference("ui-theme: { preference: system }"), "system");
    }

    #[test]
    fn falls_back_to_system() {
        assert_eq!(parse_theme_preference(""), "system");
        assert_eq!(parse_theme_preference("ui-theme:\n  other: 1\n"), "system");
        assert_eq!(parse_theme_preference("ui-theme:\n  preference: neon\n"), "system");
        assert_eq!(parse_theme_preference("not: [valid"), "system");
    }
}
