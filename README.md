# CadCraft desktop shell

The desktop app for each CadCraft app (docs/0.4-PART2.md track W): **the app's browser edition, unchanged, in a system
webview window.** Windows uses WebView2 (part of Windows 10/11, Edge's Chromium); Linux uses WebKitGTK; macOS will use
WKWebView. The shell is Tauri 2 (Apache-2.0 OR MIT). No web server, no port, no app code in the shell.

Website: https://getcadcraft.com. Every CadCraft repo: https://github.com/getcadcraft

```
desktop-shell/
  src-tauri/            the shell (Rust): window, the cadcraft protocol, the file bridge, downloads, single instance
    src/main.rs         window, pop-ups, navigation, downloads → "Save as", second start → open in the running window
    src/protocol.rs     serves app/ from the install folder: http://cadcraft.localhost/ (Windows) · cadcraft://localhost/
    src/files.rs        window.cadcraftDesktop's commands + the grant list (only files the person chose)
    src/account.rs      sign-in with a short code (C.2): token only in the OS credential store; cloud.request → sync API
    src/updates.rs      signed updates (W.7): only with a feed + the owner's key in shell.json; always asks
    src/windows.rs      suite mode (Studio): the hub + app windows, per-app window places
    src/control.rs      Studio's local MCP server (X.5): 127.0.0.1, token, Origin/Host rules → the hub window
  patches/              our own stand-ins for crates whose license may not ship (option-ext MPL, webpki-root-certs CDLA)
  licenses.mjs          the shell's crate license gate + THIRD_PARTY.md (build.mjs runs it first)
  bridge/bridge.js      window.cadcraftDesktop, injected before the page (host.js's desktop half)
  apps.json             per app: file types (own / polite / open with)
  build.mjs             edition → stage/<slug>/ → installer in out/ (+ SHA256SUMS)
  make-icon.py          the app's icon: the CadCraft mark on its layer colour
  test/bridge.test.mjs  bridge + host.js, no webview          node test/bridge.test.mjs
  test/e2e.mjs          the real shell on WebKitGTK via WebDriver   dbus-run-session -- xvfb-run -a node test/e2e.mjs
  frontend/             a one-line page compiled in, shown only when an install has lost its app folder
```

## How it fits together

- **The app** is the browser edition the site's own packer makes (`devshell/pack.php <slug> … --edition browser`, session
  `edition` D.1), copied in as `app/` next to the executable with `shell.json` (`{app, name, version, headers}`).
  `build.mjs` packs it, so the license/secrets gate runs on exactly what ships, and refuses to build when the gate fails.
- **The origin never changes**: `http://cadcraft.localhost` on Windows, `cadcraft://localhost` elsewhere. Settings,
  recent files and autosave live in its storage.
- **Files** go through `window.cadcraftDesktop` (contract in the site's `assets/js/host.js`, which the apps call):
  native open/save/folder dialogs, `readFile`, `writeFile` (temp file + rename), `stat`, `listDir`, `launchFiles`,
  `onOpenFiles`, `info`. The shell refuses any path the person did not open, save, or pick a folder for
  (`not_allowed`); grants are remembered in the app's data folder so recent files reopen after a restart.
- **Downloads** (apps that still use `<a download>`) open a "Save as" dialog instead of landing in Downloads.
- **Links** that leave the app open in the person's browser. The app's own pop-ups (the panels window) are real windows.
- **Double-click** a file while the app runs: the second start hands the path to the running window and exits.
- **Element fullscreen** (the app bar's screen modes) takes the window fullscreen with it.
- **shell.json** (written by build.mjs): `{app, name, version, headers, suite?, account?, updates?}`. `account: false` (store
  builds until decision 11) removes sign-in and the cloud; `suite: true` is Studio; `updates` only with the owner's key.
- **Studio (suite mode):** the hub is the main window and opens each app in windows of its own
  (`cadcraftDesktop.windows.*`); files opened at launch go to the hub, which routes them. The hub can run the local MCP
  server (`cadcraftDesktop.control.*`), whose rules match `getcadcraft.com/tools/mcp/live-server.mjs`.

## Build

```
node build.mjs cadcraft                 # dev build of CadCraft → out/CadCraft-dev-setup.exe
node build.mjs all --build 0.4          # every app's 0.4 release (refuses an app whose edition fails the gate)
node build.mjs cadcraft --edition <dir> # an edition folder packed already
node build.mjs cadcraft --target linux  # unbundled Linux binary (tests)
node build.mjs studio --edition <suite> # Studio: hub + eight apps (the studio repo's pack/suite.mjs)
node build.mjs cadcraft --store --edition <store edition zip>   # store package: no sign-in, no update feed
```

Needs: Rust + `cargo tauri` + `cargo-xwin` (Windows cross-compile), NSIS (`makensis`), Python 3 + Pillow, and for
Linux/tests `libwebkit2gtk-4.1-dev`, `webkit2gtk-driver`, `xvfb`, `matchbox-window-manager`. MSI and code signing need
Windows (WiX, signtool / Azure Trusted Signing): `.github/workflows/windows.yml`.

## The site's checkout

Packing an edition, the end-to-end test and two opt-in checks need the getcadcraft.com site's own checkout, which is
not part of this repo. They find it only through environment variables, and stop with a message when one is needed
and unset:

| Variable | What it points at | Used by |
|---|---|---|
| `CADCRAFT_SITE` | the site's folder | `build.mjs` without `--edition` (its packer), `test/e2e.mjs`, `cargo test -- --ignored mcp_checker`, `test/bridge.test.mjs` (host.js) |
| `CADCRAFT_HOST_JS` | any copy of `host.js` (e.g. an edition's) | `test/bridge.test.mjs`, instead of the site's |
| `CADCRAFT_STUDIO` | the studio repo's folder | `test/e2e.mjs` (Studio checks; unset = skipped) |

With `--edition <dir|zip>`, `build.mjs` needs none of them (that is how CI builds).

## Tests

- `cargo test` (in `src-tauri/`): the protocol never leaves the app folder; grants (incl. a sheet set's folder); atomic
  writes; launch arguments; window labels; tokens. Opt-in: `cargo test -- --ignored` = a live device code over the OS's
  TLS, a credential-store round trip (needs a Secret Service on Linux), and agents' MCP checker against control.rs
  (needs `CADCRAFT_SITE`).
- `node test/bridge.test.mjs`: the bridge against a stand-in shell, and the site's `host.js` on top of it (with
  `CADCRAFT_SITE` or `CADCRAFT_HOST_JS` set; without them the host.js checks skip).
- `dbus-run-session -- xvfb-run -a node test/e2e.mjs [--signin] [--shots dir] [--fullscreen]`: the debug shell on
  WebKitGTK with a real edition and a real DWG (files, grants, downloads, layout across a restart, updates, account:false),
  Studio from `$CADCRAFT_STUDIO/out/studio-suite/vdev` (routing, windows, BroadcastChannel, places, the MCP server
  through agents' checker), and with `--signin` the live sign-in on getcadcraft.com (its test member is set up by a helper
  that stays with the site, `$CADCRAFT_SITE/tools/desktop-test/device-helper.php`).
  `CC_E2E_BIN` runs a copy of the binary (so the build can go on meanwhile). Debug builds only: dialogs take their answers
  from the file named by `CC_TEST_ANSWERS`.
- Windows itself (WebView2, the installer, Explorer's file types, SmartScreen) is checked by hand on clean Windows 10
  and 11 machines: docs W.8.
