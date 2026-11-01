// SPDX-License-Identifier: Apache-2.0
//! Studio's local MCP server (docs/AGENTS-SPEC.md §6.2; X.5, asked by `agents`): the native twin of the reference
//! `getcadcraft.com/tools/mcp/live-server.mjs`, with exactly its rules. It decides nothing about tools: it checks each
//! request, hands the JSON-RPC message to the **hub window** (`cadcraftDesktop.control.onRequest`) and returns the hub's
//! answer. Off until the hub starts it; suite mode only; only the hub window may drive it. The token lasts the whole run
//! (kept across stop/start, so assistants stay set up); newToken() replaces it. It is not kept across restarts.
//!
//! Rules: listens on 127.0.0.1 only (port 47615, else the next free one) · `Authorization: Bearer <token>` on every
//! request (32 random bytes, base64url; made at start and by newToken(); compared in constant time; never in a URL) else
//! 401 + WWW-Authenticate · Host other than 127.0.0.1:<port> / localhost:<port> → 403 · any Origin header → 403 ·
//! POST /mcp (JSON answers, no SSE): `initialize` issues Mcp-Session-Id (32 hex); later requests need it (missing 400,
//! unknown 404); a notification or response → 202; GET → 405; DELETE with the session → 204 (0.8: a request whose
//! `params._meta` carries `io.modelcontextprotocol/protocolVersion` is MCP 2026-07-28 and needs no session: the headers
//! MCP-Protocol-Version / Mcp-Method / Mcp-Name must equal the body, else 400 with -32020; protocol errors answer 400; see
//! `modern_check` and docs/AI-CONNECT-SPEC.md § 0, § 1.4; the address and token are also written to `connect.json`, a file only
//! the person can read, for `cadcraft-mcp connect`) · POST /control (same token,
//! optional X-CadCraft-Session) · JSON only (415), ≤ 4 MB (413), query strings 400 · anything else 404. An answer with
//! error code -32002 (the person pressed Disconnect) ends the session.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, Runtime, State, WebviewWindow};

pub const DEFAULT_PORT: u16 = 47615;
const MAX_BODY: usize = 4 * 1024 * 1024;
const MAX_SESSIONS: usize = 32;
const ANSWER_WAIT: Duration = Duration::from_secs(120);

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("the system's random source");
    b
}

fn base64url(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 2);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        if c.len() > 1 {
            out.push(A[(n >> 6) as usize & 63] as char);
        }
        if c.len() > 2 {
            out.push(A[n as usize & 63] as char);
        }
    }
    out
}

pub fn random_token() -> String {
    base64url(&random_bytes::<32>())
}

fn session_id() -> String {
    random_bytes::<16>().iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant time for equal lengths; the same amount of work when the lengths differ.
pub fn token_matches(given: &str, token: &str) -> bool {
    let (a, b) = (given.as_bytes(), token.as_bytes());
    if b.is_empty() || a.len() != b.len() {
        let _ = b.iter().fold(0u8, |acc, x| acc | (x ^ x));
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

struct Session {
    last: Instant,
}

const META_V: &str = "io.modelcontextprotocol/protocolVersion";
const META_I: &str = "io.modelcontextprotocol/clientInfo";

/// Standard base64 decoding (the `=?base64?…?=` form of Mcp-Name / Mcp-Method headers); None when it is not valid base64 or UTF-8.
fn unb64(s: &str) -> Option<String> {
    let val = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let bytes: Vec<u8> = s.bytes().filter(|c| *c != b'=').collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= val(*c)? << (18 - 6 * i as u32);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    String::from_utf8(out).ok()
}

/// A header value as the body would say it: the `=?base64?…?=` form is decoded, anything else is taken as it is.
fn header_value(h: &str) -> String {
    match h.strip_prefix("=?base64?").and_then(|r| r.strip_suffix("?=")) {
        Some(b) => unb64(b).unwrap_or_default(),
        None => h.to_string(),
    }
}

/// MCP 2026-07-28 (docs/AI-CONNECT-SPEC.md § 0): a request whose `params._meta` has the protocol version is stateless. Ok(None) = a
/// legacy request (no session logic changes); Ok(Some(name)) = a stateless request from the client `name` (clientInfo, else
/// "unknown"); Err(body) = HTTP 400 with code -32020 because a header differs from the body or is missing.
fn modern_check(msg: &Value, hdr: &dyn Fn(&str) -> Option<String>) -> Result<Option<String>, Value> {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let Some(meta) = msg.get("params").and_then(|p| p.get("_meta")).filter(|m| m.is_object()) else {
        return Ok(None);
    };
    let Some(version) = meta.get(META_V) else {
        return Ok(None);
    };
    if method == "initialize" {
        return Ok(None);
    }
    let bad = |m: &str| rpc_err(&id, -32020, &format!("Header mismatch: {m}"));
    if hdr("MCP-Protocol-Version").as_deref() != version.as_str() {
        return Err(bad("MCP-Protocol-Version must equal the protocol version in _meta."));
    }
    if !method.is_empty() && hdr("Mcp-Method").map(|h| header_value(&h)).as_deref() != Some(method) {
        return Err(bad("Mcp-Method must equal the method."));
    }
    let named = match method {
        "tools/call" | "prompts/get" => msg.get("params").and_then(|p| p.get("name")).and_then(|n| n.as_str()),
        "resources/read" => msg.get("params").and_then(|p| p.get("uri")).and_then(|n| n.as_str()),
        _ => None,
    };
    if method.is_empty() || !["tools/call", "prompts/get", "resources/read"].contains(&method) {
        // no name to compare
    } else if hdr("Mcp-Name").map(|h| header_value(&h)).as_deref() != named {
        return Err(bad("Mcp-Name must equal the name in the request."));
    }
    let name = meta
        .get(META_I)
        .and_then(|c| c.get("name"))
        .and_then(|n| n.as_str())
        .map(|n| n.chars().take(100).collect::<String>())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "unknown".into());
    Ok(Some(name))
}

/// Where `cadcraft-mcp connect` finds the hub's address and token (tools/mcp/bridge.mjs `connectFile` is the same rule).
pub fn connect_file() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if let Some(p) = std::env::var_os("CADCRAFT_CONNECT_FILE") {
        return Some(PathBuf::from(p));
    }
    #[cfg(target_os = "windows")]
    {
        return std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("CadCraft Studio").join("connect.json"));
    }
    #[cfg(target_os = "macos")]
    {
        return std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library").join("Application Support").join("CadCraft Studio").join("connect.json"));
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
        base.map(|b| b.join("cadcraft-studio").join("connect.json"))
    }
}

/// Write {url, token, pid, startedAt} for the bridge: readable by this user only (0600 on Unix; under the user's own
/// %APPDATA% on Windows, which only that user and administrators can read). Best effort: a failure is not Studio's failure.
fn write_connect(url: &str, token: &str) {
    let Some(path) = connect_file() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let text = json!({ "url": url, "token": token, "pid": std::process::id(), "startedAt": started }).to_string();
    let tmp = path.with_extension("json.tmp");
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)
    };
    #[cfg(not(unix))]
    let file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&tmp);
    if let Ok(mut f) = file {
        use std::io::Write;
        if f.write_all(text.as_bytes()).is_ok() {
            drop(f);
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

fn clear_connect() {
    if let Some(p) = connect_file() {
        let _ = std::fs::remove_file(p);
    }
}

struct Running {
    port: u16,
    token: Mutex<String>,
    sessions: Mutex<HashMap<String, Session>>,
    server: Arc<tiny_http::Server>,
}

#[derive(Default)]
pub struct Control {
    running: Mutex<Option<Arc<Running>>>,
    /// One token for the whole run: made on the first start, kept across stop/start; only newToken() changes it.
    token: Mutex<Option<String>>,
    pending: Mutex<HashMap<u64, mpsc::Sender<Option<Value>>>>,
    next: AtomicU64,
}

fn rpc_err(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn reply(req: tiny_http::Request, code: u16, body: Option<&Value>, extra: &[(&str, String)]) {
    let text = body.map(|b| b.to_string()).unwrap_or_default();
    let mut r = tiny_http::Response::from_string(text).with_status_code(code);
    let mut h = vec![("Cache-Control", "no-store".to_string()), ("X-Content-Type-Options", "nosniff".to_string())];
    if body.is_some() {
        h.push(("Content-Type", "application/json".to_string()));
    }
    h.extend(extra.iter().cloned());
    for (k, v) in h {
        if let Ok(hd) = tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()) {
            r.add_header(hd);
        }
    }
    let _ = req.respond(r);
}

fn header(req: &tiny_http::Request, name: &str) -> Option<String> {
    req.headers().iter().find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name)).map(|h| h.value.as_str().to_string())
}

/// The hub's answer to one message, or None (no answer: a notification, or reply(null)).
fn forward<R: Runtime>(app: &AppHandle<R>, msg: &Value, session: &str) -> Result<Option<Value>, ()> {
    let ctl = app.state::<Control>();
    let id = ctl.next.fetch_add(1, Ordering::Relaxed) + 1;
    let (tx, rx) = mpsc::channel();
    ctl.pending.lock().unwrap().insert(id, tx);
    let hub = app.get_webview_window("main").ok_or(())?;
    let js = format!(
        "window.__ccDesktopControl?window.__ccDesktopControl({id},{msg},{{id:{}}}):window.__TAURI_INTERNALS__.invoke('cc_control_reply',{{id:{id},answer:null,missing:true}})",
        serde_json::to_string(session).unwrap()
    );
    if hub.eval(&js).is_err() {
        ctl.pending.lock().unwrap().remove(&id);
        return Err(());
    }
    let got = rx.recv_timeout(ANSWER_WAIT);
    ctl.pending.lock().unwrap().remove(&id);
    got.map_err(|_| ())
}

/// One request, by the rules above. `forward` = the hub (or a stand-in in tests).
fn handle(forward: &dyn Fn(&Value, &str) -> Result<Option<Value>, ()>, run: &Running, mut req: tiny_http::Request) {
    let null = Value::Null;
    let port = run.port;
    // Read the body first (≤ 4 MB), whatever the answer: clients keep the connection open, and an unread body would be
    // parsed as the next request.
    let mut body = Vec::new();
    if req.as_reader().take(MAX_BODY as u64 + 1).read_to_end(&mut body).is_err() {
        return;
    }
    if body.len() > MAX_BODY {
        return reply(req, 413, Some(&rpc_err(&null, -32000, "Too large")), &[("Connection", "close".into())]);
    }
    let host = header(&req, "Host").unwrap_or_default().to_ascii_lowercase();
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return reply(req, 403, Some(&rpc_err(&null, -32000, "Forbidden host")), &[]);
    }
    if header(&req, "Origin").is_some() {
        return reply(req, 403, Some(&rpc_err(&null, -32000, "Requests from web pages are refused")), &[]);
    }
    let url = req.url().to_string();
    let (path, query) = url.split_once('?').map(|(p, q)| (p.to_string(), Some(q.to_string()))).unwrap_or((url.clone(), None));
    if path != "/mcp" && path != "/control" {
        return reply(req, 404, Some(&rpc_err(&null, -32000, "Not found")), &[]);
    }
    let auth = header(&req, "Authorization").unwrap_or_default();
    let ok = auth.strip_prefix("Bearer ").is_some_and(|t| token_matches(t, &run.token.lock().unwrap()));
    if !ok {
        let msg = rpc_err(&null, -32000, "A valid token is needed (Authorization: Bearer …)");
        return reply(req, 401, Some(&msg), &[("WWW-Authenticate", "Bearer realm=\"CadCraft Studio\"".into())]);
    }
    if query.is_some() {
        return reply(req, 400, Some(&rpc_err(&null, -32000, "No query strings")), &[]);
    }
    let mcp = path == "/mcp";
    let sid = header(&req, "Mcp-Session-Id");
    let method = req.method().as_str().to_ascii_uppercase();
    if method == "GET" {
        return reply(req, 405, Some(&rpc_err(&null, -32000, "No event stream here: POST JSON-RPC messages")), &[("Allow", "POST, DELETE".into())]);
    }
    if method == "DELETE" && mcp {
        let gone = sid.as_ref().is_some_and(|s| run.sessions.lock().unwrap().remove(s).is_some());
        return if gone { reply(req, 204, None, &[]) } else { reply(req, 404, Some(&rpc_err(&null, -32000, "No such session")), &[]) };
    }
    if method != "POST" {
        let allow = if mcp { "POST, DELETE" } else { "POST" };
        return reply(req, 405, Some(&rpc_err(&null, -32000, "POST only")), &[("Allow", allow.into())]);
    }
    let ctype = header(&req, "Content-Type").unwrap_or_default().to_ascii_lowercase();
    if !(ctype == "application/json" || ctype.starts_with("application/json;") || ctype.starts_with("application/json ")) {
        return reply(req, 415, Some(&rpc_err(&null, -32000, "Content-Type must be application/json")), &[]);
    }
    let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
        return reply(req, 400, Some(&rpc_err(&null, -32700, "Parse error")), &[]);
    };
    if msg.is_array() {
        return reply(req, 400, Some(&rpc_err(&null, -32600, "Batches are not supported")), &[]);
    }
    if !msg.is_object() {
        return reply(req, 400, Some(&rpc_err(&null, -32600, "Invalid request")), &[]);
    }
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    // 2026-07-28: stateless requests (version and capabilities in params._meta) need no session
    let modern = if mcp {
        match modern_check(&msg, &|n| header(&req, n)) {
            Ok(m) => m,
            Err(body) => return reply(req, 400, Some(&body), &[]),
        }
    } else {
        None
    };
    let session = if let Some(name) = &modern {
        format!("client:{name}")
    } else if mcp {
        if msg.get("method").and_then(|m| m.as_str()) == Some("initialize") {
            let mut ss = run.sessions.lock().unwrap();
            if ss.len() >= MAX_SESSIONS {
                if let Some(oldest) = ss.iter().min_by_key(|(_, s)| s.last).map(|(k, _)| k.clone()) {
                    ss.remove(&oldest);
                }
            }
            let s = session_id();
            ss.insert(s.clone(), Session { last: Instant::now() });
            s
        } else {
            let Some(s) = sid else {
                return reply(req, 400, Some(&rpc_err(&id, -32000, "Mcp-Session-Id is required (initialize first)")), &[]);
            };
            if !run.sessions.lock().unwrap().contains_key(&s) {
                return reply(req, 404, Some(&rpc_err(&id, -32000, "This session has ended; initialize again")), &[]);
            }
            s
        }
    } else {
        let cid: String = header(&req, "X-CadCraft-Session")
            .unwrap_or_else(|| "control".into())
            .chars()
            .take(64)
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        let s = format!("control:{}", if cid.is_empty() { "control" } else { &cid });
        run.sessions.lock().unwrap().entry(s.clone()).or_insert(Session { last: Instant::now() });
        s
    };
    if let Some(s) = run.sessions.lock().unwrap().get_mut(&session) {
        s.last = Instant::now();
    }
    let answer = forward(&msg, &session).unwrap_or_else(|_| Some(rpc_err(&id, -32603, "Internal error")));
    let is_init = msg.get("method").and_then(|m| m.as_str()) == Some("initialize");
    let err_code = answer.as_ref().and_then(|a| a.get("error")).and_then(|e| e.get("code")).and_then(|c| c.as_i64());
    if (mcp && is_init && err_code.is_some()) || err_code == Some(-32002) {
        run.sessions.lock().unwrap().remove(&session);
    }
    let hdr: Vec<(&str, String)> = if mcp && modern.is_none() { vec![("Mcp-Session-Id", session.clone())] } else { vec![] };
    if msg.get("id").is_none() || msg.get("method").is_none() {
        return reply(req, 202, None, &hdr);
    }
    let out = answer.unwrap_or_else(|| rpc_err(&id, -32603, "No answer"));
    // protocol errors of a stateless request are HTTP 400 (invalid params, unsupported version, missing capability, header mismatch)
    let status = if modern.is_some() && matches!(err_code, Some(-32602) | Some(-32020) | Some(-32021) | Some(-32022)) { 400 } else { 200 };
    reply(req, status, Some(&out), &hdr)
}

fn info(run: &Running) -> Value {
    json!({ "running": true, "url": format!("http://127.0.0.1:{}/mcp", run.port), "port": run.port, "token": *run.token.lock().unwrap() })
}

fn hub_only<R: Runtime>(w: &WebviewWindow<R>, suite: &crate::windows::Suite) -> Result<(), String> {
    if !suite.on {
        return Err("off: not the suite".into());
    }
    if w.label() != "main" {
        return Err("not_allowed: only the hub".into());
    }
    Ok(())
}

#[tauri::command]
pub fn cc_control_start<R: Runtime>(
    app: AppHandle<R>,
    window: WebviewWindow<R>,
    suite: State<'_, crate::windows::Suite>,
    ctl: State<'_, Control>,
) -> Result<Value, String> {
    hub_only(&window, &suite)?;
    let mut slot = ctl.running.lock().unwrap();
    if let Some(r) = slot.as_ref() {
        return Ok(info(r));
    }
    // 127.0.0.1 only; the usual port, else the next free one.
    let (server, port) = (DEFAULT_PORT..DEFAULT_PORT + 20)
        .find_map(|p| tiny_http::Server::http(("127.0.0.1", p)).ok().map(|s| (s, p)))
        .ok_or("no free port near 47615")?;
    let token = ctl.token.lock().unwrap().get_or_insert_with(random_token).clone();
    let run = Arc::new(Running { port, token: Mutex::new(token), sessions: Mutex::new(HashMap::new()), server: Arc::new(server) });
    let (r2, h) = (run.clone(), app.clone());
    std::thread::spawn(move || {
        for req in r2.server.incoming_requests() {
            let (r3, h3) = (r2.clone(), h.clone());
            std::thread::spawn(move || handle(&|m: &Value, s: &str| forward(&h3, m, s), &r3, req));
        }
    });
    let out = info(&run);
    write_connect(out["url"].as_str().unwrap_or_default(), out["token"].as_str().unwrap_or_default());
    *slot = Some(run);
    Ok(out)
}

#[tauri::command]
pub fn cc_control_stop<R: Runtime>(window: WebviewWindow<R>, suite: State<'_, crate::windows::Suite>, ctl: State<'_, Control>) -> Result<(), String> {
    hub_only(&window, &suite)?;
    if let Some(r) = ctl.running.lock().unwrap().take() {
        r.server.unblock();
        r.sessions.lock().unwrap().clear();
        clear_connect();
    }
    Ok(())
}

#[tauri::command]
pub fn cc_control_server<R: Runtime>(window: WebviewWindow<R>, suite: State<'_, crate::windows::Suite>, ctl: State<'_, Control>) -> Result<Value, String> {
    hub_only(&window, &suite)?;
    Ok(ctl.running.lock().unwrap().as_ref().map(|r| info(r)).unwrap_or_else(|| json!({ "running": false })))
}

/// A new token: every session ends.
#[tauri::command]
pub fn cc_control_new_token<R: Runtime>(window: WebviewWindow<R>, suite: State<'_, crate::windows::Suite>, ctl: State<'_, Control>) -> Result<String, String> {
    hub_only(&window, &suite)?;
    let t = random_token();
    *ctl.token.lock().unwrap() = Some(t.clone());
    if let Some(r) = ctl.running.lock().unwrap().as_ref() {
        *r.token.lock().unwrap() = t.clone();
        r.sessions.lock().unwrap().clear();
        write_connect(&format!("http://127.0.0.1:{}/mcp", r.port), &t);
    }
    Ok(t)
}

#[tauri::command]
pub fn cc_control_drop_session<R: Runtime>(window: WebviewWindow<R>, suite: State<'_, crate::windows::Suite>, ctl: State<'_, Control>, id: String) -> Result<bool, String> {
    hub_only(&window, &suite)?;
    Ok(ctl.running.lock().unwrap().as_ref().is_some_and(|r| r.sessions.lock().unwrap().remove(&id).is_some()))
}

/// The hub's answer to message `id` (null = no answer). `missing` = the hub has no listener.
#[tauri::command]
pub fn cc_control_reply<R: Runtime>(window: WebviewWindow<R>, ctl: State<'_, Control>, id: u64, answer: Option<Value>, missing: Option<bool>) -> Result<(), String> {
    if window.label() != "main" {
        return Err("not_allowed: only the hub".into());
    }
    let answer = if missing == Some(true) { Some(rpc_err(&Value::Null, -32603, "Studio's hub is not listening")) } else { answer };
    if let Some(tx) = ctl.pending.lock().unwrap().remove(&id) {
        let _ = tx.send(answer);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(version: &str) -> Value {
        json!({ META_V: version, "io.modelcontextprotocol/clientCapabilities": {}, META_I: { "name": "evalclient", "version": "1" } })
    }
    fn hdrs<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |n: &str| pairs.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.to_string())
    }

    #[test]
    fn base64_headers() {
        assert_eq!(unb64("SGVsbG8sIOS4lueVjA==").as_deref(), Some("Hello, 世界"));
        assert_eq!(header_value("=?base64?SGVsbG8sIOS4lueVjA==?="), "Hello, 世界");
        assert_eq!(header_value("cad_command_run"), "cad_command_run");
        assert_eq!(unb64("!!!"), None);
    }

    #[test]
    fn stateless_requests() {
        let call = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "cad_hello", "arguments": {}, "_meta": meta("2026-07-28") } });
        let ok = [("MCP-Protocol-Version", "2026-07-28"), ("Mcp-Method", "tools/call"), ("Mcp-Name", "cad_hello")];
        assert_eq!(modern_check(&call, &hdrs(&ok)).unwrap().as_deref(), Some("evalclient"));
        // a legacy request (no _meta, or an initialize) is not touched
        let legacy = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
        assert_eq!(modern_check(&legacy, &hdrs(&[])).unwrap(), None);
        let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-11-25", "_meta": meta("2026-07-28") } });
        assert_eq!(modern_check(&init, &hdrs(&[])).unwrap(), None);
        // each header that differs is -32020
        for wrong in [
            [("MCP-Protocol-Version", "2025-11-25"), ("Mcp-Method", "tools/call"), ("Mcp-Name", "cad_hello")],
            [("MCP-Protocol-Version", "2026-07-28"), ("Mcp-Method", "tools/list"), ("Mcp-Name", "cad_hello")],
            [("MCP-Protocol-Version", "2026-07-28"), ("Mcp-Method", "tools/call"), ("Mcp-Name", "other")],
        ] {
            let e = modern_check(&call, &hdrs(&wrong)).unwrap_err();
            assert_eq!(e["error"]["code"], -32020);
        }
        assert!(modern_check(&call, &hdrs(&[("Mcp-Method", "tools/call"), ("Mcp-Name", "cad_hello")])).is_err(), "a missing version header");
        // tools/list needs no Mcp-Name; a base64 Mcp-Name is decoded
        let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": { "_meta": meta("2026-07-28") } });
        assert!(modern_check(&list, &hdrs(&[("MCP-Protocol-Version", "2026-07-28"), ("Mcp-Method", "tools/list")])).is_ok());
        let unicode = json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "Hello, 世界", "_meta": meta("2026-07-28") } });
        assert!(modern_check(&unicode, &hdrs(&[("MCP-Protocol-Version", "2026-07-28"), ("Mcp-Method", "tools/call"), ("Mcp-Name", "=?base64?SGVsbG8sIOS4lueVjA==?=")])).is_ok());
    }

    #[test]
    fn connect_file_override() {
        // only checked when the variable is set by the caller; the rule itself is tools/mcp/bridge.mjs connectFile
        if let Some(p) = std::env::var_os("CADCRAFT_CONNECT_FILE") {
            assert_eq!(connect_file(), Some(std::path::PathBuf::from(p)));
        }
    }

    /// agents' own checker (docs/AGENTS-SPEC.md §6.2) against this server with a stand-in hub. Needs Node and the site
    /// (CADCRAFT_SITE = the site's folder, which has tools/mcp/test/check-server.mjs):
    /// `CADCRAFT_SITE=<site> cargo test -- --ignored mcp_checker`
    #[test]
    #[ignore]
    fn mcp_checker() {
        let site = std::env::var("CADCRAFT_SITE").expect("CADCRAFT_SITE is not set: point it at the site's folder (tools/mcp/test/check-server.mjs)");
        let checker = std::path::Path::new(&site).join("tools/mcp/test/check-server.mjs");
        assert!(checker.is_file(), "no checker at {}", checker.display());
        let (server, port) = (47700..47720).find_map(|p| tiny_http::Server::http(("127.0.0.1", p)).ok().map(|s| (s, p))).unwrap();
        let run = Arc::new(Running { port, token: Mutex::new(random_token()), sessions: Mutex::new(HashMap::new()), server: Arc::new(server) });
        let r2 = run.clone();
        std::thread::spawn(move || {
            for req in r2.server.incoming_requests() {
                let r3 = r2.clone();
                std::thread::spawn(move || {
                    let hub = |m: &Value, _s: &str| -> Result<Option<Value>, ()> {
                        if m.get("id").is_none() {
                            return Ok(None);
                        }
                        Ok(Some(json!({ "jsonrpc": "2.0", "id": m["id"], "result": { "tools": [], "protocolVersion": "2025-06-18" } })))
                    };
                    handle(&hub, &r3, req)
                });
            }
        });
        let token = run.token.lock().unwrap().clone();
        let out = std::process::Command::new("node").arg(&checker).args([format!("http://127.0.0.1:{port}/mcp").as_str(), token.as_str(), "--verbose"]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        println!("{text}");
        assert!(out.status.success(), "{text}");
    }

    #[test]
    fn tokens() {
        let t = random_token();
        assert_eq!(t.len(), 43, "32 bytes base64url without padding");
        assert!(t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_ne!(t, random_token());
        assert!(token_matches(&t, &t));
        assert!(!token_matches(&t[..42], &t) && !token_matches("", &t) && !token_matches(&t, ""));
        assert_eq!(base64url(b"\xfb\xff"), "-_8");
        assert_eq!(session_id().len(), 32);
    }
}
