// SPDX-License-Identifier: Apache-2.0
//! Tablets (docs/0.5-PART3.md TB.1): the shell's own small mobile plugin, the same commands on both systems.
//!
//! - **Android** — `CadcraftPlugin.kt` (gen/android/app/src/main/java/com/getcadcraft/studio/): the Storage Access
//!   Framework (`ACTION_OPEN_DOCUMENT`, `ACTION_CREATE_DOCUMENT`, `ACTION_OPEN_DOCUMENT_TREE`, each kept with
//!   `takePersistableUriPermission` so recent files reopen after a restart), file descriptors for reading and writing, and
//!   the Android Keystore for the sign-in token (an AES key that never leaves the Keystore; the token encrypted with it in
//!   the app's private preferences).
//! - **iPad** — `CadcraftPlugin.swift` (gen/apple, TB.3): the document picker, security-scoped bookmarks as the "uri",
//!   file descriptors opened while the scope is held. The token goes to the Keychain through `keyring` (account.rs).
//!
//! Every call blocks until the system answers (a picker waits for the person), so callers run it off the main thread
//! (`tauri::async_runtime::spawn_blocking`). The page never sees a content URI or a bookmark: files.rs hands it
//! `cc-doc:/<key>/<name>` paths and keeps key → uri in the grant list.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::OnceLock;
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::Wry;

static HANDLE: OnceLock<PluginHandle<Wry>> = OnceLock::new();

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_cadcraft);

pub fn init() -> TauriPlugin<Wry> {
    Builder::new("cadcraft")
        .setup(|_app, api| {
            #[cfg(target_os = "android")]
            let handle = api.register_android_plugin("com.getcadcraft.studio", "CadcraftPlugin")?;
            #[cfg(target_os = "ios")]
            let handle = api.register_ios_plugin(init_plugin_cadcraft)?;
            let _ = HANDLE.set(handle);
            Ok(())
        })
        .build()
}

/// A document or a folder the person chose. `uri`: Android's content URI, iPad's bookmark (base64).
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Doc {
    pub uri: String,
    pub name: String,
    pub size: Option<u64>,
    /// Milliseconds since 1970, when the provider knows it.
    pub mtime: Option<f64>,
    pub dir: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Docs {
    docs: Vec<Doc>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OneDoc {
    doc: Option<Doc>,
}

fn call<T: DeserializeOwned>(cmd: &str, args: serde_json::Value) -> Result<T, String> {
    let h = HANDLE.get().ok_or("error: the documents plugin is not ready")?;
    h.run_mobile_plugin(cmd, args).map_err(|e| {
        let m = e.to_string();
        // The plugin words its refusals with our codes ("not_found: …", "permission_denied: …"); anything else is an error.
        for code in ["not_found:", "permission_denied:", "not_allowed:", "not_supported:"] {
            if let Some(i) = m.find(code) {
                return m[i..].to_string();
            }
        }
        format!("error: {m}")
    })
}

/// The system's document picker. Empty = cancelled. Every type is offered: providers name CAD types inconsistently
/// (a .dwg is image/vnd.dwg, application/acad or application/octet-stream), and each app checks what it opens.
pub fn open_documents(multiple: bool) -> Result<Vec<Doc>, String> {
    call::<Docs>("openDocuments", json!({ "multiple": multiple })).map(|d| d.docs)
}

/// "Save as": the person picks where; `None` = cancelled.
pub fn create_document(name: &str, mime: &str) -> Result<Option<Doc>, String> {
    call::<OneDoc>("createDocument", json!({ "name": name, "mimeType": mime })).map(|d| d.doc)
}

/// A folder (sheet sets, "Open a folder"); `None` = cancelled.
pub fn open_tree() -> Result<Option<Doc>, String> {
    call::<OneDoc>("openTree", json!({})).map(|d| d.doc)
}

/// The file behind a document, opened by the system for us: `r` read, `wt` write (truncated first).
pub fn open(uri: &str, mode: &str) -> Result<std::fs::File, String> {
    #[derive(Deserialize)]
    struct Fd {
        fd: i32,
    }
    let fd = call::<Fd>("openFd", json!({ "uri": uri, "mode": mode }))?.fd;
    if fd < 0 {
        return Err("error: no file descriptor".into());
    }
    use std::os::fd::FromRawFd;
    // SAFETY: the plugin detached this descriptor for us (ParcelFileDescriptor.detachFd / open(2)); we own it from here.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// Size and time of a document; `None` = gone (deleted, or its permission was taken back).
pub fn stat(uri: &str) -> Result<Option<Doc>, String> {
    call::<OneDoc>("stat", json!({ "uri": uri })).map(|d| d.doc)
}

/// A folder's entries: `tree` = the folder the person chose, `rel` = the path below it (empty = the folder itself).
pub fn children(tree: &str, rel: &[String]) -> Result<Vec<Doc>, String> {
    call::<Docs>("children", json!({ "tree": tree, "rel": rel })).map(|d| d.docs)
}

/// A file below a chosen folder by its path; with `create`, made (with its folders) when missing.
pub fn find(tree: &str, rel: &[String], create: bool) -> Result<Option<Doc>, String> {
    call::<OneDoc>("find", json!({ "tree": tree, "rel": rel, "create": create })).map(|d| d.doc)
}

/// Gives a document's lasting permission back (the grant list dropped it: Android keeps at most 512 per app).
pub fn release(uri: &str) {
    let _ = call::<serde_json::Value>("release", json!({ "uri": uri }));
}

/// The device's own name for the sign-in list ("Pixel Tablet"); empty when unknown.
pub fn device_name() -> String {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Name {
        name: String,
    }
    call::<Name>("deviceName", json!({})).map(|n| n.name).unwrap_or_default()
}

/// Android: TLS through rustls, verified by the system's own trust manager (rustls-platform-verifier; its Kotlin half is
/// in the APK). Called before the first connection: hands the verifier the process's JavaVM and application context.
#[cfg(target_os = "android")]
pub fn tls_ready() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| {
        // One crypto provider for the process (ring), whatever else names rustls.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let ctx = ndk_context::android_context();
        // SAFETY: tao initialises ndk_context with the process's JavaVM and the application context (a global ref).
        let vm = unsafe { jni22::JavaVM::from_raw(ctx.vm().cast()) };
        let _ = vm.attach_current_thread(|env| -> Result<(), jni22::errors::Error> {
            let context = unsafe { jni22::objects::JObject::from_raw(env, ctx.context().cast()) };
            rustls_platform_verifier::android::init_with_env(env, context)
        });
    });
}

/// Android: the TLS connector for Studio Live's sockets (the relay is wss://), the same verification as above.
#[cfg(all(target_os = "android", feature = "live"))]
pub fn ws_connector() -> Result<tokio_tungstenite::Connector, String> {
    use rustls_platform_verifier::ConfigVerifierExt;
    tls_ready();
    let cfg = rustls::ClientConfig::with_platform_verifier().map_err(|e| e.to_string())?;
    Ok(tokio_tungstenite::Connector::Rustls(std::sync::Arc::new(cfg)))
}

/// Android (TB.5): mDNS for Studio Live needs Wi-Fi multicast; the plugin holds a MulticastLock while Studio is on screen
/// once Live has asked for it.
#[cfg(target_os = "android")]
pub fn multicast(on: bool) {
    let _ = call::<serde_json::Value>("multicast", json!({ "on": on }));
}

/// iPad (TB.5): Studio Live's `_ccstudio._tcp` through Bonjour (raw multicast needs Apple's multicast entitlement).
#[cfg(target_os = "ios")]
pub fn bonjour_advertise(name: &str, port: u16, app: &str) -> Result<(), String> {
    call::<serde_json::Value>("bonjourAdvertise", json!({ "name": name, "port": port, "app": app })).map(|_| ())
}
#[cfg(target_os = "ios")]
pub fn bonjour_stop(name: &str) {
    let _ = call::<serde_json::Value>("bonjourStop", json!({ "name": name }));
}
/// → (address, port, app) of the instance `name`, within `ms`.
#[cfg(target_os = "ios")]
pub fn bonjour_browse(name: &str, ms: u64) -> Result<Vec<(String, u16, String)>, String> {
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Hit {
        addr: String,
        port: u16,
        app: String,
    }
    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Hits {
        found: Vec<Hit>,
    }
    call::<Hits>("bonjourBrowse", json!({ "name": name, "ms": ms })).map(|h| h.found.into_iter().map(|x| (x.addr, x.port, x.app)).collect())
}

/// The sign-in token's store on Android (the Keystore; account.rs). iPad uses the Keychain through `keyring`.
#[cfg(target_os = "android")]
pub mod secret {
    use super::call;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Value {
        value: Option<String>,
    }

    pub fn get(key: &str) -> Option<String> {
        call::<Value>("secretGet", json!({ "key": key })).ok().and_then(|v| v.value)
    }
    pub fn set(key: &str, value: &str) -> Result<(), String> {
        call::<serde_json::Value>("secretSet", json!({ "key": key, "value": value })).map(|_| ())
    }
    pub fn delete(key: &str) {
        let _ = call::<serde_json::Value>("secretDelete", json!({ "key": key }));
    }
}
