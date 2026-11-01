//! Bring your own key (0.8 AC.11, session ai-remote_PART_2; docs/AI-CONNECT-HOSTED-SPEC.md § 7; JS engine tools/mcp/byok.mjs).
//!
//! The person's API key for a model provider lives in the OS credential store, in the same place as Studio's sign-in (account.rs:
//! Windows Credential Manager, macOS Keychain, Secret Service). The web view can set it, ask whether one is stored and delete it; it can
//! NEVER read it back. A provider call goes through `cc_ask_call`: this side adds the key, makes the HTTPS request and returns only the
//! status and the JSON body. A key is never sent to an http address that is not this computer, never follows a redirect, never logged,
//! and never leaves for CadCraft.
//!
//! UNBUILT: written on the VPS (rule 25), to be built and tried on workstation11a. Desktop only; Android keeps its key through
//! mobile.rs's Keystore secret, which has no `get` for the web view either, and is a follow-up (the commands answer "not available").
//! Mount (lib.rs, owner of the Studio MCP side): `mod ask_keys;` and in generate_handler![]:
//!   ask_keys::cc_ask_key_set, ask_keys::cc_ask_key_has, ask_keys::cc_ask_key_delete, ask_keys::cc_ask_call

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tauri::{Runtime, WebviewWindow};

/// Only Studio's own window (the hub) in the suite may touch a key or call a provider: an app window or a page inside one never can.
fn hub_only<R: Runtime>(w: &WebviewWindow<R>, suite: &crate::windows::Suite) -> Result<(), String> {
    if !suite.on || w.label() != "main" {
        return Err("not_allowed: only Studio's own window".into());
    }
    Ok(())
}

const SERVICE: &str = "org.getcadcraft.studio.ask-keys";
const MAX_RESPONSE: u64 = 4 * 1024 * 1024;

#[derive(Deserialize)]
pub struct AskRequest {
    pub path: String,
    #[serde(default)]
    pub headers: std::collections::BTreeMap<String, String>,
    pub body: Value,
}

#[derive(Serialize, Default)]
pub struct AskAnswer {
    pub status: u16,
    pub json: Option<Value>,
    pub error: Option<String>,
}

fn name_ok(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

#[cfg(not(target_os = "android"))]
fn entry(name: &str) -> Result<keyring::Entry, String> {
    if !name_ok(name) {
        return Err("bad key name".into());
    }
    keyring::Entry::new(SERVICE, name).map_err(|e| format!("credential store: {e}"))
}

#[tauri::command]
pub fn cc_ask_key_set<R: Runtime>(window: WebviewWindow<R>, suite: tauri::State<'_, crate::windows::Suite>, name: String, secret: String) -> Result<(), String> {
    hub_only(&window, &suite)?;
    #[cfg(target_os = "android")]
    {
        let _ = (name, secret);
        return Err("not available on Android yet".into());
    }
    #[cfg(not(target_os = "android"))]
    {
        if secret.len() < 8 || secret.len() > 400 || secret.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err("that does not look like an API key".into());
        }
        entry(&name)?.set_password(&secret).map_err(|e| format!("credential store: {e}"))
    }
}

#[tauri::command]
pub fn cc_ask_key_has<R: Runtime>(window: WebviewWindow<R>, suite: tauri::State<'_, crate::windows::Suite>, name: String) -> bool {
    if hub_only(&window, &suite).is_err() {
        return false;
    }
    #[cfg(target_os = "android")]
    {
        let _ = name;
        false
    }
    #[cfg(not(target_os = "android"))]
    {
        entry(&name).ok().map(|e| e.get_password().is_ok()).unwrap_or(false)
    }
}

#[tauri::command]
pub fn cc_ask_key_delete<R: Runtime>(window: WebviewWindow<R>, suite: tauri::State<'_, crate::windows::Suite>, name: String) -> Result<(), String> {
    hub_only(&window, &suite)?;
    #[cfg(target_os = "android")]
    {
        let _ = name;
        Ok(())
    }
    #[cfg(not(target_os = "android"))]
    {
        match entry(&name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(format!("credential store: {e}")),
        }
    }
}

fn is_loopback(host: &str) -> bool {
    host == "localhost" || host == "[::1]" || host == "::1" || host.starts_with("127.")
}

/// provider: anthropic | openai | compatible. `base_url` empty = the provider's own address.
#[tauri::command(async)]
pub fn cc_ask_call<R: Runtime>(window: WebviewWindow<R>, suite: tauri::State<'_, crate::windows::Suite>, provider: String, base_url: String, key_name: String, request: AskRequest) -> AskAnswer {
    let fail = |e: &str| AskAnswer { status: 0, json: None, error: Some(e.into()) };
    if hub_only(&window, &suite).is_err() {
        return fail("not_allowed");
    }
    let base = if !base_url.is_empty() {
        base_url.trim_end_matches('/').to_string()
    } else {
        match provider.as_str() {
            "anthropic" => "https://api.anthropic.com".into(),
            "openai" => "https://api.openai.com/v1".into(),
            _ => return fail("no_address"),
        }
    };
    let host = base.split("://").nth(1).unwrap_or("").split(['/', ':']).next().unwrap_or("").to_lowercase();
    let local = is_loopback(&host);
    if !base.starts_with("https://") && !(base.starts_with("http://") && local) {
        return fail("insecure_address");
    }
    if !request.path.starts_with('/') || request.path.contains("..") {
        return fail("bad_request");
    }
    #[cfg(not(target_os = "android"))]
    let key = entry(&key_name).ok().and_then(|e| e.get_password().ok());
    #[cfg(target_os = "android")]
    let key: Option<String> = { let _ = &key_name; None };
    let needs_key = !(provider == "compatible" && local);
    if needs_key && key.is_none() {
        return fail("no_key");
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0) // a key never follows a redirect to another host
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent(concat!("CadCraft-Studio/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut req = agent.post(&format!("{base}{}", request.path));
    for (k, v) in &request.headers {
        let lk = k.to_ascii_lowercase();
        if lk == "x-api-key" || lk == "authorization" || lk == "host" || lk == "cookie" {
            continue; // only this side sets credentials
        }
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(k) = &key {
        req = if provider == "anthropic" { req.header("x-api-key", k.as_str()) } else { req.header("authorization", format!("Bearer {k}")) };
    }
    match req.send_json(&request.body) {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let text = match resp.body_mut().with_config().limit(MAX_RESPONSE).read_to_string() {
                Ok(t) => t,
                Err(_) => return AskAnswer { status, json: None, error: Some("too_large".into()) },
            };
            AskAnswer { status, json: serde_json::from_str(&text).ok(), error: None }
        }
        Err(_) => fail("unreachable"),
    }
}
