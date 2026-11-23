// SPDX-License-Identifier: Apache-2.0
//! Studio's bridge for live plant and building data (0.8 XE.9, Studio side 0.11): Modbus TCP, MQTT 3.1.1 and BACnet/IP, READ ONLY.
//!
//! A page cannot open a socket, so Studio does, and nothing else: the protocols themselves live once, in the page's own codecs
//! (getcadcraft.com lib/live: modbus.js, mqtt.js, bacnet.js; rule 39, one engine). This module is a pipe with a gate:
//! - **Where.** A device on this computer or a private network (IPv4 10/8, 172.16/12, 192.168/16, 169.254/16, loopback; IPv6 loopback,
//!   fc00::/7, fe80::/10) is reached at once. Any other address needs the person's yes in a Studio dialog, once per host while Studio
//!   runs (lib/live policy.js: "a public address needs an approval for that host"). Multicast, broadcast (but see BACnet discovery) and
//!   0.0.0.0 are refused. A name is looked up and the address it gives is what is checked and used.
//! - **What.** Every byte the page sends is checked here, so a page cannot talk Studio into writing to a device:
//!   Modbus TCP: one whole frame, protocol id 0, function 1 to 4 (the four reads); nothing else.
//!   MQTT: whole packets only, CONNECT (without a will message: a will is a publish), SUBSCRIBE, UNSUBSCRIBE, PUBACK, PINGREQ, DISCONNECT;
//!   never PUBLISH, never QoS 2 handshakes.
//!   BACnet/IP: Original-Unicast/Broadcast-NPDU with an application message (never a network-layer one), and the APDU is Who-Is
//!   (unconfirmed service 8) or an unsegmented ReadProperty request (confirmed service 12); nothing else.
//! - **How much.** At most 8 connections at a time, 64 KiB out per call, 256 KiB in per call, 3 s to connect, at most 5 s per wait.
//!   Nothing is logged or kept: bytes pass through.
//!
//! BACnet discovery: `host: "broadcast"` (BACnet only) sends Who-Is to 255.255.255.255:47808 and accepts answers from private addresses;
//! only Who-Is may go out on it. Studio binds UDP 47808 when it is free (devices broadcast I-Am to that port), else any port (then only
//! devices that answer the sender directly are heard).
//!
//! Page side (agreed shape, docs/0.11-sessions/closing.md § Inbox to `twin-11`): `cadcraftDesktop.liveNet.open({protocol, host, port})
//! → id`, `send(id, bytes)`, `recv(id, waitMs) → bytes` (empty = nothing yet), `close(id)`; lib/live builds `live.read(...)` on top.
//!
//! The std-only core compiles and passes alone: `rustc --edition 2021 --test --cfg standalone live_net.rs && ./live_net`.

use std::collections::{HashMap, HashSet};
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const MAX_SESSIONS: usize = 8;
pub const MAX_SEND: usize = 64 * 1024;
pub const MAX_RECV: usize = 256 * 1024;
pub const CONNECT_WAIT: Duration = Duration::from_secs(3);
pub const MAX_WAIT_MS: u64 = 5000;
pub const BACNET_PORT: u16 = 47808;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proto {
    Modbus,
    Mqtt,
    Bacnet,
}

impl Proto {
    pub fn parse(s: &str) -> Option<Proto> {
        match s {
            "modbus" => Some(Proto::Modbus),
            "mqtt" => Some(Proto::Mqtt),
            "bacnet" => Some(Proto::Bacnet),
            _ => None,
        }
    }
    pub fn default_port(self) -> u16 {
        match self {
            Proto::Modbus => 502,
            Proto::Mqtt => 1883,
            Proto::Bacnet => BACNET_PORT,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Proto::Modbus => "Modbus TCP",
            Proto::Mqtt => "MQTT",
            Proto::Bacnet => "BACnet/IP",
        }
    }
}

/// May Studio reach this address: at once (`Ok(true)`), only after the person's yes (`Ok(false)`), or never (`Err`).
pub fn reach(ip: IpAddr) -> Result<bool, &'static str> {
    match ip {
        IpAddr::V4(v) => {
            if v.is_unspecified() || v.is_multicast() || v.is_broadcast() {
                return Err("multicast, broadcast and 0.0.0.0 are not devices");
            }
            Ok(v.is_loopback() || v.is_private() || v.is_link_local())
        }
        IpAddr::V6(v) => {
            if let Some(m) = v.to_ipv4_mapped() {
                return reach(IpAddr::V4(m));
            }
            if v.is_unspecified() || v.is_multicast() {
                return Err("multicast and :: are not devices");
            }
            let s = v.segments()[0];
            Ok(v.is_loopback() || (s & 0xfe00) == 0xfc00 || (s & 0xffc0) == 0xfe80)
        }
    }
}

/// A host the page typed: a name or an address (IPv6 with or without brackets). Plain characters only.
pub fn clean_host(host: &str) -> Result<String, &'static str> {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    if h.is_empty() || h.len() > 253 || !h.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'%' | b'_')) {
        return Err("that is not a host name or address");
    }
    Ok(h)
}

// ---- the gate: what may go out -------------------------------------------------------------------------------------------------

/// Modbus TCP: exactly one frame, protocol id 0, a length that matches, function 1 to 4 (read coils, discrete inputs, holding
/// registers, input registers).
pub fn modbus_ok(f: &[u8]) -> Result<(), &'static str> {
    if f.len() < 8 || f.len() > 260 {
        return Err("not a Modbus TCP frame");
    }
    if f[2] != 0 || f[3] != 0 {
        return Err("protocol id is not 0");
    }
    let len = ((f[4] as usize) << 8) | f[5] as usize;
    if len < 2 || 6 + len != f.len() {
        return Err("the length field does not match the frame");
    }
    if !(1..=4).contains(&f[7]) {
        return Err("only the four Modbus reads (functions 1 to 4) are sent: Studio never writes to a device");
    }
    Ok(())
}

/// One MQTT fixed header at `o`: (packet type, flags, where the body starts, body length).
fn mqtt_head(b: &[u8], o: usize) -> Result<(u8, u8, usize, usize), &'static str> {
    let first = *b.get(o).ok_or("cut-off packet")?;
    let (mut len, mut mult, mut i) = (0usize, 1usize, o + 1);
    loop {
        let d = *b.get(i).ok_or("cut-off packet")?;
        len += (d & 127) as usize * mult;
        i += 1;
        if d & 128 == 0 {
            break;
        }
        mult *= 128;
        if i - o > 4 {
            return Err("bad remaining length");
        }
    }
    if i + len > b.len() {
        return Err("cut-off packet");
    }
    Ok((first >> 4, first & 15, i, len))
}

/// MQTT 3.1.1 as a subscriber: every packet in `b` is whole and one a reader sends. CONNECT is checked for its will flag (a will is
/// a message the broker publishes for us) and its reserved bit.
pub fn mqtt_ok(b: &[u8]) -> Result<(), &'static str> {
    if b.is_empty() || b.len() > MAX_SEND {
        return Err("nothing to send, or too much");
    }
    let mut o = 0;
    while o < b.len() {
        let (kind, flags, body, len) = mqtt_head(b, o)?;
        let ok_flags = match kind {
            1 => {
                // CONNECT: "MQTT", level 4, then the connect flags
                let v = &b[body..body + len];
                if v.len() < 10 || v[0] != 0 || v[1] != 4 || &v[2..6] != b"MQTT" || v[6] != 4 {
                    return Err("CONNECT must be MQTT 3.1.1");
                }
                if v[7] & 0x01 != 0 {
                    return Err("CONNECT reserved flag is set");
                }
                if v[7] & 0x04 != 0 {
                    return Err("CONNECT with a will message is refused: a will is a publish, and Studio never publishes");
                }
                flags == 0
            }
            4 | 12 | 14 => flags == 0,     // PUBACK, PINGREQ, DISCONNECT
            8 | 10 => flags == 2,          // SUBSCRIBE, UNSUBSCRIBE
            3 => return Err("PUBLISH is never sent: Studio only reads"),
            _ => return Err("not a packet a subscriber sends"),
        };
        if !ok_flags {
            return Err("bad flags in the packet header");
        }
        o = body + len;
    }
    Ok(())
}

/// BACnet/IP: BVLC Original-Unicast (0x0a) or Original-Broadcast (0x0b) NPDU, an application message, and Who-Is or an
/// unsegmented ReadProperty. `discovery` = the broadcast session: Who-Is only.
pub fn bacnet_ok(b: &[u8], discovery: bool) -> Result<(), &'static str> {
    if b.len() < 8 || b.len() > 1497 {
        return Err("not a BACnet/IP packet");
    }
    if b[0] != 0x81 || ((b[2] as usize) << 8 | b[3] as usize) != b.len() {
        return Err("not BACnet/IP (BVLC header)");
    }
    if b[1] != 0x0a && b[1] != 0x0b {
        return Err("only original unicast and broadcast messages are sent");
    }
    if b[4] != 1 {
        return Err("unknown NPDU version");
    }
    let c = b[5];
    if c & 0x80 != 0 {
        return Err("network-layer messages are never sent");
    }
    let mut o = 6;
    if c & 0x20 != 0 {
        let dlen = *b.get(o + 2).ok_or("cut-off NPDU")? as usize;
        o += 3 + dlen;
    }
    if c & 0x08 != 0 {
        let slen = *b.get(o + 2).ok_or("cut-off NPDU")? as usize;
        o += 3 + slen;
    }
    if c & 0x20 != 0 {
        o += 1; // hop count
    }
    let a = b.get(o..).filter(|a| !a.is_empty()).ok_or("no application data")?;
    let who_is = a[0] >> 4 == 1 && a.get(1) == Some(&8);
    if discovery {
        return if who_is { Ok(()) } else { Err("only Who-Is goes out on the discovery broadcast") };
    }
    if who_is {
        return Ok(());
    }
    if a[0] >> 4 == 0 {
        if a[0] & 0x08 != 0 {
            return Err("segmented requests are not sent");
        }
        if a.len() >= 4 && a[3] == 12 {
            return Ok(());
        }
    }
    Err("only Who-Is and ReadProperty are sent: Studio never writes to a device")
}

pub fn gate(p: Proto, bytes: &[u8], discovery: bool) -> Result<(), &'static str> {
    if bytes.len() > MAX_SEND {
        return Err("too much in one send");
    }
    match p {
        Proto::Modbus => modbus_ok(bytes),
        Proto::Mqtt => mqtt_ok(bytes),
        Proto::Bacnet => bacnet_ok(bytes, discovery),
    }
}

// ---- sessions ----------------------------------------------------------------------------------------------------------------------

enum Sock {
    Tcp(TcpStream),
    Udp { sock: UdpSocket, to: SocketAddr, discovery: bool },
}

struct Session {
    proto: Proto,
    sock: Sock,
}

/// Where to connect: the address for `host`, after the reach rule (`approved` = the person said yes to this host). `Err` says why not;
/// `Ok(Err(addr))` = a yes is needed for that address first.
pub fn target(p: Proto, host: &str, port: u16, approved: bool) -> Result<Result<SocketAddr, SocketAddr>, String> {
    if port == 0 {
        return Err("port 0 is not a port".into());
    }
    let h = clean_host(host)?;
    let addrs: Vec<SocketAddr> = match h.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, port)],
        Err(_) => (h.as_str(), port).to_socket_addrs().map_err(|e| format!("cannot find {h}: {e}"))?.collect(),
    };
    let mut needs_yes = None;
    let mut why = "no address";
    for a in addrs {
        match reach(a.ip()) {
            Ok(true) => return Ok(Ok(a)),
            Ok(false) if approved => return Ok(Ok(a)),
            Ok(false) => needs_yes = needs_yes.or(Some(a)),
            Err(m) => why = m,
        }
    }
    let _ = p;
    match needs_yes {
        Some(a) => Ok(Err(a)),
        None => Err(why.into()),
    }
}

#[derive(Default)]
pub struct Live {
    sessions: Mutex<HashMap<u32, Arc<Session>>>,
    next: AtomicU32,
    /// Hosts the person said yes to while Studio runs (lowercase, as typed).
    approved: Mutex<HashSet<String>>,
}

impl Live {
    pub fn approved(&self, host: &str) -> bool {
        clean_host(host).ok().is_some_and(|h| self.approved.lock().map(|a| a.contains(&h)).unwrap_or(false))
    }
    pub fn approve(&self, host: &str) {
        if let (Ok(h), Ok(mut a)) = (clean_host(host), self.approved.lock()) {
            a.insert(h);
        }
    }

    fn add(&self, s: Session) -> Result<u32, String> {
        let mut m = self.sessions.lock().map_err(|_| "poisoned".to_string())?;
        if m.len() >= MAX_SESSIONS {
            return Err(format!("at most {MAX_SESSIONS} device connections at a time: close one first"));
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        m.insert(id, Arc::new(s));
        Ok(id)
    }

    /// Open a connection to an address `target` already allowed.
    pub fn open_at(&self, p: Proto, at: SocketAddr) -> Result<u32, String> {
        let sock = match p {
            Proto::Modbus | Proto::Mqtt => {
                let s = TcpStream::connect_timeout(&at, CONNECT_WAIT).map_err(|e| format!("cannot connect to {at}: {e}"))?;
                s.set_nodelay(true).ok();
                s.set_write_timeout(Some(CONNECT_WAIT)).ok();
                Sock::Tcp(s)
            }
            Proto::Bacnet => {
                let any: SocketAddr = if at.is_ipv4() { (Ipv4Addr::UNSPECIFIED, 0).into() } else { (Ipv6Addr::UNSPECIFIED, 0).into() };
                let sock = UdpSocket::bind(any).map_err(|e| format!("cannot open a UDP port: {e}"))?;
                Sock::Udp { sock, to: at, discovery: false }
            }
        };
        self.add(Session { proto: p, sock })
    }

    /// BACnet discovery: Who-Is to the local broadcast, answers from private addresses. Port 47808 when free (I-Am is broadcast
    /// there), else any port.
    pub fn open_discovery(&self) -> Result<u32, String> {
        let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, BACNET_PORT)).or_else(|_| UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))).map_err(|e| format!("cannot open a UDP port: {e}"))?;
        sock.set_broadcast(true).map_err(|e| format!("cannot broadcast: {e}"))?;
        let to = SocketAddr::from((Ipv4Addr::BROADCAST, BACNET_PORT));
        self.add(Session { proto: Proto::Bacnet, sock: Sock::Udp { sock, to, discovery: true } })
    }

    fn get(&self, id: u32) -> Result<Arc<Session>, String> {
        self.sessions.lock().map_err(|_| "poisoned".to_string())?.get(&id).cloned().ok_or_else(|| "no such connection (it was closed)".into())
    }

    pub fn send(&self, id: u32, bytes: &[u8]) -> Result<(), String> {
        let s = self.get(id)?;
        let discovery = matches!(s.sock, Sock::Udp { discovery: true, .. });
        gate(s.proto, bytes, discovery).map_err(|m| m.to_string())?;
        let r = match &s.sock {
            Sock::Tcp(t) => (&*t).write_all(bytes),
            Sock::Udp { sock, to, .. } => sock.send_to(bytes, to).map(|_| ()),
        };
        r.map_err(|e| {
            self.close(id);
            format!("send failed, connection closed: {e}")
        })
    }

    /// What has arrived, waiting up to `wait_ms` (at most 5 s) for the first byte. Empty = nothing yet. TCP: what is there, up to
    /// 256 KiB (the page's codec joins the pieces). UDP: one datagram from the device (discovery: from any private address).
    pub fn recv(&self, id: u32, wait_ms: u64) -> Result<Vec<u8>, String> {
        let s = self.get(id)?;
        let wait = Duration::from_millis(wait_ms.clamp(1, MAX_WAIT_MS));
        match &s.sock {
            Sock::Tcp(t) => {
                t.set_read_timeout(Some(wait)).ok();
                let mut buf = vec![0u8; 64 * 1024];
                let mut out = Vec::new();
                match (&*t).read(&mut buf) {
                    Ok(0) => {
                        self.close(id);
                        return Err("the device closed the connection".into());
                    }
                    Ok(n) => out.extend_from_slice(&buf[..n]),
                    Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Ok(out),
                    Err(e) => {
                        self.close(id);
                        return Err(format!("connection lost: {e}"));
                    }
                }
                // take what else is already there, without waiting again
                t.set_nonblocking(true).ok();
                while out.len() < MAX_RECV {
                    let room = (MAX_RECV - out.len()).min(buf.len());
                    match (&*t).read(&mut buf[..room]) {
                        Ok(0) => break,
                        Ok(n) => out.extend_from_slice(&buf[..n]),
                        Err(_) => break,
                    }
                }
                t.set_nonblocking(false).ok();
                Ok(out)
            }
            Sock::Udp { sock, to, discovery } => {
                let end = Instant::now() + wait;
                let mut buf = vec![0u8; 1500];
                loop {
                    let left = end.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        return Ok(Vec::new());
                    }
                    sock.set_read_timeout(Some(left)).ok();
                    match sock.recv_from(&mut buf) {
                        Ok((n, from)) => {
                            let ours = if *discovery { reach(from.ip()) == Ok(true) } else { from.ip() == to.ip() };
                            if ours {
                                return Ok(buf[..n].to_vec());
                            }
                        }
                        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Ok(Vec::new()),
                        Err(e) => return Err(format!("receive failed: {e}")),
                    }
                }
            }
        }
    }

    pub fn close(&self, id: u32) -> bool {
        let s = self.sessions.lock().ok().and_then(|mut m| m.remove(&id));
        if let Some(s) = &s {
            if let Sock::Tcp(t) = &s.sock {
                t.shutdown(Shutdown::Both).ok();
            }
        }
        s.is_some()
    }

    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.sessions.lock().map(|m| m.len()).unwrap_or(0)
    }
}

#[cfg(not(standalone))]
mod commands {
    use super::*;
    use tauri::State;

    /// Open a read-only connection. A device outside this computer and the private networks needs the person's yes (asked here,
    /// once per host while Studio runs). `host: "broadcast"` = BACnet discovery.
    #[tauri::command]
    pub async fn cc_live_net_open(win: tauri::WebviewWindow, live: State<'_, Live>, protocol: String, host: String, port: Option<u16>) -> Result<u32, String> {
        use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
        let p = Proto::parse(&protocol).ok_or("unknown protocol (modbus, mqtt or bacnet)")?;
        if host.trim().eq_ignore_ascii_case("broadcast") {
            return if p == Proto::Bacnet { live.open_discovery() } else { Err("only BACnet discovers devices by broadcast".into()) };
        }
        let port = port.unwrap_or(p.default_port());
        let (h, pp) = (host.clone(), p);
        let approved = live.approved(&host);
        let t = tauri::async_runtime::spawn_blocking(move || target(pp, &h, port, approved)).await.map_err(|e| e.to_string())??;
        let at = match t {
            Ok(a) => a,
            Err(a) => {
                let shown: String = host.chars().filter(|c| !c.is_control()).take(80).collect();
                let text = format!(
                    "Connect to {shown} ({}) with {}?\n\nThis device is not on this computer or a private network. Studio only reads from it and never writes to it.",
                    a.ip(),
                    p.name()
                );
                let yes = match crate::files::test_answer() {
                    Some(x) => x.is_some(),
                    None => {
                        let w = win.clone();
                        tauri::async_runtime::spawn_blocking(move || w.dialog().message(text).title("Connect to a device?").kind(MessageDialogKind::Warning).buttons(MessageDialogButtons::OkCancelCustom("Connect".into(), "Cancel".into())).blocking_show()).await.map_err(|e| e.to_string())?
                    }
                };
                if !yes {
                    return Err("not_allowed: you did not allow a connection to that device".into());
                }
                live.approve(&host);
                a
            }
        };
        // At most 3 s (CONNECT_WAIT), on the async runtime: never the main thread.
        live.open_at(p, at)
    }

    #[tauri::command]
    pub fn cc_live_net_send(live: State<'_, Live>, id: u32, bytes: Vec<u8>) -> Result<(), String> {
        live.send(id, &bytes)
    }

    /// Async: the wait (up to 5 s) never holds the main thread.
    #[tauri::command]
    pub async fn cc_live_net_recv(live: State<'_, Live>, id: u32, wait_ms: Option<u64>) -> Result<Vec<u8>, String> {
        live.recv(id, wait_ms.unwrap_or(1000))
    }

    #[tauri::command]
    pub fn cc_live_net_close(live: State<'_, Live>, id: u32) -> bool {
        live.close(id)
    }
}
#[cfg(not(standalone))]
pub use commands::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn where_studio_may_connect() {
        let v4 = |s: &str| reach(s.parse().unwrap());
        assert_eq!(v4("192.168.1.20"), Ok(true));
        assert_eq!(v4("10.0.0.5"), Ok(true));
        assert_eq!(v4("172.20.1.1"), Ok(true));
        assert_eq!(v4("169.254.3.4"), Ok(true));
        assert_eq!(v4("127.0.0.1"), Ok(true));
        assert_eq!(v4("172.32.0.1"), Ok(false), "outside 172.16/12: a yes first");
        assert_eq!(v4("203.0.113.9"), Ok(false));
        assert!(v4("0.0.0.0").is_err() && v4("224.0.0.1").is_err() && v4("255.255.255.255").is_err());
        assert_eq!(v4("::1"), Ok(true));
        assert_eq!(v4("fd12:3456::1"), Ok(true));
        assert_eq!(v4("fe80::1"), Ok(true));
        assert_eq!(v4("::ffff:192.168.0.2"), Ok(true), "a mapped IPv4 address is judged as IPv4");
        assert_eq!(v4("2001:db8::1"), Ok(false));
        assert!(v4("ff02::1").is_err());
        assert!(clean_host("plc 1").is_err() && clean_host("").is_err() && clean_host("a/b").is_err());
        assert_eq!(clean_host(" [FE80::1] ").unwrap(), "fe80::1");
    }

    #[test]
    fn a_target_needs_a_yes_outside_private_networks() {
        assert_eq!(target(Proto::Modbus, "192.168.1.20", 502, false).unwrap(), Ok("192.168.1.20:502".parse().unwrap()));
        assert_eq!(target(Proto::Mqtt, "203.0.113.9", 1883, false).unwrap(), Err("203.0.113.9:1883".parse().unwrap()));
        assert_eq!(target(Proto::Mqtt, "203.0.113.9", 1883, true).unwrap(), Ok("203.0.113.9:1883".parse().unwrap()));
        assert!(target(Proto::Modbus, "224.0.0.1", 502, true).is_err(), "never multicast, yes or not");
        assert!(target(Proto::Modbus, "192.168.1.20", 0, false).is_err());
        assert_eq!(target(Proto::Modbus, "localhost", 502, false).unwrap().map(|a| a.ip().is_loopback()), Ok(true), "a name: the address it gives is checked");
    }

    // frames as lib/live's codecs build them
    fn mb(fc: u8) -> Vec<u8> {
        vec![0, 1, 0, 0, 0, 6, 1, fc, 0, 0, 0, 2]
    }

    #[test]
    fn modbus_reads_only() {
        for fc in 1..=4 {
            assert_eq!(modbus_ok(&mb(fc)), Ok(()));
        }
        for fc in [5u8, 6, 15, 16, 8, 43, 0x2b, 0x17] {
            assert!(modbus_ok(&mb(fc)).is_err(), "function {fc} refused");
        }
        let mut f = mb(3);
        f[5] = 7;
        assert!(modbus_ok(&f).is_err(), "length that does not match");
        let mut two = mb(3);
        two.extend(mb(3));
        assert!(modbus_ok(&two).is_err(), "one frame per send");
        let mut p = mb(3);
        p[2] = 1;
        assert!(modbus_ok(&p).is_err(), "protocol id 0 only");
    }

    fn mqtt_connect(flags: u8) -> Vec<u8> {
        let mut v = vec![0, 4, b'M', b'Q', b'T', b'T', 4, flags, 0, 60, 0, 2, b'c', b'c'];
        let mut p = vec![0x10, v.len() as u8];
        p.append(&mut v);
        p
    }

    #[test]
    fn mqtt_subscribes_and_never_publishes() {
        assert_eq!(mqtt_ok(&mqtt_connect(0x02)), Ok(()));
        assert_eq!(mqtt_ok(&mqtt_connect(0xc2)), Ok(()), "user name and password are fine");
        assert!(mqtt_ok(&mqtt_connect(0x06)).unwrap_err().contains("will"), "a will message is a publish");
        assert!(mqtt_ok(&mqtt_connect(0x03)).is_err(), "reserved bit");
        let sub = [0x82, 8, 0, 1, 0, 3, b'a', b'/', b'#', 0];
        assert_eq!(mqtt_ok(&sub), Ok(()));
        assert_eq!(mqtt_ok(&[0xa2, 5, 0, 2, 0, 1, b'a']), Ok(()), "UNSUBSCRIBE");
        assert_eq!(mqtt_ok(&[0x40, 2, 0, 7]), Ok(()), "PUBACK");
        assert_eq!(mqtt_ok(&[0xc0, 0, 0xe0, 0]), Ok(()), "PINGREQ then DISCONNECT in one send");
        assert!(mqtt_ok(&[0x30, 5, 0, 1, b'a', b'h', b'i']).unwrap_err().contains("PUBLISH"));
        let mut mixed = sub.to_vec();
        mixed.extend([0x30, 3, 0, 1, b'a']);
        assert!(mqtt_ok(&mixed).is_err(), "a PUBLISH hidden after a SUBSCRIBE");
        assert!(mqtt_ok(&[0x80, 3, 0, 1, 0]).is_err(), "SUBSCRIBE needs flags 2");
        assert!(mqtt_ok(&[0x50, 2, 0, 1]).is_err() && mqtt_ok(&[0x62, 2, 0, 1]).is_err(), "no QoS 2 handshake");
        assert!(mqtt_ok(&[0x82, 8, 0, 1]).is_err(), "cut-off packet");
        assert!(mqtt_ok(&[0xc0, 0xff, 0xff, 0xff, 0xff, 0x01]).is_err(), "bad remaining length");
    }

    // bacnet.js whoIs() and readProperty({invoke 1, analog-input 3, present-value})
    fn who_is() -> Vec<u8> {
        vec![0x81, 0x0b, 0, 12, 1, 0x20, 0xff, 0xff, 0, 0xff, 0x10, 0x08]
    }
    fn read_prop() -> Vec<u8> {
        let mut b = vec![0x81, 0x0a, 0, 0, 1, 0x04, 0x00, 0x05, 1, 12, 0x0c, 0, 0, 0, 3, 0x19, 85];
        let n = b.len() as u16;
        b[2] = (n >> 8) as u8;
        b[3] = n as u8;
        b
    }

    #[test]
    fn bacnet_who_is_and_read_property_only() {
        assert_eq!(bacnet_ok(&who_is(), false), Ok(()));
        assert_eq!(bacnet_ok(&who_is(), true), Ok(()));
        assert_eq!(bacnet_ok(&read_prop(), false), Ok(()));
        assert!(bacnet_ok(&read_prop(), true).is_err(), "discovery sends Who-Is only");
        let mut write = read_prop();
        write[9] = 15; // WriteProperty
        assert!(bacnet_ok(&write, false).is_err());
        let mut reinit = read_prop();
        reinit[9] = 20; // ReinitializeDevice
        assert!(bacnet_ok(&reinit, false).is_err());
        let mut seg = read_prop();
        seg[6] = 0x08;
        assert!(bacnet_ok(&seg, false).is_err(), "segmented");
        let mut net = who_is();
        net[5] = 0x80;
        assert!(bacnet_ok(&net, false).is_err(), "network-layer message");
        let mut len = who_is();
        len[3] = 13;
        assert!(bacnet_ok(&len, false).is_err(), "BVLC length must match");
        let mut fwd = who_is();
        fwd[1] = 0x04;
        assert!(bacnet_ok(&fwd, false).is_err(), "no Forwarded-NPDU");
        let mut i_have = who_is();
        i_have[11] = 1; // unconfirmed service 1 = I-Have
        assert!(bacnet_ok(&i_have, false).is_err());
    }

    #[test]
    fn a_modbus_read_goes_through_and_a_write_never_leaves() {
        let srv = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = srv.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let (mut c, _) = srv.accept().unwrap();
            let mut req = [0u8; 12];
            c.read_exact(&mut req).unwrap();
            assert_eq!(req[7], 3, "only the read arrived");
            c.write_all(&[0, 1, 0, 0, 0, 7, 1, 3, 4, 0, 42, 0, 7]).unwrap();
            let mut more = [0u8; 1];
            c.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
            assert!(c.read(&mut more).map(|n| n == 0).unwrap_or(true), "nothing else was sent");
        });
        let live = Live::default();
        let id = live.open_at(Proto::Modbus, at).unwrap();
        assert!(live.send(id, &mb(6)).is_err(), "a write is refused before it leaves");
        live.send(id, &mb(3)).unwrap();
        let mut got = Vec::new();
        for _ in 0..20 {
            got.extend(live.recv(id, 200).unwrap());
            if got.len() >= 13 {
                break;
            }
        }
        assert_eq!(got, vec![0, 1, 0, 0, 0, 7, 1, 3, 4, 0, 42, 0, 7]);
        assert!(live.close(id));
        h.join().unwrap();
        assert!(live.send(id, &mb(3)).is_err(), "closed is closed");
    }

    #[test]
    fn bacnet_answers_come_from_the_device_only() {
        let dev = UdpSocket::bind("127.0.0.1:0").unwrap();
        let at = dev.local_addr().unwrap();
        let live = Live::default();
        let id = live.open_at(Proto::Bacnet, at).unwrap();
        live.send(id, &read_prop()).unwrap();
        let mut buf = [0u8; 64];
        let (n, from) = dev.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], &read_prop()[..]);
        dev.send_to(&[0x81, 0x0a, 0, 4], from).unwrap();
        assert_eq!(live.recv(id, 1000).unwrap(), vec![0x81, 0x0a, 0, 4]);
        assert!(live.recv(id, 50).unwrap().is_empty(), "nothing more: empty, not an error");
        live.close(id);
    }

    #[test]
    fn at_most_eight_connections() {
        let live = Live::default();
        let dev = UdpSocket::bind("127.0.0.1:0").unwrap();
        let at = dev.local_addr().unwrap();
        let ids: Vec<u32> = (0..MAX_SESSIONS).map(|_| live.open_at(Proto::Bacnet, at).unwrap()).collect();
        assert!(live.open_at(Proto::Bacnet, at).unwrap_err().contains("at most"));
        live.close(ids[0]);
        assert!(live.open_at(Proto::Bacnet, at).is_ok());
        assert_eq!(live.count(), MAX_SESSIONS);
        assert!(live.approved("203.0.113.9") == false);
        live.approve("203.0.113.9");
        assert!(live.approved(" 203.0.113.9 "));
    }
}
