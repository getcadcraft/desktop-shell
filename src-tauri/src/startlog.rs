// SPDX-License-Identifier: Apache-2.0
//! The start-up log (0.9 FX.29): one small text file a person can send when Studio does not start (Apple silicon, 2026-10-25: "does not
//! start until it is force quit"; the cause is NOT known, so this records where a start stops instead of guessing).
//!
//! `startup.log` sits in the app-data folder (portable copy: `data/`; macOS: `~/Library/Application Support/<identifier>/`; Windows:
//! `%APPDATA%\<identifier>\`; Linux: `~/.local/share/<identifier>/`). Each start writes a fresh log and keeps the one before as
//! `startup.previous.log`, so the log of the start that hung is still there after the force quit and the next start. Every line is
//! `+<milliseconds since the shell began> [thread] stage`. It holds no file names, paths of documents, tokens or account data: stages,
//! the version, the system and the process's own folder only.
//!
//! A watchdog adds a line when the main page has not finished loading 20 seconds after the window was built, naming the last stage
//! reached, and a panic hook adds the panic's place and message. `mark` before the data folder is known is kept in memory and written
//! when `init` runs.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

struct Log {
    began: Instant,
    path: Option<PathBuf>,
    pending: Vec<String>,
    last: String,
    /// (milliseconds since the shell began, stage) of this start, for `summary` (0.10 PERF.18: the About box shows the total).
    marks: Vec<(u128, String)>,
}

static LOG: OnceLock<Mutex<Log>> = OnceLock::new();
static LOADED: AtomicBool = AtomicBool::new(false);

fn log() -> &'static Mutex<Log> {
    LOG.get_or_init(|| Mutex::new(Log { began: Instant::now(), path: None, pending: Vec::new(), last: String::new(), marks: Vec::new() }))
}

fn append(path: &PathBuf, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

/// Record one stage of the start. Cheap, never panics, safe from any thread.
pub fn mark(stage: &str) {
    let Ok(mut l) = log().lock() else { return };
    let t = std::thread::current();
    let ms = l.began.elapsed().as_millis();
    let line = format!("+{:>6} ms [{}] {stage}", ms, t.name().unwrap_or("worker"));
    l.last = stage.to_string();
    if l.marks.len() < 400 {
        l.marks.push((ms, stage.to_string()));
    }
    match l.path.clone() {
        Some(p) => append(&p, &line),
        None => l.pending.push(line),
    }
}

/// Record a stage only the first time (the first page request, the first paint): later calls are ignored.
pub fn mark_once(stage: &str) {
    let seen = log().lock().map(|l| l.marks.iter().any(|(_, s)| s == stage)).unwrap_or(true);
    if !seen {
        mark(stage);
    }
}

/// The two stages the page reports itself (0.10 PERF.18), through `cadcraftDesktop.startLog.mark`: nothing else a page sends is written.
pub const PAGE_STAGES: [&str; 2] = ["app module running", "first drawing paint"];

/// A page-reported stage: only the two names above, only the first time, from the main window or an app window (`from` is the window's
/// label, written as given: `main`, `app-cadcraft-2`). Returns whether it was written.
pub fn page_stage(from: &str, stage: &str) -> bool {
    if !PAGE_STAGES.contains(&stage) {
        return false;
    }
    let ok_label = from == "main" || (from.starts_with("app-") && from.len() <= 40 && from.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
    if !ok_label {
        return false;
    }
    let line = format!("{stage} ({from})");
    let seen = log().lock().map(|l| l.marks.iter().any(|(_, s)| *s == line)).unwrap_or(true);
    if seen {
        return false;
    }
    mark(&line);
    true
}

fn at(marks: &[(u128, String)], start: &str) -> Option<u128> {
    marks.iter().find(|(_, s)| s.starts_with(start)).map(|m| m.0)
}

/// Budget (PERF.18): the window shown in under 1 s, the app usable in under 3 s on a warm start.
pub const BUDGET_WINDOW_MS: u128 = 1000;
pub const BUDGET_USABLE_MS: u128 = 3000;

/// This start in one line of JSON for the About box: the milliseconds of the stages a person can name, `null` for one not reached yet,
/// and the two budget answers. No names, paths or account data (every stage is a literal).
pub fn summary() -> String {
    let m = log().lock().map(|l| l.marks.clone()).unwrap_or_default();
    let j = |v: Option<u128>| v.map(|n| n.to_string()).unwrap_or_else(|| "null".into());
    let window = at(&m, "setup: main window built");
    let request = at(&m, "first page request");
    let loaded = at(&m, "main page finished loading");
    let module = at(&m, "app module running");
    let paint = at(&m, "first drawing paint");
    // "usable" = the first drawing paint when the app reports one, else the module running, else the page loaded
    let usable = paint.or(module).or(loaded);
    let within = |v: Option<u128>, max: u128| v.map(|n| (n <= max).to_string()).unwrap_or_else(|| "null".into());
    format!(
        "{{\"windowBuiltMs\":{},\"firstRequestMs\":{},\"pageLoadedMs\":{},\"moduleRunningMs\":{},\"firstPaintMs\":{},\"totalMs\":{},\"windowWithinBudget\":{},\"usableWithinBudget\":{},\"budgetWindowMs\":{BUDGET_WINDOW_MS},\"budgetUsableMs\":{BUDGET_USABLE_MS}}}",
        j(window),
        j(request),
        j(loaded),
        j(module),
        j(paint),
        j(usable),
        within(window, BUDGET_WINDOW_MS),
        within(usable, BUDGET_USABLE_MS)
    )
}

/// The text of this start's log, or the one before's (`previous`), for the About box's "Copy" and "Save" (the last 64 kB).
pub fn text(previous: bool) -> String {
    let Some(p) = log().lock().ok().and_then(|l| l.path.clone()) else { return String::new() };
    let p = if previous { p.with_file_name("startup.previous.log") } else { p };
    let t = std::fs::read_to_string(p).unwrap_or_default();
    if t.len() <= 65536 {
        return t;
    }
    let mut cut = t.len() - 65536;
    while !t.is_char_boundary(cut) {
        cut += 1;
    }
    t[cut..].to_string()
}

/// The folder is known: keep the last start's log as `startup.previous.log`, begin this start's, write what was marked before.
pub fn init(path: Option<PathBuf>) {
    let Some(path) = path else { return };
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let prev = path.with_file_name("startup.previous.log");
    if path.is_file() {
        let _ = std::fs::rename(&path, &prev);
    }
    let Ok(mut l) = log().lock() else { return };
    let header = format!(
        "CadCraft Studio start-up log. This file lists the steps of one start; it has no file names, passwords or account data. Version {} on {} {}.",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    append(&path, &header);
    for line in l.pending.drain(..) {
        append(&path, &line);
    }
    l.path = Some(path);
}

/// The main page finished loading: the start worked.
pub fn page_loaded() {
    LOADED.store(true, Ordering::SeqCst);
    mark("main page finished loading");
}

/// A panic goes into the log (place and message), then on to the previous hook (the usual message and abort rules stay).
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        mark(&format!("PANIC {info}"));
        prev(info);
    }));
}

/// 20 s after the main window was built: if its page has not finished loading, say so and name the last stage. Runs once.
pub fn watchdog() {
    let _ = std::thread::Builder::new().name("startlog-watchdog".into()).spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(20));
        if !LOADED.load(Ordering::SeqCst) {
            let last = log().lock().map(|l| l.last.clone()).unwrap_or_default();
            mark(&format!("WATCHDOG 20 s after the window was built and the main page has not finished loading; last stage before this line: {last}"));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // the log is one process-wide value: tests that start it again run one at a time
    static ONE: Mutex<()> = Mutex::new(());

    #[test]
    fn writes_pending_lines_and_keeps_the_last_start() {
        let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let d = std::env::temp_dir().join(format!("cc-startlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let f = d.join("startup.log");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(&f, "old start").unwrap();
        mark("before the folder was known");
        init(Some(f.clone()));
        mark("after");
        let t = std::fs::read_to_string(&f).unwrap();
        assert!(t.contains("before the folder was known") && t.contains("after") && t.starts_with("CadCraft Studio start-up log"));
        assert_eq!(std::fs::read_to_string(d.join("startup.previous.log")).unwrap(), "old start");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn summary_text_and_page_stages() {
        let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
        let d = std::env::temp_dir().join(format!("cc-startlog2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        init(Some(d.join("startup.log")));
        mark("setup: main window built, waiting for its page");
        mark_once("first page request served");
        mark_once("first page request served");
        mark("main page finished loading");
        // only the two names, only once, only from our own windows
        assert!(!page_stage("main", "my own words"));
        assert!(!page_stage("popup-1", "app module running"));
        assert!(!page_stage("app-x y", "app module running"));
        assert!(page_stage("app-cadcraft-1", "app module running"));
        assert!(!page_stage("app-cadcraft-1", "app module running"));
        assert!(page_stage("main", "first drawing paint"));
        let s = summary();
        for k in ["windowBuiltMs", "firstRequestMs", "pageLoadedMs", "moduleRunningMs", "firstPaintMs", "totalMs", "windowWithinBudget", "usableWithinBudget"] {
            assert!(s.contains(&format!("\"{k}\":")), "{k} in {s}");
        }
        assert!(!s.contains("null,\"totalMs\"") || s.contains("firstPaintMs\":null"), "{s}");
        assert!(s.contains("\"windowWithinBudget\":true"), "a test start is quick: {s}");
        let t = text(false);
        assert_eq!(t.matches("first page request served").count(), 1, "once");
        assert!(t.contains("app module running (app-cadcraft-1)") && t.contains("first drawing paint (main)"));
        assert!(text(true).is_empty() || text(true).len() < 65537);
        let _ = std::fs::remove_dir_all(&d);
        if let Ok(mut l) = log().lock() {
            l.path = None;
            l.marks.clear();
        }
    }
}
