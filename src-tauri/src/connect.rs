// SPDX-License-Identifier: Apache-2.0
//! Studio › Connect your AI, the native half (docs/AI-CONNECT-SPEC.md § 6; Track AC.2). The hub page (studio/hub) decides what to write
//! with tools/mcp/clients.mjs; this file is the only file access that page has for AI apps' settings, and it is a FIXED list:
//! the settings file of each known AI app (+ its dated backup and its temp file), that app's folder, the names of four programs to
//! look for, and exactly three `claude mcp …` command lines. Anything else is refused. Only the hub window may ask, and only in the
//! suite. The table below is the same as `CLIENTS` in clients.mjs (`node tools/mcp/test/connect-table.mjs` compares them).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tauri::{Runtime, State, WebviewWindow};

const MAX_READ: u64 = 2 * 1024 * 1024;

/// id, folder on Windows, folder on macOS, folder on Linux, file. {home} {appdata} {xdgConfig} are filled in by `resolve`.
const TABLE: &[(&str, &str, &str, &str, &str)] = &[
    ("claude-desktop", "{appdata}/Claude", "{home}/Library/Application Support/Claude", "{xdgConfig}/Claude", "claude_desktop_config.json"),
    ("cursor", "{home}/.cursor", "{home}/.cursor", "{home}/.cursor", "mcp.json"),
    ("windsurf", "{home}/.codeium/windsurf", "{home}/.codeium/windsurf", "{home}/.codeium/windsurf", "mcp_config.json"),
    ("vscode", "{appdata}/Code/User", "{home}/Library/Application Support/Code/User", "{xdgConfig}/Code/User", "mcp.json"),
    ("codex", "{home}/.codex", "{home}/.codex", "{home}/.codex", "config.toml"),
    ("gemini", "{home}/.gemini", "{home}/.gemini", "{home}/.gemini", "settings.json"),
];
/// Programs `which` may look for.
const PROGRAMS: &[&str] = &["claude", "codex", "gemini", "code"];
/// The only command lines `run` may start (Claude Code's own `claude mcp`): add, remove, list.
const RUNS: &[&[&str]] = &[
    &["mcp", "add", "cadcraft", "--scope", "user", "--", "npx", "-y", "@getcadcraft/mcp", "connect"],
    &["mcp", "remove", "cadcraft", "--scope", "user"],
    &["mcp", "list"],
];

#[derive(Clone)]
pub struct Env {
    pub os: &'static str,
    pub home: String,
    pub appdata: String,
    pub xdg: String,
}

pub fn env_now() -> Env {
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default();
    let os = if cfg!(target_os = "windows") { "win32" } else if cfg!(target_os = "macos") { "darwin" } else { "linux" };
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| format!("{home}\\AppData\\Roaming"));
    let xdg = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{home}/.config"));
    Env { os, home, appdata, xdg }
}

fn resolve(tpl: &str, e: &Env) -> String {
    let p = tpl.replace("{home}", &e.home).replace("{appdata}", &e.appdata).replace("{xdgConfig}", &e.xdg);
    if e.os == "win32" { p.replace('/', "\\") } else { p }
}

fn dir_of(row: &(&str, &str, &str, &str, &str), e: &Env) -> String {
    resolve(match e.os { "win32" => row.1, "darwin" => row.2, _ => row.3 }, e)
}

fn sep(e: &Env) -> &'static str {
    if e.os == "win32" { "\\" } else { "/" }
}

/// Is `path` one of the settings files, its dated backup (`.cadcraft-backup-<digits>`) or its temp file (`.cadcraft-tmp`)?
pub fn allowed_file(path: &str, e: &Env) -> bool {
    TABLE.iter().any(|row| {
        let file = format!("{}{}{}", dir_of(row, e), sep(e), row.4);
        path == file
            || path == format!("{file}.cadcraft-tmp")
            || path.strip_prefix(&format!("{file}.cadcraft-backup-")).is_some_and(|d| !d.is_empty() && d.len() <= 14 && d.bytes().all(|b| b.is_ascii_digit()))
    })
}

pub fn allowed_dir(path: &str, e: &Env) -> bool {
    TABLE.iter().any(|row| dir_of(row, e) == path)
}

pub fn allowed_run(cmd: &str, args: &[String]) -> bool {
    cmd == "claude" && RUNS.iter().any(|r| r.len() == args.len() && r.iter().zip(args).all(|(a, b)| a == b))
}

fn guard<R: Runtime>(w: &WebviewWindow<R>, suite: &crate::windows::Suite) -> Result<(), String> {
    if !suite.on {
        return Err("off: not the suite".into());
    }
    if w.label() != "main" {
        return Err("not_allowed: only the hub".into());
    }
    Ok(())
}

fn check_file(path: &str) -> Result<(), String> {
    if allowed_file(path, &env_now()) { Ok(()) } else { Err("not_allowed: that file is not one of the AI apps' settings".into()) }
}

#[tauri::command]
pub fn cc_connect_env<R: Runtime>(window: WebviewWindow<R>, suite: State<'_, crate::windows::Suite>) -> Result<Value, String> {
    guard(&window, &suite)?;
    let e = env_now();
    Ok(json!({ "platform": e.os, "home": e.home, "appdata": e.appdata, "xdgConfig": e.xdg }))
}

#[tauri::command]
pub fn cc_connect_exists<R: Runtime>(window: WebviewWindow<R>, suite: State<'_, crate::windows::Suite>, path: String) -> Result<bool, String> {
    guard(&window, &suite)?;
    let e = env_now();
    if !allowed_file(&path, &e) && !allowed_dir(&path, &e) {
        return Err("not_allowed: not a place Connect your AI looks".into());
    }
    Ok(Path::new(&path).exists())
}

#[tauri::command]
pub fn cc_connect_read<R: Runtime>(window: WebviewWindow<R>, suite: State<'_, crate::windows::Suite>, path: String) -> Result<String, String> {
    guard(&window, &suite)?;
    check_file(&path)?;
    let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_READ {
        return Err("too_large: the settings file is over 2 MB".into());
    }
    std::fs::read_to_string(&path).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cc_connect_write<R: Runtime>(window: WebviewWindow<R>, suite: State<'_, crate::windows::Suite>, path: String, text: String) -> Result<(), String> {
    guard(&window, &suite)?;
    check_file(&path)?;
    if text.len() as u64 > MAX_READ {
        return Err("too_large".into());
    }
    std::fs::write(&path, text).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cc_connect_rename<R: Runtime>(window: WebviewWindow<R>, suite: State<'_, crate::windows::Suite>, from: String, to: String) -> Result<(), String> {
    guard(&window, &suite)?;
    check_file(&from)?;
    check_file(&to)?;
    std::fs::rename(&from, &to).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cc_connect_mkdirp<R: Runtime>(window: WebviewWindow<R>, suite: State<'_, crate::windows::Suite>, path: String) -> Result<(), String> {
    guard(&window, &suite)?;
    if !allowed_dir(&path, &env_now()) {
        return Err("not_allowed: not an AI app's settings folder".into());
    }
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())
}

fn find_program(name: &str) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) { std::env::var("PATHEXT").unwrap_or(".EXE;.CMD;.BAT".into()).split(';').map(|s| s.to_string()).collect() } else { vec![String::new()] };
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|dir| exts.iter().map(|x| dir.join(format!("{name}{x}"))).find(|p| p.is_file()))
}

#[tauri::command]
pub fn cc_connect_which<R: Runtime>(window: WebviewWindow<R>, suite: State<'_, crate::windows::Suite>, cmd: String) -> Result<Option<String>, String> {
    guard(&window, &suite)?;
    if !PROGRAMS.contains(&cmd.as_str()) {
        return Err("not_allowed: not a program Connect your AI looks for".into());
    }
    Ok(find_program(&cmd).map(|p| p.to_string_lossy().into_owned()))
}

#[tauri::command]
pub fn cc_connect_run<R: Runtime>(window: WebviewWindow<R>, suite: State<'_, crate::windows::Suite>, cmd: String, args: Vec<String>) -> Result<Value, String> {
    guard(&window, &suite)?;
    if !allowed_run(&cmd, &args) {
        return Err("not_allowed: that command line is not one Connect your AI runs".into());
    }
    let program = find_program(&cmd).ok_or("not_found: Claude Code is not on this computer's PATH")?;
    #[cfg(windows)]
    let out = std::process::Command::new("cmd").arg("/C").arg(&program).args(&args).output();
    #[cfg(not(windows))]
    let out = std::process::Command::new(&program).args(&args).output();
    let out = out.map_err(|e| e.to_string())?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Ok(json!({ "code": out.status.code().unwrap_or(1), "out": text.chars().take(4000).collect::<String>() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lin() -> Env { Env { os: "linux", home: "/home/me".into(), appdata: "/home/me/AppData/Roaming".into(), xdg: "/home/me/.config".into() } }
    fn win() -> Env { Env { os: "win32", home: "C:\\Users\\me".into(), appdata: "C:\\Users\\me\\AppData\\Roaming".into(), xdg: "C:\\Users\\me\\.config".into() } }

    #[test]
    fn only_the_known_settings_files() {
        let e = lin();
        assert!(allowed_file("/home/me/.config/Claude/claude_desktop_config.json", &e));
        assert!(allowed_file("/home/me/.cursor/mcp.json", &e));
        assert!(allowed_file("/home/me/.codex/config.toml", &e));
        assert!(allowed_file("/home/me/.config/Claude/claude_desktop_config.json.cadcraft-backup-202610281230", &e));
        assert!(allowed_file("/home/me/.cursor/mcp.json.cadcraft-tmp", &e));
        for bad in [
            "/home/me/.ssh/id_rsa", "/home/me/.config/Claude/../../.ssh/id_rsa", "/home/me/.cursor/mcp.json.bak", "/home/me/.cursor/mcp.json.cadcraft-backup-",
            "/home/me/.cursor/mcp.json.cadcraft-backup-12ab", "/home/me/.cursor/mcp.json.cadcraft-backup-123456789012345", "/home/me/.cursor/other.json", "/etc/passwd", "",
        ] {
            assert!(!allowed_file(bad, &e), "{bad}");
        }
        assert!(allowed_dir("/home/me/.gemini", &e) && !allowed_dir("/home/me", &e) && !allowed_dir("/home/me/.gemini/x", &e));
    }

    #[test]
    fn windows_paths_use_backslashes() {
        let e = win();
        assert!(allowed_file("C:\\Users\\me\\AppData\\Roaming\\Claude\\claude_desktop_config.json", &e));
        assert!(allowed_file("C:\\Users\\me\\.cursor\\mcp.json", &e));
        assert!(!allowed_file("C:/Users/me/.cursor/mcp.json", &e));
    }

    #[test]
    fn only_three_command_lines() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(allowed_run("claude", &a(&["mcp", "list"])));
        assert!(allowed_run("claude", &a(&["mcp", "remove", "cadcraft", "--scope", "user"])));
        assert!(allowed_run("claude", &a(&["mcp", "add", "cadcraft", "--scope", "user", "--", "npx", "-y", "@getcadcraft/mcp", "connect"])));
        assert!(!allowed_run("claude", &a(&["mcp", "add", "cadcraft", "--scope", "user", "--", "sh", "-c", "evil"])));
        assert!(!allowed_run("claude", &a(&["mcp", "list", "&&", "rm"])));
        assert!(!allowed_run("sh", &a(&["mcp", "list"])));
        assert!(!allowed_run("codex", &a(&["mcp", "list"])));
    }
}
