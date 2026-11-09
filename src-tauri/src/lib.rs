// SPDX-License-Identifier: Apache-2.0
// CadCraft desktop shell (docs/0.4-PART2.md track W): one window with the system webview (WebView2 on Windows) showing
// one app's browser edition, served from the install folder through the `cadcraft` protocol — no web server, no port.
// The page gets `window.cadcraftDesktop` (bridge/bridge.js, for the apps' host.js); links that leave the app open in the
// person's own browser; downloads ask where to save. No app code lives here: the app is the edition folder next to the
// executable (`app/`) plus `shell.json` (name, slug, extra headers).
//
// Tablets (docs/0.5-PART3.md track TB): the same library is Studio on Android and iPad. There is one web view there, so
// the bridge shows each app as a full-screen view inside it (bridge.js, "views"); desktop-only pieces stay off
// (`cfg(desktop)`): the updater, single instance, window places, pop-up windows, "Save as" for downloads, the assistants'
// local MCP server and carrying 0.4 per-app data. Files and the sign-in token go through src/mobile.rs.

mod account;
#[cfg(desktop)]
mod carry;
#[cfg(desktop)]
mod connect;
#[cfg(desktop)]
mod control;
// Bring your own key (0.8 AC.11) and "Send to a machine" (0.8 EG.40): the web view can set/ask/forget a key, never read it back.
#[cfg(desktop)]
mod ask_keys;
#[cfg(desktop)]
mod machines;
// Read an MTConnect agent on the local network (0.9 D8.13): the native half of ccStudioBridge.hostFetch.
#[cfg(desktop)]
mod hostfetch;
mod files;
mod lock_ops;   // 0.8 SU.1058: create-only and remove for strict vault locks (used by files.rs)
mod links;
mod startlog;   // 0.9 FX.29: the start-up log a person can send
#[cfg(target_os = "linux")]
mod linux;
// Studio Live (docs/0.5-PLAN.md track LV): the session server, owned by live-core (crypto.rs by live-relay), and Studio's glue
// for it (studio_live.rs: cadcraftDesktop.live). Default feature `live`; `build.mjs --no-live` leaves both out.
#[cfg(feature = "live")]
#[allow(dead_code, unused_imports)]
mod live;
#[cfg(mobile)]
mod mobile;
mod package;
mod portable;
#[cfg(desktop)]
mod print;
mod protocol;
#[cfg(feature = "live")]
mod studio_live;
// LAN render nodes (0.7 CU.7): finding and reaching other Studios' computers; bytes only (render_nodes.rs).
#[cfg(all(feature = "live", desktop))]
mod render_nodes;
// The Scan button's network scanners (Super 0.1, Start from a scan): eSCL scanners found by mDNS, requests carried on the LAN (scanner.rs).
#[cfg(all(feature = "live", desktop))]
mod scanner;
// Outside render programs and "Open in…" (0.8 RE.3/RE.5/RE.6/RE.7). Blender (GPL) is not in the shipped table: owner rule 43, see engines.rs.
#[cfg(desktop)]
mod engines;
// A Modbus TCP socket on this computer for the Material flow add-on's PLC emulation (0.8 SU.858 step 5): plc.rs, std-only core tested alone.
#[cfg(desktop)]
mod plc;
#[cfg(desktop)]
mod updates;
mod windows;

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
#[cfg(desktop)]
use std::sync::atomic::{AtomicU32, Ordering};
#[cfg(desktop)]
use tauri::webview::{DownloadEvent, NewWindowResponse};
use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};
#[cfg(desktop)]
use tauri::Webview;
use tauri_plugin_opener::OpenerExt;

const BRIDGE: &str = include_str!("../../bridge/bridge.js");

/// The flags script injected before the bridge (window.__ccShellFlags).
struct ShellFlags(String);
#[cfg(desktop)]
static POPUPS: AtomicU32 = AtomicU32::new(0);
#[cfg(desktop)]
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
    /// A store package (build.mjs --store): the store updates it; info() says so, so pages word updates right.
    store: bool,
    /// D7.53: the zip: data beside the program, no updater (portable.rs). Read by portable::init() before this.
    portable: bool,
    /// W.7: present only when the build has a signed release feed (see updates.rs).
    #[cfg(desktop)]
    updates: Option<updates::UpdateConfig>,
}

/// Where the edition lives: `<resources>/app`. Only when that is missing (a development run from target/), the folder
/// named by CADCRAFT_DESKTOP_ROOT. Android: `app/` in the package's assets (protocol.rs reads them in place).
#[cfg(not(target_os = "android"))]
fn site_root<R: Runtime>(app: &AppHandle<R>) -> PathBuf {
    let bundled = app.path().resource_dir().map(|d| d.join("app")).unwrap_or_default();
    if bundled.join("index.html").is_file() {
        return bundled;
    }
    std::env::var_os("CADCRAFT_DESKTOP_ROOT").map(PathBuf::from).unwrap_or(bundled)
}
#[cfg(target_os = "android")]
fn site_root<R: Runtime>(_app: &AppHandle<R>) -> PathBuf {
    PathBuf::from("app")
}

fn read_config(root: &std::path::Path) -> ShellConfig {
    let p = root.parent().map(|d| d.join("shell.json")).unwrap_or_default();
    protocol::read_source(&p).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// A window none of whose area is on a connected screen (a saved place on a monitor that is gone) is centred on the
/// main window's screen instead.
#[cfg(desktop)]
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
/// Tablets: the bridge turns `<a download>` into the save picker itself (bridge.js).
#[cfg(desktop)]
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    startlog::mark("run: begin");
    startlog::install_panic_hook();
    // Portable copy (0.7 D7.53): found before anything reads or writes a data folder.
    portable::init();
    startlog::mark(if portable::is_portable() { "portable copy found" } else { "installed copy (no portable data folder)" });
    // Linux (0.7 D7.57): before WebKitGTK starts, so a driver it can't draw with gives a window, not a black one.
    #[cfg(target_os = "linux")]
    linux::guard();
    startlog::mark("builder: plugins next (single-instance first on desktop: a second start hands over and exits)");
    let builder = tauri::Builder::default();
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            // On macOS the plugin makes a second start connect to this copy's socket and exit at once, whether or not this copy ever got a window
            // up (a first start that hangs makes every later double-click do nothing): the log says whether a main window exists.
            startlog::mark(&format!("a second start was handed to this running copy; main window present: {}", app.get_webview_window("main").is_some()));
            // A second start (double-click on a file, or a ccstudio:// link while Studio runs): hand them to this window.
            let args: Vec<String> = argv.into_iter().skip(1).collect();
            hand_over(app, args, std::path::Path::new(&cwd));
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        // W.4b: the main window's size, position, screen and maximized state come back on the next start; a saved place
        // on a screen that is no longer connected is dropped (the plugin checks the monitors). Pop-ups are placed by the
        // app itself (app-panels.js remembers the panels window), so only "main" is kept here.
        .plugin(tauri_plugin_updater::Builder::new().build());
    // A portable copy keeps the main window's place in data/places.json instead (the plugin writes to the PC's profile).
    #[cfg(desktop)]
    let builder = if portable::is_portable() {
        builder
    } else {
        builder.plugin(tauri_plugin_window_state::Builder::new().with_filter(|label| label == "main").build())
    };
    // Tablets: documents (Android's Storage Access Framework, iPad's document picker) and the sign-in token's store.
    #[cfg(mobile)]
    let builder = builder.plugin(mobile::init());
    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .register_asynchronous_uri_scheme_protocol(protocol::SCHEME, |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            std::thread::spawn(move || {
                let site = app.state::<protocol::Site>();
                responder.respond(protocol::respond(&site, &request));
            });
        })
        .invoke_handler(tauri::generate_handler![
            #[cfg(desktop)]
            print::cc_printers_list,
            #[cfg(desktop)]
            print::cc_print,
            files::cc_open_dialog,
            files::cc_save_dialog,
            files::cc_open_folder_dialog,
            files::cc_read_file,
            files::cc_write_file,
            files::cc_create_file_exclusive,
            files::cc_remove_file,
            files::cc_stat,
            files::cc_list_dir,
            files::cc_launch_files,
            links::cc_links_launch,
            links::cc_launch_apps,
            #[cfg(desktop)]
            carry::cc_carry_plan,
            #[cfg(desktop)]
            carry::cc_carry_put,
            #[cfg(desktop)]
            carry::cc_carry_take,
            #[cfg(desktop)]
            carry::cc_carry_done,
            cc_info,
            cc_set_fullscreen,
            cc_is_fullscreen,
            cc_account_window,
            cc_close_window,
            cc_open_url,
            cc_test_windows,
            account::cc_account_status,
            account::cc_account_sign_in,
            account::cc_account_cancel,
            account::cc_account_sign_out,
            account::cc_account_open_device,
            account::cc_cloud_request,
            #[cfg(desktop)]
            updates::cc_updates_status,
            #[cfg(desktop)]
            updates::cc_updates_check,
            #[cfg(desktop)]
            updates::cc_updates_set_auto,
            #[cfg(desktop)]
            windows::cc_windows_open,
            #[cfg(desktop)]
            windows::cc_windows_list,
            #[cfg(desktop)]
            windows::cc_windows_focus,
            #[cfg(desktop)]
            windows::cc_windows_focus_hub,
            #[cfg(desktop)]
            control::cc_control_start,
            #[cfg(desktop)]
            control::cc_control_stop,
            #[cfg(desktop)]
            control::cc_control_server,
            #[cfg(desktop)]
            control::cc_control_new_token,
            #[cfg(desktop)]
            control::cc_control_drop_session,
            #[cfg(desktop)]
            control::cc_control_reply,
            #[cfg(desktop)]
            connect::cc_connect_env,
            #[cfg(desktop)]
            connect::cc_connect_exists,
            #[cfg(desktop)]
            connect::cc_connect_read,
            #[cfg(desktop)]
            connect::cc_connect_write,
            #[cfg(desktop)]
            connect::cc_connect_rename,
            #[cfg(desktop)]
            connect::cc_connect_mkdirp,
            #[cfg(desktop)]
            connect::cc_connect_which,
            #[cfg(desktop)]
            connect::cc_connect_run,
            #[cfg(desktop)]
            engines::cc_engine_list,
            #[cfg(desktop)]
            engines::cc_engine_find,
            #[cfg(desktop)]
            engines::cc_engine_pick,
            #[cfg(desktop)]
            engines::cc_engine_forget,
            #[cfg(desktop)]
            engines::cc_engine_start,
            #[cfg(desktop)]
            engines::cc_engine_status,
            #[cfg(desktop)]
            engines::cc_engine_result,
            #[cfg(desktop)]
            engines::cc_engine_cancel,
            #[cfg(desktop)]
            engines::cc_engine_open_list,
            #[cfg(desktop)]
            engines::cc_engine_open,
            #[cfg(desktop)]
            engines::cc_engine_bambu_connect,
            #[cfg(desktop)]
            engines::cc_engine_project,
            #[cfg(desktop)]
            engines::cc_engine_project_pick,
            #[cfg(desktop)]
            engines::cc_engine_send,
            #[cfg(desktop)]
            engines::cc_engine_addon_find,
            #[cfg(desktop)]
            engines::cc_engine_addon_pick,
            #[cfg(desktop)]
            engines::cc_engine_addon_start,
            #[cfg(desktop)]
            plc::cc_plc_start,
            #[cfg(desktop)]
            plc::cc_plc_poll,
            #[cfg(desktop)]
            plc::cc_plc_respond,
            #[cfg(desktop)]
            plc::cc_plc_stop,
            #[cfg(desktop)]
            plc::cc_plc_connect,
            #[cfg(desktop)]
            plc::cc_plc_exchange,
            #[cfg(desktop)]
            plc::cc_plc_disconnect,
            #[cfg(desktop)]
            ask_keys::cc_ask_key_set,
            #[cfg(desktop)]
            ask_keys::cc_ask_key_has,
            #[cfg(desktop)]
            ask_keys::cc_ask_key_delete,
            #[cfg(desktop)]
            ask_keys::cc_ask_call,
            #[cfg(desktop)]
            machines::cc_machine_request,
            #[cfg(desktop)]
            machines::cc_machine_key_set,
            #[cfg(desktop)]
            machines::cc_machine_key_has,
            #[cfg(desktop)]
            machines::cc_machine_key_forget,
            #[cfg(desktop)]
            hostfetch::cc_host_fetch,
            #[cfg(feature = "live")]
            studio_live::cc_live_open,
            #[cfg(feature = "live")]
            studio_live::cc_live_send,
            #[cfg(feature = "live")]
            studio_live::cc_live_recv,
            #[cfg(feature = "live")]
            studio_live::cc_live_close,
            #[cfg(feature = "live")]
            studio_live::cc_live_session,
            #[cfg(feature = "live")]
            studio_live::cc_live_advertise,
            #[cfg(feature = "live")]
            studio_live::cc_live_unadvertise,
            #[cfg(feature = "live")]
            studio_live::cc_live_browse,
            #[cfg(feature = "live")]
            studio_live::cc_live_relay,
            #[cfg(feature = "live")]
            studio_live::cc_live_relay_stop,
            #[cfg(feature = "live")]
            studio_live::cc_live_export,
            #[cfg(feature = "live")]
            studio_live::cc_live_start_from,
            #[cfg(all(feature = "live", mobile))]
            studio_live::cc_live_view_closed,
            #[cfg(all(feature = "live", desktop))]
            render_nodes::cc_render_advertise,
            #[cfg(all(feature = "live", desktop))]
            render_nodes::cc_render_unadvertise,
            #[cfg(all(feature = "live", desktop))]
            render_nodes::cc_render_accept,
            #[cfg(all(feature = "live", desktop))]
            render_nodes::cc_render_browse,
            #[cfg(all(feature = "live", desktop))]
            render_nodes::cc_render_connect,
            #[cfg(all(feature = "live", desktop))]
            render_nodes::cc_render_recv,
            #[cfg(all(feature = "live", desktop))]
            render_nodes::cc_render_send,
            #[cfg(all(feature = "live", desktop))]
            render_nodes::cc_render_close,
            #[cfg(all(feature = "live", desktop))]
            scanner::cc_scanners_browse,
            #[cfg(all(feature = "live", desktop))]
            scanner::cc_scanner_request,
        ])
        // A window's end also ends its Studio Live session (studio_live.rs).
        .on_window_event(|_w, _e| {
            #[cfg(feature = "live")]
            if matches!(_e, tauri::WindowEvent::Destroyed) {
                studio_live::on_window_destroyed(_w.app_handle(), _w.label());
                #[cfg(desktop)]
                render_nodes::on_window_destroyed(_w.app_handle(), _w.label());
            }
        })
        .setup(|app| {
            startlog::init(portable::data_file(app.handle(), "startup.log"));
            startlog::mark("setup: begin (the app-data folder is known)");
            let root = site_root(app.handle());
            let mut cfg = read_config(&root);
            // D7.53: a portable copy keeps its data beside the program and never replaces itself (updates.rs: it only says
            // when a newer zip is out).
            cfg.portable = portable::is_portable();
            let headers = cfg.headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            app.manage(windows::Suite::new(cfg.suite, root.clone(), portable::data_file(app.handle(), "places.json")));
            // Studio Live: app windows host and join; the suite folder is what browser joiners on the LAN get (LV.12).
            #[cfg(feature = "live")]
            app.manage(studio_live::StudioLive::new(cfg.suite, root.clone()));
            #[cfg(all(feature = "live", desktop))]
            app.manage(render_nodes::RenderNodes::new(cfg.suite));
            #[cfg(all(feature = "live", desktop))]
            app.manage(scanner::Scanners::default());
            app.manage(protocol::Site { root, headers });
            #[cfg(desktop)]
            app.manage(engines::Jobs::default());
            #[cfg(desktop)]
            app.manage(plc::Hub::default());
            #[cfg(desktop)]
            app.manage(plc::Client::default());
            #[cfg(desktop)]
            app.manage(control::Control::default());
            startlog::mark("setup: config read, windows, protocol and engine state managed");
            app.manage(files::Grants::load(portable::data_file(app.handle(), "grants.json")));
            app.manage(files::Launch::default());
            app.manage(links::Links::default());
            app.manage(links::LaunchApps::default());
            app.manage(cfg.clone());
            let app_name = if cfg.name.is_empty() { app.package_info().name.clone() } else { cfg.name.clone() };
            let account_on = cfg.account.unwrap_or(true);
            app.manage(account::Account::new(&app.config().identifier, &app_name, account_on));
            // Read by bridge.js before it builds window.cadcraftDesktop (only these flags; no secrets). `mobile`: one web
            // view, apps as views inside it (bridge.js). `nativeFullscreen` (macOS, GH.1): the page's full screen is the
            // window's own full screen, not WKWebView's element fullscreen (bridge.js nativeFullscreen).
            let flags = format!(
                "window.__ccShellFlags=Object.freeze({{account:{account_on},suite:{},live:{},mobile:{},nativeFullscreen:{}}});",
                cfg.suite,
                cfg!(feature = "live"),
                cfg!(mobile),
                cfg!(target_os = "macos")
            );
            app.manage(ShellFlags(flags));
            #[cfg(desktop)]
            {
                app.manage(updates::Updates::new(cfg.updates.clone(), cfg.portable, portable::data_file(app.handle(), "updates.json")));
                updates::on_start(app.handle());
                let cwd = std::env::current_dir().unwrap_or_default();
                let args: Vec<String> = std::env::args().skip(1).collect();
                hand_over(app.handle(), args, &cwd);
            }

            startlog::mark("setup: update check scheduled, launch arguments handed over");
            let title = if cfg.name.is_empty() { app.package_info().name.clone() } else { cfg.name.clone() };
            startlog::mark("setup: building the main window");
            #[cfg(desktop)]
            desktop_main_window(app.handle(), &title, cfg.suite)?;
            #[cfg(mobile)]
            app_window(app.handle(), "main", WebviewUrl::CustomProtocol(protocol::start_url("index.html")), &title).build()?;
            startlog::mark("setup: main window built, waiting for its page");
            startlog::watchdog();
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("CadCraft desktop shell failed to start")
        .run(|app, event| {
            match &event {
                tauri::RunEvent::Ready => startlog::mark("event loop ready"),
                tauri::RunEvent::ExitRequested { .. } => startlog::mark("exit requested"),
                _ => {}
            }
            // FX.27 / DC.117: macOS passes a double-clicked or "Open With" file as an Apple event, Android as a VIEW intent;
            // neither is in argv. Both reach the same hand-off as argv does.
            #[cfg(any(target_os = "macos", target_os = "ios", target_os = "android"))]
            if let tauri::RunEvent::Opened { urls } = &event {
                files::open_urls(app, urls);
            }
            let _ = (app, event);
        });
}

/// The one door for "open this" at the start and on a second start: files (paths or `file://`), `ccstudio://` join links and
/// `--app=<slug>` shortcuts. Files end in `files::Launch::add`, which every other source of an opened file uses too (FX.27).
pub(crate) fn hand_over<R: Runtime>(app: &AppHandle<R>, args: Vec<String>, cwd: &std::path::Path) {
    app.state::<links::Links>().0.add(app, links::links_from_args(&args));
    app.state::<links::LaunchApps>().0.add(app, links::apps_from_args(&args));
    let files = files::files_from_args(args, cwd);
    app.state::<files::Launch>().add(app, files);
}

/// Studio Live's outgoing WebSockets: one implementation, in live/ws.rs (the test harness builds it too).
#[cfg(feature = "live")]
pub(crate) use live::ws::ws_connect;

/// The desktop's main window: its place from the last run, the carry page on Studio's first start, pop-ups allowed.
#[cfg(desktop)]
fn desktop_main_window<R: Runtime>(app: &AppHandle<R>, title: &str, suite: bool) -> tauri::Result<()> {
    let first_start = if portable::is_portable() {
        !app.state::<windows::Suite>().has_place("main")
    } else {
        !app
            .path()
            .app_config_dir()
            .map(|d| d.join(tauri_plugin_window_state::DEFAULT_FILENAME))
            .ok()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .is_some_and(|v| v.get("main").is_some())
    };
    // SD.8: Studio's first start opens on the carry page while the separate apps' data comes over (carry.rs).
    let carry = carry::Carry::new(app, suite && !portable::is_portable());
    let first_page = if carry.on { ".shell/carry.html" } else { "index.html" };
    app.manage(carry);
    let main = app_window(app, "main", WebviewUrl::CustomProtocol(protocol::start_url(first_page)), title);
    // Maximized on the very first start only; after that the window-state plugin (a portable copy: places.json) brings
    // back the person's own.
    let main = match app.state::<windows::Suite>().place_for(app, "main").filter(|_| portable::is_portable()) {
        Some(p) => p.apply(main),
        None => main.inner_size(1440.0, 900.0).maximized(first_start),
    };
    main.build()?;
    if portable::is_portable() {
        if let Some(w) = app.get_webview_window("main") {
            windows::track_place(&w, "main");
        }
    }
    // Keep the window's place on disk soon after it stops moving or resizing, not only on a clean exit (a crash or a
    // forced logoff would otherwise lose it).
    if let Some(main) = app.get_webview_window("main") {
        let h = app.clone();
        main.on_window_event(move |e| {
            // A portable copy has no window-state plugin (places.json instead): saving through it would find no state.
            if !portable::is_portable() && matches!(e, tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_)) {
                let n = PLACE_CHANGES.fetch_add(1, Ordering::Relaxed) + 1;
                let h = h.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(700));
                    if PLACE_CHANGES.load(Ordering::Relaxed) == n {
                        // On the main thread, like the plugin's own Moved/Resized handler: from this thread the plugin
                        // holds its cache lock while it waits on the main thread for the window's size, and that thread
                        // is waiting for the same lock — Studio hung on macOS (71.3, Galax65). VISIBLE is left out: a
                        // hidden window must not come back hidden.
                        let h2 = h.clone();
                        let _ = h.run_on_main_thread(move || {
                            use tauri_plugin_window_state::{AppHandleExt, StateFlags};
                            let _ = h2.save_window_state(StateFlags::all() & !StateFlags::VISIBLE);
                        });
                    }
                });
            }
        });
    }
    if let Some(main) = app.get_webview_window("main") {
        allow_script_popups(&main);
    }
    // FX.27 + OF.4: a file dropped on the window (files::drop_on_window: add-on packages to the page, other files as a double-click).
    if let Some(main) = app.get_webview_window("main") {
        let h = app.clone();
        main.on_window_event(move |e| {
            if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = e {
                files::drop_on_window(&h, "main", paths);
            }
        });
    }
    #[cfg(windows)]
    sweep_closed_popups(app);
    Ok(())
}

/// Every window that shows our pages (the hub/app main window, and in suite mode each app window): the flags and the
/// bridge before the page, links out to the person's browser, the app's own pop-ups as real windows, downloads → "Save as".
#[cfg(desktop)]
pub(crate) fn app_window<'a, R: Runtime>(app: &'a AppHandle<R>, label: &str, url: WebviewUrl, title: &str) -> WebviewWindowBuilder<'a, R, AppHandle<R>> {
    let handle = app.clone();
    let popup_handle = app.clone();
    let popup_title = title.to_string();
    let b = WebviewWindowBuilder::new(app, label, url);
    // Portable (D7.53): one profile folder beside the program for every window (WebView2 wants the same one for all).
    let b = match portable::webview_dir() {
        Some(d) => b.data_directory(d),
        None => b,
    };
    b.title(title)
        .initialization_script(&app.state::<ShellFlags>().0)
        .initialization_script(BRIDGE)
        // FX.29: the main window's first page start and finish go into the start-up log (startlog.rs).
        .on_page_load(|w, p| {
            if w.label() == "main" {
                match p.event() {
                    tauri::webview::PageLoadEvent::Started => startlog::mark("main page started loading"),
                    tauri::webview::PageLoadEvent::Finished => startlog::page_loaded(),
                }
            }
        })
        // window.open: the app's own pop-ups (the movable panels' second-monitor window is a same-origin about:blank the
        // page fills itself) become real windows linked to this one, so window.opener works and the size/position
        // features are honored; anything else opens in the person's browser.
        .on_new_window(move |url, features| {
            if !protocol::is_local(&url) {
                let _ = popup_handle.opener().open_url(url.as_str(), None::<&str>);
                return NewWindowResponse::Deny;
            }
            let label = format!("popup-{}", POPUPS.fetch_add(1, Ordering::Relaxed));
            // Built on about:blank: the engine loads the asked-for page into the window it is handed (WebView2's NewWindow
            // must not have navigated; built on the page itself, a hand-off between apps — BimCraft's Takeoff into
            // TakeoffCraft — closed Studio on Windows, 0.6 Discord report). Tauri's own on_new_window example does the same.
            let pb = WebviewWindowBuilder::new(&popup_handle, label, WebviewUrl::External("about:blank".parse().expect("about:blank")));
            let pb = match portable::webview_dir() {
                Some(d) => pb.data_directory(d),
                None => pb,
            };
            match pb
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
                    close_with_page(&window);
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

/// Tablets: the one web view. The flags and the bridge go into every frame (each app is a same-origin view inside the
/// hub's page, bridge.js); links that leave Studio open in the person's browser.
#[cfg(mobile)]
pub(crate) fn app_window<'a, R: Runtime>(app: &'a AppHandle<R>, label: &str, url: WebviewUrl, title: &str) -> WebviewWindowBuilder<'a, R, AppHandle<R>> {
    let _ = title;
    let handle = app.clone();
    WebviewWindowBuilder::new(app, label, url)
        .initialization_script_for_all_frames(&app.state::<ShellFlags>().0)
        .initialization_script_for_all_frames(BRIDGE)
        .on_navigation(move |url| {
            if protocol::is_local(url) {
                return true;
            }
            let _ = handle.opener().open_url(url.as_str(), None::<&str>);
            false
        })
}

/// DR.5: a pop-up whose page is closed by script (the panels window after its last panel docks, "Put panels back", the
/// opener's `w.close()`) must go away as a window. wry only tears down the web view on that request (WebView2
/// `WindowCloseRequested` → its container HWND; WebKitGTK `close` → the widget), which left an empty native window on
/// screen. We hear the same request (Linux: the widget going away) and destroy the window too. Deferred to the event loop through another thread,
/// so it never runs inside the web view's own callback.
#[cfg(desktop)]
fn close_with_page<R: Runtime>(window: &tauri::WebviewWindow<R>) {
    let w = window.clone();
    let destroy = move || {
        let w = w.clone();
        std::thread::spawn(move || {
            let h = w.app_handle().clone();
            let _ = h.run_on_main_thread(move || {
                let _ = w.destroy();
            });
        });
    };
    // WebKitGTK: wry's `close` handler destroys the widget at once, and handlers connected after it never run; the
    // widget's own `destroy` does (it also fires when the window closes normally: destroying it again is a no-op).
    #[cfg(target_os = "linux")]
    let _ = window.with_webview(move |wv| {
        use gtk::prelude::WidgetExt;
        wv.inner().connect_destroy(move |_| destroy());
    });
    #[cfg(windows)]
    let _ = window.with_webview(move |wv| {
        use webview2_com::WindowCloseRequestedEventHandler;
        unsafe {
            if let Ok(core) = wv.controller().CoreWebView2() {
                let mut token = 0i64;
                let _ = core.add_WindowCloseRequested(
                    &WindowCloseRequestedEventHandler::create(Box::new(move |_, _| {
                        destroy();
                        Ok(())
                    })),
                    &mut token,
                );
            }
        }
    });
    #[cfg(not(any(target_os = "linux", windows)))]
    let _ = destroy;
}

/// DR.5, Windows fallback (not checkable on Linux): should the handler above ever not be reached after wry's, any pop-up
/// whose web view lost its container window (what wry's close does) is destroyed within a second.
#[cfg(windows)]
fn sweep_closed_popups<R: Runtime>(app: &AppHandle<R>) {
    let h = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        for (label, w) in h.webview_windows() {
            if !label.starts_with("popup-") {
                continue;
            }
            let w2 = w.clone();
            let _ = w.with_webview(move |wv| {
                use ::windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::IsWindow};
                let mut parent = HWND::default();
                let gone = unsafe { wv.controller().ParentWindow(&mut parent).is_err() || !IsWindow(Some(parent)).as_bool() };
                if gone {
                    let w3 = w2.clone();
                    std::thread::spawn(move || {
                        let h = w3.app_handle().clone();
                        let _ = h.run_on_main_thread(move || {
                            let _ = w3.destroy();
                        });
                    });
                }
            });
        }
    });
}

/// Debug builds only (e2e, DR.5): the shell's native windows, which WebDriver can't see once their web view is gone.
#[cfg(debug_assertions)]
#[tauri::command]
fn cc_test_windows(app: AppHandle) -> Vec<String> {
    let mut v: Vec<String> = app.webview_windows().into_keys().collect();
    v.sort();
    v
}
#[cfg(not(debug_assertions))]
#[tauri::command]
fn cc_test_windows() -> Result<Vec<String>, String> {
    Err("not in release builds".into())
}

/// The app reopens its panels window by itself at start (W.4b), without a click. Only our own pages run here.
#[cfg(desktop)]
pub(crate) fn allow_script_popups<R: Runtime>(_w: &tauri::WebviewWindow<R>) {
    #[cfg(target_os = "linux")]
    let _ = _w.with_webview(|wv| {
        use webkit2gtk::{SettingsExt, WebViewExt};
        if let Some(st) = WebViewExt::settings(&wv.inner()) {
            st.set_javascript_can_open_windows_automatically(true);
        }
    });
}

/// What the page may know about the shell: versions and the platform (for help variants and the About box). Tablets:
/// `platform` is `android` / `ios`, and a view inside the hub's page says its own id (bridge.js).
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
        "store": cfg.store,
        "portable": cfg.portable,
        "mobile": cfg!(mobile),
    })
}

/// The page entered or left element fullscreen: the window follows, so it covers the screen (WebView2 alone would only
/// fill the window's client area). On macOS bridge.js calls it instead of WKWebView's element fullscreen (GH.1): the
/// window's own full screen, the same as the green button. Tablets: the app already fills the screen.
#[tauri::command]
fn cc_set_fullscreen(window: tauri::WebviewWindow, on: bool) {
    #[cfg(desktop)]
    if window.is_fullscreen().unwrap_or(false) != on {
        let _ = window.set_fullscreen(on);
    }
    #[cfg(mobile)]
    let _ = (window, on);
}

/// Whether the window is full screen now (macOS: bridge.js resolves the page's request on it and notices the person
/// leaving full screen with the green button or the Window menu, GH.1). Tablets: never.
#[tauri::command]
fn cc_is_fullscreen(window: tauri::WebviewWindow) -> bool {
    #[cfg(desktop)]
    let on = window.is_fullscreen().unwrap_or(false);
    #[cfg(mobile)]
    let on = {
        let _ = window;
        false
    };
    on
}

/// The sign-in window (C.2): the shell's own page, one at a time, centred over the app. async: a window built from a
/// sync command deadlocks WebView2 on Windows (blank window). Tablets: the bridge shows the same page as a view.
#[tauri::command(async)]
fn cc_account_window(app: AppHandle) -> Result<(), String> {
    if !app.state::<account::Account>().enabled {
        return Err("off: this build has no sign-in".into());
    }
    account_window(&app)
}

#[cfg(desktop)]
fn account_window(app: &AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("account") {
        let _ = w.unminimize();
        return w.set_focus().map_err(|e| e.to_string());
    }
    let mut b = WebviewWindowBuilder::new(app, "account", WebviewUrl::CustomProtocol(protocol::start_url(".shell/account.html")));
    if let Some(d) = portable::webview_dir() {
        b = b.data_directory(d);
    }
    let mut b = b
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
#[cfg(mobile)]
fn account_window(_app: &AppHandle) -> Result<(), String> {
    Err("not_supported: the bridge shows the sign-in page as a view".into())
}

/// Closes the calling window — the shell's sign-in window, or (suite mode) an app window closing itself, e.g. its
/// "Close window" — since scripts may not close windows they did not open. Never the hub or another window. Tablets:
/// views close in the bridge; the one window never closes this way.
#[tauri::command]
fn cc_close_window(window: tauri::WebviewWindow) {
    if window.label() == "account" || window.label().starts_with("app-") {
        let _ = window.close();
    }
}

/// A link that leaves Studio (bridge.js catches it: on tablets from a view too; on computers since 2026-11-01, when
/// macOS's What's new links did nothing), opened in the person's browser. Only http(s) and mailto, never Studio's own pages.
#[tauri::command]
fn cc_open_url(app: AppHandle, url: String) -> Result<(), String> {
    let u = tauri::Url::parse(&url).map_err(|_| "error: not a link".to_string())?;
    if !matches!(u.scheme(), "http" | "https" | "mailto") || protocol::is_local(&u) {
        return Err("not_allowed: not a link out".into());
    }
    app.opener().open_url(u.as_str(), None::<&str>).map_err(|e| e.to_string())
}
