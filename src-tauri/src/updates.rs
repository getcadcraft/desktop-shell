// SPDX-License-Identifier: Apache-2.0
//! Updates (docs/0.4-PART2.md W.7): check the app's signed release feed, ask, then install.
//!
//! - **Off unless the build has a feed and a key.** `shell.json` `updates {endpoint, pubkey, auto}` is written by
//!   build.mjs only when CC_UPDATE_ENDPOINT and CC_UPDATE_PUBKEY are set (the owner's minisign public key; the private key
//!   lives only in CI). Without them there is no updater and the app never calls home.
//! - **One request, nothing else:** at most one check a start (30 s after it, when automatic checks are on) plus the
//!   person's own "Check for updates". The feed is `latest.json` on the app's GitHub Releases.
//! - **Asks first.** Nothing downloads until the person says yes; the package must carry a valid signature from the
//!   owner's key or Tauri refuses it. Whether automatic checks start on or off is decision 10 (`auto`); the person can
//!   switch them in the app (prefs in the app data folder).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager, Runtime, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::UpdaterExt;

#[derive(Deserialize, Serialize, Default, Clone)]
#[serde(default)]
pub struct UpdateConfig {
    pub endpoint: String,
    pub pubkey: String,
    /// Automatic checks when the person has not chosen (decision 10).
    pub auto: bool,
}

#[derive(Deserialize, Serialize, Default)]
#[serde(default)]
struct Prefs {
    /// The person's choice; None = the build's default.
    auto: Option<bool>,
}

pub struct Updates {
    cfg: Option<UpdateConfig>,
    prefs_file: Option<PathBuf>,
    prefs: Mutex<Prefs>,
    busy: Mutex<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// false: this build has no update feed (nothing is ever checked)
    available: bool,
    auto: bool,
}

impl Updates {
    pub fn new(cfg: Option<UpdateConfig>, prefs_file: Option<PathBuf>) -> Self {
        let cfg = cfg.filter(|c| c.endpoint.starts_with("https://") && !c.pubkey.trim().is_empty());
        let prefs = prefs_file.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Updates { cfg, prefs_file, prefs: Mutex::new(prefs), busy: Mutex::new(false) }
    }
    fn auto(&self) -> bool {
        self.cfg.as_ref().map(|c| self.prefs.lock().unwrap().auto.unwrap_or(c.auto)).unwrap_or(false)
    }
}

/// At start: one check after 30 s when this build has a feed and automatic checks are on.
pub fn on_start<R: Runtime>(app: &AppHandle<R>) {
    if !app.state::<Updates>().auto() {
        return;
    }
    let h = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio_sleep(30).await;
        let _ = check(&h, false).await;
    });
}

async fn tokio_sleep(secs: u64) {
    let _ = tauri::async_runtime::spawn_blocking(move || std::thread::sleep(std::time::Duration::from_secs(secs))).await;
}

/// Check, ask, install. `asked` = the person pressed "Check for updates" (then "you're up to date" is said too).
async fn check<R: Runtime>(app: &AppHandle<R>, asked: bool) -> Result<String, String> {
    let up = app.state::<Updates>();
    let Some(cfg) = up.cfg.clone() else { return Ok("off".into()) };
    {
        let mut b = up.busy.lock().unwrap();
        if *b {
            return Ok("busy".into());
        }
        *b = true;
    }
    let done = |r: Result<String, String>| {
        *app.state::<Updates>().busy.lock().unwrap() = false;
        r
    };
    let url = match cfg.endpoint.parse() {
        Ok(u) => u,
        Err(_) => return done(Err("bad feed address".into())),
    };
    let updater = match app.updater_builder().pubkey(cfg.pubkey.clone()).endpoints(vec![url]).and_then(|b| b.build()) {
        Ok(u) => u,
        Err(e) => return done(Err(e.to_string())),
    };
    let name = app.package_info().name.clone();
    let current = app.package_info().version.to_string();
    let update = match updater.check().await {
        Ok(u) => u,
        Err(e) => return done(Err(format!("network: {e}"))),
    };
    let Some(update) = update else {
        if asked {
            app.dialog().message(format!("{name} {current} is the newest version.")).title("No update").kind(MessageDialogKind::Info).show(|_| {});
        }
        return done(Ok("current".into()));
    };
    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog()
        .message(format!(
            "{name} {} is available (you have {current}).\n\nInstall it now? {name} closes and opens again; save your work first.",
            update.version
        ))
        .title("Update available")
        .buttons(MessageDialogButtons::OkCancelCustom("Install now".into(), "Not now".into()))
        .show(move |yes| {
            let _ = tx.send(yes);
        });
    let yes = tauri::async_runtime::spawn_blocking(move || rx.recv().unwrap_or(false)).await.unwrap_or(false);
    if !yes {
        return done(Ok("declined".into()));
    }
    // The signature is checked against the owner's key before anything runs.
    if let Err(e) = update.download_and_install(|_, _| {}, || {}).await {
        return done(Err(format!("install: {e}")));
    }
    app.restart();
}

#[tauri::command]
pub fn cc_updates_status(up: State<'_, Updates>) -> UpdateStatus {
    UpdateStatus { available: up.cfg.is_some(), auto: up.auto() }
}

#[tauri::command]
pub async fn cc_updates_check<R: Runtime>(app: AppHandle<R>) -> Result<String, String> {
    check(&app, true).await
}

#[tauri::command]
pub fn cc_updates_set_auto(up: State<'_, Updates>, on: bool) -> UpdateStatus {
    let mut p = up.prefs.lock().unwrap();
    p.auto = Some(on);
    if let Some(f) = &up.prefs_file {
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(f, serde_json::to_vec(&*p).unwrap_or_default());
    }
    drop(p);
    UpdateStatus { available: up.cfg.is_some(), auto: up.auto() }
}
