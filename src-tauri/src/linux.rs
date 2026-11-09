// SPDX-License-Identifier: Apache-2.0
//! Linux: keep WebKitGTK from drawing a black window (0.7 D7.57).
//!
//! WebKitGTK 2.4x renders through a DMA-BUF buffer by default. On some graphics stacks (NVIDIA's proprietary driver, some
//! Wayland sessions, virtual machines, an update that changed the driver or WebKitGTK) that path fails without an error:
//! the window opens and stays black or blank. WebKit's own switch for it is `WEBKIT_DISABLE_DMABUF_RENDERER=1`, which falls
//! back to the older renderer; it must be set before the first web view exists, so `run()` calls `guard()` first thing.
//!
//! The fallback is not free: on a machine where the fast path works (AMD, Intel) it can leave the 3D view drawn on the CPU.
//! A Ryzen AI Max+ 395 ran the Flatpak at about 2 fps with it forced on, and smooth with it left alone (community report,
//! 2026-11-08, confirmed by the tester). So the guard now only turns the renderer off where the trouble is known: an NVIDIA
//! or virtual graphics device, or when the graphics devices cannot be read at all (unknown stays on the safe side).
//!
//! Rules: never override what the person set (any value, including `0`, is theirs); `CC_WEBKIT_DMABUF=1` keeps WebKit's
//! default even so; `CC_WEBKIT_DMABUF=0` forces the fallback on any machine; nothing else is touched.

/// PCI vendor ids read from /sys/class/drm/card*/device/vendor. NVIDIA, then the usual virtual adapters
/// (virtio, VMware, QEMU/Bochs, VirtualBox, Red Hat QXL).
const NEEDS_FALLBACK: [u32; 6] = [0x10de, 0x1af4, 0x15ad, 0x1234, 0x80ee, 0x1b36];

/// True when the fallback renderer is the right default for these graphics devices. An empty list means "could not tell".
pub fn needs_fallback(vendors: &[u32]) -> bool {
    vendors.is_empty() || vendors.iter().any(|v| NEEDS_FALLBACK.contains(v))
}

/// Vendor ids of the graphics devices this machine shows (only real cards, not connectors such as card0-DP-1).
pub fn drm_vendors() -> Vec<u32> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/sys/class/drm") {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if !(n.starts_with("card") && n[4..].chars().all(|c| c.is_ascii_digit())) { continue; }
            if let Ok(t) = std::fs::read_to_string(e.path().join("device/vendor")) {
                if let Ok(v) = u32::from_str_radix(t.trim().trim_start_matches("0x"), 16) { out.push(v); }
            }
        }
    }
    out
}

/// What to set, given how the environment looks. A pure function so the rules are tested without touching the process.
pub fn plan(get: impl Fn(&str) -> Option<String>, vendors: &[u32]) -> Vec<(&'static str, &'static str)> {
    let set = |k: &str| get(k).map_or(false, |v| !v.is_empty());
    let mut out = Vec::new();
    if set("CC_WEBKIT_DMABUF") && get("CC_WEBKIT_DMABUF").as_deref() == Some("1") {
        return out;
    }
    let forced = get("CC_WEBKIT_DMABUF").as_deref() == Some("0");
    if !set("WEBKIT_DISABLE_DMABUF_RENDERER") && (forced || needs_fallback(vendors)) {
        out.push(("WEBKIT_DISABLE_DMABUF_RENDERER", "1"));
    }
    out
}

/// Apply the plan to this process. Single-threaded at that point (before Tauri starts), which is when `set_var` is safe.
pub fn guard() {
    for (k, v) in plan(|k| std::env::var(k).ok(), &drm_vendors()) {
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

    const AMD: [u32; 1] = [0x1002];
    const NVIDIA: [u32; 1] = [0x10de];

    #[test]
    fn turns_the_dmabuf_renderer_off_on_nvidia_and_virtual_adapters() {
        let off = vec![("WEBKIT_DISABLE_DMABUF_RENDERER", "1")];
        assert_eq!(plan(env(&[]), &NVIDIA), off);
        assert_eq!(plan(env(&[]), &[0x1af4]), off); // virtio
        assert_eq!(plan(env(&[]), &[0x1002, 0x10de]), off); // AMD iGPU + NVIDIA card: stay safe
    }

    #[test]
    fn leaves_amd_and_intel_on_webkits_fast_path() {
        assert!(plan(env(&[]), &AMD).is_empty());
        assert!(plan(env(&[]), &[0x8086]).is_empty());
    }

    #[test]
    fn an_unreadable_graphics_list_keeps_the_safe_fallback() {
        assert_eq!(plan(env(&[]), &[]).len(), 1);
        assert!(needs_fallback(&[]));
    }

    #[test]
    fn leaves_the_persons_own_setting_alone_even_when_it_is_zero() {
        assert!(plan(env(&[("WEBKIT_DISABLE_DMABUF_RENDERER", "0")]), &NVIDIA).is_empty());
        assert!(plan(env(&[("WEBKIT_DISABLE_DMABUF_RENDERER", "1")]), &AMD).is_empty());
    }

    #[test]
    fn an_empty_value_counts_as_not_set() {
        assert_eq!(plan(env(&[("WEBKIT_DISABLE_DMABUF_RENDERER", "")]), &NVIDIA).len(), 1);
    }

    #[test]
    fn cc_webkit_dmabuf_one_keeps_the_default_and_zero_forces_the_fallback() {
        assert!(plan(env(&[("CC_WEBKIT_DMABUF", "1")]), &NVIDIA).is_empty());
        assert_eq!(plan(env(&[("CC_WEBKIT_DMABUF", "0")]), &AMD).len(), 1);
    }
}
