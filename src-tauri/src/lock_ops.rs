// SPDX-License-Identifier: Apache-2.0
//! Hand-over to `platforms-08` (from `vault-08`, SU.1058 step 1): the two file operations strict vault locks need.
//! Paste into desktop-shell/src-tauri/src/files.rs (next to write_atomic); `check`, `io_err`, `Grants` are that file's.
//! Standalone check (no tauri, no cargo):  rustc --edition 2021 --test lock_ops.rs -o /tmp/lock_ops && /tmp/lock_ops
use std::path::Path;

fn io_err(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => format!("not_found: {e}"),
        std::io::ErrorKind::PermissionDenied => format!("permission_denied: {e}"),
        _ => e.to_string(),
    }
}

/// Make the file only if it does not exist (the OS decides: `create_new` = O_EXCL / CREATE_NEW). → Ok(true) when this call
/// made it, Ok(false) when it was already there. Folders above it are made. A write that fails after the file was made
/// removes it again, so a lock is never left empty.
pub fn create_exclusive(p: &Path, bytes: &[u8]) -> Result<bool, String> {
    use std::io::Write;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io_err(&e))?;
    }
    let mut f = match std::fs::OpenOptions::new().write(true).create_new(true).open(p) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(io_err(&e)),
    };
    if let Err(e) = f.write_all(bytes).and_then(|_| f.sync_all()) {
        drop(f);
        let _ = std::fs::remove_file(p);
        return Err(io_err(&e));
    }
    Ok(true)
}

/// 0.11 ORS.27: a drawing's own lock file is the drawing's whole name plus `.cclock` (`Plan.dwg` → `Plan.dwg.cclock`,
/// drawing-lock.js `lockPathFor`). Given a normalised path, → the drawing it locks, when it is one: the `.cclock` ending in
/// any case (paths are case-folded on Windows and macOS), something before it, and not a bare separator. Grants::allows
/// lets the page reach this one sibling of a file the person opened, nothing else beside it.
pub fn lock_target(n: &str) -> Option<&str> {
    const END: &str = ".cclock";
    if n.len() <= END.len() || !n.is_char_boundary(n.len() - END.len()) {
        return None;
    }
    let (base, end) = n.split_at(n.len() - END.len());
    if !end.eq_ignore_ascii_case(END) || base.ends_with(['/', '\\']) || base.is_empty() {
        return None;
    }
    Some(base)
}

/// Delete one file. → Ok(true) when something was removed, Ok(false) when there was no such file. A folder is refused.
pub fn remove_one(p: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => Err("not_a_file: that is a folder".into()),
        Ok(_) => match std::fs::remove_file(p) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io_err(&e)),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_err(&e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cc-lockops-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn second_create_loses_and_keeps_the_first_bytes() {
        let d = tmp("create");
        let f = d.join(".ccvault/locks/a.json"); // folders above it do not exist yet
        assert_eq!(create_exclusive(&f, b"one"), Ok(true));
        assert_eq!(create_exclusive(&f, b"two"), Ok(false));
        assert_eq!(std::fs::read(&f).unwrap(), b"one");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn sixteen_threads_one_winner() {
        let d = tmp("race");
        let f = d.join("locks/x.json");
        let wins: usize = (0..16)
            .map(|i| { let f = f.clone(); std::thread::spawn(move || create_exclusive(&f, format!("t{i}").as_bytes()).unwrap()) })
            .collect::<Vec<_>>().into_iter().map(|h| h.join().unwrap() as usize).sum();
        assert_eq!(wins, 1);
        let body = String::from_utf8(std::fs::read(&f).unwrap()).unwrap();
        assert!(body.starts_with('t') && body.len() >= 2, "the winner's bytes are whole: {body}");
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn remove_says_whether_it_removed_and_refuses_folders() {
        let d = tmp("remove");
        let f = d.join("a.json");
        assert_eq!(remove_one(&f), Ok(false));
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(remove_one(&f), Ok(true));
        assert!(!f.exists());
        assert_eq!(remove_one(&f), Ok(false));
        assert!(remove_one(&d).unwrap_err().starts_with("not_a_file"));
        assert!(d.exists());
        std::fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn a_drawings_lock_names_its_drawing() {
        assert_eq!(lock_target("/jobs/Plan.dwg.cclock"), Some("/jobs/Plan.dwg"));
        assert_eq!(lock_target("c:\\jobs\\plan.dwg.cclock"), Some("c:\\jobs\\plan.dwg"));
        assert_eq!(lock_target("/jobs/Plan.dxf.CCLOCK"), Some("/jobs/Plan.dxf"));
        assert_eq!(lock_target("/jobs/Plan.dwg"), None);
        assert_eq!(lock_target(".cclock"), None);
        assert_eq!(lock_target("/jobs/.cclock"), None);
        assert_eq!(lock_target("/jobs/Plan.dwg.cclock.tmp"), None);
        assert_eq!(lock_target("/jobs/Plan.dwgcclock"), None);
        assert_eq!(lock_target("/j/é.cclock"), Some("/j/é"));
    }

    #[test]
    fn a_lock_can_be_taken_again_after_removal() {
        let d = tmp("again");
        let f = d.join("l.json");
        assert_eq!(create_exclusive(&f, b"a"), Ok(true));
        assert_eq!(remove_one(&f), Ok(true));
        assert_eq!(create_exclusive(&f, b"b"), Ok(true));
        std::fs::remove_dir_all(d).unwrap();
    }
}
