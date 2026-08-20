//! Platform-specific helpers for the desktop shell.

use std::path::PathBuf;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// Opens the containing folder (and selects the file) after a download
/// finishes. Uses Explorer on Windows, `open -R` on macOS, and `xdg-open`
/// on Linux.
pub(crate) fn open_containing_folder(path: &PathBuf) {
    #[cfg(target_os = "windows")]
    {
        // Just open the parent directory. The `explorer /select,<file>`
        // approach is unreliable: when the path format isn't exactly right,
        // Explorer ignores /select and falls back to "Documents".
        if let Some(parent) = path.parent() {
            let _ = std::process::Command::new("explorer.exe")
                .arg(parent)
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .spawn();
        }
    }

    #[cfg(target_os = "macos")]
    {
        // `open -R <path>` reveals the file in Finder.
        let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    }

    #[cfg(target_os = "linux")]
    {
        // Open the parent directory with the default file manager.
        if let Some(parent) = path.parent() {
            let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
        }
    }
}
