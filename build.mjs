// SPDX-License-Identifier: Apache-2.0
// build.mjs — desktop installers (docs/0.4-PART2.md W.3/W.4/W.5): one shell build + one app's browser edition + a small
// per-app config. No app code lives in the shell; the app is copied in as `app/` beside the executable.
//
//   node build.mjs <slug…|all> [--build dev|x.y] [--edition <dir>] [--target windows|linux] [--allow-gate]
//   node build.mjs studio --edition <suite folder|zip>     Studio: the hub + eight apps in one install (suite mode, hub H.9;
//                                                          the suite comes from the studio repo's pack/suite.mjs)
//
//   --build      which app build to pack (default dev). The edition comes from the site's own packer:
//                sudo -u www-data php devshell/pack.php <slug> … --edition browser --build <b>
//                (needs CADCRAFT_SITE = the site's folder; with --edition the site is not needed)
//   --edition    use an edition already packed: a folder, or a release zip (one slug only) — how CI gets it
//   --target     windows (default: NSIS .exe cross-compiled with cargo-xwin) | linux (unbundled binary, for tests)
//                | macos (on a Mac only, S.5/S.6 as Studio: universal .app + .dmg; Studio's Info.plist keys from macos/,
//                file types with macOS ranks; signed and notarized when APPLE_* are set — NOT RUN YET, no Mac here)
//                | android (Studio only, TB.2: a debug APK to try on a tablet or the emulator; with --release an AAB for
//                Google Play, signed with the owner's upload key from CC_ANDROID_KEYSTORE, CC_ANDROID_KEYSTORE_PASSWORD,
//                CC_ANDROID_KEY_ALIAS, CC_ANDROID_KEY_PASSWORD — unset = unsigned; needs ANDROID_HOME, the NDK, Java 17)
//                | ios (Studio only, TB.3: on a Mac with Xcode — see IPAD.md; --sim = the iPad Simulator, no iPad or team needed)
//   --release    android/ios: the store build (no sign-in until decision 11, shell.json store:true) instead of a test build
//   --apk        android: the website's APK (owner, 2026-10-15: published on /download for sideloading) — a release APK with
//                sign-in like the desktop download, signed with CC_ANDROID_KEYSTORE… (refused without it: an APK signed
//                with another key could never be updated in place) → out/<Name>-<release>.apk
//   --abi        android: the Rust targets, comma-separated (default aarch64,x86_64; --release aarch64,armv7,x86_64)
//   --allow-gate build even when the license/secrets gate fails (spikes only; the result must never ship)
//   --store      a store package (S.3): shell.json account:false (no sign-in until decision 11), never an update feed
//                (the store updates it), out/<Name>-<release>-store-setup.exe. Give it the store edition with --edition.
//   --webview2-fixed <dir>   carry Microsoft's WebView2 Fixed Version runtime in the install (Windows, cross-built only):
//                <dir> = the extracted cab (Microsoft.WebView2.FixedVersionRuntime.<v>.x64). The installer never asks for
//                WebView2 and the app never uses the system's; ~250 MB more per app, and the runtime does NOT update
//                itself — rebuild with a newer runtime for Chromium security fixes. Not with the updater (Tauri then
//                forces the download bootstrapper). Refused when the runtime is older than the newest Edge Stable
//                release of its major (webview2.mjs, SD.2); the routine for a Chromium security release is in RUNTIME.md.
//   --webview2-unchecked   build even when Microsoft's release list can't be reached (offline); never publish that build
//   --version x.y.z   installer version + file name instead of the release's (e.g. 0.4.1 = the 0.4 apps, new installer)
//   --no-live    leave Studio Live out of the shell (cargo --no-default-features), e.g. for 0.4.1
//
// Out: out/<Name>-<version>-setup.exe (+ <Name>-<version>.msi when built on Windows) + out/SHA256SUMS (real checksums).
// On Linux: NSIS only, cross-compiled with cargo-xwin. On Windows (CI, .github/workflows/windows.yml): NSIS + MSI (WiX).
// Signing (W.6): set CC_SIGN_COMMAND to a command Tauri runs per file, %1 = the file (e.g. Azure Trusted Signing:
//   trusted-signing-cli -e <endpoint> -a <account> -c <profile> -d CadCraft %1). Unset = unsigned (SmartScreen warns).
// Updates (W.7, off unless all are set): CC_UPDATE_PUBKEY (the owner's updater public key) turns the feed on in
// shell.json — https://github.com/getcadcraft/<slug>/releases/latest/download/latest.json, or CC_UPDATE_ENDPOINT
// ({slug} is replaced); CC_UPDATE_AUTO=1 makes automatic checks the default (decision 10). TAURI_SIGNING_PRIVATE_KEY
// (+ _PASSWORD; CI secret only) signs the installer for the updater and writes out/<slug>-latest.json.

import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { checkRuntime, fetchFeed, runtimeVersion } from './webview2.mjs';

const HERE = path.dirname(new URL(import.meta.url).pathname);
// The site's folder, only needed to pack an edition (no --edition). No default: the site is not part of this repo.
const site = () => {
  const s = process.env.CADCRAFT_SITE;
  if (!s) throw new Error('CADCRAFT_SITE is not set: point it at the site\'s folder to pack an edition, or pass --edition <dir|zip>');
  return s;
};
const ALL = ['cadcraft', 'bimcraft', 'modelcraft', 'markupcraft', 'takeoffcraft', 'civilcraft', 'detailcraft', 'partcraft'];

const argv = process.argv.slice(2);
const opt = (k, d) => { const i = argv.indexOf(k); return i >= 0 ? argv.splice(i, 2)[1] : d; };
const flag = (k) => { const i = argv.indexOf(k); return i >= 0 ? (argv.splice(i, 1), true) : false; };
const BUILD = opt('--build', 'dev');
const EDITION = opt('--edition', null);
const TARGET = opt('--target', 'windows');
const ALLOW_GATE = flag('--allow-gate');
const STORE = flag('--store');
const WV2_FIXED = opt('--webview2-fixed', null);
const WV2_UNCHECKED = flag('--webview2-unchecked');
const VERSION = opt('--version', null);
// --no-live: build the shell without Studio Live (cargo --no-default-features): 0.4.x point releases.
const NO_LIVE = flag('--no-live');
// Tablets (docs/0.5-PART3.md track TB): Studio only.
const MOBILE = TARGET === 'android' || TARGET === 'ios';
const RELEASE = flag('--release');
// ios: --sim builds for the iPad Simulator on the Mac (Apple silicon) instead of a device: no iPad and no signing team needed.
const SIM = flag('--sim');
const SIDELOAD = flag('--apk');
if (SIDELOAD && (TARGET !== 'android' || RELEASE)) { console.error('--apk: --target android, without --release (that is the store AAB)'); process.exit(1); }
const ABI = opt('--abi', null);
const featureArgs = NO_LIVE ? ['--', '--no-default-features'] : [];
if (VERSION && !/^\d+\.\d+\.\d+$/.test(VERSION)) { console.error('--version takes x.y.z'); process.exit(1); }
if (WV2_FIXED) {
  const msedge = path.join(path.resolve(WV2_FIXED), 'msedgewebview2.exe');
  if (!fs.existsSync(msedge)) { console.error(`--webview2-fixed: no msedgewebview2.exe in ${WV2_FIXED}`); process.exit(1); }
  // On Windows the Tauri CLI adds the runtime folder to the resources itself, from a path relative to src-tauri; here we
  // add it (Linux cross-build, where the CLI does not). Wiring the Windows path is left until CI needs it.
  if (process.platform === 'win32' || TARGET !== 'windows') { console.error('--webview2-fixed: Windows installers cross-built on Linux only'); process.exit(1); }
  if (process.env.TAURI_SIGNING_PRIVATE_KEY) { console.error('--webview2-fixed: not with the updater (Tauri forces the download bootstrapper)'); process.exit(1); }
  // SD.2: never ship a runtime older than the newest of its major (webview2.mjs). The folder must be named as Microsoft's
  // cab is (…FixedVersionRuntime.<v>.x64), so the version is known. No network → refuse, unless --webview2-unchecked.
  const wv = runtimeVersion(WV2_FIXED);
  if (!wv) { console.error(`--webview2-fixed: no version in the folder name ${WV2_FIXED} (keep Microsoft's name: …FixedVersionRuntime.<v>.x64)`); process.exit(1); }
  let feed = null;
  try { feed = await fetchFeed(); } catch (e) {
    if (!WV2_UNCHECKED) { console.error(`--webview2-fixed: cannot check ${wv} against Microsoft's release list (${e.message}); --webview2-unchecked builds anyway`); process.exit(1); }
    console.warn(`!! WebView2 ${wv} NOT checked against Microsoft's release list (${e.message}): do not publish this build unchecked`);
  }
  if (feed) {
    const verdict = checkRuntime(wv, feed);
    if (!verdict.ok) { console.error(`--webview2-fixed: ${verdict.message}`); process.exit(1); }
    console.log(`== ${verdict.message}`);
  }
}
// Where the runtime lands, beside the executable; Tauri sets WEBVIEW2_BROWSER_EXECUTABLE_FOLDER to <exe dir>/<this>.
// tauri-build also wants the same path to exist relative to src-tauri, so it is a symlink there (git-ignored).
const WV2_DIR = 'webview2';
if (WV2_FIXED) {
  const link = path.join(HERE, 'src-tauri', WV2_DIR);
  fs.rmSync(link, { force: true });
  fs.symlinkSync(path.resolve(WV2_FIXED), link);
}
const slugs = argv.includes('all') ? ALL : argv;
if (slugs.includes('studio') && (slugs.length !== 1 || !EDITION)) { console.error('studio builds alone, from --edition <suite>'); process.exit(1); }
if (!slugs.length || slugs.some((s) => !ALL.includes(s) && s !== 'studio')) {
  console.error('usage: node build.mjs <slug…|all> [--build dev|x.y] [--edition <dir>] [--target windows|linux] [--allow-gate]');
  process.exit(1);
}
if (EDITION && slugs.length !== 1) { console.error('--edition takes one slug'); process.exit(1); }
if (MOBILE && (slugs.length !== 1 || slugs[0] !== 'studio')) { console.error(`--target ${TARGET}: Studio only (node build.mjs studio --edition <suite> --target ${TARGET})`); process.exit(1); }
if (TARGET === 'ios' && process.platform !== 'darwin') { console.error('--target ios builds on a Mac only (Xcode): see IPAD.md'); process.exit(1); }
if (!EDITION) try { site(); } catch (e) { console.error(e.message); process.exit(1); }

const APPS = JSON.parse(fs.readFileSync(path.join(HERE, 'apps.json'), 'utf8'));
// Studio registers every app's types once: own ones as the default, polite ones when free, the rest (.pdf too) Open with.
{
  const own = new Set(), polite = new Set(), all = new Set();
  for (const a of ALL) {
    const f = APPS[a].files;
    f.own.forEach((x) => own.add(x)); f.polite.forEach((x) => polite.add(x));
    [...f.own, ...f.polite, ...f.openWith].forEach((x) => all.add(x));
  }
  for (const x of own) polite.delete(x);
  APPS.studio = { files: { own: [...own], polite: [...polite], openWith: [...all].filter((x) => !own.has(x) && !polite.has(x)) } };
}
const SHELL_VERSION = JSON.parse(fs.readFileSync(path.join(HERE, 'src-tauri/tauri.conf.json'), 'utf8')).version;
const run = (cmd, args, o = {}) => {
  const r = spawnSync(cmd, args, { stdio: 'inherit', ...o });
  if (r.status !== 0 && !o.allowFail) throw new Error(`${cmd} ${args.join(' ')} → exit ${r.status}`);
  return r.status;
};
// The Python that draws the icons (make-icon.py needs Pillow). PYTHON picks another one; on a Mac /usr/bin/python3 is only a
// launcher, and the Python it starts may not be the one Pillow was installed for (owner's Mac mini, 2026-10-15).
const PYTHON = process.env.PYTHON || 'python3';
{
  const probe = spawnSync(PYTHON, ['-c', 'import PIL'], { stdio: 'ignore' });
  if (probe.status !== 0 && !argv.includes('--help')) {
    const exe = spawnSync(PYTHON, ['-c', 'import sys; print(sys.executable)'], { encoding: 'utf8' }).stdout?.trim() || PYTHON;
    console.error(`Pillow is missing for ${exe} (the icons need it). Install it for that Python:\n  ${exe} -m pip install --user pillow\nor point PYTHON at a Python that has it: PYTHON=/path/to/python3 node build.mjs …`);
    process.exit(1);
  }
}
const cargoEnv = { ...process.env, PATH: `${process.env.HOME}/.cargo/bin:${process.env.PATH}`, XWIN_ACCEPT_LICENSE: '1' };

/** The browser edition for one app: packed by the site's own packer (and its license/secrets gate). */
function edition(slug) {
  if (EDITION && /\.zip$/i.test(EDITION)) {
    const dir = path.join(HERE, 'stage', '_zip', slug);
    fs.rmSync(dir, { recursive: true, force: true });
    fs.mkdirSync(dir, { recursive: true });
    if (process.platform === 'win32') run('tar', ['-xf', path.resolve(EDITION), '-C', dir]);
    else run('unzip', ['-q', path.resolve(EDITION), '-d', dir]);
    const find = (d) => fs.existsSync(path.join(d, 'index.html')) && fs.existsSync(path.join(d, 'release.json')) ? d
      : fs.readdirSync(d, { withFileTypes: true }).filter((e) => e.isDirectory()).map((e) => find(path.join(d, e.name))).find(Boolean);
    const root = find(dir);
    if (!root) throw new Error(`${slug}: no edition (index.html + release.json) in ${EDITION}`);
    return root;
  }
  if (EDITION) return path.resolve(EDITION);
  const SITE = site();
  const out = path.join(SITE, 'tmp/desktop-editions', slug);
  const status = run('sudo', ['-u', 'www-data', 'php', path.join(SITE, 'devshell/pack.php'), slug, out, '--edition', 'browser', '--build', BUILD, '--force'], { allowFail: true });
  if (status === 2 && !ALLOW_GATE) throw new Error(`${slug}: the license/secrets gate failed — not building an installer from it`);
  if (status !== 0 && status !== 2) throw new Error(`${slug}: pack failed (${status})`);
  const dir = path.join(out, 'browser-edition', `v${BUILD}`);
  if (!fs.existsSync(path.join(dir, 'index.html'))) throw new Error(`${slug}: no edition at ${dir}`);
  return dir;
}

/** Version for the installer: a release "0.4" → 0.4.0; dev builds carry the shell's version and say "dev" in the file name. */
const semver = (release) => (/^\d+\.\d+(\.\d+)?$/.test(release) ? (release.split('.').length === 2 ? `${release}.0` : release) : SHELL_VERSION);

/** NSIS hook, Studio only (docs/0.5-PLAN.md SD.8): before Studio's files go in, the separate per-app installs it
 *  replaces (0.4: CadCraft-0.4-setup.exe …) are found and, after one question, removed with their own uninstaller,
 *  silently. Only ours: the uninstall entry's Publisher is CadCraft and the folder holds shell.json. Their data stays
 *  (a silent Tauri uninstall never ticks "Delete the application data"): Studio carries it over at its first start
 *  (src-tauri/src/carry.rs; checked by test/e2e.mjs on Linux). An app that is open is never closed for the person: they close it and retry, or it stays.
 *  Silent (/S) and passive (/P) installs remove without asking and skip open apps. Runs before POSTINSTALL, so the old
 *  apps give back .dwg/.ifc/… before Studio's polite types look for a free default. */
function replaceHooks(names) {
  const U = 'Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall';
  const L = ['Var CcOldDir', 'Var CcOldNames', 'Var CcOldRun'];
  // $CcOldDir = the folder of our own per-app install of NAME, else "".
  L.push('!macro CcOldFind NAME',
    '  StrCpy $CcOldDir ""',
    `  ReadRegStr $R0 HKCU "${U}\\\${NAME}" "Publisher"`,
    `  ReadRegStr $R1 HKCU "${U}\\\${NAME}" "InstallLocation"`,
    '  StrCpy $R2 $R1 1',
    "  ${If} $R2 == '\"'",
    '    StrCpy $R1 $R1 -1 1',
    '  ${EndIf}',
    '  ${If} $R0 == "CadCraft"',
    '  ${AndIf} $R1 != ""',
    '  ${AndIf} $R1 != $INSTDIR',
    '  ${AndIf} ${FileExists} "$R1\\shell.json"',
    '  ${AndIf} ${FileExists} "$R1\\uninstall.exe"',
    '    StrCpy $CcOldDir $R1',
    '  ${EndIf}',
    '!macroend');
  // $CcOldRun = 1 when EXE is running (the Restart Manager check Tauri's own CheckIfAppIsRunning uses, without the kill).
  L.push('!macro CcOldRunning EXE',
    '  StrCpy $CcOldRun 0',
    '  !insertmacro RestartManager_StartSession $R0',
    '  ${If} $R0 != ""',
    '    !insertmacro RestartManager_RegisterFile $R0 "${EXE}"',
    '    ${If} $0 = 0',
    "      System::Call 'RSTRTMGR::RmGetList(p R0, *i .r1, *i .r2, p 0, *i .r3) i .r0'",
    '      ${If} $0 = ${ERROR_MORE_DATA}',
    '        StrCpy $CcOldRun 1',
    '      ${EndIf}',
    '    ${EndIf}',
    '    !insertmacro RestartManager_EndSession $R0',
    '  ${EndIf}',
    '!macroend');
  // _?= runs the uninstaller in place and waits for it (otherwise it copies itself to TEMP and returns at once).
  L.push('!macro CcOldRemove NAME',
    '  !insertmacro CcOldFind "${NAME}"',
    '  ${If} $CcOldDir != ""',
    '    cc_old_check_${NAME}:',
    '    !insertmacro CcOldRunning "$CcOldDir\\${NAME}.exe"',
    '    ${If} $CcOldRun = 1',
    '      IfSilent cc_old_skip_${NAME}',
    '      StrCmp $PassiveMode 1 cc_old_skip_${NAME}',
    '      MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "${NAME} is open. Save your work and close ${NAME}, then choose Retry.$\\r$\\n$\\r$\\nCancel keeps ${NAME} installed beside Studio." IDRETRY cc_old_check_${NAME}',
    '      Goto cc_old_skip_${NAME}',
    '    ${EndIf}',
    '    DetailPrint "Removing ${NAME}: it is part of CadCraft Studio now"',
    "    ExecWait '\"$CcOldDir\\uninstall.exe\" /S _?=$CcOldDir' $R0",
    '    ${If} $R0 = 0',
    '      Delete "$CcOldDir\\uninstall.exe"',
    '      RMDir "$CcOldDir"',
    '    ${EndIf}',
    '    cc_old_skip_${NAME}:',
    '  ${EndIf}',
    '!macroend');
  L.push('!macro NSIS_HOOK_PREINSTALL');
  for (const r of ['$0', '$1', '$2', '$3', '$R0', '$R1', '$R2']) L.push(`  Push ${r}`);
  L.push('  StrCpy $CcOldNames ""');
  for (const n of names) {
    L.push(`  !insertmacro CcOldFind "${n}"`, '  ${If} $CcOldDir != ""', `    StrCpy $CcOldNames "$CcOldNames, ${n}"`, '  ${EndIf}');
  }
  L.push('  ${If} $CcOldNames != ""',
    '    StrCpy $CcOldNames $CcOldNames "" 2',
    '    IfSilent cc_old_go',
    '    StrCmp $PassiveMode 1 cc_old_go',
    '    MessageBox MB_YESNO|MB_ICONQUESTION "CadCraft Studio has all eight apps in one install.$\\r$\\n$\\r$\\nRemove the separate apps installed before ($CcOldNames)? Your settings, recent files and recovery copies come over to Studio." IDYES cc_old_go',
    '    Goto cc_old_done',
    '    cc_old_go:');
  for (const n of names) L.push(`    !insertmacro CcOldRemove "${n}"`);
  L.push('    cc_old_done:', '  ${EndIf}');
  for (const r of ['$R2', '$R1', '$R0', '$3', '$2', '$1', '$0']) L.push(`  Pop ${r}`);
  L.push('!macroend');
  return L;
}

/** NSIS hook: polite file associations (W.4). Every type → "Open with"; `own` types → our default; `polite` types → our
 *  default only when nothing else claimed them. Uninstall takes back only what is still ours. */
function hooks(slug, name, exe, apps = []) {
  const f = APPS[slug].files;
  const types = APPS._types;
  const all = [...f.own, ...f.polite, ...f.openWith];
  const cls = (ext) => `CadCraft.${slug}.${ext}`;
  const L = [];
  L.push('; Generated by desktop-shell/build.mjs — file types for ' + name + ' (docs/0.4-PART2.md W.4).');
  // SD.8 (Studio): the separate 0.4 apps it replaces are removed first (after one question), before POSTINSTALL claims types.
  if (apps.length) L.push(...replaceHooks(apps.map((a) => a.name)));
  L.push('!macro NSIS_HOOK_POSTINSTALL');
  L.push(`  WriteRegStr SHCTX "Software\\Classes\\Applications\\${exe}.exe" "FriendlyAppName" "${name}"`);
  L.push(`  WriteRegStr SHCTX "Software\\Classes\\Applications\\${exe}.exe\\shell\\open\\command" "" '"$INSTDIR\\${exe}.exe" "%1"'`);
  for (const ext of all) {
    L.push(`  WriteRegStr SHCTX "Software\\Classes\\${cls(ext)}" "" "${types[ext] || ext.toUpperCase() + ' file'}"`);
    L.push(`  WriteRegStr SHCTX "Software\\Classes\\${cls(ext)}\\DefaultIcon" "" '"$INSTDIR\\${exe}.exe",0'`);
    L.push(`  WriteRegStr SHCTX "Software\\Classes\\${cls(ext)}\\shell\\open" "" "Open with ${name}"`);
    L.push(`  WriteRegStr SHCTX "Software\\Classes\\${cls(ext)}\\shell\\open\\command" "" '"$INSTDIR\\${exe}.exe" "%1"'`);
    L.push(`  WriteRegStr SHCTX "Software\\Classes\\.${ext}\\OpenWithProgids" "${cls(ext)}" ""`);
    L.push(`  WriteRegStr SHCTX "Software\\Classes\\Applications\\${exe}.exe\\SupportedTypes" ".${ext}" ""`);
  }
  for (const ext of f.own) L.push(`  WriteRegStr SHCTX "Software\\Classes\\.${ext}" "" "${cls(ext)}"`);
  for (const ext of f.polite) {
    L.push(`  ReadRegStr $R0 HKCR ".${ext}" ""`);
    L.push(`  StrCmp $R0 "" 0 +2`);
    L.push(`    WriteRegStr SHCTX "Software\\Classes\\.${ext}" "" "${cls(ext)}"`);
  }
  if (slug === 'studio') {
    // ccstudio:// links (LV.25: "Open in Studio" on getcadcraft.com/live/<room>): the browser starts Studio with the link.
    L.push(`  WriteRegStr SHCTX "Software\\Classes\\ccstudio" "" "URL:${name} link"`);
    L.push(`  WriteRegStr SHCTX "Software\\Classes\\ccstudio" "URL Protocol" ""`);
    L.push(`  WriteRegStr SHCTX "Software\\Classes\\ccstudio\\DefaultIcon" "" '"$INSTDIR\\${exe}.exe",0'`);
    L.push(`  WriteRegStr SHCTX "Software\\Classes\\ccstudio\\shell\\open\\command" "" '"$INSTDIR\\${exe}.exe" "%1"'`);
    // Studio Live (LV.13): let Studio host on private networks only, for this program only (no port: the session
    // server binds a free one). Needs an elevated installer; a per-user install isn't, so this fails quietly and
    // Windows asks once instead, the first time Studio hosts ("allow on private networks").
    L.push(`  nsExec::Exec 'netsh advfirewall firewall delete rule name="${name}" program="$INSTDIR\\${exe}.exe"'`);
    L.push('  Pop $R0');
    L.push(`  nsExec::Exec 'netsh advfirewall firewall add rule name="${name}" dir=in action=allow program="$INSTDIR\\${exe}.exe" profile=private enable=yes'`);
    L.push('  Pop $R0');
  }
  if (apps.length) {
    // SD.6: a Start menu shortcut per app ("CadCraft", "BimCraft", …) that opens Studio straight into it, in a folder of
    // its own so it never collides with a 0.4 per-app install's own shortcut.
    L.push(`  CreateDirectory "$SMPROGRAMS\\${name}"`);
    for (const a of apps) L.push(`  CreateShortCut "$SMPROGRAMS\\${name}\\${a.name}.lnk" "$INSTDIR\\${exe}.exe" "--app=${a.slug}" "$INSTDIR\\appicons\\${a.slug}.ico" 0`);
  }
  L.push(`  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'`);
  if (WV2_FIXED) {
    // Microsoft (WebView2 distribution, Fixed Version): on Windows 10, runtime 120+ in an unpackaged app needs these two
    // grants on its folder, or the web view's sandboxed processes can't read it.
    L.push(`  nsExec::Exec 'icacls "$INSTDIR\\${WV2_DIR}" /grant *S-1-15-2-2:(OI)(CI)(RX)'`);
    L.push('  Pop $R0');
    L.push(`  nsExec::Exec 'icacls "$INSTDIR\\${WV2_DIR}" /grant *S-1-15-2-1:(OI)(CI)(RX)'`);
    L.push('  Pop $R0');
  }
  L.push('!macroend');
  L.push('!macro NSIS_HOOK_PREUNINSTALL');
  for (const ext of all) {
    L.push(`  ReadRegStr $R0 SHCTX "Software\\Classes\\.${ext}" ""`);
    L.push(`  StrCmp $R0 "${cls(ext)}" 0 +2`);
    L.push(`    DeleteRegValue SHCTX "Software\\Classes\\.${ext}" ""`);
    L.push(`  DeleteRegValue SHCTX "Software\\Classes\\.${ext}\\OpenWithProgids" "${cls(ext)}"`);
    L.push(`  DeleteRegKey /ifempty SHCTX "Software\\Classes\\.${ext}\\OpenWithProgids"`);
    L.push(`  DeleteRegKey /ifempty SHCTX "Software\\Classes\\.${ext}"`);
    L.push(`  DeleteRegKey SHCTX "Software\\Classes\\${cls(ext)}"`);
  }
  L.push(`  DeleteRegKey SHCTX "Software\\Classes\\Applications\\${exe}.exe"`);
  if (apps.length) {
    for (const a of apps) L.push(`  Delete "$SMPROGRAMS\\${name}\\${a.name}.lnk"`);
    L.push(`  RMDir "$SMPROGRAMS\\${name}"`);   // only when empty
  }
  if (slug === 'studio') {
    // The firewall rule (only there if the install was elevated) and any rule Windows' own prompt made for this program.
    L.push(`  nsExec::Exec 'netsh advfirewall firewall delete rule name=all program="$INSTDIR\\${exe}.exe"'`);
    L.push('  Pop $R0');
  }
  if (slug === 'studio') {   // only while it is still ours (another Studio install may have taken it over)
    L.push(`  ReadRegStr $R0 SHCTX "Software\\Classes\\ccstudio\\shell\\open\\command" ""`);
    L.push(`  StrCmp $R0 '"$INSTDIR\\${exe}.exe" "%1"' 0 +2`);
    L.push(`    DeleteRegKey SHCTX "Software\\Classes\\ccstudio"`);
  }
  L.push(`  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'`);
  L.push('!macroend');
  return L.join('\r\n') + '\r\n';
}

/** iPad (TB.3, on a Mac with Xcode; IPAD.md): the suite and shell.json as the app bundle's resources (plain files there,
 *  read by protocol.rs like the desktop's app/ folder), the Xcode project in src-tauri/gen/apple (made once on the Mac by
 *  `cargo tauri ios init`, then committed). Test build: a development-signed build for a device; --release: the App Store
 *  Connect export (TestFlight). Signing: the owner's team (APPLE_DEVELOPMENT_TEAM, or the project's own setting). */
function buildIos({ slug, ed, name, release, version, fileVersion, stage, tauriDir }) {
  if (!fs.existsSync(path.join(tauriDir, 'gen/apple'))) throw new Error('no src-tauri/gen/apple: run `cargo tauri ios init` once on the Mac and commit it (IPAD.md)');
  fs.mkdirSync(stage, { recursive: true });
  const app = path.join(stage, 'app');
  if (!fs.existsSync(path.join(app, 'index.html'))) fs.cpSync(ed, app, { recursive: true, dereference: true });
  for (const f of ['.htaccess', 'web.config', 'INSTALL.md', 'sw.js']) fs.rmSync(path.join(app, f), { force: true });
  const manifests = fs.readdirSync(path.join(app, 'apps')).flatMap((a) => {
    const d = path.join(app, 'apps', a, 'assets/apps', a);
    return fs.existsSync(d) ? fs.readdirSync(d).map((v) => path.join(d, v, 'manifest.json')).filter((f) => fs.existsSync(f)) : [];
  });
  const isolate = manifests.map((f) => JSON.parse(fs.readFileSync(f, 'utf8')).isolate).find(Boolean);
  const headers = isolate ? { 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': isolate } : {};
  fs.writeFileSync(path.join(stage, 'shell.json'), JSON.stringify({ app: slug, name, version: release, headers, suite: true,
    ...(RELEASE ? { store: true, account: false } : {}) }, null, 2));
  const rel2 = (p) => path.relative(tauriDir, p);
  const cfgFile = path.join(stage, 'tauri.override.json');
  fs.writeFileSync(cfgFile, JSON.stringify({ version, bundle: { resources: {
    [rel2(app) + '/']: 'app/', [rel2(path.join(stage, 'shell.json'))]: 'shell.json',
    [rel2(THIRD_PARTY)]: 'THIRD_PARTY.md', [rel2(path.join(HERE, 'LICENSE'))]: 'LICENSE',
  } } }, null, 2));
  run(PYTHON, [path.join(HERE, 'make-icon.py'), slug, path.join(stage, 'icon.png')]);
  run('cargo', ['tauri', 'icon', path.join(stage, 'icon.png'), '-o', path.join(stage, 'icons')], { cwd: tauriDir, env: cargoEnv, stdio: 'ignore' });
  const iosIcons = path.join(stage, 'icons/ios');
  const appIcon = path.join(tauriDir, 'gen/apple/Assets.xcassets/AppIcon.appiconset');
  if (fs.existsSync(iosIcons) && fs.existsSync(appIcon)) for (const f of fs.readdirSync(iosIcons)) fs.copyFileSync(path.join(iosIcons, f), path.join(appIcon, f));
  if (SIM) {
    // The iPad Simulator (owner, 2026-10-15: no iPad to test on). A debug build for aarch64-sim; the .app goes to out/ and is
    // installed with simctl. If this Tauri CLI can't make a simulator build, `cargo tauri ios dev` with the same config runs it.
    run('cargo', ['tauri', 'ios', 'build', '--debug', '--target', 'aarch64-sim', '--config', cfgFile, ...featureArgs], { cwd: HERE, env: cargoEnv, allowFail: true });
    const apps = [];
    const walk = (d, depth = 0) => {
      if (!fs.existsSync(d) || depth > 6) return;
      for (const e of fs.readdirSync(d, { withFileTypes: true })) {
        const p = path.join(d, e.name);
        if (e.isDirectory() && e.name.endsWith('.app')) apps.push(p); else if (e.isDirectory()) walk(p, depth + 1);
      }
    };
    walk(path.join(tauriDir, 'gen/apple/build'));
    const built = apps.filter((a) => /sim/i.test(a)).sort((a, b) => fs.statSync(b).mtimeMs - fs.statSync(a).mtimeMs)[0];
    if (!built) {
      console.error(`No simulator .app found. Run it on the simulator directly instead:\n  cargo tauri ios dev --config ${cfgFile} "iPad Pro 13-inch (M4)"\n(list simulators: xcrun simctl list devices available | grep iPad)`);
      throw new Error(`${slug}: no simulator build`);
    }
    const dest = path.join(HERE, 'out', `${name.replace(/ /g, '-')}-${fileVersion}-ipad-simulator.app`);
    fs.rmSync(dest, { recursive: true, force: true });
    fs.cpSync(built, dest, { recursive: true });
    console.log(`   → ${path.relative(HERE, dest)}\n   Run it:  open -a Simulator;  xcrun simctl boot "iPad Pro 13-inch (M4)";  xcrun simctl install booted "${dest}";  xcrun simctl launch booted com.getcadcraft.studio`);
    return;
  }
  run('cargo', ['tauri', 'ios', 'build', ...(RELEASE ? ['--export-method', 'app-store-connect'] : ['--debug']), '--target', 'aarch64',
    '--config', cfgFile, ...featureArgs], { cwd: HERE, env: cargoEnv });
  const outDir = path.join(tauriDir, 'gen/apple/build/arm64');
  const ipa = fs.existsSync(outDir) ? fs.readdirSync(outDir).filter((f) => f.endsWith('.ipa')).map((f) => path.join(outDir, f))[0] : null;
  if (!ipa) throw new Error(`${slug}: no .ipa under ${outDir}`);
  const dest = path.join(HERE, 'out', `${name.replace(/ /g, '-')}-${fileVersion}${RELEASE ? '' : '-ipad-test'}.ipa`);
  fs.copyFileSync(ipa, dest);
  console.log(`   → ${path.relative(HERE, dest)} (${(fs.statSync(dest).size / 1048576).toFixed(1)} MB)`);
}

/** The Android SDK, its NDK and Java 17 for `cargo tauri android build`. */
function androidEnv() {
  const home = process.env.ANDROID_HOME || process.env.ANDROID_SDK_ROOT || (fs.existsSync('/opt/android-sdk') ? '/opt/android-sdk' : '');
  if (!home) throw new Error('ANDROID_HOME is not set (the Android SDK)');
  const ndks = path.join(home, 'ndk');
  const ndk = process.env.NDK_HOME || (fs.existsSync(ndks) ? path.join(ndks, fs.readdirSync(ndks).sort().pop()) : '');
  if (!ndk) throw new Error('NDK_HOME is not set (the Android NDK)');
  let java = process.env.JAVA_HOME || '';
  if (!java) {
    const r = spawnSync('sh', ['-c', 'readlink -f "$(command -v java)"'], { encoding: 'utf8' });
    if (r.status === 0 && r.stdout.trim()) java = path.dirname(path.dirname(r.stdout.trim()));
  }
  return { ...cargoEnv, ANDROID_HOME: home, NDK_HOME: ndk, ...(java ? { JAVA_HOME: java } : {}) };
}

/** Tablets (TB.2 Android, TB.3 iPad): Studio's suite as the app's own files, one window, the stores' builds. Android: the
 *  suite goes into the APK's assets (read in place, protocol.rs), the Gradle project is src-tauri/gen/android (committed). */
function buildMobile({ slug, ed, rel, name, release, version, fileVersion, stage }) {
  const tauriDir = path.join(HERE, 'src-tauri');
  if (TARGET === 'ios') return buildIos({ slug, ed, name, release, version, fileVersion, stage, tauriDir });
  const gen = path.join(tauriDir, 'gen/android');
  if (!fs.existsSync(path.join(gen, 'app/build.gradle.kts'))) throw new Error('no src-tauri/gen/android: it is committed with the shell (cargo tauri android init made it)');
  // The Kotlin half of rustls-platform-verifier is vendored (android/maven): its version must be the Rust crate's.
  const lock = fs.readFileSync(path.join(tauriDir, 'Cargo.lock'), 'utf8');
  const want = lock.match(/name = "rustls-platform-verifier-android"\nversion = "([^"]+)"/)?.[1];
  const aar = want && path.join(HERE, 'android/maven/org/rustls/rustls-platform-verifier', want, `rustls-platform-verifier-${want}.aar`);
  if (!want || !fs.existsSync(aar)) throw new Error(`android/maven has no rustls-platform-verifier ${want} (Cargo.lock): see android/README.md`);
  const gradleApp = fs.readFileSync(path.join(gen, 'app/build.gradle.kts'), 'utf8');
  if (!gradleApp.includes(`org.rustls:rustls-platform-verifier:${want}`)) throw new Error(`gen/android/app/build.gradle.kts must name rustls-platform-verifier ${want}`);

  // The suite and its shell.json as assets; a store build has no sign-in until decision 11 (as the desktop's --store).
  const assets = path.join(gen, 'app/src/main/assets');
  fs.rmSync(path.join(assets, 'app'), { recursive: true, force: true });
  fs.mkdirSync(assets, { recursive: true });
  fs.cpSync(ed, path.join(assets, 'app'), { recursive: true, dereference: true });
  for (const f of ['.htaccess', 'web.config', 'INSTALL.md', 'sw.js']) fs.rmSync(path.join(assets, 'app', f), { force: true });
  const manifests = fs.readdirSync(path.join(assets, 'app/apps')).flatMap((a) => {
    const d = path.join(assets, 'app/apps', a, 'assets/apps', a);
    return fs.existsSync(d) ? fs.readdirSync(d).map((v) => path.join(d, v, 'manifest.json')).filter((f) => fs.existsSync(f)) : [];
  });
  const isolate = manifests.map((f) => JSON.parse(fs.readFileSync(f, 'utf8')).isolate).find(Boolean);
  const headers = isolate ? { 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': isolate } : {};
  fs.writeFileSync(path.join(assets, 'shell.json'), JSON.stringify({ app: slug, name, version: release, headers, suite: true,
    ...(RELEASE ? { store: true, account: false } : {}) }, null, 2));
  fs.copyFileSync(THIRD_PARTY, path.join(assets, 'THIRD_PARTY.md'));
  fs.copyFileSync(path.join(HERE, 'LICENSE'), path.join(assets, 'LICENSE'));

  // Icons: the CadCraft mark on Studio's ink, as Android's launcher icons.
  fs.mkdirSync(stage, { recursive: true });
  run(PYTHON, [path.join(HERE, 'make-icon.py'), slug, path.join(stage, 'icon.png')]);
  run('cargo', ['tauri', 'icon', path.join(stage, 'icon.png'), '-o', path.join(stage, 'icons')], { cwd: tauriDir, env: cargoEnv, stdio: 'ignore' });
  const res = path.join(stage, 'icons/android');
  if (fs.existsSync(res)) fs.cpSync(res, path.join(gen, 'app/src/main/res'), { recursive: true });

  const cfgFile = path.join(stage, 'tauri.override.json');
  fs.writeFileSync(cfgFile, JSON.stringify({ version }, null, 2));
  // The owner's upload key (release only), for the Gradle signing config; removed again whatever happens.
  const keyFile = path.join(gen, 'keystore.properties');
  const k = process.env;
  if (SIDELOAD && !k.CC_ANDROID_KEYSTORE) throw new Error('--apk needs the release key: CC_ANDROID_KEYSTORE, _PASSWORD, CC_ANDROID_KEY_ALIAS, CC_ANDROID_KEY_PASSWORD');
  if ((RELEASE || SIDELOAD) && k.CC_ANDROID_KEYSTORE) {
    for (const v of ['CC_ANDROID_KEYSTORE_PASSWORD', 'CC_ANDROID_KEY_ALIAS', 'CC_ANDROID_KEY_PASSWORD']) if (!k[v]) throw new Error(`CC_ANDROID_KEYSTORE needs ${v} too`);
    const esc = (v) => String(v).replace(/\\/g, '\\\\');
    fs.writeFileSync(keyFile, `storeFile=${esc(path.resolve(k.CC_ANDROID_KEYSTORE))}\nstorePassword=${esc(k.CC_ANDROID_KEYSTORE_PASSWORD)}\nkeyAlias=${esc(k.CC_ANDROID_KEY_ALIAS)}\nkeyPassword=${esc(k.CC_ANDROID_KEY_PASSWORD)}\n`, { mode: 0o600 });
  }
  const signed = (RELEASE || SIDELOAD) && fs.existsSync(keyFile);
  // Gradle's incremental packaging rewrites a debug APK in place and leaves the old bytes as dead space: start clean.
  fs.rmSync(path.join(gen, 'app/build/outputs'), { recursive: true, force: true });
  fs.rmSync(path.join(gen, 'app/build/intermediates/apk'), { recursive: true, force: true });
  const abis = (ABI || (RELEASE || SIDELOAD ? 'aarch64,armv7,x86_64' : 'aarch64,x86_64')).split(',').map((x) => x.trim()).filter(Boolean);
  try {
    run('cargo', ['tauri', 'android', 'build', ...(RELEASE ? ['--aab'] : SIDELOAD ? ['--apk'] : ['--debug', '--apk']), ...abis.flatMap((a) => ['--target', a]),
      '--config', cfgFile, ...featureArgs], { cwd: HERE, env: {
      ...androidEnv(),
      // The test APK: Gradle's debug variant (debug-signed, installable next to nothing else), but the Rust library
      // optimized and without debug info — ~200 MB per ABI otherwise, and a tablet should feel like the real thing.
      ...(RELEASE || SIDELOAD ? {} : { CARGO_PROFILE_DEV_OPT_LEVEL: 's', CARGO_PROFILE_DEV_DEBUG: '0', CARGO_PROFILE_DEV_STRIP: 'symbols', CARGO_PROFILE_DEV_INCREMENTAL: 'false' }),
    } });
  } finally {
    fs.rmSync(keyFile, { force: true });
  }
  const outs = path.join(gen, 'app/build/outputs');
  const built = RELEASE ? path.join(outs, 'bundle/universalRelease/app-universal-release.aab')
    : path.join(outs, SIDELOAD ? 'apk/universal/release/app-universal-release.apk' : 'apk/universal/debug/app-universal-debug.apk');
  if (!fs.existsSync(built)) throw new Error(`${slug}: nothing at ${built}`);
  const base = `${name.replace(/ /g, '-')}-${fileVersion}`;
  const file = RELEASE ? `${base}${signed ? '' : '-unsigned'}.aab` : SIDELOAD ? `${base}.apk` : `${base}-android-test.apk`;
  const dest = path.join(HERE, 'out', file);
  fs.copyFileSync(built, dest);
  console.log(`   → ${path.relative(HERE, dest)} (${(fs.statSync(dest).size / 1048576).toFixed(1)} MB)${RELEASE && !signed ? ' — unsigned: Google Play needs the owner\'s upload key (CC_ANDROID_KEYSTORE…)' : ''}`);
}

// The shell's own license gate (its Rust crates) before anything is built; writes THIRD_PARTY.md for the install.
if (TARGET === 'macos' && process.platform !== 'darwin') { console.error('--target macos builds on a Mac only (Apple\'s tools)'); process.exit(1); }
const triple = { windows: 'x86_64-pc-windows-msvc', macos: 'aarch64-apple-darwin', android: 'aarch64-linux-android', ios: 'aarch64-apple-ios' }[TARGET] || 'x86_64-unknown-linux-gnu';
// Tablets: their own crate list (rustls, the platform verifier, the NDK glue), beside the build, not over the desktop's file.
const THIRD_PARTY = MOBILE ? path.join(HERE, 'stage', `THIRD_PARTY-${TARGET}.md`) : path.join(HERE, 'THIRD_PARTY.md');
if (MOBILE) fs.mkdirSync(path.dirname(THIRD_PARTY), { recursive: true });
run('node', [path.join(HERE, 'licenses.mjs'), '--target', triple, '--out', THIRD_PARTY, ...(NO_LIVE ? ['--no-default-features'] : [])]);

const sha256 = (file) => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
fs.mkdirSync(path.join(HERE, 'out'), { recursive: true });

const failed = [];
for (const slug of slugs) {
  try {
  const ed = edition(slug);
  const rel = JSON.parse(fs.readFileSync(path.join(ed, 'release.json'), 'utf8'));
  const name = rel.name;
  const release = String(rel.release);
  const version = VERSION || semver(release);
  const fileVersion = VERSION || release; // the name on the installer: CadCraft-0.4-setup.exe, CadCraft-0.4.1-setup.exe
  const exe = name; // CadCraft.exe, BimCraft.exe, …
  console.log(`== ${slug}: ${name} ${release} (installer version ${version})`);

  // Stage: app/ + shell.json + icons + hooks + the Tauri config merged over src-tauri/tauri.conf.json.
  const stage = path.join(HERE, 'stage', slug);
  fs.rmSync(stage, { recursive: true, force: true });
  fs.mkdirSync(stage, { recursive: true });
  fs.cpSync(ed, path.join(stage, 'app'), { recursive: true, dereference: true });
  // Web-server files mean nothing inside the desktop app (the service worker is refused on the shell's origin anyway).
  for (const f of ['.htaccess', 'web.config', 'INSTALL.md', 'sw.js']) fs.rmSync(path.join(stage, 'app', f), { force: true });
  const suite = rel.format === 'cadcraft-suite';
  if (suite !== (slug === 'studio')) throw new Error(`${slug}: ${suite ? 'a suite folder needs the slug studio' : 'studio needs a suite folder'}`);
  if (MOBILE) { buildMobile({ slug, ed, rel, name, release, version, fileVersion, stage }); continue; }
  // Headers for the whole origin: one app's manifest, or (Studio) any of the suite's apps that wants isolation.
  const manifests = suite
    ? fs.readdirSync(path.join(stage, 'app/apps')).flatMap((a) => {
      const d = path.join(stage, 'app/apps', a, 'assets/apps', a);
      return fs.existsSync(d) ? fs.readdirSync(d).map((v) => path.join(d, v, 'manifest.json')).filter((f) => fs.existsSync(f)) : [];
    })
    : [path.join(stage, 'app/assets/apps', slug, release, 'manifest.json')];
  const isolate = manifests.map((f) => JSON.parse(fs.readFileSync(f, 'utf8')).isolate).find(Boolean);
  const headers = isolate ? { 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': isolate } : {};
  const updates = !STORE && process.env.CC_UPDATE_PUBKEY ? {
    endpoint: (process.env.CC_UPDATE_ENDPOINT || 'https://github.com/getcadcraft/{slug}/releases/latest/download/latest.json').replaceAll('{slug}', slug),
    pubkey: process.env.CC_UPDATE_PUBKEY,
    auto: process.env.CC_UPDATE_AUTO === '1',
  } : undefined;
  const account = STORE ? false : undefined;
  fs.writeFileSync(path.join(stage, 'shell.json'), JSON.stringify({ app: slug, name, version: release, headers, updates, account, suite: suite || undefined, store: STORE || undefined }, null, 2));
  run(PYTHON, [path.join(HERE, 'make-icon.py'), slug, path.join(stage, 'icon.png')]);
  run('cargo', ['tauri', 'icon', path.join(stage, 'icon.png'), '-o', path.join(stage, 'icons')], { cwd: path.join(HERE, 'src-tauri'), env: cargoEnv, stdio: 'ignore' });
  // Studio: each app's own icon for its Start menu shortcut (SD.6), from the suite's app list.
  const suiteApps = suite ? (rel.apps || []).filter((a) => ALL.includes(a.slug)) : [];
  if (suiteApps.length) {
    fs.mkdirSync(path.join(stage, 'appicons'), { recursive: true });
    for (const a of suiteApps) {
      const png = path.join(stage, 'appicons', `${a.slug}.png`);
      run(PYTHON, [path.join(HERE, 'make-icon.py'), a.slug, png]);
      run('python3', ['-c', 'import sys; from PIL import Image; Image.open(sys.argv[1]).save(sys.argv[2], sizes=[(16,16),(24,24),(32,32),(48,48),(64,64),(128,128),(256,256)])', png, png.replace(/\.png$/, '.ico')]);
      fs.rmSync(png);
    }
  }
  fs.writeFileSync(path.join(stage, 'hooks.nsh'), hooks(slug, name, exe, suiteApps));
  if (WV2_FIXED) {
    const wv = path.basename(path.resolve(WV2_FIXED)).match(/(\d+\.\d+\.\d+\.\d+)/)?.[1] || 'unknown version';
    fs.writeFileSync(path.join(stage, 'WebView2-NOTICE.txt'), [
      `The ${WV2_DIR} folder is the Microsoft Edge WebView2 Runtime, Fixed Version ${wv}: Microsoft's software, shipped`,
      `unchanged so ${name} draws its window without a separate WebView2 install. It is not covered by ${name}'s`,
      'Apache-2.0 license; Microsoft\'s terms apply. It is made possible by Chromium and other open source software:',
      `run ${WV2_DIR}\\show_third_party_software_licenses.bat for its credits. It does not update itself; a newer`,
      `${name} installer brings a newer runtime.`, '',
    ].join('\r\n'));
  }
  const rel2 = (p) => path.relative(path.join(HERE, 'src-tauri'), p);
  const config = {
    productName: name,
    mainBinaryName: exe,
    version,
    identifier: `com.getcadcraft.${slug}`,
    bundle: {
      ...(process.env.TAURI_SIGNING_PRIVATE_KEY ? { createUpdaterArtifacts: true } : {}),
      shortDescription: name,
      icon: ['32x32.png', '128x128.png', 'icon.ico'].map((f) => rel2(path.join(stage, 'icons', f))),
      resources: {
        [rel2(path.join(stage, 'app')) + '/']: 'app/',
        [rel2(path.join(stage, 'shell.json'))]: 'shell.json',
        [rel2(path.join(HERE, 'THIRD_PARTY.md'))]: 'THIRD_PARTY.md',
        [rel2(path.join(HERE, 'LICENSE'))]: 'LICENSE',
        ...(suiteApps.length ? { [rel2(path.join(stage, 'appicons')) + '/']: 'appicons/' } : {}),
        ...(WV2_FIXED ? {
          [`${WV2_DIR}/`]: `${WV2_DIR}/`,
          [rel2(path.join(stage, 'WebView2-NOTICE.txt'))]: 'WebView2-NOTICE.txt',
        } : {}),
      },
      windows: {
        ...(WV2_FIXED ? { webviewInstallMode: { type: 'fixedRuntime', path: `${WV2_DIR}/` } } : {}),
        nsis: { installMode: 'currentUser', installerHooks: rel2(path.join(stage, 'hooks.nsh')) },
        ...(process.env.CC_SIGN_COMMAND ? { signCommand: process.env.CC_SIGN_COMMAND } : {}),
      },
    },
  };
  // Signing for the updater needs the public key in the bundler's config too (the shell itself reads shell.json).
  if (process.env.TAURI_SIGNING_PRIVATE_KEY && !STORE) {
    if (!updates) throw new Error('TAURI_SIGNING_PRIVATE_KEY without CC_UPDATE_PUBKEY: set both, or neither');
    config.plugins = { updater: { pubkey: updates.pubkey, endpoints: [updates.endpoint] } };
  }
  if (TARGET === 'macos') {
    // macOS registers file types through the bundle's Info.plist (Tauri's fileAssociations; Windows keeps its own polite
    // NSIS hook): own types Owner, polite ones Default (used only when nothing else claims them), the rest Alternate.
    const f = APPS[slug].files, T = APPS._types;
    const assoc = (exts, rank) => exts.map((ext) => ({ ext: [ext], name: T[ext] || `${ext.toUpperCase()} file`, role: 'Editor', rank }));
    config.bundle.fileAssociations = [...assoc(f.own, 'Owner'), ...assoc(f.polite, 'Default'), ...assoc(f.openWith, 'Alternate')];
    config.bundle.macOS = {
      minimumSystemVersion: '13.0',   // Catalog: planned macOS 13 Ventura or later
      // No Developer ID (test builds): sign ad hoc ("-"), so the bundle's signature is whole — Apple silicon refuses to run
      // an unsigned or broken one. Gatekeeper still asks (right-click › Open the first time); never publish such a build.
      ...(process.env.APPLE_SIGNING_IDENTITY ? {} : { signingIdentity: '-' }),
      ...(suite ? { infoPlist: rel2(path.join(HERE, 'macos/Studio.Info.plist')) } : {}),
      // Mac App Store (S.6): sandboxed, files through the open/save panels, network client + server (Studio Live).
      ...(STORE ? { entitlements: rel2(path.join(HERE, 'macos/Studio.MAS.entitlements')) } : {}),
    };
  }
  const cfgFile = path.join(stage, 'tauri.override.json');
  fs.writeFileSync(cfgFile, JSON.stringify(config, null, 2));

  const tauriDir = path.join(HERE, 'src-tauri');
  if (TARGET === 'windows') {
    const native = process.platform === 'win32';
    // MSI (WiX) needs a numeric version: only releases get one.
    const bundles = native && /^\d+\.\d+(\.\d+)?$/.test(release) ? 'nsis,msi' : 'nsis';
    const args = native ? ['tauri', 'build', '--bundles', bundles, '--config', cfgFile, ...featureArgs]
      : ['tauri', 'build', '--runner', 'cargo-xwin', '--target', 'x86_64-pc-windows-msvc', '--bundles', bundles, '--config', cfgFile, ...featureArgs];
    const base0 = path.join(tauriDir, native ? 'target/release/bundle' : 'target/x86_64-pc-windows-msvc/release/bundle');
    for (const sub of ['nsis', 'msi']) { // signatures from an earlier signed build must never be mistaken for this one's
      const d = path.join(base0, sub);
      if (fs.existsSync(d)) for (const f of fs.readdirSync(d)) if (f.endsWith('.sig')) fs.rmSync(path.join(d, f));
    }
    run('cargo', args, { cwd: tauriDir, env: cargoEnv });
    const base = path.join(tauriDir, native ? 'target/release/bundle' : 'target/x86_64-pc-windows-msvc/release/bundle');
    const pick = (sub, ext) => {
      const dir = path.join(base, sub);
      return fs.existsSync(dir) ? fs.readdirSync(dir).filter((f) => f.endsWith(ext) && f.startsWith(name + '_' + version)).map((f) => path.join(dir, f))[0] : null;
    };
    const exeOut = pick('nsis', '.exe');
    if (!exeOut) throw new Error(`${slug}: no NSIS installer under ${base}`);
    const tagOut = STORE ? '-store' : '';
    const outs = [[exeOut, `${name}-${fileVersion}${tagOut}-setup.exe`]];
    if (bundles.includes('msi')) {
      const msi = pick('msi', '.msi');
      if (!msi) throw new Error(`${slug}: no MSI under ${base}`);
      outs.push([msi, `${name}-${fileVersion}${tagOut}.msi`]);
    }
    for (const [from, to] of outs) {
      const dest = path.join(HERE, 'out', to);
      fs.copyFileSync(from, dest);
      console.log(`   → ${path.relative(HERE, dest)} (${(fs.statSync(dest).size / 1048576).toFixed(1)} MB)`);
    }
    // The updater feed entry (W.7): only with the signing key, for the NSIS installer as published on the release.
    if (!STORE && process.env.TAURI_SIGNING_PRIVATE_KEY && fs.existsSync(exeOut + '.sig')) {
      const tag = `${slug}-v${version}`;
      const feed = {
        version, notes: `${name} ${release}`, pub_date: new Date().toISOString(),
        platforms: { 'windows-x86_64': {
          signature: fs.readFileSync(exeOut + '.sig', 'utf8').trim(),
          url: `https://github.com/getcadcraft/${slug}/releases/download/${tag}/${encodeURIComponent(`${name}-${release}-setup.exe`)}`,
        } },
      };
      fs.writeFileSync(path.join(HERE, 'out', `${slug}-latest.json`), JSON.stringify(feed, null, 2));
      console.log(`   → out/${slug}-latest.json (upload as latest.json on the ${tag} release)`);
    }
  } else if (TARGET === 'macos') {
    // Universal (Apple silicon + Intel). Signing/notarizing: Tauri reads APPLE_CERTIFICATE (+ _PASSWORD), APPLE_SIGNING_IDENTITY,
    // APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID from the environment (CI secrets; the owner's Apple Developer account).
    run('cargo', ['tauri', 'build', '--bundles', 'app,dmg', '--target', 'universal-apple-darwin', '--config', cfgFile, ...featureArgs], { cwd: tauriDir, env: cargoEnv });
    const dir = path.join(tauriDir, 'target/universal-apple-darwin/release/bundle/dmg');
    const dmg = fs.existsSync(dir) ? fs.readdirSync(dir).filter((f) => f.endsWith('.dmg')).map((f) => path.join(dir, f))[0] : null;
    if (!dmg) throw new Error(`${slug}: no .dmg under ${dir}`);
    const dest = path.join(HERE, 'out', `${name}-${fileVersion}${STORE ? '-store' : ''}.dmg`);
    fs.copyFileSync(dmg, dest);
    console.log(`   → ${path.relative(HERE, dest)} (${(fs.statSync(dest).size / 1048576).toFixed(1)} MB)`);
  } else {
    run('cargo', ['tauri', 'build', '--no-bundle', '--config', cfgFile, ...featureArgs], { cwd: tauriDir, env: cargoEnv });
  }
  } catch (e) {
    console.error(`!! ${slug}: ${e.message}`);
    failed.push(slug);
  }
}

const sums = fs.readdirSync(path.join(HERE, 'out')).filter((f) => /\.(exe|msi|dmg|apk|aab|ipa)$/.test(f)).sort()
  .map((f) => `${sha256(path.join(HERE, 'out', f))}  ${f}`);
fs.writeFileSync(path.join(HERE, 'out/SHA256SUMS'), sums.join('\n') + '\n');
console.log(sums.join('\n'));
if (failed.length) { console.error(`not built: ${failed.join(', ')}`); process.exit(1); }
