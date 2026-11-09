// SPDX-License-Identifier: Apache-2.0
//! Read a page of a machine on the local network, the native half of `ccStudioBridge.hostFetch` (0.9 D8.13; the contract is in
//! getcadcraft.com lib/factory/monitor.js). A web page cannot read an MTConnect agent: the agent speaks plain http on the shop
//! network, sends no CORS headers, and an https page may not call plain http. So the page asks here, and this is narrow:
//! GET only, plain http to a PRIVATE address only (the same rule as machines.rs, checked again after name resolution, and the
//! request goes to the address that was checked), the MTConnect paths only (`/probe`, `/current`, `/sample`, `/assets`, each
//! optionally under one device name, with a query string), no headers from the page, no redirects, 8 MB, 15 s. It never writes.
//!
//! STATUS: written on the VPS, NOT compiled (rule 25: no cargo here). Mounted in lib.rs (`mod hostfetch;` and `hostfetch::cc_host_fetch` in
//! `generate_handler![…]`, both `#[cfg(desktop)]`) and in bridge.js (`window.ccStudioBridge.hostFetch`, suite mode, computers only). The
//! first build on a machine with cargo is its first test: fix any compile error here, then run the check below.
//! `cargo test hostfetch` runs the address and path rules below without a network.

use serde_json::{Map, Value};
use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;
use tauri::ipc::Response;
use tauri::{Runtime, WebviewWindow};

const MAX_BODY: usize = 8 * 1024 * 1024;
const ENDPOINTS: &[&str] = &["probe", "current", "sample", "assets"];

/// `http://host:port/[device/]endpoint[?query]` → (host, port, path-and-query). Plain http only.
fn split(url: &str) -> Result<(String, u16, String), String> {
    let rest = url.strip_prefix("http://").ok_or("not_allowed: plain http to a local machine only")?;
    let (auth, target) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => return Err("not_allowed: no path".into()),
    };
    let (host, port) = match auth.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => (h.trim_matches(|c| c == '[' || c == ']'), p.parse::<u16>().map_err(|_| "bad port")?),
        _ => (auth.trim_matches(|c| c == '[' || c == ']'), 80),
    };
    if host.is_empty() || port == 0 {
        return Err("bad address".into());
    }
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (target, None),
    };
    let segs: Vec<&str> = path[1..].split('/').collect();
    let name_ok = |s: &str| !s.is_empty() && s.len() <= 80 && s != "." && s != ".." && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '~'));
    let path_ok = match segs.as_slice() {
        [e] => ENDPOINTS.contains(e),
        [d, e] => name_ok(d) && ENDPOINTS.contains(e),
        _ => false,
    };
    let query_ok = query.map_or(true, |q| q.len() <= 2000 && q.chars().all(|c| c.is_ascii_graphic() && c != '#' && c != '\\'));
    if !path_ok || !query_ok {
        return Err("not_allowed: that is not an MTConnect agent path".into());
    }
    Ok((host.to_string(), port, target.to_string()))
}

/// Argument: `url`. Answer (raw): 2 bytes status · 4 bytes JSON-head length · head · body.
#[tauri::command(async)]
pub fn cc_host_fetch<R: Runtime>(_window: WebviewWindow<R>, suite: tauri::State<'_, crate::windows::Suite>, url: String) -> Result<Response, String> {
    if !suite.on {
        return Err("off: not the suite".into());
    }
    let (host, port, target) = split(&url)?;
    // Resolve once, require every answer to be private, and connect to the address we checked.
    let addrs: Vec<_> = (host.as_str(), port).to_socket_addrs().map_err(|e| format!("network: cannot find {host}: {e}"))?.collect();
    if addrs.is_empty() || !addrs.iter().all(|a| crate::machines::private(&a.ip())) {
        return Err("not_allowed: that is not an address on your local network".into());
    }
    let full = match addrs[0].ip() {
        IpAddr::V4(a) => format!("http://{a}:{port}{target}"),
        IpAddr::V6(a) => format!("http://[{a}]:{port}{target}"),
    };
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(15)))
        .user_agent(concat!("CadCraft-desktop/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent();
    let mut resp = agent.get(&full).header("Host", &host).header("Accept", "application/xml, text/xml").call().map_err(|e| format!("network: {e}"))?;
    let status = resp.status().as_u16();
    let mut head = Map::new();
    if let Some(v) = resp.headers().get("content-type").and_then(|v| v.to_str().ok()) {
        head.insert("content-type".into(), Value::String(v.into()));
    }
    let body = resp.body_mut().with_config().limit(MAX_BODY as u64).read_to_vec().map_err(|e| format!("network: {e}"))?;
    let head = serde_json::to_vec(&Value::Object(head)).unwrap();
    let mut out = Vec::with_capacity(6 + head.len() + body.len());
    out.extend_from_slice(&status.to_be_bytes());
    out.extend_from_slice(&(head.len() as u32).to_be_bytes());
    out.extend_from_slice(&head);
    out.extend_from_slice(&body);
    Ok(Response::new(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_paths() {
        assert_eq!(split("http://192.168.1.50:5000/probe").unwrap(), ("192.168.1.50".into(), 5000, "/probe".into()));
        assert_eq!(split("http://cnc.local/mill-1/current").unwrap().2, "/mill-1/current");
        assert_eq!(split("http://10.0.0.5:5000/sample?from=120&count=500").unwrap().2, "/sample?from=120&count=500");
        assert_eq!(split("http://[fd12::1]:5000/assets").unwrap().0, "fd12::1");
    }

    #[test]
    fn refused() {
        for no in [
            "https://192.168.1.50/probe", "http://192.168.1.50", "http://192.168.1.50/", "http://192.168.1.50/admin", "http://192.168.1.50/a/b/probe",
            "http://192.168.1.50/../probe", "http://192.168.1.50/../current", "http://192.168.1.50/m%20x/probe", "http://192.168.1.50/probe#x",
            "http://192.168.1.50/probe?a b", "http://:80/probe", "http://192.168.1.50:0/probe",
        ] {
            assert!(split(no).is_err(), "{no}");
        }
    }
}
