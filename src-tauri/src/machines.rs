// SPDX-License-Identifier: Apache-2.0
//! Send to a machine on the local network, the native half (0.8 EG.40; protocol and drivers in studio/hub/machines.js).
//! A web page cannot reach a printer on the person's network (mixed content, CORS, no raw sockets), so this is the page's one way
//! to it, and it is narrow: plain http to a PRIVATE address only (checked again after name resolution, and the request goes to the
//! address that was checked, so a name that changes its answer cannot move it), the four driver families' paths only, three
//! methods, five header names, no redirects, 256 MB, 60 s. The machine's key is kept in the OS credential store under the machine's
//! id; the page sets it and never reads it back: a request carries `X-Api-Key: {{key}}` and this file fills it in.
//!
//! STATUS: written on the VPS, NOT built (rule 25: no cargo here). To mount it, once, on a machine that can build:
//!   lib.rs   `mod machines;` and, in `generate_handler![…]`, under `#[cfg(desktop)]`:
//!            machines::cc_machine_request, machines::cc_machine_key_set, machines::cc_machine_key_has, machines::cc_machine_key_forget
//!   bridge   desktop-shell/bridge/bridge.js: `desk.machines = { request, keySet, keyHas, keyForget }` over `invoke` (see the log of
//!            studio_PART_2, § EG.40) — the answer layout is account.rs's: 2 bytes status · 4 bytes JSON-head length · head · body.
//! `cargo test machines` runs the address and header rules below without a network.

use serde_json::{Map, Value};
use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;
use tauri::ipc::{Request, Response};
use tauri::{Runtime, WebviewWindow};

const SERVICE: &str = "CadCraft Studio machines";
const MAX_BODY: usize = 256 * 1024 * 1024;
const PATH_PREFIXES: &[&str] = &["/api/version", "/api/files/local", "/api/v1/files/usb/", "/server/info", "/server/files/upload", "/machine/status", "/machine/file/gcodes/"];
const HEADERS: &[&str] = &["content-type", "x-api-key", "print-after-upload", "overwrite", "accept"];

/// Private-network addresses only: RFC 1918, link-local, IPv6 unique-local and link-local. Never loopback, never public.
fn private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => a.is_private() || a.is_link_local(),
        IpAddr::V6(a) => {
            let s = a.segments()[0];
            (s & 0xfe00) == 0xfc00 || (s & 0xffc0) == 0xfe80
        }
    }
}

/// `http://host:port/path` → (host, port, path). Plain http only: a LAN machine's certificate is rarely one we could verify.
fn split(url: &str) -> Result<(String, u16, String), String> {
    let rest = url.strip_prefix("http://").ok_or("not_allowed: plain http to a local machine only")?;
    let (auth, path) = match rest.find('/') {
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
    let ok_path = path.starts_with('/')
        && !path.contains("..")
        && !path.contains('#')
        && !path.contains('\\')
        && path.chars().all(|c| c.is_ascii_graphic())
        && PATH_PREFIXES.iter().any(|p| path == *p || path.starts_with(p));
    if !ok_path {
        return Err("not_allowed: that is not a machine upload path".into());
    }
    Ok((host.to_string(), port, path.to_string()))
}

fn header_ok(name: &str) -> bool {
    HEADERS.contains(&name.to_ascii_lowercase().as_str())
}

fn machine_id_ok(id: &str) -> bool {
    id.len() >= 3 && id.len() <= 41 && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !id.starts_with('-')
}

fn key_entry(id: &str) -> Result<keyring::Entry, String> {
    if !machine_id_ok(id) {
        return Err("bad machine id".into());
    }
    keyring::Entry::new(SERVICE, id).map_err(|e| format!("credential store: {e}"))
}

fn guard<R: Runtime>(w: &WebviewWindow<R>, suite: &crate::windows::Suite) -> Result<(), String> {
    if !suite.on {
        return Err("off: not the suite".into());
    }
    let _ = w; // any suite window may send a job; only the hub sets keys (below)
    Ok(())
}

#[tauri::command]
pub fn cc_machine_key_set<R: Runtime>(window: WebviewWindow<R>, suite: tauri::State<'_, crate::windows::Suite>, id: String, key: String) -> Result<(), String> {
    guard(&window, &suite)?;
    if window.label() != "main" {
        return Err("not_allowed: keys are set in Studio's own window".into());
    }
    if key.is_empty() || key.len() > 200 || !key.chars().all(|c| c.is_ascii_graphic()) {
        return Err("bad key".into());
    }
    key_entry(&id)?.set_password(&key).map_err(|e| format!("credential store: {e}"))
}

#[tauri::command]
pub fn cc_machine_key_has<R: Runtime>(window: WebviewWindow<R>, suite: tauri::State<'_, crate::windows::Suite>, id: String) -> Result<bool, String> {
    guard(&window, &suite)?;
    Ok(key_entry(&id)?.get_password().is_ok())
}

#[tauri::command]
pub fn cc_machine_key_forget<R: Runtime>(window: WebviewWindow<R>, suite: tauri::State<'_, crate::windows::Suite>, id: String) -> Result<(), String> {
    guard(&window, &suite)?;
    let _ = key_entry(&id)?.delete_credential();
    Ok(())
}

/// Headers in: x-cc-method (GET | POST | PUT), x-cc-url, x-cc-machine (the id whose key `{{key}}` means), x-cc-headers (a JSON object of
/// the request headers). Body = the raw bytes to send. Answer (raw): 2 bytes status · 4 bytes JSON-head length · head · body.
#[tauri::command(async)]
pub fn cc_machine_request<R: Runtime>(window: WebviewWindow<R>, suite: tauri::State<'_, crate::windows::Suite>, request: Request<'_>) -> Result<Response, String> {
    guard(&window, &suite)?;
    let h = request.headers();
    let get = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).map(String::from);
    let method = get("x-cc-method").unwrap_or_else(|| "GET".into()).to_uppercase();
    if !matches!(method.as_str(), "GET" | "POST" | "PUT") {
        return Err("not_allowed: GET, POST and PUT only".into());
    }
    let (host, port, path) = split(&get("x-cc-url").ok_or("missing url")?)?;
    // Resolve once, require every answer to be private, and connect to the address we checked.
    let addrs: Vec<_> = (host.as_str(), port).to_socket_addrs().map_err(|e| format!("network: cannot find {host}: {e}"))?.collect();
    if addrs.is_empty() || !addrs.iter().all(|a| private(&a.ip())) {
        return Err("not_allowed: that is not an address on your local network".into());
    }
    let ip = addrs[0].ip();
    let url = match ip {
        IpAddr::V4(a) => format!("http://{a}:{port}{path}"),
        IpAddr::V6(a) => format!("http://[{a}]:{port}{path}"),
    };
    let wanted: Map<String, Value> = match get("x-cc-headers") {
        Some(j) => serde_json::from_str::<Value>(&j).ok().and_then(|v| v.as_object().cloned()).ok_or("bad headers")?,
        None => Map::new(),
    };
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(60)))
        .user_agent(concat!("CadCraft-desktop/", env!("CARGO_PKG_VERSION")))
        .build()
        .new_agent();
    let body: Vec<u8> = crate::files::body_bytes(request.body()).unwrap_or_default();
    if body.len() > MAX_BODY {
        return Err("size: over 256 MB".into());
    }
    let machine = get("x-cc-machine").unwrap_or_default();
    let mut hdrs: Vec<(String, String)> = vec![("Host".into(), host.clone())];
    for (k, v) in &wanted {
        if !header_ok(k) {
            return Err(format!("not_allowed: header {k}"));
        }
        let v = v.as_str().ok_or("bad header value")?;
        let v = if v == "{{key}}" {
            key_entry(&machine)?.get_password().map_err(|_| "key: no key is stored for this machine".to_string())?
        } else {
            v.to_string()
        };
        if v.chars().any(|c| c.is_control()) {
            return Err("bad header value".into());
        }
        hdrs.push((k.clone(), v));
    }
    let result = match method.as_str() {
        "GET" => {
            let mut r = agent.get(&url);
            for (k, v) in &hdrs {
                r = r.header(k, v);
            }
            r.call()
        }
        m => {
            let mut r = if m == "POST" { agent.post(&url) } else { agent.put(&url) };
            for (k, v) in &hdrs {
                r = r.header(k, v);
            }
            r.send(&body[..])
        }
    };
    let mut resp = result.map_err(|e| format!("network: {e}"))?;
    let status = resp.status().as_u16();
    let mut head = Map::new();
    if let Some(v) = resp.headers().get("content-type").and_then(|v| v.to_str().ok()) {
        head.insert("content-type".into(), Value::String(v.into()));
    }
    let out_body = resp.body_mut().with_config().limit(8 * 1024 * 1024).read_to_vec().map_err(|e| format!("network: {e}"))?;
    let head = serde_json::to_vec(&Value::Object(head)).unwrap();
    let mut out = Vec::with_capacity(6 + head.len() + out_body.len());
    out.extend_from_slice(&status.to_be_bytes());
    out.extend_from_slice(&(head.len() as u32).to_be_bytes());
    out.extend_from_slice(&head);
    out.extend_from_slice(&out_body);
    Ok(Response::new(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_addresses_only() {
        for ok in ["10.1.2.3", "172.16.0.1", "172.31.9.9", "192.168.1.40", "169.254.7.7", "fd12:3456::1", "fe80::1"] {
            assert!(private(&ok.parse().unwrap()), "{ok}");
        }
        for no in ["8.8.8.8", "172.32.0.1", "127.0.0.1", "0.0.0.0", "100.64.0.1", "2001:db8::1", "::1", "::ffff:8.8.8.8"] {
            assert!(!private(&no.parse().unwrap()), "{no}");
        }
    }

    #[test]
    fn urls_and_paths() {
        assert_eq!(split("http://192.168.1.40:5000/api/version").unwrap(), ("192.168.1.40".into(), 5000, "/api/version".into()));
        assert_eq!(split("http://printer.local/server/info").unwrap().1, 80);
        assert_eq!(split("http://[fd12::1]:7125/server/files/upload").unwrap().0, "fd12::1");
        assert!(split("https://192.168.1.40/api/version").is_err());
        assert!(split("http://192.168.1.40/api/version/../../etc").is_err());
        assert!(split("http://192.168.1.40/admin").is_err());
        assert!(split("http://192.168.1.40/api/files/local#x").is_err());
        assert!(split("http://192.168.1.40").is_err());
        assert!(split("http://:80/api/version").is_err());
        assert!(split("http://192.168.1.40/machine/file/gcodes/a b.gcode").is_err());
    }

    #[test]
    fn headers_and_ids() {
        assert!(header_ok("X-Api-Key") && header_ok("Print-After-Upload") && header_ok("content-type"));
        assert!(!header_ok("Authorization") && !header_ok("Cookie") && !header_ok("Host"));
        assert!(machine_id_ok("ender-3") && !machine_id_ok("-x") && !machine_id_ok("a") && !machine_id_ok("Ender 3"));
    }
}
