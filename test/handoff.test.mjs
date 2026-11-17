// SPDX-License-Identifier: Apache-2.0
// The "open this" hand-off, the drop hook and the start-up log (0.9 FX.27, DC.117, OF.4, FX.29, FX.28), as source contracts: the
// pieces a Windows, Mac or Android build needs exist and are wired through ONE place. Node only; no webview, no build. What only a
// build can show is in docs/0.9-sessions/studio-fix-09.md (per item: what to look for).
//   node test/handoff.test.mjs
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';

const SHELL = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const src = (f) => fs.readFileSync(path.join(SHELL, 'src-tauri/src', f), 'utf8');
const lib = src('lib.rs'), files = src('files.rs'), windows = src('windows.rs'), startlog = src('startlog.rs');
let n = 0;
const t = (name, fn) => { fn(); n++; console.log('ok   ' + name); };

t('FX.27: macOS Apple events and Android intents arrive as RunEvent::Opened and go to files::open_urls', () => {
  assert.match(lib, /\.build\(tauri::generate_context!\(\)\)/);
  assert.match(lib, /RunEvent::Opened \{ urls \}[\s\S]{0,80}files::open_urls\(app, urls\)/);
  assert.match(lib, /target_os = "macos", target_os = "ios", target_os = "android"/);
});
t('FX.27: argv at the start, a second start and an opened URL share hand_over -> Launch::add', () => {
  assert.equal((lib.match(/hand_over\(/g) || []).length >= 2, true, 'hand_over is called from the start (argv) and from a second start; open_urls (files.rs) is the third');
  assert.match(files, /crate::hand_over\(app, args/);
  assert.match(lib, /fn hand_over[\s\S]{0,600}files::Launch>\(\)\.add\(app, files\)/);
  // no second copy of the argv handling
  assert.equal((lib.match(/files_from_args\(/g) || []).length, 1);
});
t('FX.27 + OF.4: a drop on the hub and on every app window goes through files::drop_on_window', () => {
  assert.match(lib, /DragDropEvent::Drop \{ paths, \.\. \}[\s\S]{0,120}files::drop_on_window\(&h, "main", paths\)/);
  assert.match(windows, /DragDropEvent::Drop \{ paths, \.\. \}\) => files::drop_on_window\(&h, &l, paths\)/);
});
t('OF.4: an add-on package (*.ccaddon.json) goes to window.ccAddonsFromFile, never opened as a drawing', () => {
  assert.match(files, /ends_with\("\.ccaddon\.json"\)/);
  assert.match(files, /window\.ccAddonsFromFile&&window\.ccAddonsFromFile\(\{\{name:\{\},bytes:new TextEncoder\(\)\.encode\(\{\}\)\}\}\)/);
  const mount = fs.readFileSync(path.join(SHELL, '../getcadcraft.com/public_html/assets/js/app-addons-account-mount.js'), 'utf8');
  assert.match(mount, /window\.ccAddonsFromFile = async \(f\) =>/, 'the page side of the hook exists');
  assert.match(mount, /f && f\.bytes instanceof Uint8Array/, 'and takes {name, bytes}');
});
t('DC.117: the Android manifest takes VIEW for every own file type of the suite, from file and content', () => {
  const apps = JSON.parse(fs.readFileSync(path.join(SHELL, 'apps.json'), 'utf8'));
  const own = [...new Set(Object.entries(apps).filter(([k]) => k[0] !== '_').flatMap(([, a]) => a.files.own))];
  assert.ok(own.includes('modelcraft'));
  const xml = fs.readFileSync(path.join(SHELL, 'src-tauri/gen/android/app/src/main/AndroidManifest.xml'), 'utf8');
  for (const ext of own) for (const scheme of ['content', 'file']) {
    const block = xml.split('<intent-filter>').find((b) => b.includes(`android:scheme="${scheme}"`) && b.includes(`\\\\.${ext}"`));
    assert.ok(block, `a VIEW filter for .${ext} from ${scheme}`);
    assert.match(block, /android\.intent\.action\.VIEW/);
    assert.match(block, /category\.DEFAULT/);
  }
});
t('FX.28: a maximized place keeps its normal rectangle and is not positioned', () => {
  assert.match(windows, /fn place_keep/);
  assert.equal((windows.match(/place_of\(/g) || []).length, 1, 'place_of is only called from place_keep');
  assert.match(windows, /if self\.maximized \{\s*b\.center\(\)\.maximized\(true\)/);
});
t('FX.29: every start-up stage is logged, the last start is kept, a watchdog and a panic hook exist', () => {
  for (const stage of ['run: begin', 'setup: begin', 'setup: building the main window', 'main window built', 'main page started loading']) {
    assert.ok(lib.includes(stage), `lib.rs marks "${stage}"`);
  }
  assert.match(lib, /startlog::init\(portable::data_file\(app\.handle\(\), "startup\.log"\)\)/);
  assert.match(lib, /startlog::install_panic_hook\(\)/);
  assert.match(lib, /startlog::watchdog\(\)/);
  assert.match(startlog, /startup\.previous\.log/);
  assert.match(startlog, /no file names, passwords or account data|no file names, paths of documents, tokens or account data/);
  // the log never takes a path, a name or a token: mark() callers only pass literals and counts
  assert.doesNotMatch(lib, /startlog::mark\(&format!\([^)]*(path|file|token|args|argv)/i);
});

// ---- 0.10 UI.4: torn-off windows as real Studio windows, remembered per monitor ----
const places = src('popup_places.rs');
t('UI.4: the pop-up hand-off still builds the window on about:blank (0.6 crash) and every pop-up goes through popup_hook', () => {
  assert.match(lib, /WebviewUrl::External\("about:blank"\.parse\(\)\.expect\("about:blank"\)\)/);
  assert.match(lib, /NewWindowResponse::Create \{ window \}/);
  assert.match(lib, /close_with_page\(&window\);\s*windows::popup_hook\(&window, &opener_label\)/);
  assert.match(lib, /windows::popup_titled\(&w, &opener_t, &t\)/);
});
t('UI.4: the place file holds the app, the block title and numbers only; the state is managed on desktop', () => {
  assert.match(lib, /mod popup_places;/);
  assert.match(lib, /app\.manage\(popup_places::PopupState::new\(portable::data_file\(app\.handle\(\), "popup-windows\.txt"\)\)\)/);
  assert.match(places, /std-only on purpose/);
  assert.doesNotMatch(places.split('#[cfg(test)]')[0], /use (tauri|serde)/);
});
t('UI.4: an app window\'s close (the hub, an app window) closes its pop-ups and notes them to be offered back', () => {
  assert.match(lib, /CloseRequested \{ \.\. \}\) \{\s*windows::popup_opener_closing\(&h, "main"\)/);
  assert.match(windows, /popup_opener_closing\(&h, &l\);/);
  assert.match(windows, /fn popup_opener_closing[\s\S]{0,400}w\.close\(\)/);
});
t('UI.4: the page asks cc_popup_restore (registered, desktop only); the four new commands are registered', () => {
  for (const c of ['windows::cc_popup_restore,', 'windows::cc_startlog_mark,', 'windows::cc_startlog_summary,', 'windows::cc_startlog_text,']) assert.ok(lib.includes(c), c);
  assert.match(windows, /#\[cfg\(desktop\)\]\s*#\[tauri::command\]\s*pub fn cc_popup_restore/);
});
t('UI.4: a place is applied only when a connected screen still shows it', () => {
  assert.match(windows, /if !p\.shown_on\(&mons\(window\)\)/);
});

// ---- 0.10 PERF.18: the start-up log a person can send ----
t('PERF.18: the log gains the first page request, one line per app window, and the two stages only the page knows', () => {
  assert.match(lib, /startlog::mark_once\("first page request served"\)/);
  assert.match(lib, /window \{\} page started loading/);
  assert.match(startlog, /PAGE_STAGES: \[&str; 2\] = \["app module running", "first drawing paint"\]/);
  assert.match(windows, /startlog::page_stage\(caller\.label\(\), &stage\)/);
  assert.match(windows, /startlog::mark\(&format!\("app window \{label\} built"\)\)/);
});
t('PERF.18: the summary carries the budget (window under 1 s, usable under 3 s) and the text is capped at 64 kB', () => {
  assert.match(startlog, /BUDGET_WINDOW_MS: u128 = 1000/);
  assert.match(startlog, /BUDGET_USABLE_MS: u128 = 3000/);
  assert.match(startlog, /65536/);
});

console.log(`\n${n} passed`);
