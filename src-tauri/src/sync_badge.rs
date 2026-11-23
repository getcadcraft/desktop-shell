// SPDX-License-Identifier: Apache-2.0
//! The sync state outside the page (0.11 SCF.9): a small sign on the app's own taskbar button or dock icon, so a person sees that
//! saves are still waiting, or that one needs them, without the window in front.
//!
//! The page that runs the sync queue (Studio's hub; a one-app edition's or SuperCad Desktop's own page) reports its summary through
//! `cadcraftDesktop.sync.state({state, waiting, problems})`; the shell turns it into one `Badge` here and windows.rs puts it on the
//! main window:
//! - **macOS:** the dock badge (a number, or "!" when the person is needed).
//! - **Windows:** the taskbar overlay icon (a 16 x 16 dot drawn here: blue = saving, grey = waiting offline, ochre = sign in again,
//!   red = a conflict or a save that failed). Windows has no badge number.
//! - **Linux:** the launcher count where the desktop has one (libunity: GNOME's dock, KDE's task manager); nothing elsewhere.
//!
//! Nothing at all while everything is saved or the person uses no cloud: quiet is the normal state. No tray icon: a tray needs
//! a system library on Linux (libayatana-appindicator) the .deb, AppImage and Flatpak do not carry, and Studio keeps running
//! only while a window is open. The badge holds numbers and one of the states below only: no file or account names.
//!
//! Standalone check (no tauri, no cargo):  rustc --edition 2021 --test sync_badge.rs -o /tmp/sync_badge && /tmp/sync_badge

/// What the page reports. Unknown words read as `Off` (the badge is cleared, never guessed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    /// Not signed in, no cloud, or the browser edition's rules: nothing to show.
    Off,
    /// Everything has reached the server.
    Synced,
    /// Saves are on their way (`waiting` of them).
    Saving,
    /// Saves wait for the connection to come back.
    Offline,
    /// The sign-in ended: the queue waits for the person to sign in again.
    SignIn,
    /// A newer version on the server, or a save the server refused: the person decides (`problems` of them).
    Problem,
}

impl SyncState {
    pub fn parse(s: &str) -> SyncState {
        match s {
            "synced" => SyncState::Synced,
            "saving" | "waiting" => SyncState::Saving,
            "offline" => SyncState::Offline,
            "signin" | "paused" => SyncState::SignIn,
            "problem" | "conflict" | "failed" => SyncState::Problem,
            _ => SyncState::Off,
        }
    }
}

/// The colour of the Windows overlay dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dot {
    Blue,
    Grey,
    Ochre,
    Red,
}

impl Dot {
    /// RGB: the site's paper-space layer colours (getcadcraft.com tokens.css --layer-blue #1E48C8, --layer-ochre #8A6200,
    /// --layer-red #B3122E; the white ring keeps them readable on a dark taskbar) and a neutral grey.
    pub fn rgb(self) -> [u8; 3] {
        match self {
            Dot::Blue => [0x1e, 0x48, 0xc8],
            Dot::Grey => [0x6b, 0x6b, 0x6b],
            Dot::Ochre => [0x8a, 0x62, 0x00],
            Dot::Red => [0xb3, 0x12, 0x2e],
        }
    }
}

/// What the shell shows for one report: the dock label (macOS), the launcher count (Linux) and the overlay dot (Windows).
/// All three `None` = clear every sign.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Badge {
    pub label: Option<String>,
    pub count: Option<i64>,
    pub dot: Option<Dot>,
}

/// The most a count says ("99+" past it on the dock).
pub const COUNT_MAX: u32 = 99;

fn label_of(n: u32) -> Option<String> {
    match n {
        0 => None,
        n if n > COUNT_MAX => Some(format!("{COUNT_MAX}+")),
        n => Some(n.to_string()),
    }
}

/// One report → the signs. `waiting` = saves not yet on the server, `problems` = conflicts + failed saves.
pub fn badge(state: SyncState, waiting: u32, problems: u32) -> Badge {
    let count = |n: u32| if n == 0 { None } else { Some(i64::from(n.min(COUNT_MAX))) };
    match state {
        SyncState::Off | SyncState::Synced => Badge::default(),
        SyncState::Saving if waiting == 0 => Badge::default(),
        SyncState::Saving => Badge { label: label_of(waiting), count: count(waiting), dot: Some(Dot::Blue) },
        SyncState::Offline if waiting == 0 => Badge::default(),
        SyncState::Offline => Badge { label: label_of(waiting), count: count(waiting), dot: Some(Dot::Grey) },
        SyncState::SignIn => Badge { label: Some("!".into()), count: count(waiting), dot: Some(Dot::Ochre) },
        SyncState::Problem => Badge { label: Some("!".into()), count: count(problems.max(1)), dot: Some(Dot::Red) },
    }
}

/// The Windows overlay: a `size` x `size` RGBA dot (anti-aliased edge, a thin white ring so it reads on a dark or light
/// taskbar). Row-major, 4 bytes a pixel.
pub fn dot_rgba(dot: Dot, size: u32) -> Vec<u8> {
    let [r, g, b] = dot.rgb();
    let n = size.max(1) as usize;
    let c = n as f32 / 2.0;
    let outer = c - 0.5;
    let ring = 1.4_f32;
    let mut px = vec![0u8; n * n * 4];
    for y in 0..n {
        for x in 0..n {
            let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt();
            let a = (outer - d + 0.5).clamp(0.0, 1.0);
            if a <= 0.0 {
                continue;
            }
            let white = (d - (outer - ring)).clamp(0.0, 1.0);
            let mix = |v: u8| (v as f32 * (1.0 - white) + 255.0 * white).round() as u8;
            let i = (y * n + x) * 4;
            px[i] = mix(r);
            px[i + 1] = mix(g);
            px[i + 2] = mix(b);
            px[i + 3] = (a * 255.0).round() as u8;
        }
    }
    px
}

/// The window whose report counts: the main window only (Studio's hub; the one page of a one-app edition or SuperCad Desktop).
/// An app window inside Studio sends its saves through the hub's queue, so it has nothing of its own to report.
pub fn reporter_ok(label: &str) -> bool {
    label == "main"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_when_everything_is_saved() {
        assert_eq!(badge(SyncState::Off, 0, 0), Badge::default());
        assert_eq!(badge(SyncState::Synced, 0, 0), Badge::default());
        assert_eq!(badge(SyncState::Synced, 4, 2), Badge::default(), "synced wins over stale numbers");
        assert_eq!(badge(SyncState::Saving, 0, 0), Badge::default());
        assert_eq!(badge(SyncState::Offline, 0, 0), Badge::default(), "offline with nothing waiting: nothing to say");
    }

    #[test]
    fn waiting_saves_are_counted() {
        let b = badge(SyncState::Saving, 3, 0);
        assert_eq!((b.label.as_deref(), b.count, b.dot), (Some("3"), Some(3), Some(Dot::Blue)));
        let b = badge(SyncState::Offline, 2, 0);
        assert_eq!((b.label.as_deref(), b.count, b.dot), (Some("2"), Some(2), Some(Dot::Grey)));
        let b = badge(SyncState::Saving, 250, 0);
        assert_eq!((b.label.as_deref(), b.count), (Some("99+"), Some(99)));
    }

    #[test]
    fn the_person_is_needed() {
        let b = badge(SyncState::SignIn, 0, 0);
        assert_eq!((b.label.as_deref(), b.count, b.dot), (Some("!"), None, Some(Dot::Ochre)));
        let b = badge(SyncState::SignIn, 5, 0);
        assert_eq!(b.count, Some(5), "Linux shows how many wait for the sign-in");
        let b = badge(SyncState::Problem, 1, 2);
        assert_eq!((b.label.as_deref(), b.count, b.dot), (Some("!"), Some(2), Some(Dot::Red)));
        assert_eq!(badge(SyncState::Problem, 0, 0).count, Some(1), "a problem always shows");
    }

    #[test]
    fn words_from_the_page() {
        assert_eq!(SyncState::parse("synced"), SyncState::Synced);
        assert_eq!(SyncState::parse("saving"), SyncState::Saving);
        assert_eq!(SyncState::parse("waiting"), SyncState::Saving);
        assert_eq!(SyncState::parse("offline"), SyncState::Offline);
        assert_eq!(SyncState::parse("signin"), SyncState::SignIn);
        assert_eq!(SyncState::parse("paused"), SyncState::SignIn);
        assert_eq!(SyncState::parse("conflict"), SyncState::Problem);
        assert_eq!(SyncState::parse("failed"), SyncState::Problem);
        assert_eq!(SyncState::parse("Plan.dwg"), SyncState::Off, "anything else clears the badge");
        assert_eq!(SyncState::parse(""), SyncState::Off);
    }

    #[test]
    fn the_dot_is_round_with_a_white_ring() {
        let px = dot_rgba(Dot::Red, 16);
        assert_eq!(px.len(), 16 * 16 * 4);
        let at = |x: usize, y: usize| &px[(y * 16 + x) * 4..(y * 16 + x) * 4 + 4];
        assert_eq!(at(0, 0)[3], 0, "corners are clear");
        assert_eq!(at(8, 8), &[0xb3, 0x12, 0x2e, 255], "the middle is the colour, opaque");
        let edge = at(8, 0);
        assert!(edge[3] > 0 && edge[0] > 0xb3 && edge[1] > 0x12, "the top edge is lighter (the ring): {edge:?}");
        assert_eq!(dot_rgba(Dot::Blue, 0).len(), 4, "a zero size still gives one pixel, never a panic");
    }

    #[test]
    fn only_the_main_window_reports() {
        assert!(reporter_ok("main"));
        assert!(!reporter_ok("app-cadcraft-1") && !reporter_ok("popup-3") && !reporter_ok("account"));
    }
}
