// SPDX-License-Identifier: Apache-2.0
//! Signing in to getcadcraft.com with a short code (docs/0.4-PART2.md C.2): the device flow, run by the shell.
//!
//! The app shows a code (ABCD-EFGH), the person approves it on getcadcraft.com/device in their own browser, and the shell
//! gets a token for this one install (scope `sync` on the server). **The token never reaches the page**: it lives in the
//! OS credential store (Windows Credential Manager, macOS Keychain, Secret Service / kernel keyring on Linux), and pages
//! reach the cloud only through `cc_cloud_request`, which adds the bearer itself and goes only to the sync API and
//! /api/v1/me. Sign out revokes the token on the server and deletes it here. No other server secret exists in the shell.

use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::{AppHandle, Manager, Runtime, State};
use tauri_plugin_opener::OpenerExt;

pub const SITE: &str = "https://getcadcraft.com";
const KEY_USER: &str = "getcadcraft.com";

/// What the page may know. Never the token.
#[derive(Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// signed_out | waiting (a code is shown) | signed_in
    pub state: String,
    pub user: Option<Value>,
    pub code: Option<String>,
    pub verification_uri: Option<String>,
    pub expires_at: Option<u64>,
    /// Why the last attempt or call ended: denied | expired | revoked | network | error
    pub reason: Option<String>,
    pub message: Option<String>,
}

pub struct Account {
    /// false = this build has no sign-in at all (store builds until decision 11: shell.json `account: false`).
    pub enabled: bool,
    site: String,
    service: String,
    #[cfg_attr(desktop, allow(dead_code))]
    app_name: String,
    client_name: String,
    status: Mutex<Status>,
    /// Bumped to stop a running poll (cancel, a new sign-in, sign out).
    generation: Mutex<u64>,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "macos",
        "android" => "android",
        "ios" => "ios",
        _ => "linux",
    }
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|s| s.trim().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '.').take(40).collect())
        .unwrap_or_default()
}

fn agent() -> ureq::Agent {
    // Android (TB.1): no system OpenSSL, so rustls, still verified by the system's own certificates (platform verifier).
    #[cfg(target_os = "android")]
    let provider = {
        crate::mobile::tls_ready();
        ureq::tls::TlsProvider::Rustls
    };
    #[cfg(not(target_os = "android"))]
    let provider = ureq::tls::TlsProvider::NativeTls;
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(60)))
        .user_agent(concat!("CadCraft-desktop/", env!("CARGO_PKG_VERSION")))
        // The operating system's own TLS and trust store (no bundled CA list).
        .tls_config(ureq::tls::TlsConfig::builder().provider(provider).root_certs(ureq::tls::RootCerts::PlatformVerifier).build())
        .build()
        .into()
}

impl Account {
    /// The site this build talks to (Studio Live checks relay addresses against it).
    pub fn site(&self) -> &str {
        &self.site
    }

    /// `service` = the app's identifier (one token per install); `app_name` names the install on /device and in the
    /// account's device list ("CadCraft (DESKTOP-7Q)"; the site adds "on Windows").
    /// Touches no credential store: on a desktop without a Secret Service that would hold the window back ~25 s (D-Bus
    /// activation timeout). The stored sign-in is looked up on the first `status()` call, off the UI thread.
    pub fn new(service: &str, app_name: &str, enabled: bool) -> Self {
        let site = debug_site().unwrap_or_else(|| SITE.to_string());
        let host = hostname();
        let client_name = if host.is_empty() { app_name.to_string() } else { format!("{app_name} ({host})") };
        let state = if enabled { "unknown" } else { "off" };
        let status = Status { state: state.into(), ..Default::default() };
        Account { enabled, site, service: service.to_string(), app_name: app_name.to_string(), client_name, status: Mutex::new(status), generation: Mutex::new(0) }
    }

    #[cfg(not(target_os = "android"))]
    fn entry(&self) -> Option<keyring::Entry> {
        keyring::Entry::new(&self.service, KEY_USER).ok()
    }
    #[cfg(not(target_os = "android"))]
    fn token(&self) -> Option<String> {
        self.entry()?.get_password().ok().filter(|t| t.starts_with("ccu_"))
    }
    #[cfg(not(target_os = "android"))]
    fn store(&self, token: &str) -> Result<(), String> {
        self.entry().ok_or("no credential store")?.set_password(token).map_err(|e| format!("credential store: {e}"))
    }
    #[cfg(not(target_os = "android"))]
    fn forget(&self) {
        if let Some(e) = self.entry() {
            let _ = e.delete_credential();
        }
    }
    // Android (TB.1): `keyring` builds there but has no store; the Keystore holds the key that seals the token (mobile.rs).
    #[cfg(target_os = "android")]
    fn token(&self) -> Option<String> {
        crate::mobile::secret::get(&format!("{}:{KEY_USER}", self.service)).filter(|t| t.starts_with("ccu_"))
    }
    #[cfg(target_os = "android")]
    fn store(&self, token: &str) -> Result<(), String> {
        crate::mobile::secret::set(&format!("{}:{KEY_USER}", self.service), token).map_err(|e| format!("credential store: {e}"))
    }
    #[cfg(target_os = "android")]
    fn forget(&self) {
        crate::mobile::secret::delete(&format!("{}:{KEY_USER}", self.service));
    }

    /// The name this install signs in as ("CadCraft Studio (Pixel Tablet)"). Tablets ask the system for the device's own
    /// name here, off the main thread, not at start.
    fn client_name(&self) -> String {
        #[cfg(mobile)]
        {
            let device = crate::mobile::device_name();
            if !device.is_empty() {
                return format!("{} ({device})", self.app_name);
            }
        }
        self.client_name.clone()
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    fn set<R: Runtime>(&self, app: &AppHandle<R>, s: Status) {
        *self.status.lock().unwrap() = s.clone();
        // Every window with the bridge hears about it (the hub's card, the sign-in window, the app).
        let js = format!("window.__ccDesktopAccount&&window.__ccDesktopAccount({})", serde_json::to_string(&s).unwrap());
        for w in app.webview_windows().values() {
            let _ = w.eval(&js);
        }
    }

    fn bump(&self) -> u64 {
        let mut g = self.generation.lock().unwrap();
        *g += 1;
        *g
    }
    fn current(&self, gen: u64) -> bool {
        *self.generation.lock().unwrap() == gen
    }
}

/// Debug builds only: CC_SITE points the shell at another site (tests).
#[cfg(debug_assertions)]
fn debug_site() -> Option<String> {
    std::env::var("CC_SITE").ok().filter(|s| s.starts_with("https://") || s.starts_with("http://127.0.0.1"))
}
#[cfg(not(debug_assertions))]
fn debug_site() -> Option<String> {
    None
}

fn read_json(resp: &mut ureq::http::Response<ureq::Body>) -> Value {
    resp.body_mut().read_json::<Value>().unwrap_or(Value::Null)
}

/// Start signing in: ask for a code, open /device in the person's browser, poll in the background.
#[tauri::command(async)]
pub fn cc_account_sign_in<R: Runtime>(app: AppHandle<R>, acct: State<'_, Account>) -> Result<Status, String> {
    if !acct.enabled {
        return Err("off: this build has no sign-in".into());
    }
    let gen = acct.bump();
    let mut resp = agent()
        .post(&format!("{}/api/v1/device/code", acct.site))
        .send_json(json!({ "client_name": acct.client_name(), "platform": platform() }))
        .map_err(|e| format!("network: {e}"))?;
    let code_status = resp.status().as_u16();
    let body = read_json(&mut resp);
    let data = &body["data"];
    if code_status != 200 || !data["device_code"].is_string() {
        let msg = body["message"].as_str().unwrap_or("The website did not give a code. Try again in a minute.").to_string();
        let s = Status { state: "signed_out".into(), reason: Some(if code_status == 429 { "throttled" } else { "error" }.into()), message: Some(msg.clone()), ..Default::default() };
        acct.set(&app, s);
        return Err(msg);
    }
    let device_code = data["device_code"].as_str().unwrap_or_default().to_string();
    let expires_in = data["expires_in"].as_u64().unwrap_or(600);
    let interval = data["interval"].as_u64().unwrap_or(5).max(1);
    let complete = data["verification_uri_complete"].as_str().map(String::from);
    let s = Status {
        state: "waiting".into(),
        code: data["user_code"].as_str().map(String::from),
        verification_uri: data["verification_uri"].as_str().map(String::from),
        expires_at: Some(now() + expires_in),
        ..Default::default()
    };
    acct.set(&app, s.clone());
    if let Some(u) = complete.as_deref().filter(|u| u.starts_with(&acct.site)) {
        let _ = app.opener().open_url(u, None::<&str>);
    }
    let h = app.clone();
    std::thread::spawn(move || poll(h, gen, device_code, interval, expires_in));
    Ok(s)
}

fn poll<R: Runtime>(app: AppHandle<R>, gen: u64, device_code: String, mut interval: u64, expires_in: u64) {
    let acct = app.state::<Account>();
    let end = Instant::now() + Duration::from_secs(expires_in);
    let a = agent();
    let fail = |reason: &str, message: &str| {
        acct.set(&app, Status { state: "signed_out".into(), reason: Some(reason.into()), message: Some(message.into()), ..Default::default() });
    };
    loop {
        std::thread::sleep(Duration::from_secs(interval));
        if !acct.current(gen) {
            return;
        }
        if Instant::now() > end {
            return fail("expired", "The code ran out before it was approved. Ask for a new one.");
        }
        let mut resp = match a.post(&format!("{}/api/v1/device/token", acct.site)).send_json(json!({ "device_code": device_code })) {
            Ok(r) => r,
            Err(_) => continue, // offline for a moment: keep trying until the code runs out
        };
        let st = resp.status().as_u16();
        let body = read_json(&mut resp);
        if !acct.current(gen) {
            return;
        }
        if st == 200 {
            let Some(token) = body["data"]["access_token"].as_str() else { return fail("error", "The website's answer had no token.") };
            if let Err(e) = acct.store(token) {
                // Never fall back to a file or the page's storage: no credential store = not signed in.
                revoke(&acct.site, token);
                return fail("keystore", &format!("This computer's credential store refused the sign-in ({e})."));
            }
            let user = me(&acct.site, token).ok();
            return acct.set(&app, Status { state: "signed_in".into(), user, ..Default::default() });
        }
        match body["code"].as_str().unwrap_or("") {
            "authorization_pending" => {}
            "slow_down" => interval = body["interval"].as_u64().unwrap_or(interval + 5).max(interval + 1),
            "access_denied" => return fail("denied", "The sign-in was turned down on the website."),
            "expired_token" => return fail("expired", "The code ran out before it was approved. Ask for a new one."),
            _ if st == 429 => interval += 5,
            _ => return fail("error", body["message"].as_str().unwrap_or("The sign-in did not work. Try again.")),
        }
    }
}

fn me(site: &str, token: &str) -> Result<Value, String> {
    let mut r = agent().get(&format!("{site}/api/v1/me")).header("Authorization", &format!("Bearer {token}")).call().map_err(|e| e.to_string())?;
    if r.status().as_u16() != 200 {
        return Err(format!("{}", r.status()));
    }
    let v = read_json(&mut r);
    let u = &v["data"]["user"];
    Ok(json!({ "name": u["name"], "email": u["email"] }))
}

fn revoke(site: &str, token: &str) -> bool {
    agent()
        .post(&format!("{site}/api/v1/logout"))
        .header("Authorization", &format!("Bearer {token}"))
        .send_empty()
        .map(|r| r.status().as_u16() < 300 || r.status().as_u16() == 401)
        .unwrap_or(false)
}

#[tauri::command(async)]
pub fn cc_account_status<R: Runtime>(app: AppHandle<R>, acct: State<'_, Account>) -> Status {
    let mut s = acct.status();
    if s.state == "unknown" {
        let signed_in = acct.token().is_some();
        s.state = if signed_in { "signed_in" } else { "signed_out" }.into();
        *acct.status.lock().unwrap() = s.clone();
    }
    if s.state == "signed_in" && s.user.is_none() {
        if let Some(t) = acct.token() {
            if let Ok(u) = me(&acct.site, &t) {
                s.user = Some(u);
                acct.set(&app, s.clone());
            }
        }
    }
    s
}

#[tauri::command(async)]
pub fn cc_account_cancel<R: Runtime>(app: AppHandle<R>, acct: State<'_, Account>) -> Status {
    if !acct.enabled {
        return acct.status();
    }
    acct.bump();
    let s = Status { state: if acct.token().is_some() { "signed_in" } else { "signed_out" }.into(), ..Default::default() };
    acct.set(&app, s.clone());
    s
}

/// Revoke on the server (best effort: offline still signs out here) and delete the token from the credential store.
#[tauri::command(async)]
pub fn cc_account_sign_out<R: Runtime>(app: AppHandle<R>, acct: State<'_, Account>) -> Status {
    if !acct.enabled {
        return acct.status();
    }
    acct.bump();
    let revoked = acct.token().map(|t| revoke(&acct.site, &t)).unwrap_or(true);
    acct.forget();
    let s = Status {
        state: "signed_out".into(),
        reason: (!revoked).then(|| "offline_signout".to_string()),
        message: (!revoked).then(|| "Signed out on this computer. The website could not be reached: the sign-in is also listed on getcadcraft.com/account/security, where you can remove it.".to_string()),
        ..Default::default()
    };
    acct.set(&app, s.clone());
    s
}

/// Paths a page may reach with the sign-in: the sync API, /api/v1/me and the caller's own picture (SI.7), and Studio Live's
/// invite links and signed-in names (LV.23, LV.19). All are sync-scope on the site; nothing else (library, account, team,
/// billing, admin) is reachable from a page, whatever its query string.
pub fn cloud_path_allowed(path: &str) -> bool {
    let exact_or_query = |p: &str| path == p || path.strip_prefix(p).is_some_and(|r| r.starts_with('?'));
    (path.starts_with("/api/v1/sync/")
        || exact_or_query("/api/v1/me")
        || exact_or_query("/api/v1/me/avatar")
        || ["check", "rooms", "end", "assertion"].iter().any(|x| exact_or_query(&format!("/api/v1/live/{x}"))))
        && !path.contains("..")
        && !path.contains('#')
        && !path.contains('\\')
        && !["%2e", "%2f", "%5c"].iter().any(|x| path.to_ascii_lowercase().contains(x))
        && path.chars().all(|c| c.is_ascii_graphic())
}

/// The page's only way to the cloud: method + path in headers, body = raw bytes. Only the paths `cloud_path_allowed` lets by.
/// Answer (raw): 2 bytes status (big-endian) · 4 bytes length of a JSON header block · that JSON · the body.
#[tauri::command(async)]
pub fn cc_cloud_request<R: Runtime>(app: AppHandle<R>, acct: State<'_, Account>, request: Request<'_>) -> Result<Response, String> {
    if !acct.enabled {
        return Err("off: this build has no sign-in".into());
    }
    let h = request.headers();
    let get = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).map(String::from);
    let method = get("x-cc-method").unwrap_or_else(|| "GET".into()).to_uppercase();
    let path = get("x-cc-path").ok_or("missing path")?;
    if !cloud_path_allowed(&path) {
        return Err("not_allowed: only the sync API".into());
    }
    let token = acct.token().ok_or("signed_out: sign in to getcadcraft.com first")?;
    let url = format!("{}{}", acct.site, path);
    let auth = format!("Bearer {token}");
    let a = agent();
    let result = match method.as_str() {
        "GET" => a.get(&url).header("Authorization", &auth).call(),
        "POST" => {
            // Bytes (raw, or base64 from a tablet's bridge: files.rs body_bytes); other JSON goes as JSON.
            let body: Vec<u8> = match (crate::files::body_bytes(request.body()), request.body()) {
                (Some(b), _) => b,
                (None, InvokeBody::Json(v)) => serde_json::to_vec(v).unwrap_or_default(),
                (None, _) => Vec::new(),
            };
            let ctype = get("x-cc-content-type").unwrap_or_else(|| "application/octet-stream".into());
            a.post(&url).header("Authorization", &auth).header("Content-Type", &ctype).send(&body[..])
        }
        _ => return Err("only GET and POST".into()),
    };
    let mut resp = result.map_err(|e| format!("network: {e}"))?;
    let status = resp.status().as_u16();
    let mut head = serde_json::Map::new();
    for k in ["content-type", "x-cadcraft-version", "x-cadcraft-sha256", "retry-after"] {
        if let Some(v) = resp.headers().get(k).and_then(|v| v.to_str().ok()) {
            head.insert(k.into(), Value::String(v.into()));
        }
    }
    let body = resp.body_mut().with_config().limit(200 * 1024 * 1024).read_to_vec().map_err(|e| format!("network: {e}"))?;
    // Signed out on the website (revoked, expired, account closed): forget the token here and say so; local work stays.
    if status == 401 {
        let code = serde_json::from_slice::<Value>(&body).ok().and_then(|v| v["code"].as_str().map(String::from)).unwrap_or_default();
        if matches!(code.as_str(), "token_revoked" | "token_expired" | "invalid_token" | "account_inactive") {
            acct.forget();
            acct.set(&app, Status {
                state: "signed_out".into(),
                reason: Some("revoked".into()),
                message: Some("This computer was signed out on getcadcraft.com. Sign in again to use your cloud drawings; nothing on this computer was lost.".into()),
                ..Default::default()
            });
        }
    }
    let head = serde_json::to_vec(&Value::Object(head)).unwrap();
    let mut out = Vec::with_capacity(6 + head.len() + body.len());
    out.extend_from_slice(&status.to_be_bytes());
    out.extend_from_slice(&(head.len() as u32).to_be_bytes());
    out.extend_from_slice(&head);
    out.extend_from_slice(&body);
    Ok(Response::new(out))
}

/// "Open getcadcraft.com/device" again (the person closed the tab): the page for the code on screen.
#[tauri::command(async)]
pub fn cc_account_open_device<R: Runtime>(app: AppHandle<R>, acct: State<'_, Account>) -> Result<(), String> {
    if !acct.enabled {
        return Err("off: this build has no sign-in".into());
    }
    let s = acct.status();
    let code = s.code.filter(|_| s.state == "waiting").ok_or("no code is waiting")?;
    let code: String = code.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    app.opener().open_url(format!("{}/device?code={code}", acct.site), None::<&str>).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_paths() {
        for ok in ["/api/v1/me", "/api/v1/me?x=1", "/api/v1/sync/status", "/api/v1/me/avatar", "/api/v1/me/avatar?size=64",
            "/api/v1/live/check?app=cadcraft", "/api/v1/live/rooms", "/api/v1/live/end", "/api/v1/live/assertion"] {
            assert!(cloud_path_allowed(ok), "{ok}");
        }
        for no in ["/api/v1/men", "/api/v1/me/avatars", "/api/v1/live/relay/host", "/api/v1/live/rooms/../../admin",
            "/api/v1/library/items", "/api/v1/me/avatar%2f..", "/api/v1/live/keys", "/api/v1/sync/x#y", "/api/v1/me/ x"] {
            assert!(!cloud_path_allowed(no), "{no}");
        }
    }

    /// Needs a credential store (Linux: a Secret Service session). `cargo test -- --ignored keyring_round_trip`
    #[test]
    #[ignore]
    fn keyring_round_trip() {
        let e = keyring::Entry::new("com.getcadcraft.test", KEY_USER).unwrap();
        e.set_password("ccu_test").expect("set");
        assert_eq!(e.get_password().expect("get"), "ccu_test");
        e.delete_credential().expect("delete");
        assert!(e.get_password().is_err());
    }

    /// Network: `cargo test -- --ignored` (asks the live site for a code; nothing is approved).
    #[test]
    #[ignore]
    fn device_code_over_os_tls() {
        let t = Instant::now();
        let mut r = agent()
            .post(&format!("{SITE}/api/v1/device/code"))
            .send_json(json!({ "client_name": "CadCraft shell test", "platform": platform() }))
            .expect("request");
        assert_eq!(r.status().as_u16(), 200);
        let v = read_json(&mut r);
        assert!(v["data"]["user_code"].as_str().unwrap_or("").len() == 9, "{v}");
        assert!(t.elapsed() < Duration::from_secs(10));
    }
}
