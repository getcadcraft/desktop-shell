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
