// SPDX-License-Identifier: Apache-2.0
//! The session server's transport: WebSocket over TCP (`TCP_NODELAY`), one task per connection that only moves bytes,
//! and one task that owns the `Session`, the clock and every connection's frame keys (open on the way in, seal on the
//! way out). Runs on its own thread with its own tokio runtime, so the host's UI never waits on it (spec § 11). The
//! host's app window and the replica attach in-process (`attach_local`) and exchange envelopes without frames.

use super::crypto;
use super::http;
use super::msg::Envelope;
use super::session::{Config, ConnId, Out, Session, Stats};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

/// Largest frame a LAN peer may send (an edit is at most 1 MB, spec § 11; plus envelope and frame overhead).
pub const MAX_FRAME: usize = 1024 * 1024 + 64 * 1024;
/// Loopback peers (a test harness's replica sends whole snapshots).
pub const MAX_FRAME_LOCAL: usize = 256 * 1024 * 1024;

pub(super) enum Event {
    /// A connection: its writer, whether it is in-process (trusted), and how it came (`local`, `lan`, `relay`).
    Open(ConnId, mpsc::UnboundedSender<Wire>, bool, &'static str),
    /// Something for the host's window (the relay's state).
    Notice(Envelope),
    /// Bytes from a network connection: a frame (or a plain envelope before the handshake ends).
    Frame(ConnId, Vec<u8>),
    /// An envelope from an in-process client.
    Local(ConnId, Vec<u8>),
    Closed(ConnId),
    Stats(oneshot::Sender<(Stats, u64, usize)>),
    Export(oneshot::Sender<rmpv::Value>),
    Stop,
}

/// To a connection's writer (network) or to an in-process client.
pub enum Wire {
    /// Network: frame bytes as sent. In-process: one encoded envelope.
    Bytes(Vec<u8>),
    Close(String),
}

/// Where to listen and what else to serve on the same port.
#[derive(Clone, Default)]
pub struct Serve {
    /// The browser edition's folder (LV.12): `http://<host>:<port>/…` serves it, so browser joiners share the socket's
    /// origin. None = only the socket.
    pub www: Option<std::path::PathBuf>,
}

#[derive(Clone)]
pub struct LiveHandle {
    pub addr: SocketAddr,
    pub session_id: String,
    tx: mpsc::UnboundedSender<Event>,
    next_conn: Arc<AtomicU64>,
    serve: Arc<Serve>,
    rt: tokio::runtime::Handle,
}

/// An in-process client (the host's window, the replica): envelopes in and out, no frames.
pub struct LocalClient {
    pub conn: ConnId,
    tx: mpsc::UnboundedSender<Event>,
    pub rx: mpsc::UnboundedReceiver<Wire>,
}

impl LocalClient {
    pub fn send(&self, env: &Envelope) {
        let _ = self.tx.send(Event::Local(self.conn, env.encode()));
    }
    /// One encoded envelope (as the window's JS produced it).
    pub fn send_bytes(&self, b: Vec<u8>) {
        let _ = self.tx.send(Event::Local(self.conn, b));
    }
    pub fn close(&self) {
        let _ = self.tx.send(Event::Closed(self.conn));
    }
}

impl LiveHandle {
    pub fn attach_local(&self) -> LocalClient {
        let conn = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let (wtx, wrx) = mpsc::unbounded_channel();
        let _ = self.tx.send(Event::Open(conn, wtx, true, "local"));
        LocalClient { conn, tx: self.tx.clone(), rx: wrx }
    }
    /// (stats, seq, connections) — for tests and the host's status line.
    pub fn stats_blocking(&self) -> Option<(Stats, u64, usize)> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Event::Stats(tx)).ok()?;
        rx.blocking_recv().ok()
    }
    pub async fn stats(&self) -> Option<(Stats, u64, usize)> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Event::Stats(tx)).ok()?;
        rx.await.ok()
    }
    pub fn stop(&self) {
        let _ = self.tx.send(Event::Stop);
    }
    pub(super) fn tx(&self) -> &mpsc::UnboundedSender<Event> {
        &self.tx
    }
    pub(super) fn next_conn(&self) -> &AtomicU64 {
        &self.next_conn
    }
    pub(super) fn runtime(&self) -> &tokio::runtime::Handle {
        &self.rt
    }
    /// The session's state for a host handoff (spec § 10): `Session::export()`. The new host passes it to `start_from`.
    pub async fn export_state(&self) -> Option<rmpv::Value> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Event::Export(tx)).ok()?;
        rx.await.ok()
    }
}

/// Starts a session server on its own thread. `bind` 0.0.0.0:0 picks a free port (the address is in the handle).
pub fn start(cfg: Config, bind: SocketAddr, serve: Serve) -> std::io::Result<LiveHandle> {
    start_with(cfg, bind, serve, None)
}

/// A session server carrying on from another host's `export_state()` (host handoff): the same log, locks and people,
/// who rejoin with their resume tokens.
pub fn start_from(cfg: Config, bind: SocketAddr, serve: Serve, state: rmpv::Value) -> std::io::Result<LiveHandle> {
    start_with(cfg, bind, serve, Some(state))
}

fn start_with(cfg: Config, bind: SocketAddr, serve: Serve, state: Option<rmpv::Value>) -> std::io::Result<LiveHandle> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("cc-live".into()).spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        rt.block_on(async move {
            let listener = match TcpListener::bind(bind).await {
                Ok(l) => l,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let addr = listener.local_addr().unwrap();
            let (tx, rx) = mpsc::unbounded_channel();
            let session = match state {
                None => Session::new(cfg),
                Some(st) => match Session::import(cfg, &st, 0) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = ready_tx.send(Err(std::io::Error::other(e)));
                        return;
                    }
                },
            };
            let handle = LiveHandle {
                addr,
                session_id: session.id.clone(),
                tx: tx.clone(),
                next_conn: Arc::new(AtomicU64::new(1)),
                serve: Arc::new(serve),
                rt: tokio::runtime::Handle::current(),
            };
            let _ = ready_tx.send(Ok(handle.clone()));
            let accept = tokio::spawn(accept_loop(listener, handle.clone()));
            run_session(session, rx).await;
            accept.abort();
        });
    })?;
    ready_rx.recv().map_err(|_| std::io::Error::other("live server thread died"))?
}

async fn accept_loop(listener: TcpListener, h: LiveHandle) {
    loop {
        let Ok((stream, peer)) = listener.accept().await else { continue };
        let h = h.clone();
        tokio::spawn(async move {
            let _ = serve_conn(stream, peer, h).await;
        });
    }
}

async fn serve_conn(mut stream: TcpStream, peer: SocketAddr, h: LiveHandle) -> Result<(), String> {
    stream.set_nodelay(true).map_err(|e| e.to_string())?;
    let head = http::read_head(&mut stream).await.ok_or("no request")?;
    if !(head.upgrade_ws && head.path == "/live") {
        http::serve_file(&mut stream, &head, h.serve.www.as_deref()).await;
        return Ok(());
    }
    let stream = http::Replay::new(head.raw, stream);
    let local = peer.ip().is_loopback();
    let mut wcfg = WebSocketConfig::default();
    wcfg.max_message_size = Some(if local { MAX_FRAME_LOCAL } else { MAX_FRAME });
    wcfg.max_frame_size = wcfg.max_message_size;
    let check = |req: &Request, resp: Response| {
        if req.uri().path() == "/live" {
            Ok(resp)
        } else {
            let mut r = tokio_tungstenite::tungstenite::http::Response::new(Some("not found".into()));
            *r.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::NOT_FOUND;
            Err(r)
        }
    };
    let ws = tokio_tungstenite::accept_hdr_async_with_config(stream, check, Some(wcfg)).await.map_err(|e| e.to_string())?;
    let (mut sink, mut source) = ws.split();
    let conn = h.next_conn.fetch_add(1, Ordering::Relaxed);
    let (wtx, mut wrx) = mpsc::unbounded_channel::<Wire>();
    let _ = h.tx.send(Event::Open(conn, wtx, false, "lan"));
    let writer = tokio::spawn(async move {
        while let Some(w) = wrx.recv().await {
            match w {
                Wire::Bytes(b) => {
                    if sink.send(Message::Binary(b.into())).await.is_err() {
                        break;
                    }
                }
                Wire::Close(_) => {
                    let _ = sink.close().await;
                    break;
                }
            }
        }
    });
    while let Some(msg) = source.next().await {
        match msg {
            Ok(Message::Binary(b)) => {
                if h.tx.send(Event::Frame(conn, b.to_vec())).is_err() {
                    break;
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {} // text frames are not part of the protocol; WebSocket pings are answered by tungstenite
        }
    }
    let _ = h.tx.send(Event::Closed(conn));
    writer.abort();
    Ok(())
}

struct ConnIo {
    w: mpsc::UnboundedSender<Wire>,
    local: bool,
    keys: Option<crypto::Session>,
}

async fn run_session(mut s: Session, mut rx: mpsc::UnboundedReceiver<Event>) {
    let t0 = Instant::now();
    let now = || t0.elapsed().as_millis() as u64;
    let mut conns: HashMap<ConnId, ConnIo> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_millis(8));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let out = tokio::select! {
            ev = rx.recv() => match ev {
                None | Some(Event::Stop) => break,
                Some(Event::Open(c, w, local, via)) => {
                    conns.insert(c, ConnIo { w, local, keys: None });
                    s.open(c, local, via, now());
                    vec![]
                }
                Some(Event::Notice(env)) => s.notice(env),
                Some(Event::Frame(c, b)) | Some(Event::Local(c, b)) => {
                    // Network frames are opened with the connection's keys once the handshake is done.
                    let body = match conns.get_mut(&c) {
                        None => continue,
                        Some(io) => match io.keys.as_mut() {
                            None => Ok(Some(b)),
                            Some(k) => match k.open(&b) {
                                Ok(crypto::Opened::Data(body)) => Ok(Some(body)),
                                Ok(crypto::Opened::Rekeyed) => Ok(None), // joiners never send these; open refuses them
                                Err(e) => Err(e.to_string()),
                            },
                        },
                    };
                    match body.map(|b| b.map(|b| Envelope::decode(&b))) {
                        Ok(None) => vec![],
                        Ok(Some(Ok(env))) => s.frame(c, env, now()),
                        Ok(Some(Err(why))) | Err(why) => {
                            if let Some(io) = conns.remove(&c) { let _ = io.w.send(Wire::Close(why)); }
                            s.closed(c, now())
                        }
                    }
                }
                Some(Event::Closed(c)) => { conns.remove(&c); s.closed(c, now()) }
                Some(Event::Stats(reply)) => { let _ = reply.send((s.stats.clone(), s.seq(), conns.len())); vec![] }
                Some(Event::Export(reply)) => { let _ = reply.send(s.export()); vec![] }
            },
            _ = tick.tick() => s.tick(now()),
        };
        for o in out {
            match o {
                Out::To(c, env) => {
                    let Some(io) = conns.get_mut(&c) else { continue };
                    let bytes = env.encode();
                    let frame = match io.keys.as_mut() {
                        None => Ok(bytes),
                        Some(k) => k.seal(&bytes),
                    };
                    match frame {
                        Ok(f) => {
                            let _ = io.w.send(Wire::Bytes(f));
                        }
                        Err(e) => {
                            // A direction ran out of keys (255 rotations / 2^63 frames): the joiner must join again.
                            let _ = io.w.send(Wire::Close(e.to_string()));
                        }
                    }
                }
                Out::Secure(c, keys) => {
                    if let Some(io) = conns.get_mut(&c) {
                        if !io.local {
                            io.keys = Some(*keys);
                        }
                    }
                }
                Out::Rekey(c) => {
                    if let Some(io) = conns.get_mut(&c) {
                        if let Some(k) = io.keys.as_mut() {
                            match k.rekey() {
                                Ok(f) => {
                                    let _ = io.w.send(Wire::Bytes(f));
                                }
                                Err(e) => {
                                    let _ = io.w.send(Wire::Close(e.to_string()));
                                }
                            }
                        }
                    }
                }
                Out::Close(c, why) => {
                    if let Some(io) = conns.remove(&c) {
                        let _ = io.w.send(Wire::Close(why));
                    }
                }
            }
        }
    }
}
