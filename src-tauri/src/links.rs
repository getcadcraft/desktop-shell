// SPDX-License-Identifier: Apache-2.0
//! `ccstudio://` links (docs/0.5-PLAN.md LV.25, session desktop): the invite page getcadcraft.com/live/<ROOM> offers
//! "Open in Studio" = `ccstudio://join/<ROOM>#<secret>`. The installer registers the scheme (Windows: build.mjs's NSIS
//! hook; Linux: the desktop entry's `x-scheme-handler/ccstudio`), so the system starts Studio with the link as an
//! argument, or a running Studio gets it through the single-instance hand-over. Studio builds only.
//!
//! The fragment is the session's secret: a link is never logged, never put in a window title, and reaches only the hub
//! page (`cadcraftDesktop.links`), untouched. Anything that is not exactly a join link is dropped without a word.
//!
//! Also here (SD.6): `--app=<slug>`, the argument of the per-app shortcuts the Studio installer adds ("CadCraft",
//! "BimCraft", …): the hub opens that app (`cadcraftDesktop.windows.launchApps()` / `onLaunchApp`), as it routes a file.
// Tablets: launch arguments, second starts and the desktop's dialogs don't exist there (TB.1).
#![cfg_attr(mobile, allow(dead_code))]

use std::sync::Mutex;
use tauri::{AppHandle, Manager, Runtime, State, WebviewWindow};

/// `ccstudio://join/<ROOM>#<secret>`: ROOM is 8 characters of 0-9 A-H J K M N P-T V-Z (live-relay's alphabet), the
/// secret 22–43 characters of base64url. One optional "/" after the room (some browsers add it).
pub fn is_join_link(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("ccstudio://join/") else { return false };
    let Some((room, secret)) = rest.split_once('#') else { return false };
    let room = room.strip_suffix('/').unwrap_or(room);
    let room_ok = room.len() == 8 && room.bytes().all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'H' | b'J' | b'K' | b'M' | b'N' | b'P'..=b'T' | b'V'..=b'Z'));
    let secret_ok = (22..=43).contains(&secret.len()) && secret.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    room_ok && secret_ok
}

/// The join links among command-line arguments (each once, in order).
pub fn links_from_args<'a, I: IntoIterator<Item = &'a String>>(args: I) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in args {
        if is_join_link(a) && !out.contains(a) {
            out.push(a.clone());
        }
    }
    out
}

/// The eight apps a shortcut may name (Catalog order).
pub const APPS: [&str; 8] = ["cadcraft", "bimcraft", "modelcraft", "markupcraft", "takeoffcraft", "civilcraft", "detailcraft", "partcraft"];

/// `--app=<slug>` arguments naming one of the eight apps (each once, in order).
pub fn apps_from_args<'a, I: IntoIterator<Item = &'a String>>(args: I) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in args {
        if let Some(slug) = a.strip_prefix("--app=") {
            if APPS.contains(&slug) && !out.iter().any(|x| x == slug) {
                out.push(slug.to_string());
            }
        }
    }
    out
}

/// Things handed to Studio's hub at start or on a second start: kept until the hub asks for them, then passed on at once
/// through `window.<hook>(list)` (bridge.js). Studio builds only.
pub struct HubQueue {
    pending: Mutex<Vec<String>>,
    drained: Mutex<bool>,
    hook: &'static str,
}

impl HubQueue {
    const fn new(hook: &'static str) -> Self {
        HubQueue { pending: Mutex::new(Vec::new()), drained: Mutex::new(false), hook }
    }
    pub fn add<R: Runtime>(&self, app: &AppHandle<R>, items: Vec<String>) {
        if items.is_empty() || !app.state::<crate::windows::Suite>().on {
            return;
        }
        if *self.drained.lock().unwrap() {
            if let Some(w) = app.get_webview_window("main") {
                let js = format!("window.{h}&&window.{h}({})", serde_json::to_string(&items).unwrap(), h = self.hook);
                let _ = w.eval(&js);
            }
        } else {
            self.pending.lock().unwrap().extend(items);
        }
    }
    fn take<R: Runtime>(&self, window: &WebviewWindow<R>) -> Result<Vec<String>, String> {
        if window.label() != "main" {
            return Err("not_allowed: only the hub takes these".into());
        }
        *self.drained.lock().unwrap() = true;
        Ok(std::mem::take(&mut *self.pending.lock().unwrap()))
    }
}

/// ccstudio:// join links for the hub.
pub struct Links(pub HubQueue);
impl Default for Links {
    fn default() -> Self { Links(HubQueue::new("__ccDesktopLink")) }
}
/// Apps named by `--app=<slug>` (the per-app shortcuts) for the hub.
pub struct LaunchApps(pub HubQueue);
impl Default for LaunchApps {
    fn default() -> Self { LaunchApps(HubQueue::new("__ccDesktopApp")) }
}

/// The links Studio was started with (each once); later ones arrive through `onOpen`. The hub only.
#[tauri::command]
pub fn cc_links_launch<R: Runtime>(window: WebviewWindow<R>, links: State<'_, Links>) -> Result<Vec<String>, String> {
    links.0.take(&window)
}

/// The apps Studio was started for (a per-app shortcut; each once); later ones arrive through `onLaunchApp`. The hub only.
#[tauri::command]
pub fn cc_launch_apps<R: Runtime>(window: WebviewWindow<R>, apps: State<'_, LaunchApps>) -> Result<Vec<String>, String> {
    apps.0.take(&window)
}

#[cfg(test)]
mod tests {
    use super::*;
    const OK: &str = "ccstudio://join/AB3K7MNP#abcdefghijklmnopqrstuv";
    #[test]
    fn join_links() {
        assert!(is_join_link(OK));
        assert!(is_join_link("ccstudio://join/AB3K7MNP/#abcdefghijklmnopqrstuv_-0123456789ABCDEFGHI"));
        for bad in [
            "ccstudio://join/AB3K7MNP",                           // no secret
            "ccstudio://join/AB3K7MN#abcdefghijklmnopqrstuv",     // room too short
            "ccstudio://join/AB3K7MNI#abcdefghijklmnopqrstuv",    // I is not in the alphabet
            "ccstudio://join/ab3k7mnp#abcdefghijklmnopqrstuv",    // lower case room
            "ccstudio://join/AB3K7MNP#short",                     // secret too short
            "ccstudio://join/AB3K7MNP#abcdefghijklmnopqrstuv%20", // not base64url
            "ccstudio://host/AB3K7MNP#abcdefghijklmnopqrstuv",    // not join
            "CCSTUDIO://join/AB3K7MNP#abcdefghijklmnopqrstuv",
            "https://getcadcraft.com/live/AB3K7MNP#abcdefghijklmnopqrstuv",
            "ccstudio://join/AB3K7MNP#abcdefghijklmnopqrstuv\" --evil",
        ] {
            assert!(!is_join_link(bad), "{bad}");
        }
        let args: Vec<String> = vec!["--flag".into(), OK.into(), "/tmp/a.dwg".into(), OK.into(), "ccstudio://x".into()];
        assert_eq!(links_from_args(&args), vec![OK.to_string()]);
    }
    #[test]
    fn app_shortcuts() {
        let args: Vec<String> = ["--app=bimcraft", "--app=nope", "--app=../x", "--app=bimcraft", "--app=", "app=cadcraft", "--app=partcraft"]
            .iter().map(|s| s.to_string()).collect();
        assert_eq!(apps_from_args(&args), vec!["bimcraft".to_string(), "partcraft".to_string()]);
    }
}
