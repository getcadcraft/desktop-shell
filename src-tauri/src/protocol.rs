// SPDX-License-Identifier: Apache-2.0
//! The `cadcraft` protocol: serves the app's browser edition folder to the webview straight from disk — no web server,
//! no port. Windows/Android see it as http://cadcraft.localhost/, macOS/Linux as cadcraft://localhost/. The origin must
//! never change: localStorage, IndexedDB (recent files, autosave, settings) belong to it.

use std::path::{Component, Path, PathBuf};
use tauri::http::{header, Request, Response, StatusCode};

pub const SCHEME: &str = "cadcraft";

/// The page's own address on this platform.
pub fn start_url(page: &str) -> tauri::Url {
    #[cfg(any(windows, target_os = "android"))]
    let base = format!("http://{SCHEME}.localhost/");
    #[cfg(not(any(windows, target_os = "android")))]
    let base = format!("{SCHEME}://localhost/");
    tauri::Url::parse(&base).and_then(|u| u.join(page)).expect("start url")
}

/// True for the app's own pages and in-page data; anything else is a link out (opened in the person's browser).
pub fn is_local(url: &tauri::Url) -> bool {
    match url.scheme() {
        s if s == SCHEME => true,
        "about" | "blob" | "data" => true,
        "http" | "https" => url.host_str() == Some(&format!("{SCHEME}.localhost")),
        _ => false,
    }
}

pub struct Site {
    pub root: PathBuf,
    /// Extra response headers, e.g. COOP/COEP for an app whose manifest asks for a cross-origin isolated page.
    pub headers: Vec<(String, String)>,
}

fn mime(path: &Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" | "md" => "text/plain; charset=utf-8",
        "xml" | "xsd" => "application/xml",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "bcmap" | "pfb" | "icc" | "data" | "whl" | "tar" | "gz" => "application/octet-stream",
        _ => "application/octet-stream",
    }
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// URL path → a file under the root, or None. Only plain names: no "..", no drive letters, no backslashes.
pub fn resolve(root: &Path, url_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(url_path)?;
    let mut rel = decoded.trim_start_matches('/').to_string();
    if rel.is_empty() || rel.ends_with('/') {
        rel.push_str("index.html");
    }
    if rel.contains('\\') || rel.contains('\0') || rel.contains(':') {
        return None;
    }
    let mut p = root.to_path_buf();
    for c in Path::new(&rel).components() {
        match c {
            Component::Normal(s) => p.push(s),
            _ => return None,
        }
    }
    if p.is_dir() {
        p.push("index.html");
    }
    p.is_file().then_some(p)
}

/// The shell's own pages (the sign-in window), compiled into the executable. Same origin as the app, so the bridge works.
const SHELL_PAGES: &[(&str, &[u8])] = &[("/.shell/account.html", include_bytes!("../../bridge/account.html"))];

pub fn respond(site: &Site, req: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let mut b = Response::builder();
    for (k, v) in &site.headers {
        b = b.header(k.as_str(), v.as_str());
    }
    if let Some((_, page)) = SHELL_PAGES.iter().find(|(p, _)| *p == req.uri().path()) {
        return b.status(StatusCode::OK).header(header::CONTENT_TYPE, "text/html; charset=utf-8").body(page.to_vec()).unwrap();
    }
    let Some(path) = resolve(&site.root, req.uri().path()) else {
        return b
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(b"Not found".to_vec())
            .unwrap();
    };
    match std::fs::read(&path) {
        Ok(bytes) => b
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, mime(&path))
            .header("X-Content-Type-Options", "nosniff")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(if req.method() == tauri::http::Method::HEAD { Vec::new() } else { bytes })
            .unwrap(),
        Err(_) => b.status(StatusCode::INTERNAL_SERVER_ERROR).body(Vec::new()).unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_only_inside_the_root() {
        let dir = std::env::temp_dir().join(format!("cc-proto-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("assets/a b")).unwrap();
        std::fs::write(dir.join("index.html"), "x").unwrap();
        std::fs::write(dir.join("assets/a b/m.js"), "x").unwrap();
        assert_eq!(resolve(&dir, "/"), Some(dir.join("index.html")));
        assert_eq!(resolve(&dir, "/assets/a%20b/m.js"), Some(dir.join("assets/a b/m.js")));
        assert_eq!(resolve(&dir, "/assets/a b/"), None);
        assert_eq!(resolve(&dir, "/../index.html"), None);
        assert_eq!(resolve(&dir, "/assets/%2e%2e/index.html"), None);
        assert_eq!(resolve(&dir, "/assets%5c..%5cindex.html"), None);
        assert_eq!(resolve(&dir, "/C:/Windows/win.ini"), None);
        assert_eq!(resolve(&dir, "/nope.js"), None);
        assert_eq!(mime(Path::new("x.wasm")), "application/wasm");
        assert_eq!(mime(Path::new("x.MJS")), "text/javascript; charset=utf-8");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn local_urls() {
        assert!(is_local(&tauri::Url::parse("cadcraft://localhost/index.html").unwrap()));
        assert!(is_local(&tauri::Url::parse("http://cadcraft.localhost/x").unwrap()));
        assert!(is_local(&tauri::Url::parse("about:blank").unwrap()));
        assert!(!is_local(&tauri::Url::parse("https://getcadcraft.com/device").unwrap()));
        assert!(!is_local(&tauri::Url::parse("http://localhost:8080/").unwrap()));
        assert!(!is_local(&tauri::Url::parse("file:///etc/passwd").unwrap()));
    }
}
