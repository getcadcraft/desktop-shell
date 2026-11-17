// SPDX-License-Identifier: Apache-2.0
// See Cargo.toml. Prints what devices.rs would hand the page: the 3D mice, the MIDI inputs and, with `read`, their bytes.
#[path = "../../../src-tauri/src/devices.rs"]
#[allow(dead_code)]
mod devices;

#[cfg(any(target_os = "macos", windows))]
fn main() {
    use devices::{native, Hub, MidiMsg, Opened, Report};
    use std::time::{Duration, Instant};
    let t = Instant::now();
    let hid = native::hid_list();
    println!("3D mice: {} ({} ms)", hid.len(), t.elapsed().as_millis());
    for r in &hid { println!("  {}  {:04x}:{:04x}  {}", r.id, r.vendor_id, r.product_id, r.product_name); }
    let t = Instant::now();
    let midi = native::midi_list();
    println!("MIDI inputs: {} ({} ms)", midi.len(), t.elapsed().as_millis());
    for r in &midi { println!("  {}  {}", r.id, r.name); }
    if std::env::args().nth(1).as_deref() != Some("read") { return; }
    let (hh, mh): (Hub<Report>, Hub<MidiMsg>) = (Hub::default(), Hub::default());
    if let Some(r) = hid.first() {
        match hh.open(&r.id, "w") { Ok(Opened::Start(f)) => println!("open {}: {:?}", r.id, native::hid_start(&r.id, f)), _ => {} }
    }
    if let Some(r) = midi.first() {
        match mh.open(&r.id, "w") { Ok(Opened::Start(f)) => println!("open {}: {:?}", r.id, native::midi_start(&r.id, f)), _ => {} }
    }
    println!("move the 3D mouse, turn a knob (10 seconds)…");
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if let Some(r) = hid.first() { match hh.recv(&r.id, "w") { Ok(v) => for x in v { println!("hid  report {} {:02x?}", x.report_id, x.bytes); }, Err(e) => { println!("hid  {e}"); break; } } }
        if let Some(r) = midi.first() { match mh.recv(&r.id, "w") { Ok(v) => for x in v { println!("midi {:02x?}", x.bytes); }, Err(e) => { println!("midi {e}"); break; } } }
        std::thread::sleep(Duration::from_millis(8));
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn main() {
    println!("Linux needs no library: run the tests in devices.rs (rustc --edition 2021 --test --cfg standalone devices.rs).");
}
