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
}

static LOG: OnceLock<Mutex<Log>> = OnceLock::new();
static LOADED: AtomicBool = AtomicBool::new(false);

fn log() -> &'static Mutex<Log> {
    LOG.get_or_init(|| Mutex::new(Log { began: Instant::now(), path: None, pending: Vec::new(), last: String::new() }))
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
    let line = format!("+{:>6} ms [{}] {stage}", l.began.elapsed().as_millis(), t.name().unwrap_or("worker"));
    l.last = stage.to_string();
    match l.path.clone() {
        Some(p) => append(&p, &line),
        None => l.pending.push(line),
    }
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

    #[test]
    fn writes_pending_lines_and_keeps_the_last_start() {
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
}
