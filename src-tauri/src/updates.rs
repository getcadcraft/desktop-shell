// SPDX-License-Identifier: Apache-2.0
//! Updates (docs/0.4-PART2.md W.7; 0.7 MSIX + one update endpoint): check, ask, then install.
//!
//! Where an update comes from depends on how this copy was installed (`Source`):
//! - **feed** — the website's installer (NSIS, with the fixed WebView2 runtime), the AppImage, the .deb, the Mac's .app: the
//!   Tauri updater asks getcadcraft.com's one update endpoint (`/api/v1/updates/{{target}}/{{arch}}/{{current_version}}?edition=…`;
//!   204 = up to date), downloads the file it names and installs it only when it carries a valid signature from the owner's key.
//! - **notify** — the portable zip: the same endpoint, but it only says that a newer version exists and opens the download
//!   page. A running portable copy can't safely swap its own program, app folder and WebView2 runtime (they are open, and its
//!   data folder lives beside them), so it never replaces itself.
//! - **package** — an MSIX (package identity): the Store or App Installer updates it (package.rs). The Tauri updater never runs
//!   inside a package.
//!
//! - **Off unless the build has a feed and a key** (feed / notify): `shell.json` `updates {endpoint, pubkey, auto, edition}` is
//!   written by build.mjs only when CC_UPDATE_PUBKEY is set. Without it there is no updater and the app never calls home. A
//!   package needs neither (Windows has the source).
//! - **One request, nothing else:** at most one check a start (30 s after it, when automatic checks are on; feed / notify only —
//!   App Installer and the Store check by themselves) plus the person's own "Check for updates".
//! - **Asks first.** Nothing downloads until the person says yes.
//! - **Remembered:** the person's automatic-checks choice and the last check (when, what it found, the newest version and its
//!   notes) in `updates.json` in the app data folder, shown by Studio's Settings › Updates through `cc_updates_status`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager, Runtime, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tauri_plugin_updater::UpdaterExt;

use crate::package;

/// Why a check or an install failed, as the code the page gets in front of the message (bridge.js `fail`: `err.code`).
/// The hub shows its own words for each code; the text after it is for the log and the owner (D7.54, 0.7).
///   no_build   the endpoint has no file for this system (a release published for Windows only: "target not found")
///   network    the endpoint or the file could not be fetched at all
///   feed       the endpoint answered, but not with a usable release (missing, not JSON, a bad version)
///   signature  the package or its signature does not match the owner's key: nothing was installed
///   install    the package was fetched and verified, but putting it in place failed (disk, rights, the package manager)
///   not_store  (MSIX) a package installed from a file: neither the Store nor App Installer can update it
fn failure_code(e: &tauri_plugin_updater::Error) -> &'static str {
    use tauri_plugin_updater::Error as E;
    match e {
        E::TargetNotFound(_) | E::TargetsNotFound(_) | E::UnsupportedArch | E::UnsupportedOs => "no_build",
        E::Reqwest(_) | E::Network(_) => "network",
        E::ReleaseNotFound | E::Serialization(_) | E::Semver(_) | E::UrlParse(_) | E::EmptyEndpoints | E::InsecureTransportProtocol => "feed",
        E::Minisign(_) | E::Base64(_) | E::SignatureUtf8(_) | E::SignedVersionMismatch { .. } | E::MissingSignedVersion => "signature",
        _ => "install",
    }
}

fn failure(e: &tauri_plugin_updater::Error) -> String {
    format!("{}: {e}", failure_code(e))
}

/// The code in front of a "code: text" failure.
fn code_of(msg: &str) -> String {
    msg.split(": ").next().filter(|c| !c.is_empty() && c.chars().all(|c| c.is_ascii_lowercase() || c == '_')).unwrap_or("install").to_string()
}

fn yes() -> bool {
    true
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct UpdateConfig {
    pub endpoint: String,
    pub pubkey: String,
    /// Automatic checks when the person has not chosen (decision 10; owner 2026-10-24: on, once a day).
    #[serde(default = "yes")]
    pub auto: bool,
    /// What this build is, sent to the endpoint as `?edition=` and shown in Settings: installer | portable | msix-web |
    /// msix-store | deb | appimage | dmg.
    pub edition: String,
    /// A test build (`build.mjs --update-test`): the endpoint may be http on this computer (127.0.0.1 / localhost). Never set
    /// in a build that is published; the plugin config that allows http is in that test build only.
    pub test: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        UpdateConfig { endpoint: String::new(), pubkey: String::new(), auto: true, edition: String::new(), test: false }
    }
}

/// One automatic check a day (owner 2026-10-24), timed from the last check of any kind.
pub const DAY_MS: u64 = 86_400_000;

/// Is an automatic check due? Never checked, a day since the last check, or a clock that went back.
pub fn due(last_at: Option<u64>, now: u64) -> bool {
    match last_at {
        None => true,
        Some(a) => a > now || now - a >= DAY_MS,
    }
}

/// Where this copy's updates come from (see the module text).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Feed,
    Notify,
    Package,
}

fn endpoint_ok(c: &UpdateConfig) -> bool {
    c.endpoint.starts_with("https://")
        || (c.test && (c.endpoint.starts_with("http://127.0.0.1:") || c.endpoint.starts_with("http://localhost:")))
}

/// The decision (pure): a package always goes to Windows; otherwise only a build with an endpoint and the owner's key has an
/// updater, and a portable copy only tells.
pub fn source_for(cfg: Option<&UpdateConfig>, portable: bool, packaged: bool) -> Option<Source> {
    if packaged {
        return Some(Source::Package);
    }
    let c = cfg?;
    if !endpoint_ok(c) || c.pubkey.trim().is_empty() {
        return None;
    }
    Some(if portable { Source::Notify } else { Source::Feed })
}

/// The edition as sent to the endpoint. One Linux build makes both the .deb and the AppImage from one shell.json (edition
/// `linux`): an AppImage runs with APPIMAGE set, an installed .deb without it. Empty = not set (None).
pub fn resolve_edition(e: &str, appimage: bool) -> Option<String> {
    match e {
        "" => None,
        "linux" => Some(if appimage { "appimage" } else { "deb" }.into()),
        other => Some(other.into()),
    }
}

/// The endpoint with this copy's edition in it ({edition}; Tauri fills {{target}} {{arch}} {{current_version}}).
pub fn endpoint_for(endpoint: &str, edition: &str) -> String {
    endpoint.replace("{edition}", edition)
}

/// The last check, kept for Settings › Updates.
#[derive(Deserialize, Serialize, Default, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct LastCheck {
    /// Unix time in milliseconds.
    pub at: u64,
    /// current | available | declined | download | installing | a failure code
    pub outcome: String,
    /// The newest version on offer for this edition (empty when up to date or unknown).
    pub latest: String,
    /// Its release notes, as the endpoint gave them.
    pub notes: String,
    /// true when the person started it (false: the automatic check at start).
    pub asked: bool,
    /// A failure's text after its code (the Windows / updater error, for support; the hub shows its own words).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

#[derive(Deserialize, Serialize, Default)]
#[serde(default)]
struct Prefs {
    /// The person's choice; None = the build's default.
    auto: Option<bool>,
    last: Option<LastCheck>,
}

pub struct Updates {
    cfg: Option<UpdateConfig>,
    source: Option<Source>,
    route: Option<package::Route>,
    prefs_file: Option<PathBuf>,
    prefs: Mutex<Prefs>,
    busy: Mutex<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// false: this build has no updater (nothing is ever checked)
    available: bool,
    auto: bool,
    /// Automatic checks are ours to switch (feed / notify); a package is checked by Windows.
    auto_allowed: bool,
    /// feed | notify | package | null
    source: Option<Source>,
    /// package only: store | appinstaller | sideloaded
    route: Option<&'static str>,
    edition: String,
    /// The installed version (x.y.z; a package: its own version).
    version: String,
    /// windows | linux | macos
    platform: &'static str,
    busy: bool,
    last: Option<LastCheck>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl Updates {
    pub fn new(cfg: Option<UpdateConfig>, portable: bool, prefs_file: Option<PathBuf>) -> Self {
        let packaged = package::packaged();
        let source = source_for(cfg.as_ref(), portable, packaged);
        // A test build only (`test`, never published): CC_UPDATES_ROUTE=store|appinstaller|sideloaded forces the route, so the
        // Store's code path can be run on a sideloaded package (it is not a Store install, so the Store has nothing for it).
        #[cfg(windows)]
        let forced = cfg.as_ref().filter(|c| c.test).and_then(|_| std::env::var("CC_UPDATES_ROUTE").ok()).and_then(|r| match r.as_str() {
            "store" => Some(package::Route::Store),
            "appinstaller" => Some(package::Route::AppInstaller),
            "sideloaded" => Some(package::Route::Sideloaded),
            _ => None,
        });
        #[cfg(windows)]
        let route = if packaged { Some(forced.unwrap_or_else(package::route)) } else { None };
        #[cfg(not(windows))]
        let route = None;
        let prefs = prefs_file.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Updates { cfg, source, route, prefs_file, prefs: Mutex::new(prefs), busy: Mutex::new(false) }
    }
    /// Automatic checks are ours for every edition with our endpoint behind it: feed, notify and the website MSIX (App
    /// Installer reads the same service). The Store package follows the Store; a sideloaded package has no source.
    fn auto_allowed(&self) -> bool {
        matches!(self.source, Some(Source::Feed | Source::Notify)) || self.route == Some(package::Route::AppInstaller)
    }
    /// On unless the person switched them off (owner 2026-10-24: on by default, once a day). The build's `auto` is the
    /// default; a shell.json without one (older builds) counts as on.
    fn auto(&self) -> bool {
        self.auto_allowed() && self.prefs.lock().unwrap().auto.unwrap_or(self.cfg.as_ref().map(|c| c.auto).unwrap_or(true))
    }
    fn save(&self, p: &Prefs) {
        if let Some(f) = &self.prefs_file {
            if let Some(d) = f.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(f, serde_json::to_vec(p).unwrap_or_default());
        }
    }
    fn remember(&self, outcome: &str, latest: &str, notes: &str, asked: bool) {
        let mut p = self.prefs.lock().unwrap();
        p.last = Some(LastCheck { at: now_ms(), outcome: outcome.into(), latest: latest.into(), notes: notes.into(), asked, detail: String::new() });
        self.save(&p);
    }
    /// One line in `updates.log` beside the prefs (UTC seconds, then the text); the file is cut to its newest half past 64 KiB.
    fn log(&self, text: &str) {
        let Some(f) = self.prefs_file.as_ref().map(|p| p.with_file_name("updates.log")) else { return };
        let line = format!("{} {}\n", now_ms() / 1000, text.replace('\n', " "));
        let mut old = std::fs::read_to_string(&f).unwrap_or_default();
        if old.len() > 65_536 {
            let cut = old.len() / 2;
            let at = old[cut..].find('\n').map(|i| cut + i + 1).unwrap_or(cut);
            old = old[at..].to_string();
        }
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&f, old + &line);
    }
    /// After a restart into the version an update was installing: say it was installed (Settings › Updates "updated").
    fn note_finished_update(&self, running: &str) {
        let mut p = self.prefs.lock().unwrap();
        let done = matches!(&p.last, Some(l) if l.outcome == "installing" && package::short_version(&l.latest) == package::short_version(running));
        if done {
            if let Some(l) = p.last.as_mut() {
                l.outcome = "installed".into();
            }
            self.save(&p);
            drop(p);
            self.log(&format!("updated to {running}"));
        }
    }
    fn edition(&self) -> String {
        let e = self.cfg.as_ref().map(|c| c.edition.clone()).unwrap_or_default();
        if let Some(e) = resolve_edition(&e, std::env::var_os("APPIMAGE").is_some()) {
            return e;
        }
        match self.route {
            Some(package::Route::Store) => "msix-store".into(),
            Some(_) => "msix-web".into(),
            None => String::new(),
        }
    }
    fn status<R: Runtime>(&self, app: &AppHandle<R>) -> UpdateStatus {
        #[cfg(windows)]
        let pkg_version = if self.route.is_some() { package::version().map(|v| package::short_version(&v)) } else { None };
        #[cfg(not(windows))]
        let pkg_version: Option<String> = None;
        UpdateStatus {
            available: self.source.is_some(),
            auto: self.auto(),
            auto_allowed: self.auto_allowed(),
            source: self.source,
            route: self.route.map(|r| r.name()),
            edition: self.edition(),
            version: pkg_version.unwrap_or_else(|| app.package_info().version.to_string()),
            platform: match std::env::consts::OS { "macos" => "macos", "windows" => "windows", "linux" => "linux", o => if o.is_empty() { "unknown" } else { "other" } },
            busy: *self.busy.lock().unwrap(),
            last: self.prefs.lock().unwrap().last.clone(),
        }
    }
}

/// Automatic checks (owner 2026-10-24): at most one a day while Studio runs, timed from the last check kept in the prefs — a start
/// more than a day after it checks ~30 s in; a Studio left running checks again once a day has passed (looked at hourly). The
/// person's choice (off) wins at every turn; switching it on later takes effect within the hour. A finished update is noted.
pub fn on_start<R: Runtime>(app: &AppHandle<R>) {
    let up = app.state::<Updates>();
    up.note_finished_update(&app.package_info().version.to_string());
    if !up.auto_allowed() {
        return;
    }
    let h = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(30));
        loop {
            let up = h.state::<Updates>();
            let last = up.prefs.lock().unwrap().last.as_ref().map(|l| l.at);
            if up.auto() && due(last, now_ms()) {
                let _ = tauri::async_runtime::block_on(check(&h, false));
            }
            std::thread::sleep(std::time::Duration::from_secs(3600));
        }
    });
}

/// The ask-first dialog: true = the person said yes.
async fn ask<R: Runtime>(app: &AppHandle<R>, title: &str, text: String, yes: &str) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog()
        .message(text)
        .title(title)
        .buttons(MessageDialogButtons::OkCancelCustom(yes.into(), "Not now".into()))
        .show(move |yes| {
            let _ = tx.send(yes);
        });
    tauri::async_runtime::spawn_blocking(move || rx.recv().unwrap_or(false)).await.unwrap_or(false)
}

fn say_current<R: Runtime>(app: &AppHandle<R>, name: &str, current: &str) {
    app.dialog().message(format!("{name} {current} is the newest version.")).title("No update").kind(MessageDialogKind::Info).show(|_| {});
}

/// Check, ask, install. `asked` = the person pressed "Check for updates" (then "you're up to date" is said too).
async fn check<R: Runtime>(app: &AppHandle<R>, asked: bool) -> Result<String, String> {
    let up = app.state::<Updates>();
    let Some(source) = up.source else { return Ok("off".into()) };
    {
        let mut b = up.busy.lock().unwrap();
        if *b {
            return Ok("busy".into());
        }
        *b = true;
    }
    let r = match source {
        Source::Package => check_package(app, asked).await,
        Source::Feed | Source::Notify => check_feed(app, asked, source).await,
    };
    let up = app.state::<Updates>();
    *up.busy.lock().unwrap() = false;
    if let Err(e) = &r {
        // Keep the newest version seen, so Settings still names it after a failed install.
        let (latest, notes) = up.prefs.lock().unwrap().last.as_ref().map(|l| (l.latest.clone(), l.notes.clone())).unwrap_or_default();
        up.remember(&code_of(e), &latest, &notes, asked);
        if let Some(l) = up.prefs.lock().unwrap().last.as_mut() {
            l.detail = e.split_once(": ").map(|(_, t)| t.chars().take(300).collect()).unwrap_or_default();
        }
        let p = up.prefs.lock().unwrap();
        up.save(&p);
        drop(p);
        // The real error for the owner and support (the hub shows its own words): updates.log beside the prefs.
        up.log(&format!("{} failed: {e}", if asked { "check (asked)" } else { "automatic check" }));
    }
    r
}

async fn check_feed<R: Runtime>(app: &AppHandle<R>, asked: bool, source: Source) -> Result<String, String> {
    let up = app.state::<Updates>();
    let cfg = up.cfg.clone().unwrap_or_default();
    let url = endpoint_for(&cfg.endpoint, &up.edition()).parse().map_err(|_| "feed: bad endpoint address".to_string())?;
    let updater = app.updater_builder().pubkey(cfg.pubkey.clone()).endpoints(vec![url]).and_then(|b| b.build()).map_err(|e| failure(&e))?;
    let name = app.package_info().name.clone();
    let current = app.package_info().version.to_string();
    let update = updater.check().await.map_err(|e| failure(&e))?;
    let Some(update) = update else {
        up.remember("current", "", "", asked);
        if asked {
            say_current(app, &name, &current);
        }
        return Ok("current".into());
    };
    let notes = update.body.clone().unwrap_or_default();
    up.remember("available", &update.version, &notes, asked);
    if source == Source::Notify {
        // The portable copy: tell, then open the download page (never a swap of this folder).
        let text = format!("{name} {} is available (you have {current}).\n\nThis portable copy doesn't replace itself: download the new portable zip and move your data folder into it.", update.version);
        if !ask(app, "Update available", text, "Open the download page").await {
            up.remember("declined", &update.version, &notes, asked);
            return Ok("declined".into());
        }
        use tauri_plugin_opener::OpenerExt;
        let _ = app.opener().open_url("https://getcadcraft.com/download#studio", None::<&str>);
        up.remember("download", &update.version, &notes, asked);
        return Ok("download".into());
    }
    let text = format!("{name} {} is available (you have {current}).\n\nInstall it now? {name} closes and opens again; save your work first.", update.version);
    if !ask(app, "Update available", text, "Install now").await {
        up.remember("declined", &update.version, &notes, asked);
        return Ok("declined".into());
    }
    up.remember("installing", &update.version, &notes, asked);
    up.log(&format!("installing {} (from {current})", update.version));
    // The signature is checked against the owner's key before anything runs.
    update.download_and_install(|_, _| {}, || {}).await.map_err(|e| failure(&e))?;
    app.restart();
}

#[cfg(windows)]
fn hwnd<R: Runtime>(app: &AppHandle<R>) -> isize {
    app.get_webview_window("main").and_then(|w| w.hwnd().ok()).map(|h| h.0 as isize).unwrap_or(0)
}

#[cfg(windows)]
async fn check_package<R: Runtime>(app: &AppHandle<R>, asked: bool) -> Result<String, String> {
    let up = app.state::<Updates>();
    let route = up.route.unwrap_or(package::Route::Sideloaded);
    let h = hwnd(app);
    let found = tauri::async_runtime::spawn_blocking(move || package::check(route, h)).await.map_err(|e| format!("install: {e}"))??;
    let name = app.package_info().name.clone();
    let current = package::version().map(|v| package::short_version(&v)).unwrap_or_default();
    let Some(found) = found else {
        up.remember("current", "", "", asked);
        if asked {
            say_current(app, &name, &current);
        }
        return Ok("current".into());
    };
    let latest = found.version.unwrap_or_default();
    // Release notes for what Windows offers: the endpoint knows them for our own route (msix-web); the Store has its listing.
    let notes = String::new();
    up.remember("available", &latest, &notes, asked);
    let v = if latest.is_empty() { "A newer version".to_string() } else { format!("{name} {latest}") };
    let from = if route == package::Route::Store { "the Microsoft Store" } else { "getcadcraft.com" };
    let text = format!("{v} is available from {from} (you have {current}).\n\nInstall it now? {name} closes and opens again; save your work first.");
    if !ask(app, "Update available", text, "Install now").await {
        up.remember("declined", &latest, &notes, asked);
        return Ok("declined".into());
    }
    up.remember("installing", &latest, &notes, asked);
    up.log(&format!("installing {latest} (from {current}) through {}", route.name()));
    tauri::async_runtime::spawn_blocking(move || package::install(route, h)).await.map_err(|e| format!("install: {e}"))??;
    // App Installer shuts Studio down before this returns (and starts it again); the Store's own window does the rest.
    Ok("installing".into())
}

#[cfg(not(windows))]
async fn check_package<R: Runtime>(_app: &AppHandle<R>, _asked: bool) -> Result<String, String> {
    Err("not_store: not a Windows package".into())
}

#[tauri::command]
pub fn cc_updates_status<R: Runtime>(app: AppHandle<R>, up: State<'_, Updates>) -> UpdateStatus {
    up.status(&app)
}

#[tauri::command]
pub async fn cc_updates_check<R: Runtime>(app: AppHandle<R>) -> Result<String, String> {
    check(&app, true).await
}

#[tauri::command]
pub fn cc_updates_set_auto<R: Runtime>(app: AppHandle<R>, up: State<'_, Updates>, on: bool) -> UpdateStatus {
    {
        let mut p = up.prefs.lock().unwrap();
        p.auto = Some(on);
        up.save(&p);
    }
    up.status(&app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri_plugin_updater::Error as E;

    #[test]
    fn a_feed_without_this_system_is_not_a_network_failure() {
        assert_eq!(failure_code(&E::TargetNotFound("linux-x86_64".into())), "no_build");
        assert_eq!(failure_code(&E::TargetsNotFound(vec!["linux-x86_64".into(), "linux".into()])), "no_build");
        assert!(failure(&E::TargetNotFound("linux-x86_64".into())).starts_with("no_build: "));
    }

    #[test]
    fn each_failure_has_its_own_code() {
        assert_eq!(failure_code(&E::Network("reset".into())), "network");
        assert_eq!(failure_code(&E::ReleaseNotFound), "feed");
        assert_eq!(failure_code(&E::SignatureUtf8("x".into())), "signature");
        assert_eq!(failure_code(&E::MissingSignedVersion), "signature");
        assert_eq!(failure_code(&E::TempDirNotFound), "install");
        assert_eq!(failure_code(&E::DebInstallFailed), "install");
    }

    #[test]
    fn the_code_is_what_the_bridge_reads() {
        // bridge.js: /^([a-z_]+): / — the code is lower case and underscores, then a colon and a space.
        for e in [E::ReleaseNotFound, E::TargetNotFound("x".into()), E::Network("x".into()), E::MissingSignedVersion, E::DebInstallFailed] {
            let m = failure(&e);
            let code = m.split(": ").next().unwrap();
            assert!(code.chars().all(|c| c.is_ascii_lowercase() || c == '_'), "{m}");
        }
        assert_eq!(code_of("not_store: a file"), "not_store");
        assert_eq!(code_of("Weird text"), "install");
    }

    fn cfg(endpoint: &str, pubkey: &str, test: bool) -> UpdateConfig {
        UpdateConfig { endpoint: endpoint.into(), pubkey: pubkey.into(), test, ..Default::default() }
    }

    #[test]
    fn a_package_never_runs_the_file_updater() {
        let c = cfg("https://getcadcraft.com/api/v1/updates/{{target}}/{{arch}}/{{current_version}}?edition=msix-web", "key", false);
        assert_eq!(source_for(Some(&c), false, true), Some(Source::Package));
        assert_eq!(source_for(None, false, true), Some(Source::Package));
        assert_eq!(source_for(Some(&c), true, true), Some(Source::Package));
    }

    #[test]
    fn the_portable_copy_only_tells() {
        let c = cfg("https://getcadcraft.com/api/v1/updates/{{target}}/{{arch}}/{{current_version}}?edition=portable", "key", false);
        assert_eq!(source_for(Some(&c), true, false), Some(Source::Notify));
        assert_eq!(source_for(Some(&c), false, false), Some(Source::Feed));
    }

    #[test]
    fn no_key_or_no_https_means_no_updater() {
        assert_eq!(source_for(None, false, false), None);
        assert_eq!(source_for(Some(&cfg("https://getcadcraft.com/x", " ", false)), false, false), None);
        assert_eq!(source_for(Some(&cfg("http://getcadcraft.com/x", "key", false)), false, false), None);
        // http only for a test build, and only on this computer
        assert_eq!(source_for(Some(&cfg("http://127.0.0.1:8765/x", "key", false)), false, false), None);
        assert_eq!(source_for(Some(&cfg("http://127.0.0.1:8765/x", "key", true)), false, false), Some(Source::Feed));
        assert_eq!(source_for(Some(&cfg("http://example.com/x", "key", true)), false, false), None);
    }

    #[test]
    fn the_edition_goes_into_the_endpoint() {
        let e = "https://getcadcraft.com/api/v1/updates/{{target}}/{{arch}}/{{current_version}}?app=studio&edition={edition}";
        assert_eq!(endpoint_for(e, "installer"), "https://getcadcraft.com/api/v1/updates/{{target}}/{{arch}}/{{current_version}}?app=studio&edition=installer");
        assert_eq!(resolve_edition("linux", true).as_deref(), Some("appimage"));
        assert_eq!(resolve_edition("linux", false).as_deref(), Some("deb"));
        assert_eq!(resolve_edition("portable", false).as_deref(), Some("portable"));
        assert_eq!(resolve_edition("", false), None);
    }

    #[test]
    fn automatic_checks_are_once_a_day() {
        assert!(due(None, 5), "never checked: due");
        assert!(!due(Some(1_000), 1_000 + DAY_MS - 1), "less than a day: not due");
        assert!(due(Some(1_000), 1_000 + DAY_MS), "a day: due");
        assert!(due(Some(10 * DAY_MS), DAY_MS), "the clock went back: due");
        // On by default; an older shell.json without `auto` counts as on; the person's choice wins.
        assert!(UpdateConfig::default().auto);
        let c: UpdateConfig = serde_json::from_str(r#"{"endpoint":"https://x","pubkey":"k"}"#).unwrap();
        assert!(c.auto);
        let off: UpdateConfig = serde_json::from_str(r#"{"endpoint":"https://x","pubkey":"k","auto":false}"#).unwrap();
        assert!(!off.auto);
        let dir = std::env::temp_dir().join(format!("cc-updates-auto-{}", std::process::id()));
        let up = Updates::new(Some(cfg("https://getcadcraft.com/x", "key", false)), false, Some(dir.join("updates.json")));
        assert!(up.auto());
        up.prefs.lock().unwrap().auto = Some(false);
        assert!(!up.auto(), "the person's off wins");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_finished_update_is_noted_after_the_restart() {
        let dir = std::env::temp_dir().join(format!("cc-updates-done-{}", std::process::id()));
        let up = Updates::new(Some(cfg("https://getcadcraft.com/x", "key", false)), false, Some(dir.join("updates.json")));
        up.remember("installing", "0.7.1", "n", true);
        up.note_finished_update("0.7.0");
        assert_eq!(up.prefs.lock().unwrap().last.as_ref().unwrap().outcome, "installing", "still the old version: unchanged");
        up.note_finished_update("0.7.1");
        assert_eq!(up.prefs.lock().unwrap().last.as_ref().unwrap().outcome, "installed");
        let log = std::fs::read_to_string(dir.join("updates.log")).unwrap();
        assert!(log.trim_end().ends_with("updated to 0.7.1"), "{log}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_last_check_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("cc-updates-test-{}", std::process::id()));
        let f = dir.join("updates.json");
        let c = cfg("https://getcadcraft.com/x", "key", false);
        let up = Updates::new(Some(c.clone()), false, Some(f.clone()));
        up.remember("available", "0.7.1", "Fixes", true);
        let again = Updates::new(Some(c), false, Some(f));
        let l = again.prefs.lock().unwrap().last.clone().unwrap();
        assert_eq!((l.outcome.as_str(), l.latest.as_str(), l.notes.as_str(), l.asked), ("available", "0.7.1", "Fixes", true));
        assert!(l.at > 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
