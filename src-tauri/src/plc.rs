// SPDX-License-Identifier: Apache-2.0
//! A Modbus TCP socket for the Material flow add-on's PLC emulation (0.8 SU.858 step 5; owner 2026-10-29: "go with Studio step 5, as an add-on").
//!
//! The model (the add-on's plc.js, in a ModelCraft window) plays the plant; a PLC elsewhere — or a soft PLC on this computer — plays the controller and talks
//! Modbus TCP. A page cannot open a port, so Studio does: this module listens on THIS COMPUTER ONLY (127.0.0.1; a PLC on another machine reaches it through
//! the person's own tunnel or proxy, never through a port we open to the network), reads whole Modbus TCP frames, keeps each until the page answers it
//! (plc-link.js polls), and writes the answer back. It knows nothing about registers or coils: the page decides every answer.
//!
//! Limits: at most 4 connections, 64 frames waiting, a frame is at most 260 bytes (MBAP header 7 + PDU 253), the page has 2 s to answer one, an idle
//! connection is dropped after 60 s, a frame whose protocol id is not 0 or whose length field does not fit ends the connection. Frames are not stored
//! after they are answered. Nothing is logged but counts.
//!
//! The std-only core compiles and passes alone: `rustc --edition 2021 --test --cfg standalone plc.rs && ./plc`. The Tauri commands (cc_plc_start,
//! cc_plc_poll, cc_plc_respond, cc_plc_stop) are behind `cfg(not(standalone))`; lib.rs: `mod plc;`, `.manage(plc::Hub::default())`, the four names in
//! generate_handler! (computers only), and bridge.js `plc: { start, poll, respond, stop }`.

use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const MAX_FRAME: usize = 260;
pub const MAX_CONNECTIONS: usize = 4;
pub const MAX_WAITING: usize = 64;
pub const ANSWER_WITHIN: Duration = Duration::from_secs(2);
pub const IDLE_AFTER: Duration = Duration::from_secs(60);

type Frame = (u64, Vec<u8>);

#[derive(Default)]
struct Shared {
    queue: Mutex<VecDeque<Frame>>,
    answers: Mutex<HashMap<u64, Option<Vec<u8>>>>,
    answered: Condvar,
    next: AtomicU64,
    stop: AtomicBool,
    conns: AtomicUsize,
    served: AtomicU64,
}

/// What a Modbus TCP header says about the rest of the frame: how many bytes follow the 7-byte header, or why the connection must end.
pub fn rest_of_frame(header: &[u8; 7]) -> Result<usize, &'static str> {
    if header[2] != 0 || header[3] != 0 {
        return Err("protocol id is not 0");
    }
    let len = ((header[4] as usize) << 8) | header[5] as usize; // unit id + PDU
    if len < 2 || len > 254 {
        return Err("length does not fit a Modbus frame");
    }
    Ok(len - 1)
}

/// Read one whole frame (header + PDU). Ok(None) = the other side closed cleanly between frames.
pub fn read_frame<R: Read>(r: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut head = [0u8; 7];
    let mut got = 0;
    while got < 7 {
        match r.read(&mut head[got..]) {
            Ok(0) => {
                return if got == 0 { Ok(None) } else { Err(std::io::Error::new(ErrorKind::UnexpectedEof, "cut-off header")) };
            }
            Ok(n) => got += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    let rest = rest_of_frame(&head).map_err(|m| std::io::Error::new(ErrorKind::InvalidData, m))?;
    let mut frame = Vec::with_capacity(7 + rest);
    frame.extend_from_slice(&head);
    frame.resize(7 + rest, 0);
    r.read_exact(&mut frame[7..])?;
    Ok(Some(frame))
}

#[derive(Default, Clone)]
pub struct Hub {
    shared: Arc<Shared>,
    local: Arc<Mutex<Option<SocketAddr>>>,
}

impl Hub {
    /// Listen on 127.0.0.1:port (0 = any free port). → the port. A second start while one runs is an error (stop first).
    pub fn start(&self, port: u16) -> Result<u16, String> {
        let mut slot = self.local.lock().map_err(|_| "poisoned".to_string())?;
        if slot.is_some() {
            return Err("already listening".into());
        }
        let l = TcpListener::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))).map_err(|e| format!("cannot listen on port {port}: {e}"))?;
        l.set_nonblocking(true).map_err(|e| e.to_string())?;
        let addr = l.local_addr().map_err(|e| e.to_string())?;
        self.shared.stop.store(false, Ordering::SeqCst);
        let sh = self.shared.clone();
        thread::spawn(move || accept_loop(l, sh));
        *slot = Some(addr);
        Ok(addr.port())
    }

    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Ok(mut s) = self.local.lock() {
            *s = None;
        }
        if let Ok(mut q) = self.shared.queue.lock() {
            q.clear();
        }
        self.shared.answered.notify_all();
    }

    pub fn listening(&self) -> Option<SocketAddr> {
        self.local.lock().ok().and_then(|s| *s)
    }

    /// The frames that came in since the last poll (oldest first).
    pub fn poll(&self) -> Vec<Frame> {
        match self.shared.queue.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// The page's answer to frame `id` (None = it gives none: the PLC's request times out, as with a real device).
    pub fn respond(&self, id: u64, bytes: Option<Vec<u8>>) {
        if let Some(b) = &bytes {
            if b.len() < 8 || b.len() > MAX_FRAME {
                self.shared.answers.lock().map(|mut m| m.insert(id, None)).ok();
                self.shared.answered.notify_all();
                return;
            }
        }
        if let Ok(mut m) = self.shared.answers.lock() {
            if m.len() > 4 * MAX_WAITING {
                m.clear();
            }
            m.insert(id, bytes);
        }
        self.shared.answered.notify_all();
    }

    pub fn served(&self) -> u64 {
        self.shared.served.load(Ordering::SeqCst)
    }
    pub fn connections(&self) -> usize {
        self.shared.conns.load(Ordering::SeqCst)
    }
}

fn accept_loop(l: TcpListener, sh: Arc<Shared>) {
    while !sh.stop.load(Ordering::SeqCst) {
        match l.accept() {
            Ok((s, peer)) => {
                if !peer.ip().is_loopback() || sh.conns.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                    let _ = s.shutdown(Shutdown_Both());
                    continue;
                }
                sh.conns.fetch_add(1, Ordering::SeqCst);
                let sh2 = sh.clone();
                thread::spawn(move || {
                    serve(s, &sh2);
                    sh2.conns.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(10)),
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
}

#[allow(non_snake_case)]
fn Shutdown_Both() -> std::net::Shutdown {
    std::net::Shutdown::Both
}

fn serve(mut s: TcpStream, sh: &Arc<Shared>) {
    let _ = s.set_nonblocking(false);
    let _ = s.set_nodelay(true);
    let _ = s.set_read_timeout(Some(IDLE_AFTER));
    let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
    while !sh.stop.load(Ordering::SeqCst) {
        let frame = match read_frame(&mut s) {
            Ok(Some(f)) => f,
            _ => break, // closed, idle too long, or not Modbus
        };
        let id = sh.next.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut q = match sh.queue.lock() {
                Ok(q) => q,
                Err(_) => break,
            };
            if q.len() >= MAX_WAITING {
                break; // the page is not keeping up: end this connection rather than grow
            }
            q.push_back((id, frame));
        }
        // wait for the page's answer
        let deadline = Instant::now() + ANSWER_WITHIN;
        let mut answers = match sh.answers.lock() {
            Ok(a) => a,
            Err(_) => break,
        };
        let out = loop {
            if let Some(a) = answers.remove(&id) {
                break a;
            }
            let now = Instant::now();
            if now >= deadline || sh.stop.load(Ordering::SeqCst) {
                break None;
            }
            answers = match sh.answered.wait_timeout(answers, deadline - now) {
                Ok((g, _)) => g,
                Err(_) => return,
            };
        };
        drop(answers);
        if let Some(bytes) = out {
            sh.served.fetch_add(1, Ordering::SeqCst);
            if s.write_all(&bytes).is_err() {
                break;
            }
        }
    }
    let _ = s.shutdown(Shutdown_Both());
}

#[cfg(not(standalone))]
mod commands {
    use super::*;
    use serde::Serialize;
    use tauri::State;

    #[derive(Serialize)]
    pub struct PlcFrame {
        pub id: u64,
        pub bytes: Vec<u8>,
    }
    #[derive(Serialize)]
    pub struct PlcStarted {
        pub port: u16,
    }

    /// Start listening (computers only). Ports below 1024 need privileges we do not ask for: refused here with a plain message.
    #[tauri::command]
    pub fn cc_plc_start(hub: State<'_, Hub>, port: u16) -> Result<PlcStarted, String> {
        if port != 0 && port < 1024 {
            return Err("use a port from 1024 up (a port below needs administrator rights)".into());
        }
        hub.start(port).map(|port| PlcStarted { port })
    }

    #[tauri::command]
    pub fn cc_plc_poll(hub: State<'_, Hub>) -> Vec<PlcFrame> {
        hub.poll().into_iter().map(|(id, bytes)| PlcFrame { id, bytes }).collect()
    }

    #[tauri::command]
    pub fn cc_plc_respond(hub: State<'_, Hub>, id: u64, bytes: Option<Vec<u8>>) {
        hub.respond(id, bytes);
    }

    #[tauri::command]
    pub fn cc_plc_stop(hub: State<'_, Hub>) {
        hub.stop();
    }
}
#[cfg(not(standalone))]
pub use commands::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn read_coil_frame(tx: u16) -> Vec<u8> {
        vec![(tx >> 8) as u8, tx as u8, 0, 0, 0, 6, 1, 1, 0, 0, 0, 1]
    }

    /// A page stand-in: polls and answers every frame with `answer(frame)`.
    fn responder(hub: Hub, answer: fn(&[u8]) -> Option<Vec<u8>>) -> Arc<AtomicBool> {
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        thread::spawn(move || {
            while !s2.load(Ordering::SeqCst) {
                for (id, f) in hub.poll() {
                    hub.respond(id, answer(&f));
                }
                thread::sleep(Duration::from_millis(2));
            }
        });
        stop
    }
    fn echo_ok(f: &[u8]) -> Option<Vec<u8>> {
        // transaction id echoed, one coil = 0
        Some(vec![f[0], f[1], 0, 0, 0, 4, f[6], f[7], 1, 0])
    }
    fn none(_: &[u8]) -> Option<Vec<u8>> {
        None
    }
    fn talk(port: u16, req: &[u8]) -> std::io::Result<Vec<u8>> {
        let mut c = TcpStream::connect(("127.0.0.1", port))?;
        c.set_read_timeout(Some(Duration::from_millis(1500)))?;
        c.write_all(req)?;
        let mut buf = vec![0u8; 64];
        let n = c.read(&mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    #[test]
    fn header_rules() {
        assert_eq!(rest_of_frame(&[0, 1, 0, 0, 0, 6, 1]), Ok(5));
        assert_eq!(rest_of_frame(&[0, 1, 0, 1, 0, 6, 1]), Err("protocol id is not 0"));
        assert!(rest_of_frame(&[0, 1, 0, 0, 0, 1, 1]).is_err(), "length 1 has no function code");
        assert!(rest_of_frame(&[0, 1, 0, 0, 0, 255, 1]).is_err(), "length 255 is more than a PDU may be");
        assert_eq!(rest_of_frame(&[0, 1, 0, 0, 0, 254, 1]), Ok(253));
    }

    #[test]
    fn frames_are_read_whole_even_when_they_arrive_in_pieces() {
        struct Dribble(Vec<u8>, usize);
        impl Read for Dribble {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                if self.1 >= self.0.len() {
                    return Ok(0);
                }
                b[0] = self.0[self.1];
                self.1 += 1;
                Ok(1)
            }
        }
        let f = read_coil_frame(9);
        assert_eq!(read_frame(&mut Dribble(f.clone(), 0)).unwrap().unwrap(), f);
        assert!(read_frame(&mut Cursor::new(Vec::<u8>::new())).unwrap().is_none(), "clean close between frames");
        assert!(read_frame(&mut Cursor::new(vec![0, 1, 0])).is_err(), "cut-off header");
        assert!(read_frame(&mut Cursor::new(f[..9].to_vec())).is_err(), "cut-off body");
        assert!(read_frame(&mut Cursor::new(vec![0, 1, 0, 9, 0, 6, 1, 1, 0, 0, 0, 1])).is_err(), "not Modbus");
    }

    #[test]
    fn a_request_goes_to_the_page_and_its_answer_comes_back() {
        let hub = Hub::default();
        let port = hub.start(0).unwrap();
        assert!(hub.listening().unwrap().ip().is_loopback(), "listens on this computer only");
        let stop = responder(hub.clone(), echo_ok);
        let got = talk(port, &read_coil_frame(0x1234)).unwrap();
        assert_eq!(got, vec![0x12, 0x34, 0, 0, 0, 4, 1, 1, 1, 0]);
        assert_eq!(hub.served(), 1);
        stop.store(true, Ordering::SeqCst);
        hub.stop();
    }

    #[test]
    fn several_requests_on_one_connection_keep_their_order() {
        let hub = Hub::default();
        let port = hub.start(0).unwrap();
        let stop = responder(hub.clone(), echo_ok);
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.set_read_timeout(Some(Duration::from_millis(1500))).unwrap();
        for tx in 1u16..=5 {
            c.write_all(&read_coil_frame(tx)).unwrap();
            let mut buf = [0u8; 10];
            c.read_exact(&mut buf).unwrap();
            assert_eq!(u16::from_be_bytes([buf[0], buf[1]]), tx);
        }
        stop.store(true, Ordering::SeqCst);
        hub.stop();
    }

    #[test]
    fn no_answer_means_silence_and_the_connection_stays() {
        let hub = Hub::default();
        let port = hub.start(0).unwrap();
        let stop = responder(hub.clone(), none);
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.set_read_timeout(Some(Duration::from_millis(400))).unwrap();
        c.write_all(&read_coil_frame(1)).unwrap();
        let mut b = [0u8; 4];
        assert!(c.read(&mut b).is_err(), "a timeout, not an answer");
        stop.store(true, Ordering::SeqCst);
        stop_all_responders(&hub);
        let stop2 = responder(hub.clone(), echo_ok);
        c.set_read_timeout(Some(Duration::from_millis(1500))).unwrap();
        c.write_all(&read_coil_frame(2)).unwrap();
        let mut buf = [0u8; 10];
        c.read_exact(&mut buf).unwrap();
        assert_eq!(u16::from_be_bytes([buf[0], buf[1]]), 2, "the same connection still works");
        stop2.store(true, Ordering::SeqCst);
        hub.stop();
    }
    fn stop_all_responders(_: &Hub) {
        thread::sleep(Duration::from_millis(20));
    }

    #[test]
    fn something_that_is_not_modbus_ends_the_connection() {
        let hub = Hub::default();
        let port = hub.start(0).unwrap();
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.set_read_timeout(Some(Duration::from_millis(1500))).unwrap();
        c.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
        let mut b = [0u8; 8];
        assert_eq!(c.read(&mut b).unwrap_or(0), 0, "closed, nothing sent back");
        assert!(hub.poll().is_empty(), "and nothing was handed to the page");
        hub.stop();
    }

    #[test]
    fn at_most_four_connections() {
        let hub = Hub::default();
        let port = hub.start(0).unwrap();
        let held: Vec<TcpStream> = (0..MAX_CONNECTIONS).map(|_| TcpStream::connect(("127.0.0.1", port)).unwrap()).collect();
        thread::sleep(Duration::from_millis(100));
        assert_eq!(hub.connections(), MAX_CONNECTIONS);
        let mut fifth = TcpStream::connect(("127.0.0.1", port)).unwrap();
        fifth.set_read_timeout(Some(Duration::from_millis(1000))).unwrap();
        let mut b = [0u8; 1];
        assert_eq!(fifth.read(&mut b).unwrap_or(0), 0, "the fifth is closed at once");
        drop(held);
        hub.stop();
    }

    #[test]
    fn a_bad_answer_is_dropped_and_a_second_start_is_refused() {
        let hub = Hub::default();
        let _ = hub.start(0).unwrap();
        assert!(hub.start(0).is_err(), "already listening");
        hub.respond(5, Some(vec![1, 2, 3])); // too short to be a frame
        assert_eq!(hub.shared.answers.lock().unwrap().get(&5), Some(&None));
        hub.respond(6, Some(vec![0u8; MAX_FRAME + 1]));
        assert_eq!(hub.shared.answers.lock().unwrap().get(&6), Some(&None));
        hub.stop();
        assert!(hub.listening().is_none());
        let again = hub.start(0);
        assert!(again.is_ok(), "after stop it can start again");
        hub.stop();
    }

    #[test]
    fn the_page_that_never_answers_is_not_waited_for_forever() {
        let hub = Hub::default();
        let port = hub.start(0).unwrap();
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.set_read_timeout(Some(Duration::from_millis(3500))).unwrap();
        c.write_all(&read_coil_frame(1)).unwrap();
        thread::sleep(Duration::from_millis(2300)); // past ANSWER_WITHIN
        assert_eq!(hub.poll().len(), 1, "the frame was waiting for the page");
        c.write_all(&read_coil_frame(2)).unwrap(); // the connection is still usable
        thread::sleep(Duration::from_millis(100));
        assert_eq!(hub.poll().len(), 1);
        hub.stop();
    }
}
