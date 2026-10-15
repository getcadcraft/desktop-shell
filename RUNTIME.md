# The WebView2 runtime in the Windows installer (SD.2)

CadCraft Studio for Windows carries Microsoft's **WebView2 Fixed Version runtime** in its install (`webview2\` beside
`CadCraft Studio.exe`), so the installer never stops to download WebView2. The price: that runtime **never updates
itself**. Every Chromium security fix reaches people only through a new Studio installer. This page is the routine.

## The rule `build.mjs` enforces

`node build.mjs studio … --webview2-fixed <dir>` refuses a runtime older than the newest Edge Stable release of the same
major for Windows x64, read from Microsoft's own list (`https://edgeupdates.microsoft.com/api/products?view=enterprise`;
`webview2.mjs`). Microsoft publishes a Fixed Version runtime for each Stable release and keeps the newest of each recent
major for download. A newer major being out is printed as a note, not refused: its fixed runtime may lag a day or two.
No network → refused, unless `--webview2-unchecked` (offline test builds only; never publish one).

Check a runtime on its own: `node webview2.mjs <runtime folder or version>` (exit 0 current, 1 refused, 2 unreachable).

## When Chromium ships a security fix

Edge Stable gets a new version (a patch like 155.0.4283.45 → 155.0.4283.60, or a new major every four weeks). Then:

1. **Get the runtime.** Microsoft's WebView2 page (developer.microsoft.com/microsoft-edge/webview2, "Fixed Version",
   x64) → `Microsoft.WebView2.FixedVersionRuntime.<v>.x64.cab`. Keep Microsoft's file name: `build.mjs` reads the version
   from it.
2. **Archive and extract** beside the others (old ones stay, so an older installer can be rebuilt exactly):
   ```
   cd <refs>/webview2      # the runtimes archive, outside the repos
   cabextract -q Microsoft.WebView2.FixedVersionRuntime.<v>.x64.cab     # → Microsoft.WebView2.FixedVersionRuntime.<v>.x64/
   sha256sum Microsoft.WebView2.FixedVersionRuntime.<v>.x64.cab >> SHA256SUMS
   node <desktop-shell>/webview2.mjs Microsoft.WebView2.FixedVersionRuntime.<v>.x64   # must say current
   ```
3. **Rebuild Studio** with the same suite and a new patch version (the apps are unchanged; only the installer moves):
   ```
   node build.mjs studio --edition <the suite of the published release> \
     --webview2-fixed <refs>/webview2/Microsoft.WebView2.FixedVersionRuntime.<v>.x64 --version <x.y.z+1>
   ```
4. **Check** on one Windows machine: WINDOWS-CHECK.md § "Studio with WebView2 inside" (steps W1–W8; ~10 minutes).
5. **Publish** (owner's go): copy into the release's sources, `sudo -u www-data php bin/publish-downloads.php <x.y>` on
   the site; post the What's new line "Security update: the built-in web engine is now <v>".

How fast: Microsoft rates most Chromium fixes "High"; aim for a new installer within a week of a Stable release that
fixes a High or Critical bug, and within the four-week major cycle otherwise. Between releases, the installed runtime
keeps working; it just keeps the old bugs.

## What else follows the runtime

- The installer's NSIS hook grants `ALL APPLICATION PACKAGES` and `ALL RESTRICTED APPLICATION PACKAGES` read/execute on
  `webview2\` (Microsoft's requirement for runtime 120+ in an unpackaged app on Windows 10).
- `WebView2-NOTICE.txt` beside the exe names the version and says Microsoft's terms apply (not Apache-2.0).
- The updater (W.7) can't be combined with a fixed runtime (Tauri then forces the download bootstrapper); fixed-runtime
  installers update by a new installer only.
