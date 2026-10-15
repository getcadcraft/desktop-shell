// SPDX-License-Identifier: Apache-2.0
//! One port for both (spec § 8): `GET /live` with `Upgrade: websocket` is the session socket; any other `GET`/`HEAD`
//! serves a file from the browser edition Studio ships (`www` root), so a browser joiner's page and its socket share an
//! origin. Browsers block a page from another origin (an https site, or about:blank) from opening a socket to a LAN
//! address (Local Network Access), which is why the host serves the page.
//!
//! Deliberately small: HTTP/1.1, GET and HEAD only, one request per connection, no directory listings, no query
//! handling, no symlinks out of the root, files only.

use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;

pub const MAX_HEAD: usize = 16 * 1024;

pub struct Head {
    pub method: String,
    pub path: String,
    pub upgrade_ws: bool,
    /// Everything read so far (the head and possibly more), replayed to the WebSocket handshake.
    pub raw: Vec<u8>,
}

/// Reads the request head. None = not HTTP, too long, or the peer went away.
pub async fn read_head(stream: &mut TcpStream) -> Option<Head> {
    let mut raw = Vec::with_capacity(1024);
    let mut buf = [0u8; 2048];
    let end = loop {
        let n = tokio::time::timeout(std::time::Duration::from_secs(10), stream.read(&mut buf)).await.ok()?.ok()?;
        if n == 0 {
            return None;
        }
        raw.extend_from_slice(&buf[..n]);
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if raw.len() > MAX_HEAD {
            return None;
        }
    };
    let text = std::str::from_utf8(&raw[..end]).ok()?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_string();
    let target = first.next()?.to_string();
    let mut upgrade_ws = false;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().eq_ignore_ascii_case("upgrade") && v.trim().eq_ignore_ascii_case("websocket") {
                upgrade_ws = true;
            }
        }
    }
    let path = target.split(['?', '#']).next().unwrap_or("/").to_string();
    Some(Head { method, path, upgrade_ws, raw })
}

/// A stream that first replays bytes already read, then reads from the socket.
pub struct Replay {
    pre: Vec<u8>,
    pos: usize,
    inner: TcpStream,
}

impl Replay {
    pub fn new(pre: Vec<u8>, inner: TcpStream) -> Self {
        Replay { pre, pos: 0, inner }
    }
}

impl AsyncRead for Replay {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        if self.pos < self.pre.len() {
            let n = (self.pre.len() - self.pos).min(buf.remaining());
            let start = self.pos;
            buf.put_slice(&self.pre[start..start + n]);
            self.pos += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Replay {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, b: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, b)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// Maps a URL path to a file under `root`, refusing anything that leaves it. `/` and `/dir/` → `index.html`.
pub fn resolve(root: &Path, url_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(url_path)?;
    let mut p = root.to_path_buf();
    for comp in Path::new(decoded.trim_start_matches('/')).components() {
        match comp {
            Component::Normal(s) => {
                let s = s.to_str()?;
                if s.starts_with('.') {
                    return None; // no dotfiles
                }
                p.push(s);
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    if decoded.ends_with('/') || p.is_dir() {
        p.push("index.html");
    }
    // No symlinks out of the root.
    let real = p.canonicalize().ok()?;
    let real_root = root.canonicalize().ok()?;
    if !real.starts_with(&real_root) || !real.is_file() {
        return None;
    }
    Some(real)
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            let v = u8::from_str_radix(h, 16).ok()?;
            if v == 0 {
                return None;
            }
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

pub fn content_type(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
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
        "txt" | "md" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "data" | "bin" => "application/octet-stream",
        _ => "application/octet-stream",
    }
}

/// Answers one plain HTTP request and closes.
pub async fn serve_file(stream: &mut TcpStream, head: &Head, root: Option<&Path>) {
    let reply = |code: &str, ctype: &str, len: usize| {
        format!(
            "HTTP/1.1 {code}\r\nContent-Type: {ctype}\r\nContent-Length: {len}\r\nCache-Control: no-cache\r\n\
             X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n"
        )
    };
    if head.method != "GET" && head.method != "HEAD" {
        let _ = stream.write_all(reply("405 Method Not Allowed", "text/plain", 0).as_bytes()).await;
        return;
    }
    // A folder without its slash: redirect, so the page's relative links resolve inside it.
    if !head.path.ends_with('/') {
        if let (Some(r), Some(f)) = (root, root.and_then(|r| resolve(r, &head.path))) {
            if f.file_name().map(|n| n == "index.html").unwrap_or(false) && !head.path.ends_with("index.html") {
                let _ = r;
                let loc = format!("{}/", head.path);
                let msg = format!(
                    "HTTP/1.1 301 Moved Permanently\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = stream.write_all(msg.as_bytes()).await;
                return;
            }
        }
    }
    let file = root.and_then(|r| resolve(r, &head.path));
    let Some(file) = file else {
        let body = b"Not found";
        let _ = stream.write_all(reply("404 Not Found", "text/plain; charset=utf-8", body.len()).as_bytes()).await;
        if head.method == "GET" {
            let _ = stream.write_all(body).await;
        }
        return;
    };
    let Ok(mut bytes) = tokio::fs::read(&file).await else {
        let _ = stream.write_all(reply("500 Internal Server Error", "text/plain", 0).as_bytes()).await;
        return;
    };
    if content_type(&file).starts_with("text/html") {
        bytes = mark_page(bytes);
    }
    let _ = stream.write_all(reply("200 OK", content_type(&file), bytes.len()).as_bytes()).await;
    if head.method == "GET" {
        let _ = stream.write_all(&bytes).await;
    }
    let _ = stream.flush().await;
}

/// Pages the session server serves say so (`<meta name="cc-live">` right after `<head>`), so the app's host.live knows it
/// can join the session from this page (`host.live.available().page`).
pub const PAGE_MARK: &str = "<meta name=\"cc-live\" content=\"1\">";

pub fn mark_page(bytes: Vec<u8>) -> Vec<u8> {
    let lower: Vec<u8> = bytes.iter().take(64 * 1024).map(|b| b.to_ascii_lowercase()).collect();
    let Some(i) = lower.windows(5).position(|w| w == b"<head") else { return bytes };
    let Some(j) = lower[i..].iter().position(|b| *b == b'>') else { return bytes };
    let at = i + j + 1;
    let mut out = Vec::with_capacity(bytes.len() + PAGE_MARK.len());
    out.extend_from_slice(&bytes[..at]);
    out.extend_from_slice(PAGE_MARK.as_bytes());
    out.extend_from_slice(&bytes[at..]);
    out
}
