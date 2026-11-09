// SPDX-License-Identifier: Apache-2.0
// STUDIO COPY of getcadcraft.com/tools/render/studio-bridge/engines.rs (the `engines` session's file), mounted by studio_PART_3 (2026-10-28).
// One difference, owner rule 43 (no GPL program launched through Studio): Blender is GPL, so its two table rows (ENGINES, OPEN_IN) exist only
// with the cargo feature `blender`, which is OFF in every build we ship; its scripts are not shipped; "blender" is also in DENIED_PROGRAMS so an
// Owner, 2026-11-03: PrusaSlicer and OrcaSlicer (AGPL-3.0) may be launched here as Kind::Slicer, the person's own installed programs, never bundled: lib/printstage's picker.
// add-on cannot name it. Unreal Engine (a link engine, nothing runs), usdview and the system's own program stay. RE.4 is the owner's decision.
//! Outside render engines from Studio (0.8 RE.3, session `engines`; docs/0.8-PART1.md D3). The page never builds a command line:
//! it asks for "engine blender, this package, these typed settings" and gets progress and a picture back. Everything that decides
//! what runs lives here:
//!
//! - **Which programs.** A fixed table (`ENGINES`): an engine id, the file names its program may have, where the program lives on
//!   Windows, macOS and Linux (`candidates`), the lowest version we support. A person can pick the program once with a file dialog;
//!   the pick is only taken when the file's name is one of the engine's own (`valid_program`) and is remembered in `engines.conf`.
//! - **Which scripts.** Only the script shipped inside Studio (`script_path`: `<resources>/engines/<id>/<script>`, never a path from
//!   the page). The script gets one argument: the job file Rust wrote.
//! - **Which arguments.** Built here from typed fields (`BlenderSettings::parse` refuses unknown keys, bad values and out-of-range
//!   numbers; `blender_args` is the whole command line). No shell is involved; nothing from the page reaches an argument as text.
//! - **Where it runs.** A fresh temp folder per job (`cadcraft-engine-<id>`): the package (a stored zip from the page) is unpacked
//!   into it with every name checked (`unzip_stored`: relative names only, no `..`, bounded sizes, CRC), the job file is written,
//!   the program runs with that folder as its working directory, the result is read back from `out/` (`read_result`: a plain file
//!   inside the folder, ≤ 512 MiB) and the folder is removed whatever happened.
//! - **Progress and cancel.** The script prints lines the shell reads: `CCPROGRESS 0.42`, `CCRESULT out/result.png`,
//!   `CCERROR <code>`. Cancel kills the whole process tree (a process group on Linux and macOS, `taskkill /T` on Windows).
//!
//! Std only (the Tauri commands at the end are behind `cfg(not(standalone))`): `rustc --edition 2021 --test --cfg standalone
//! engines.rs` builds and runs the tests with no crates. Not compiled with the shell here (rule 25: no cargo on the VPS):
//! `platforms-08` builds it on workstation11a. Tablets: the commands answer `unsupported` (no outside programs there).

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub const MAX_PACKAGE_BYTES: usize = 2 * 1024 * 1024 * 1024;
pub const MAX_FILE_BYTES: usize = 1024 * 1024 * 1024;
pub const MAX_FILES: usize = 4096;
pub const MAX_RESULT_BYTES: u64 = 512 * 1024 * 1024;
/// A process engine's case file (JSON) is small; 1 MiB is far more than a flowsheet needs.
pub const MAX_CASE_BYTES: usize = 1024 * 1024;
/// A process engine that has not answered in this many seconds is stopped.
pub const MAX_PROCESS_SECONDS: u64 = 15 * 60;
/// A job that has not finished in this many seconds is stopped (a renderer that hangs must not hold the computer for ever).
pub const MAX_JOB_SECONDS: u64 = 12 * 3600;

fn bad(code: &str, msg: &str) -> String {
    format!("{code}: {msg}")
}

// ------------------------------------------------------------------------------------------------ the engine table

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Os {
    Windows,
    Mac,
    Linux,
}

impl Os {
    pub fn here() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Mac
        } else {
            Os::Linux
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Renders in the background and hands the picture back.
    Render,
    /// Takes the scene over in its own program (the person opens the project there).
    Link,
    /// Computes an answer for the page (0.8 CH.7, owner 2026-10-29: a permissively licensed process engine such as Cantera, run through the person's own Python): a case file in, a result
    /// file out, no picture. Opt-in at build time (cargo feature `cantera`, off in every build we ship until the owner builds it on Windows).
    Process,
    /// Turns a plate into G-code for the print stage (roadmap PS.14): the person's own installed slicer, a separate process. The STL comes from the page; the settings files
    /// and the whole command line are written here from typed fields; the G-code is read back and the folder removed.
    Slicer,
}

pub struct EngineDef {
    pub id: &'static str,
    /// Plain text for the page's list (engine names appear only in the engine list, the Open in list and their help page).
    pub name: &'static str,
    pub kind: Kind,
    /// Lower-case file names (without `.exe`) the program may have; a picked file must be one of them.
    pub programs: &'static [&'static str],
    /// The script inside Studio's resources, relative to `<resources>/engines/<id>/`; `None` = nothing runs (link engines).
    pub script: Option<&'static str>,
    /// Lowest (major, minor) that works (the spike in docs/spikes/RE.4 decides Blender's).
    pub min_version: (u32, u32),
}

pub const ENGINES: &[EngineDef] = &[
    #[cfg(feature = "blender")]
    EngineDef { id: "blender", name: "Blender", kind: Kind::Render, programs: &["blender"], script: Some("cc_blender_render.py"), min_version: (4, 2) },
    #[cfg(feature = "cantera")]
    EngineDef { id: "cantera", name: "Cantera", kind: Kind::Process, programs: &["python3", "python"], script: Some("cc_cantera_equilibrate.py"), min_version: (3, 9) },
    EngineDef { id: "unreal", name: "Unreal Engine", kind: Kind::Link, programs: &["unrealeditor"], script: None, min_version: (5, 3) },
    // Their version is not read (OrcaSlicer's --help and --version print nothing on Windows): a command they refuse is reported in the job's log.
    EngineDef { id: "prusaslicer", name: "PrusaSlicer", kind: Kind::Slicer, programs: &["prusa-slicer-console", "prusa-slicer", "prusaslicer"], script: None, min_version: (0, 0) },
    EngineDef { id: "orcaslicer", name: "OrcaSlicer", kind: Kind::Slicer, programs: &["orca-slicer", "orcaslicer"], script: None, min_version: (0, 0) },
];

pub fn engine(id: &str) -> Option<&'static EngineDef> {
    ENGINES.iter().find(|e| e.id == id)
}

/// "Blender 4.2.1 LTS" / "Blender 3.6.5" (the first line of `blender --version`) → (4, 2). Anything else → None.
pub fn parse_blender_version(out: &str) -> Option<(u32, u32)> {
    let line = out.lines().map(str::trim).find(|l| l.starts_with("Blender "))?;
    let mut it = line["Blender ".len()..].split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty());
    let major: u32 = it.next()?.parse().ok()?;
    let minor: u32 = it.next()?.parse().ok()?;
    Some((major, minor))
}

/// "Python 3.12.1" (the first line of `python --version`) → (3, 12). The Microsoft Store stub prints nothing useful on stdout, so it never parses.
pub fn parse_python_version(out: &str) -> Option<(u32, u32)> {
    let line = out.lines().map(str::trim).find(|l| l.starts_with("Python "))?;
    let mut it = line["Python ".len()..].split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty());
    let major: u32 = it.next()?.parse().ok()?;
    let minor: u32 = it.next()?.parse().ok()?;
    Some((major, minor))
}

/// "UE_5.4" (the folder the launcher installs into) → (5, 4).
pub fn parse_unreal_folder(name: &str) -> Option<(u32, u32)> {
    let rest = name.strip_prefix("UE_")?;
    let mut it = rest.split('.');
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// Where each engine's program usually is. `env` reads an environment variable, `list` the entries of a folder (both injected, so
/// the tests run every system's rules on any system). Only paths; nothing is run or opened here.
pub fn candidates(id: &str, os: Os, env: &dyn Fn(&str) -> Option<String>, list: &dyn Fn(&Path) -> Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let sorted_desc = |mut v: Vec<PathBuf>| {
        v.sort_by(|a, b| b.cmp(a));
        v
    };
    match (id, os) {
        ("blender", Os::Windows) => {
            for var in ["ProgramFiles", "ProgramW6432"] {
                if let Some(pf) = env(var) {
                    let base = Path::new(&pf).join("Blender Foundation");
                    for d in sorted_desc(list(&base)) {
                        if d.file_name().map(|n| n.to_string_lossy().starts_with("Blender ")).unwrap_or(false) {
                            out.push(d.join("blender.exe"));
                        }
                    }
                }
            }
        }
        ("blender", Os::Mac) => out.push(PathBuf::from("/Applications/Blender.app/Contents/MacOS/Blender")),
        ("blender", Os::Linux) => {
            for p in ["/usr/bin/blender", "/usr/local/bin/blender", "/snap/bin/blender"] {
                out.push(PathBuf::from(p));
            }
            for d in sorted_desc(list(Path::new("/opt"))) {
                if d.file_name().map(|n| n.to_string_lossy().to_lowercase().starts_with("blender")).unwrap_or(false) {
                    out.push(d.join("blender"));
                }
            }
        }
        ("cantera", Os::Windows) => {
            // python.org's per-user install (the default), newest first, then the system-wide folder
            if let Some(la) = env("LOCALAPPDATA") {
                let base = Path::new(&la).join("Programs").join("Python");
                for d in sorted_desc(list(&base)) {
                    if d.file_name().map(|n| n.to_string_lossy().starts_with("Python3")).unwrap_or(false) {
                        out.push(d.join("python.exe"));
                    }
                }
            }
            if let Some(pf) = env("ProgramFiles") {
                for d in sorted_desc(list(Path::new(&pf))) {
                    if d.file_name().map(|n| n.to_string_lossy().starts_with("Python3")).unwrap_or(false) {
                        out.push(d.join("python.exe"));
                    }
                }
            }
        }
        ("cantera", Os::Mac) => {
            for p in ["/opt/homebrew/bin/python3", "/usr/local/bin/python3", "/usr/bin/python3"] {
                out.push(PathBuf::from(p));
            }
        }
        ("cantera", Os::Linux) => {
            for p in ["/usr/bin/python3", "/usr/local/bin/python3"] {
                out.push(PathBuf::from(p));
            }
        }
        ("unreal", Os::Windows) => {
            for var in ["ProgramFiles", "ProgramW6432"] {
                if let Some(pf) = env(var) {
                    let base = Path::new(&pf).join("Epic Games");
                    for d in sorted_desc(list(&base)) {
                        if d.file_name().map(|n| parse_unreal_folder(&n.to_string_lossy()).is_some()).unwrap_or(false) {
                            out.push(d.join("Engine").join("Binaries").join("Win64").join("UnrealEditor.exe"));
                        }
                    }
                }
            }
        }
        ("unreal", Os::Mac) => {
            for d in sorted_desc(list(Path::new("/Users/Shared/Epic Games"))) {
                if d.file_name().map(|n| parse_unreal_folder(&n.to_string_lossy()).is_some()).unwrap_or(false) {
                    out.push(d.join("Engine/Binaries/Mac/UnrealEditor.app/Contents/MacOS/UnrealEditor"));
                }
            }
        }
        ("prusaslicer", Os::Windows) => {
            for var in ["ProgramFiles", "ProgramW6432"] {
                if let Some(pf) = env(var) {
                    out.push(Path::new(&pf).join("Prusa3D/PrusaSlicer/prusa-slicer-console.exe"));
                }
            }
        }
        ("prusaslicer", Os::Mac) => {
            out.push(PathBuf::from("/Applications/Original Prusa Drivers/PrusaSlicer.app/Contents/MacOS/PrusaSlicer"));
            out.push(PathBuf::from("/Applications/PrusaSlicer.app/Contents/MacOS/PrusaSlicer"));
        }
        ("prusaslicer", Os::Linux) => out.push(PathBuf::from("/usr/bin/prusa-slicer")),
        ("orcaslicer", Os::Windows) => {
            for var in ["ProgramFiles", "ProgramW6432"] {
                if let Some(pf) = env(var) {
                    out.push(Path::new(&pf).join("OrcaSlicer/orca-slicer.exe"));
                }
            }
        }
        ("orcaslicer", Os::Mac) => out.push(PathBuf::from("/Applications/OrcaSlicer.app/Contents/MacOS/OrcaSlicer")),
        ("orcaslicer", Os::Linux) => out.push(PathBuf::from("/usr/bin/orca-slicer")),
        _ => {} // Unreal on Linux has no standard place: the person picks it
    }
    out
}

/// A program the person picked: its file name must be one of the engine's own (so a pick cannot turn a button into "run anything").
/// A macOS `.app` folder is accepted and means the program inside it.
pub fn valid_program(def: &EngineDef, path: &Path) -> Result<PathBuf, String> {
    let mut p = path.to_path_buf();
    if p.extension().map(|e| e == "app").unwrap_or(false) && p.is_dir() {
        let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        p = p.join("Contents/MacOS").join(stem);
    }
    let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let name = name.strip_suffix(".exe").unwrap_or(&name).to_string();
    if !def.programs.iter().any(|x| *x == name) {
        return Err(bad("not_this_program", &format!("that file is not {}'s program", def.name)));
    }
    let meta = std::fs::metadata(&p).map_err(|_| bad("not_found", "that file is not there"))?;
    if !meta.is_file() {
        return Err(bad("not_found", "that is not a program file"));
    }
    Ok(p)
}

/// engines.conf: one `id<TAB>path` per line. A path with a tab or a line break cannot be stored.
pub fn parse_conf(text: &str) -> HashMap<String, PathBuf> {
    text.lines()
        .filter_map(|l| {
            let (id, p) = l.split_once('\t')?;
            (conf_key_ok(id) && !p.is_empty()).then(|| (id.to_string(), PathBuf::from(p)))
        })
        .collect()
}

pub fn write_conf(map: &HashMap<String, PathBuf>) -> Result<String, String> {
    let mut ids: Vec<&String> = map.keys().collect();
    ids.sort();
    let mut s = String::new();
    for id in ids {
        let p = map[id].to_string_lossy();
        if p.contains('\t') || p.contains('\n') || p.contains('\r') {
            return Err(bad("invalid", "that path cannot be remembered"));
        }
        s.push_str(&format!("{id}\t{p}\n"));
    }
    Ok(s)
}

// ------------------------------------------------------------------------------------------------ typed settings → arguments

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlenderEngine {
    Cycles,
    Eevee,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Device {
    Cpu,
    Gpu,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Png,
    Exr,
}

#[derive(Clone, PartialEq, Debug)]
pub struct BlenderSettings {
    pub engine: BlenderEngine,
    pub samples: u32,
    pub device: Device,
    pub denoise: bool,
    pub format: Format,
    pub width: u32,
    pub height: u32,
    /// An animation: frame range and rate (None = a still).
    pub frames: Option<(u32, u32, u32)>,
}

impl Default for BlenderSettings {
    fn default() -> Self {
        BlenderSettings { engine: BlenderEngine::Cycles, samples: 64, device: Device::Cpu, denoise: true, format: Format::Png, width: 1920, height: 1080, frames: None }
    }
}

impl BlenderSettings {
    /// From the page's key/value pairs (strings). Every key must be one we know, every value inside its range: a typo or a
    /// value we did not list is an error, never passed on. `frames` is "first,last,fps".
    pub fn parse(pairs: &[(&str, &str)]) -> Result<BlenderSettings, String> {
        let mut s = BlenderSettings::default();
        let num = |k: &str, v: &str, lo: u32, hi: u32| -> Result<u32, String> {
            let n: u32 = v.parse().map_err(|_| bad("invalid", &format!("{k} is not a number")))?;
            if n < lo || n > hi { Err(bad("invalid", &format!("{k} must be {lo} to {hi}"))) } else { Ok(n) }
        };
        for (k, v) in pairs {
            match *k {
                "engine" => s.engine = match *v { "cycles" => BlenderEngine::Cycles, "eevee" => BlenderEngine::Eevee, _ => return Err(bad("invalid", "engine must be cycles or eevee")) },
                "samples" => s.samples = num("samples", v, 1, 4096)?,
                "device" => s.device = match *v { "cpu" => Device::Cpu, "gpu" => Device::Gpu, _ => return Err(bad("invalid", "device must be cpu or gpu")) },
                "denoise" => s.denoise = match *v { "1" | "true" => true, "0" | "false" => false, _ => return Err(bad("invalid", "denoise must be true or false")) },
                "format" => s.format = match *v { "png" => Format::Png, "exr" => Format::Exr, _ => return Err(bad("invalid", "format must be png or exr")) },
                "width" => s.width = num("width", v, 1, 16384)?,
                "height" => s.height = num("height", v, 1, 16384)?,
                "frames" => {
                    let p: Vec<&str> = v.split(',').collect();
                    if p.len() != 3 {
                        return Err(bad("invalid", "frames must be first,last,fps"));
                    }
                    let (a, b, fps) = (num("first frame", p[0], 0, 1_000_000)?, num("last frame", p[1], 0, 1_000_000)?, num("fps", p[2], 1, 240)?);
                    if b < a || b - a > 100_000 {
                        return Err(bad("invalid", "the frame range is wrong"));
                    }
                    s.frames = Some((a, b, fps));
                }
                other => return Err(bad("invalid", &format!("{other} is not a setting of this engine"))),
            }
        }
        if (s.width as u64) * (s.height as u64) > 8192 * 8192 {
            return Err(bad("invalid", "the picture is too big"));
        }
        Ok(s)
    }

    /// The job file the script reads (`cc-engine-job.json`): fixed keys, numbers and words only, so it needs no escaping beyond the
    /// manifest's name, which is a checked relative path.
    pub fn job_json(&self, manifest: &str, out_dir: &str) -> String {
        let frames = match self.frames {
            Some((a, b, f)) => format!("[{a},{b},{f}]"),
            None => "null".to_string(),
        };
        format!(
            "{{\"format\":\"cadcraft-engine-job-1\",\"engine\":\"{}\",\"samples\":{},\"device\":\"{}\",\"denoise\":{},\"output\":\"{}\",\"width\":{},\"height\":{},\"frames\":{},\"manifest\":\"{}\",\"out\":\"{}\"}}",
            match self.engine { BlenderEngine::Cycles => "cycles", BlenderEngine::Eevee => "eevee" },
            self.samples,
            match self.device { Device::Cpu => "cpu", Device::Gpu => "gpu" },
            self.denoise,
            match self.format { Format::Png => "png", Format::Exr => "exr" },
            self.width,
            self.height,
            frames,
            manifest,
            out_dir
        )
    }
}

/// The whole command line for Blender: background, our script, nothing from the person's own startup file (their add-ons and
/// preferences must not change a render), the job file after `--`. The first value is the program, not an argument.
pub fn blender_args(script: &Path, job_file: &Path) -> Vec<String> {
    vec![
        "--background".into(),
        "--factory-startup".into(),
        "--noaudio".into(),
        "--python-exit-code".into(),
        "3".into(),
        "--python".into(),
        script.to_string_lossy().into_owned(),
        "--".into(),
        job_file.to_string_lossy().into_owned(),
    ]
}

/// The whole command line for a process engine's Python: our script, the case file, the result file. The first value is the program, not an argument.
pub fn python_args(script: &Path, case_file: &Path, result_file: &Path) -> Vec<String> {
    vec![script.to_string_lossy().into_owned(), case_file.to_string_lossy().into_owned(), result_file.to_string_lossy().into_owned()]
}

/// The shipped script: `<resources>/engines/<id>/<script>` for an engine in the table, and it must exist. Never a path from the page.
pub fn script_path(resources: &Path, def: &EngineDef) -> Result<PathBuf, String> {
    let s = def.script.ok_or_else(|| bad("no_script", &format!("{} does not run a script", def.name)))?;
    let p = resources.join("engines").join(def.id).join(s);
    if p.is_file() { Ok(p) } else { Err(bad("no_script", &format!("{}'s script is missing from this build", def.name))) }
}

// ------------------------------------------------------------------------------------------------ the package

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for i in 0..256u32 {
        let mut c = i;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        table[i as usize] = c;
    }
    let mut c = 0xFFFF_FFFFu32;
    for b in data {
        c = table[((c ^ *b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

/// A name inside a package: relative, forward slashes, no `.` or `..` or empty parts, no drive letter, no control characters.
pub fn safe_rel(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 260
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.contains(':')
        && !name.chars().any(|c| c.is_control())
        && name.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}

fn le16(b: &[u8], o: usize) -> usize {
    u16::from_le_bytes([b[o], b[o + 1]]) as usize
}
fn le32(b: &[u8], o: usize) -> usize {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as usize
}

/// The package the page sends (a stored zip, `render/handoff.js` zipPackage): every entry checked. → (name, bytes).
pub fn unzip_stored(b: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    if b.len() < 30 || b[0] != 0x50 || b[1] != 0x4b {
        return Err(bad("not_package", "that is not a package"));
    }
    if b.len() > MAX_PACKAGE_BYTES {
        return Err(bad("too_big", "the package is too big"));
    }
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let (mut o, mut total) = (0usize, 0usize);
    while o + 30 <= b.len() && le32(b, o) == 0x0403_4b50 {
        let (method, crc, size, nl, xl) = (le16(b, o + 8), le32(b, o + 14) as u32, le32(b, o + 18), le16(b, o + 26), le16(b, o + 28));
        if method != 0 {
            return Err(bad("compressed_entry", "the package has a compressed file"));
        }
        if out.len() >= MAX_FILES || size > MAX_FILE_BYTES {
            return Err(bad("too_big", "the package is too big"));
        }
        total = total.saturating_add(size);
        if total > MAX_PACKAGE_BYTES {
            return Err(bad("too_big", "the package is too big"));
        }
        let start = o + 30 + nl + xl;
        if start > b.len() || start.checked_add(size).map(|e| e > b.len()).unwrap_or(true) {
            return Err(bad("damaged", "the package is cut short"));
        }
        let name = std::str::from_utf8(&b[o + 30..o + 30 + nl]).map_err(|_| bad("bad_name", "a file name is not text"))?.to_string();
        if !safe_rel(&name) {
            return Err(bad("bad_name", "a file name in the package is not allowed"));
        }
        let data = &b[start..start + size];
        if crc32(data) != crc {
            return Err(bad("damaged", "a file in the package is damaged"));
        }
        if out.iter().any(|(n, _)| *n == name) {
            return Err(bad("bad_name", "a file name is there twice"));
        }
        out.push((name, data.to_vec()));
        o = start + size;
    }
    Ok(out)
}

/// A fresh job folder in the system's temp folder.
pub fn job_dir(id: u64) -> Result<PathBuf, String> {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) ^ ((std::process::id() as u128) << 64);
    let d = std::env::temp_dir().join(format!("cadcraft-engine-{id}-{:016x}", nonce as u64));
    std::fs::create_dir_all(d.join("out")).map_err(|e| bad("job_failed", &e.to_string()))?;
    Ok(d)
}

/// Write the unpacked package under `dir` (names already checked) and return the manifest's relative name.
pub fn write_package(dir: &Path, files: &[(String, Vec<u8>)]) -> Result<(), String> {
    if !files.iter().any(|(n, _)| n == "cadcraft-render.json") {
        return Err(bad("no_manifest", "the package has no manifest"));
    }
    for (name, data) in files {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(|e| bad("job_failed", &e.to_string()))?;
        }
        std::fs::write(&p, data).map_err(|e| bad("job_failed", &e.to_string()))?;
    }
    Ok(())
}

/// The picture the script says it made: `out/<plain name>.png|exr`, or `.zip` (an animation's frames, stored), a regular file inside the job folder (not a link, not outside it).
pub fn read_result(dir: &Path, rel: &str) -> Result<Vec<u8>, String> {
    let ok_name = safe_rel(rel) && rel.starts_with("out/") && rel.matches('/').count() == 1 && (rel.ends_with(".png") || rel.ends_with(".exr") || rel.ends_with(".zip") || rel == "out/result.json");
    if !ok_name {
        return Err(bad("bad_result", "the engine named a file it may not hand back"));
    }
    let p = dir.join(rel);
    let meta = std::fs::symlink_metadata(&p).map_err(|_| bad("no_picture", "the engine did not make a picture"))?;
    if !meta.is_file() || meta.len() > MAX_RESULT_BYTES {
        return Err(bad("bad_result", "the picture is not a plain file, or is too big"));
    }
    let (real, root) = (std::fs::canonicalize(&p).map_err(|e| e.to_string())?, std::fs::canonicalize(dir).map_err(|e| e.to_string())?);
    if !real.starts_with(&root) {
        return Err(bad("bad_result", "the picture is outside the job folder"));
    }
    std::fs::read(&real).map_err(|e| bad("no_picture", &e.to_string()))
}

// ------------------------------------------------------------------------------------------------ progress lines and running

#[derive(Debug, PartialEq, Clone)]
pub enum Line {
    Progress(f32),
    Result(String),
    Error(String),
    Text(String),
}

/// One line of the script's output: `CCPROGRESS 0.42` (clamped to 0…1), `CCRESULT out/x.png`, `CCERROR code`, anything else is log text.
pub fn parse_line(l: &str) -> Line {
    let l = l.trim_end_matches(['\r', '\n']);
    if let Some(v) = l.strip_prefix("CCPROGRESS ") {
        if let Ok(f) = v.trim().parse::<f32>() {
            if f.is_finite() {
                return Line::Progress(f.clamp(0.0, 1.0));
            }
        }
    } else if let Some(v) = l.strip_prefix("CCRESULT ") {
        return Line::Result(v.trim().to_string());
    } else if let Some(v) = l.strip_prefix("CCERROR ") {
        let code: String = v.trim().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(40).collect();
        return Line::Error(if code.is_empty() { "engine_failed".into() } else { code });
    }
    if let Some(f) = sample_progress(l) {
        return Line::Progress(f);
    }
    Line::Text(l.chars().take(400).collect())
}

/// What Blender itself prints while it draws a still: Cycles "... | Sample 12/64", Eevee "... | Rendering 12 / 64 samples". → 0…1 of the
/// samples (the script's own `CCPROGRESS` lines are for the steps around the render and for animation frames).
pub fn sample_progress(l: &str) -> Option<f32> {
    let take_pair = |rest: &str| -> Option<f32> {
        let mut it = rest.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty());
        let (a, b): (f32, f32) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?);
        (b > 0.0 && a <= b).then(|| a / b)
    };
    if let Some(i) = l.find("| Sample ") {
        return take_pair(&l[i + "| Sample ".len()..].chars().take(20).collect::<String>());
    }
    if let Some(i) = l.find("| Rendering ") {
        let rest: String = l[i + "| Rendering ".len()..].chars().take(24).collect();
        if rest.contains("samples") {
            return take_pair(&rest);
        }
    }
    None
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Running,
    Done,
    Failed,
    Cancelled,
}

/// What a job is doing now; the page polls it.
#[derive(Clone, Debug)]
pub struct Status {
    pub state: State,
    pub progress: f32,
    pub error: String,
    pub result: Option<String>,
    pub log: Vec<String>,
}

impl Status {
    fn new() -> Status {
        Status { state: State::Running, progress: 0.0, error: String::new(), result: None, log: Vec::new() }
    }
}

/// Stop a process and everything it started: a process group (Linux, macOS), `taskkill /T` (Windows).
pub fn kill_tree(pid: u32) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).creation_flags(0x0800_0000).status();
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("kill").args(["-KILL", "--", &format!("-{pid}")]).status();
        let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
    }
}

fn spawn(program: &Path, args: &[String], cwd: &Path) -> std::io::Result<Child> {
    let mut c = Command::new(program);
    c.args(args).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    c.spawn()
}

/// Run `program args` in `cwd`, feeding every output line (stdout and stderr) to `on_line`. `cancel` kills the tree. → the exit code
/// (`None` = killed). Returns after the process and both readers are done.
pub fn run_process(program: &Path, args: &[String], cwd: &Path, cancel: &AtomicBool, seconds: u64, on_line: &(dyn Fn(Line) + Sync)) -> Result<Option<i32>, String> {
    let mut child = spawn(program, args, cwd).map_err(|e| bad("cannot_start", &format!("the program could not start: {e}")))?;
    let pid = child.id();
    let out = child.stdout.take();
    let err = child.stderr.take();
    let started = std::time::Instant::now();
    let code = std::thread::scope(|s| {
        for stream in [out.map(|o| Box::new(o) as Box<dyn std::io::Read + Send>), err.map(|e| Box::new(e) as Box<dyn std::io::Read + Send>)].into_iter().flatten() {
            s.spawn(move || {
                for l in BufReader::new(stream).split(b'\n').map_while(Result::ok) {
                    on_line(parse_line(&String::from_utf8_lossy(&l)));
                }
            });
        }
        loop {
            match child.try_wait() {
                Ok(Some(st)) => break st.code(),
                Ok(None) => {}
                Err(_) => break None,
            }
            if cancel.load(Ordering::SeqCst) || started.elapsed().as_secs() > seconds {
                kill_tree(pid);
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
    });
    Ok(code)
}

/// The jobs in flight. A job's folder is removed when its result is taken or it is cancelled.
#[derive(Default)]
pub struct Jobs {
    map: Mutex<HashMap<u64, Arc<Job>>>,
    next: Mutex<u64>,
}

pub struct Job {
    pub dir: PathBuf,
    pub status: Mutex<Status>,
    pub cancel: AtomicBool,
}

impl Jobs {
    pub fn add(&self, dir: PathBuf) -> (u64, Arc<Job>) {
        let mut n = self.next.lock().unwrap();
        *n += 1;
        let job = Arc::new(Job { dir, status: Mutex::new(Status::new()), cancel: AtomicBool::new(false) });
        self.map.lock().unwrap().insert(*n, job.clone());
        (*n, job)
    }
    pub fn get(&self, id: u64) -> Option<Arc<Job>> {
        self.map.lock().unwrap().get(&id).cloned()
    }
    pub fn remove(&self, id: u64) -> Option<Arc<Job>> {
        self.map.lock().unwrap().remove(&id)
    }
    pub fn running(&self) -> usize {
        self.map.lock().unwrap().values().filter(|j| j.status.lock().unwrap().state == State::Running).count()
    }
}

/// Run one prepared job to its end, updating `job.status`. The caller has written the package and the job file into `job.dir`.
// ------------------------------------------------------------------------------------------------ slicers (roadmap PS.14)

/// The fields a print job has. `parse` refuses unknown keys and anything out of range (the same limits as lib/printstage's cleanJob), so nothing from the page reaches a command line.
#[derive(Clone, Debug, PartialEq)]
pub struct SlicerSettings {
    pub bed: [f64; 3],
    pub nozzle: f64,
    pub filament: f64,
    pub nozzle_temp: u32,
    pub bed_temp: u32,
    pub fan: u32,
    pub retract: f64,
    pub layer: f64,
    pub walls: u32,
    pub top: u32,
    pub bottom: u32,
    pub infill: u32,
    pub speed: f64,
    pub supports: bool,
    pub skirt: bool,
    pub material: String,
    pub start: String,
    pub end: String,
    /// "" = the bed and G-code above; "bambu-p2s" = OrcaSlicer's own P2S presets from the person's install, a sliced .gcode.3mf for Bambu Connect.
    pub target: String,
}

impl SlicerSettings {
    pub fn parse(pairs: &[(&str, &str)]) -> Result<SlicerSettings, String> {
        let mut s = SlicerSettings { bed: [220.0, 220.0, 250.0], nozzle: 0.4, filament: 1.75, nozzle_temp: 205, bed_temp: 60, fan: 100, retract: 0.8, layer: 0.2, walls: 2, top: 4, bottom: 3, infill: 15, speed: 50.0, supports: false, skirt: true, material: "pla".into(), start: String::new(), end: String::new(), target: String::new() };
        let num = |k: &str, v: &str, lo: f64, hi: f64| -> Result<f64, String> {
            let n: f64 = v.parse().map_err(|_| bad("invalid", &format!("{k} is not a number")))?;
            if !n.is_finite() || n < lo || n > hi { Err(bad("invalid", &format!("{k} must be {lo} to {hi}"))) } else { Ok(n) }
        };
        let whole = |k: &str, v: &str, lo: f64, hi: f64| -> Result<u32, String> { num(k, v, lo, hi).map(|n| n.round() as u32) };
        let yes = |k: &str, v: &str| -> Result<bool, String> { match v { "1" | "true" => Ok(true), "0" | "false" => Ok(false), _ => Err(bad("invalid", &format!("{k} is not yes or no"))) } };
        let text = |k: &str, v: &str| -> Result<String, String> {
            if v.len() > 8000 || v.chars().any(|c| c != '\n' && !(' '..='~').contains(&c)) { Err(bad("invalid", &format!("{k} must be plain G-code text"))) } else { Ok(v.to_string()) }
        };
        for (k, v) in pairs {
            match *k {
                "bed" => {
                    let p: Vec<&str> = v.trim_matches(|c| c == '[' || c == ']').split(',').collect();
                    if p.len() != 3 { return Err(bad("invalid", "bed must be x,y,z")); }
                    for i in 0..3 { s.bed[i] = num("bed", p[i].trim(), 50.0, 2000.0)?; }
                }
                "nozzle" => s.nozzle = num(k, v, 0.1, 1.2)?,
                "filament" => s.filament = num(k, v, 1.0, 3.0)?,
                "nozzle_temp" => s.nozzle_temp = whole(k, v, 150.0, 300.0)?,
                "bed_temp" => s.bed_temp = whole(k, v, 0.0, 120.0)?,
                "fan" => s.fan = whole(k, v, 0.0, 100.0)?,
                "retract" => s.retract = num(k, v, 0.0, 8.0)?,
                "layer" => s.layer = num(k, v, 0.05, 0.8)?,
                "walls" => s.walls = whole(k, v, 1.0, 8.0)?,
                "top" => s.top = whole(k, v, 0.0, 20.0)?,
                "bottom" => s.bottom = whole(k, v, 0.0, 20.0)?,
                "infill" => s.infill = whole(k, v, 0.0, 100.0)?,
                "speed" => s.speed = num(k, v, 5.0, 300.0)?,
                "supports" => s.supports = yes(k, v)?,
                "skirt" => s.skirt = yes(k, v)?,
                "material" => s.material = match *v { "pla" | "petg" | "abs" | "tpu" => v.to_string(), _ => return Err(bad("invalid", "unknown material")) },
                "start" => s.start = text(k, v)?,
                "end" => s.end = text(k, v)?,
                "target" => s.target = match *v { "" | "bambu-p2s" => v.to_string(), _ => return Err(bad("invalid", "unknown printer")) },
                _ => return Err(bad("invalid", &format!("unknown setting {k}"))),
            }
        }
        Ok(s)
    }
}

fn gcode_for_ini(text: &str) -> String { text.replace('\\', "").replace('\n', "\\n") }
fn json_text(text: &str) -> String { let mut o = String::from("\""); for c in text.chars() { match c { '"' => o.push_str("\\\""), '\\' => o.push_str("\\\\"), '\n' => o.push_str("\\n"), c => o.push(c) } } o.push('"'); o }

/// (the files to write in the job's folder, the command line, where the G-code lands). `stl` is the plate, written by the caller beside these.
pub fn slicer_job(id: &str, s: &SlicerSettings, stl: &str) -> Result<(Vec<(String, String)>, Vec<String>, String), String> {
    let first = s.layer.max((s.nozzle * 0.75).min(0.3));
    let shape = format!("0x0,{x}x0,{x}x{y},0x{y}", x = s.bed[0], y = s.bed[1]);
    let args = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    match id {
        "prusaslicer" => {
            let ini = [
                format!("bed_shape = {shape}"), format!("max_print_height = {}", s.bed[2]), format!("layer_height = {}", s.layer), format!("first_layer_height = {first}"),
                format!("nozzle_diameter = {}", s.nozzle), format!("filament_diameter = {}", s.filament), format!("temperature = {}", s.nozzle_temp), format!("first_layer_temperature = {}", s.nozzle_temp),
                format!("bed_temperature = {}", s.bed_temp), format!("first_layer_bed_temperature = {}", s.bed_temp), format!("perimeters = {}", s.walls), format!("top_solid_layers = {}", s.top),
                format!("bottom_solid_layers = {}", s.bottom), format!("fill_density = {}%", s.infill), format!("support_material = {}", s.supports as u8), format!("skirts = {}", s.skirt as u8),
                format!("max_fan_speed = {}", s.fan), format!("min_fan_speed = {}", s.fan), format!("retract_length = {}", s.retract), format!("perimeter_speed = {}", s.speed),
                format!("infill_speed = {}", s.speed), "use_relative_e_distances = 1".into(), "layer_gcode = G92 E0".into(), format!("start_gcode = {}", gcode_for_ini(&s.start)), format!("end_gcode = {}", gcode_for_ini(&s.end)),
            ].join("\n") + "\n";
            Ok((vec![("cc-print.ini".into(), ini)], args(&["--export-gcode", "--dont-arrange", "--load", "cc-print.ini", "--output", "out/plate.gcode", stl]), "out/plate.gcode".into()))
        }
        "orcaslicer" => {
            // "from": "system": OrcaSlicer's compatibility check compares a preset's compatible_printers with the printer's system name, and a "User" preset without `inherits` has none (src/OrcaSlicer.cpp, CLI_PROCESS_NOT_COMPATIBLE).
            let area = format!("[\"0x0\",\"{x}x0\",\"{x}x{y}\",\"0x{y}\"]", x = s.bed[0], y = s.bed[1]);
            let machine = format!("{{\"type\":\"machine\",\"from\":\"system\",\"name\":\"cc-machine\",\"printer_settings_id\":\"cc-machine\",\"gcode_flavor\":\"marlin\",\"nozzle_diameter\":[\"{}\"],\"printable_area\":{area},\"printable_height\":\"{}\",\"layer_change_gcode\":\"G92 E0\",\"use_relative_e_distances\":\"1\",\"machine_start_gcode\":{},\"machine_end_gcode\":{},\"retraction_length\":[\"{}\"],\"extruder_colour\":[\"#808080\"],\"default_print_profile\":\"cc-process\",\"default_filament_profile\":[\"cc-filament\"]}}", s.nozzle, s.bed[2], json_text(&s.start), json_text(&s.end), s.retract);
            let process = format!("{{\"type\":\"process\",\"from\":\"system\",\"name\":\"cc-process\",\"print_settings_id\":\"cc-process\",\"layer_height\":\"{}\",\"initial_layer_print_height\":\"{first}\",\"wall_loops\":\"{}\",\"top_shell_layers\":\"{}\",\"bottom_shell_layers\":\"{}\",\"sparse_infill_density\":\"{}%\",\"enable_support\":\"{}\",\"skirt_loops\":\"{}\",\"outer_wall_speed\":\"{}\",\"inner_wall_speed\":\"{}\",\"sparse_infill_speed\":\"{}\",\"compatible_printers\":[\"cc-machine\"]}}", s.layer, s.walls, s.top, s.bottom, s.infill, s.supports as u8, s.skirt as u8, s.speed * 0.6, s.speed, s.speed);
            let kind = s.material.to_uppercase();
            let filament = format!("{{\"type\":\"filament\",\"from\":\"system\",\"name\":\"cc-filament\",\"filament_settings_id\":[\"cc-filament\"],\"filament_type\":[\"{kind}\"],\"filament_diameter\":[\"{}\"],\"nozzle_temperature\":[\"{t}\"],\"nozzle_temperature_initial_layer\":[\"{t}\"],\"hot_plate_temp\":[\"{b}\"],\"hot_plate_temp_initial_layer\":[\"{b}\"],\"fan_max_speed\":[\"{f}\"],\"fan_min_speed\":[\"{f}\"],\"compatible_printers\":[\"cc-machine\"]}}", s.filament, t = s.nozzle_temp, b = s.bed_temp, f = s.fan);
            Ok((vec![("cc-machine.json".into(), machine), ("cc-process.json".into(), process), ("cc-filament.json".into(), filament)],
                args(&["--load-settings", "cc-machine.json;cc-process.json", "--load-filaments", "cc-filament.json", "--slice", "0", "--arrange", "0", "--orient", "0", "--outputdir", "out", stl]), "out/plate_1.gcode".into()))
        }
        _ => Err(bad("unknown_engine", "not a slicer")),
    }
}

/// A JSON object's top-level (key, raw value text), std only. Enough for preset files; anything else is an error.
fn json_top(text: &str) -> Result<Vec<(String, String)>, String> {
    let b = text.as_bytes();
    let wrong = || bad("invalid", "a preset file could not be read");
    let mut i = 0;
    let skip = |i: &mut usize| while *i < b.len() && (b[*i] as char).is_ascii_whitespace() { *i += 1 };
    fn string_end(b: &[u8], mut i: usize) -> Option<usize> {
        i += 1;
        while i < b.len() {
            match b[i] { b'\\' => i += 2, b'"' => return Some(i + 1), _ => i += 1 }
        }
        None
    }
    skip(&mut i);
    if b.get(i) != Some(&b'{') { return Err(wrong()); }
    i += 1;
    let mut out = Vec::new();
    loop {
        skip(&mut i);
        match b.get(i) { Some(b'}') => return Ok(out), Some(b',') => { i += 1; continue; } Some(b'"') => {} _ => return Err(wrong()) }
        let e = string_end(b, i).ok_or_else(wrong)?;
        let key = text[i + 1..e - 1].to_string();
        i = e;
        skip(&mut i);
        if b.get(i) != Some(&b':') { return Err(wrong()); }
        i += 1;
        skip(&mut i);
        let start = i;
        let mut depth = 0i32;
        loop {
            match b.get(i).ok_or_else(wrong)? {
                b'"' => { i = string_end(b, i).ok_or_else(wrong)?; continue; }
                b'[' | b'{' => depth += 1,
                b']' | b'}' if depth > 0 => depth -= 1,
                b',' | b'}' if depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        out.push((key, text[start..i].trim().to_string()));
    }
}

/// A preset with everything it inherits written out: the CLI wants whole files. Names come from the program's own files; only plain names are followed.
fn flat_preset(dir: &Path, name: &str) -> Result<Vec<(String, String)>, String> {
    let mut merged: Vec<(String, String)> = Vec::new();
    let mut at = name.to_string();
    for _ in 0..12 {
        if at.contains(['/', '\\']) || at.contains("..") { return Err(bad("invalid", "a preset name is not plain")); }
        let text = std::fs::read_to_string(dir.join(format!("{at}.json"))).map_err(|_| bad("no_profile", &format!("this OrcaSlicer has no preset {at}")))?;
        let own = json_top(&text)?;
        let parent = own.iter().find(|(k, _)| k == "inherits").map(|(_, v)| v.trim_matches('"').to_string()).unwrap_or_default();
        for (k, v) in own {
            if !merged.iter().any(|(x, _)| *x == k) { merged.push((k, v)); }
        }
        if parent.is_empty() {
            merged.retain(|(k, _)| k != "inherits");
            return Ok(merged);
        }
        at = parent;
    }
    Err(bad("invalid", "a preset inherits too deeply"))
}

fn preset_text(mut p: Vec<(String, String)>, overrides: &[(&str, String)]) -> String {
    for (k, v) in overrides {
        match p.iter_mut().find(|(x, _)| x == k) { Some(e) => e.1 = v.clone(), None => p.push((k.to_string(), v.clone())) }
    }
    let body: Vec<String> = p.iter().map(|(k, v)| format!("{}:{v}", json_text(k))).collect();
    format!("{{{}}}", body.join(","))
}

/// Where OrcaSlicer keeps its presets, beside its program: Windows, then macOS, then a Linux package.
pub fn orca_profiles(program: &Path) -> Result<PathBuf, String> {
    let dir = program.parent().unwrap_or(Path::new("."));
    let tries = [dir.join("resources/profiles/BBL"), dir.join("../Resources/profiles/BBL"), dir.join("../share/OrcaSlicer/profiles/BBL"), PathBuf::from("/usr/share/OrcaSlicer/profiles/BBL")];
    tries.into_iter().find(|p| p.is_dir()).ok_or_else(|| bad("no_profile", "this OrcaSlicer has no Bambu Lab presets"))
}

/// The P2S route: the maker's presets as OrcaSlicer ships them, with the person's choices of walls, skins, infill and supports over them; temperatures stay the filament preset's.
/// Nothing of these presets is in anything we ship: they are read from the person's install when they print.
pub fn bambu_job(s: &SlicerSettings, stl: &str, profiles: &Path) -> Result<(Vec<(String, String)>, Vec<String>, String), String> {
    let process = if s.layer <= 0.14 { "0.12mm High Quality @BBL P2S" } else if s.layer <= 0.22 { "0.20mm Standard @BBL P2S" } else { "0.24mm Standard @BBL P2S" };
    let filament = format!("Generic {} @BBL P2S", s.material.to_uppercase());
    let q = |v: String| format!("\"{v}\"");
    let machine = preset_text(flat_preset(&profiles.join("machine"), "Bambu Lab P2S 0.4 nozzle")?, &[]);
    let process = preset_text(flat_preset(&profiles.join("process"), process)?, &[
        ("wall_loops", q(s.walls.to_string())), ("top_shell_layers", q(s.top.to_string())), ("bottom_shell_layers", q(s.bottom.to_string())),
        ("sparse_infill_density", q(format!("{}%", s.infill))), ("enable_support", q((s.supports as u8).to_string())), ("skirt_loops", q((s.skirt as u8).to_string())),
    ]);
    let filament = preset_text(flat_preset(&profiles.join("filament"), &filament)?, &[]);
    Ok((vec![("cc-machine.json".into(), machine), ("cc-process.json".into(), process), ("cc-filament.json".into(), filament)],
        ["--load-settings", "cc-machine.json;cc-process.json", "--load-filaments", "cc-filament.json", "--slice", "0", "--arrange", "0", "--orient", "0", "--outputdir", "out", "--export-3mf", "plate.gcode.3mf", stl].iter().map(|x| x.to_string()).collect(),
        "out/plate.gcode.3mf".into()))
}

/// The import link Bambu's wiki gives: both values percent-encoded; version is the protocol's.
pub fn bambu_connect_url(path: &str, name: &str) -> String {
    let enc = |t: &str| t.bytes().map(|c| if c.is_ascii_alphanumeric() || b"-_.~".contains(&c) { (c as char).to_string() } else { format!("%{c:02X}") }).collect::<String>();
    format!("bambu-connect://import-file?path={}&name={}&version=1.0.0", enc(path), enc(name))
}

/// `drive` for a slicer: it prints no CCRESULT line, so a clean exit with the G-code in place is the result. Any other exit is a failure even with a file there: a crash can leave half a
/// program, and half a program must never reach a printer. The last lines it printed stay in the job's log for the page to show.
pub fn drive_slicer(program: &Path, args: &[String], job: &Job, seconds: u64, result: &str) {
    let note = |l: Line| {
        let mut s = job.status.lock().unwrap();
        match l {
            Line::Progress(f) => s.progress = f,
            Line::Text(t) => {
                s.log.push(t);
                if s.log.len() > 40 {
                    s.log.remove(0);
                }
            }
            _ => {}
        }
    };
    let r = run_process(program, args, &job.dir, &job.cancel, seconds, &note);
    let mut s = job.status.lock().unwrap();
    s.state = match r {
        Err(e) => {
            s.error = e.split(':').next().unwrap_or("job_failed").to_string();
            State::Failed
        }
        Ok(_) if job.cancel.load(Ordering::SeqCst) => State::Cancelled,
        Ok(None) => {
            s.error = "timed_out".into();
            State::Failed
        }
        Ok(Some(0)) if job.dir.join(result).is_file() => {
            s.result = Some(result.to_string());
            s.progress = 1.0;
            State::Done
        }
        Ok(_) => {
            s.error = "engine_failed".into();
            State::Failed
        }
    };
}

pub fn drive(program: &Path, args: &[String], job: &Job, seconds: u64) {
    let note = |l: Line| {
        let mut s = job.status.lock().unwrap();
        match l {
            Line::Progress(f) => s.progress = f,
            Line::Result(r) => s.result = Some(r),
            Line::Error(c) => s.error = c,
            Line::Text(t) => {
                s.log.push(t);
                if s.log.len() > 40 {
                    s.log.remove(0);
                }
            }
        }
    };
    let r = run_process(program, args, &job.dir, &job.cancel, seconds, &note);
    let mut s = job.status.lock().unwrap();
    s.state = match r {
        Err(e) => {
            s.error = e.split(':').next().unwrap_or("job_failed").to_string();
            State::Failed
        }
        Ok(_) if job.cancel.load(Ordering::SeqCst) => State::Cancelled,
        Ok(None) => {
            if s.error.is_empty() {
                s.error = "timed_out".into();
            }
            State::Failed
        }
        Ok(Some(0)) if s.error.is_empty() && s.result.is_some() => {
            s.progress = 1.0;
            State::Done
        }
        Ok(Some(c)) => {
            if s.error.is_empty() {
                s.error = if c == 3 { "script_failed".into() } else { "engine_failed".into() };
            }
            State::Failed
        }
    };
}

// ------------------------------------------------------------------------------------------------ Open in … (RE.6)

/// A program that can open the scene and keep it (nothing comes back): the package is written to a folder the person can find, and
/// the program is started on one file of it. The same rules as the engines: a fixed table, no shell, no arguments from the page.
pub struct OpenDef {
    pub id: &'static str,
    pub name: &'static str,
    /// Lower-case program file names (without `.exe`); `system` has none: the system's own opener for the file type.
    pub programs: &'static [&'static str],
    /// The file kinds it reads: "usd" (scene.usdc / .usdz / .usda) and "glb" (scene.glb).
    pub reads: &'static [&'static str],
    /// Blender gets the scene built exactly as the Blender engine builds it (a shipped script); the others just open the file.
    pub script: Option<&'static str>,
}

pub const OPEN_IN: &[OpenDef] = &[
    #[cfg(feature = "blender")]
    OpenDef { id: "blender", name: "Blender", programs: &["blender"], reads: &["usd", "glb"], script: Some("cc_blender_open.py") },
    OpenDef { id: "usdview", name: "usdview", programs: &["usdview"], reads: &["usd"], script: None },
    OpenDef { id: "system", name: "The system's own program", programs: &[], reads: &["usd", "glb"], script: None },
];

pub fn open_def(id: &str) -> Option<&'static OpenDef> {
    OPEN_IN.iter().find(|d| d.id == id)
}

/// Which package file opens for a kind: "usd" → the package's scene.usdc / .usdz / .usda, "glb" → scene.glb. Anything else is refused.
pub fn open_file_for(def: &OpenDef, kind: &str, files: &[(String, Vec<u8>)]) -> Result<String, String> {
    if !def.reads.contains(&kind) {
        return Err(bad("invalid", &format!("{} does not read {kind} files", def.name)));
    }
    let wanted: &[&str] = match kind {
        "usd" => &["scene.usdc", "scene.usdz", "scene.usda"],
        "glb" => &["scene.glb"],
        _ => return Err(bad("invalid", "unknown file kind")),
    };
    wanted.iter().find(|w| files.iter().any(|(n, _)| n == *w)).map(|w| w.to_string()).ok_or_else(|| bad("no_file", &format!("the package has no {kind} file")))
}

/// A folder name from a file's name: letters, digits, space, dot, dash and underscore; the rest dropped; never empty, never starting with a dot.
pub fn safe_folder_name(name: &str) -> String {
    let s: String = name.chars().filter(|c| c.is_alphanumeric() || " ._-".contains(*c)).take(60).collect();
    let s = s.trim().trim_start_matches('.').trim().to_string();
    if s.is_empty() { "scene".into() } else { s }
}

/// `<base>/<name>` (a new folder: `-2`, `-3` … if it is taken, nothing is overwritten) for the package's files. → the folder.
pub fn hand_off_folder(base: &Path, name: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(base).map_err(|e| bad("job_failed", &e.to_string()))?;
    let want = safe_folder_name(name);
    for k in 1..1000 {
        let d = base.join(if k == 1 { want.clone() } else { format!("{want}-{k}") });
        if std::fs::create_dir(&d).is_ok() {
            return Ok(d);
        }
    }
    Err(bad("job_failed", "no free folder name"))
}

/// A program on the search path: `dirs` are the entries of PATH (injected); `exists` says whether a file is there.
pub fn on_path(names: &[&str], os: Os, dirs: &[PathBuf], exists: &dyn Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in dirs {
        for n in names {
            let f = if os == Os::Windows { format!("{n}.exe") } else { n.to_string() };
            let p = d.join(f);
            if exists(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// The system's own opener for a file: `explorer` (Windows), `open` (macOS), `xdg-open` (Linux). → (program, arguments).
pub fn system_open(os: Os, file: &Path) -> (String, Vec<String>) {
    let f = file.to_string_lossy().into_owned();
    match os {
        Os::Windows => ("explorer.exe".into(), vec![f]),
        Os::Mac => ("open".into(), vec![f]),
        Os::Linux => ("xdg-open".into(), vec![f]),
    }
}

/// Blender, opened on the scene (not in the background: the person's own Blender window, their own preferences): our script, the job file.
pub fn blender_open_args(script: &Path, job_file: &Path) -> Vec<String> {
    vec!["--python".into(), script.to_string_lossy().into_owned(), "--".into(), job_file.to_string_lossy().into_owned()]
}

/// Start a program and do not wait for it (the person keeps working in it); its output is not kept.
pub fn launch_detached(program: &Path, args: &[String], cwd: &Path) -> Result<(), String> {
    let mut c = Command::new(program);
    c.args(args).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000 | 0x0000_0008); // no console window, detached
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    c.spawn().map(|_| ()).map_err(|e| bad("cannot_start", &format!("the program could not start: {e}")))
}

// ------------------------------------------------------------------------------------------------ Unreal link (RE.5)

/// The remembered Unreal project folder is stored in engines.conf under this key (the program's own pick is under "unreal").
pub const UNREAL_PROJECT_KEY: &str = "unreal-project";

/// A folder is an Unreal project when it holds a `<name>.uproject` file. → the project's name.
pub fn uproject_in(dir: &Path) -> Result<String, String> {
    let rd = std::fs::read_dir(dir).map_err(|_| bad("not_found", "that folder is not there"))?;
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "uproject").unwrap_or(false) && e.path().is_file())
        .filter_map(|e| e.path().file_stem().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names.into_iter().next().ok_or_else(|| bad("not_a_project", "that folder has no Unreal project file (.uproject)"))
}

/// The two small layers around CadCraft's scene (docs/spikes/RE.5.md): a root stage whose sublayers are the person's own layer
/// (stronger) and CadCraft's `scene.<ext>`, and the person's empty layer. CadCraft writes the root and the user layer once and never
/// again; only the scene file and the manifest are overwritten, so the person's edits cannot be lost to a reload.
pub fn unreal_layers(name: &str, scene_file: &str) -> (String, String) {
    let n = safe_folder_name(name);
    (
        format!("#usda 1.0\n(\n    defaultPrim = \"World\"\n    upAxis = \"Z\"\n    metersPerUnit = 1\n    subLayers = [\n        @./{n}.user.usda@,\n        @./{scene_file}@\n    ]\n)\n"),
        "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n".to_string(),
    )
}

/// Send the package into the project: `<project>/CadCraft/<name>/` gets the package's files (the scene, the manifest, IES, textures,
/// overwritten each time) and, only when they are not there, `<name>.root.usda` and `<name>.user.usda`. → (folder, whether the stage is new).
pub fn unreal_send(project: &Path, name: &str, files: &[(String, Vec<u8>)]) -> Result<(PathBuf, bool), String> {
    uproject_in(project)?;
    if !files.iter().any(|(n, _)| n == "cadcraft-render.json") {
        return Err(bad("no_manifest", "the package has no manifest"));
    }
    let scene = ["scene.usdc", "scene.usdz", "scene.usda"].iter().find(|w| files.iter().any(|(n, _)| n == *w)).ok_or_else(|| bad("no_file", "the package has no USD file"))?;
    let n = safe_folder_name(name);
    let dir = project.join("CadCraft").join(&n);
    std::fs::create_dir_all(&dir).map_err(|e| bad("job_failed", &e.to_string()))?;
    for (fname, data) in files {
        let p = dir.join(fname);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(|e| bad("job_failed", &e.to_string()))?;
        }
        // the new file goes beside the old one and is moved over it, so Unreal never reads a half-written scene
        let tmp = dir.join(format!(".{}.tmp", fname.replace('/', "_")));
        std::fs::write(&tmp, data).map_err(|e| bad("job_failed", &e.to_string()))?;
        std::fs::rename(&tmp, &p).map_err(|e| bad("job_failed", &e.to_string()))?;
    }
    let (root, user) = unreal_layers(&n, scene);
    let (root_p, user_p) = (dir.join(format!("{n}.root.usda")), dir.join(format!("{n}.user.usda")));
    let fresh = !root_p.exists();
    if fresh {
        std::fs::write(&root_p, root).map_err(|e| bad("job_failed", &e.to_string()))?;
    }
    if !user_p.exists() {
        std::fs::write(&user_p, user).map_err(|e| bad("job_failed", &e.to_string()))?;
    }
    Ok((dir, fresh))
}

// ------------------------------------------------------------------------------------------------ Engines from add-ons (RE.7)

/// An add-on's engine runs a program with a script that sits **inside the signed add-on** (docs/ADDONS-SPEC.md § 16). The page sends the
/// package with the script added as `engine/<file>`; what runs and with which arguments is declared once and checked here.
#[derive(Clone, Debug, PartialEq)]
pub struct AddonRun {
    pub addon: String,
    /// File names (without `.exe`) the program may have; found on the search path, or picked once by the person.
    pub programs: Vec<String>,
    /// `engine/<name>` inside the package.
    pub script: String,
    /// Argument templates: a placeholder from `ADDON_PLACEHOLDERS`, or a plain literal (`--quiet`, `4`).
    pub args: Vec<String>,
}

pub const ADDON_PLACEHOLDERS: &[&str] = &["{script}", "{job}", "{manifest}", "{out}", "{width}", "{height}", "{samples}"];

/// Programs an add-on can never name: shells and script hosts (an add-on that needs Python or Node says so; it does not get a shell).
pub const DENIED_PROGRAMS: &[&str] = &["blender", "sh", "bash", "zsh", "fish", "dash", "csh", "tcsh", "ksh", "cmd", "powershell", "pwsh", "wscript", "cscript", "mshta", "rundll32", "regsvr32", "msiexec", "bitsadmin", "certutil", "curl", "wget", "ssh", "scp", "nc", "ncat", "sudo", "su", "doas", "env", "xargs", "osascript", "open", "explorer", "start"];

/// An add-on id as ADDONS-SPEC § 2 says: `^[a-z][a-z0-9.-]{1,63}$`, no `..`, no `--`, no trailing `.` or `-`.
pub fn addon_id_ok(id: &str) -> bool {
    let b = id.as_bytes();
    (2..=64).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
        && !id.contains("..")
        && !id.contains("--")
        && !id.ends_with('.')
        && !id.ends_with('-')
}

fn plain_program(n: &str) -> bool {
    !n.is_empty() && n.len() <= 40 && n.chars().next().map(|c| c.is_ascii_alphanumeric()).unwrap_or(false) && n.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) && !n.to_lowercase().ends_with(".exe")
}

/// Flags that make a program run code from its command line (or a module by name) instead of the add-on's script file.
const CODE_FLAGS: &[&str] = &["-c", "-e", "-m", "-x", "-i", "--eval", "--exec", "--command", "-command", "-encodedcommand", "--print", "-p"];

fn plain_literal(a: &str) -> bool {
    if CODE_FLAGS.contains(&a.split('=').next().unwrap_or("").to_lowercase().as_str()) {
        return false;
    }
    let t = a.trim_start_matches('-');
    a.len() <= 42 && !t.is_empty() && a.len() - t.len() <= 2 && t.chars().next().map(|c| c.is_ascii_alphanumeric()).unwrap_or(false) && t.chars().all(|c| c.is_ascii_alphanumeric() || "_.=-".contains(c))
}

pub fn validate_addon_run(r: &AddonRun) -> Result<(), String> {
    if !addon_id_ok(&r.addon) {
        return Err(bad("invalid", "the add-on's id is not an id"));
    }
    if r.programs.is_empty() || r.programs.len() > 4 || !r.programs.iter().all(|p| plain_program(p)) {
        return Err(bad("invalid", "the program names are not plain names"));
    }
    if let Some(p) = r.programs.iter().find(|p| DENIED_PROGRAMS.contains(&p.to_lowercase().as_str())) {
        return Err(bad("denied_program", &format!("{p} is a program an add-on may not run")));
    }
    let script_ok = safe_rel(&r.script) && r.script.starts_with("engine/") && r.script.len() <= 120 && r.script.matches('/').count() <= 3;
    if !script_ok {
        return Err(bad("invalid", "the script must be a file under engine/ in the add-on"));
    }
    if r.args.len() > 16 {
        return Err(bad("invalid", "too many arguments"));
    }
    for a in &r.args {
        if !(ADDON_PLACEHOLDERS.contains(&a.as_str()) || plain_literal(a)) {
            return Err(bad("invalid", &format!("the argument {a:?} is not a placeholder or a plain word")));
        }
    }
    if !r.args.iter().any(|a| a == "{script}") {
        return Err(bad("invalid", "the arguments must pass {script}"));
    }
    Ok(())
}

/// The command line: placeholders filled from values Rust made (paths inside the job folder, numbers), literals as they are. The first
/// value is the program, not an argument.
pub fn addon_args(r: &AddonRun, dir: &Path, manifest: &str, s: &BlenderSettings) -> Vec<String> {
    r.args
        .iter()
        .map(|a| match a.as_str() {
            "{script}" => dir.join(&r.script).to_string_lossy().into_owned(),
            "{job}" => dir.join("cc-engine-job.json").to_string_lossy().into_owned(),
            "{manifest}" => dir.join(manifest).to_string_lossy().into_owned(),
            "{out}" => dir.join("out").to_string_lossy().into_owned(),
            "{width}" => s.width.to_string(),
            "{height}" => s.height.to_string(),
            "{samples}" => s.samples.to_string(),
            lit => lit.to_string(),
        })
        .collect()
}

/// The consent the person gave once, remembered as `allow:<addon>:<program>` in engines.conf. A new add-on, or the same one naming a
/// new program, asks again.
pub fn consent_key(addon: &str, program: &str) -> String {
    format!("allow:{addon}:{program}")
}

/// engines.conf keys beyond the engines' own: the Unreal project, a program picked for an add-on, consents.
pub fn conf_key_ok(k: &str) -> bool {
    engine(k).is_some()
        || k == UNREAL_PROJECT_KEY
        || k.strip_prefix("allow:").or_else(|| k.strip_prefix("program:")).and_then(|r| r.split_once(':')).map(|(a, p)| addon_id_ok(a) && plain_program(p)).unwrap_or(false)
}

// ------------------------------------------------------------------------------------------------ Tauri commands

#[cfg(not(standalone))]
mod commands {
    //! Paste into the shell as `src/engines.rs`'s second half (see integration.txt). Tablets: `unsupported`.
    use super::*;
    use serde::Serialize;
    use tauri::ipc::Request;
    use tauri::{AppHandle, Manager, State};

    #[derive(Serialize)]
    pub struct Found {
        pub id: &'static str,
        pub name: &'static str,
        pub kind: &'static str,
        pub found: bool,
        pub version: String,
        pub path: String,
        pub why: String,
    }

    fn conf_path(app: &AppHandle) -> Result<PathBuf, String> {
        // portable copies keep it in data/ beside the program, others in the app-data folder (portable.rs; CE.10's rule: nothing outside data/)
        crate::portable::data_file(app, "engines.conf").ok_or_else(|| "no folder for engines.conf".to_string())
    }
    fn read_conf(app: &AppHandle) -> HashMap<String, PathBuf> {
        conf_path(app).ok().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| parse_conf(&t)).unwrap_or_default()
    }

    fn version_of(def: &EngineDef, program: &Path) -> Option<(u32, u32)> {
        match def.id {
            "blender" => {
                let o = Command::new(program).arg("--version").stdin(Stdio::null()).output().ok()?;
                parse_blender_version(&String::from_utf8_lossy(&o.stdout))
            }
            "unreal" => program.ancestors().find_map(|a| a.file_name().and_then(|n| parse_unreal_folder(&n.to_string_lossy()))),
            "cantera" => {
                let o = Command::new(program).arg("--version").stdin(Stdio::null()).output().ok()?;
                parse_python_version(&String::from_utf8_lossy(&o.stdout))
            }
            _ => None,
        }
    }

    fn find_one(app: &AppHandle, def: &'static EngineDef) -> Found {
        let mut f = Found { id: def.id, name: def.name, kind: match def.kind { Kind::Render => "render", Kind::Process => "process", Kind::Link => "link", Kind::Slicer => "slicer" }, found: false, version: String::new(), path: String::new(), why: "not_found".into() };
        let picked = read_conf(app).get(def.id).cloned();
        let env = |k: &str| std::env::var(k).ok();
        let list = |d: &Path| std::fs::read_dir(d).map(|r| r.filter_map(|e| e.ok().map(|e| e.path())).collect()).unwrap_or_default();
        let mut tries: Vec<PathBuf> = picked.into_iter().collect();
        tries.extend(candidates(def.id, Os::here(), &env, &list));
        for p in tries {
            if let Ok(p) = valid_program(def, &p) {
                let v = version_of(def, &p);
                f.path = p.to_string_lossy().into_owned();
                f.found = true;
                match v {
                    Some((a, b)) => {
                        f.version = format!("{a}.{b}");
                        f.why = if (a, b) >= def.min_version { String::new() } else { "too_old".into() };
                        f.found = (a, b) >= def.min_version;
                    }
                    None => f.why = "unknown_version".into(),
                }
                if f.found || f.why == "too_old" {
                    return f;
                }
            }
        }
        f
    }

    #[tauri::command(async)]
    pub async fn cc_engine_list(app: AppHandle) -> Result<Vec<Found>, String> {
        if cfg!(mobile) {
            return Err(bad("unsupported", "no outside programs on tablets"));
        }
        tauri::async_runtime::spawn_blocking(move || ENGINES.iter().map(|d| find_one(&app, d)).collect()).await.map_err(|e| e.to_string())
    }

    #[tauri::command(async)]
    pub async fn cc_engine_find(app: AppHandle, engine: String) -> Result<Found, String> {
        let def = super::engine(&engine).ok_or_else(|| bad("unknown_engine", "no such engine"))?;
        tauri::async_runtime::spawn_blocking(move || find_one(&app, def)).await.map_err(|e| e.to_string())
    }

    /// The person picks the program once (a file dialog; the file's name must be the engine's own). → the new `Found`, or null when cancelled.
    #[tauri::command(async)]
    pub async fn cc_engine_pick(app: AppHandle, win: tauri::WebviewWindow, engine: String) -> Result<Option<Found>, String> {
        use tauri_plugin_dialog::DialogExt;
        let def = super::engine(&engine).ok_or_else(|| bad("unknown_engine", "no such engine"))?;
        let picked: Option<PathBuf> = match crate::files::test_answer() {
            Some(a) => a.and_then(|v| v.into_iter().next()),
            None => tauri::async_runtime::spawn_blocking(move || win.dialog().file().set_parent(&win).set_title(&format!("Find {}", def.name)).blocking_pick_file().and_then(|f| f.into_path().ok()))
                .await
                .map_err(|e| e.to_string())?,
        };
        let Some(p) = picked else { return Ok(None) };
        let program = valid_program(def, &p)?;
        let mut conf = read_conf(&app);
        conf.insert(def.id.to_string(), program);
        let text = write_conf(&conf)?;
        let cp = conf_path(&app)?;
        if let Some(dir) = cp.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&cp, text).map_err(|e| e.to_string())?;
        Ok(Some(find_one(&app, def)))
    }

    #[tauri::command(async)]
    pub async fn cc_engine_forget(app: AppHandle, engine: String) -> Result<(), String> {
        let def = super::engine(&engine).ok_or_else(|| bad("unknown_engine", "no such engine"))?;
        let mut conf = read_conf(&app);
        conf.remove(def.id);
        let cp = conf_path(&app)?;
        if let Some(d) = cp.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        std::fs::write(&cp, write_conf(&conf)?).map_err(|e| e.to_string())
    }

    /// Body = the package (a stored zip); header `x-cc-engine` = `{engine, settings: {key: "value"}}` as JSON, percent-encoded.
    /// → the job id. The program runs on a thread; the page polls `cc_engine_status`.
    #[tauri::command(async)]
    pub async fn cc_engine_start(app: AppHandle, jobs: State<'_, Jobs>, request: Request<'_>) -> Result<u64, String> {
        let head: serde_json::Value = request
            .headers()
            .get("x-cc-engine")
            .and_then(|v| v.to_str().ok())
            .and_then(crate::files::decode_component)
            .and_then(|s| serde_json::from_str(&s).ok())
            .ok_or_else(|| bad("invalid", "missing job"))?;
        let engine = head.get("engine").and_then(|v| v.as_str()).ok_or_else(|| bad("invalid", "missing engine"))?.to_string();
        let def = super::engine(&engine).ok_or_else(|| bad("unknown_engine", "no such engine"))?;
        if def.kind == Kind::Process {
            // 0.8 CH.7: the body is the case file (JSON text, at most 1 MiB); the script is the one shipped inside Studio; nothing from the page reaches the command line
            let bytes = crate::files::body_bytes(request.body()).ok_or_else(|| bad("invalid", "expected the case file"))?;
            if bytes.len() > MAX_CASE_BYTES || serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
                return Err(bad("invalid", "the case is not JSON of a sensible size"));
            }
            let resources = app.path().resource_dir().map_err(|e| e.to_string())?;
            let script = script_path(&resources, def)?;
            let found = tauri::async_runtime::spawn_blocking({
                let app = app.clone();
                move || find_one(&app, def)
            })
            .await
            .map_err(|e| e.to_string())?;
            if !found.found {
                return Err(bad(&found.why, &format!("{} is not available", def.name)));
            }
            let program = PathBuf::from(&found.path);
            let dir = job_dir(0)?;
            let case_file = dir.join("cc-case.json");
            if let Err(e) = std::fs::write(&case_file, &bytes) {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(bad("job_failed", &e.to_string()));
            }
            let (id, job) = jobs.add(dir.clone());
            let args = python_args(&script, &case_file, &dir.join("out").join("result.json"));
            std::thread::spawn(move || drive(&program, &args, &job, MAX_PROCESS_SECONDS));
            return Ok(id);
        }
        if def.kind == Kind::Slicer {
            let pairs: Vec<(String, String)> = head
                .get("settings")
                .and_then(|v| v.as_object())
                .map(|o| o.iter().map(|(k, v)| (k.clone(), match v { serde_json::Value::String(s) => s.clone(), other => other.to_string() })).collect())
                .unwrap_or_default();
            let refs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            let settings = SlicerSettings::parse(&refs)?;
            let bytes = crate::files::body_bytes(request.body()).ok_or_else(|| bad("invalid", "expected the plate"))?;
            if bytes.len() < 84 || bytes.len() > MAX_FILE_BYTES {
                return Err(bad("invalid", "the plate is not a sensible size"));
            }
            let found = tauri::async_runtime::spawn_blocking({
                let app = app.clone();
                move || find_one(&app, def)
            })
            .await
            .map_err(|e| e.to_string())?;
            if !found.found {
                return Err(bad(&found.why, &format!("{} is not available", def.name)));
            }
            let program = PathBuf::from(&found.path);
            let (files, args, result) = if settings.target == "bambu-p2s" {
                if def.id != "orcaslicer" { return Err(bad("invalid", "that route uses OrcaSlicer")); }
                bambu_job(&settings, "plate.stl", &orca_profiles(&program)?)?
            } else {
                slicer_job(def.id, &settings, "plate.stl")?
            };
            let dir = job_dir(0)?;
            let prep = std::fs::create_dir_all(dir.join("out")).and_then(|_| std::fs::write(dir.join("plate.stl"), &bytes)).and_then(|_| files.iter().try_for_each(|(n, t)| std::fs::write(dir.join(n), t)));
            if let Err(e) = prep {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(bad("job_failed", &e.to_string()));
            }
            let (id, job) = jobs.add(dir);
            std::thread::spawn(move || drive_slicer(&program, &args, &job, MAX_PROCESS_SECONDS, &result));
            return Ok(id);
        }
        if def.kind != Kind::Render {
            return Err(bad("invalid", "that engine does not render here"));
        }
        let pairs: Vec<(String, String)> = head
            .get("settings")
            .and_then(|v| v.as_object())
            .map(|o| o.iter().map(|(k, v)| (k.clone(), match v { serde_json::Value::String(s) => s.clone(), other => other.to_string() })).collect())
            .unwrap_or_default();
        let refs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let settings = BlenderSettings::parse(&refs)?;
        let bytes = crate::files::body_bytes(request.body()).ok_or_else(|| bad("invalid", "expected the package"))?;
        let resources = app.path().resource_dir().map_err(|e| e.to_string())?;
        let script = script_path(&resources, def)?;
        let found = tauri::async_runtime::spawn_blocking({
            let app = app.clone();
            move || find_one(&app, def)
        })
        .await
        .map_err(|e| e.to_string())?;
        if !found.found {
            return Err(bad(&found.why, &format!("{} is not available", def.name)));
        }
        let program = PathBuf::from(&found.path);
        let files = unzip_stored(&bytes)?;
        let dir = job_dir(0)?;
        let prep = write_package(&dir, &files).and_then(|_| {
            let job_file = dir.join("cc-engine-job.json");
            std::fs::write(&job_file, settings.job_json("cadcraft-render.json", "out")).map_err(|e| bad("job_failed", &e.to_string()))?;
            Ok(job_file)
        });
        let job_file = match prep {
            Ok(f) => f,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        let (id, job) = jobs.add(dir);
        let args = blender_args(&script, &job_file);
        std::thread::spawn(move || drive(&program, &args, &job, MAX_JOB_SECONDS));
        Ok(id)
    }

    #[derive(Serialize)]
    pub struct StatusOut {
        pub state: &'static str,
        pub progress: f32,
        pub error: String,
        pub log: Vec<String>,
    }

    #[tauri::command]
    pub fn cc_engine_status(jobs: State<'_, Jobs>, job: u64) -> Result<StatusOut, String> {
        let j = jobs.get(job).ok_or_else(|| bad("no_job", "no such job"))?;
        let s = j.status.lock().unwrap().clone();
        Ok(StatusOut { state: match s.state { super::State::Running => "running", super::State::Done => "done", super::State::Failed => "failed", super::State::Cancelled => "cancelled" }, progress: s.progress, error: s.error, log: s.log })
    }

    /// The picture of a finished job as bytes; the job folder goes with it.
    #[tauri::command(async)]
    pub async fn cc_engine_result(jobs: State<'_, Jobs>, job: u64) -> Result<tauri::ipc::Response, String> {
        let j = jobs.get(job).ok_or_else(|| bad("no_job", "no such job"))?;
        let (state, rel) = {
            let s = j.status.lock().unwrap();
            (s.state.clone(), s.result.clone())
        };
        if state != super::State::Done {
            return Err(bad("not_done", "the job has not finished"));
        }
        let rel = rel.ok_or_else(|| bad("no_picture", "no picture"))?;
        let dir = j.dir.clone();
        let bytes = tauri::async_runtime::spawn_blocking(move || read_result(&dir, &rel)).await.map_err(|e| e.to_string())??;
        let _ = std::fs::remove_dir_all(&j.dir);
        jobs.remove(job);
        Ok(tauri::ipc::Response::new(bytes))
    }

    #[tauri::command]
    pub fn cc_engine_cancel(jobs: State<'_, Jobs>, job: u64) -> Result<(), String> {
        let j = jobs.get(job).ok_or_else(|| bad("no_job", "no such job"))?;
        j.cancel.store(true, Ordering::SeqCst);
        let dir = j.dir.clone();
        std::thread::spawn(move || {
            // the driver thread sees the flag, kills the tree and marks the job cancelled; the folder goes a moment later
            std::thread::sleep(std::time::Duration::from_millis(600));
            let _ = std::fs::remove_dir_all(dir);
        });
        Ok(())
    }

    #[derive(Serialize)]
    pub struct OpenRow {
        pub id: &'static str,
        pub name: &'static str,
        pub found: bool,
        pub version: String,
        pub reads: Vec<&'static str>,
    }

    /// The programs that can open the scene: Blender (found as for the engine), usdview (on the search path), the system's own program.
    #[tauri::command(async)]
    pub async fn cc_engine_open_list(app: AppHandle) -> Result<Vec<OpenRow>, String> {
        if cfg!(mobile) {
            return Err(bad("unsupported", "no outside programs on tablets"));
        }
        tauri::async_runtime::spawn_blocking(move || {
            let dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
            OPEN_IN
                .iter()
                .map(|d| {
                    let (found, version) = match d.id {
                        "blender" => super::engine("blender").map(|e| { let f = find_one(&app, e); (f.found, f.version) }).unwrap_or((false, String::new())),
                        "system" => (true, String::new()),
                        _ => (!on_path(d.programs, Os::here(), &dirs, &|p| p.is_file()).is_empty(), String::new()),
                    };
                    OpenRow { id: d.id, name: d.name, found, version, reads: d.reads.to_vec() }
                })
                .collect()
        })
        .await
        .map_err(|e| e.to_string())
    }

    /// Hands a sliced .gcode.3mf to Bambu Connect, Bambu's documented route for other programs: a local file and its URL scheme. It opens there; the person starts it there. Header `x-cc-name`.
    #[tauri::command]
    pub async fn cc_engine_bambu_connect(app: AppHandle, request: Request<'_>) -> Result<(), String> {
        if cfg!(mobile) {
            return Err(bad("unsupported", "no outside programs on tablets"));
        }
        let name: String = request.headers().get("x-cc-name").and_then(|v| v.to_str().ok()).and_then(crate::files::decode_component).unwrap_or_default().chars().filter(|c| c.is_ascii_alphanumeric() || " _-".contains(*c)).take(80).collect();
        let name = if name.trim().is_empty() { "plate".to_string() } else { name.trim().to_string() };
        let bytes = crate::files::body_bytes(request.body()).ok_or_else(|| bad("invalid", "expected the file"))?;
        if bytes.len() < 4 || &bytes[..2] != b"PK" || bytes.len() > MAX_FILE_BYTES {
            return Err(bad("invalid", "that is not a sliced 3MF"));
        }
        let dir = std::env::temp_dir().join("cadcraft-print");
        std::fs::create_dir_all(&dir).map_err(|e| bad("job_failed", &e.to_string()))?;
        let file = dir.join(format!("{name}.gcode.3mf"));
        std::fs::write(&file, &bytes).map_err(|e| bad("job_failed", &e.to_string()))?;
        let url = super::bambu_connect_url(&file.to_string_lossy(), &format!("{name}.gcode.3mf"));
        use tauri_plugin_opener::OpenerExt;
        app.opener().open_url(url, None::<&str>).map_err(|e| bad("no_bambu_connect", &e.to_string()))
    }

    /// Body = the package (a stored zip, with scene.usdc/.usdz and/or scene.glb); header `x-cc-open` = `{program, kind: "usd"|"glb", name}`.
    /// The package is unpacked into `<Documents>/CadCraft hand-off/<name>/` (a new folder each time) and the program started on the file.
    /// → the folder's path, so the page can say where the files are.
    #[tauri::command(async)]
    pub async fn cc_engine_open(app: AppHandle, request: Request<'_>) -> Result<String, String> {
        if cfg!(mobile) {
            return Err(bad("unsupported", "no outside programs on tablets"));
        }
        let head: serde_json::Value = request.headers().get("x-cc-open").and_then(|v| v.to_str().ok()).and_then(crate::files::decode_component).and_then(|s| serde_json::from_str(&s).ok()).ok_or_else(|| bad("invalid", "missing job"))?;
        let id = head.get("program").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let kind = head.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let name = head.get("name").and_then(|v| v.as_str()).unwrap_or("scene").to_string();
        let def = open_def(&id).ok_or_else(|| bad("unknown_engine", "no such program"))?;
        let bytes = crate::files::body_bytes(request.body()).ok_or_else(|| bad("invalid", "expected the package"))?;
        let resources = app.path().resource_dir().map_err(|e| e.to_string())?;
        let docs = app.path().document_dir().map_err(|e| e.to_string())?;
        tauri::async_runtime::spawn_blocking(move || -> Result<String, String> {
            let files = unzip_stored(&bytes)?;
            let file = open_file_for(def, &kind, &files)?;
            let mut script: Option<PathBuf> = None;
            let program: PathBuf = match def.id {
                "blender" => {
                    let eng = super::engine("blender").ok_or_else(|| bad("unknown_engine", "Blender is not part of this build"))?;
                    let found = find_one(&app, eng);
                    if !found.found { return Err(bad(&found.why, "Blender is not available")); }
                    let s = def.script.ok_or_else(|| bad("no_script", "no script"))?;
                    let p = resources.join("engines").join("blender").join(s);
                    if !p.is_file() { return Err(bad("no_script", "the Open in Blender script is missing from this build")); }
                    script = Some(p);
                    PathBuf::from(found.path)
                }
                "usdview" => {
                    let dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
                    let p = on_path(def.programs, Os::here(), &dirs, &|p| p.is_file()).into_iter().next().ok_or_else(|| bad("not_found", "usdview is not on this computer"))?;
                    p
                }
                _ => PathBuf::new(),
            };
            let dir = hand_off_folder(&docs.join("CadCraft hand-off"), &name)?;
            for (n, d) in &files {
                let p = dir.join(n);
                if let Some(parent) = p.parent() { std::fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
                std::fs::write(&p, d).map_err(|e| bad("job_failed", &e.to_string()))?;
            }
            let target = dir.join(&file);
            match def.id {
                "blender" => {
                    // the same job file the engine writes, so the scene is built exactly as it is rendered; size and engine are only used by the script's setup
                    let job = BlenderSettings::default().job_json("cadcraft-render.json", "out");
                    let job_file = dir.join("cc-engine-job.json");
                    std::fs::write(&job_file, job).map_err(|e| bad("job_failed", &e.to_string()))?;
                    std::fs::create_dir_all(dir.join("out")).map_err(|e| e.to_string())?;
                    let args = blender_open_args(script.as_deref().ok_or_else(|| bad("no_script", "no script"))?, &job_file);
                    launch_detached(&program, &args, &dir)?;
                }
                "usdview" => { launch_detached(&program, &[target.to_string_lossy().into_owned()], &dir)?; }
                _ => { let (p, a) = system_open(Os::here(), &target); launch_detached(Path::new(&p), &a, &dir)?; }
            }
            Ok(dir.to_string_lossy().into_owned())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// The Unreal project folder CadCraft sends to (remembered), or null.
    #[tauri::command]
    pub fn cc_engine_project(app: AppHandle) -> Result<Option<String>, String> {
        Ok(read_conf(&app).get(UNREAL_PROJECT_KEY).map(|p| p.to_string_lossy().into_owned()))
    }

    /// The person picks the project folder once (a folder dialog; it must hold a .uproject). → the folder, or null when cancelled.
    #[tauri::command(async)]
    pub async fn cc_engine_project_pick(app: AppHandle, win: tauri::WebviewWindow) -> Result<Option<String>, String> {
        use tauri_plugin_dialog::DialogExt;
        if cfg!(mobile) {
            return Err(bad("unsupported", "no outside programs on tablets"));
        }
        let picked: Option<PathBuf> = match crate::files::test_answer() {
            Some(a) => a.and_then(|v| v.into_iter().next()),
            None => tauri::async_runtime::spawn_blocking(move || win.dialog().file().set_parent(&win).set_title("Pick your Unreal project folder").blocking_pick_folder().and_then(|f| f.into_path().ok()))
                .await
                .map_err(|e| e.to_string())?,
        };
        let Some(dir) = picked else { return Ok(None) };
        uproject_in(&dir)?;
        let mut conf = read_conf(&app);
        conf.insert(UNREAL_PROJECT_KEY.to_string(), dir.clone());
        let cp = conf_path(&app)?;
        if let Some(d) = cp.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        std::fs::write(&cp, write_conf(&conf)?).map_err(|e| e.to_string())?;
        Ok(Some(dir.to_string_lossy().into_owned()))
    }

    /// Body = the package; header `x-cc-open` = `{name}`. Writes it into the remembered project (`unreal_send`). → the folder.
    #[tauri::command(async)]
    pub async fn cc_engine_send(app: AppHandle, request: Request<'_>) -> Result<String, String> {
        if cfg!(mobile) {
            return Err(bad("unsupported", "no outside programs on tablets"));
        }
        let head: serde_json::Value = request.headers().get("x-cc-open").and_then(|v| v.to_str().ok()).and_then(crate::files::decode_component).and_then(|s| serde_json::from_str(&s).ok()).ok_or_else(|| bad("invalid", "missing job"))?;
        let name = head.get("name").and_then(|v| v.as_str()).unwrap_or("scene").to_string();
        let bytes = crate::files::body_bytes(request.body()).ok_or_else(|| bad("invalid", "expected the package"))?;
        let project = read_conf(&app).get(UNREAL_PROJECT_KEY).cloned().ok_or_else(|| bad("no_project", "pick your Unreal project folder first"))?;
        tauri::async_runtime::spawn_blocking(move || {
            let files = unzip_stored(&bytes)?;
            unreal_send(&project, &name, &files).map(|(d, _)| d.to_string_lossy().into_owned())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    fn addon_run_from(head: &serde_json::Value) -> Result<AddonRun, String> {
        let strs = |k: &str| -> Vec<String> { head.get(k).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
        let r = AddonRun { addon: head.get("addon").and_then(|v| v.as_str()).unwrap_or("").to_string(), programs: strs("programs"), script: head.get("script").and_then(|v| v.as_str()).unwrap_or("").to_string(), args: strs("args") };
        validate_addon_run(&r)?;
        Ok(r)
    }

    /// The program an add-on names: the person's pick for it, else the first of its names found on the search path. → (path, name) or null.
    fn addon_program(app: &AppHandle, r: &AddonRun) -> Option<(PathBuf, String)> {
        let conf = read_conf(app);
        let dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        for name in &r.programs {
            if let Some(p) = conf.get(&format!("program:{}:{}", r.addon, name)) {
                if p.is_file() && p.file_stem().map(|s| s.to_string_lossy().to_lowercase() == name.to_lowercase()).unwrap_or(false) {
                    return Some((p.clone(), name.clone()));
                }
            }
            if let Some(p) = on_path(&[name.as_str()], Os::here(), &dirs, &|p| p.is_file()).into_iter().next() {
                return Some((p, name.clone()));
            }
        }
        None
    }

    /// Header `x-cc-engine` = `{addon, programs, script, args}`: is a program there? → {found, program, allowed}. Nothing runs.
    #[tauri::command(async)]
    pub async fn cc_engine_addon_find(app: AppHandle, request: Request<'_>) -> Result<serde_json::Value, String> {
        let head: serde_json::Value = request.headers().get("x-cc-engine").and_then(|v| v.to_str().ok()).and_then(crate::files::decode_component).and_then(|s| serde_json::from_str(&s).ok()).ok_or_else(|| bad("invalid", "missing job"))?;
        let r = addon_run_from(&head)?;
        tauri::async_runtime::spawn_blocking(move || {
            let hit = addon_program(&app, &r);
            let allowed = hit.as_ref().map(|(_, n)| read_conf(&app).contains_key(&consent_key(&r.addon, n))).unwrap_or(false);
            Ok(serde_json::json!({ "found": hit.is_some(), "program": hit.map(|(_, n)| n), "allowed": allowed }))
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// The person picks the program for this add-on once (the file's name must be one the add-on declared).
    #[tauri::command(async)]
    pub async fn cc_engine_addon_pick(app: AppHandle, win: tauri::WebviewWindow, request: Request<'_>) -> Result<Option<String>, String> {
        use tauri_plugin_dialog::DialogExt;
        let head: serde_json::Value = request.headers().get("x-cc-engine").and_then(|v| v.to_str().ok()).and_then(crate::files::decode_component).and_then(|s| serde_json::from_str(&s).ok()).ok_or_else(|| bad("invalid", "missing job"))?;
        let r = addon_run_from(&head)?;
        let picked: Option<PathBuf> = match crate::files::test_answer() {
            Some(a) => a.and_then(|v| v.into_iter().next()),
            None => tauri::async_runtime::spawn_blocking(move || win.dialog().file().set_parent(&win).set_title("Find the program").blocking_pick_file().and_then(|f| f.into_path().ok())).await.map_err(|e| e.to_string())?,
        };
        let Some(p) = picked else { return Ok(None) };
        let stem = p.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        let name = r.programs.iter().find(|n| n.to_lowercase() == stem).ok_or_else(|| bad("not_this_program", "that file is not one of the programs this add-on named"))?.clone();
        if !p.is_file() {
            return Err(bad("not_found", "that file is not there"));
        }
        let mut conf = read_conf(&app);
        conf.insert(format!("program:{}:{}", r.addon, name), p);
        let cp = conf_path(&app)?;
        if let Some(d) = cp.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        std::fs::write(&cp, write_conf(&conf)?).map_err(|e| e.to_string())?;
        Ok(Some(name))
    }

    /// Body = the package with the add-on's script added as `engine/<file>`; header `x-cc-engine` = `{addon, name, programs, script, args,
    /// settings}`. The first time for an add-on and program, **Studio's own dialog** says what will run and asks (the page cannot answer it);
    /// the answer is remembered. → the job id (poll with cc_engine_status / result / cancel).
    #[tauri::command(async)]
    pub async fn cc_engine_addon_start(app: AppHandle, win: tauri::WebviewWindow, jobs: State<'_, Jobs>, request: Request<'_>) -> Result<u64, String> {
        use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
        if cfg!(mobile) {
            return Err(bad("unsupported", "no outside programs on tablets"));
        }
        let head: serde_json::Value = request.headers().get("x-cc-engine").and_then(|v| v.to_str().ok()).and_then(crate::files::decode_component).and_then(|s| serde_json::from_str(&s).ok()).ok_or_else(|| bad("invalid", "missing job"))?;
        let r = addon_run_from(&head)?;
        let addon_name: String = head.get("name").and_then(|v| v.as_str()).unwrap_or(&r.addon).chars().filter(|c| !c.is_control()).take(60).collect();
        let pairs: Vec<(String, String)> = head.get("settings").and_then(|v| v.as_object()).map(|o| o.iter().map(|(k, v)| (k.clone(), match v { serde_json::Value::String(s) => s.clone(), other => other.to_string() })).collect()).unwrap_or_default();
        let refs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let settings = BlenderSettings::parse(&refs)?;
        let bytes = crate::files::body_bytes(request.body()).ok_or_else(|| bad("invalid", "expected the package"))?;
        let (program, pname) = tauri::async_runtime::spawn_blocking({ let app = app.clone(); let r = r.clone(); move || addon_program(&app, &r) }).await.map_err(|e| e.to_string())?.ok_or_else(|| bad("not_found", "the program is not on this computer"))?;
        let key = consent_key(&r.addon, &pname);
        if !read_conf(&app).contains_key(&key) {
            let text = format!("The add-on \"{addon_name}\" wants to run the program \"{pname}\" on this computer, with a script that comes with the add-on.\n\nA script can do anything the program can. Only allow this for an add-on you trust.");
            let yes = match crate::files::test_answer() {
                Some(a) => a.is_some(),
                None => { let w = win.clone(); tauri::async_runtime::spawn_blocking(move || w.dialog().message(text).title("Run a program for an add-on?").buttons(MessageDialogButtons::OkCancelCustom("Allow".into(), "Don't allow".into())).blocking_show()).await.map_err(|e| e.to_string())? }
            };
            if !yes {
                return Err(bad("not_allowed", "you did not allow this add-on to run that program"));
            }
            let mut conf = read_conf(&app);
            conf.insert(key, PathBuf::from("allowed"));
            let cp = conf_path(&app)?;
            if let Some(d) = cp.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            std::fs::write(&cp, write_conf(&conf)?).map_err(|e| e.to_string())?;
        }
        let files = unzip_stored(&bytes)?;
        if !files.iter().any(|(n, _)| *n == r.script) {
            return Err(bad("no_script", "the package does not hold the add-on's script"));
        }
        let dir = job_dir(0)?;
        let prep = write_package(&dir, &files).and_then(|_| std::fs::write(dir.join("cc-engine-job.json"), settings.job_json("cadcraft-render.json", "out")).map_err(|e| bad("job_failed", &e.to_string())));
        if let Err(e) = prep {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
        let args = addon_args(&r, &dir, "cadcraft-render.json", &settings);
        let (id, job) = jobs.add(dir);
        std::thread::spawn(move || drive(&program, &args, &job, MAX_JOB_SECONDS));
        Ok(id)
    }
}
#[cfg(not(standalone))]
pub use commands::*;

// ------------------------------------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }
    fn list_of(entries: &[(&str, &[&str])]) -> impl Fn(&Path) -> Vec<PathBuf> {
        let m: HashMap<PathBuf, Vec<PathBuf>> = entries.iter().map(|(d, es)| (PathBuf::from(d), es.iter().map(|e| Path::new(d).join(e)).collect())).collect();
        move |d| m.get(d).cloned().unwrap_or_default()
    }

    #[test]
    fn blender_version_is_read() {
        assert_eq!(parse_blender_version("Blender 4.2.1 LTS\n\tbuild date: 2024"), Some((4, 2)));
        assert_eq!(parse_blender_version("\nBlender 3.6.5\n"), Some((3, 6)));
        assert_eq!(parse_blender_version("Blender 4.5.0 Alpha"), Some((4, 5)));
        assert_eq!(parse_blender_version("blender: command not found"), None);
        assert_eq!(parse_blender_version("Blender"), None);
        assert_eq!(parse_unreal_folder("UE_5.4"), Some((5, 4)));
        assert_eq!(parse_unreal_folder("UE_5.3.2"), Some((5, 3)));
        assert_eq!(parse_unreal_folder("Launcher"), None);
    }

    #[test]
    fn python_version_is_read() {
        assert_eq!(parse_python_version("Python 3.12.1\n"), Some((3, 12)));
        assert_eq!(parse_python_version("Python 3.9.0rc1"), Some((3, 9)));
        assert_eq!(parse_python_version("Python was not found; run without arguments to install from the Microsoft Store"), None);
        assert_eq!(parse_python_version(""), None);
        assert_eq!(parse_python_version("Python"), None);
    }

    #[test]
    fn a_process_engines_command_line_is_three_paths_and_nothing_else() {
        let a = python_args(Path::new("/res/engines/cantera/cc_cantera_equilibrate.py"), Path::new("/tmp/j/cc-case.json"), Path::new("/tmp/j/out/result.json"));
        assert_eq!(a, ["/res/engines/cantera/cc_cantera_equilibrate.py", "/tmp/j/cc-case.json", "/tmp/j/out/result.json"]);
        // the one json a process engine may hand back; nothing else named .json
        let dir = std::env::temp_dir().join(format!("cc-proc-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("out")).unwrap();
        std::fs::write(dir.join("out/result.json"), b"{}").unwrap();
        std::fs::write(dir.join("out/other.json"), b"{}").unwrap();
        assert_eq!(read_result(&dir, "out/result.json").unwrap(), b"{}");
        assert!(read_result(&dir, "out/other.json").unwrap_err().starts_with("bad_result:"));
        assert!(read_result(&dir, "../out/result.json").unwrap_err().starts_with("bad_result:"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "cantera")]
    #[test]
    fn cantera_is_a_process_engine_run_through_python() {
        let def = engine("cantera").unwrap();
        assert!(def.kind == Kind::Process && def.script == Some("cc_cantera_equilibrate.py") && def.min_version == (3, 9));
        let none = |_: &str| None;
        let w = candidates("cantera", Os::Windows, &env_of(&[("LOCALAPPDATA", "C:\\U\\a\\AppData\\Local")]), &list_of(&[("C:\\U\\a\\AppData\\Local/Programs/Python", &["Python311", "Python312", "Launcher"])]));
        let w: Vec<String> = w.iter().map(|p| p.to_string_lossy().replace('\\', "/")).collect();
        assert_eq!(w.len(), 2);
        assert!(w[0].contains("Python312") && w[0].ends_with("python.exe"));
        assert!(candidates("cantera", Os::Linux, &none, &list_of(&[])).contains(&PathBuf::from("/usr/bin/python3")));
        assert!(candidates("cantera", Os::Mac, &none, &list_of(&[])).contains(&PathBuf::from("/opt/homebrew/bin/python3")));
        assert!(DENIED_PROGRAMS.iter().all(|d| !def.programs.contains(d)), "python is not on the denied list: the engine's own table names it");
    }

    #[cfg(not(feature = "cantera"))]
    fn slicer_pairs() -> SlicerSettings {
        SlicerSettings::parse(&[("bed", "[220,220,250]"), ("start", "G90\nM83\nM140 S60\nM104 S205\nG28\nM190 S60\nM109 S205\nG92 E0"), ("end", "M104 S0\nM140 S0\nM107\nM84"), ("infill", "15"), ("supports", "false")]).unwrap()
    }

    #[test]
    fn slicer_settings_refuse_what_they_do_not_know() {
        assert!(SlicerSettings::parse(&[("nozzle_temp", "900")]).is_err());
        assert!(SlicerSettings::parse(&[("bed", "[220,220]")]).is_err());
        assert!(SlicerSettings::parse(&[("material", "plutonium")]).is_err());
        assert!(SlicerSettings::parse(&[("output", "../../x")]).is_err());
        assert!(SlicerSettings::parse(&[("start", "G28\u{7}")]).is_err());
    }

    #[test]
    fn slicers_are_a_fixed_command_line() {
        let s = slicer_pairs();
        let (files, args, result) = slicer_job("prusaslicer", &s, "plate.stl").unwrap();
        assert_eq!(args, ["--export-gcode", "--dont-arrange", "--load", "cc-print.ini", "--output", "out/plate.gcode", "plate.stl"]);
        assert_eq!(result, "out/plate.gcode");
        assert!(files[0].1.contains("start_gcode = G90\\nM83"));
        let (files, args, result) = slicer_job("orcaslicer", &s, "plate.stl").unwrap();
        assert_eq!(files.len(), 3);
        assert!(args.contains(&"--slice".to_string()) && args.contains(&"--outputdir".to_string()));
        assert_eq!(result, "out/plate_1.gcode");
        assert!(slicer_job("calc", &s, "plate.stl").is_err());
        assert!(engine("prusaslicer").is_some() && engine("orcaslicer").is_some());
    }

    #[test]
    fn bambu_presets_are_flattened_and_the_link_is_encoded() {
        let d = job_dir(1).unwrap();
        for sub in ["machine", "process", "filament"] { std::fs::create_dir_all(d.join(sub)).unwrap(); }
        std::fs::write(d.join("machine/Bambu Lab P2S 0.4 nozzle.json"), r#"{"name":"m","inherits":"base","a":["x,}"],"b":"own"}"#).unwrap();
        std::fs::write(d.join("machine/base.json"), r#"{"b":"parent","c":{"d":[1,2]},"from":"system"}"#).unwrap();
        std::fs::write(d.join("process/0.20mm Standard @BBL P2S.json"), r#"{"name":"p","wall_loops":"9"}"#).unwrap();
        std::fs::write(d.join("filament/Generic PLA @BBL P2S.json"), r#"{"name":"f","inherits":"../../secret"}"#).unwrap();
        assert!(bambu_job(&slicer_pairs(), "plate.stl", &d).is_err(), "a parent that is a path is refused");
        std::fs::write(d.join("filament/Generic PLA @BBL P2S.json"), r#"{"name":"f"}"#).unwrap();
        let (files, args, result) = bambu_job(&slicer_pairs(), "plate.stl", &d).unwrap();
        assert!(files[0].1.contains(r#""b":"own""#) && files[0].1.contains(r#""c":{"d":[1,2]}"#) && files[0].1.contains(r#""a":["x,}"]"#) && !files[0].1.contains("inherits"));
        assert!(files[1].1.contains(r#""wall_loops":"2""#) && files[1].1.contains(r#""sparse_infill_density":"15%""#));
        assert!(args.contains(&"--export-3mf".to_string()) && result == "out/plate.gcode.3mf");
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(bambu_connect_url("C:\\T\\a b.gcode.3mf", "a b.gcode.3mf"), "bambu-connect://import-file?path=C%3A%5CT%5Ca%20b.gcode.3mf&name=a%20b.gcode.3mf&version=1.0.0");
        assert!(SlicerSettings::parse(&[("target", "x1c")]).is_err());
    }

    /// CC_ORCA_PROFILES=<OrcaSlicer's profiles/BBL> with CC_SLICER_FIXTURE: the P2S job for the real program.
    #[test]
    fn bambu_fixture_for_the_real_program() {
        let (Ok(dir), Ok(p)) = (std::env::var("CC_SLICER_FIXTURE"), std::env::var("CC_ORCA_PROFILES")) else { return };
        let (files, args, result) = bambu_job(&slicer_pairs(), "plate.stl", Path::new(&p)).unwrap();
        let d = Path::new(&dir).join("bambu");
        std::fs::create_dir_all(&d).unwrap();
        for (n, t) in files { std::fs::write(d.join(n), t).unwrap(); }
        std::fs::write(d.join("args.txt"), args.join("\n")).unwrap();
        std::fs::write(d.join("result.txt"), result).unwrap();
    }

    /// Writes the job for tools/printstage/remote-slicers.mjs, which runs exactly these files through the real programs: CC_SLICER_FIXTURE=<dir>.
    #[test]
    fn slicer_fixture_for_the_real_programs() {
        let Ok(dir) = std::env::var("CC_SLICER_FIXTURE") else { return };
        let s = slicer_pairs();
        for id in ["prusaslicer", "orcaslicer"] {
            let (files, args, result) = slicer_job(id, &s, "plate.stl").unwrap();
            let d = Path::new(&dir).join(id);
            std::fs::create_dir_all(&d).unwrap();
            for (n, t) in files { std::fs::write(d.join(n), t).unwrap(); }
            std::fs::write(d.join("args.txt"), args.join("\n")).unwrap();
            std::fs::write(d.join("result.txt"), result).unwrap();
        }
    }

    #[test]
    fn without_the_cantera_feature_there_is_no_process_engine() {
        assert!(engine("cantera").is_none());
        assert!(ENGINES.iter().all(|e| e.kind != Kind::Process));
    }

    #[test]
    fn programs_are_looked_for_in_each_systems_known_places() {
        let none = |_: &str| None;
        let w = candidates("blender", Os::Windows, &env_of(&[("ProgramFiles", "C:\\Program Files")]), &list_of(&[("C:\\Program Files/Blender Foundation", &["Blender 3.6", "Blender 4.2", "Other"])]));
        let w: Vec<String> = w.iter().map(|p| p.to_string_lossy().replace('\\', "/")).collect();
        assert_eq!(w, ["C:/Program Files/Blender Foundation/Blender 4.2/blender.exe", "C:/Program Files/Blender Foundation/Blender 3.6/blender.exe"]);
        let m = candidates("blender", Os::Mac, &none, &list_of(&[]));
        assert_eq!(m, [PathBuf::from("/Applications/Blender.app/Contents/MacOS/Blender")]);
        let l = candidates("blender", Os::Linux, &none, &list_of(&[("/opt", &["blender-4.2.1-linux-x64", "google", "Blender-3.6"])]));
        assert!(l.contains(&PathBuf::from("/usr/bin/blender")) && l.contains(&PathBuf::from("/opt/blender-4.2.1-linux-x64/blender")) && !l.iter().any(|p| p.to_string_lossy().contains("google")));
        let u = candidates("unreal", Os::Windows, &env_of(&[("ProgramFiles", "C:\\PF")]), &list_of(&[("C:\\PF/Epic Games", &["UE_5.3", "UE_5.4", "Launcher"])]));
        assert_eq!(u.len(), 2);
        assert!(u[0].to_string_lossy().contains("UE_5.4") && u[0].to_string_lossy().ends_with("UnrealEditor.exe"));
        assert!(candidates("unreal", Os::Linux, &none, &list_of(&[])).is_empty());
        assert!(candidates("nothing", Os::Linux, &none, &list_of(&[])).is_empty());
    }

    #[cfg(feature = "blender")]
    #[test]
    fn a_picked_file_must_be_the_engines_own_program() {
        let dir = std::env::temp_dir().join(format!("cc-engine-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let b = dir.join("Blender.EXE");
        std::fs::write(&b, b"x").unwrap();
        let def = engine("blender").unwrap();
        assert!(valid_program(def, &b).is_ok(), "case and .exe do not matter");
        let other = dir.join("cmd.exe");
        std::fs::write(&other, b"x").unwrap();
        assert!(valid_program(def, &other).unwrap_err().starts_with("not_this_program:"));
        assert!(valid_program(def, &dir.join("blender")).unwrap_err().starts_with("not_found:"), "a name that fits but is not there");
        let sub = dir.join("blender");
        std::fs::create_dir_all(&sub).unwrap();
        assert!(valid_program(def, &sub).unwrap_err().starts_with("not_found:"), "a folder is not a program");
        assert!(valid_program(engine("unreal").unwrap(), &b).is_err(), "Blender's file is not Unreal's");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(feature = "blender")]
    #[test]
    fn remembered_paths_round_trip_and_unknown_engines_are_dropped() {
        let mut m = HashMap::new();
        m.insert("blender".to_string(), PathBuf::from("/opt/blender/blender"));
        let t = write_conf(&m).unwrap();
        assert_eq!(parse_conf(&t), m);
        assert!(parse_conf("evil\t/bin/sh\nblender\t\n").is_empty());
        m.insert("unreal".to_string(), PathBuf::from("/a\tb"));
        assert!(write_conf(&m).is_err(), "a path with a tab cannot be stored");
    }

    #[test]
    fn settings_come_from_typed_fields_only() {
        let s = BlenderSettings::parse(&[("engine", "eevee"), ("samples", "128"), ("device", "gpu"), ("denoise", "false"), ("format", "exr"), ("width", "640"), ("height", "360")]).unwrap();
        assert_eq!((s.engine, s.samples, s.device, s.denoise, s.format, s.width, s.height), (BlenderEngine::Eevee, 128, Device::Gpu, false, Format::Exr, 640, 360));
        assert_eq!(BlenderSettings::parse(&[]).unwrap(), BlenderSettings::default());
        for bad_pair in [("engine", "cycles; rm -rf /"), ("engine", "Cycles"), ("samples", "0"), ("samples", "99999"), ("samples", "-1"), ("samples", "1e3"), ("device", "tpu"), ("denoise", "maybe"), ("format", "../x"), ("width", "0"), ("width", "20000"), ("frames", "1,2"), ("frames", "5,1,24"), ("frames", "0,10,0"), ("python", "import os"), ("--python", "x"), ("", "")] {
            assert!(BlenderSettings::parse(&[bad_pair]).unwrap_err().starts_with("invalid:"), "{bad_pair:?}");
        }
        assert!(BlenderSettings::parse(&[("width", "16384"), ("height", "16384")]).unwrap_err().starts_with("invalid:"), "too many pixels");
        let a = BlenderSettings::parse(&[("frames", "1,48,24")]).unwrap();
        assert_eq!(a.frames, Some((1, 48, 24)));
        let j = a.job_json("cadcraft-render.json", "out");
        assert!(j.contains("\"frames\":[1,48,24]") && j.contains("\"manifest\":\"cadcraft-render.json\"") && j.contains("\"engine\":\"cycles\"") && j.contains("\"denoise\":true"), "{j}");
        assert!(BlenderSettings::default().job_json("m.json", "out").contains("\"frames\":null"));
    }

    #[test]
    fn blender_gets_a_fixed_command_line_and_no_shell() {
        let a = blender_args(Path::new("/res/engines/blender/cc_blender_render.py"), Path::new("/tmp/j/cc-engine-job.json"));
        assert_eq!(a, ["--background", "--factory-startup", "--noaudio", "--python-exit-code", "3", "--python", "/res/engines/blender/cc_blender_render.py", "--", "/tmp/j/cc-engine-job.json"]);
        assert!(a.iter().all(|x| !x.contains(';') && !x.contains('|')));
    }

    #[cfg(feature = "blender")]
    #[test]
    fn only_the_shipped_script_is_ever_run() {
        let res = std::env::temp_dir().join(format!("cc-engine-res-{}", std::process::id()));
        std::fs::create_dir_all(res.join("engines/blender")).unwrap();
        let def = engine("blender").unwrap();
        assert!(script_path(&res, def).unwrap_err().starts_with("no_script:"), "missing script");
        std::fs::write(res.join("engines/blender/cc_blender_render.py"), "x").unwrap();
        assert_eq!(script_path(&res, def).unwrap(), res.join("engines/blender/cc_blender_render.py"));
        assert!(script_path(&res, engine("unreal").unwrap()).unwrap_err().starts_with("no_script:"), "a link engine runs no script");
        std::fs::remove_dir_all(&res).unwrap();
    }

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, data) in files {
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&[20, 0, 0, 8, 0, 0, 0, 0, 0x21, 0]);
            out.extend_from_slice(&crc32(data).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
        }
        out
    }

    #[test]
    fn crc_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn a_package_unpacks_only_if_every_name_is_safe() {
        let ok = zip_of(&[("cadcraft-render.json", b"{}"), ("scene.usdc", b"PXR-USDC"), ("ies/a.ies", b"IES")]);
        let f = unzip_stored(&ok).unwrap();
        assert_eq!(f.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["cadcraft-render.json", "scene.usdc", "ies/a.ies"]);
        for evil in ["../x", "/etc/passwd", "a/../../b", "a\\b", "C:/x", "a//b", "./a", "", "a/./b", "x\u{0}y"] {
            let z = zip_of(&[("cadcraft-render.json", b"{}"), (evil, b"x")]);
            assert!(unzip_stored(&z).unwrap_err().starts_with("bad_name:"), "{evil:?}");
        }
        assert!(unzip_stored(&zip_of(&[("a", b"1"), ("a", b"2")])).unwrap_err().starts_with("bad_name:"), "twice");
        let mut flipped = ok.clone();
        let at = flipped.windows(4).position(|w| w == b"PXR-").unwrap();
        flipped[at] ^= 0xff;
        assert!(unzip_stored(&flipped).unwrap_err().starts_with("damaged:"), "crc");
        assert!(unzip_stored(&ok[..ok.len() - 3]).unwrap_err().starts_with("damaged:"), "cut short");
        assert!(unzip_stored(b"not a zip at all, really not one").unwrap_err().starts_with("not_package:"));
        let mut compressed = ok.clone();
        compressed[8] = 8;
        assert!(unzip_stored(&compressed).unwrap_err().starts_with("compressed_entry:"));
        let many: Vec<(String, Vec<u8>)> = (0..MAX_FILES + 1).map(|i| (format!("f{i}"), vec![])).collect();
        let refs: Vec<(&str, &[u8])> = many.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
        assert!(unzip_stored(&zip_of(&refs)).unwrap_err().starts_with("too_big:"));
    }

    #[test]
    fn the_job_folder_gets_the_package_and_needs_its_manifest() {
        let d = job_dir(7).unwrap();
        assert!(d.join("out").is_dir() && d.file_name().unwrap().to_string_lossy().starts_with("cadcraft-engine-7-"));
        assert!(write_package(&d, &[("scene.usdc".into(), vec![1])]).unwrap_err().starts_with("no_manifest:"));
        write_package(&d, &[("cadcraft-render.json".into(), b"{}".to_vec()), ("ies/a.ies".into(), b"x".to_vec())]).unwrap();
        assert!(d.join("ies/a.ies").is_file());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn progress_lines_are_read_and_clamped() {
        assert_eq!(parse_line("CCPROGRESS 0.42\n"), Line::Progress(0.42));
        assert_eq!(parse_line("CCPROGRESS 7"), Line::Progress(1.0));
        assert_eq!(parse_line("CCPROGRESS -3"), Line::Progress(0.0));
        assert_eq!(parse_line("CCPROGRESS nan"), Line::Text("CCPROGRESS nan".into()));
        assert_eq!(parse_line("CCPROGRESS abc"), Line::Text("CCPROGRESS abc".into()));
        assert_eq!(parse_line("CCRESULT out/result.png\r\n"), Line::Result("out/result.png".into()));
        assert_eq!(parse_line("CCERROR usd_import_failed; rm"), Line::Error("usd_import_failedrm".into()));
        assert_eq!(parse_line("CCERROR "), Line::Error("engine_failed".into()));
        assert_eq!(parse_line("Fra:1 Mem:12M | Sample 3/64"), Line::Progress(3.0 / 64.0));
        assert_eq!(parse_line("Fra:1 Mem:12M | Updating Scene"), Line::Text("Fra:1 Mem:12M | Updating Scene".into()));
        assert_eq!(parse_line("Fra:1 Mem:12M (Peak 14M) | Time:00:01.20 | Remaining:00:03.00 | Mem:3M, Peak:3M | Scene, ViewLayer | Sample 16/64"), Line::Progress(0.25));
        assert_eq!(parse_line("Fra:1 Mem:12M | Time:00:01.20 | Remaining:00:03.00 | Rendering 8 / 32 samples"), Line::Progress(0.25));
        assert_eq!(parse_line("| Sample 70/64"), Line::Text("| Sample 70/64".into()), "more than all is not progress");
        assert_eq!(parse_line("| Sample 3/0"), Line::Text("| Sample 3/0".into()));
        assert_eq!(parse_line("| Rendering Tile 3 / 9"), Line::Text("| Rendering Tile 3 / 9".into()), "only sample counts");
        match parse_line(&"x".repeat(5000)) {
            Line::Text(t) => assert_eq!(t.len(), 400),
            _ => panic!(),
        }
    }

    #[test]
    fn only_a_plain_picture_inside_out_can_be_handed_back() {
        let d = job_dir(9).unwrap();
        std::fs::write(d.join("out/result.png"), b"\x89PNGdata").unwrap();
        assert_eq!(read_result(&d, "out/result.png").unwrap(), b"\x89PNGdata");
        std::fs::write(d.join("out/frames.zip"), b"PK").unwrap();
        assert_eq!(read_result(&d, "out/frames.zip").unwrap(), b"PK");
        for rel in ["result.png", "out/../cc-engine-job.json", "/etc/passwd", "out/sub/x.png", "out/result.txt", "out/", "../out/result.png", "out/a\\b.png"] {
            assert!(read_result(&d, rel).unwrap_err().starts_with("bad_result:"), "{rel}");
        }
        assert!(read_result(&d, "out/missing.png").unwrap_err().starts_with("no_picture:"));
        #[cfg(unix)]
        {
            std::fs::write(d.join("secret.png"), b"s").unwrap();
            std::os::unix::fs::symlink(d.join("secret.png"), d.join("out/link.png")).unwrap();
            assert!(read_result(&d, "out/link.png").unwrap_err().starts_with("bad_result:"), "a link is not a picture");
        }
        std::fs::remove_dir_all(&d).unwrap();
    }


    #[cfg(feature = "blender")]
    #[test]
    fn open_in_files_programs_and_folders() {
        let blender = open_def("blender").unwrap();
        let pk = |names: &[&str]| -> Vec<(String, Vec<u8>)> { names.iter().map(|n| (n.to_string(), vec![])).collect() };
        assert_eq!(open_file_for(blender, "usd", &pk(&["cadcraft-render.json", "scene.usdc"])).unwrap(), "scene.usdc");
        assert_eq!(open_file_for(blender, "usd", &pk(&["scene.usdz"])).unwrap(), "scene.usdz");
        assert_eq!(open_file_for(blender, "glb", &pk(&["scene.glb", "scene.usdc"])).unwrap(), "scene.glb");
        assert!(open_file_for(blender, "glb", &pk(&["scene.usdc"])).unwrap_err().starts_with("no_file:"));
        assert!(open_file_for(open_def("usdview").unwrap(), "glb", &pk(&["scene.glb"])).unwrap_err().starts_with("invalid:"), "usdview reads USD only");
        assert!(open_file_for(blender, "../x", &pk(&["scene.usdc"])).unwrap_err().starts_with("invalid:"));
        assert!(open_def("cmd").is_none() && open_def("blender;rm").is_none());
        assert_eq!(safe_folder_name("My house; rm -rf /"), "My house rm -rf");
        assert_eq!(safe_folder_name("../../etc"), "etc");
        assert_eq!(safe_folder_name("..."), "scene");
        assert_eq!(safe_folder_name(""), "scene");
        assert!(safe_folder_name(&"a".repeat(200)).len() <= 60);
        let base = std::env::temp_dir().join(format!("cc-handoff-test-{}", std::process::id()));
        let a = hand_off_folder(&base, "House").unwrap();
        let b = hand_off_folder(&base, "House").unwrap();
        assert!(a.ends_with("House") && b.ends_with("House-2") && a.is_dir() && b.is_dir(), "a new folder each time, nothing overwritten");
        std::fs::remove_dir_all(&base).unwrap();
        let dirs = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let has = |p: &Path| p == Path::new("/b/usdview");
        assert_eq!(on_path(&["usdview"], Os::Linux, &dirs, &has), vec![PathBuf::from("/b/usdview")]);
        assert!(on_path(&["usdview"], Os::Linux, &dirs, &|_| false).is_empty());
        assert_eq!(on_path(&["usdview"], Os::Windows, &dirs, &|p| p == Path::new("/a").join("usdview.exe")), vec![PathBuf::from("/a").join("usdview.exe")]);
        assert_eq!(system_open(Os::Linux, Path::new("/h/s.usdc")), ("xdg-open".to_string(), vec!["/h/s.usdc".to_string()]));
        assert_eq!(system_open(Os::Mac, Path::new("/h/s.glb")).0, "open");
        assert_eq!(system_open(Os::Windows, Path::new("C:/h/s.glb")).0, "explorer.exe");
        assert_eq!(blender_open_args(Path::new("/res/cc_blender_open.py"), Path::new("/h/job.json")), ["--python", "/res/cc_blender_open.py", "--", "/h/job.json"]);
    }


    #[test]
    fn unreal_gets_the_scene_and_keeps_the_persons_layer() {
        let base = std::env::temp_dir().join(format!("cc-unreal-test-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        assert!(uproject_in(&base).unwrap_err().starts_with("not_a_project:"));
        assert!(uproject_in(&base.join("nope")).unwrap_err().starts_with("not_found:"));
        std::fs::write(base.join("MyGame.uproject"), "{}").unwrap();
        assert_eq!(uproject_in(&base).unwrap(), "MyGame");
        let pk = |scene: &[u8]| vec![("cadcraft-render.json".to_string(), b"{\"a\":1}".to_vec()), ("scene.usdc".to_string(), scene.to_vec()), ("ies/a.ies".to_string(), b"IES".to_vec())];
        let (dir, fresh) = unreal_send(&base, "My house; x", &pk(b"v1")).unwrap();
        assert!(fresh && dir.ends_with("CadCraft/My house x") && dir.join("scene.usdc").is_file() && dir.join("ies/a.ies").is_file());
        let root = std::fs::read_to_string(dir.join("My house x.root.usda")).unwrap();
        assert!(root.contains("@./My house x.user.usda@,") && root.contains("@./scene.usdc@") && root.find("user.usda").unwrap() < root.find("scene.usdc").unwrap(), "the person's layer is the stronger one");
        // the person edits their layer in Unreal; CadCraft sends again
        std::fs::write(dir.join("My house x.user.usda"), "#usda 1.0\n# the person's edit\n").unwrap();
        let (_, fresh2) = unreal_send(&base, "My house; x", &pk(b"v2-longer")).unwrap();
        assert!(!fresh2, "the stage already exists");
        assert_eq!(std::fs::read(dir.join("scene.usdc")).unwrap(), b"v2-longer", "the scene is replaced");
        assert!(std::fs::read_to_string(dir.join("My house x.user.usda")).unwrap().contains("the person's edit"), "the person's layer is never overwritten");
        assert_eq!(std::fs::read_to_string(dir.join("My house x.root.usda")).unwrap(), root, "nor the root stage");
        assert!(std::fs::read_dir(&dir).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp")), "no half-written files left");
        assert!(unreal_send(&base, "x", &[("scene.usdc".into(), vec![])]).unwrap_err().starts_with("no_manifest:"));
        assert!(unreal_send(&base, "x", &[("cadcraft-render.json".into(), vec![])]).unwrap_err().starts_with("no_file:"));
        assert!(unreal_send(&base.join("MyGame.uproject"), "x", &pk(b"v")).is_err(), "a file is not a project folder");
        let z = unreal_send(&base, "Z", &[("cadcraft-render.json".into(), vec![1]), ("scene.usdz".into(), vec![2])]).unwrap().0;
        assert!(std::fs::read_to_string(z.join("Z.root.usda")).unwrap().contains("@./scene.usdz@"), "a .usdz scene is named in the root stage");
        let mut m = HashMap::new();
        m.insert(UNREAL_PROJECT_KEY.to_string(), base.clone());
        assert_eq!(parse_conf(&write_conf(&m).unwrap()), m, "the project folder is remembered like a program");
        std::fs::remove_dir_all(&base).unwrap();
    }


    #[test]
    fn an_addons_run_is_declared_once_and_checked() {
        let ok = AddonRun { addon: "render-mitsuba".into(), programs: vec!["python".into(), "python3".into()], script: "engine/cc_mitsuba_render.py".into(), args: vec!["{script}".into(), "{job}".into(), "--quiet".into()] };
        assert!(validate_addon_run(&ok).is_ok());
        let with = |f: &dyn Fn(&mut AddonRun)| { let mut r = ok.clone(); f(&mut r); validate_addon_run(&r).unwrap_err() };
        for id in ["", "A", "a", "a..b", "a--b", "a.", "-a", "1a", "a b", "a/b"] {
            assert!(with(&|r| r.addon = id.into()).starts_with("invalid:"), "{id:?}");
        }
        assert!(addon_id_ok("com.example.pipes") && addon_id_ok("render-mitsuba") && !addon_id_ok(&"a".repeat(65)));
        for sh in ["sh", "bash", "PowerShell", "cmd", "curl", "sudo", "env", "open", "osascript"] {
            assert!(with(&|r| r.programs = vec![sh.into()]).starts_with("denied_program:"), "{sh}");
        }
        for p in ["", "a b", "a/b", "../python", "py;thon", "python.exe", "-python", "a\nb"] {
            assert!(with(&|r| r.programs = vec![p.into()]).starts_with("invalid:"), "{p:?}");
        }
        assert!(with(&|r| r.programs = vec![]).starts_with("invalid:") && with(&|r| r.programs = (0..5).map(|i| format!("p{i}")).collect()).starts_with("invalid:"));
        for sc in ["", "run.py", "engine/../x.py", "/engine/x.py", "engine\\x.py", "engine/a/b/c/d.py", "engine//x", "other/x.py", "engine/x.py\n"] {
            assert!(with(&|r| r.script = sc.into()).starts_with("invalid:"), "{sc:?}");
        }
        for a in ["-c", "--eval=x;y", "$(id)", "`id`", "a b", "{evil}", "{script}x", "---x", "-", "", "../x", "a|b"] {
            assert!(with(&|r| r.args = vec!["{script}".into(), a.into()]).starts_with("invalid:"), "{a:?}");
        }
        assert!(with(&|r| r.args = vec!["--quiet".into()]).starts_with("invalid:"), "the script must be passed");
        assert!(with(&|r| r.args = (0..17).map(|_| "{script}".to_string()).collect()).starts_with("invalid:"));
        let mut many = ok.clone();
        many.args = vec!["{script}".into(), "-v".into(), "--threads=4".into(), "2".into(), "{manifest}".into(), "{out}".into(), "{width}".into(), "{height}".into(), "{samples}".into()];
        assert!(validate_addon_run(&many).is_ok());
    }

    #[cfg(feature = "blender")]
    #[test]
    fn placeholders_become_paths_in_the_job_folder_and_numbers() {
        let r = AddonRun { addon: "x1".into(), programs: vec!["python".into()], script: "engine/run.py".into(), args: vec!["{script}".into(), "--quiet".into(), "{manifest}".into(), "{out}".into(), "{width}".into(), "{height}".into(), "{samples}".into(), "{job}".into()] };
        let s = BlenderSettings::parse(&[("width", "320"), ("height", "200"), ("samples", "12")]).unwrap();
        let a = addon_args(&r, Path::new("/tmp/j"), "cadcraft-render.json", &s);
        assert_eq!(a, ["/tmp/j/engine/run.py", "--quiet", "/tmp/j/cadcraft-render.json", "/tmp/j/out", "320", "200", "12", "/tmp/j/cc-engine-job.json"]);
        assert_eq!(consent_key("render-mitsuba", "python"), "allow:render-mitsuba:python");
        for (k, v) in [("allow:render-mitsuba:python", true), ("program:render-mitsuba:python3", true), ("allow:Bad Id:python", false), ("allow:a1:py/thon", false), ("allow:a1", false), ("evil", false), ("blender", true), ("unreal-project", true)] {
            assert_eq!(conf_key_ok(k), v, "{k}");
        }
        let mut m = HashMap::new();
        m.insert("allow:render-mitsuba:python".to_string(), PathBuf::from("allowed"));
        assert_eq!(parse_conf(&write_conf(&m).unwrap()), m, "a consent is remembered like a picked program");
    }

    #[cfg(unix)]
    #[test]
    fn a_launched_program_is_not_waited_for() {
        let d = std::env::temp_dir();
        let t = std::time::Instant::now();
        launch_detached(Path::new("/bin/sh"), &["-c".into(), "sleep 2".into()], &d).unwrap();
        assert!(t.elapsed().as_millis() < 1000, "returned at once");
        assert!(launch_detached(Path::new("/nonexistent/prog"), &[], &d).unwrap_err().starts_with("cannot_start:"));
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::sync::Mutex as M;

        fn slicer_run(script: &str) -> (State, String, Option<String>) {
            let dir = job_dir(1).unwrap();
            std::fs::create_dir_all(dir.join("out")).unwrap();
            let job = Job { dir: dir.clone(), status: Mutex::new(Status::new()), cancel: AtomicBool::new(false) };
            drive_slicer(Path::new("/bin/sh"), &["-c".into(), script.into()], &job, 20, "out/plate.gcode");
            let s = job.status.lock().unwrap();
            let r = (s.state.clone(), s.error.clone(), s.result.clone());
            let _ = std::fs::remove_dir_all(&dir);
            r
        }

        #[test]
        fn a_slicer_counts_only_with_a_clean_exit_and_its_file() {
            assert_eq!(slicer_run("echo G28 > out/plate.gcode"), (State::Done, String::new(), Some("out/plate.gcode".to_string())));
            let (state, error, result) = slicer_run("echo half > out/plate.gcode; exit 3");   // a crash leaves half a program
            assert!(state == State::Failed && error == "engine_failed" && result.is_none());
            let (state, error, result) = slicer_run("exit 0");   // nothing written
            assert!(state == State::Failed && error == "engine_failed" && result.is_none());
        }

        fn run(script: &str, seconds: u64, cancel_after_ms: Option<u64>) -> (Option<i32>, Vec<Line>, u128) {
            let d = job_dir(1).unwrap();
            let cancel = Arc::new(AtomicBool::new(false));
            if let Some(ms) = cancel_after_ms {
                let c = cancel.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                    c.store(true, Ordering::SeqCst);
                });
            }
            let lines = M::new(Vec::new());
            let t = std::time::Instant::now();
            let code = run_process(Path::new("/bin/sh"), &["-c".into(), script.into()], &d, &cancel, seconds, &|l| lines.lock().unwrap().push(l)).unwrap();
            let ms = t.elapsed().as_millis();
            std::fs::remove_dir_all(&d).unwrap();
            (code, lines.into_inner().unwrap(), ms)
        }

        #[test]
        fn a_script_reports_progress_and_a_result() {
            let (code, lines, _) = run("echo CCPROGRESS 0.25; echo hello; echo CCPROGRESS 1; echo 'CCRESULT out/result.png'; echo oops >&2", 20, None);
            assert_eq!(code, Some(0));
            assert!(lines.contains(&Line::Progress(0.25)) && lines.contains(&Line::Progress(1.0)) && lines.contains(&Line::Result("out/result.png".into())) && lines.contains(&Line::Text("hello".into())) && lines.contains(&Line::Text("oops".into())), "{lines:?}");
        }

        #[test]
        fn the_exit_code_comes_back() {
            assert_eq!(run("exit 3", 20, None).0, Some(3));
        }

        #[test]
        fn cancel_kills_the_whole_tree_quickly() {
            // the shell starts a child that would sleep for a minute; Cancel must end both
            let (code, _, ms) = run("sleep 60 & echo $! > child.pid; wait", 600, Some(300));
            assert_eq!(code, None);
            assert!(ms < 5000, "took {ms} ms");
        }

        #[test]
        fn a_job_that_runs_too_long_is_stopped() {
            let t = std::time::Instant::now();
            let (code, _, _) = run("sleep 60", 1, None);
            assert_eq!(code, None);
            assert!(t.elapsed().as_secs() < 10);
        }

        #[test]
        fn a_program_that_cannot_start_is_an_error_not_a_panic() {
            let d = job_dir(2).unwrap();
            let r = run_process(Path::new("/nonexistent/blender"), &[], &d, &AtomicBool::new(false), 5, &|_| {});
            assert!(r.unwrap_err().starts_with("cannot_start:"));
            std::fs::remove_dir_all(&d).unwrap();
        }

        #[test]
        fn a_whole_job_ends_done_failed_or_cancelled() {
            let jobs = Jobs::default();
            let mk = || { let d = job_dir(3).unwrap(); jobs.add(d) };
            let sh = Path::new("/bin/sh");
            let (_, ok) = mk();
            drive(sh, &["-c".into(), "echo CCPROGRESS 0.5; echo 'CCRESULT out/result.png'".into()], &ok, 20);
            let s = ok.status.lock().unwrap().clone();
            assert_eq!((s.state, s.progress, s.result.as_deref()), (State::Done, 1.0, Some("out/result.png")));
            let (_, no_pic) = mk();
            drive(sh, &["-c".into(), "echo CCPROGRESS 1".into()], &no_pic, 20);
            assert_eq!(no_pic.status.lock().unwrap().state, State::Failed, "exit 0 without a result is not done");
            let (_, errored) = mk();
            drive(sh, &["-c".into(), "echo CCERROR usd_import_failed; echo CCRESULT out/result.png".into()], &errored, 20);
            let s = errored.status.lock().unwrap().clone();
            assert_eq!((s.state, s.error.as_str()), (State::Failed, "usd_import_failed"));
            let (_, script_failed) = mk();
            drive(sh, &["-c".into(), "exit 3".into()], &script_failed, 20);
            assert_eq!(script_failed.status.lock().unwrap().error, "script_failed");
            let (_, stopped) = mk();
            stopped.cancel.store(true, Ordering::SeqCst);
            drive(sh, &["-c".into(), "sleep 30".into()], &stopped, 20);
            assert_eq!(stopped.status.lock().unwrap().state, State::Cancelled);
            assert_eq!(jobs.running(), 0, "every job has ended");
            for id in 1..=5 {
                if let Some(j) = jobs.remove(id) { let _ = std::fs::remove_dir_all(&j.dir); }
            }
        }
    }

    /// Owner rule 43 (studio_PART_3): with the shipped build (no `blender` feature) the GPL program is nowhere in the tables and cannot be
    /// reached as an add-on's program either.
    #[cfg(not(feature = "blender"))]
    #[test]
    fn blender_is_not_in_the_shipped_build() {
        assert!(engine("blender").is_none(), "no Blender engine");
        assert!(open_def("blender").is_none(), "no Open in Blender");
        assert!(ENGINES.iter().all(|e| e.programs.iter().all(|p| *p != "blender")) && OPEN_IN.iter().all(|d| d.programs.iter().all(|p| *p != "blender")));
        assert!(DENIED_PROGRAMS.contains(&"blender"));
        assert!(engine("unreal").is_some() && open_def("usdview").is_some() && open_def("system").is_some(), "the permissive and link rows stay");
    }
}
