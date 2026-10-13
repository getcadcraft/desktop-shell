// SPDX-License-Identifier: Apache-2.0
// CadCraft desktop shell (docs/0.4-PART2.md track W): one window with the system webview (WebView2 on Windows) showing
// one app's browser edition, served from the install folder through the `cadcraft` protocol — no web server, no port.
// The page gets `window.cadcraftDesktop` (bridge/bridge.js, for the apps' host.js); links that leave the app open in the
// person's own browser; downloads ask where to save. No app code lives here: the app is the edition folder next to the
// executable (`app/`) plus `shell.json` (name, slug, extra headers).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod account;
mod control;
mod files;
mod protocol;
mod updates;
mod windows;

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use tauri::webview::{DownloadEvent, NewWindowResponse};
use tauri::{AppHandle, Manager, Runtime, Webview, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

const BRIDGE: &str = include_str!("../../bridge/bridge.js");

/// The flags script injected before the bridge (window.__ccShellFlags).
struct ShellFlags(String);
static POPUPS: AtomicU32 = AtomicU32::new(0);
static PLACE_CHANGES: AtomicU32 = AtomicU32::new(0);

/// `shell.json` beside the app folder — written by build.mjs from apps.json.
#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct ShellConfig {
    app: String,
    name: String,
    version: String,
    headers: BTreeMap<String, String>,
    /// Studio: the hub at the root, each app under apps/<slug>/ in windows of its own (windows.rs).
    suite: bool,
    /// false: no sign-in in this build (store builds until decision 11). Default true.
    account: Option<bool>,
    /// W.7: present only when the build has a signed release feed (see updates.rs).
    updates: Option<updates::UpdateConfig>,
}

/// Where the edition lives: `<resources>/app`. Only when that is missing (a development run from target/), the folder
/// named by CADCRAFT_DESKTOP_ROOT.
fn site_root<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    let bundled = app.path().resource_dir().map(|d| d.join("app")).unwrap_or_default();
    if bundled.join("index.html").is_file() {
        return bundled;
    }
    std::env::var_os("CADCRAFT_DESKTOP_ROOT").map(PathBuf::from).unwrap_or(bundled)
}

fn read_config(root: &std::path::Path) -> ShellConfig {
    let p = root.parent().map(|d| d.join("shell.json")).unwrap_or_default();
    std::fs::read(&p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// A window none of whose area is on a connected screen (a saved place on a monitor that is gone) is centred on the
/// main window's screen instead.
pub(crate) fn keep_on_screen<R: Runtime>(w: &tauri::WebviewWindow<R>) {
    let (Ok(pos), Ok(size)) = (w.outer_position(), w.outer_size()) else { return };
    let monitors = w.available_monitors().unwrap_or_default();
    let visible = |m: &tauri::Monitor| {
        let (mp, ms) = (m.position(), m.size());
        let x = (pos.x + size.width as i32).min(mp.x + ms.width as i32) - pos.x.max(mp.x);
        let y = (pos.y + size.height as i32).min(mp.y + ms.height as i32) - pos.y.max(mp.y);
        x >= 64 && y >= 32
    };
    if monitors.iter().any(visible) {
        return;
    }
    let target = w
        .app_handle()
        .get_webview_window("main")
        .and_then(|m| m.current_monitor().ok().flatten())
        .or_else(|| w.primary_monitor().ok().flatten());
    if let Some(m) = target {
        let (mp, ms) = (m.position(), m.size());
        let wdt = size.width.min(ms.width);
        let hgt = size.height.min(ms.height);
        let _ = w.set_size(tauri::PhysicalSize::new(wdt, hgt));
        let _ = w.set_position(tauri::PhysicalPosition::new(mp.x + (ms.width - wdt) as i32 / 2, mp.y + (ms.height - hgt) as i32 / 2));
    }
}

/// Downloads (an app's "Export", "Download PDF", …): ask where to save, like any desktop program. Runs on the UI
/// thread, so it uses rfd's own synchronous dialog (the dialog plugin's blocking calls would wait on this thread).
fn on_download<R: Runtime>(webview: Webview<R>, event: DownloadEvent<'_>) -> bool {
    match event {
        DownloadEvent::Requested { destination, .. } => {
            let mut d = rfd::FileDialog::new().set_title("Save");
            if let Some(n) = destination.file_name() {
                d = d.set_file_name(n.to_string_lossy());
            }
            if let Some(dir) = destination.parent() {
                d = d.set_directory(dir);
            }
            let window = webview.window();
            d = d.set_parent(&window);
            let picked = match files::test_answer() {
                Some(a) => a.and_then(|v| v.into_iter().next()),
                None => d.save_file(),
            };
            match picked {
                Some(p) => {
                    webview.app_handle().state::<files::Grants>().grant_file(&p);
                    *destination = p;
                    true
                }
                None => false,
            }
        }
        DownloadEvent::Finished { .. } => true,
        _ => true,
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            // A second start (double-click on a file while the app runs): hand the files to this window.
            let files = files::files_from_args(argv.into_iter().skip(1), std::path::Path::new(&cwd));
            app.state::<files::Launch>().add(app, files);
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        // W.4b: the main window's size, position, screen and maximized state come back on the next start; a saved place
        // on a screen that is no longer connected is dropped (the plugin checks the monitors). Pop-ups are placed by the
        // app itself (app-panels.js remembers the panels window), so only "main" is kept here.
        .plugin(tauri_plugin_window_state::Builder::new().with_filter(|label| label == "main").build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .register_asynchronous_uri_scheme_protocol(protocol::SCHEME, |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            std::thread::spawn(move || {
                let site = app.state::<protocol::Site>();
                responder.respond(protocol::respond(&site, &request));
            });
        })
        .invoke_handler(tauri::generate_handler![
            files::cc_open_dialog,
            files::cc_save_dialog,
            files::cc_open_folder_dialog,
            files::cc_read_file,
            files::cc_write_file,
            files::cc_stat,
            files::cc_list_dir,
            files::cc_launch_files,
            cc_info,
            cc_set_fullscreen,
            cc_account_window,
            cc_close_window,
            account::cc_account_status,
            account::cc_account_sign_in,
            account::cc_account_cancel,
            account::cc_account_sign_out,
            account::cc_account_open_device,
            account::cc_cloud_request,
            updates::cc_updates_status,
            updates::cc_updates_check,
            updates::cc_updates_set_auto,
            windows::cc_windows_open,
            windows::cc_windows_list,
            windows::cc_windows_focus,
            windows::cc_windows_focus_hub,
            control::cc_control_start,
            control::cc_control_stop,
            control::cc_control_server,
            control::cc_control_new_token,
            control::cc_control_drop_session,
            control::cc_control_reply,
        ])
        .setup(|app| {
            let root = site_root(app.handle());
            let cfg = read_config(&root);
            let headers = cfg.headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            app.manage(windows::Suite::new(cfg.suite, root.clone(), app.path().app_data_dir().ok().map(|d| d.join("places.json"))));
            app.manage(protocol::Site { root, headers });
            app.manage(control::Control::default());
            app.manage(files::Grants::load(app.path().app_data_dir().ok().map(|d| d.join("grants.json"))));
            app.manage(files::Launch::default());
            app.manage(cfg.clone());
            let app_name = if cfg.name.is_empty() { app.package_info().name.clone() } else { cfg.name.clone() };
            let account_on = cfg.account.unwrap_or(true);
            app.manage(account::Account::new(&app.config().identifier, &app_name, account_on));
            // Read by bridge.js before it builds window.cadcraftDesktop (only these flags; no secrets).
            let flags = format!("window.__ccShellFlags=Object.freeze({{account:{account_on},suite:{}}});", cfg.suite);
            app.manage(ShellFlags(flags));
            app.manage(updates::Updates::new(cfg.updates.clone(), app.path().app_data_dir().ok().map(|d| d.join("updates.json"))));
            updates::on_start(app.handle());
            let cwd = std::env::current_dir().unwrap_or_default();
            let launch = files::files_from_args(std::env::args().skip(1), &cwd);
            app.state::<files::Launch>().add(app.handle(), launch);

            let first_start = !app
                .path()
                .app_config_dir()
                .map(|d| d.join(tauri_plugin_window_state::DEFAULT_FILENAME))
                .ok()
                .and_then(|f| std::fs::read(f).ok())
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .is_some_and(|v| v.get("main").is_some());
            let title = if cfg.name.is_empty() { app.package_info().name.clone() } else { cfg.name.clone() };
            app_window(app.handle(), "main", WebviewUrl::CustomProtocol(protocol::start_url("index.html")), &title)
                // Maximized on the very first start only; after that the window-state plugin brings back the person's own.
                .inner_size(1440.0, 900.0)
                .maximized(first_start)
                .build()?;
            // Keep the window's place on disk soon after it stops moving or resizing, not only on a clean exit (a crash or a
            // forced logoff would otherwise lose it).
            if let Some(main) = app.get_webview_window("main") {
                let h = app.handle().clone();
                main.on_window_event(move |e| {
                    if matches!(e, tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_)) {
                        let n = PLACE_CHANGES.fetch_add(1, Ordering::Relaxed) + 1;
                        let h = h.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(700));
                            if PLACE_CHANGES.load(Ordering::Relaxed) == n {
                                use tauri_plugin_window_state::{AppHandleExt, StateFlags};
                                let _ = h.save_window_state(StateFlags::all());
                            }
                        });
                    }
                });
            }
            if let Some(main) = app.get_webview_window("main") {
                allow_script_popups(&main);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("CadCraft desktop shell failed to start");
}

/// Every window that shows our pages (the hub/app main window, and in suite mode each app window): the flags and the
/// bridge before the page, links out to the person's browser, the app's own pop-ups as real windows, downloads → "Save as".
pub(crate) fn app_window<'a, R: Runtime>(app: &'a AppHandle<R>, label: &str, url: WebviewUrl, title: &str) -> WebviewWindowBuilder<'a, R, AppHandle<R>> {
    let handle = app.clone();
    let popup_handle = app.clone();
    let popup_title = title.to_string();
    WebviewWindowBuilder::new(app, label, url)
        .title(title)
        .initialization_script(&app.state::<ShellFlags>().0)
        .initialization_script(BRIDGE)
        // window.open: the app's own pop-ups (the movable panels' second-monitor window is a same-origin about:blank the
        // page fills itself) become real windows linked to this one, so window.opener works and the size/position
        // features are honored; anything else opens in the person's browser.
        .on_new_window(move |url, features| {
            if !protocol::is_local(&url) {
                let _ = popup_handle.opener().open_url(url.as_str(), None::<&str>);
                return NewWindowResponse::Deny;
            }
            let label = format!("popup-{}", POPUPS.fetch_add(1, Ordering::Relaxed));
            match WebviewWindowBuilder::new(&popup_handle, label, WebviewUrl::External(url.clone()))
                .window_features(features)
                .title(&popup_title)
                .on_document_title_changed(|w, t| {
                    if !t.is_empty() {
                        let _ = w.set_title(&t);
                    }
                })
                .on_download(on_download)
                .build()
            {
                Ok(window) => {
                    keep_on_screen(&window);
                    NewWindowResponse::Create { window }
                }
                Err(_) => NewWindowResponse::Deny,
            }
        })
        .on_download(on_download)
        .on_document_title_changed(|w, t| {
            if !t.is_empty() {
                let _ = w.set_title(&t);
            }
        })
        .min_inner_size(960.0, 600.0)
        .on_navigation(move |url| {
            if protocol::is_local(url) {
                return true;
            }
            let _ = handle.opener().open_url(url.as_str(), None::<&str>);
            false
        })
}

/// The app reopens its panels window by itself at start (W.4b), without a click. Only our own pages run here.
pub(crate) fn allow_script_popups<R: Runtime>(_w: &tauri::WebviewWindow<R>) {
    #[cfg(target_os = "linux")]
    let _ = _w.with_webview(|wv| {
        use webkit2gtk::{SettingsExt, WebViewExt};
        if let Some(st) = WebViewExt::settings(&wv.inner()) {
            st.set_javascript_can_open_windows_automatically(true);
        }
    });
}

/// What the page may know about the shell: versions and the platform (for help variants and the About box).
#[tauri::command]
fn cc_info(app: AppHandle, window: tauri::WebviewWindow, cfg: tauri::State<'_, ShellConfig>) -> serde_json::Value {
    serde_json::json!({
        "windowId": window.label(),
        "suite": cfg.suite,
        "shell": app.package_info().version.to_string(),
        "app": cfg.app,
        "name": cfg.name,
        "version": cfg.version,
        "platform": std::env::consts::OS,
    })
}

/// The page entered or left element fullscreen: the window follows, so it covers the screen (WebView2 alone would only
/// fill the window's client area).
#[tauri::command]
fn cc_set_fullscreen(window: tauri::WebviewWindow, on: bool) {
    if window.is_fullscreen().unwrap_or(false) != on {
        let _ = window.set_fullscreen(on);
    }
}

/// The sign-in window (C.2): the shell's own page, one at a time, centred over the app.
#[tauri::command]
fn cc_account_window(app: AppHandle) -> Result<(), String> {
    if !app.state::<account::Account>().enabled {
        return Err("off: this build has no sign-in".into());
    }
    if let Some(w) = app.get_webview_window("account") {
        let _ = w.unminimize();
        return w.set_focus().map_err(|e| e.to_string());
    }
    let mut b = WebviewWindowBuilder::new(&app, "account", WebviewUrl::CustomProtocol(protocol::start_url(".shell/account.html")))
        .title("Sign in to getcadcraft.com")
        .initialization_script(&app.state::<ShellFlags>().0)
        .initialization_script(BRIDGE)
        .inner_size(460.0, 520.0)
        .resizable(false)
        .center();
    if let Some(main) = app.get_webview_window("main") {
        b = b.parent(&main).map_err(|e| e.to_string())?;
    }
    b.build().map(|_| ()).map_err(|e| e.to_string())
}

/// Closes the calling window — the shell's sign-in window, or (suite mode) an app window closing itself, e.g. its
/// "Close window" — since scripts may not close windows they did not open. Never the hub or another window.
#[tauri::command]
fn cc_close_window(window: tauri::WebviewWindow) {
    if window.label() == "account" || window.label().starts_with("app-") {
        let _ = window.close();
    }
}
