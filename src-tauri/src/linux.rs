// SPDX-License-Identifier: Apache-2.0
//! Linux: keep WebKitGTK from drawing a black window (0.7 D7.57).
//!
//! WebKitGTK 2.4x renders through a DMA-BUF buffer by default. On some graphics stacks (NVIDIA's proprietary driver, some
//! Wayland sessions, virtual machines, an update that changed the driver or WebKitGTK) that path fails without an error:
//! the window opens and stays black or blank. WebKit's own switch for it is `WEBKIT_DISABLE_DMABUF_RENDERER=1`, which falls
//! back to the older renderer; it must be set before the first web view exists, so `run()` calls `guard()` first thing.
//!
//! Rules: never override what the person set (any value, including `0`, is theirs); `CC_WEBKIT_DMABUF=1` keeps WebKit's
//! default even so (for a machine where the fast path works and the fallback is slower); nothing else is touched.

/// What to set, given how the environment looks. A pure function so the rules are tested without touching the process.
pub fn plan(get: impl Fn(&str) -> Option<String>) -> Vec<(&'static str, &'static str)> {
    let set = |k: &str| get(k).map_or(false, |v| !v.is_empty());
    let mut out = Vec::new();
    if set("CC_WEBKIT_DMABUF") && get("CC_WEBKIT_DMABUF").as_deref() == Some("1") {
        return out;
    }
    if !set("WEBKIT_DISABLE_DMABUF_RENDERER") {
        out.push(("WEBKIT_DISABLE_DMABUF_RENDERER", "1"));
    }
    out
}

/// Apply the plan to this process. Single-threaded at that point (before Tauri starts), which is when `set_var` is safe.
pub fn guard() {
    for (k, v) in plan(|k| std::env::var(k).ok()) {
        std::env::set_var(k, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn turns_the_dmabuf_renderer_off_when_nobody_said_otherwise() {
        assert_eq!(plan(env(&[])), vec![("WEBKIT_DISABLE_DMABUF_RENDERER", "1")]);
    }

    #[test]
    fn leaves_the_persons_own_setting_alone_even_when_it_is_zero() {
        assert!(plan(env(&[("WEBKIT_DISABLE_DMABUF_RENDERER", "0")])).is_empty());
        assert!(plan(env(&[("WEBKIT_DISABLE_DMABUF_RENDERER", "1")])).is_empty());
    }

    #[test]
    fn an_empty_value_counts_as_not_set() {
        assert_eq!(plan(env(&[("WEBKIT_DISABLE_DMABUF_RENDERER", "")])).len(), 1);
    }

    #[test]
    fn cc_webkit_dmabuf_keeps_webkits_default() {
        assert!(plan(env(&[("CC_WEBKIT_DMABUF", "1")])).is_empty());
        assert_eq!(plan(env(&[("CC_WEBKIT_DMABUF", "0")])).len(), 1);
    }
}
