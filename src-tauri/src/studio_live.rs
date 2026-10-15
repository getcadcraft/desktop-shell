// SPDX-License-Identifier: Apache-2.0
//! Studio Live in Studio's windows (spec docs/STUDIO-LIVE-SPEC.md, messages docs/LIVE-PROTOCOL.md, both in the site repo):
//! the glue between an app window and the session server in `live/`, to live-studio's contract (docs/0.5-sessions/
//! desktop.md § Inbox, "the Studio glue for Studio Live"). Studio builds only (suite mode), app windows (browse: the hub too).
//!
//!   cadcraftDesktop.live.transport('local', {app, schema})  this window's own session: started on first use
//!     (`live::start`, 0.0.0.0 (debug builds: `CC_LIVE_BIND`), a free port, Studio's suite folder served on it for browser joiners), then `attach_local`;
//!     envelopes both ways, no frames. One session per window; it stops when the window's host connection closes or the
//!     window goes.
//!   transport('join', {url})  a joiner's socket opened here, not by the page: WebView2 refuses sockets from Studio's origin
//!     to LAN addresses. `ws://<lan addr>:<port>/live`, or the relay's `wss://<site>/live/relay/join/<ROOM>`; binary messages
//!     pass through untouched (the relay's text is dropped). live-client.js does the handshake (`encrypts` false).
//!   session() → {running, port, addrs (non-loopback IPv4, private ranges first), app}
//!   advertise(tag | {room}, {app}) / unadvertise(tag | {room})  mDNS `_ccstudio._tcp`, instance = the tag only (a short code's
//!     2 characters, or `room_tag(ROOM)` for an invite link, LV.26), port = the session's, TXT `app=<slug>`; nothing else.
//!   browse(tag | {room}, ms) → [{addr, port, app}]   the first answer for that instance, within ms (≤ 5 s)
//!   relay(url, ticket) / relayStop()  host this window's session through the relay too (`room.relay_url`, LV.23)
//!   exportState() → bytes (`handle.export_state()`)   startFrom(bytes) → session()   host handoff (LV.17)
//!
//! Transport plumbing: `cc_live_open` → an id; `cc_live_send` (raw body: u32 length ‖ bytes, repeated; the page sends one
//! batch at a time, so order holds); `cc_live_recv` (long poll: waits for the next records, then takes what is queued):
//! records kind (0 open, 1 message, 2 close) ‖ u32 length ‖ payload (close: JSON {reason, code}); `cc_live_close`.

use crate::live::{self, crypto, LiveHandle, RelayHandle};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::ipc::{Request, Response};
use tauri::{AppHandle, Manager, Runtime, State, WebviewWindow};
use tokio::sync::mpsc;

const SERVICE: &str = "_ccstudio._tcp.local.";
/// After the host's window leaves its session, the server lives this long so `end` / `moved` reach everyone.
const STOP_AFTER: Duration = Duration::from_secs(3);
const RECV_BATCH: usize = 4 * 1024 * 1024;

enum In {
    Open,
    Msg(Vec<u8>),
    Close(String, u16),
}

enum Out {
    Local(live::LocalClient),
    Net(mpsc::UnboundedSender<Vec<u8>>),
}

struct Conn {
    window: String,
    out: Out,
    rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<In>>>,
}

#[derive(Default)]
struct Win {
    handle: Option<LiveHandle>,
    relay: Option<RelayHandle>,
    app: String,
    /// The window's own (host) connection to its session.
    host_conn: Option<u64>,
    /// mDNS full names this window advertises.
    ads: Vec<String>,
}

pub struct StudioLive {
    on: bool,
    www: PathBuf,
    next: AtomicU64,
    conns: Mutex<HashMap<u64, Conn>>,
    wins: Mutex<HashMap<String, Win>>,
    mdns: Mutex<Option<ServiceDaemon>>,
}

impl StudioLive {
    pub fn new(on: bool, www: PathBuf) -> Self {
        StudioLive { on, www, next: AtomicU64::new(1), conns: Mutex::default(), wins: Mutex::default(), mdns: Mutex::default() }
    }

    fn daemon(&self) -> Result<ServiceDaemon, String> {
        let mut d = self.mdns.lock().unwrap();
        if d.is_none() {
            *d = Some(ServiceDaemon::new().map_err(|e| format!("mdns: {e}"))?);
        }
        Ok(d.clone().unwrap())
    }

    /// Ends a window's session: its relay, its advertisements, then (after STOP_AFTER) the server itself.
    fn stop_window(&self, label: &str) {
        let w = self.wins.lock().unwrap().remove(label);
        let Some(w) = w else { return };
        if let Ok(d) = self.daemon_if_running() {
            for full in &w.ads {
                let _ = d.unregister(full);
            }
        }
        if w.handle.is_some() || w.relay.is_some() {
            std::thread::spawn(move || {
                std::thread::sleep(STOP_AFTER);
                if let Some(r) = w.relay {
                    r.stop();
                }
                if let Some(h) = w.handle {
                    h.stop();
                }
            });
        }
    }

    fn daemon_if_running(&self) -> Result<ServiceDaemon, ()> {
        self.mdns.lock().unwrap().clone().ok_or(())
    }

    /// The window went away (main.rs, WindowEvent::Destroyed): its connections close and its session ends.
    pub fn window_gone(&self, label: &str) {
        let ids: Vec<u64> = self.conns.lock().unwrap().iter().filter(|(_, c)| c.window == label).map(|(k, _)| *k).collect();
        for id in ids {
            self.close(id);
        }
        self.stop_window(label);
    }

    fn close(&self, id: u64) {
        let Some(c) = self.conns.lock().unwrap().remove(&id) else { return };
        match c.out {
            Out::Local(lc) => {
                lc.close();
                let host = self.wins.lock().unwrap().get(&c.window).map(|w| w.host_conn == Some(id)).unwrap_or(false);
                if host {
                    self.stop_window(&c.window);
                }
            }
            Out::Net(tx) => drop(tx), // the socket task closes the WebSocket and ends
        }
    }

    fn info(&self, label: &str) -> SessionInfo {
        let wins = self.wins.lock().unwrap();
        match wins.get(label).and_then(|w| w.handle.as_ref().map(|h| (h.addr.port(), w.app.clone()))) {
            Some((port, app)) => SessionInfo { running: true, port, addrs: session_addrs(), app },
            None => SessionInfo { running: false, port: 0, addrs: vec![], app: String::new() },
        }
    }
}

#[derive(Serialize)]
pub struct SessionInfo {
    running: bool,
    port: u16,
    addrs: Vec<String>,
    app: String,
}

/// Where sessions listen: every interface (LAN joining needs it). Debug builds only: `CC_LIVE_BIND` (an IP, e.g.
/// 127.0.0.1) keeps test sessions off the network, like CC_SITE in account.rs.
#[cfg(debug_assertions)]
fn bind_ip() -> IpAddr {
    std::env::var("CC_LIVE_BIND").ok().and_then(|v| v.parse().ok()).unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
}
#[cfg(not(debug_assertions))]
fn bind_ip() -> IpAddr {
    IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
}

/// The addresses a joiner can reach this machine's sessions at: the LAN's, or the one address a debug build was bound to.
fn session_addrs() -> Vec<String> {
    let ip = bind_ip();
    if ip.is_unspecified() { lan_addrs() } else { vec![ip.to_string()] }
}

/// This machine's addresses for the QR and the advertisement: non-loopback IPv4 (no 169.254), private ranges first.
pub fn lan_addrs() -> Vec<String> {
    let mut v: Vec<std::net::Ipv4Addr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            IpAddr::V4(a) if !a.is_link_local() && !a.is_unspecified() => Some(a),
            _ => None,
        })
        .collect();
    v.sort_by_key(|a| (!a.is_private(), a.octets()));
    v.dedup();
    v.into_iter().map(|a| a.to_string()).collect()
}

/// The caller's window: its label; on a tablet (TB.5), the view inside Studio's one window that the bridge names
/// (`app-<slug>-<n>`, bridge.js views). Only there, and only from the main window: a desktop label is never overridden.
fn caller(w: &WebviewWindow, view: Option<&str>) -> String {
    #[cfg(mobile)]
    {
        if w.label() == "main" {
            if let Some(v) = view.filter(|v| v.starts_with("app-") && crate::windows::app_of(v).is_some()) {
                return v.to_string();
            }
        }
    }
    let _ = view;
    w.label().to_string()
}

/// A raw-body command's view (header `x-cc-view`).
fn view_header(r: &Request<'_>) -> Option<String> {
    r.headers().get("x-cc-view").and_then(|v| v.to_str().ok()).map(String::from)
}

fn suite_window(live: &StudioLive, w: &WebviewWindow, view: Option<&str>, hub_too: bool) -> Result<String, String> {
    if !live.on {
        return Err("off: not the suite".into());
    }
    let label = caller(w, view);
    let label = label.as_str();
    if label.starts_with("app-") || (hub_too && label == "main") {
        Ok(label.to_string())
    } else {
        Err("not_allowed: app windows only".into())
    }
}

/// `ws://<host>:<port>/live` (a host on the LAN), or the site's relay join URL for a room.
pub fn join_url_ok(url: &str, site: &str) -> bool {
    if let Some(rest) = url.strip_prefix("ws://") {
        let Some(hostport) = rest.strip_suffix("/live") else { return false };
        let Some((host, port)) = hostport.rsplit_once(':') else { return false };
        let port_ok = port.parse::<u16>().map(|p| p > 0).unwrap_or(false);
        let host_ok = !host.is_empty()
            && host.len() <= 64
            && (host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
                || (host.starts_with('[') && host.ends_with(']') && host[1..host.len() - 1].parse::<std::net::Ipv6Addr>().is_ok()));
        if port_ok && host_ok {
            return true;
        }
    }
    relay_url_ok(url, site, "join")
}

/// `<site as ws(s)>/live/relay/<kind>/<ROOM>` with an 8-character room id.
pub fn relay_url_ok(url: &str, site: &str, kind: &str) -> bool {
    let base = if let Some(h) = site.strip_prefix("https://") { format!("wss://{h}") } else if let Some(h) = site.strip_prefix("http://") { format!("ws://{h}") } else { return false };
    let Some(room) = url.strip_prefix(&format!("{base}/live/relay/{kind}/")) else { return false };
    room_ok(room)
}

fn room_ok(room: &str) -> bool {
    room.len() == 8 && room.bytes().all(|b| crypto::CODE_ALPHABET.contains(&b))
}

/// The instance name for a tag (a short code's 2 characters, or a room's 8-character tag) or `{room}`.
fn tag_of(tag: &Option<String>, room: &Option<String>) -> Result<String, String> {
    if let Some(r) = room {
        let r = r.to_ascii_uppercase();
        return if room_ok(&r) { Ok(crypto::room_tag(&r)) } else { Err("bad room".into()) };
    }
    let t = tag.as_deref().unwrap_or("").to_ascii_uppercase();
    if (t.len() == 2 || t.len() == 8) && t.bytes().all(|b| crypto::CODE_ALPHABET.contains(&b)) {
        Ok(t)
    } else {
        Err("bad tag".into())
    }
}

fn record(out: &mut Vec<u8>, kind: u8, payload: &[u8]) {
    out.push(kind);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
}

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

#[derive(Deserialize)]
pub struct OpenArgs {
    kind: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    app: String,
    #[serde(default)]
    schema: Option<u32>,
}

#[tauri::command]
pub async fn cc_live_open(
    window: WebviewWindow, view: Option<String>,
    live: State<'_, StudioLive>,
    acct: State<'_, crate::account::Account>,
    args: OpenArgs,
) -> Result<u64, String> {
    let label = suite_window(&live, &window, view.as_deref(), false)?;
    let own_app = crate::windows::app_of(&label).unwrap_or_default();
    let id = live.next.fetch_add(1, Ordering::Relaxed);
    let (in_tx, in_rx) = mpsc::unbounded_channel::<In>();
    let out = match args.kind.as_str() {
        "local" => {
            if !args.app.is_empty() && args.app != own_app {
                return Err("not_allowed: another app's session".into());
            }
            let handle = {
                let mut wins = live.wins.lock().unwrap();
                let w = wins.entry(label.clone()).or_default();
                if w.host_conn.is_some_and(|c| live.conns.lock().unwrap().contains_key(&c)) {
                    return Err("busy: this window already has a session".into());
                }
                if w.handle.is_none() {
                    let cfg = live::Config { app: own_app.clone(), schema: args.schema.unwrap_or(1), ..Default::default() };
                    let bind = std::net::SocketAddr::new(bind_ip(), 0);
                    let h = live::start(cfg, bind, live::Serve { www: Some(live.www.clone()) }).map_err(|e| format!("error: {e}"))?;
                    w.handle = Some(h);
                    w.app = own_app.clone();
                }
                w.host_conn = Some(id);
                w.handle.clone().unwrap()
            };
            let mut lc = handle.attach_local();
            let (_keep, dummy) = mpsc::unbounded_channel();
            let mut rx = std::mem::replace(&mut lc.rx, dummy);
            let _ = in_tx.send(In::Open);
            tauri::async_runtime::spawn(async move {
                while let Some(w) = rx.recv().await {
                    let more = match w {
                        live::Wire::Bytes(b) => in_tx.send(In::Msg(b)).is_ok(),
                        live::Wire::Close(why) => {
                            let _ = in_tx.send(In::Close(why, 0));
                            false
                        }
                    };
                    if !more {
                        break;
                    }
                }
            });
            Out::Local(lc)
        }
        "join" => {
            if !join_url_ok(&args.url, acct.site()) {
                return Err("not_allowed: not a live session's address".into());
            }
            let (out_tx, out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
            tauri::async_runtime::spawn(socket(args.url.clone(), in_tx, out_rx));
            Out::Net(out_tx)
        }
        _ => return Err("error: kind is local or join".into()),
    };
    live.conns.lock().unwrap().insert(id, Conn { window: label, out, rx: Arc::new(tokio::sync::Mutex::new(in_rx)) });
    Ok(id)
}

/// A joiner's WebSocket: frames through untouched, the relay's text dropped.
async fn socket(url: String, tx: mpsc::UnboundedSender<In>, mut out: mpsc::UnboundedReceiver<Vec<u8>>) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let ws = match tokio::time::timeout(Duration::from_secs(10), crate::ws_connect(url.as_str())).await {
        Ok(Ok((ws, _))) => ws,
        Ok(Err(e)) => return drop(tx.send(In::Close(format!("connect: {e}"), 0))),
        Err(_) => return drop(tx.send(In::Close("connect: timed out".into(), 0))),
    };
    if let Ok(s) = ws.get_ref().get_ref_tcp() {
        let _ = s.set_nodelay(true);
    }
    let _ = tx.send(In::Open);
    let (mut sink, mut source) = ws.split();
    loop {
        tokio::select! {
            m = out.recv() => match m {
                Some(b) => if sink.send(Message::Binary(b.into())).await.is_err() { break },
                None => { let _ = sink.close().await; return; }   // the page closed it
            },
            msg = source.next() => match msg {
                Some(Ok(Message::Binary(b))) => { if tx.send(In::Msg(b.to_vec())).is_err() { break } }
                Some(Ok(Message::Close(f))) => {
                    let (code, reason) = f.map(|f| (u16::from(f.code), f.reason.to_string())).unwrap_or((1005, String::new()));
                    let _ = tx.send(In::Close(reason, code));
                    return;
                }
                Some(Ok(_)) => {}   // text (the relay's {"t":"ready"}), pings: not the protocol's
                Some(Err(e)) => { let _ = tx.send(In::Close(e.to_string(), 0)); return; }
                None => break,
            },
        }
    }
    let _ = tx.send(In::Close("closed".into(), 1006));
}

trait TcpRef {
    fn get_ref_tcp(&self) -> Result<&tokio::net::TcpStream, ()>;
}
impl TcpRef for tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream> {
    fn get_ref_tcp(&self) -> Result<&tokio::net::TcpStream, ()> {
        match self {
            tokio_tungstenite::MaybeTlsStream::Plain(s) => Ok(s),
            _ => Err(()),
        }
    }
}

/// Long poll: the next records of a connection (at least one), then whatever else is already queued.
#[tauri::command]
pub async fn cc_live_recv(window: WebviewWindow, view: Option<String>, live: State<'_, StudioLive>, id: u64) -> Result<Response, String> {
    let rx = {
        let conns = live.conns.lock().unwrap();
        match conns.get(&id) {
            Some(c) if c.window == caller(&window, view.as_deref()) => Some(c.rx.clone()),
            _ => None,
        }
    };
    let mut out = vec![];
    let Some(rx) = rx else {
        push_in(&mut out, In::Close("gone".into(), 0));
        return Ok(Response::new(out));
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
    Ok(Response::new(out))
}

/// Body: one or more envelopes / frames, each u32 length (big-endian) ‖ bytes. Header `x-cc-live` = the connection id.
#[tauri::command]
pub async fn cc_live_send(window: WebviewWindow, live: State<'_, StudioLive>, request: Request<'_>) -> Result<(), String> {
    let id: u64 = request.headers().get("x-cc-live").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).ok_or("missing id")?;
    let body = &crate::files::body_bytes(request.body()).ok_or("expected bytes")?;
    let conns = live.conns.lock().unwrap();
    let me = caller(&window, view_header(&request).as_deref());
    let c = conns.get(&id).filter(|c| c.window == me).ok_or("closed: no such connection")?;
    let mut p = 0;
    while p + 4 <= body.len() {
        let n = u32::from_be_bytes([body[p], body[p + 1], body[p + 2], body[p + 3]]) as usize;
        p += 4;
        if p + n > body.len() {
            return Err("error: truncated batch".into());
        }
        let b = body[p..p + n].to_vec();
        p += n;
        match &c.out {
            Out::Local(lc) => lc.send_bytes(b),
            Out::Net(tx) => {
                let _ = tx.send(b);
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn cc_live_close(window: WebviewWindow, view: Option<String>, live: State<'_, StudioLive>, id: u64) {
    let me = caller(&window, view.as_deref());
    let mine = live.conns.lock().unwrap().get(&id).map(|c| c.window == me).unwrap_or(false);
    if mine {
        live.close(id);
    }
}

#[tauri::command]
pub fn cc_live_session(window: WebviewWindow, view: Option<String>, live: State<'_, StudioLive>) -> Result<SessionInfo, String> {
    let label = suite_window(&live, &window, view.as_deref(), false)?;
    Ok(live.info(&label))
}

#[derive(Deserialize)]
pub struct AdArgs {
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    room: Option<String>,
    #[serde(default)]
    app: String,
    #[serde(default)]
    ms: Option<u64>,
}

#[tauri::command(async)]
pub fn cc_live_advertise(window: WebviewWindow, view: Option<String>, live: State<'_, StudioLive>, args: AdArgs) -> Result<(), String> {
    let label = suite_window(&live, &window, view.as_deref(), false)?;
    let tag = tag_of(&args.tag, &args.room)?;
    let (port, app) = {
        let wins = live.wins.lock().unwrap();
        let w = wins.get(&label).filter(|w| w.handle.is_some()).ok_or("error: no session in this window")?;
        (w.handle.as_ref().unwrap().addr.port(), w.app.clone())
    };
    if !args.app.is_empty() && args.app != app {
        return Err("not_allowed: another app".into());
    }
    // iPad (TB.5): raw multicast needs an entitlement Apple grants case by case, so Bonjour (the system's own) answers.
    #[cfg(target_os = "ios")]
    {
        crate::mobile::bonjour_advertise(&tag, port, &app)?;
        let full = format!("{tag}.{SERVICE}");
        if let Some(w) = live.wins.lock().unwrap().get_mut(&label) {
            if !w.ads.contains(&full) {
                w.ads.push(full);
            }
        }
        return Ok(());
    }
    #[cfg(target_os = "android")]
    crate::mobile::multicast(true);
    let addrs = lan_addrs();
    if addrs.is_empty() {
        return Err("offline: no network address".into());
    }
    // A host name that says nothing about the machine or the person.
    let mut r = [0u8; 4];
    getrandom::fill(&mut r).map_err(|e| e.to_string())?;
    let host = format!("ccstudio-{}.local.", r.iter().map(|x| format!("{x:02x}")).collect::<String>());
    let info = ServiceInfo::new(SERVICE, &tag, &host, addrs.join(",").as_str(), port, &[("app", app.as_str())][..]).map_err(|e| format!("mdns: {e}"))?;
    let full = info.get_fullname().to_string();
    let d = live.daemon()?;
    d.register(info).map_err(|e| format!("mdns: {e}"))?;
    let mut wins = live.wins.lock().unwrap();
    if let Some(w) = wins.get_mut(&label) {
        if !w.ads.contains(&full) {
            w.ads.push(full);
        }
    }
    Ok(())
}

#[tauri::command(async)]
pub fn cc_live_unadvertise(window: WebviewWindow, view: Option<String>, live: State<'_, StudioLive>, args: AdArgs) -> Result<(), String> {
    let label = suite_window(&live, &window, view.as_deref(), false)?;
    let tag = tag_of(&args.tag, &args.room)?;
    let full = format!("{tag}.{SERVICE}");
    let had = live.wins.lock().unwrap().get_mut(&label).map(|w| {
        let n = w.ads.len();
        w.ads.retain(|x| !x.eq_ignore_ascii_case(&full));
        n != w.ads.len()
    });
    if had == Some(true) {
        #[cfg(target_os = "ios")]
        crate::mobile::bonjour_stop(&tag);
        #[cfg(not(target_os = "ios"))]
        if let Ok(d) = live.daemon_if_running() {
            let _ = d.unregister(&full);
        }
    }
    Ok(())
}

#[derive(Serialize, Debug)]
pub struct Found {
    addr: String,
    port: u16,
    app: String,
}

/// Looks for one instance of `_ccstudio._tcp` for at most `ms` (≤ 5 s); answers as soon as it resolves.
#[tauri::command(async)]
pub fn cc_live_browse(window: WebviewWindow, view: Option<String>, live: State<'_, StudioLive>, args: AdArgs) -> Result<Vec<Found>, String> {
    suite_window(&live, &window, view.as_deref(), true)?;
    let tag = tag_of(&args.tag, &args.room)?;
    let wait = Duration::from_millis(args.ms.unwrap_or(1500).clamp(100, 5000));
    #[cfg(target_os = "ios")]
    return Ok(crate::mobile::bonjour_browse(&tag, wait.as_millis() as u64)?
        .into_iter()
        .map(|(addr, port, app)| Found { addr, port, app })
        .collect());
    #[cfg(not(target_os = "ios"))]
    {
        #[cfg(target_os = "android")]
        crate::mobile::multicast(true);
        let d = live.daemon()?;
        browse(&d, &tag, wait)
    }
}

pub fn browse(d: &ServiceDaemon, tag: &str, wait: Duration) -> Result<Vec<Found>, String> {
    let want = format!("{tag}.{SERVICE}");
    let rx = d.browse(SERVICE).map_err(|e| format!("mdns: {e}"))?;
    let until = Instant::now() + wait;
    let mut found = vec![];
    while found.is_empty() {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok(ServiceEvent::ServiceResolved(s)) if s.fullname.eq_ignore_ascii_case(&want) => {
                let app = s.txt_properties.get_property_val_str("app").unwrap_or("").to_string();
                let mut ips: Vec<IpAddr> = s.addresses.iter().map(|a| a.to_ip_addr()).collect();
                ips.sort_by_key(|ip| (!ip.is_ipv4(), matches!(ip, IpAddr::V4(a) if !a.is_private()), *ip));
                for ip in ips {
                    if let IpAddr::V6(a) = ip {
                        if (a.segments()[0] & 0xffc0) == 0xfe80 {
                            continue; // link-local needs a zone the page can't give
                        }
                    }
                    found.push(Found { addr: ip.to_string(), port: s.port, app: app.clone() });
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = d.stop_browse(SERVICE);
    Ok(found)
}

#[derive(Deserialize)]
pub struct RelayArgs {
    url: String,
    ticket: String,
}

#[tauri::command(async)]
pub fn cc_live_relay(
    window: WebviewWindow, view: Option<String>,
    live: State<'_, StudioLive>,
    acct: State<'_, crate::account::Account>,
    args: RelayArgs,
) -> Result<(), String> {
    let label = suite_window(&live, &window, view.as_deref(), false)?;
    if !relay_url_ok(&args.url, acct.site(), "host") {
        return Err("not_allowed: not the site's relay".into());
    }
    if args.ticket.is_empty() || args.ticket.len() > 256 {
        return Err("error: bad ticket".into());
    }
    let mut wins = live.wins.lock().unwrap();
    let w = wins.get_mut(&label).filter(|w| w.handle.is_some()).ok_or("error: no session in this window")?;
    if let Some(old) = w.relay.take() {
        old.stop();
    }
    w.relay = Some(w.handle.as_ref().unwrap().relay(&args.url, &args.ticket));
    Ok(())
}

#[tauri::command]
pub fn cc_live_relay_stop(window: WebviewWindow, view: Option<String>, live: State<'_, StudioLive>) -> Result<(), String> {
    let label = suite_window(&live, &window, view.as_deref(), false)?;
    if let Some(r) = live.wins.lock().unwrap().get_mut(&label).and_then(|w| w.relay.take()) {
        r.stop();
    }
    Ok(())
}

#[tauri::command]
pub async fn cc_live_export(window: WebviewWindow, view: Option<String>, live: State<'_, StudioLive>) -> Result<Response, String> {
    let label = suite_window(&live, &window, view.as_deref(), false)?;
    let h = live.wins.lock().unwrap().get(&label).and_then(|w| w.handle.clone()).ok_or("error: no session in this window")?;
    let st = h.export_state().await.ok_or("error: the session has stopped")?;
    let mut out = vec![];
    rmpv::encode::write_value(&mut out, &st).map_err(|e| e.to_string())?;
    Ok(Response::new(out))
}

/// The host handed the session to this window: a session server carrying on from the state (`live::start_from`).
#[tauri::command(async)]
pub fn cc_live_start_from(window: WebviewWindow, live: State<'_, StudioLive>, request: Request<'_>) -> Result<SessionInfo, String> {
    let label = suite_window(&live, &window, view_header(&request).as_deref(), false)?;
    let own_app = crate::windows::app_of(&label).unwrap_or_default();
    let body = &crate::files::body_bytes(request.body()).ok_or("expected bytes")?;
    let state = rmpv::decode::read_value(&mut &body[..]).map_err(|_| "error: not a session state".to_string())?;
    let field = |k: &str| state.as_map().and_then(|m| m.iter().find(|(x, _)| x.as_str() == Some(k)).map(|(_, v)| v.clone()));
    let app = field("app").and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
    let schema = field("schema").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
    if app != own_app {
        return Err("not_allowed: another app's session".into());
    }
    let busy = live.wins.lock().unwrap().get(&label).is_some_and(|w| w.handle.is_some());
    if busy {
        return Err("busy: this window already has a session".into());
    }
    let cfg = live::Config { app: own_app.clone(), schema, ..Default::default() };
    let h = live::start_from(cfg, std::net::SocketAddr::new(bind_ip(), 0), live::Serve { www: Some(live.www.clone()) }, state).map_err(|e| format!("error: {e}"))?;
    {
        let mut wins = live.wins.lock().unwrap();
        let w = wins.entry(label.clone()).or_default();
        w.handle = Some(h);
        w.app = own_app;
    }
    Ok(live.info(&label))
}

/// main.rs: every window's end goes through here.
/// Tablets (TB.5): the bridge closed a view (bridge.js); its session ends as a closed window's does.
#[cfg(mobile)]
#[tauri::command]
pub fn cc_live_view_closed(app: AppHandle, view: String) {
    if view.starts_with("app-") && crate::windows::app_of(&view).is_some() {
        on_window_destroyed(&app, &view);
    }
}

pub fn on_window_destroyed<R: Runtime>(app: &AppHandle<R>, label: &str) {
    if let Some(l) = app.try_state::<StudioLive>() {
        l.window_gone(label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_and_tags() {
        let site = "https://getcadcraft.com";
        assert!(join_url_ok("ws://192.168.1.20:51234/live", site));
        assert!(join_url_ok("ws://[fd00::1]:4000/live", site));
        assert!(join_url_ok("ws://studio-pc.local:4000/live", site));
        assert!(join_url_ok("wss://getcadcraft.com/live/relay/join/K7QX9M2B", site));
        assert!(!join_url_ok("wss://evil.example/live/relay/join/K7QX9M2B", site));
        assert!(!join_url_ok("ws://192.168.1.20:51234/other", site));
        assert!(!join_url_ok("ws://192.168.1.20/live", site));
        assert!(!join_url_ok("ws://a b:1/live", site));
        assert!(!join_url_ok("wss://getcadcraft.com/live/relay/join/K7QX9M2", site));
        assert!(relay_url_ok("wss://getcadcraft.com/live/relay/host/K7QX9M2B", site, "host"));
        assert!(!relay_url_ok("wss://getcadcraft.com/live/relay/join/K7QX9M2B", site, "host"));
        assert!(relay_url_ok("ws://127.0.0.1:9000/live/relay/host/K7QX9M2B", "http://127.0.0.1:9000", "host"));
        assert_eq!(tag_of(&Some("k7".into()), &None).unwrap(), "K7");
        assert!(tag_of(&Some("K7-Q4".into()), &None).is_err());
        assert_eq!(tag_of(&None, &Some("K7QX9M2B".into())).unwrap(), crypto::room_tag("K7QX9M2B"));
        assert!(tag_of(&None, &Some("../x".into())).is_err());
        let a = lan_addrs();
        assert!(a.iter().all(|x| x.parse::<std::net::Ipv4Addr>().map(|i| !i.is_loopback()).unwrap_or(false)));
    }

    #[test]
    fn records() {
        let mut out = vec![];
        assert!(push_in(&mut out, In::Open));
        assert!(push_in(&mut out, In::Msg(vec![1, 2, 3])));
        assert!(!push_in(&mut out, In::Close("bye".into(), 4004)));
        assert_eq!(&out[..5], &[0, 0, 0, 0, 0]);
        assert_eq!(&out[5..13], &[1, 0, 0, 0, 3, 1, 2, 3]);
        assert_eq!(out[13], 2);
        assert!(String::from_utf8_lossy(&out[18..]).contains("4004"));
    }

    /// Needs multicast on a network interface (skipped where there is none): advertise a tag, browse finds it.
    #[test]
    #[ignore]
    fn mdns_advertise_then_browse() {
        let addrs = lan_addrs();
        if addrs.is_empty() {
            return;
        }
        let d = ServiceDaemon::new().unwrap();
        let info = ServiceInfo::new(SERVICE, "Q4", "ccstudio-test.local.", addrs.join(",").as_str(), 4711, &[("app", "cadcraft")][..]).unwrap();
        d.register(info).unwrap();
        let found = browse(&d, "Q4", Duration::from_secs(3)).unwrap();
        assert!(found.iter().any(|f| f.port == 4711 && f.app == "cadcraft"), "{found:?}");
        let _ = d.shutdown();
    }
}
