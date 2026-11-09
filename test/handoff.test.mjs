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

console.log(`\n${n} passed`);
