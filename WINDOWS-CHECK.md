# Checking a desktop installer on clean Windows (W.8)

For the owner's clean Windows 10 and Windows 11 machines (VMs from a fresh image, no CadCraft ever installed). About 20
minutes per app per machine. Write each result in `docs/0.4-sessions/desktop.md` § W.8 (pass / fail + what you saw).
These are the things that can only be seen on Windows; everything else is tested here on every build.

Files: `out/<App>-<version>-setup.exe` (and `.msi`), with `out/SHA256SUMS`. Samples: the public fixtures in the site
repo `fixtures/` (e.g. `fixtures/dwg/kitchen-equipment.dwg` for CadCraft).

## 1. Download and install

1. Check the checksum: `Get-FileHash .\<App>-<version>-setup.exe` matches the line in SHA256SUMS.
2. Run the installer **as a normal user** (not an administrator). Expect: no UAC prompt (per-user install).
   - Unsigned build: SmartScreen says "Windows protected your PC" → More info → Run anyway. Note the wording.
   - Signed build (W.6): no SmartScreen warning, or "Publisher: CadCraft". Note which.
   - If WebView2 is missing (rare on 10, never on 11) the installer fetches it: note that it did.
3. Start menu has the app; Settings › Apps lists it with its version.

## 2. The app runs

4. Start it. The window opens maximized with the app, both themes work (Paper/Model), no error banner.
5. **Fullscreen**: the app bar's Full screen button (or Ctrl+Shift+F) → the whole screen, taskbar hidden; Esc → back.
6. Help (F1) opens; links that leave the app open in your browser, not inside the app.
7. PartCraft only: the status bar does not say single-threaded / "threads off" (COOP/COEP on WebView2).
8. Panels: pop the panels out to a second window (movable panels), move it, close and reopen the app (W.4b).

## 3. Files

9. **Double-click** the sample in Explorer.
   - Own types (CadCraft `.ccsheets`, ModelCraft `.modelcraft`): opens in the app.
   - `.dwg` / `.dxf` (CadCraft), `.ifc` (BimCraft), `.landxml` (CivilCraft): opens in the app **only if no other program
     had the type**; otherwise the other program still opens it and the app is under right-click › Open with.
   - `.pdf` (MarkupCraft, TakeoffCraft): your PDF reader still opens it; the app is under right-click › Open with.
10. With the app open, double-click a second file: it opens in the running window (no second window).
11. Drag a file from Explorer onto the app: it opens.
12. Edit, **Save** (Ctrl+S): the file on disk changes (check its modified time), no copy appears in Downloads.
    (Apps that do not use the host adapter yet ask where to save instead — note which.)
13. Save as → native Save dialog → the new file is where you chose. Export a PDF/CSV → native Save dialog.
14. Close and reopen: the start screen's recent files list reopens the file.

## 4. Uninstall leaves nothing behind

15. Settings › Apps › the app › Uninstall (tick "Delete the application data").
16. Gone: the install folder (`%LOCALAPPDATA%\<App>`), the Start menu entry, `%APPDATA%\com.getcadcraft.<slug>`,
    `%LOCALAPPDATA%\com.getcadcraft.<slug>` (WebView2 data), and in `regedit` under `HKCU\Software\Classes`: no
    `CadCraft.<slug>.*` keys, no `CadCraft.<slug>.*` value under any `.<ext>\OpenWithProgids`, and `.ccsheets` /
    `.dwg` defaults are back to what they were.
17. Double-click the sample again: Windows opens it as before the install.

## 5. MSI (IT departments)

18. `msiexec /i <App>-<version>.msi /qn` as an administrator → installed for all users; `msiexec /x … /qn` removes it.

## Studio with WebView2 inside (SD.2; 0.4.1 and later)

From 0.4.1 Studio carries its own web engine (RUNTIME.md). Run sections 1–4 with `CadCraft Studio-<version>-setup.exe`,
then these, on **(a) a clean Windows 10 with no WebView2 runtime** (check first: Settings › Apps has no "Microsoft Edge
WebView2 Runtime", and `HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}`
is absent) and **(b) a Windows 11**:

- W1. The installer never mentions WebView2 and never downloads anything (try it with the network unplugged on (a)).
- W2. `%LOCALAPPDATA%\CadCraft Studio\webview2\msedgewebview2.exe` exists; its version (Properties › Details) matches
  `WebView2-NOTICE.txt` beside the exe.
- W3. `icacls "%LOCALAPPDATA%\CadCraft Studio\webview2"` lists `APPLICATION PACKAGE AUTHORITY\ALL APPLICATION PACKAGES`
  and `…\ALL RESTRICTED APPLICATION PACKAGES` with `(OI)(CI)(RX)`.
- W4. Studio starts and the hub draws (no blank window); open CadCraft and PartCraft from the hub. Task Manager › Details:
  the `msedgewebview2.exe` processes run from the install folder, not `C:\Program Files (x86)\Microsoft\EdgeWebView`.
- W5. PartCraft's status bar does not say single-threaded (COOP/COEP under WebView2).
- W6. File types: double-click a `.dwg` (only if nothing else had `.dwg`), a `.ccsheets` and a `.modelcraft`: Studio
  opens each in the right app; `.pdf` is only under Open with.
- W7. Uninstall: the whole install folder is gone, `webview2\` included (nothing left in `%LOCALAPPDATA%\CadCraft Studio`).
- W8. On (a) only: after uninstalling, still no "Microsoft Edge WebView2 Runtime" in Settings › Apps (we never installed one).

Before uninstalling (W7), also:

- W9. **Shortcuts per app (SD.6):** Start menu › "CadCraft Studio" folder has CadCraft, BimCraft, ModelCraft, MarkupCraft,
  TakeoffCraft, CivilCraft, DetailCraft and PartCraft, each with its own coloured icon; typing "BimCraft" in Start finds
  it. Each opens Studio straight into that app; with Studio already open, it opens the app in the running Studio.
- W10. **Panels window (DR.5):** in CadCraft, pop the panels out to their own window, then dock the last one back to a
  side: the empty "CadCraft panels" window closes. Pop them out again and use "Put panels back": it closes too.
- W11. **Invite links:** `regedit` › `HKCU\Software\Classes\ccstudio\shell\open\command` = `"…\CadCraft Studio.exe" "%1"`.
  Type `ccstudio://join/AB3K7MNP#abcdefghijklmnopqrstuv` into the browser's address bar: the browser asks to open CadCraft
  Studio, and Studio comes forward (it does nothing more with a made-up room until Studio Live ships).
- W12. **Firewall (Studio Live):** run the installer once more with right-click › Run as administrator: Windows Defender
  Firewall › Advanced › Inbound Rules has "CadCraft Studio" (Private only, this program). A normal per-user install
  can't add it: note whether Windows asks later instead (that question comes only when Studio first hosts, in 0.5).
  After W7, the rule is gone.

**Studio replaces the separate apps (SD.8)** — on a machine with the 0.4 per-app installers (`/downloads/0.4/`), before
installing Studio:

- W13. Install CadCraft 0.4 and BimCraft 0.4. In CadCraft: open the sample `.dwg`, change Settings (e.g. Model theme),
  draw a line and leave it unsaved; close CadCraft (the recovery copy stays). Then open CadCraft again and leave it open.
- W14. Run the Studio installer. It asks once: "Remove the separate apps installed before (CadCraft, BimCraft)?" → Yes.
  CadCraft is open: "CadCraft is open… choose Retry" → close CadCraft → Retry. Both disappear from Settings › Apps and the
  Start menu; `%LOCALAPPDATA%\CadCraft` and `…\BimCraft` are gone; `%LOCALAPPDATA%\com.getcadcraft.cadcraft` is still there.
  `.dwg` now opens in Studio (if CadCraft had it). Try once with Cancel instead of Retry: CadCraft stays installed beside Studio.
- W15. First start of Studio: a short "Setting up CadCraft Studio" page, then the hub. In CadCraft (inside Studio): the
  Model theme is set, the recent files list has the sample and it reopens, and "Recover your work?" offers the unsaved line.
  `%APPDATA%\com.getcadcraft.studio\carried.json` lists cadcraft and bimcraft as done. Start Studio again: straight to the hub.
- W16. Silent: `CadCraft-Studio-<v>-setup.exe /S` with CadCraft 0.4 installed and closed removes it without asking.

## MSIX (the Microsoft Store package; 0.7)

On a clean Windows 10 (2004 or later) and a Windows 11, with the sideloaded test package from `msix/PACK.md` (a throwaway
certificate whose subject equals the manifest's Publisher, trusted once) — and again with the Store's own copy once it is live:

- M1. `Add-AppxPackage` installs without a UAC prompt (MSIX is per user). Settings › Apps lists "CadCraft Studio" with its version.
- M2. Start has **nine** entries: Studio and one tile per app (CadCraft, BimCraft, …), each with its own icon; each tile opens Studio
  straight into that app (`--app=<slug>`), and with Studio already open, into the running Studio.
- M3. The hub draws (the system's WebView2 is used: no `webview2\` folder in the package); PartCraft's status bar does not say
  single-threaded.
- M4. Files: double-click a `.dwg`, `.ifc`, `.ccsheets`: Windows asks which program the first time (a package never takes a default
  silently); "Always" → Studio opens it in the right app. `.pdf` is only under Open with.
- M5. `ccstudio://join/AB3K7MNP#abcdefghijklmnopqrstuv` in the browser's address bar: it offers to open CadCraft Studio and Studio
  comes forward.
- M6. Studio Live: host from an app; Windows Defender Firewall › Inbound rules lists the package's rules (private networks only);
  a second computer on the LAN joins without any prompt on the host.
- M7. Settings and recovery copies live in the package's private AppData: `%LOCALAPPDATA%\Packages\<package family name>\LocalCache`
  (not `%LOCALAPPDATA%\com.getcadcraft.studio`). Save a file in your Documents; uninstall from Settings › Apps: the package, its
  settings and its recovery copies are gone, **your saved file is still there**.
- M8. Settings › Updates: "Updates come from: Microsoft Store" (a Store install) or "getcadcraft.com, through Windows App
  Installer" (installed from the website's `.appinstaller`); a package installed from a bare `.msix` says it has no update source
  (`not_store`), and Check for updates says why instead of failing. Automatic checks are Windows' own for a package.
- M9. The Windows App Certification Kit report (`appcert.exe test …`): paste its failures, if any.
- M10. Updates (0.7): the website package — install from its `.appinstaller`, publish a newer version, Studio › Settings › Updates ›
  Check for updates → "Update available" → Install now: Studio closes, Windows updates the package and starts it again on the new
  version (Settings › Updates shows it). The Store package — the Store test in `docs/0.7-sessions/platforms-runsheets.md` (a private
  flight with two versions). Proven on workstation11a 2026-10-25 for the website route with a test certificate (install, hold-back,
  update + restart in 6 s, once-a-day check, sideloaded / Store path `not_store`): site `docs/results/0.7/2026-10-24-msix-updates.md`.

## Updates in the installer and the portable zip (0.7)

- U1. Studio › Settings › Updates shows the version, "Windows installer" (or "Portable"), "Updates come from: getcadcraft.com", Last
  checked and Latest version available ("You are on the latest version." when nothing is newer).
- U2. (Proven on workstation11a 2026-10-24 with test builds, as were U3 and U4.) With a newer release published: Check for updates → "Update available" → Install now → the new installer runs (the fixed
  WebView2 runtime comes with it), Studio starts again on the new version. "Not now" installs nothing and Settings keeps the
  version on offer with an Install button.
- U3. A package that doesn't match CadCraft's signature installs nothing: "The update didn't match CadCraft's signature…".
- U4. The portable zip: Check for updates tells about a newer zip and offers the download page; it never replaces its own folder.
- U5. With the network off: "Couldn't reach the update server…".
