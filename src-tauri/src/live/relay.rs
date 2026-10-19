// SPDX-License-Identifier: Apache-2.0
//! The host's side of the relay (spec § 9; wire format docs/LIVE-RELAY.md, session live-relay's service): Studio opens
//! ONE outbound WebSocket per invite-link room, `wss://getcadcraft.com/live/relay/host/<ROOM>`, so hosting works behind
//! any router. Each joiner the relay announces becomes a connection of the session like a LAN socket (its own handshake
//! with context `relay:<ROOM>`, its own frame keys); `via` is `relay`. The relay sees only handshake messages and
//! ciphertext.
//!
//! Binary messages are `kind (1) ‖ joiner id (u32 BE) ‖ payload`: 0 a frame, 1 joined, 2 left (host → relay: remove).
//! Text: `{"t":"host","ticket"}` first, then `ready` / `license` / `closed` from the relay and `{"t":"end"}` from us. The
//! relay socket dropping (restart, network) means every joiner must rejoin (LV.10): we reconnect with the same ticket.
//! The host window hears about the relay through `relay` envelopes (`{state: ready | license | closed | reconnecting |
//! stopped, …}`).

use super::msg::{map, Envelope};
use super::net::{Event, LiveHandle, Wire};
use futures_util::{SinkExt, StreamExt};
use rmpv::Value;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;

/// Close codes after which reconnecting is pointless (docs/LIVE-RELAY.md § Close codes).
const FINAL_CODES: &[u16] = &[4004, 4005, 4401];

pub struct RelayHandle {
    stop: watch::Sender<bool>,
}

impl RelayHandle {
    /// Ends the room on the relay (`{"t":"end"}`) and closes the host socket; joiners through it are told and leave.
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }
}

impl LiveHandle {
    /// Hosts this session through the relay: `host_url` = `room.relay_url` from `POST /api/v1/live/rooms`
    /// (…/live/relay/host/<ROOM>), `ticket` = its `host_ticket`. The link's secret is registered separately with a
    /// `key {kid: ROOM, secret, context: "relay:<ROOM>"}` from the host's window.
    pub fn relay(&self, host_url: &str, ticket: &str) -> RelayHandle {
        let (stop, rx) = watch::channel(false);
        let h = self.clone();
        let url = host_url.to_string();
        let ticket = ticket.to_string();
        self.runtime().spawn(async move { run(h, url, ticket, rx).await });
        RelayHandle { stop }
    }
}

fn notice(h: &LiveHandle, pairs: Vec<(&str, Value)>) {
    let _ = h.tx().send(Event::Notice(Envelope::new("relay").body(map(pairs))));
}

async fn run(h: LiveHandle, url: String, ticket: String, mut stop: watch::Receiver<bool>) {
    let mut wait = Duration::from_millis(500);
    loop {
        if *stop.borrow() {
            break;
        }
        match once(&h, &url, &ticket, &mut stop).await {
            Ok(Done::Stopped) => {
                notice(&h, vec![("state", "stopped".into())]);
                return;
            }
            Ok(Done::Final(code, reason)) => {
                notice(&h, vec![("state", "closed".into()), ("code", code.into()), ("reason", reason.into())]);
                return;
            }
            Ok(Done::Dropped) => wait = Duration::from_millis(500),
            Err(e) => notice(&h, vec![("state", "reconnecting".into()), ("error", e.into())]),
        }
        notice(&h, vec![("state", "reconnecting".into())]);
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = stop.changed() => {}
        }
        wait = (wait * 2).min(Duration::from_secs(30));
    }
    notice(&h, vec![("state", "stopped".into())]);
}

enum Done {
    Stopped,
    Final(u16, String),
    Dropped,
}

async fn once(h: &LiveHandle, url: &str, ticket: &str, stop: &mut watch::Receiver<bool>) -> Result<Done, String> {
    let (ws, _) = super::ws::ws_connect(url).await.map_err(|e| e.to_string())?;
    let (mut sink, mut source) = ws.split();
    let hello = format!("{{\"t\":\"host\",\"ticket\":{}}}", json_str(ticket));
    sink.send(Message::Text(hello.into())).await.map_err(|e| e.to_string())?;
    // One writer for the socket: frames for every joiner funnel into it.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            let last = matches!(m, Message::Close(_));
            if sink.send(m).await.is_err() || last {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let mut joiners: HashMap<u32, u64> = HashMap::new();
    let mut ended: Option<Done> = None;
    loop {
        tokio::select! {
            _ = stop.changed() => {
                if *stop.borrow() {
                    let _ = out_tx.send(Message::Text("{\"t\":\"end\"}".into()));
                    let _ = out_tx.send(Message::Close(None));
                    ended = Some(Done::Stopped);
                    break;
                }
            }
            msg = source.next() => {
                let Some(msg) = msg else { break };
                match msg {
                    Ok(Message::Binary(b)) if b.len() >= 5 => {
                        let kind = b[0];
                        let cid = u32::from_be_bytes([b[1], b[2], b[3], b[4]]);
                        match kind {
                            1 => {
                                let conn = h.next_conn().fetch_add(1, Ordering::Relaxed);
                                let (wtx, mut wrx) = mpsc::unbounded_channel::<Wire>();
                                let out = out_tx.clone();
                                // The session's frames for this joiner, wrapped for the relay.
                                tokio::spawn(async move {
                                    while let Some(w) = wrx.recv().await {
                                        match w {
                                            Wire::Bytes(f) => {
                                                let mut m = Vec::with_capacity(5 + f.len());
                                                m.push(0);
                                                m.extend_from_slice(&cid.to_be_bytes());
                                                m.extend_from_slice(&f);
                                                if out.send(Message::Binary(m.into())).is_err() { break; }
                                            }
                                            Wire::Close(_) => {
                                                let mut m = vec![2u8];
                                                m.extend_from_slice(&cid.to_be_bytes());
                                                let _ = out.send(Message::Binary(m.into()));
                                                break;
                                            }
                                        }
                                    }
                                });
                                joiners.insert(cid, conn);
                                let _ = h.tx().send(Event::Open(conn, wtx, false, "relay"));
                            }
                            0 => {
                                if let Some(conn) = joiners.get(&cid) {
                                    let _ = h.tx().send(Event::Frame(*conn, b[5..].to_vec()));
                                }
                            }
                            2 => {
                                if let Some(conn) = joiners.remove(&cid) {
                                    let _ = h.tx().send(Event::Closed(conn));
                                }
                            }
                            _ => {}
                        }
                    }
                    Ok(Message::Text(t)) => {
                        let t = t.to_string();
                        let field = |k: &str| json_field(&t, k);
                        match field("t").as_deref() {
                            Some("ready") => notice(h, vec![
                                ("state", "ready".into()),
                                ("room", field("room").unwrap_or_default().into()),
                                ("license_ok", (field("license_ok").as_deref() == Some("true")).into()),
                                // The relay's own joiner cap for this room (0 = none): the host window says when people
                                // over it can't come through the link (DEC-2-5).
                                ("max_clients", field("max_clients").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0).into()),
                            ]),
                            Some("license") => notice(h, vec![("state", "license".into()), ("ok", (field("ok").as_deref() == Some("true")).into())]),
                            Some("closed") => notice(h, vec![("state", "closing".into()), ("reason", field("reason").unwrap_or_default().into())]),
                            _ => {}
                        }
                    }
                    Ok(Message::Close(c)) => {
                        if let Some(c) = c {
                            let code: u16 = c.code.into();
                            if FINAL_CODES.contains(&code) {
                                ended = Some(Done::Final(code, c.reason.to_string()));
                            }
                        }
                        break;
                    }
                    Err(_) => break,
                    _ => {}
                }
            }
        }
    }
    // Every joiner through this socket is gone; they rejoin when we are back.
    for (_, conn) in joiners.drain() {
        let _ = h.tx().send(Event::Closed(conn));
    }
    drop(out_tx);
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
    Ok(ended.unwrap_or(Done::Dropped))
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// One top-level field of the relay's small flat JSON messages, as text (strings unquoted). Enough for
/// {"t","room","ok","license_ok","reason"}; anything else is ignored.
fn json_field(t: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":");
    let i = t.find(&pat)? + pat.len();
    let rest = t[i..].trim_start();
    if let Some(r) = rest.strip_prefix('"') {
        let end = r.find('"')?;
        Some(r[..end].to_string())
    } else {
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        Some(rest[..end].trim().to_string())
    }
}
