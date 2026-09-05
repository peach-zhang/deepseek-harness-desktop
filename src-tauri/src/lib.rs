mod app;
mod archive;
mod backend;
mod commands;
mod db;
mod desktop_update;
mod platform;
mod plugins;
mod runtime;
mod theme;
mod update;

/// Bundled Harness version. Read from `HARNESS_VERSION` at the project root
/// by `build.rs` — change that single file when updating the bundled release.
pub(crate) const HARNESS_VERSION: &str = env!("HARNESS_VERSION");
pub(crate) const MAX_DIAGNOSTIC_LINES: usize = 12;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    app::run();
}
