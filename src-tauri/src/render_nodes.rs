// SPDX-License-Identifier: Apache-2.0
//! LAN render nodes (0.7 CU.7; the pages' side is `lib/compute/lan*.js` in the site repo, contract docs/apps/compute.md
//! § LAN render nodes). Studio builds with the `live` feature, app windows only, computers only (tablets have no node).
//!
//! The pages talk to each other end to end (Studio Live's SPAKE2 handshake with the node's code, then sealed frames); this
//! module only **finds each other and carries bytes**, and never reads them:
//!
//!   renderNodes.advertise(tag)        listen on a free port and advertise mDNS `_ccrender._tcp`, instance = the 2-character tag
//!                                     only (no user, computer or file name in what the network sees); off until the page calls it
//!   renderNodes.stopAdvertising()
//!   renderNodes.onConnection(cb)      each peer that connects (`/render`) becomes a transport for cb; one at a time per node
//!   renderNodes.browse() / onNodes    the nodes found on this network now: [{tag, name, host, port}] (name = the tag: the shell
//!                                     knows no computer name the person allowed)
//!   renderNodes.connect({host, port}) a transport to a node's `ws://<host>:<port>/render`
//!   transport = {send(Uint8Array), close(), onmessage(Uint8Array), onclose()}
//!
//! Rules (all enforced here, tested in this file): peers must have a private address (loopback, RFC 1918, link-local, IPv6
//! unique-local) both ways — a connection from or to anything else is refused; the path is exactly `/render`; binary messages
//! only, ≤ 8 MiB; one connection at a time per node (a second is refused before its handshake); never through the relay.
//!
//! Plumbing is Studio Live's (studio_live.rs): `cc_render_connect` / `cc_render_accept` give an id, `cc_render_send` takes one
//! message as the raw body (header `x-cc-render` = the id), `cc_render_recv` is a long poll answering records kind (0 open,
//! 1 message, 2 close) ‖ u32 length ‖ payload, `cc_render_close` ends it.

use crate::live::crypto;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::ipc::{Request, Response};
use tauri::{AppHandle, Manager, Runtime, State, WebviewWindow};
use tokio::sync::{mpsc, watch};

const SERVICE: &str = "_ccrender._tcp.local.";
/// One binary message: the page splits scenes into 4 MiB parts, so this is twice that.
pub const MAX_MSG: usize = 8 * 1024 * 1024;
const RECV_BATCH: usize = 4 * 1024 * 1024;
const HANDSHAKE: Duration = Duration::from_secs(5);

/// Addresses a render node may talk to: this machine, the LAN's private ranges, link-local, IPv6 unique-local.
pub fn private_peer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => a.is_private() || a.is_loopback() || a.is_link_local(),
        IpAddr::V6(a) => {
            a.is_loopback()
                || (a.segments()[0] & 0xfe00) == 0xfc00
                || (a.segments()[0] & 0xffc0) == 0xfe80
                || a.to_ipv4_mapped().map_or(false, |v| private_peer(IpAddr::V4(v)))
        }
    }
}

/// A node's tag: 2 characters of Studio Live's code alphabet, upper case.
pub fn tag_ok(tag: &str) -> Option<String> {
    let t = tag.to_ascii_uppercase();
    (t.len() == 2 && t.bytes().all(|b| crypto::CODE_ALPHABET.contains(&b))).then_some(t)
}

enum In {
    Open,
    Msg(Vec<u8>),
    Close(String, u16),
}

fn record(out: &mut Vec<u8>, kind: u8, payload: &[u8]) {
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
}

/// false after a close record: nothing follows it.
fn push_in(out: &mut Vec<u8>, m: In) -> bool {
    match m {
        In::Open => record(out, 0, &[]),
        In::Msg(b) => record(out, 1, &b),
        In::Close(reason, code) => {
            record(out, 2, serde_json::json!({ "reason": reason, "code": code }).to_string().as_bytes());
            return false;
        }
    }
    true
}

struct Conn {
    window: String,
    out: mpsc::UnboundedSender<Vec<u8>>,
    rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<In>>>,
    /// An accepted connection holds its node's one place until it ends.
    active: Option<Arc<AtomicBool>>,
}

struct Node {
    window: String,
    stop: watch::Sender<bool>,
    accept: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<u64>>>,
    fullname: Option<String>,
}

pub struct Inner {
    on: bool,
    next: AtomicU64,
    conns: Mutex<HashMap<u64, Conn>>,
    node: Mutex<Option<Node>>,
    mdns: Mutex<Option<ServiceDaemon>>,
}

#[derive(Clone)]
pub struct RenderNodes(Arc<Inner>);

impl RenderNodes {
    pub fn new(on: bool) -> Self {
        RenderNodes(Arc::new(Inner { on, next: AtomicU64::new(1), conns: Mutex::default(), node: Mutex::default(), mdns: Mutex::default() }))
    }

    fn daemon(&self) -> Result<ServiceDaemon, String> {
        let mut d = self.0.mdns.lock().unwrap();
        if d.is_none() {
            *d = Some(ServiceDaemon::new().map_err(|e| format!("mdns: {e}"))?);
        }
        Ok(d.clone().unwrap())
    }

    fn register(&self, window: &str, out: mpsc::UnboundedSender<Vec<u8>>, rx: mpsc::UnboundedReceiver<In>, active: Option<Arc<AtomicBool>>) -> u64 {
        let id = self.0.next.fetch_add(1, Ordering::SeqCst);
        self.0.conns.lock().unwrap().insert(id, Conn { window: window.to_string(), out, rx: Arc::new(tokio::sync::Mutex::new(rx)), active });
        id
    }

    fn close(&self, id: u64) {
        let Some(c) = self.0.conns.lock().unwrap().remove(&id) else { return };
        if let Some(a) = c.active {
            a.store(false, Ordering::SeqCst);
        }
        drop(c.out); // the socket task closes the WebSocket and ends
    }

    /// Stops this window's node (listener, advertisement) and closes its connections.
    pub fn stop_node(&self, window: &str) {
        let node = {
            let mut n = self.0.node.lock().unwrap();
            if n.as_ref().map_or(false, |x| x.window == window) { n.take() } else { None }
        };
        if let Some(n) = node {
            let _ = n.stop.send(true);
            if let (Some(full), Some(d)) = (n.fullname, self.0.mdns.lock().unwrap().clone()) {
                let _ = d.unregister(&full);
            }
        }
        let ids: Vec<u64> = self.0.conns.lock().unwrap().iter().filter(|(_, c)| c.window == window).map(|(k, _)| *k).collect();
        for id in ids {
            self.close(id);
        }
    }

    /// Starts listening on `bind` (port 0 = a free one) for `window`; returns the port. The caller advertises it.
    pub async fn serve(&self, window: &str, bind: IpAddr) -> Result<u16, String> {
        if self.0.node.lock().unwrap().is_some() {
            return Err("busy: this computer is already offering to render".into());
        }
        let listener = tokio::net::TcpListener::bind((bind, 0)).await.map_err(|e| format!("error: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let (stop_tx, stop_rx) = watch::channel(false);
        let (acc_tx, acc_rx) = mpsc::unbounded_channel();
        {
            let mut n = self.0.node.lock().unwrap();
            if n.is_some() {
                return Err("busy: this computer is already offering to render".into());
            }
            *n = Some(Node { window: window.to_string(), stop: stop_tx, accept: Arc::new(tokio::sync::Mutex::new(acc_rx)), fullname: None });
        }
        let me = self.clone();
        let window = window.to_string();
        tauri::async_runtime::spawn(async move { me.accept_loop(listener, window, stop_rx, acc_tx).await });
        Ok(port)
    }

    async fn accept_loop(&self, listener: tokio::net::TcpListener, window: String, mut stop: watch::Receiver<bool>, acc: mpsc::UnboundedSender<u64>) {
        let active = Arc::new(AtomicBool::new(false));
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                r = listener.accept() => {
                    let Ok((stream, peer)) = r else { continue };
                    // Only the LAN, and only one at a time: refused before any handshake, so a stranger learns nothing.
                    if !private_peer(peer.ip()) || active.swap(true, Ordering::SeqCst) {
                        drop(stream);
                        continue;
                    }
                    let me = self.clone();
                    let (window, acc, active) = (window.clone(), acc.clone(), active.clone());
                    tauri::async_runtime::spawn(async move {
                        use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
                        let cb = |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
                            if req.uri().path() == "/render" {
                                Ok(resp)
                            } else {
                                let mut e = ErrorResponse::new(Some("not found".into()));
                                *e.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::NOT_FOUND;
                                Err(e)
                            }
                        };
                        let ws = match tokio::time::timeout(HANDSHAKE, tokio_tungstenite::accept_hdr_async(stream, cb)).await {
                            Ok(Ok(ws)) => ws,
                            _ => { active.store(false, Ordering::SeqCst); return; }
                        };
                        let (tx_in, rx_in) = mpsc::unbounded_channel();
                        let (tx_out, rx_out) = mpsc::unbounded_channel();
                        let id = me.register(&window, tx_out, rx_in, Some(active.clone()));
                        let _ = tx_in.send(In::Open);
                        if acc.send(id).is_err() {
                            me.close(id);
                            return;
                        }
                        pump(ws, tx_in, rx_out).await;
                        me.close(id);
                    });
                }
            }
        }
    }

    /// A transport to a node at `ws://<ip>:<port>/render`. Private addresses only, never a name.
    pub async fn connect(&self, window: &str, host: &str, port: u16) -> Result<u64, String> {
        let ip: IpAddr = host.trim_matches(|c| c == '[' || c == ']').parse().map_err(|_| "not_allowed: an address, not a name".to_string())?;
        if !private_peer(ip) {
            return Err("not_allowed: this network's computers only".into());
        }
        if port == 0 {
            return Err("error: bad port".into());
        }
        let url = match ip {
            IpAddr::V4(a) => format!("ws://{a}:{port}/render"),
            IpAddr::V6(a) => format!("ws://[{a}]:{port}/render"),
        };
        let ws = match tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(url.as_str())).await {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(e)) => return Err(format!("connect: {e}")),
            Err(_) => return Err("connect: timed out".into()),
        };
        let (tx_in, rx_in) = mpsc::unbounded_channel();
        let (tx_out, rx_out) = mpsc::unbounded_channel();
        let id = self.register(window, tx_out, rx_in, None);
        let _ = tx_in.send(In::Open);
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            pump(ws, tx_in, rx_out).await;
            me.close(id);
        });
        Ok(id)
    }

    /// The next record batch of a connection (at least one record), for its window.
    async fn recv(&self, window: &str, id: u64) -> Vec<u8> {
        let rx = self.0.conns.lock().unwrap().get(&id).filter(|c| c.window == window).map(|c| c.rx.clone());
        let mut out = vec![];
        let Some(rx) = rx else {
            push_in(&mut out, In::Close("gone".into(), 0));
            return out;
        };
        let mut rx = rx.lock().await;
        match rx.recv().await {
            None => {
                push_in(&mut out, In::Close("closed".into(), 0));
            }
            Some(m) => {
                if push_in(&mut out, m) {
                    while out.len() < RECV_BATCH {
                        match rx.try_recv() {
                            Ok(m) => {
                                if !push_in(&mut out, m) {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }
            }
        }
        out
    }

    fn send(&self, window: &str, id: u64, bytes: Vec<u8>) -> Result<(), String> {
        if bytes.len() > MAX_MSG {
            return Err("error: message too large".into());
        }
        let conns = self.0.conns.lock().unwrap();
        let c = conns.get(&id).filter(|c| c.window == window).ok_or("closed: no such connection")?;
        c.out.send(bytes).map_err(|_| "closed: the other end left".to_string())
    }
}

/// Moves messages between a WebSocket and the page's queues until either end closes. Text and pings are not the protocol's;
/// a binary message over MAX_MSG ends the connection (1009).
async fn pump<S>(ws: tokio_tungstenite::WebSocketStream<S>, tx: mpsc::UnboundedSender<In>, mut out: mpsc::UnboundedReceiver<Vec<u8>>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let (mut sink, mut source) = ws.split();
    loop {
        tokio::select! {
            m = out.recv() => match m {
                Some(b) => if sink.send(Message::Binary(b.into())).await.is_err() { break },
                None => { let _ = sink.close().await; return; }   // the page closed it
            },
            msg = source.next() => match msg {
                Some(Ok(Message::Binary(b))) => {
                    if b.len() > MAX_MSG {
                        let _ = tx.send(In::Close("message too large".into(), 1009));
                        let _ = sink.close().await;
                        return;
                    }
                    if tx.send(In::Msg(b.to_vec())).is_err() { break }
                }
                Some(Ok(Message::Close(f))) => {
                    let (code, reason) = f.map(|f| (u16::from(f.code), f.reason.to_string())).unwrap_or((1005, String::new()));
                    let _ = tx.send(In::Close(reason, code));
                    return;
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => { let _ = tx.send(In::Close(e.to_string(), 0)); return; }
                None => break,
            },
        }
    }
    let _ = tx.send(In::Close("closed".into(), 1006));
}

// ----------------------------------------------------------------------------------------------------------------- commands

/// App windows of Studio only (a node belongs to the app that offers it; the hub does not).
fn app_window(rn: &RenderNodes, w: &WebviewWindow) -> Result<String, String> {
    if !rn.0.on {
        return Err("off: not the suite".into());
    }
    let label = w.label();
    if label.starts_with("app-") { Ok(label.to_string()) } else { Err("not_allowed: app windows only".into()) }
}

#[cfg(debug_assertions)]
fn bind_ip() -> IpAddr {
    std::env::var("CC_RENDER_BIND").ok().and_then(|v| v.parse().ok()).unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
}
#[cfg(not(debug_assertions))]
fn bind_ip() -> IpAddr {
    IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
}

#[derive(Deserialize)]
pub struct AdArgs {
    tag: String,
}

#[derive(Serialize)]
pub struct AdInfo {
    port: u16,
}

#[tauri::command]
pub async fn cc_render_advertise(window: WebviewWindow, rn: State<'_, RenderNodes>, args: AdArgs) -> Result<AdInfo, String> {
    let label = app_window(&rn, &window)?;
    let tag = tag_ok(&args.tag).ok_or("error: bad tag")?;
    let rn = rn.inner().clone();
    let port = rn.serve(&label, bind_ip()).await?;
    let adv = (|| -> Result<String, String> {
        let addrs = crate::studio_live::lan_addrs();
        if addrs.is_empty() {
            return Err("offline: no network address".into());
        }
        // A host name that says nothing about the machine or the person.
        let mut r = [0u8; 4];
        getrandom::fill(&mut r).map_err(|e| e.to_string())?;
        let host = format!("ccrender-{}.local.", r.iter().map(|x| format!("{x:02x}")).collect::<String>());
        let info = ServiceInfo::new(SERVICE, &tag, &host, addrs.join(",").as_str(), port, &[][..] as &[(&str, &str)]).map_err(|e| format!("mdns: {e}"))?;
        let full = info.get_fullname().to_string();
        rn.daemon()?.register(info).map_err(|e| format!("mdns: {e}"))?;
        Ok(full)
    })();
    match adv {
        Ok(full) => {
            if let Some(n) = rn.0.node.lock().unwrap().as_mut() {
                n.fullname = Some(full);
            }
            Ok(AdInfo { port })
        }
        Err(e) => {
            rn.stop_node(&label);
            Err(e)
        }
    }
}

#[tauri::command]
pub fn cc_render_unadvertise(window: WebviewWindow, rn: State<'_, RenderNodes>) -> Result<(), String> {
    let label = app_window(&rn, &window)?;
    rn.stop_node(&label);
    Ok(())
}

/// Long poll: the id of the next peer that connected to this window's node, or null once the node stopped.
#[tauri::command]
pub async fn cc_render_accept(window: WebviewWindow, rn: State<'_, RenderNodes>) -> Result<Option<u64>, String> {
    let label = app_window(&rn, &window)?;
    let acc = rn.0.node.lock().unwrap().as_ref().filter(|n| n.window == label).map(|n| n.accept.clone());
    let Some(acc) = acc else { return Ok(None) };
    let mut stop = rn.0.node.lock().unwrap().as_ref().map(|n| n.stop.subscribe());
    let mut acc = acc.lock().await;
    Ok(match stop.as_mut() {
        Some(s) => tokio::select! { id = acc.recv() => id, _ = s.changed() => None },
        None => acc.recv().await,
    })
}

#[derive(Deserialize)]
pub struct BrowseArgs {
    #[serde(default)]
    ms: Option<u64>,
}

#[derive(Serialize, Debug)]
pub struct NodeFound {
    tag: String,
    name: String,
    host: String,
    port: u16,
}

/// Every `_ccrender._tcp` instance that answers within `ms` (≤ 5 s), this computer's own left out.
#[tauri::command(async)]
pub fn cc_render_browse(window: WebviewWindow, rn: State<'_, RenderNodes>, args: BrowseArgs) -> Result<Vec<NodeFound>, String> {
    app_window(&rn, &window)?;
    let wait = Duration::from_millis(args.ms.unwrap_or(1500).clamp(100, 5000));
    let own = rn.0.node.lock().unwrap().as_ref().and_then(|n| n.fullname.clone());
    let d = rn.daemon()?;
    browse(&d, wait, own.as_deref())
}

fn browse(d: &ServiceDaemon, wait: Duration, own: Option<&str>) -> Result<Vec<NodeFound>, String> {
    let rx = d.browse(SERVICE).map_err(|e| format!("mdns: {e}"))?;
    let until = Instant::now() + wait;
    let mut found: Vec<NodeFound> = vec![];
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok(ServiceEvent::ServiceResolved(s)) => {
                if own.map_or(false, |o| o.eq_ignore_ascii_case(&s.fullname)) {
                    continue;
                }
                let Some(tag) = s.fullname.strip_suffix(&format!(".{SERVICE}")).and_then(tag_ok) else { continue };
                if found.iter().any(|f| f.tag == tag) {
                    continue;
                }
                let mut ips: Vec<IpAddr> = s.addresses.iter().map(|a| a.to_ip_addr()).filter(|ip| private_peer(*ip)).collect();
                ips.sort_by_key(|ip| (!ip.is_ipv4(), matches!(ip, IpAddr::V4(a) if !a.is_private()), *ip));
                let Some(ip) = ips.into_iter().find(|ip| !matches!(ip, IpAddr::V6(a) if (a.segments()[0] & 0xffc0) == 0xfe80)) else { continue };
                found.push(NodeFound { name: tag.clone(), tag, host: ip.to_string(), port: s.port });
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = d.stop_browse(SERVICE);
    Ok(found)
}

#[derive(Deserialize)]
pub struct ConnectArgs {
    host: String,
    port: u16,
}

#[tauri::command]
pub async fn cc_render_connect(window: WebviewWindow, rn: State<'_, RenderNodes>, args: ConnectArgs) -> Result<u64, String> {
    let label = app_window(&rn, &window)?;
    rn.inner().clone().connect(&label, &args.host, args.port).await
}

#[tauri::command]
pub async fn cc_render_recv(window: WebviewWindow, rn: State<'_, RenderNodes>, id: u64) -> Result<Response, String> {
    let label = app_window(&rn, &window)?;
    Ok(Response::new(rn.recv(&label, id).await))
}

/// Body: one message. Header `x-cc-render` = the connection id.
#[tauri::command]
pub fn cc_render_send(window: WebviewWindow, rn: State<'_, RenderNodes>, request: Request<'_>) -> Result<(), String> {
    let label = app_window(&rn, &window)?;
    let id: u64 = request.headers().get("x-cc-render").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).ok_or("missing id")?;
    let body = crate::files::body_bytes(request.body()).ok_or("expected bytes")?;
    rn.send(&label, id, body.to_vec())
}

#[tauri::command]
pub fn cc_render_close(window: WebviewWindow, rn: State<'_, RenderNodes>, id: u64) {
    if let Ok(label) = app_window(&rn, &window) {
        let mine = rn.0.conns.lock().unwrap().get(&id).map_or(false, |c| c.window == label);
        if mine {
            rn.close(id);
        }
    }
}

/// The window went away: its node stops and its connections close.
pub fn on_window_destroyed<R: Runtime>(app: &AppHandle<R>, label: &str) {
    if let Some(rn) = app.try_state::<RenderNodes>() {
        rn.stop_node(label);
        // Connections this window made as a client (no node of its own).
        let ids: Vec<u64> = rn.0.conns.lock().unwrap().iter().filter(|(_, c)| c.window == label).map(|(k, _)| *k).collect();
        for id in ids {
            rn.close(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn lo() -> IpAddr {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    }
    fn run<F: std::future::Future>(f: F) -> F::Output {
        tauri::async_runtime::block_on(f)
    }
    /// The records of one batch as (kind, payload).
    fn records(b: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut v = vec![];
        let mut p = 0;
        while p + 5 <= b.len() {
            let n = u32::from_be_bytes([b[p + 1], b[p + 2], b[p + 3], b[p + 4]]) as usize;
            v.push((b[p], b[p + 5..p + 5 + n].to_vec()));
            p += 5 + n;
        }
        v
    }
    /// What the page's accept loop does: the next peer's id.
    async fn accepted(rn: &RenderNodes) -> Option<u64> {
        let acc = rn.0.node.lock().unwrap().as_ref().map(|n| n.accept.clone())?;
        let mut acc = acc.lock().await;
        tokio::time::timeout(Duration::from_secs(5), acc.recv()).await.ok().flatten()
    }
    /// Receives until a message record arrives; returns its bytes.
    async fn message(rn: &RenderNodes, w: &str, id: u64) -> Vec<u8> {
        loop {
            let b = tokio::time::timeout(Duration::from_secs(10), rn.recv(w, id)).await.expect("a message within 10 s");
            for (k, p) in records(&b) {
                if k == 1 {
                    return p;
                }
                assert_ne!(k, 2, "closed: {}", String::from_utf8_lossy(&p));
            }
        }
    }

    #[test]
    fn only_this_networks_computers_may_talk() {
        for ok in ["127.0.0.1", "10.1.2.3", "172.16.0.9", "172.31.255.1", "192.168.1.20", "169.254.3.4", "::1", "fd12:3456::1", "fe80::1", "::ffff:192.168.0.5"] {
            assert!(private_peer(ok.parse().unwrap()), "{ok}");
        }
        for no in ["8.8.8.8", "1.1.1.1", "172.32.0.1", "100.64.0.1", "2001:4860:4860::8888", "::ffff:8.8.8.8", "0.0.0.0"] {
            assert!(!private_peer(no.parse().unwrap()), "{no}");
        }
        let _ = (Ipv6Addr::LOCALHOST,);
    }

    #[test]
    fn a_tag_is_two_characters_of_the_code_alphabet() {
        assert_eq!(tag_ok("k7").as_deref(), Some("K7"));
        assert!(tag_ok("K").is_none() && tag_ok("K7K").is_none() && tag_ok("O0").is_none() && tag_ok("a b").is_none() && tag_ok("").is_none());
    }

    #[test]
    fn two_studios_over_a_real_socket() {
        run(async {
            let node = RenderNodes::new(true);
            let client = RenderNodes::new(true);
            let port = node.serve("app-modelcraft", lo()).await.unwrap();
            assert!(node.serve("app-bimcraft", lo()).await.unwrap_err().starts_with("busy"), "one offer per computer");

            let c = client.connect("app-bimcraft", "127.0.0.1", port).await.unwrap();
            let s = accepted(&node).await.expect("the node sees the peer");
            // Both ends start with an open record.
            assert_eq!(records(&client.recv("app-bimcraft", c).await)[0].0, 0);
            assert_eq!(records(&node.recv("app-modelcraft", s).await)[0].0, 0);

            // Bytes pass untouched, both ways, in order.
            client.send("app-bimcraft", c, b"hello node".to_vec()).unwrap();
            client.send("app-bimcraft", c, vec![0, 255, 7]).unwrap();
            assert_eq!(message(&node, "app-modelcraft", s).await, b"hello node");
            assert_eq!(message(&node, "app-modelcraft", s).await, vec![0, 255, 7]);
            let big: Vec<u8> = (0..6 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
            node.send("app-modelcraft", s, big.clone()).unwrap();
            assert!(message(&client, "app-bimcraft", c).await == big, "a 6 MiB message arrives whole");

            // One connection at a time: a second peer is refused before its handshake.
            assert!(client.connect("app-bimcraft", "127.0.0.1", port).await.is_err(), "the node is taken");

            // A window can't use another window's connection.
            assert!(client.send("app-markupcraft", c, vec![1]).is_err());
            assert!(records(&client.recv("app-markupcraft", c).await).iter().any(|(k, _)| *k == 2), "someone else's id answers 'gone'");

            // The client leaves: the node hears a close, and its place frees up for the next peer.
            client.close(c);
            let mut saw_close = false;
            for _ in 0..5 {
                let b = tokio::time::timeout(Duration::from_secs(5), node.recv("app-modelcraft", s)).await.unwrap();
                if records(&b).iter().any(|(k, _)| *k == 2) { saw_close = true; break; }
            }
            assert!(saw_close, "the node is told the peer left");
            node.close(s);
            let again = client.connect("app-bimcraft", "127.0.0.1", port).await;
            assert!(again.is_ok(), "after the first peer left, the next may connect: {again:?}");
            assert!(accepted(&node).await.is_some());

            // Stopping the node closes everything and the port goes quiet.
            node.stop_node("app-modelcraft");
            assert!(node.0.node.lock().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(client.connect("app-bimcraft", "127.0.0.1", port).await.is_err(), "nothing listens after stop");
        });
    }

    #[test]
    fn the_path_must_be_render_and_the_message_size_is_capped() {
        run(async {
            let node = RenderNodes::new(true);
            let port = node.serve("app-modelcraft", lo()).await.unwrap();
            // Any other path is a 404 at the handshake.
            let wrong = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/live")).await;
            assert!(wrong.is_err(), "/live is not served here");
            // The refused attempt did not use the node's one place.
            let client = RenderNodes::new(true);
            let c = client.connect("app-bimcraft", "127.0.0.1", port).await.expect("a refused path leaves the place free");
            let _ = accepted(&node).await.unwrap();
            // Too large to send at all.
            assert!(client.send("app-bimcraft", c, vec![0; MAX_MSG + 1]).unwrap_err().contains("too large"));
            assert!(client.send("app-bimcraft", c, vec![0; MAX_MSG]).is_ok(), "exactly the cap is fine");
            node.stop_node("app-modelcraft");
        });
    }

    #[test]
    fn connect_takes_a_private_address_and_nothing_else() {
        run(async {
            let rn = RenderNodes::new(true);
            for (host, want) in [("8.8.8.8", "not_allowed"), ("example.com", "not_allowed"), ("localhost", "not_allowed"), ("", "not_allowed"), ("10.0.0.1; rm", "not_allowed")] {
                let e = rn.connect("app-x", host, 9).await.unwrap_err();
                assert!(e.starts_with(want), "{host}: {e}");
            }
            assert!(rn.connect("app-x", "127.0.0.1", 0).await.unwrap_err().contains("bad port"));
        });
    }

    #[test]
    #[ignore = "needs multicast on this machine (like studio_live's mdns test)"]
    fn mdns_advertise_then_browse() {
        let d = ServiceDaemon::new().unwrap();
        let info = ServiceInfo::new(SERVICE, "K7", "ccrender-test.local.", "127.0.0.1", 4242, &[][..] as &[(&str, &str)]).unwrap();
        d.register(info).unwrap();
        let found = browse(&d, Duration::from_secs(5), None).unwrap();
        assert!(found.iter().any(|f| f.tag == "K7" && f.port == 4242), "{found:?}");
    }
}
