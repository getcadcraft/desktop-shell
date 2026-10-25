// SPDX-License-Identifier: Apache-2.0
//! Printing straight from Studio (0.7 D7.20): the system's printers, and a plotted sheet sent to one with a paper size and a
//! number of copies. The page (CadCraft's Plot dialog) makes the bytes; the shell only hands them to the system's print
//! service, so no PDF program is needed and nothing is sent anywhere else.
//!
//! - **Linux and macOS (CUPS):** the plotted PDF goes to `lp` as it is (`lp -d <printer> -n <copies> -o media=<paper>`);
//!   printers from `lpstat -e` / `-d`, papers from `lpoptions -p <printer> -l`.
//! - **Windows:** there is no command that prints a PDF without a PDF program, so the page sends the same plotted sheet as a
//!   PNG at 300 dpi and the shell prints it through .NET's `PrintDocument` (PowerShell, a fixed script; the printer, paper
//!   and copies reach it in environment variables, never in the script text), fitted and centred in the printable area.
//! - Names are checked: the printer must be one the system listed a moment ago, a paper name is plain text, copies are 1–99.
//!   The program is started without a shell, with arguments, and the temp file is removed whatever happens.
//!   Tablets have no printing here (the page says so).

use serde::{Deserialize, Serialize};
use std::process::Command;
use tauri::ipc::Request;

/// 128 MiB: a plotted A0 sheet with pictures is far below it.
const MAX_BYTES: usize = 128 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Printer {
    pub name: String,
    pub default: bool,
    pub papers: Vec<String>,
}

#[derive(Serialize)]
pub struct Printers {
    /// What the page must send: "pdf" (CUPS) or "png" (Windows).
    pub accepts: &'static str,
    pub printers: Vec<Printer>,
}

#[derive(Deserialize, Debug)]
#[serde(default)]
pub struct PrintJob {
    pub printer: String,
    pub copies: u32,
    /// The printer's own name for the paper (one of its `papers`); empty = the printer's current paper.
    pub paper: String,
    pub landscape: bool,
    pub title: String,
}

impl Default for PrintJob {
    fn default() -> Self {
        PrintJob { printer: String::new(), copies: 1, paper: String::new(), landscape: false, title: String::new() }
    }
}

const fn accepts() -> &'static str {
    if cfg!(windows) { "png" } else { "pdf" }
}

fn bad(code: &str, msg: &str) -> String {
    format!("{code}: {msg}")
}

/// A paper or document name: plain characters only (it ends up as a CUPS option value or an environment variable).
pub fn plain_name(s: &str, max: usize) -> bool {
    !s.is_empty() && s.chars().count() <= max && s.chars().all(|c| c.is_alphanumeric() || " ._-()/+×x".contains(c))
}

/// The job, checked against the printers the system listed. Returns the clean copy count.
pub fn check_job(job: &PrintJob, printers: &[Printer]) -> Result<u32, String> {
    if !printers.iter().any(|p| p.name == job.printer) {
        return Err(bad("no_printer", "that printer is not on this computer's list"));
    }
    if job.copies == 0 || job.copies > 99 {
        return Err(bad("invalid", "copies must be 1 to 99"));
    }
    if !job.paper.is_empty() && !plain_name(&job.paper, 60) {
        return Err(bad("invalid", "the paper name has characters a printer name does not"));
    }
    Ok(job.copies)
}

// ---- CUPS (Linux, macOS) ----

/// `lpstat -e` → one destination per line.
pub fn parse_lpstat_e(out: &str) -> Vec<String> {
    out.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}

/// `lpstat -d` → "system default destination: NAME".
pub fn parse_lpstat_d(out: &str) -> Option<String> {
    out.lines().find_map(|l| l.trim().strip_prefix("system default destination:").map(|n| n.trim().to_string())).filter(|n| !n.is_empty())
}

/// `lpoptions -p NAME -l` → the values of the PageSize line ("PageSize/Media Size: A4 *Letter Legal" → A4, Letter, Legal).
pub fn parse_lpoptions_papers(out: &str) -> Vec<String> {
    out.lines()
        .find(|l| l.starts_with("PageSize/") || l.starts_with("media/"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.split_whitespace().map(|s| s.trim_start_matches('*').to_string()).filter(|s| plain_name(s, 60)).collect())
        .unwrap_or_default()
}

/// The `lp` arguments for a job (the file last). No shell is involved.
pub fn lp_args(job: &PrintJob, copies: u32, file: &str) -> Vec<String> {
    let mut a = vec!["-d".into(), job.printer.clone(), "-n".into(), copies.to_string()];
    if plain_name(&job.title, 80) {
        a.extend(["-t".into(), job.title.clone()]);
    }
    if !job.paper.is_empty() {
        a.extend(["-o".into(), format!("media={}", job.paper)]);
    }
    if job.landscape {
        a.extend(["-o".into(), "landscape".into()]);
    }
    // the sheet is drawn to its paper; if the printer's paper differs a little (or margins), fit rather than cut
    a.extend(["-o".into(), "fit-to-page".into()]);
    a.push("--".into());
    a.push(file.into());
    a
}

fn run_text(cmd: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(cmd).args(args).output().ok()?;
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

#[cfg(not(windows))]
fn list_printers() -> Result<Vec<Printer>, String> {
    let Some(names) = run_text("lpstat", &["-e"]) else { return Err(bad("no_print_service", "the print service (CUPS) is not installed or not running")) };
    let default = run_text("lpstat", &["-d"]).and_then(|o| parse_lpstat_d(&o));
    Ok(parse_lpstat_e(&names)
        .into_iter()
        .map(|name| {
            let papers = run_text("lpoptions", &["-p", &name, "-l"]).map(|o| parse_lpoptions_papers(&o)).unwrap_or_default();
            Printer { default: default.as_deref() == Some(name.as_str()), name, papers }
        })
        .collect())
}

#[cfg(not(windows))]
fn send(job: &PrintJob, copies: u32, bytes: &[u8]) -> Result<String, String> {
    let dir = temp_dir()?;
    let file = dir.join("plot.pdf");
    let r = std::fs::write(&file, bytes).map_err(|e| bad("print_failed", &e.to_string())).and_then(|_| {
        let out = Command::new("lp").args(lp_args(job, copies, &file.to_string_lossy())).output().map_err(|e| bad("print_failed", &format!("lp could not start: {e}")))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(bad("print_failed", String::from_utf8_lossy(&out.stderr).trim()))
        }
    });
    let _ = std::fs::remove_dir_all(&dir);
    r
}

// ---- Windows ----

/// The printers and their papers. Fixed text; nothing from the page is in it.
pub const WIN_LIST_SCRIPT: &str = r#"
Add-Type -AssemblyName System.Drawing
$def = (New-Object System.Drawing.Printing.PrinterSettings).PrinterName
$all = @()
foreach ($n in [System.Drawing.Printing.PrinterSettings]::InstalledPrinters) {
  $s = New-Object System.Drawing.Printing.PrinterSettings
  $s.PrinterName = $n
  $all += [pscustomobject]@{ name = [string]$n; default = ($n -eq $def); papers = @($s.PaperSizes | ForEach-Object { [string]$_.PaperName }) }
}
ConvertTo-Json -InputObject @($all) -Depth 4 -Compress
"#;

/// Prints the PNG in CC_FILE on CC_PRINTER: copies CC_COPIES, paper CC_PAPER (a name from that printer's list, or empty),
/// landscape when CC_LANDSCAPE is 1; fitted, centred, in the printable area. Fixed text; the values come as environment variables.
pub const WIN_PRINT_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$img = [System.Drawing.Image]::FromFile($env:CC_FILE)
try {
  $pd = New-Object System.Drawing.Printing.PrintDocument
  $pd.PrinterSettings.PrinterName = $env:CC_PRINTER
  if (-not $pd.PrinterSettings.IsValid) { throw 'no_printer' }
  $pd.PrinterSettings.Copies = [int16]$env:CC_COPIES
  $pd.DocumentName = $env:CC_TITLE
  $pd.DefaultPageSettings.Landscape = ($env:CC_LANDSCAPE -eq '1')
  if ($env:CC_PAPER) {
    foreach ($p in $pd.PrinterSettings.PaperSizes) { if ($p.PaperName -eq $env:CC_PAPER) { $pd.DefaultPageSettings.PaperSize = $p } }
  }
  $pd.add_PrintPage({
    param($sender, $e)
    $b = $e.Graphics.VisibleClipBounds
    $k = [Math]::Min($b.Width / $img.Width, $b.Height / $img.Height)
    $w = $img.Width * $k; $h = $img.Height * $k
    $e.Graphics.DrawImage($img, $b.X + ($b.Width - $w) / 2, $b.Y + ($b.Height - $h) / 2, $w, $h)
    $e.HasMorePages = $false
  })
  $pd.Print()
} finally { $img.Dispose() }
"#;

pub fn parse_win_printers(json: &str) -> Result<Vec<Printer>, String> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).map_err(|e| bad("print_failed", &format!("the printer list could not be read: {e}")))?;
    let items = match v {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Null => Vec::new(),
        o => vec![o],
    };
    Ok(items
        .into_iter()
        .filter_map(|p| {
            let name = p.get("name")?.as_str()?.to_string();
            let default = p.get("default").and_then(|d| d.as_bool()).unwrap_or(false);
            let papers = match p.get("papers") {
                Some(serde_json::Value::Array(a)) => a.iter().filter_map(|s| s.as_str()).filter(|s| plain_name(s, 60)).map(String::from).collect(),
                Some(serde_json::Value::String(s)) if plain_name(s, 60) => vec![s.clone()],
                _ => Vec::new(),
            };
            Some(Printer { name, default, papers })
        })
        .collect())
}

#[cfg(windows)]
fn powershell(script: &str, env: &[(&str, String)]) -> Result<std::process::Output, String> {
    use std::os::windows::process::CommandExt;
    let mut c = Command::new("powershell.exe");
    c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script]).creation_flags(0x0800_0000);
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().map_err(|e| bad("print_failed", &format!("PowerShell could not start: {e}")))
}

#[cfg(windows)]
fn list_printers() -> Result<Vec<Printer>, String> {
    let o = powershell(WIN_LIST_SCRIPT, &[])?;
    if !o.status.success() {
        return Err(bad("print_failed", String::from_utf8_lossy(&o.stderr).trim()));
    }
    parse_win_printers(&String::from_utf8_lossy(&o.stdout))
}

#[cfg(windows)]
fn send(job: &PrintJob, copies: u32, bytes: &[u8]) -> Result<String, String> {
    let dir = temp_dir()?;
    let file = dir.join("plot.png");
    let r = std::fs::write(&file, bytes).map_err(|e| bad("print_failed", &e.to_string())).and_then(|_| {
        let title = if plain_name(&job.title, 80) { job.title.clone() } else { "CadCraft plot".to_string() };
        let o = powershell(
            WIN_PRINT_SCRIPT,
            &[("CC_FILE", file.to_string_lossy().into_owned()), ("CC_PRINTER", job.printer.clone()), ("CC_COPIES", copies.to_string()), ("CC_PAPER", job.paper.clone()),
              ("CC_LANDSCAPE", if job.landscape { "1" } else { "0" }.into()), ("CC_TITLE", title)],
        )?;
        if o.status.success() { Ok("sent".into()) } else { Err(bad("print_failed", String::from_utf8_lossy(&o.stderr).trim())) }
    });
    let _ = std::fs::remove_dir_all(&dir);
    r
}

fn temp_dir() -> Result<std::path::PathBuf, String> {
    let mut b = [0u8; 8];
    getrandom_fill(&mut b);
    let d = std::env::temp_dir().join(format!("cadcraft-print-{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>()));
    std::fs::create_dir_all(&d).map_err(|e| bad("print_failed", &e.to_string()))?;
    Ok(d)
}

/// Eight bytes that differ per job: time and process id are enough for a temp folder name (it is not a secret).
fn getrandom_fill(b: &mut [u8; 8]) {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) ^ ((std::process::id() as u128) << 64);
    b.copy_from_slice(&t.to_le_bytes()[..8]);
}

#[tauri::command(async)]
pub async fn cc_printers_list() -> Result<Printers, String> {
    tauri::async_runtime::spawn_blocking(|| list_printers().map(|printers| Printers { accepts: accepts(), printers })).await.map_err(|e| e.to_string())?
}

/// Body = the bytes (a PDF, or a PNG on Windows); header `x-cc-print` = the job as JSON, percent-encoded.
#[tauri::command(async)]
pub async fn cc_print(request: Request<'_>) -> Result<String, String> {
    let job: PrintJob = request
        .headers()
        .get("x-cc-print")
        .and_then(|v| v.to_str().ok())
        .and_then(crate::files::decode_component)
        .and_then(|s| serde_json::from_str(&s).ok())
        .ok_or_else(|| bad("invalid", "missing job"))?;
    let bytes = crate::files::body_bytes(request.body()).ok_or_else(|| bad("invalid", "expected bytes"))?;
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err(bad("invalid", "nothing to print, or too big"));
    }
    let sig_ok = if cfg!(windows) { bytes.starts_with(b"\x89PNG") } else { bytes.starts_with(b"%PDF") };
    if !sig_ok {
        return Err(bad("invalid", if cfg!(windows) { "this system prints a PNG" } else { "this system prints a PDF" }));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let printers = list_printers()?;
        let copies = check_job(&job, &printers)?;
        send(&job, copies, &bytes)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, papers: &[&str]) -> Printer {
        Printer { name: name.into(), default: false, papers: papers.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn cups_output_is_read() {
        assert_eq!(parse_lpstat_e("Office_HP\nPlotter-1\n\n"), vec!["Office_HP", "Plotter-1"]);
        assert_eq!(parse_lpstat_d("system default destination: Plotter-1\n").as_deref(), Some("Plotter-1"));
        assert_eq!(parse_lpstat_d("no system default destination\n"), None);
        let l = "PageSize/Media Size: A4 *Letter Legal ARCHD\nResolution/Resolution: *600dpi\n";
        assert_eq!(parse_lpoptions_papers(l), vec!["A4", "Letter", "Legal", "ARCHD"]);
        assert!(parse_lpoptions_papers("Resolution/Resolution: *600dpi\n").is_empty());
    }

    #[test]
    fn lp_gets_arguments_not_a_shell_line() {
        let job = PrintJob { printer: "Plotter-1".into(), copies: 2, paper: "A3".into(), landscape: true, title: "Plan - A1".into() };
        let a = lp_args(&job, 2, "/tmp/x/plot.pdf");
        assert_eq!(a, ["-d", "Plotter-1", "-n", "2", "-t", "Plan - A1", "-o", "media=A3", "-o", "landscape", "-o", "fit-to-page", "--", "/tmp/x/plot.pdf"]);
        let none = PrintJob { printer: "P".into(), ..Default::default() };
        assert_eq!(lp_args(&none, 1, "f.pdf"), ["-d", "P", "-n", "1", "-o", "fit-to-page", "--", "f.pdf"]);
        // a title with shell or option characters is left out rather than passed on
        let evil = PrintJob { printer: "P".into(), title: "x; rm -rf /".into(), ..Default::default() };
        assert!(!lp_args(&evil, 1, "f.pdf").contains(&"-t".to_string()));
    }

    #[test]
    fn the_job_is_checked() {
        let list = vec![p("Office", &["A4"]), p("Plotter", &["A1"])];
        let ok = PrintJob { printer: "Plotter".into(), copies: 3, paper: "A1".into(), ..Default::default() };
        assert_eq!(check_job(&ok, &list), Ok(3));
        let unknown = PrintJob { printer: "Rm".into(), ..Default::default() };
        assert!(check_job(&unknown, &list).unwrap_err().starts_with("no_printer:"));
        for copies in [0, 100] {
            let j = PrintJob { printer: "Office".into(), copies, ..Default::default() };
            assert!(check_job(&j, &list).unwrap_err().starts_with("invalid:"), "{copies}");
        }
        let paper = PrintJob { printer: "Office".into(), paper: "A4; id".into(), ..Default::default() };
        assert!(check_job(&paper, &list).unwrap_err().starts_with("invalid:"));
        assert!(plain_name("ARCH D (24 x 36 in)", 60) && plain_name("A3", 60) && !plain_name("", 60) && !plain_name("a\nb", 60) && !plain_name("a=b", 60));
    }

    #[test]
    fn windows_list_is_read_with_one_printer_or_none() {
        let one = r#"[{"name":"Office","default":true,"papers":["A4","Letter"]}]"#;
        let got = parse_win_printers(one).unwrap();
        assert_eq!(got, vec![Printer { name: "Office".into(), default: true, papers: vec!["A4".into(), "Letter".into()] }]);
        assert!(parse_win_printers("[]").unwrap().is_empty());
        assert!(parse_win_printers("").is_err());
        let single = r#"{"name":"Solo","default":false,"papers":"A4"}"#;
        assert_eq!(parse_win_printers(single).unwrap()[0].papers, vec!["A4"]);
        // the scripts take values from the environment only
        assert!(WIN_PRINT_SCRIPT.contains("$env:CC_PRINTER") && !WIN_PRINT_SCRIPT.contains("{0}"));
    }
}
