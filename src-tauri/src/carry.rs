// SPDX-License-Identifier: Apache-2.0
//! SD.8 (docs/0.5-PLAN.md): Studio replaces the separate 0.4 apps, and brings their data along. Each per-app install kept
//! its recent files, autosave/recovery copies and settings in a web view profile of its own (`com.getcadcraft.<slug>`
//! beside Studio's), which Studio's pages can't see. At Studio's first start:
//!
//! 1. the main window opens on the shell's own page `.shell/carry.html` (Studio's profile) instead of the hub;
//! 2. for each old profile not carried yet (at most 3 tries, `carried.json`), the shell copies it, caches left out (the
//!    original is never touched), and opens a hidden window on the copy, same origin, which reads its localStorage and
//!    IndexedDB and hands them over (`cc_carry_put`);
//! 3. the carry page writes them into Studio's profile without overwriting anything Studio already has, the old
//!    `grants.json` is merged (recent files still reopen), and `cc_carry_done` records the result;
//! 4. the page goes on to the hub (whatever is not done after ~60 s waits for the next start).
//! Studio builds only. The page's own logic (what is copied, never overwritten) is in bridge/carry.html.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, Runtime, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// Never copied: caches and lock files of WebKitGTK and WebView2 (Chromium).
const SKIP: &[&str] = &[
    "WebKitCache", "CacheStorage", "mediakeys", "Cache", "Code Cache", "GPUCache", "GrShaderCache", "ShaderCache",
    "GraphiteDawnCache", "DawnCache", "DawnGraphiteCache", "DawnWebGPUCache", "Crashpad", "Service Worker",
    "component_crx_cache", "BrowserMetrics", "lockfile", "SingletonLock", "LOCK",
];
/// A profile bigger than this (after skipping caches) is left behind and said so.
const MAX_BYTES: u64 = 2 << 30;
const MAX_TRIES: u32 = 3;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Record {
    pub done: bool,
    pub tries: u32,
    #[serde(default)]
    pub result: serde_json::Value,
}

pub struct Job {
    pub slug: String,
    /// The old app's web view profile (Windows: %LOCALAPPDATA%\com.getcadcraft.<slug>; Linux ~/.local/share/…).
    pub profile: PathBuf,
    /// Its grants.json (Windows: %APPDATA%\com.getcadcraft.<slug>\grants.json).
    pub grants: PathBuf,
}

pub struct Carry {
    pub on: bool,
    jobs: Mutex<Vec<Job>>,
    state_file: Option<PathBuf>,
    state: Mutex<HashMap<String, Record>>,
    work: Option<PathBuf>,
    data: Mutex<HashMap<String, String>>,
}

impl Carry {
    /// What to carry, found once at start: Studio builds only, old profiles beside Studio's own, not done, under 3 tries.
    pub fn new<R: Runtime>(app: &AppHandle<R>, suite: bool) -> Self {
        let own = app.config().identifier.clone();
        let local = app.path().app_local_data_dir().ok();
        let data = app.path().app_data_dir().ok();
        let state_file = data.as_ref().map(|d| d.join("carried.json"));
        let state: HashMap<String, Record> =
            state_file.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let mut jobs = Vec::new();
        if suite {
            if let (Some(local), Some(data)) = (&local, &data) {
                for slug in crate::links::APPS {
                    let id = format!("com.getcadcraft.{slug}");
                    if id == own {
                        continue;
                    }
                    let rec = state.get(slug).cloned().unwrap_or_default();
                    if rec.done || rec.tries >= MAX_TRIES {
                        continue;
                    }
                    let (Some(lp), Some(dp)) = (local.parent(), data.parent()) else { continue };
                    let profile = lp.join(&id);
                    if profile.is_dir() {
                        jobs.push(Job { slug: slug.to_string(), profile, grants: dp.join(&id).join("grants.json") });
                    }
                }
            }
        }
        Carry {
            on: !jobs.is_empty(),
            jobs: Mutex::new(jobs),
            state_file,
            state: Mutex::new(state),
            work: local.map(|l| l.join("carry")),
            data: Mutex::new(HashMap::new()),
        }
    }

    fn save(&self, st: &HashMap<String, Record>) {
        if let Some(p) = &self.state_file {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            if let Ok(b) = serde_json::to_vec_pretty(st) {
                let _ = std::fs::write(p, b);
            }
        }
    }
}

/// Copies `from` into `to`, leaving out SKIP names; refuses past MAX_BYTES. Returns the bytes copied.
pub fn copy_profile(from: &Path, to: &Path) -> std::io::Result<u64> {
    fn walk(from: &Path, to: &Path, total: &mut u64) -> std::io::Result<()> {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            let name = e.file_name();
            if SKIP.iter().any(|s| name.to_string_lossy() == *s) {
                continue;
            }
            let ft = e.file_type()?;
            if ft.is_dir() {
                walk(&e.path(), &to.join(&name), total)?;
            } else if ft.is_file() {
                *total += e.metadata()?.len();
                if *total > MAX_BYTES {
                    return Err(std::io::Error::other("too_big"));
                }
                std::fs::copy(e.path(), to.join(&name))?;
            }
        }
        Ok(())
    }
    let _ = std::fs::remove_dir_all(to);
    let mut total = 0;
    walk(from, to, &mut total)?;
    Ok(total)
}

/// The carry page asks what to carry: each job counts a try, its profile is copied and read in a hidden window on the
/// copy. → the slugs it started. async: windows built from a sync command deadlock WebView2.
#[tauri::command(async)]
pub fn cc_carry_plan<R: Runtime>(app: AppHandle<R>, window: WebviewWindow<R>, carry: State<'_, Carry>) -> Result<Vec<String>, String> {
    if window.label() != "main" {
        return Err("not_allowed".into());
    }
    let jobs = std::mem::take(&mut *carry.jobs.lock().unwrap());
    let mut started = Vec::new();
    for j in jobs {
        {
            let mut st = carry.state.lock().unwrap();
            st.entry(j.slug.clone()).or_default().tries += 1;
            carry.save(&st);
        }
        let Some(work) = &carry.work else { continue };
        let copy = work.join(&j.slug);
        match copy_profile(&j.profile, &copy) {
            Ok(_) => {}
            Err(e) => {
                let mut st = carry.state.lock().unwrap();
                st.entry(j.slug.clone()).or_default().result = serde_json::json!({ "error": e.to_string() });
                carry.save(&st);
                let _ = std::fs::remove_dir_all(&copy);
                continue;
            }
        }
        // Remember where its grants are for cc_carry_done.
        carry.data.lock().unwrap().insert(format!("grants:{}", j.slug), j.grants.to_string_lossy().into_owned());
        let url = WebviewUrl::CustomProtocol(crate::protocol::start_url(&format!(".shell/carry.html?read={}", j.slug)));
        let built = WebviewWindowBuilder::new(&app, format!("carry-{}", j.slug), url)
            .title("CadCraft Studio")
            .visible(false)
            .data_directory(copy)
            .build();
        if built.is_ok() {
            started.push(j.slug);
        }
    }
    Ok(started)
}

/// A hidden reader window hands over what its profile holds (JSON from carry.html), then goes away.
#[tauri::command]
pub fn cc_carry_put<R: Runtime>(app: AppHandle<R>, window: WebviewWindow<R>, carry: State<'_, Carry>, slug: String, data: String) -> Result<(), String> {
    if window.label() != format!("carry-{slug}") {
        return Err("not_allowed".into());
    }
    carry.data.lock().unwrap().insert(slug.clone(), data);
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.eval(&format!("window.__ccCarry&&window.__ccCarry({})", serde_json::to_string(&slug).unwrap()));
    }
    let w = window.clone();
    std::thread::spawn(move || {
        let h = w.app_handle().clone();
        let _ = h.run_on_main_thread(move || {
            let _ = w.destroy();
        });
    });
    Ok(())
}

/// The carry page takes what a reader handed over (once).
#[tauri::command]
pub fn cc_carry_take<R: Runtime>(window: WebviewWindow<R>, carry: State<'_, Carry>, slug: String) -> Result<Option<String>, String> {
    if window.label() != "main" {
        return Err("not_allowed".into());
    }
    Ok(carry.data.lock().unwrap().remove(&slug))
}

/// The carry page wrote one app's data (ok) or gave up on it: record it, merge the old grants, drop the copy.
#[tauri::command]
pub fn cc_carry_done<R: Runtime>(
    app: AppHandle<R>,
    window: WebviewWindow<R>,
    carry: State<'_, Carry>,
    slug: String,
    ok: bool,
    result: serde_json::Value,
) -> Result<(), String> {
    if window.label() != "main" || !crate::links::APPS.contains(&slug.as_str()) {
        return Err("not_allowed".into());
    }
    let grants = carry.data.lock().unwrap().remove(&format!("grants:{slug}"));
    let mut merged = 0;
    if ok {
        if let Some(g) = grants {
            merged = app.state::<crate::files::Grants>().merge_from(Path::new(&g));
        }
    }
    {
        let mut st = carry.state.lock().unwrap();
        let r = st.entry(slug.clone()).or_default();
        r.done = ok;
        r.result = serde_json::json!({ "page": result, "grants": merged });
        carry.save(&st);
    }
    if let Some(w) = &carry.work {
        let _ = std::fs::remove_dir_all(w.join(&slug));
    }
    if let Some(r) = app.get_webview_window(&format!("carry-{slug}")) {
        let _ = r.destroy();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copies_without_caches() {
        let d = std::env::temp_dir().join(format!("cc-carry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let from = d.join("old");
        std::fs::create_dir_all(from.join("localstorage")).unwrap();
        std::fs::create_dir_all(from.join("WebKitCache/Version 17")).unwrap();
        std::fs::create_dir_all(from.join("EBWebView/Default/Code Cache")).unwrap();
        std::fs::write(from.join("localstorage/x.localstorage"), b"abc").unwrap();
        std::fs::write(from.join("WebKitCache/Version 17/big"), b"cache").unwrap();
        std::fs::write(from.join("EBWebView/Default/Preferences"), b"{}").unwrap();
        std::fs::write(from.join("EBWebView/Default/Code Cache/js"), b"cache").unwrap();
        std::fs::write(from.join("EBWebView/lockfile"), b"").unwrap();
        let to = d.join("copy");
        assert_eq!(copy_profile(&from, &to).unwrap(), 5);
        assert!(to.join("localstorage/x.localstorage").is_file());
        assert!(to.join("EBWebView/Default/Preferences").is_file());
        assert!(!to.join("WebKitCache").exists() && !to.join("EBWebView/Default/Code Cache").exists() && !to.join("EBWebView/lockfile").exists());
        assert!(from.join("WebKitCache/Version 17/big").is_file(), "the original is untouched");
        let _ = std::fs::remove_dir_all(&d);
    }
}
