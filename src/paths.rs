//! Folders for notebooks, settings and downloads on each platform (Issues #42, #48).
//!
//! - Linux: XDG (`~/.local/share/ink-notes`, `~/.config/ink-notes`, `~/.cache/ink-notes`)
//! - Windows: `%APPDATA%\ink-notes` for data and settings, `%LOCALAPPDATA%\ink-notes` for
//!   downloads
//! - Android: the app's internal folder, set once at startup with [`set_app_dir`]
//!
//! The environment is read on every call, so tests can point the folders elsewhere.

use std::path::PathBuf;
use std::sync::OnceLock;

/// Fixed app folder (Android: internal storage of the app)
static APP_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Puts data, settings and downloads below `dir` (Android, where there is no XDG or home).
pub fn set_app_dir(dir: PathBuf) {
    let _ = APP_DIR.set(dir);
}

/// Value of an environment variable, unless empty.
fn env_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
}

/// Home folder of the user.
pub fn home_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env_dir("USERPROFILE").or_else(|| env_dir("HOME"))
    } else {
        env_dir("HOME")
    }
}

/// Folder for notebooks and emergency backups.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = APP_DIR.get() {
        return Some(dir.clone());
    }
    if cfg!(windows) {
        return env_dir("APPDATA").map(|d| d.join("ink-notes"));
    }
    env_dir("XDG_DATA_HOME")
        .or_else(|| home_dir().map(|h| h.join(".local").join("share")))
        .map(|d| d.join("ink-notes"))
}

/// Folder for the settings (`ui.json`).
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = APP_DIR.get() {
        return Some(dir.join("config"));
    }
    if cfg!(windows) {
        return env_dir("APPDATA").map(|d| d.join("ink-notes"));
    }
    env_dir("XDG_CONFIG_HOME")
        .or_else(|| home_dir().map(|h| h.join(".config")))
        .map(|d| d.join("ink-notes"))
}

/// Folder for downloaded updates.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(dir) = APP_DIR.get() {
        return Some(dir.join("cache"));
    }
    if cfg!(windows) {
        return env_dir("LOCALAPPDATA").map(|d| d.join("ink-notes"));
    }
    env_dir("XDG_CACHE_HOME")
        .or_else(|| home_dir().map(|h| h.join(".cache")))
        .map(|d| d.join("ink-notes"))
}
