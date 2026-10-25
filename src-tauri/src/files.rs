// SPDX-License-Identifier: Apache-2.0
//! The file half of `window.cadcraftDesktop` (host.js's desktop bridge, docs/0.4-PART2.md W.2 + W2.15): native open,
//! save and folder dialogs, reading and writing (atomic) and listing folders.
//!
//! **Grants.** The page reaches only files the person chose: picked in a dialog, saved through a dialog, opened by
//! double-click / "Open with", or inside a folder they picked. Anything else is refused with `not_allowed`, so a bug
//! or a hostile script in the page cannot read the disk. Grants are kept (most recent first, capped) in the app's
//! data folder, so the recent-files list still reopens after a restart.
//!
//! **Tablets (TB.1).** Files there are documents behind a content URI (Android) or a security-scoped bookmark (iPad),
//! never paths (mobile.rs). The page gets a path of our own for each: `cc-doc:/<key>/<name>` for a document,
//! `cc-doc:/<key>/<folder name>/<path below it>` inside a chosen folder, so the apps' file names and extensions read as
//! usual (host.js `baseName`, `dirName`). The grant list keeps key → uri; an unknown key is `not_allowed` like any path
//! the person never chose.

// Tablets: launch arguments, second starts and the desktop's dialogs don't exist there (TB.1).
#![cfg_attr(mobile, allow(dead_code))]

use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::{AppHandle, Manager, Runtime, State, WebviewWindow};
#[cfg(desktop)]
use tauri_plugin_dialog::{DialogExt, FileDialogBuilder};

const MAX_FILES: usize = 1000;
const MAX_DIRS: usize = 100;
/// Android keeps at most 512 lasting document permissions per app (128 before Android 11); we stay well under it and
/// give the oldest back.
#[cfg_attr(desktop, allow(dead_code))]
const MAX_DOCS: usize = 400;
/// The page's own name for a tablet document (see above).
pub const DOC_PREFIX: &str = "cc-doc:/";

#[derive(Default, Serialize, Deserialize)]
struct GrantList {
    files: Vec<String>,
    dirs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    docs: Vec<DocGrant>,
}

/// A tablet document or folder the person chose: our key, the system's uri, its name when chosen.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DocGrant {
    pub key: String,
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub dir: bool,
}

/// `cc-doc:/<key>/<name>[/<below>…]` → (key, the parts after the key). Plain names only: no "", ".", "..", "\\" or ":".
pub fn parse_doc_path(path: &str) -> Option<(String, Vec<String>)> {
    let rest = path.strip_prefix(DOC_PREFIX)?;
    let mut parts = rest.split('/');
    let key = parts.next()?.to_string();
    if key.is_empty() || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let tail: Vec<String> = parts.map(String::from).collect();
    if tail.is_empty() || tail.iter().any(|p| p.is_empty() || p == "." || p == ".." || p.contains(['\\', '\0'])) {
        return None;
    }
    Some((key, tail))
}

/// The page's path for a granted document: the name made safe for one path part.
#[cfg_attr(desktop, allow(dead_code))]
pub fn doc_path(key: &str, name: &str) -> String {
    let n: String = name.chars().map(|c| if matches!(c, '/' | '\\' | '\0') { '_' } else { c }).collect();
    let n = if n.is_empty() || n == "." || n == ".." { "file".to_string() } else { n };
    format!("{DOC_PREFIX}{key}/{n}")
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

    /// SD.8: add another install's grants.json (a 0.4 per-app install Studio replaces), after this one's own (so this
    /// install's recent files stay first), within the caps. → how many were added.
    pub fn merge_from(&self, other: &Path) -> usize {
        let Some(old) = std::fs::read(other).ok().and_then(|b| serde_json::from_slice::<GrantList>(&b).ok()) else { return 0 };
        let mut l = self.list.lock().unwrap();
        let mut n = 0;
        for f in old.files {
            if l.files.len() < MAX_FILES && !l.files.contains(&f) && norm(Path::new(&f)).as_deref() == Some(f.as_str()) {
                l.files.push(f);
                n += 1;
            }
        }
        for d in old.dirs {
            if l.dirs.len() < MAX_DIRS && !l.dirs.contains(&d) && norm(Path::new(&d)).as_deref() == Some(d.as_str()) {
                l.dirs.push(d);
                n += 1;
            }
        }
        if n > 0 {
            self.save(&l);
        }
        n
    }

    /// Tablets: grant a document (or a folder, `dir`) the person chose → (its page path, uris given back to make room).
    /// The same uri keeps its key, so a file opened again is the same recent file.
    #[cfg_attr(desktop, allow(dead_code))]
    pub fn grant_doc(&self, uri: &str, name: &str, dir: bool) -> (String, Vec<String>) {
        let mut l = self.list.lock().unwrap();
        let key = match l.docs.iter().position(|d| d.uri == uri) {
            Some(i) => l.docs.remove(i).key,
            None => loop {
                let mut b = [0u8; 8];
                let _ = getrandom::fill(&mut b);
                let k: String = b.iter().map(|x| format!("{x:02x}")).collect();
                if !l.docs.iter().any(|d| d.key == k) {
                    break k;
                }
            },
        };
        let path = doc_path(&key, name);
        l.docs.insert(0, DocGrant { key, uri: uri.to_string(), name: name.to_string(), dir });
        let evicted: Vec<String> = if l.docs.len() > MAX_DOCS { l.docs.split_off(MAX_DOCS).into_iter().map(|d| d.uri).collect() } else { vec![] };
        self.save(&l);
        (path, evicted)
    }

    /// Tablets: the grant behind a page path, and the path below it inside a chosen folder (empty for the document or
    /// the folder itself). A document's path may carry another name (renamed since); a folder's parts are checked.
    pub fn doc_for(&self, path: &str) -> Option<(DocGrant, Vec<String>)> {
        let (key, tail) = parse_doc_path(path)?;
        let l = self.list.lock().unwrap();
        let g = l.docs.iter().find(|d| d.key == key)?.clone();
        let below: Vec<String> = tail[1..].to_vec();
        if !g.dir && !below.is_empty() {
            return None;
        }
        Some((g, below))
    }

    pub fn allows(&self, p: &Path) -> bool {
        if let Some(s) = p.to_str().filter(|s| s.starts_with(DOC_PREFIX)) {
            return self.doc_for(s).is_some();
        }
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

#[cfg(desktop)]
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

#[cfg(desktop)]
fn into_paths(v: Vec<tauri_plugin_dialog::FilePath>) -> Vec<PathBuf> {
    v.into_iter().filter_map(|f| f.into_path().ok()).collect()
}

/// Tablets: a blocking plugin call (a picker waits for the person) off the async runtime's threads.
#[cfg(mobile)]
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

/// Tablets: grant what the picker returned → page paths; uris pushed out of the list get their permission back.
#[cfg(mobile)]
fn grant_docs(grants: &Grants, docs: &[crate::mobile::Doc]) -> Vec<String> {
    docs.iter()
        .map(|d| {
            let (path, evicted) = grants.grant_doc(&d.uri, &d.name, d.dir);
            evicted.iter().for_each(|u| crate::mobile::release(u));
            path
        })
        .collect()
}

/// Tablets: the system's uri for a page path inside the grants (a chosen folder's file found by its path; `create`
/// makes it when saving).
#[cfg(mobile)]
fn doc_uri(grants: &Grants, path: &str, create: bool) -> Result<String, String> {
    let (g, below) = grants.doc_for(path).ok_or("not_allowed: the app can reach only files you opened or saved, or folders you chose")?;
    if below.is_empty() {
        return Ok(g.uri);
    }
    crate::mobile::find(&g.uri, &below, create)?.map(|d| d.uri).ok_or_else(|| format!("not_found: {path}"))
}

#[cfg(mobile)]
#[tauri::command]
pub async fn cc_open_dialog(grants: State<'_, Grants>, opts: DialogOpts) -> Result<Option<Vec<String>>, String> {
    let multiple = opts.multiple;
    let docs = blocking(move || crate::mobile::open_documents(multiple)).await?;
    Ok((!docs.is_empty()).then(|| grant_docs(&grants, &docs)))
}

#[cfg(mobile)]
#[tauri::command]
pub async fn cc_save_dialog(grants: State<'_, Grants>, opts: DialogOpts) -> Result<Option<String>, String> {
    let name = opts
        .default_path
        .as_deref()
        .and_then(|d| d.rsplit(['/', '\\']).next())
        .filter(|n| !n.is_empty())
        .unwrap_or("Untitled")
        .to_string();
    let doc = blocking(move || crate::mobile::create_document(&name, "application/octet-stream")).await?;
    Ok(doc.map(|d| grant_docs(&grants, &[d]).remove(0)))
}

#[cfg(mobile)]
#[tauri::command]
pub async fn cc_open_folder_dialog(grants: State<'_, Grants>, opts: DialogOpts) -> Result<Option<String>, String> {
    let _ = opts;
    let tree = blocking(crate::mobile::open_tree).await?;
    Ok(tree.map(|mut d| {
        d.dir = true;
        grant_docs(&grants, &[d]).remove(0)
    }))
}

#[cfg(desktop)]
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

#[cfg(desktop)]
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

#[cfg(desktop)]
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
    #[cfg(mobile)]
    {
        if path.starts_with(DOC_PREFIX) {
            let uri = doc_uri(&grants, &path, false)?;
            let bytes = blocking(move || {
                use std::io::Read;
                let mut v = Vec::new();
                crate::mobile::open(&uri, "r")?.read_to_end(&mut v).map_err(|e| io_err(&e))?;
                Ok(v)
            })
            .await?;
            return Ok(Response::new(bytes));
        }
    }
    let p = check(&grants, &path)?;
    let bytes = tauri::async_runtime::spawn_blocking(move || std::fs::read(p)).await.map_err(|e| e.to_string())?;
    bytes.map(Response::new).map_err(|e| io_err(&e))
}

/// The bytes a page sent: raw on the desktop; on Android Tauri's IPC carries JSON only (its WebView can't read a request
/// body), so the bridge sends `{b64}` there (bridge.js); a plain array of byte values is taken too. Anything else: None.
pub fn body_bytes(body: &InvokeBody) -> Option<Vec<u8>> {
    use base64::Engine;
    match body {
        InvokeBody::Raw(b) => Some(b.clone()),
        InvokeBody::Json(serde_json::Value::Null) => Some(Vec::new()),
        InvokeBody::Json(serde_json::Value::Object(o)) => o.get("b64")?.as_str().and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok()),
        InvokeBody::Json(serde_json::Value::Array(a)) => a.iter().map(|v| v.as_u64().filter(|n| *n < 256).map(|n| n as u8)).collect(),
        _ => None,
    }
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
    let bytes = &body_bytes(request.body()).ok_or("expected bytes")?;
    // Tablets: a document is rewritten in place (truncated, then written): a provider's document can't be replaced by
    // rename the way a file on disk is.
    #[cfg(mobile)]
    {
        if path.starts_with(DOC_PREFIX) {
            let uri = doc_uri(&grants, &path, true)?;
            let bytes = bytes.clone();
            return blocking(move || {
                use std::io::Write;
                let mut f = crate::mobile::open(&uri, "wt")?;
                f.write_all(&bytes).and_then(|_| f.sync_all()).map_err(|e| io_err(&e))
            })
            .await;
        }
    }
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
pub async fn cc_stat(grants: State<'_, Grants>, path: String) -> Result<Option<Stat>, String> {
    #[cfg(mobile)]
    {
        if path.starts_with(DOC_PREFIX) {
            let uri = match doc_uri(&grants, &path, false) {
                Ok(u) => u,
                Err(e) if e.starts_with("not_found") => return Ok(None),
                Err(e) => return Err(e),
            };
            let doc = blocking(move || crate::mobile::stat(&uri)).await?;
            return Ok(doc.filter(|d| !d.dir).map(|d| Stat { size: d.size.unwrap_or(0), mtime: d.mtime }));
        }
    }
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
pub async fn cc_list_dir(grants: State<'_, Grants>, path: String) -> Result<Vec<DirEntry>, String> {
    #[cfg(mobile)]
    {
        if path.starts_with(DOC_PREFIX) {
            let (g, below) = grants.doc_for(&path).ok_or("not_allowed: the app can reach only folders you chose")?;
            if !g.dir {
                return Err("not_found: not a folder".into());
            }
            let docs = blocking(move || crate::mobile::children(&g.uri, &below)).await?;
            let mut out: Vec<DirEntry> = docs.into_iter().map(|d| DirEntry { name: d.name, dir: d.dir }).collect();
            out.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
            return Ok(out);
        }
    }
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

pub(crate) fn decode_component(s: &str) -> Option<String> {
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

/// Command-line arguments → existing files (relative ones against `cwd`); flags are skipped. A `file://` URI (what a
/// Linux desktop passes for `%U`, which Studio's entry uses so it can also take `ccstudio://` links) becomes its path.
pub fn files_from_args<I: IntoIterator<Item = String>>(args: I, cwd: &Path) -> Vec<PathBuf> {
    args.into_iter()
        .filter(|a| !a.starts_with('-'))
        .filter_map(|a| if a.starts_with("file://") { path_from_file_uri(&a) } else { Some(PathBuf::from(&a)) })
        .map(|p| if p.is_absolute() { p } else { cwd.join(p) })
        .filter(|p| p.is_file() && norm(p).is_some())
        .collect()
}

/// `file:///home/me/a%20b.dwg` (or `file://localhost/…`) → `/home/me/a b.dwg`; other hosts and bad escapes → None.
pub fn path_from_file_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let (b, mut out, mut i) = (rest.as_bytes(), Vec::with_capacity(rest.len()), 0);
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
    }
    #[cfg(not(unix))]
    {
        // file:///C:/x → C:/x
        let s = String::from_utf8(out).ok()?;
        Some(PathBuf::from(s.strip_prefix('/').filter(|t| t.as_bytes().get(1) == Some(&b':')).unwrap_or(&s)))
    }
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

    #[cfg(unix)]
    #[test]
    fn file_uris_become_paths() {
        assert_eq!(path_from_file_uri("file:///home/me/a%20b.dwg"), Some(PathBuf::from("/home/me/a b.dwg")));
        assert_eq!(path_from_file_uri("file://localhost/tmp/x.dxf"), Some(PathBuf::from("/tmp/x.dxf")));
        assert_eq!(path_from_file_uri("file://server/share/x.dwg"), None);
        assert_eq!(path_from_file_uri("file:///bad%zz"), None);
        let d = tmp("uri");
        let f = d.join("plan one.dwg");
        std::fs::write(&f, b"x").unwrap();
        let uri = format!("file://{}", f.to_string_lossy().replace(' ', "%20"));
        assert_eq!(files_from_args(vec![uri, "ccstudio://join/AB3K7MNP#abcdefghijklmnopqrstuv".into()], &d), vec![f]);
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
    fn tablet_documents_by_key() {
        let d = tmp("docs");
        let store = d.join("grants.json");
        let g = Grants::load(Some(store.clone()));
        let (p, evicted) = g.grant_doc("content://p/document/1", "Plan A.dwg", false);
        assert!(evicted.is_empty());
        assert!(p.starts_with(DOC_PREFIX) && p.ends_with("/Plan A.dwg"), "{p}");
        assert_eq!(g.grant_doc("content://p/document/1", "Plan A.dwg", false).0, p, "the same document keeps its key");
        assert!(g.allows(Path::new(&p)));
        assert_eq!(g.doc_for(&p).unwrap().0.uri, "content://p/document/1");
        assert!(g.doc_for(&format!("{p}/x.dwg")).is_none(), "nothing below a document");
        let (f, _) = g.grant_doc("content://p/tree/7", "Job", true);
        let (tree, below) = g.doc_for(&format!("{f}/sheets/A-101.dwg")).unwrap();
        assert_eq!((tree.uri.as_str(), below), ("content://p/tree/7", vec!["sheets".to_string(), "A-101.dwg".to_string()]));
        assert!(g.doc_for(&format!("{f}/../x.dwg")).is_none() && g.doc_for(&format!("{f}//x.dwg")).is_none());
        assert!(!g.allows(Path::new("cc-doc:/0123456789abcdef/x.dwg")), "an unknown key is not allowed");
        assert!(parse_doc_path("cc-doc:/zz/x").is_none() && parse_doc_path("cc-doc:/ab").is_none());
        assert_eq!(doc_path("ab", "a/b.dwg"), "cc-doc:/ab/a_b.dwg");
        assert_eq!(doc_path("ab", ".."), "cc-doc:/ab/file");
        assert!(Grants::load(Some(store.clone())).allows(Path::new(&p)), "documents survive a restart");
        let mut out = vec![];
        for i in 0..MAX_DOCS {
            out.extend(g.grant_doc(&format!("content://p/document/n{i}"), "n.dwg", false).1);
        }
        assert_eq!(out, vec!["content://p/document/1".to_string(), "content://p/tree/7".to_string()], "the oldest are given back");
        assert!(!g.allows(Path::new(&p)));
        // A desktop list written before tablets existed still loads, and a desktop list never gains a docs field.
        std::fs::write(&store, br#"{"files":["/a/b.dwg"],"dirs":[]}"#).unwrap();
        let old = Grants::load(Some(store.clone()));
        old.grant_file(Path::new("/a/c.dwg"));
        assert!(!std::fs::read_to_string(&store).unwrap().contains("docs"));
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn bodies_raw_base64_or_array() {
        use serde_json::json;
        assert_eq!(body_bytes(&InvokeBody::Raw(vec![1, 2])), Some(vec![1, 2]));
        assert_eq!(body_bytes(&InvokeBody::Json(json!({ "b64": "AQL/" }))), Some(vec![1, 2, 255]));
        assert_eq!(body_bytes(&InvokeBody::Json(json!([1, 2, 255]))), Some(vec![1, 2, 255]));
        assert_eq!(body_bytes(&InvokeBody::Json(json!(null))), Some(vec![]));
        assert_eq!(body_bytes(&InvokeBody::Json(json!([1, 256]))), None);
        assert_eq!(body_bytes(&InvokeBody::Json(json!({ "b64": "not base64!" }))), None);
        assert_eq!(body_bytes(&InvokeBody::Json(json!("text"))), None);
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
