// SPDX-License-Identifier: Apache-2.0
//! The Scan button's scanners (Super 0.1, "Start from a scan"): network scanners and office copiers that speak eSCL (Apple's
//! AirScan / Mopria, which nearly every networked scanner sold since about 2015 does), found on this network by mDNS
//! (`_uscan._tcp`) and reached over plain HTTP on the LAN. The page drives the scan (lib/scanstart/escl.js: capabilities,
//! a scan job, its pages); the shell only finds the scanners and carries requests to them, so it holds no scanning logic.
//!
//! - **Who can be reached:** only a scanner this shell found by mDNS a moment ago, by its id, at a private, loopback or
//!   link-local address (the same rule as render nodes). The page never gives a host or a URL.
//! - **What can be asked:** GET, POST or DELETE on a path under that scanner's eSCL root (`rs`, normally `eSCL`): letters,
//!   digits, `/ _ . -` only, no `..`. POST bodies are the page's ScanSettings XML (≤ 64 KiB). Answers ≤ 256 MiB (a page).
//! - **What comes back:** one binary answer: `[status u16 BE][header length u32 BE][headers JSON][body]` — the headers are
//!   `Location` and `Content-Type` only.
//! - Nothing leaves the LAN; no sign-in; no scanner is ever reached without the person pressing Scan.
//!   Not here: TWAIN / WIA / SANE (USB scanners); an office's scan-to-folder file still opens through "A file".

use mdns_sd::{ServiceDaemon, ServiceEvent};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::State;

const SERVICE: &str = "_uscan._tcp.local.";
const MAX_BODY_IN: usize = 64 * 1024;
const MAX_ANSWER: u64 = 256 * 1024 * 1024;

#[derive(Default)]
pub struct Scanners {
    found: Mutex<HashMap<String, Found>>,
    mdns: Mutex<Option<ServiceDaemon>>,
}

#[derive(Clone)]
struct Found {
    host: IpAddr,
    port: u16,
    rs: String,
}

#[derive(Serialize)]
pub struct ScannerFound {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    /// The eSCL root the page's paths start with (TXT `rs`, cleaned: "eSCL" when the scanner gives none).
    pub rs: String,
    /// TXT `is`: input sources the scanner names (platen, adf, …); `cs`: colour modes; `pdl`: formats. As the scanner says.
    pub sources: String,
    pub colors: String,
    pub formats: String,
}

#[derive(Deserialize)]
pub struct BrowseArgs {
    pub ms: Option<u64>,
}

/// Same rule as render nodes: this machine, the LAN's private ranges, link-local, IPv6 unique-local.
fn private_peer(ip: IpAddr) -> bool {
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

/// The eSCL root from TXT `rs` ("eSCL", "/eSCL", ""): plain path characters only.
fn root_ok(rs: &str) -> Option<String> {
    let r = rs.trim().trim_matches('/');
    if r.len() > 64 || !r.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_.-".contains(&b)) || r.contains("..") {
        return None;
    }
    Some(if r.is_empty() { "eSCL".into() } else { r.into() })
}

/// A path the page may ask for: under the scanner's root, plain characters, no `..`.
pub fn path_ok(rs: &str, path: &str) -> bool {
    let p = path.trim_start_matches('/');
    p.len() <= 512
        && (p == rs || p.starts_with(&format!("{rs}/")))
        && !p.contains("..")
        && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"/_.-".contains(&b))
}

impl Scanners {
    fn daemon(&self) -> Result<ServiceDaemon, String> {
        let mut d = self.mdns.lock().unwrap();
        if d.is_none() {
            *d = Some(ServiceDaemon::new().map_err(|e| format!("mdns: {e}"))?);
        }
        Ok(d.clone().unwrap())
    }
}

#[tauri::command]
pub fn cc_scanners_browse(sc: State<'_, Scanners>, args: BrowseArgs) -> Result<Vec<ScannerFound>, String> {
    let wait = Duration::from_millis(args.ms.unwrap_or(2500).clamp(200, 8000));
    let d = sc.daemon()?;
    let rx = d.browse(SERVICE).map_err(|e| format!("mdns: {e}"))?;
    let until = Instant::now() + wait;
    let mut out: Vec<ScannerFound> = vec![];
    let mut keep: HashMap<String, Found> = HashMap::new();
    loop {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok(ServiceEvent::ServiceResolved(s)) => {
                let txt = |k: &str| s.txt_properties.get_property_val_str(k).unwrap_or("").to_string();
                let Some(rs) = root_ok(&txt("rs")) else { continue };
                let mut ips: Vec<IpAddr> = s.addresses.iter().map(|a| a.to_ip_addr()).filter(|ip| private_peer(*ip)).collect();
                ips.sort_by_key(|ip| (!ip.is_ipv4(), *ip));
                let Some(ip) = ips.into_iter().find(|ip| !matches!(ip, IpAddr::V6(a) if (a.segments()[0] & 0xffc0) == 0xfe80)) else { continue };
                let uuid = txt("uuid");
                let id = if uuid.is_empty() { s.fullname.clone() } else { uuid };
                let id: String = id.chars().filter(|c| c.is_ascii_alphanumeric() || "-._".contains(*c)).take(100).collect();
                if id.is_empty() || keep.contains_key(&id) {
                    continue;
                }
                let ty = txt("ty");
                let name = if ty.is_empty() { s.fullname.trim_end_matches(&format!(".{SERVICE}")).to_string() } else { ty };
                keep.insert(id.clone(), Found { host: ip, port: s.port, rs: rs.clone() });
                out.push(ScannerFound {
                    id,
                    name: name.chars().filter(|c| !c.is_control()).take(120).collect(),
                    host: ip.to_string(),
                    port: s.port,
                    rs,
                    sources: txt("is").chars().take(200).collect(),
                    colors: txt("cs").chars().take(200).collect(),
                    formats: txt("pdl").chars().take(400).collect(),
                });
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = d.stop_browse(SERVICE);
    *sc.found.lock().unwrap() = keep;
    Ok(out)
}

#[derive(Deserialize)]
pub struct RequestArgs {
    pub id: String,
    pub method: String,
    pub path: String,
    pub body: Option<String>,
}

#[tauri::command]
pub fn cc_scanner_request(sc: State<'_, Scanners>, args: RequestArgs) -> Result<tauri::ipc::Response, String> {
    let f = sc.found.lock().unwrap().get(&args.id).cloned().ok_or("not_found: that scanner was not found on this network; look again")?;
    if !path_ok(&f.rs, &args.path) {
        return Err("not_allowed: path".into());
    }
    let body = args.body.unwrap_or_default();
    if body.len() > MAX_BODY_IN {
        return Err("size: settings over 64 KiB".into());
    }
    let host = match f.host {
        IpAddr::V6(a) => format!("[{a}]"),
        IpAddr::V4(a) => a.to_string(),
    };
    let url = format!("http://{host}:{}/{}", f.port, args.path.trim_start_matches('/'));
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent(concat!("CadCraft-desktop/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent();
    let result = match args.method.as_str() {
        "GET" => agent.get(&url).call(),
        "DELETE" => agent.delete(&url).call(),
        "POST" => agent.post(&url).header("Content-Type", "text/xml").send(body.as_bytes()),
        _ => return Err("not_allowed: method".into()),
    };
    let mut resp = result.map_err(|e| format!("network: {e}"))?;
    let status = resp.status().as_u16();
    let head = |k: &str| resp.headers().get(k).and_then(|v| v.to_str().ok()).unwrap_or("").chars().take(500).collect::<String>();
    let headers = serde_json::json!({ "location": head("location"), "content-type": head("content-type") }).to_string();
    let data = resp.body_mut().with_config().limit(MAX_ANSWER).read_to_vec().map_err(|e| format!("network: {e}"))?;
    let mut out = Vec::with_capacity(6 + headers.len() + data.len());
    out.extend_from_slice(&status.to_be_bytes());
    out.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    out.extend_from_slice(headers.as_bytes());
    out.extend_from_slice(&data);
    Ok(tauri::ipc::Response::new(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths() {
        assert!(path_ok("eSCL", "/eSCL/ScannerCapabilities"));
        assert!(path_ok("eSCL", "eSCL/ScanJobs/42/NextDocument"));
        assert!(!path_ok("eSCL", "/admin"));
        assert!(!path_ok("eSCL", "/eSCL/../admin"));
        assert!(!path_ok("eSCL", "/eSCLx/ScanJobs"));
        assert!(!path_ok("eSCL", "/eSCL/a?b=c"));
        assert_eq!(root_ok("/eSCL").as_deref(), Some("eSCL"));
        assert_eq!(root_ok("").as_deref(), Some("eSCL"));
        assert_eq!(root_ok("../x"), None);
    }
}
