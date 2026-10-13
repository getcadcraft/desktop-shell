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
//! unknown 404); a notification or response → 202; GET → 405; DELETE with the session → 204 · POST /control (same token,
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
    let session = if mcp {
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
    let hdr: Vec<(&str, String)> = if mcp { vec![("Mcp-Session-Id", session.clone())] } else { vec![] };
    if msg.get("id").is_none() || msg.get("method").is_none() {
        return reply(req, 202, None, &hdr);
    }
    let out = answer.unwrap_or_else(|| rpc_err(&id, -32603, "No answer"));
    reply(req, 200, Some(&out), &hdr)
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
    *slot = Some(run);
    Ok(out)
}

#[tauri::command]
pub fn cc_control_stop<R: Runtime>(window: WebviewWindow<R>, suite: State<'_, crate::windows::Suite>, ctl: State<'_, Control>) -> Result<(), String> {
    hub_only(&window, &suite)?;
    if let Some(r) = ctl.running.lock().unwrap().take() {
        r.server.unblock();
        r.sessions.lock().unwrap().clear();
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
