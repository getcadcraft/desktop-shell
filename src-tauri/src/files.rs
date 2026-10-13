// SPDX-License-Identifier: Apache-2.0
//! The file half of `window.cadcraftDesktop` (host.js's desktop bridge, docs/0.4-PART2.md W.2 + W2.15): native open,
//! save and folder dialogs, reading and writing (atomic) and listing folders.
//!
//! **Grants.** The page reaches only files the person chose: picked in a dialog, saved through a dialog, opened by
//! double-click / "Open with", or inside a folder they picked. Anything else is refused with `not_allowed`, so a bug
//! or a hostile script in the page cannot read the disk. Grants are kept (most recent first, capped) in the app's
//! data folder, so the recent-files list still reopens after a restart.

use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::{AppHandle, Manager, Runtime, State, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, FileDialogBuilder};

const MAX_FILES: usize = 1000;
const MAX_DIRS: usize = 100;

#[derive(Default, Serialize, Deserialize)]
struct GrantList {
    files: Vec<String>,
    dirs: Vec<String>,
}

pub struct Grants {
    list: Mutex<GrantList>,
    store: Option<PathBuf>,
}

/// Comparable form of a path: absolute and free of "." / ".." parts, else None. Case-folded on Windows and macOS.
pub fn norm(p: &Path) -> Option<String> {
    if !p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return None;
    }
    let s = p.to_string_lossy().trim_end_matches(['/', '\\']).to_string();
    if s.is_empty() {
        return None;
    }
    Some(if cfg!(any(windows, target_os = "macos")) { s.to_lowercase() } else { s })
}

impl Grants {
    pub fn load(store: Option<PathBuf>) -> Self {
        let list = store
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Grants { list: Mutex::new(list), store }
    }

    fn save(&self, list: &GrantList) {
        if let Some(p) = &self.store {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            if let Ok(b) = serde_json::to_vec(list) {
                let _ = std::fs::write(p, b);
            }
        }
    }

    pub fn grant_file(&self, p: &Path) {
        let Some(n) = norm(p) else { return };
        let mut l = self.list.lock().unwrap();
        l.files.retain(|x| x != &n);
        l.files.insert(0, n);
        l.files.truncate(MAX_FILES);
        self.save(&l);
    }

    pub fn grant_dir(&self, p: &Path) {
        let Some(n) = norm(p) else { return };
        let mut l = self.list.lock().unwrap();
        l.dirs.retain(|x| x != &n);
        l.dirs.insert(0, n);
        l.dirs.truncate(MAX_DIRS);
        self.save(&l);
    }

    pub fn allows(&self, p: &Path) -> bool {
        let Some(n) = norm(p) else { return false };
        let l = self.list.lock().unwrap();
        let sep = if cfg!(windows) { ['\\', '/'] } else { ['/', '/'] };
        l.files.iter().any(|f| f == &n)
            || l.dirs.iter().any(|d| n == *d || (n.starts_with(d.as_str()) && n[d.len()..].starts_with(sep)))
    }
}

/// A file the person opened. A sheet set (.ccsheets) lives with its drawings, named by paths relative to it, so its
/// folder comes with it — like a folder picked in a dialog. Only that type.
pub fn grant_opened(grants: &Grants, p: &Path) {
    grants.grant_file(p);
    let set = p.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("ccsheets"));
    if set {
        if let Some(dir) = p.parent() {
            grants.grant_dir(dir);
        }
    }
}

fn check(grants: &Grants, path: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(path);
    if grants.allows(&p) {
        Ok(p)
    } else {
        Err("not_allowed: the app can reach only files you opened or saved, or folders you chose".into())
    }
}

#[derive(Deserialize, Default)]
pub struct Filter {
    name: String,
    extensions: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DialogOpts {
    multiple: bool,
    filters: Vec<Filter>,
    title: Option<String>,
    default_path: Option<String>,
}

fn builder<R: Runtime>(win: &WebviewWindow<R>, o: &DialogOpts) -> FileDialogBuilder<R> {
    let mut b = win.dialog().file().set_parent(win);
    for f in &o.filters {
        let exts: Vec<&str> = f.extensions.iter().map(|e| e.trim_start_matches('.')).filter(|e| !e.is_empty()).collect();
        if !exts.is_empty() {
            b = b.add_filter(&f.name, &exts);
        }
    }
    if let Some(t) = &o.title {
        b = b.set_title(t);
    }
    b
}

/// Test seam, debug builds only (release builds always show the real dialog): with CC_TEST_ANSWERS naming a file, each
/// dialog takes that file's first line as the person's answer — a path, `a|b` for several, or empty for "cancelled".
#[cfg(debug_assertions)]
pub fn test_answer() -> Option<Option<Vec<PathBuf>>> {
    let file = std::env::var_os("CC_TEST_ANSWERS")?;
    let text = std::fs::read_to_string(&file).ok()?;
    let mut lines = text.lines();
    let first = lines.next()?.trim().to_string();
    let _ = std::fs::write(&file, lines.collect::<Vec<_>>().join("\n"));
    Some((!first.is_empty()).then(|| first.split('|').map(PathBuf::from).collect()))
}
#[cfg(not(debug_assertions))]
pub fn test_answer() -> Option<Option<Vec<PathBuf>>> {
    None
}

fn into_paths(v: Vec<tauri_plugin_dialog::FilePath>) -> Vec<PathBuf> {
    v.into_iter().filter_map(|f| f.into_path().ok()).collect()
}

#[tauri::command]
pub async fn cc_open_dialog<R: Runtime>(
    win: WebviewWindow<R>,
    grants: State<'_, Grants>,
    opts: DialogOpts,
) -> Result<Option<Vec<String>>, String> {
    let b = builder(&win, &opts);
    let multiple = opts.multiple;
    let picked = match test_answer() {
        Some(a) => a.map(|v| if multiple { v } else { v.into_iter().take(1).collect() }),
        None => tauri::async_runtime::spawn_blocking(move || {
            if multiple {
                b.blocking_pick_files().map(into_paths)
            } else {
                b.blocking_pick_file().map(|f| into_paths(vec![f]))
            }
        })
        .await
        .map_err(|e| e.to_string())?,
    };
    Ok(picked.map(|ps| {
        ps.iter().for_each(|p| grant_opened(&grants, p));
        ps.iter().map(|p| p.to_string_lossy().into_owned()).collect()
    }))
}

#[tauri::command]
pub async fn cc_save_dialog<R: Runtime>(
    win: WebviewWindow<R>,
    grants: State<'_, Grants>,
    opts: DialogOpts,
) -> Result<Option<String>, String> {
    let mut b = builder(&win, &opts);
    if let Some(d) = opts.default_path.as_deref().filter(|d| !d.is_empty()) {
        let p = Path::new(d);
        // A full path the person already reached keeps its folder; otherwise only the name is taken.
        if p.is_absolute() && grants.allows(p) {
            if let Some(dir) = p.parent() {
                b = b.set_directory(dir);
            }
        }
        if let Some(n) = p.file_name() {
            b = b.set_file_name(n.to_string_lossy());
        }
    }
    let picked = match test_answer() {
        Some(a) => a.and_then(|v| v.into_iter().next()),
        None => tauri::async_runtime::spawn_blocking(move || b.blocking_save_file())
            .await
            .map_err(|e| e.to_string())?
            .and_then(|f| f.into_path().ok()),
    };
    Ok(picked.map(|p| {
        grants.grant_file(&p);
        p.to_string_lossy().into_owned()
    }))
}

#[tauri::command]
pub async fn cc_open_folder_dialog<R: Runtime>(
    win: WebviewWindow<R>,
    grants: State<'_, Grants>,
    opts: DialogOpts,
) -> Result<Option<String>, String> {
    let b = builder(&win, &DialogOpts { filters: vec![], ..opts });
    let picked = match test_answer() {
        Some(a) => a.and_then(|v| v.into_iter().next()),
        None => tauri::async_runtime::spawn_blocking(move || b.blocking_pick_folder())
            .await
            .map_err(|e| e.to_string())?
            .and_then(|f| f.into_path().ok()),
    };
    Ok(picked.map(|p| {
        grants.grant_dir(&p);
        p.to_string_lossy().into_owned()
    }))
}

#[tauri::command]
pub async fn cc_read_file(grants: State<'_, Grants>, path: String) -> Result<Response, String> {
    let p = check(&grants, &path)?;
    let bytes = tauri::async_runtime::spawn_blocking(move || std::fs::read(p)).await.map_err(|e| e.to_string())?;
    bytes.map(Response::new).map_err(|e| io_err(&e))
}

/// Body = the bytes; header `x-cc-path` = the target path, percent-encoded (encodeURIComponent).
#[tauri::command]
pub async fn cc_write_file(grants: State<'_, Grants>, request: Request<'_>) -> Result<(), String> {
    let path = request
        .headers()
        .get("x-cc-path")
        .and_then(|v| v.to_str().ok())
        .and_then(decode_component)
        .ok_or("missing path")?;
    let InvokeBody::Raw(bytes) = request.body() else { return Err("expected raw bytes".into()) };
    let p = check(&grants, &path)?;
    let bytes = bytes.clone();
    tauri::async_runtime::spawn_blocking(move || write_atomic(&p, &bytes)).await.map_err(|e| e.to_string())?
}

/// Temp file beside the target, flushed to disk, then renamed over it (rename replaces on Windows too), so a crash
/// or a full disk never leaves a half-written drawing.
pub fn write_atomic(p: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let dir = p.parent().ok_or("no folder")?;
    let name = p.file_name().ok_or("no file name")?.to_string_lossy();
    let nanos = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let tmp = dir.join(format!(".{name}.cc-{}-{nanos}.tmp", std::process::id()));
    let r = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, p)
    })();
    if let Err(e) = r {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_err(&e));
    }
    Ok(())
}

#[derive(Serialize)]
pub struct Stat {
    size: u64,
    mtime: Option<f64>,
}

#[tauri::command]
pub fn cc_stat(grants: State<'_, Grants>, path: String) -> Result<Option<Stat>, String> {
    let p = check(&grants, &path)?;
    Ok(std::fs::metadata(p).ok().filter(|m| m.is_file()).map(|m| Stat {
        size: m.len(),
        mtime: m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as f64),
    }))
}

#[derive(Serialize)]
pub struct DirEntry {
    name: String,
    dir: bool,
}

#[tauri::command]
pub fn cc_list_dir(grants: State<'_, Grants>, path: String) -> Result<Vec<DirEntry>, String> {
    let p = check(&grants, &path)?;
    let mut out: Vec<DirEntry> = std::fs::read_dir(p)
        .map_err(|e| io_err(&e))?
        .filter_map(|e| e.ok())
        .map(|e| DirEntry { name: e.file_name().to_string_lossy().into_owned(), dir: e.file_type().map(|t| t.is_dir()).unwrap_or(false) })
        .collect();
    out.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

fn io_err(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => format!("not_found: {e}"),
        std::io::ErrorKind::PermissionDenied => format!("permission_denied: {e}"),
        _ => e.to_string(),
    }
}

fn decode_component(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

// ---- files the app was started with, and files opened while it runs ----------------------------------------------

#[derive(Default)]
pub struct Launch {
    pending: Mutex<Vec<String>>,
    drained: Mutex<bool>,
}

/// Command-line arguments → existing files (relative ones against `cwd`); flags are skipped.
pub fn files_from_args<I: IntoIterator<Item = String>>(args: I, cwd: &Path) -> Vec<PathBuf> {
    args.into_iter()
        .filter(|a| !a.starts_with('-'))
        .map(|a| {
            let p = PathBuf::from(&a);
            if p.is_absolute() { p } else { cwd.join(p) }
        })
        .filter(|p| p.is_file() && norm(p).is_some())
        .collect()
}

impl Launch {
    pub fn add<R: Runtime>(&self, app: &AppHandle<R>, files: Vec<PathBuf>) {
        if files.is_empty() {
            return;
        }
        let grants = app.state::<Grants>();
        files.iter().for_each(|p| grant_opened(&grants, p));
        let paths: Vec<String> = files.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        if *self.drained.lock().unwrap() {
            if let Some(w) = app.get_webview_window("main") {
                let js = format!("window.__ccDesktopOpen&&window.__ccDesktopOpen({})", serde_json::to_string(&paths).unwrap());
                let _ = w.eval(&js);
            }
        } else {
            self.pending.lock().unwrap().extend(paths);
        }
    }
}

#[tauri::command]
pub fn cc_launch_files<R: Runtime>(window: WebviewWindow<R>, suite: State<'_, crate::windows::Suite>, launch: State<'_, Launch>) -> Vec<String> {
    // Suite mode: an app window gets the files the hub opened it with; launch files and second starts go to the hub.
    if window.label() != "main" {
        return suite.take_launch(window.label()).unwrap_or_default();
    }
    *launch.drained.lock().unwrap() = true;
    std::mem::take(&mut *launch.pending.lock().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cc-files-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn grants_cover_chosen_files_and_folders_only() {
        let d = tmp("grants");
        let store = d.join("grants.json");
        let g = Grants::load(Some(store.clone()));
        let f = d.join("plan.dwg");
        let folder = d.join("set");
        assert!(!g.allows(&f));
        g.grant_file(&f);
        g.grant_dir(&folder);
        assert!(g.allows(&f));
        assert!(g.allows(&folder));
        assert!(g.allows(&folder.join("sheets/A-101.dwg")));
        assert!(!g.allows(&d.join("set-other/x.dwg")), "a sibling sharing the prefix is not inside");
        assert!(!g.allows(&folder.join("../plan2.dwg")), "no .. escapes");
        assert!(!g.allows(Path::new("relative.dwg")));
        assert!(!g.allows(&d.join("other.dwg")));
        let set = d.join("job/Job.ccsheets");
        grant_opened(&g, &set);
        assert!(g.allows(&d.join("job/sheets/A-101.dwg")), "a sheet set brings its folder");
        grant_opened(&g, &d.join("lone/plan.dwg"));
        assert!(!g.allows(&d.join("lone/other.dwg")), "any other file brings only itself");
        let again = Grants::load(Some(store));
        assert!(again.allows(&f) && again.allows(&folder.join("x")), "grants survive a restart");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let d = tmp("write");
        let f = d.join("a.dxf");
        std::fs::write(&f, b"old").unwrap();
        write_atomic(&f, b"new contents").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"new contents");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
        assert!(write_atomic(&d.join("missing/x.dxf"), b"x").is_err());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn launch_args() {
        let d = tmp("args");
        std::fs::write(d.join("a.dwg"), b"x").unwrap();
        let got = files_from_args(vec!["--flag".into(), "a.dwg".into(), "missing.dwg".into()], &d);
        assert_eq!(got, vec![d.join("a.dwg")]);
        assert_eq!(decode_component("C%3A%5CUsers%5Cme%5CPlan%20A.dwg").unwrap(), "C:\\Users\\me\\Plan A.dwg");
        std::fs::remove_dir_all(d).unwrap();
    }
}
