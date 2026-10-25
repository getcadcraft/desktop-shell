// SPDX-License-Identifier: Apache-2.0
//! A portable copy (0.7 D7.53): the zip with the program, its app folder and `shell.json` (`"portable": true`), run from
//! wherever it was unpacked. Everything it keeps lives in `data/` beside the program, so a copy on a USB stick or in a
//! locked-down folder leaves nothing behind in the PC's profile:
//!
//! - `data/grants.json`, `places.json`: the files the person opened and the windows' places (the same files an install keeps
//!   in its app-data folder);
//! - `data/webview2/`: the web view's own profile (the app's settings, recent files, autosave and recovery copies);
//! - no updater (a newer zip replaces the folder), no carrying of the separate 0.4 apps' data from the PC.
//!
//! Found once at start, before any window: `shell.json` is read from the program's folder, not from the bundle's resource
//! path, so the test is the same on every system. The sign-in token stays in the PC's own credential store (account.rs):
//! "Sign out" removes it, and the help for the zip says so.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// The data folder when `shell.json` in `exe_dir` says `"portable": true`.
pub fn detect_in(exe_dir: &Path) -> Option<PathBuf> {
    let raw = std::fs::read(exe_dir.join("shell.json")).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    if v.get("portable").and_then(|p| p.as_bool()) != Some(true) {
        return None;
    }
    Some(exe_dir.join("data"))
}

/// Called first thing in `run()`.
pub fn init() {
    let found = std::env::current_exe().ok().and_then(|e| e.parent().and_then(detect_in));
    if let Some(d) = &found {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = DIR.set(found);
}

/// The data folder of a portable copy; `None` for an install.
pub fn dir() -> Option<&'static Path> {
    DIR.get().and_then(|d| d.as_deref())
}

pub fn is_portable() -> bool {
    dir().is_some()
}

/// The web view's profile folder in a portable copy (every window of the program must use the same one).
pub fn webview_dir() -> Option<PathBuf> {
    dir().map(|d| d.join("webview2"))
}

/// A file the shell keeps: in `data/` for a portable copy, else in the app-data folder.
pub fn data_file<R: tauri::Runtime>(app: &tauri::AppHandle<R>, name: &str) -> Option<PathBuf> {
    use tauri::Manager;
    match dir() {
        Some(d) => Some(d.join(name)),
        None => app.path().app_data_dir().ok().map(|d| d.join(name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cc-portable-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn needs_the_flag_in_shell_json() {
        let d = tmp("flag");
        assert_eq!(detect_in(&d), None, "no shell.json");
        std::fs::write(d.join("shell.json"), br#"{"app":"studio","version":"0.7.0"}"#).unwrap();
        assert_eq!(detect_in(&d), None, "no flag");
        std::fs::write(d.join("shell.json"), br#"{"app":"studio","portable":false}"#).unwrap();
        assert_eq!(detect_in(&d), None, "flag off");
        std::fs::write(d.join("shell.json"), br#"{"app":"studio","portable":"yes"}"#).unwrap();
        assert_eq!(detect_in(&d), None, "only a real true counts");
        std::fs::write(d.join("shell.json"), br#"{"app":"studio","portable":true}"#).unwrap();
        assert_eq!(detect_in(&d), Some(d.join("data")));
        std::fs::write(d.join("shell.json"), b"not json").unwrap();
        assert_eq!(detect_in(&d), None, "unreadable");
        let _ = std::fs::remove_dir_all(&d);
    }
}
