// SPDX-License-Identifier: Apache-2.0
//! Suite mode (Studio, docs/0.4-PART2.md H.3/H.5/H.9; asked by `hub`): one install holds the hub and the eight apps on one
//! origin. The hub is the main window; each app opens in windows of its own (several per app = several documents).
//!
//!   cadcraftDesktop.windows.open({app, paths?, search?}) → {id}     a new window on apps/<app>/index.html<search>;
//!                                                                    `paths` (already reachable) become its launchFiles()
//!   windows.list() → [{id, app, title, focused}]   windows.focus(id)   windows.focusHub()   windows.onChange(cb)
//!
//! Only in suite mode (shell.json `suite: true`), and only the hub may open windows. Files opened at launch or by a second
//! start still go to the hub, which routes them by type. Each app's window place is kept (the last one closed sets the
//! next one's), on a screen that is still there. Windows talk to each other over BroadcastChannel on the one origin.

// Tablets (TB.1): one web view, so the bridge shows apps as views itself (bridge.js); only Suite (each view's own files
// go through the bridge) is used there.
#![cfg_attr(mobile, allow(dead_code, unused_imports))]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, Runtime, State, WebviewUrl, WebviewWindow, WindowEvent};

use crate::files::{self, Grants};
use crate::protocol;

#[derive(Default, Serialize, Deserialize, Clone, Copy)]
pub struct Place {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    maximized: bool,
}

pub struct Suite {
    pub on: bool,
    site_root: PathBuf,
    /// Files handed to a window by the hub: its own launchFiles(), each once.
    launch: Mutex<HashMap<String, Vec<String>>>,
    places: Mutex<HashMap<String, Place>>,
    places_file: Option<PathBuf>,
    next: AtomicU32,
}

impl Suite {
    pub fn new(on: bool, site_root: PathBuf, places_file: Option<PathBuf>) -> Self {
        let places = places_file.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Suite { on, site_root, launch: Mutex::new(HashMap::new()), places: Mutex::new(places), places_file, next: AtomicU32::new(1) }
    }

    /// The window's own files (for cc_launch_files), each handed over once.
    pub fn take_launch(&self, label: &str) -> Option<Vec<String>> {
        self.launch.lock().unwrap().remove(label)
    }

    fn remember(&self, app: &str, p: Place) {
        let mut m = self.places.lock().unwrap();
        m.insert(app.to_string(), p);
        if let Some(f) = &self.places_file {
            if let Some(d) = f.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(f, serde_json::to_vec(&*m).unwrap_or_default());
        }
    }
}

/// "app-cadcraft-3" → "cadcraft"; "main" → "studio".
pub fn app_of(label: &str) -> Option<String> {
    if label == "main" {
        return Some("studio".into());
    }
    let rest = label.strip_prefix("app-")?;
    let (app, n) = rest.rsplit_once('-')?;
    n.parse::<u32>().ok().map(|_| app.to_string())
}

fn valid_slug(s: &str) -> bool {
    !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// `?handoff=<uuid>`-style query strings only: printable ASCII, no fragment, no spaces, short.
fn valid_search(s: &str) -> bool {
    s.is_empty() || (s.starts_with('?') && s.len() <= 512 && s.bytes().all(|b| b.is_ascii_graphic() && b != b'#' && b != b'\\'))
}

#[cfg(desktop)]
fn notify<R: Runtime>(app: &AppHandle<R>, kind: &str, id: &str) {
    if let Some(hub) = app.get_webview_window("main") {
        let ev = serde_json::json!({ "type": kind, "id": id });
        let _ = hub.eval(&format!("window.__ccDesktopWindows&&window.__ccDesktopWindows({ev})"));
    }
}

#[cfg(desktop)]
fn place_of<R: Runtime>(w: &WebviewWindow<R>) -> Option<Place> {
    let maximized = w.is_maximized().unwrap_or(false);
    let (pos, size) = (w.outer_position().ok()?, w.inner_size().ok()?);
    let scale = w.scale_factor().unwrap_or(1.0);
    Some(Place { x: pos.x, y: pos.y, w: (size.width as f64 / scale) as u32, h: (size.height as f64 / scale) as u32, maximized })
}

#[cfg(desktop)]
#[derive(Deserialize)]
pub struct OpenArgs {
    app: String,
    #[serde(default)]
    paths: Vec<String>,
    #[serde(default)]
    search: String,
}

#[cfg(desktop)]
#[derive(Serialize)]
pub struct Opened {
    id: String,
}

#[cfg(desktop)]
// async: a window built from a sync command deadlocks WebView2 on Windows (blank window).
#[tauri::command(async)]
pub fn cc_windows_open<R: Runtime>(
    app: AppHandle<R>,
    caller: WebviewWindow<R>,
    suite: State<'_, Suite>,
    grants: State<'_, Grants>,
    args: OpenArgs,
) -> Result<Opened, String> {
    if !suite.on {
        return Err("off: not the suite".into());
    }
    if caller.label() != "main" {
        return Err("not_allowed: only the hub opens windows".into());
    }
    if !valid_slug(&args.app) || args.app == "studio" {
        return Err("unknown app".into());
    }
    let page = format!("apps/{}/index.html", args.app);
    if protocol::resolve(&suite.site_root, &format!("/{page}")).is_none() {
        return Err(format!("unknown app {}", args.app));
    }
    if !valid_search(&args.search) {
        return Err("bad search".into());
    }
    // The hub may hand over only files it can reach itself (picked, launched, or under a picked folder).
    for p in &args.paths {
        if !grants.allows(Path::new(p)) {
            return Err(format!("not_allowed: {p}"));
        }
    }
    let label = format!("app-{}-{}", args.app, suite.next.fetch_add(1, Ordering::Relaxed));
    if !args.paths.is_empty() {
        args.paths.iter().for_each(|p| files::grant_opened(&grants, Path::new(p)));
        suite.launch.lock().unwrap().insert(label.clone(), args.paths.clone());
    }
    let url = protocol::start_url(&format!("{page}{}", args.search));
    // A saved place counts only when a connected screen shows enough of it (checked before the window exists: right after
    // it is built, GTK has not placed it yet).
    let monitors = app.available_monitors().unwrap_or_default();
    let place = suite.places.lock().unwrap().get(&args.app).copied().filter(|p| {
        monitors.iter().any(|m| {
            let (mp, ms) = (m.position(), m.size());
            let x = (p.x + p.w as i32).min(mp.x + ms.width as i32) - p.x.max(mp.x);
            let y = (p.y + p.h as i32).min(mp.y + ms.height as i32) - p.y.max(mp.y);
            x >= 64 && y >= 32
        })
    });
    let b = crate::app_window(&app, &label, WebviewUrl::CustomProtocol(url), &args.app);
    let b = match place {
        Some(p) => b.inner_size(p.w.max(960) as f64, p.h.max(600) as f64).position(p.x as f64, p.y as f64).maximized(p.maximized),
        None => b.inner_size(1440.0, 900.0).maximized(true),
    };
    let w = b.build().map_err(|e| e.to_string())?;
    crate::allow_script_popups(&w);
    let (h, l, a) = (app.clone(), label.clone(), args.app.clone());
    let wc = w.clone();
    w.on_window_event(move |e| match e {
        // Kept in memory as it moves; written when the window closes.
        WindowEvent::Moved(_) | WindowEvent::Resized(_) => {
            if let Some(p) = place_of(&wc) {
                h.state::<Suite>().places.lock().unwrap().insert(a.clone(), p);
            }
        }
        WindowEvent::CloseRequested { .. } => {
            if let Some(p) = place_of(&wc) {
                h.state::<Suite>().remember(&a, p);
            }
        }
        WindowEvent::Destroyed => {
            let st = h.state::<Suite>();
            let last = st.places.lock().unwrap().get(&a).copied();
            if let Some(p) = last {
                st.remember(&a, p);
            }
            h.state::<Suite>().launch.lock().unwrap().remove(&l);
            notify(&h, "closed", &l);
        }
        WindowEvent::Focused(true) => notify(&h, "focus", &l),
        _ => {}
    });
    notify(&app, "opened", &label);
    Ok(Opened { id: label })
}

#[cfg(desktop)]
#[derive(Serialize)]
pub struct Listed {
    id: String,
    app: String,
    title: String,
    focused: bool,
}

#[cfg(desktop)]
#[tauri::command]
pub fn cc_windows_list<R: Runtime>(app: AppHandle<R>, suite: State<'_, Suite>) -> Result<Vec<Listed>, String> {
    if !suite.on {
        return Err("off: not the suite".into());
    }
    let mut out: Vec<Listed> = app
        .webview_windows()
        .into_iter()
        .filter_map(|(label, w)| {
            let app = app_of(&label)?;
            Some(Listed { title: w.title().unwrap_or_default(), focused: w.is_focused().unwrap_or(false), id: label, app })
        })
        .collect();
    out.sort_by(|a, b| (a.id != "main").cmp(&(b.id != "main")).then_with(|| a.id.cmp(&b.id)));
    Ok(out)
}

#[cfg(desktop)]
fn focus<R: Runtime>(w: &WebviewWindow<R>) -> Result<(), String> {
    let _ = w.unminimize();
    let _ = w.show();
    w.set_focus().map_err(|e| e.to_string())
}

#[cfg(desktop)]
#[tauri::command]
pub fn cc_windows_focus<R: Runtime>(app: AppHandle<R>, suite: State<'_, Suite>, id: String) -> Result<(), String> {
    if !suite.on {
        return Err("off: not the suite".into());
    }
    if app_of(&id).is_none() {
        return Err("unknown window".into());
    }
    focus(&app.get_webview_window(&id).ok_or("unknown window")?)
}

#[cfg(desktop)]
#[tauri::command]
pub fn cc_windows_focus_hub<R: Runtime>(app: AppHandle<R>, suite: State<'_, Suite>) -> Result<(), String> {
    if !suite.on {
        return Err("off: not the suite".into());
    }
    focus(&app.get_webview_window("main").ok_or("no hub")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_inputs() {
        assert_eq!(app_of("main").as_deref(), Some("studio"));
        assert_eq!(app_of("app-cadcraft-12").as_deref(), Some("cadcraft"));
        assert_eq!(app_of("popup-3"), None);
        assert_eq!(app_of("account"), None);
        assert!(valid_slug("partcraft") && !valid_slug("../x") && !valid_slug("Cad"));
        assert!(valid_search("") && valid_search("?handoff=6f1c2a4e-0b7d-4d7e-9a51-2f6a8c1d9e00"));
        assert!(!valid_search("handoff=x") && !valid_search("?a=1#frag") && !valid_search("?a b") && !valid_search("?\\x"));
    }
}
