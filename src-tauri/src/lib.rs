mod app;
mod archive;
mod backend;
mod commands;
mod db;
mod platform;
mod plugins;
mod runtime;
mod theme;
mod update;

pub(crate) const HARNESS_VERSION: &str = "0.1.0-rc.7";
pub(crate) const MAX_DIAGNOSTIC_LINES: usize = 12;

pub fn run() {
    app::run();
}
