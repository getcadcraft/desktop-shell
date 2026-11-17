// SPDX-License-Identifier: Apache-2.0
//! 3D mice and MIDI controllers for Studio's own window (0.8 ID.8; write-up getcadcraft.com docs/spikes/studio-devices.md).
//!
//! Studio's web views on Linux (WebKitGTK) and macOS (WKWebView) have no WebHID and no Web MIDI, and WebView2's own doors may
//! not be usable inside a host window. The page layer (assets/js/app-devices-ui.js) is the ONE copy of the 3D mouse and MIDI
//! logic; assets/js/app-devices-studio.js gives it the two doors, backed by this module through `cadcraftDesktop.devices`:
//!
//!   cc_hid_list            → [{ id, vendorId, productId, productName, usagePage, usage }]   multi-axis controllers only
//!   cc_hid_blocked         → how many 3D mice are plugged in but not readable (Linux: no udev rule yet); 0 elsewhere
//!   cc_hid_rule            → the udev rule file's text that makes them readable (Linux); "" elsewhere
//!   cc_hid_open / close    (id)
//!   cc_hid_recv            (id) → [{ reportId, bytes }]   the reports since this window's last call; `gone: …` when unplugged
//!   cc_midi_list           → [{ id, name, manufacturer }]   inputs only
//!   cc_midi_open / close   (id)
//!   cc_midi_recv           (id) → [{ bytes, t }]   whole messages; system exclusive, clock and active sensing never passed on
//!
//! Rules (enforced here, tested in this file):
//!   * **Bytes only, input only.** Nothing is ever written to a device.
//!   * **Only multi-axis controllers.** A HID device is listed, and can be opened, only when one of its top-level collections is
//!     Generic Desktop / Multi-axis Controller (usage page 1, usage 8). When the descriptor cannot be read, the 3D mouse maker's
//!     vendor id decides. Keyboards, mice and everything else on the computer stay out of reach.
//!   * **Each window has its own queue** (Studio's app windows each run the page layer), at most MAX_QUEUE entries; the oldest
//!     are dropped. One reader per device however many windows listen; it stops when the last one closes or its window ends.
//!
//! Per platform:
//!   * Linux: no library at all. hidraw nodes through sysfs (`/sys/class/hidraw`, the report descriptor decides) and ALSA raw
//!     MIDI nodes (`/dev/snd/midiC*D*`), read without blocking. hidraw nodes belong to root unless a udev rule gives the seat
//!     access: `linux/70-cadcraft-3dmouse.rules`, installed by the .deb (stores/linux/build.mjs); with any other package the
//!     page says so and offers the file to save (cc_hid_blocked, cc_hid_rule).
//!   * macOS and Windows: the `hidapi` crate (IOHIDManager, not seizing the device so the maker's driver keeps working; hid.dll
//!     through the crate's own Rust code) and `midir` (CoreMIDI; WinMM).
//!
//! The std-only core (descriptor parser, MIDI byte parser, the per-window hub, the Linux readers) compiles and passes alone:
//! `rustc --edition 2021 --test --cfg standalone devices.rs && ./devices`. The Tauri commands are behind `cfg(not(standalone))`.
//! lib.rs: `mod devices;`, `.manage(devices::Devices::default())`, the ten names in generate_handler! (computers only), and
//! `devices::on_window_destroyed`. bridge.js: `devices: { hidList, hidBlocked, hidOpen, hidClose, hidRecv, midiList, … }`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// The most entries one window's queue keeps for one device (a 3D mouse sends about 60–125 reports a second).
pub const MAX_QUEUE: usize = 256;
/// The most devices of each kind open at once.
pub const MAX_OPEN: usize = 8;
/// The 3D mouse maker's USB vendor id (3Dconnexion); older devices carry Logitech's and are found by their usage.
pub const SPACE_VENDOR: u16 = 0x256f;

#[cfg_attr(not(standalone), derive(serde::Serialize), serde(rename_all = "camelCase"))]
#[derive(Clone, Debug, PartialEq)]
pub struct HidRow {
    pub id: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub product_name: String,
    pub usage_page: u16,
    pub usage: u16,
}

#[cfg_attr(not(standalone), derive(serde::Serialize), serde(rename_all = "camelCase"))]
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub report_id: u8,
    pub bytes: Vec<u8>,
}

#[cfg_attr(not(standalone), derive(serde::Serialize))]
#[derive(Clone, Debug, PartialEq)]
pub struct MidiRow {
    pub id: String,
    pub name: String,
    pub manufacturer: String,
}

#[cfg_attr(not(standalone), derive(serde::Serialize))]
#[derive(Clone, Debug, PartialEq)]
pub struct MidiMsg {
    pub bytes: Vec<u8>,
    /// 0: the page stamps the message when it reads it.
    pub t: f64,
}

// ---------------------------------------------------------------------------------------------------------------------
// HID report descriptor: which top-level collections a device has, and whether its reports carry a report id.

#[derive(Debug, Default, PartialEq)]
pub struct Descriptor {
    /// (usage page, usage) of each top-level collection, in order.
    pub tops: Vec<(u16, u16)>,
    /// true when the descriptor has a Report ID item: every report then starts with its id.
    pub numbered: bool,
}

pub fn parse_descriptor(d: &[u8]) -> Descriptor {
    let mut out = Descriptor::default();
    let (mut page, mut depth, mut i) = (0u16, 0usize, 0usize);
    let mut usage: Option<u32> = None;
    while i < d.len() {
        let b = d[i];
        if b == 0xfe {
            // long item: size byte, tag byte, data
            let n = *d.get(i + 1).unwrap_or(&0) as usize;
            i += 3 + n;
            continue;
        }
        let n = match b & 3 { 3 => 4, k => k as usize };
        if i + 1 + n > d.len() { break; }
        let mut v = 0u32;
        for k in 0..n { v |= (d[i + 1 + k] as u32) << (8 * k); }
        let (kind, tag) = ((b >> 2) & 3, b >> 4);
        match (kind, tag) {
            (1, 0) => page = v as u16,
            (1, 8) => out.numbered = true,
            (2, 0) => { if usage.is_none() { usage = Some(if n == 4 { v } else { ((page as u32) << 16) | (v & 0xffff) }); } }
            (0, 0xa) => {
                if depth == 0 {
                    let u = usage.unwrap_or((page as u32) << 16);
                    out.tops.push(((u >> 16) as u16, (u & 0xffff) as u16));
                }
                depth += 1;
                usage = None;
            }
            (0, 0xc) => { depth = depth.saturating_sub(1); usage = None; }
            (0, _) => usage = None,
            _ => {}
        }
        i += 1 + n;
    }
    out
}

/// Is this a device the page may see? `tops` = its top-level collections when the descriptor was readable.
pub fn space_device(vendor: u16, tops: Option<&[(u16, u16)]>) -> bool {
    match tops {
        Some(t) => t.iter().any(|&(p, u)| p == 1 && u == 8),
        None => vendor == SPACE_VENDOR,
    }
}

/// One raw read into WebHID's shape: the report id apart from the bytes.
pub fn split_report(numbered: bool, buf: &[u8]) -> Option<Report> {
    if buf.is_empty() { return None; }
    Some(if numbered { Report { report_id: buf[0], bytes: buf[1..].to_vec() } } else { Report { report_id: 0, bytes: buf.to_vec() } })
}

// ---------------------------------------------------------------------------------------------------------------------
// MIDI: a byte stream into whole messages. System exclusive, timing clock and active sensing are dropped.

#[derive(Default)]
pub struct MidiParser {
    status: u8,
    data: [u8; 2],
    have: usize,
    sysex: bool,
}

fn midi_data_bytes(status: u8) -> usize {
    match status & 0xf0 {
        0xc0 | 0xd0 => 1,
        0xf0 => match status { 0xf1 | 0xf3 => 1, 0xf2 => 2, _ => 0 },
        _ => 2,
    }
}

/// Should a whole message (from a library that already split them) be passed on?
pub fn midi_passes(msg: &[u8]) -> bool {
    !matches!(msg.first(), None | Some(0xf0) | Some(0xf7) | Some(0xf8) | Some(0xfe)) && msg[0] & 0x80 != 0 && msg.len() <= 3
}

impl MidiParser {
    pub fn feed(&mut self, b: u8, out: &mut Vec<Vec<u8>>) {
        if b >= 0xf8 {
            // real-time bytes may come between any two bytes and never end a message
            if b != 0xf8 && b != 0xfe { out.push(vec![b]); }
            return;
        }
        if b & 0x80 != 0 {
            self.sysex = b == 0xf0;
            self.have = 0;
            self.status = if b == 0xf0 || b == 0xf7 { 0 } else { b };
            if self.status != 0 && midi_data_bytes(b) == 0 {
                if b == 0xf6 { out.push(vec![b]); }
                self.status = 0;
            }
            return;
        }
        if self.sysex || self.status == 0 { return; }
        self.data[self.have] = b;
        self.have += 1;
        if self.have == midi_data_bytes(self.status) {
            let mut m = vec![self.status];
            m.extend_from_slice(&self.data[..self.have]);
            out.push(m);
            self.have = 0;
            if self.status >= 0xf0 { self.status = 0; }   // running status is for channel messages only
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The hub: one reader per device, one queue per window.

struct Source<T> {
    subs: HashMap<String, VecDeque<T>>,
    stop: Arc<AtomicBool>,
    gone: bool,
}

pub struct Hub<T> {
    inner: Arc<Mutex<HashMap<String, Source<T>>>>,
}

impl<T> Default for Hub<T> {
    fn default() -> Self { Hub { inner: Arc::new(Mutex::new(HashMap::new())) } }
}

/// The reader's end: push what the device sends, look at `stopped()`, say `gone()` when the device left.
pub struct Feed<T> {
    id: String,
    stop: Arc<AtomicBool>,
    inner: Arc<Mutex<HashMap<String, Source<T>>>>,
}

impl<T: Clone> Feed<T> {
    pub fn push(&self, v: T) {
        let mut g = self.inner.lock().unwrap();
        let Some(s) = g.get_mut(&self.id) else { return };
        if !Arc::ptr_eq(&s.stop, &self.stop) { return; }
        for q in s.subs.values_mut() {
            if q.len() >= MAX_QUEUE { q.pop_front(); }
            q.push_back(v.clone());
        }
    }
    pub fn stopped(&self) -> bool { self.stop.load(Ordering::Relaxed) }
    pub fn gone(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let mut g = self.inner.lock().unwrap();
        if let Some(s) = g.get_mut(&self.id) { if Arc::ptr_eq(&s.stop, &self.stop) { s.gone = true; } }
    }
}

pub enum Opened<T> {
    /// Another window already reads this device: this window now has its own queue.
    Joined,
    /// The first window: start a reader with this feed (and call `Hub::failed` when the device would not open).
    Start(Feed<T>),
}

impl<T: Clone> Hub<T> {
    pub fn open(&self, id: &str, who: &str) -> Result<Opened<T>, String> {
        let mut g = self.inner.lock().unwrap();
        if let Some(s) = g.get_mut(id) {
            if !s.gone {
                s.subs.entry(who.to_string()).or_default();
                return Ok(Opened::Joined);
            }
            g.remove(id);   // unplugged and plugged in again: start over
        }
        if g.len() >= MAX_OPEN { return Err("limit: too many devices are open".into()); }
        let stop = Arc::new(AtomicBool::new(false));
        let mut subs = HashMap::new();
        subs.insert(who.to_string(), VecDeque::new());
        g.insert(id.to_string(), Source { subs, stop: stop.clone(), gone: false });
        Ok(Opened::Start(Feed { id: id.to_string(), stop, inner: self.inner.clone() }))
    }
    /// The reader could not start: forget the device.
    pub fn failed(&self, id: &str) {
        if let Some(s) = self.inner.lock().unwrap().remove(id) { s.stop.store(true, Ordering::Relaxed); }
    }
    pub fn close(&self, id: &str, who: &str) {
        let mut g = self.inner.lock().unwrap();
        let Some(s) = g.get_mut(id) else { return };
        s.subs.remove(who);
        if s.subs.is_empty() { s.stop.store(true, Ordering::Relaxed); g.remove(id); }
    }
    pub fn recv(&self, id: &str, who: &str) -> Result<Vec<T>, String> {
        let mut g = self.inner.lock().unwrap();
        let Some(s) = g.get_mut(id) else { return Err("closed: this device is not open".into()) };
        if s.gone {
            s.subs.remove(who);
            if s.subs.is_empty() { g.remove(id); }
            return Err("gone: the device was unplugged".into());
        }
        match s.subs.get_mut(who) {
            Some(q) => Ok(q.drain(..).collect()),
            None => Err("closed: this device is not open".into()),
        }
    }
    /// A window ended: its queues go, and readers nobody listens to stop.
    pub fn drop_window(&self, who: &str) {
        let mut g = self.inner.lock().unwrap();
        g.retain(|_, s| {
            s.subs.remove(who);
            if s.subs.is_empty() { s.stop.store(true, Ordering::Relaxed); }
            !s.subs.is_empty()
        });
    }
    /// Devices that left the list are gone for whoever still listens.
    pub fn keep_only(&self, live: &dyn Fn(&str) -> bool) {
        for (id, s) in self.inner.lock().unwrap().iter_mut() {
            if !s.gone && !live(id) { s.gone = true; s.stop.store(true, Ordering::Relaxed); }
        }
    }
    #[cfg(test)]
    fn open_count(&self) -> usize { self.inner.lock().unwrap().len() }
}

/// Letters, digits, `-`, `_`, `.` only, at most 40: what goes into an id from a device's own text.
pub fn tidy(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')).take(40).collect()
}

// ---------------------------------------------------------------------------------------------------------------------
// Linux: hidraw and ALSA raw MIDI nodes, std only.

#[cfg(target_os = "linux")]
pub mod linux {
    use super::*;
    use std::fs;
    use std::io::{ErrorKind, Read};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::{Duration, Instant};

    /// O_NONBLOCK on the Linux targets Studio is built for (x86, x86_64, arm, aarch64, riscv).
    const O_NONBLOCK: i32 = 0o4000;

    pub struct Roots {
        pub sys_hidraw: PathBuf,
        pub dev: PathBuf,
        pub dev_snd: PathBuf,
        pub proc_asound: PathBuf,
    }
    impl Default for Roots {
        fn default() -> Self {
            Roots { sys_hidraw: "/sys/class/hidraw".into(), dev: "/dev".into(), dev_snd: "/dev/snd".into(), proc_asound: "/proc/asound".into() }
        }
    }

    pub struct HidNode {
        pub row: HidRow,
        pub node: PathBuf,
        pub numbered: bool,
        pub readable: bool,
    }

    fn open_nonblocking(p: &Path) -> std::io::Result<fs::File> {
        fs::OpenOptions::new().read(true).custom_flags(O_NONBLOCK).open(p)
    }

    /// Can this person read the node? Tried once per node (an open wakes the device), again when udev changes its access.
    fn readable(p: &Path) -> bool {
        static SEEN: Mutex<Option<HashMap<PathBuf, (u64, i64, i64, bool)>>> = Mutex::new(None);
        let Ok(m) = fs::metadata(p) else { return false };
        let key = (m.ino(), m.ctime(), m.ctime_nsec());
        let mut g = SEEN.lock().unwrap();
        let map = g.get_or_insert_with(HashMap::new);
        if let Some(&(a, b, c, ok)) = map.get(p) { if (a, b, c) == key { return ok; } }
        let ok = open_nonblocking(p).is_ok();
        if map.len() > 256 { map.clear(); }
        map.insert(p.to_path_buf(), (key.0, key.1, key.2, ok));
        ok
    }

    fn hidraw_number(name: &str) -> Option<u32> { name.strip_prefix("hidraw")?.parse().ok() }

    /// Every multi-axis controller on this computer, readable or not, in hidraw order.
    pub fn scan_hid(r: &Roots) -> Vec<HidNode> {
        let mut names: Vec<(u32, String)> = fs::read_dir(&r.sys_hidraw).into_iter().flatten().flatten()
            .filter_map(|e| { let n = e.file_name().to_string_lossy().into_owned(); hidraw_number(&n).map(|k| (k, n)) }).collect();
        names.sort();
        let mut out: Vec<HidNode> = Vec::new();
        for (_, name) in names {
            let dir = r.sys_hidraw.join(&name).join("device");
            let Ok(uevent) = fs::read_to_string(dir.join("uevent")) else { continue };
            let field = |k: &str| uevent.lines().find_map(|l| l.strip_prefix(k).and_then(|v| v.strip_prefix('='))).unwrap_or("").trim().to_string();
            let ids: Vec<u32> = field("HID_ID").split(':').filter_map(|x| u32::from_str_radix(x, 16).ok()).collect();
            if ids.len() != 3 { continue; }
            let (vendor, product) = (ids[1] as u16, ids[2] as u16);
            let desc = fs::read(dir.join("report_descriptor")).ok().filter(|d| !d.is_empty()).map(|d| parse_descriptor(&d));
            if !space_device(vendor, desc.as_ref().map(|d| d.tops.as_slice())) { continue; }
            let uniq = tidy(&field("HID_UNIQ"));
            let same = out.iter().filter(|n| n.row.vendor_id == vendor && n.row.product_id == product).count();
            let node = r.dev.join(&name);
            out.push(HidNode {
                row: HidRow {
                    id: format!("hid:{vendor:04x}:{product:04x}:{}", if uniq.is_empty() { same.to_string() } else { format!("{uniq}.{same}") }),
                    vendor_id: vendor, product_id: product,
                    product_name: field("HID_NAME").chars().take(120).collect(),
                    usage_page: 1, usage: 8,
                },
                // every 3D mouse numbers its reports; an unreadable descriptor is treated the same way
                numbered: desc.as_ref().map_or(true, |d| d.numbered),
                readable: readable(&node),
                node,
            });
        }
        out
    }

    /// Reads until `stopped()`: sleeps a little when the device is quiet, says `gone()` when the node goes away.
    fn pump(mut f: fs::File, node: PathBuf, stopped: impl Fn() -> bool, gone: impl Fn(), mut take: impl FnMut(&[u8])) {
        let mut buf = [0u8; 256];
        let (mut idle, mut looked) = (0u32, Instant::now());
        while !stopped() {
            match f.read(&mut buf) {
                Ok(n) if n > 0 => { idle = 0; take(&buf[..n]); continue; }
                Ok(_) => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                Err(_) => { gone(); return; }
            }
            if looked.elapsed() > Duration::from_millis(500) {
                looked = Instant::now();
                if !node.exists() { gone(); return; }
            }
            idle = (idle + 1).min(8);
            thread::sleep(Duration::from_millis(if idle < 4 { 1 } else { 4 }));
        }
    }

    pub fn hid_start(r: &Roots, id: &str, feed: Feed<Report>) -> Result<(), String> {
        let Some(n) = scan_hid(r).into_iter().find(|n| n.row.id == id) else { return Err("gone: the device is not plugged in".into()) };
        let f = open_nonblocking(&n.node).map_err(|e| match e.kind() {
            ErrorKind::PermissionDenied => "blocked: this computer does not let you read the device yet".to_string(),
            _ => format!("failed: {}", e.kind()),
        })?;
        let (numbered, feed) = (n.numbered, Arc::new(feed));
        let (a, b, c) = (feed.clone(), feed.clone(), feed);
        thread::Builder::new().name("cc-hid".into())
            .spawn(move || pump(f, n.node, move || a.stopped(), move || b.gone(), move |bytes| { if let Some(rep) = split_report(numbered, bytes) { c.push(rep); } }))
            .map(|_| ()).map_err(|e| format!("failed: {e}"))
    }

    fn midi_numbers(name: &str) -> Option<(u32, u32)> {
        let (c, d) = name.strip_prefix("midiC")?.split_once('D')?;
        Some((c.parse().ok()?, d.parse().ok()?))
    }

    /// The raw MIDI inputs: [(row, node)].
    pub fn scan_midi(r: &Roots) -> Vec<(MidiRow, PathBuf)> {
        let mut nodes: Vec<(u32, u32, String)> = fs::read_dir(&r.dev_snd).into_iter().flatten().flatten()
            .filter_map(|e| { let n = e.file_name().to_string_lossy().into_owned(); midi_numbers(&n).map(|(c, d)| (c, d, n)) }).collect();
        nodes.sort();
        let mut out = Vec::new();
        for (c, d, name) in nodes {
            // /proc/asound/card<c>/midi<d>: the device's name on the first line, then "Input 0" / "Output 0" blocks
            let info = fs::read_to_string(r.proc_asound.join(format!("card{c}")).join(format!("midi{d}"))).ok();
            if let Some(t) = &info { if !t.lines().any(|l| l.trim_start().starts_with("Input")) { continue; } }
            let label = info.as_deref().and_then(|t| t.lines().next()).map(str::trim).filter(|l| !l.is_empty()).map(str::to_string)
                .or_else(|| fs::read_to_string(r.proc_asound.join(format!("card{c}")).join("id")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
                .unwrap_or_else(|| format!("MIDI {c}:{d}"));
            out.push((MidiRow { id: format!("midi:{c}:{d}:{}", tidy(&label)), name: label.chars().take(120).collect(), manufacturer: String::new() }, r.dev_snd.join(name)));
        }
        out
    }

    pub fn midi_start(r: &Roots, id: &str, feed: Feed<MidiMsg>) -> Result<(), String> {
        let Some((_, node)) = scan_midi(r).into_iter().find(|(row, _)| row.id == id) else { return Err("gone: the device is not plugged in".into()) };
        let f = open_nonblocking(&node).map_err(|e| match (e.kind(), e.raw_os_error()) {
            (ErrorKind::PermissionDenied, _) => "blocked: this computer does not let you read the device".to_string(),
            (_, Some(11)) | (_, Some(16)) => "busy: another program is using this MIDI input".to_string(),   // EAGAIN, EBUSY
            (k, _) => format!("failed: {k}"),
        })?;
        let feed = Arc::new(feed);
        let (a, b, c) = (feed.clone(), feed.clone(), feed);
        let mut parser = MidiParser::default();
        let mut msgs = Vec::new();
        thread::Builder::new().name("cc-midi".into())
            .spawn(move || pump(f, node, move || a.stopped(), move || b.gone(), move |bytes| {
                for &x in bytes { parser.feed(x, &mut msgs); }
                for m in msgs.drain(..) { c.push(MidiMsg { bytes: m, t: 0.0 }); }
            }))
            .map(|_| ()).map_err(|e| format!("failed: {e}"))
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// macOS and Windows: hidapi and midir.

// `--cfg standalone --cfg native_check` builds this module without Tauri (test/devices-native: a small program that lists
// and reads real devices on a Mac or a Windows computer).
#[cfg(all(any(not(standalone), native_check), any(target_os = "macos", windows)))]
pub mod native {
    use super::*;
    use hidapi::HidApi;
    use midir::{Ignore, MidiInput};
    use std::ffi::CString;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    const CLIENT: &str = "CadCraft Studio";

    fn hid_rows(api: &HidApi) -> Vec<(HidRow, CString)> {
        let mut out: Vec<(HidRow, CString)> = Vec::new();
        for d in api.device_list() {
            if !(d.usage_page() == 1 && d.usage() == 8) { continue; }
            if out.iter().any(|(_, p)| p.as_c_str() == d.path()) { continue; }
            let (vendor, product) = (d.vendor_id(), d.product_id());
            let uniq = tidy(d.serial_number().unwrap_or(""));
            let same = out.iter().filter(|(r, _)| r.vendor_id == vendor && r.product_id == product).count();
            out.push((HidRow {
                id: format!("hid:{vendor:04x}:{product:04x}:{}", if uniq.is_empty() { same.to_string() } else { format!("{uniq}.{same}") }),
                vendor_id: vendor, product_id: product,
                product_name: d.product_string().unwrap_or("").chars().take(120).collect(),
                usage_page: 1, usage: 8,
            }, d.path().to_owned()));
        }
        out
    }

    pub fn hid_list() -> Vec<HidRow> {
        HidApi::new().map(|api| hid_rows(&api).into_iter().map(|(r, _)| r).collect()).unwrap_or_default()
    }

    pub fn hid_start(id: &str, feed: Feed<Report>) -> Result<(), String> {
        let (id, (tx, rx)) = (id.to_string(), mpsc::channel::<Result<(), String>>());
        thread::Builder::new().name("cc-hid".into()).spawn(move || {
            let opened = (|| {
                let api = HidApi::new().map_err(|e| format!("failed: {e}"))?;
                let (_, path) = hid_rows(&api).into_iter().find(|(r, _)| r.id == id).ok_or_else(|| "gone: the device is not plugged in".to_string())?;
                api.open_path(&path).map_err(|e| format!("failed: {e}"))
            })();
            let dev = match opened { Ok(d) => { let _ = tx.send(Ok(())); d } Err(e) => { let _ = tx.send(Err(e)); return; } };
            let mut desc = [0u8; 4096];
            // every 3D mouse numbers its reports; a descriptor that cannot be read is treated the same way
            let numbered = dev.get_report_descriptor(&mut desc).map(|n| parse_descriptor(&desc[..n.min(desc.len())]).numbered).unwrap_or(true);
            let mut buf = [0u8; 256];
            while !feed.stopped() {
                match dev.read_timeout(&mut buf, 100) {
                    Ok(0) => {}
                    Ok(n) => { if let Some(rep) = split_report(numbered, &buf[..n.min(buf.len())]) { feed.push(rep); } }
                    Err(_) => { feed.gone(); return; }
                }
            }
        }).map_err(|e| format!("failed: {e}"))?;
        rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| Err("failed: the device did not open in time".into()))
    }

    fn midi_in() -> Option<MidiInput> {
        let mut m = MidiInput::new(CLIENT).ok()?;
        m.ignore(Ignore::All);
        Some(m)
    }

    pub fn midi_list() -> Vec<MidiRow> {
        let Some(m) = midi_in() else { return Vec::new() };
        m.ports().iter().map(|p| {
            let name = m.port_name(p).unwrap_or_default();
            MidiRow { id: format!("midi:{}", p.id()), name: name.chars().take(120).collect(), manufacturer: String::new() }
        }).collect()
    }

    pub fn midi_start(id: &str, feed: Feed<MidiMsg>) -> Result<(), String> {
        let (id, (tx, rx)) = (id.to_string(), mpsc::channel::<Result<(), String>>());
        thread::Builder::new().name("cc-midi".into()).spawn(move || {
            let feed = Arc::new(feed);
            let sink = feed.clone();
            let opened = (|| {
                let m = midi_in().ok_or_else(|| "failed: MIDI is not available".to_string())?;
                let port = m.ports().into_iter().find(|p| format!("midi:{}", p.id()) == id).ok_or_else(|| "gone: the device is not plugged in".to_string())?;
                m.connect(&port, CLIENT, move |_, msg, _| { if midi_passes(msg) { sink.push(MidiMsg { bytes: msg.to_vec(), t: 0.0 }); } }, ())
                    .map_err(|e| format!("failed: {e}"))
            })();
            // the connection lives on this thread until the last window closes the device
            let _conn = match opened { Ok(c) => { let _ = tx.send(Ok(())); c } Err(e) => { let _ = tx.send(Err(e)); return; } };
            while !feed.stopped() { thread::sleep(Duration::from_millis(50)); }
        }).map_err(|e| format!("failed: {e}"))?;
        rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| Err("failed: the device did not open in time".into()))
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The commands.

#[derive(Default)]
pub struct Devices {
    hid: Hub<Report>,
    midi: Hub<MidiMsg>,
}

#[cfg(not(standalone))]
mod commands {
    use super::*;
    use tauri::{AppHandle, Manager, Runtime, State, WebviewWindow};

    #[cfg(target_os = "linux")]
    mod os {
        use super::super::*;
        /// Test seam, debug builds only (as files.rs CC_TEST_ANSWERS): CC_TEST_DEVICES names a folder with stand-in `sys`, `dev`,
        /// `snd` and `proc` trees (test/devices-e2e.mjs). A release build always reads the computer's own.
        fn roots() -> linux::Roots {
            #[cfg(debug_assertions)]
            if let Some(d) = std::env::var_os("CC_TEST_DEVICES") {
                let d = std::path::PathBuf::from(d);
                return linux::Roots { sys_hidraw: d.join("sys"), dev: d.join("dev"), dev_snd: d.join("snd"), proc_asound: d.join("proc") };
            }
            linux::Roots::default()
        }
        pub fn hid_list() -> Vec<HidRow> { linux::scan_hid(&roots()).into_iter().filter(|n| n.readable).map(|n| n.row).collect() }
        pub fn hid_blocked() -> u32 { linux::scan_hid(&roots()).iter().filter(|n| !n.readable).count() as u32 }
        pub fn hid_rule() -> &'static str { include_str!("../../linux/70-cadcraft-3dmouse.rules") }
        pub fn hid_start(id: &str, feed: Feed<Report>) -> Result<(), String> { linux::hid_start(&roots(), id, feed) }
        pub fn midi_list() -> Vec<MidiRow> { linux::scan_midi(&roots()).into_iter().map(|(r, _)| r).collect() }
        pub fn midi_start(id: &str, feed: Feed<MidiMsg>) -> Result<(), String> { linux::midi_start(&roots(), id, feed) }
    }
    #[cfg(any(target_os = "macos", windows))]
    mod os {
        pub use super::super::native::*;
        pub fn hid_blocked() -> u32 { 0 }
        pub fn hid_rule() -> &'static str { "" }
    }

    // async: a device list or an open can take a moment, and these must not hold the window's own thread.
    #[tauri::command]
    pub async fn cc_hid_list(d: State<'_, Devices>) -> Result<Vec<HidRow>, String> {
        let rows = os::hid_list();
        d.hid.keep_only(&|id| rows.iter().any(|r| r.id == id));
        Ok(rows)
    }

    #[tauri::command]
    pub async fn cc_hid_blocked() -> Result<u32, String> {
        Ok(os::hid_blocked())
    }

    #[tauri::command]
    pub fn cc_hid_rule() -> String {
        os::hid_rule().to_string()
    }

    #[tauri::command]
    pub async fn cc_hid_open<R: Runtime>(window: WebviewWindow<R>, d: State<'_, Devices>, id: String) -> Result<(), String> {
        if !os::hid_list().iter().any(|r| r.id == id) { return Err("gone: the device is not plugged in".into()); }
        match d.hid.open(&id, window.label())? {
            Opened::Joined => Ok(()),
            Opened::Start(feed) => os::hid_start(&id, feed).map_err(|e| { d.hid.failed(&id); e }),
        }
    }

    #[tauri::command]
    pub async fn cc_hid_close<R: Runtime>(window: WebviewWindow<R>, d: State<'_, Devices>, id: String) -> Result<(), String> {
        d.hid.close(&id, window.label());
        Ok(())
    }

    #[tauri::command]
    pub async fn cc_hid_recv<R: Runtime>(window: WebviewWindow<R>, d: State<'_, Devices>, id: String) -> Result<Vec<Report>, String> {
        d.hid.recv(&id, window.label())
    }

    #[tauri::command]
    pub async fn cc_midi_list(d: State<'_, Devices>) -> Result<Vec<MidiRow>, String> {
        let rows = os::midi_list();
        d.midi.keep_only(&|id| rows.iter().any(|r| r.id == id));
        Ok(rows)
    }

    #[tauri::command]
    pub async fn cc_midi_open<R: Runtime>(window: WebviewWindow<R>, d: State<'_, Devices>, id: String) -> Result<(), String> {
        if !os::midi_list().iter().any(|r| r.id == id) { return Err("gone: the device is not plugged in".into()); }
        match d.midi.open(&id, window.label())? {
            Opened::Joined => Ok(()),
            Opened::Start(feed) => os::midi_start(&id, feed).map_err(|e| { d.midi.failed(&id); e }),
        }
    }

    #[tauri::command]
    pub async fn cc_midi_close<R: Runtime>(window: WebviewWindow<R>, d: State<'_, Devices>, id: String) -> Result<(), String> {
        d.midi.close(&id, window.label());
        Ok(())
    }

    #[tauri::command]
    pub async fn cc_midi_recv<R: Runtime>(window: WebviewWindow<R>, d: State<'_, Devices>, id: String) -> Result<Vec<MidiMsg>, String> {
        d.midi.recv(&id, window.label())
    }

    /// A window's end closes what it had open.
    pub fn on_window_destroyed<R: Runtime>(app: &AppHandle<R>, label: &str) {
        if let Some(d) = app.try_state::<Devices>() {
            d.hid.drop_window(label);
            d.midi.drop_window(label);
        }
    }
}
#[cfg(not(standalone))]
pub use commands::*;

#[cfg(test)]
mod tests {
    use super::*;

    // A 3D mouse's descriptor, shortened: Generic Desktop / Multi-axis Controller, report 1 (three 16-bit axes), report 3 (buttons).
    const SPACE: &[u8] = &[
        0x05, 0x01, 0x09, 0x08, 0xa1, 0x01,             // usage page 1, usage 8, collection (application)
        0xa1, 0x00, 0x85, 0x01,                         //   collection (physical), report id 1
        0x16, 0xa2, 0xfe, 0x26, 0x5e, 0x01,             //   logical min -350, max 350
        0x09, 0x30, 0x09, 0x31, 0x09, 0x32,             //   x y z
        0x75, 0x10, 0x95, 0x03, 0x81, 0x06, 0xc0,       //   16 bits × 3, input, end
        0xa1, 0x00, 0x85, 0x03, 0x05, 0x09, 0x19, 0x01, 0x29, 0x02, 0x75, 0x01, 0x95, 0x02, 0x81, 0x02, 0xc0,
        0xc0,
    ];
    // A plain mouse: Generic Desktop / Mouse, no report ids.
    const MOUSE: &[u8] = &[0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x75, 0x01, 0x95, 0x03, 0x81, 0x02, 0xc0, 0xc0];

    #[test]
    fn descriptor_top_level_collections_and_report_ids() {
        assert_eq!(parse_descriptor(SPACE), Descriptor { tops: vec![(1, 8)], numbered: true });
        assert_eq!(parse_descriptor(MOUSE), Descriptor { tops: vec![(1, 2)], numbered: false });
        // two top-level collections; the second one's usage is given with its page (a 4-byte usage)
        let two = [&MOUSE[..], &[0x0b, 0x08, 0x00, 0x01, 0x00, 0xa1, 0x01, 0xc0][..]].concat();
        assert_eq!(parse_descriptor(&two).tops, vec![(1, 2), (1, 8)]);
        // cut short, a long item, nonsense: never a panic
        for n in 0..SPACE.len() { let _ = parse_descriptor(&SPACE[..n]); }
        assert_eq!(parse_descriptor(&[0xfe, 0x02, 0x10, 0xaa, 0xbb, 0x05, 0x01, 0x09, 0x08, 0xa1, 0x01, 0xc0]).tops, vec![(1, 8)]);
        assert_eq!(parse_descriptor(&[0xfe, 0xff]).tops, vec![]);
        assert_eq!(parse_descriptor(&[0xc0, 0xc0, 0x05, 0x01, 0x09, 0x08, 0xa1, 0x01]).tops, vec![(1, 8)]);
    }

    #[test]
    fn only_multi_axis_controllers_are_devices() {
        assert!(space_device(0x256f, Some(&[(1, 8)])));
        assert!(space_device(0x046d, Some(&[(1, 2), (1, 8)])));          // an older 3D mouse under another vendor id
        assert!(!space_device(0x256f, Some(&[(1, 2)])));                 // the same maker's ordinary mouse
        assert!(!space_device(0x046d, Some(&[(1, 6)])));                 // a keyboard
        assert!(space_device(0x256f, None) && !space_device(0x046d, None));   // no descriptor: the vendor id decides
    }

    #[test]
    fn reports_split_from_their_id() {
        assert_eq!(split_report(true, &[1, 0x10, 0x00, 0xf0, 0xff]), Some(Report { report_id: 1, bytes: vec![0x10, 0x00, 0xf0, 0xff] }));
        assert_eq!(split_report(false, &[7, 8]), Some(Report { report_id: 0, bytes: vec![7, 8] }));
        assert_eq!(split_report(true, &[]), None);
    }

    fn midi(bytes: &[u8]) -> Vec<Vec<u8>> {
        let (mut p, mut out) = (MidiParser::default(), Vec::new());
        for &b in bytes { p.feed(b, &mut out); }
        out
    }

    #[test]
    fn midi_bytes_become_whole_messages() {
        assert_eq!(midi(&[0xb0, 7, 100]), vec![vec![0xb0, 7, 100]]);
        // running status: a knob sends the status byte once
        assert_eq!(midi(&[0xb0, 7, 100, 7, 101, 8, 1]), vec![vec![0xb0, 7, 100], vec![0xb0, 7, 101], vec![0xb0, 8, 1]]);
        assert_eq!(midi(&[0xc1, 5, 0x90, 60, 127, 0x80, 60, 0]), vec![vec![0xc1, 5], vec![0x90, 60, 127], vec![0x80, 60, 0]]);
        // a clock byte in the middle of a message neither appears nor breaks it; start (0xfa) passes
        assert_eq!(midi(&[0xb0, 7, 0xf8, 100, 0xfe, 0xfa]), vec![vec![0xb0, 7, 100], vec![0xfa]]);
        // system exclusive never passes, and what follows it does
        assert_eq!(midi(&[0xf0, 0x7e, 0x7f, 6, 1, 0xf7, 0x90, 1, 2]), vec![vec![0x90, 1, 2]]);
        assert_eq!(midi(&[0xf0, 1, 2, 0xb0, 1, 2]), vec![vec![0xb0, 1, 2]]);   // cut off by a status byte
        // data bytes with no status are dropped; song position has two data bytes and no running status
        assert_eq!(midi(&[1, 2, 3, 0xf2, 1, 2, 3, 4]), vec![vec![0xf2, 1, 2]]);
        assert!(midi_passes(&[0xb0, 1, 2]) && !midi_passes(&[0xf0, 1, 0xf7]) && !midi_passes(&[0xf8]) && !midi_passes(&[0xfe]) && !midi_passes(&[]) && !midi_passes(&[1, 2]));
    }

    fn start<T: Clone>(h: &Hub<T>, id: &str, who: &str) -> Feed<T> {
        match h.open(id, who).unwrap() { Opened::Start(f) => f, Opened::Joined => panic!("expected the first open") }
    }

    #[test]
    fn hub_each_window_has_its_own_queue() {
        let h: Hub<u32> = Hub::default();
        let feed = start(&h, "a", "w1");
        assert!(matches!(h.open("a", "w2"), Ok(Opened::Joined)));
        feed.push(1);
        feed.push(2);
        assert_eq!(h.recv("a", "w1"), Ok(vec![1, 2]));
        assert_eq!(h.recv("a", "w1"), Ok(vec![]));
        assert_eq!(h.recv("a", "w2"), Ok(vec![1, 2]));               // the second window missed nothing
        assert!(h.recv("a", "w3").unwrap_err().starts_with("closed"));
        assert!(h.recv("b", "w1").unwrap_err().starts_with("closed"));
        // the reader runs until the last window closes
        h.close("a", "w1");
        assert!(!feed.stopped());
        h.close("a", "w2");
        assert!(feed.stopped() && h.open_count() == 0);
        feed.push(3);                                                // a late push from the old reader goes nowhere
        let again = start(&h, "a", "w1");
        feed.push(4);
        feed.gone();                                                 // and the old reader cannot end the new one
        assert_eq!(h.recv("a", "w1"), Ok(vec![]));
        assert!(!again.stopped());
    }

    #[test]
    fn hub_queue_is_bounded_and_drops_the_oldest() {
        let h: Hub<usize> = Hub::default();
        let feed = start(&h, "a", "w");
        for i in 0..MAX_QUEUE + 10 { feed.push(i); }
        let got = h.recv("a", "w").unwrap();
        assert_eq!((got.len(), got[0], *got.last().unwrap()), (MAX_QUEUE, 10, MAX_QUEUE + 9));
    }

    #[test]
    fn hub_unplugged_window_closed_and_limits() {
        let h: Hub<u32> = Hub::default();
        let feed = start(&h, "a", "w1");
        h.open("a", "w2").ok();
        feed.gone();
        assert!(feed.stopped());
        assert!(h.recv("a", "w1").unwrap_err().starts_with("gone"));
        assert_eq!(h.open_count(), 1);                               // w2 has not heard yet
        assert!(h.recv("a", "w2").unwrap_err().starts_with("gone"));
        assert_eq!(h.open_count(), 0);
        // plugged in again while a window has not heard: a fresh reader
        let f1 = start(&h, "a", "w1");
        f1.gone();
        let f2 = start(&h, "a", "w1");
        assert!(!f2.stopped() && h.recv("a", "w1") == Ok(vec![]));
        // a window's end
        let fb = start(&h, "b", "w2");
        h.open("a", "w2").ok();
        h.drop_window("w2");
        assert!(fb.stopped() && !f2.stopped() && h.open_count() == 1);
        // a device that left the list
        h.keep_only(&|id| id != "a");
        assert!(f2.stopped() && h.recv("a", "w1").unwrap_err().starts_with("gone"));
        // the limit, and a failed start frees its place
        for i in 0..MAX_OPEN { let _ = start(&h, &format!("d{i}"), "w"); }
        assert!(h.open("one-more", "w").err().unwrap().starts_with("limit"));
        h.failed("d0");
        assert!(h.open("one-more", "w").is_ok());
    }

    // The rule file names only the 3D mouse maker's devices, and runs before the seat rules.
    #[test]
    fn udev_rule_names_only_3d_mice() {
        let rule = include_str!("../../linux/70-cadcraft-3dmouse.rules");
        let lines: Vec<&str> = rule.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')).collect();
        assert_eq!(lines.len(), 3);
        for l in &lines {
            assert!(l.starts_with("KERNEL==\"hidraw*\", SUBSYSTEM==\"hidraw\", ") && l.ends_with("TAG+=\"uaccess\""), "{l}");
            assert!(l.contains("256f") || l.contains("256F") || (l.contains("idVendor}==\"046d\"") && l.contains("idProduct}==")), "{l}");
            assert!(!l.contains("MODE=") && !l.contains("GROUP=") && !l.contains("RUN"), "{l}");
        }
    }

    #[test]
    fn ids_take_only_safe_characters() {
        assert_eq!(tidy("AB-12_x.y/../ <script>"), "AB-12_x.y..script");
        assert_eq!(tidy(&"x".repeat(100)).len(), 40);
    }

    #[cfg(target_os = "linux")]
    mod linux_nodes {
        use super::super::linux::*;
        use super::super::*;
        use super::{MOUSE, SPACE};
        use std::fs;
        use std::io::Write;
        use std::path::PathBuf;
        use std::thread::sleep;
        use std::time::{Duration, Instant};

        fn roots(tag: &str) -> (PathBuf, Roots) {
            let base = std::env::temp_dir().join(format!("cc-devices-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&base);
            let r = Roots { sys_hidraw: base.join("sys"), dev: base.join("dev"), dev_snd: base.join("snd"), proc_asound: base.join("proc") };
            for d in [&r.sys_hidraw, &r.dev, &r.dev_snd, &r.proc_asound] { fs::create_dir_all(d).unwrap(); }
            (base, r)
        }
        fn hid(r: &Roots, n: u32, id: &str, name: &str, uniq: &str, desc: Option<&[u8]>) {
            let dir = r.sys_hidraw.join(format!("hidraw{n}")).join("device");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("uevent"), format!("DRIVER=hid-generic\nHID_ID={id}\nHID_NAME={name}\nHID_PHYS=usb-1\nHID_UNIQ={uniq}\n")).unwrap();
            if let Some(d) = desc { fs::write(dir.join("report_descriptor"), d).unwrap(); }
            fs::write(r.dev.join(format!("hidraw{n}")), b"").unwrap();
        }
        fn wait<T>(mut f: impl FnMut() -> Option<T>) -> T {
            let t = Instant::now();
            loop {
                if let Some(v) = f() { return v; }
                assert!(t.elapsed() < Duration::from_secs(5), "timed out");
                sleep(Duration::from_millis(5));
            }
        }

        #[test]
        fn hidraw_lists_only_multi_axis_controllers() {
            let (base, r) = roots("list");
            hid(&r, 0, "0003:000004D9:0000A067", "Some Keyboard", "", Some(&[0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0xc0]));
            hid(&r, 1, "0003:0000256F:0000C635", "3Dconnexion SpaceMouse Compact", "", Some(SPACE));
            hid(&r, 2, "0003:0000256F:0000C652", "3Dconnexion Universal Receiver", "", Some(MOUSE));   // the receiver's mouse interface
            hid(&r, 3, "0003:0000256F:0000C652", "3Dconnexion Universal Receiver", "", Some(SPACE));
            hid(&r, 4, "0005:0000256F:0000C63A", "SpaceMouse Wireless BT", "AA:BB/../cc", None);        // no descriptor: the vendor id decides
            hid(&r, 5, "0003:0000046D:0000C626", "3Dconnexion SpaceNavigator", "", Some(SPACE));
            hid(&r, 6, "0003:0000046D:0000C077", "Logitech Mouse", "", None);
            hid(&r, 10, "0003:0000256F:0000C635", "3Dconnexion SpaceMouse Compact", "", Some(SPACE));   // a second one of the same model
            fs::create_dir_all(r.sys_hidraw.join("hidraw7")).unwrap();                                   // a node with nothing in it
            fs::write(r.sys_hidraw.join("not-a-node"), b"").unwrap();
            let got = scan_hid(&r);
            let ids: Vec<&str> = got.iter().map(|n| n.row.id.as_str()).collect();
            assert_eq!(ids, ["hid:256f:c635:0", "hid:256f:c652:0", "hid:256f:c63a:AABB..cc.0", "hid:046d:c626:0", "hid:256f:c635:1"]);
            assert!(got.iter().all(|n| n.row.usage_page == 1 && n.row.usage == 8 && n.numbered && n.readable));
            assert_eq!(got[0].row.product_name, "3Dconnexion SpaceMouse Compact");
            assert_eq!((got[0].row.vendor_id, got[0].row.product_id), (0x256f, 0xc635));
            assert_eq!(got[1].node, r.dev.join("hidraw3"));
            // a node that is listed in sysfs but that this person cannot open is found, and marked
            fs::remove_file(r.dev.join("hidraw5")).unwrap();
            assert!(!scan_hid(&r).iter().find(|n| n.row.id == "hid:046d:c626:0").unwrap().readable);
            assert!(scan_hid(&Roots { sys_hidraw: base.join("none"), ..Roots::default() }).is_empty());
            let _ = fs::remove_dir_all(base);
        }

        #[test]
        fn hidraw_reports_reach_every_window_and_unplug_is_noticed() {
            let (base, r) = roots("read");
            hid(&r, 2, "0003:0000256F:0000C635", "SpaceMouse", "", Some(SPACE));
            let h: Hub<Report> = Hub::default();
            let id = "hid:256f:c635:0";
            let Ok(Opened::Start(feed)) = h.open(id, "w1") else { panic!() };
            assert!(hid_start(&r, "hid:256f:c635:9", match h.open("other", "w1") { Ok(Opened::Start(f)) => f, _ => panic!() }).unwrap_err().starts_with("gone"));
            hid_start(&r, id, feed).unwrap();
            assert!(matches!(h.open(id, "w2"), Ok(Opened::Joined)));
            let mut node = fs::OpenOptions::new().append(true).open(r.dev.join("hidraw2")).unwrap();
            node.write_all(&[1, 0x5e, 0x01, 0xa2, 0xfe, 0x00, 0x00]).unwrap();   // report 1: x 350, y -350, z 0
            let got = wait(|| h.recv(id, "w1").ok().filter(|v| !v.is_empty()));
            assert_eq!(got, vec![Report { report_id: 1, bytes: vec![0x5e, 0x01, 0xa2, 0xfe, 0x00, 0x00] }]);
            assert_eq!(wait(|| h.recv(id, "w2").ok().filter(|v| !v.is_empty())), got);
            node.write_all(&[3, 0x01]).unwrap();
            assert_eq!(wait(|| h.recv(id, "w1").ok().filter(|v| !v.is_empty())), vec![Report { report_id: 3, bytes: vec![1] }]);
            // unplugged: the node goes away
            fs::remove_file(r.dev.join("hidraw2")).unwrap();
            assert!(wait(|| h.recv(id, "w1").err()).starts_with("gone"));
            let _ = fs::remove_dir_all(base);
        }

        #[test]
        fn raw_midi_inputs_listed_and_read() {
            let (base, r) = roots("midi");
            for (c, d, info) in [(1, 0, Some("nanoKONTROL2\n\nOutput 0\n  Tx bytes     : 0\nInput 0\n  Rx bytes     : 0\n")), (2, 0, Some("Synth Out\n\nOutput 0\n  Tx bytes     : 0\n")), (3, 1, None)] {
                fs::write(r.dev_snd.join(format!("midiC{c}D{d}")), b"").unwrap();
                fs::create_dir_all(r.proc_asound.join(format!("card{c}"))).unwrap();
                if let Some(t) = info { fs::write(r.proc_asound.join(format!("card{c}")).join(format!("midi{d}")), t).unwrap(); }
            }
            fs::write(r.proc_asound.join("card3").join("id"), "Pad\n").unwrap();
            fs::write(r.dev_snd.join("pcmC0D0p"), b"").unwrap();
            let rows: Vec<MidiRow> = scan_midi(&r).into_iter().map(|(row, _)| row).collect();
            assert_eq!(rows, vec![
                MidiRow { id: "midi:1:0:nanoKONTROL2".into(), name: "nanoKONTROL2".into(), manufacturer: String::new() },   // the output-only card 2 is not an input
                MidiRow { id: "midi:3:1:Pad".into(), name: "Pad".into(), manufacturer: String::new() },
            ]);
            let h: Hub<MidiMsg> = Hub::default();
            let id = "midi:1:0:nanoKONTROL2";
            let Ok(Opened::Start(feed)) = h.open(id, "w") else { panic!() };
            midi_start(&r, id, feed).unwrap();
            let mut node = fs::OpenOptions::new().append(true).open(r.dev_snd.join("midiC1D0")).unwrap();
            node.write_all(&[0xb0, 16, 64, 0xf8, 16, 65, 0xf0, 1, 2, 0xf7]).unwrap();
            let got = wait(|| h.recv(id, "w").ok().filter(|v| v.len() >= 2));
            assert_eq!(got, vec![MidiMsg { bytes: vec![0xb0, 16, 64], t: 0.0 }, MidiMsg { bytes: vec![0xb0, 16, 65], t: 0.0 }]);
            // closing stops the reader: nothing more arrives for a window that opens it afterwards
            h.close(id, "w");
            assert!(h.recv(id, "w").unwrap_err().starts_with("closed"));
            let _ = fs::remove_dir_all(base);
        }
    }
}
