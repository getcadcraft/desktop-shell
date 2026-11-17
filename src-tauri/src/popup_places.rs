// SPDX-License-Identifier: Apache-2.0
//! Torn-off windows in Studio (0.10 UI.4): where each one was last, so it comes back on the monitor the person dragged it to, and which
//! ones were out when its app window closed, so the page can offer to put them back (UI.3) without a click.
//!
//! The page's window service (assets/js/app-windows.js) opens one same-origin pop-up per torn-off block and gives it a title
//! (`d.title = title()`), so the **key** is `<app>|<title>`: it survives a restart (the label `popup-3` does not) and needs nothing
//! from the page. The shell watches the pop-up move and resize, keeps its place, and on the next start puts the window with that
//! title back where it was, provided a connected screen still shows enough of it. Nothing here touches a document, a path or a token:
//! only an app id, a block title and four numbers per window.
//!
//! This file is std-only on purpose: its tests run alone (`rustc --edition 2021 --test src/popup_places.rs`); the Tauri glue is in
//! windows.rs (`popup_hook`, `popup_titled`, `cc_popup_restore`).
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// Most places kept (the oldest go first): block titles are few, but a title with a file name in it must not grow the file forever.
pub const MAX_PLACES: usize = 48;

/// Physical position and size on the desktop, plus the screen's scale (x1000) so the size can be asked in logical pixels again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub scale_milli: u32,
}

/// A connected screen: physical position and size.
#[derive(Clone, Copy, Debug)]
pub struct Mon {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    /// A saved place counts only when a connected screen shows enough of it (the rule `windows::Place::shown_on` uses for app windows).
    pub fn shown_on(&self, mons: &[Mon]) -> bool {
        mons.iter().any(|m| {
            let x = (self.x + self.w as i32).min(m.x + m.w as i32) - self.x.max(m.x);
            let y = (self.y + self.h as i32).min(m.y + m.h as i32) - self.y.max(m.y);
            x >= 64 && y >= 32
        })
    }

    /// The size to ask for, in logical pixels.
    pub fn logical_size(&self) -> (f64, f64) {
        let s = if self.scale_milli == 0 { 1.0 } else { self.scale_milli as f64 / 1000.0 };
        (self.w as f64 / s, self.h as f64 / s)
    }
}

/// `<app>|<title>`: printable, one line, at most 96 characters; nothing for an empty title or about:blank.
pub fn key(app: &str, title: &str) -> Option<String> {
    let clean = |s: &str| s.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string();
    let (a, t) = (clean(app), clean(title));
    if a.is_empty() || t.is_empty() || t == "about:blank" {
        return None;
    }
    let k: String = format!("{a}|{t}").chars().take(96).collect();
    Some(k.replace('\t', " "))
}

fn app_of_key(k: &str) -> &str {
    k.split('|').next().unwrap_or("")
}

fn title_of_key(k: &str) -> &str {
    k.split_once('|').map(|x| x.1).unwrap_or("")
}

#[derive(Default)]
struct Inner {
    /// Oldest first.
    rects: Vec<(String, Rect)>,
    /// Keys whose window was out when its app window closed, not yet offered back.
    restore: Vec<String>,
    /// Pop-ups alive now: label -> (opener window label, app id, key once the title is known).
    alive: HashMap<String, (String, String, Option<String>)>,
}

pub struct PopupState {
    file: Option<PathBuf>,
    inner: Mutex<Inner>,
}

impl PopupState {
    pub fn new(file: Option<PathBuf>) -> Self {
        let mut inner = Inner::default();
        if let Some(t) = file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()) {
            parse(&t, &mut inner);
        }
        PopupState { file, inner: Mutex::new(inner) }
    }

    /// A pop-up window was made by `opener` (an app window of app `app`).
    pub fn opened(&self, label: &str, opener: &str, app: &str) {
        if let Ok(mut i) = self.inner.lock() {
            i.alive.insert(label.to_string(), (opener.to_string(), app.to_string(), None));
        }
    }

    /// The pop-up's page set its title: this is the block. Returns the key and the place to restore (if one is kept).
    pub fn titled(&self, label: &str, opener: &str, app: &str, title: &str) -> Option<(String, Option<Rect>)> {
        let k = key(app, title)?;
        let mut i = self.inner.lock().ok()?;
        let e = i.alive.entry(label.to_string()).or_insert_with(|| (opener.to_string(), app.to_string(), None));
        if e.2.as_deref() == Some(k.as_str()) {
            return Some((k, None));
        }
        let first = e.2.is_none();
        e.2 = Some(k.clone());
        // It is out again: whatever offered it back has been answered.
        i.restore.retain(|r| r != &k);
        let place = if first { i.rects.iter().find(|(n, _)| n == &k).map(|x| x.1) } else { None };
        Some((k, place))
    }

    /// Where the window is now (kept in memory as it moves; written by `closed` and `flush`).
    pub fn moved(&self, label: &str, r: Rect) {
        if let Ok(mut i) = self.inner.lock() {
            let Some(k) = i.alive.get(label).and_then(|e| e.2.clone()) else { return };
            if let Some(p) = i.rects.iter().position(|(n, _)| n == &k) {
                i.rects.remove(p);
            }
            i.rects.push((k, r));
            while i.rects.len() > MAX_PLACES {
                i.rects.remove(0);
            }
        }
    }

    /// The pop-up is gone: its place is written.
    pub fn closed(&self, label: &str) {
        if let Ok(mut i) = self.inner.lock() {
            i.alive.remove(label);
        }
        self.flush();
    }

    /// An app window is closing: the pop-ups it opened are noted to be offered back next time; returns their labels (to close).
    pub fn opener_closing(&self, opener: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(mut i) = self.inner.lock() {
            let mine: Vec<(String, Option<String>)> = i.alive.iter().filter(|(_, e)| e.0 == opener).map(|(l, e)| (l.clone(), e.2.clone())).collect();
            for (l, k) in mine {
                if let Some(k) = k {
                    if !i.restore.contains(&k) {
                        i.restore.push(k);
                    }
                }
                out.push(l);
            }
        }
        self.flush();
        out
    }

    /// The block titles of `app` that were out when it last closed, each once.
    pub fn take_restore(&self, app: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(mut i) = self.inner.lock() {
            let (mine, rest): (Vec<String>, Vec<String>) = i.restore.drain(..).partition(|k| app_of_key(k) == app);
            i.restore = rest;
            out = mine.iter().map(|k| title_of_key(k).to_string()).collect();
        }
        if !out.is_empty() {
            self.flush();
        }
        out
    }

    pub fn flush(&self) {
        let (Some(f), Ok(i)) = (self.file.as_ref(), self.inner.lock()) else { return };
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let tmp = f.with_extension("tmp");
        if std::fs::write(&tmp, format(&i)).is_ok() {
            let _ = std::fs::rename(&tmp, f);
        }
    }
}

fn format(i: &Inner) -> String {
    let mut s = String::from("# CadCraft Studio: where torn-off windows were. App, block title and numbers only.\n");
    for (k, r) in &i.rects {
        s.push_str(&format!("rect\t{k}\t{}\t{}\t{}\t{}\t{}\n", r.x, r.y, r.w, r.h, r.scale_milli));
    }
    for k in &i.restore {
        s.push_str(&format!("restore\t{k}\n"));
    }
    s
}

fn parse(t: &str, i: &mut Inner) {
    for line in t.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        match f.as_slice() {
            ["rect", k, x, y, w, h, s] => {
                if let (Ok(x), Ok(y), Ok(w), Ok(h), Ok(s)) = (x.parse(), y.parse(), w.parse(), h.parse(), s.parse()) {
                    if i.rects.len() < MAX_PLACES * 2 {
                        i.rects.push((k.to_string(), Rect { x, y, w, h, scale_milli: s }));
                    }
                }
            }
            ["restore", k] => i.restore.push(k.to_string()),
            _ => {}
        }
    }
    while i.rects.len() > MAX_PLACES {
        i.rects.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32) -> Rect {
        Rect { x, y, w: 420, h: 640, scale_milli: 1000 }
    }

    #[test]
    fn keys_are_one_clean_line() {
        assert_eq!(key("cadcraft", "Toolbox").as_deref(), Some("cadcraft|Toolbox"));
        assert_eq!(key("cadcraft", "  Tool\tbox\n ").as_deref(), Some("cadcraft|Toolbox"));
        assert_eq!(key("cadcraft", ""), None);
        assert_eq!(key("cadcraft", "about:blank"), None);
        assert_eq!(key("", "Toolbox"), None);
        assert!(key("a", &"x".repeat(500)).unwrap().chars().count() == 96);
    }

    #[test]
    fn a_place_counts_only_where_a_screen_shows_it() {
        let two = [Mon { x: 0, y: 0, w: 1920, h: 1080 }, Mon { x: 1920, y: 0, w: 2560, h: 1440 }];
        assert!(r(2000, 100).shown_on(&two), "on the second screen");
        assert!(r(1700, 100).shown_on(&two), "across both");
        assert!(!r(4600, 100).shown_on(&two), "the screen it was on is gone");
        assert!(r(2000, 100).shown_on(&two[1..]));
        assert!(!r(100, 100).shown_on(&two[1..]), "only the right screen is left");
        assert!(!r(1900, 1070).shown_on(&two[..1]), "a sliver at the corner does not count");
    }

    #[test]
    fn size_comes_back_in_logical_pixels() {
        let p = Rect { x: 0, y: 0, w: 840, h: 1280, scale_milli: 2000 };
        assert_eq!(p.logical_size(), (420.0, 640.0));
        assert_eq!(Rect { scale_milli: 0, ..p }.logical_size(), (840.0, 1280.0));
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cc-popup-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("popup-windows.txt")
    }

    #[test]
    fn a_window_is_remembered_across_a_restart() {
        let f = tmp("a");
        let s = PopupState::new(Some(f.clone()));
        s.opened("popup-0", "main", "cadcraft");
        let (k, place) = s.titled("popup-0", "main", "cadcraft", "Toolbox").unwrap();
        assert_eq!(k, "cadcraft|Toolbox");
        assert!(place.is_none(), "nothing kept yet");
        s.moved("popup-0", r(2100, 80));
        s.moved("popup-0", r(2150, 90));
        s.closed("popup-0");
        // a new start
        let s2 = PopupState::new(Some(f.clone()));
        s2.opened("popup-0", "main", "cadcraft");
        let (_, place) = s2.titled("popup-0", "main", "cadcraft", "Toolbox").unwrap();
        assert_eq!(place, Some(r(2150, 90)), "the last place, on the second screen");
        // another app's Toolbox is its own window
        s2.opened("popup-1", "app-bimcraft-1", "bimcraft");
        assert_eq!(s2.titled("popup-1", "app-bimcraft-1", "bimcraft", "Toolbox").unwrap().1, None);
        // a title set twice does not move the window again
        assert_eq!(s2.titled("popup-0", "main", "cadcraft", "Toolbox").unwrap().1, None);
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn what_was_out_when_the_app_closed_is_offered_back_once() {
        let f = tmp("b");
        let s = PopupState::new(Some(f.clone()));
        for (l, t) in [("popup-0", "Toolbox"), ("popup-1", "Layers")] {
            s.opened(l, "main", "cadcraft");
            s.titled(l, "main", "cadcraft", t);
            s.moved(l, r(10, 10));
        }
        s.opened("popup-2", "app-bimcraft-1", "bimcraft");
        s.titled("popup-2", "app-bimcraft-1", "bimcraft", "Toolbox");
        s.closed("popup-1"); // the person put Layers back before closing
        let mut gone = s.opener_closing("main");
        gone.sort();
        assert_eq!(gone, vec!["popup-0".to_string()], "only the pop-ups of that window, still out");
        // restart
        let s2 = PopupState::new(Some(f.clone()));
        assert_eq!(s2.take_restore("bimcraft"), Vec::<String>::new(), "the other app's window was not closing");
        assert_eq!(s2.take_restore("cadcraft"), vec!["Toolbox".to_string()]);
        assert!(s2.take_restore("cadcraft").is_empty(), "offered once");
        let s3 = PopupState::new(Some(f.clone()));
        assert!(s3.take_restore("cadcraft").is_empty(), "and not again after the next start");
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn the_file_is_capped_and_survives_junk() {
        let f = tmp("c");
        let s = PopupState::new(Some(f.clone()));
        for n in 0..(MAX_PLACES + 10) {
            let l = format!("popup-{n}");
            s.opened(&l, "main", "a");
            s.titled(&l, "main", "a", &format!("T{n}"));
            s.moved(&l, r(n as i32, 0));
        }
        s.flush();
        let t = std::fs::read_to_string(&f).unwrap();
        assert_eq!(t.lines().filter(|l| l.starts_with("rect\t")).count(), MAX_PLACES);
        assert!(!t.contains("a|T0\t") && t.contains(&format!("a|T{}\t", MAX_PLACES + 9)), "the oldest went first");
        std::fs::write(&f, "garbage\nrect\tx\tnot\tnumbers\nrect\ta|T\t1\t2\t3\t4\t1000\n").unwrap();
        let s2 = PopupState::new(Some(f.clone()));
        s2.opened("p", "main", "a");
        assert_eq!(s2.titled("p", "main", "a", "T").unwrap().1, Some(Rect { x: 1, y: 2, w: 3, h: 4, scale_milli: 1000 }));
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }

    #[test]
    fn nothing_but_names_and_numbers_is_written() {
        let f = tmp("d");
        let s = PopupState::new(Some(f.clone()));
        s.opened("p", "main", "cadcraft");
        s.titled("p", "main", "cadcraft", "Toolbox");
        s.moved("p", r(1, 2));
        s.closed("p");
        let t = std::fs::read_to_string(&f).unwrap();
        assert!(t.lines().skip(1).all(|l| l.starts_with("rect\tcadcraft|Toolbox\t") || l.starts_with("restore\t")));
        let _ = std::fs::remove_dir_all(f.parent().unwrap());
    }
}
