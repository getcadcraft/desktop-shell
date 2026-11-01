// SPDX-License-Identifier: Apache-2.0
// The portable zip's folder (0.7 D7.53): built from a made-up stage, checked, and the checker catches what must not ship.
//   node test/portable.test.mjs
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { buildFolder, checkFolder, readme } from '../portable/layout.mjs';

const HERE = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'cc-portable-'));
let pass = 0;
const ok = (c, m) => { assert.ok(c, m); pass++; };
const eq = (a, b, m) => { assert.deepEqual(a, b, m); pass++; };

const stage = path.join(tmp, 'stage'), wv2 = path.join(tmp, 'FixedVersionRuntime.1.2.3.4.x64'), out = path.join(tmp, 'out');
fs.mkdirSync(path.join(stage, 'app'), { recursive: true });
fs.writeFileSync(path.join(stage, 'app/index.html'), '<!doctype html>');
fs.writeFileSync(path.join(stage, 'shell.json'), JSON.stringify({ app: 'studio', name: 'CadCraft Studio', version: '0.7.0', suite: true, account: true, store: true, updates: { endpoint: 'x', pubkey: 'y' } }));
fs.writeFileSync(path.join(stage, 'WebView2-NOTICE.txt'), 'x.\r\na newer CadCraft Studio installer brings a newer runtime.\r\n');
fs.mkdirSync(wv2, { recursive: true });
fs.writeFileSync(path.join(wv2, 'msedgewebview2.exe'), 'MZ');
fs.writeFileSync(path.join(tmp, 'studio.exe'), 'MZ');

const dir = buildFolder({ out, name: 'CadCraft Studio', version: '0.7.0', exe: 'CadCraft Studio', from: { exePath: path.join(tmp, 'studio.exe'), stage, wv2, shellDir: HERE } });
eq(checkFolder(dir, { exe: 'CadCraft Studio' }), [], 'a whole folder checks clean');
const sj = JSON.parse(fs.readFileSync(path.join(dir, 'shell.json'), 'utf8'));
eq([sj.portable, sj.updates, sj.store, sj.suite], [true, undefined, undefined, true], 'portable flag on; no feed; not a store build; still Studio');
ok(!/installer/.test(fs.readFileSync(path.join(dir, 'WebView2-NOTICE.txt'), 'utf8')), 'the notice does not say installer');
ok(/never updates itself/.test(readme({ name: 'X', version: '1' })) && /data\\/.test(readme({ name: 'X', version: '1' })), 'README says where data goes and that there is no updater');
eq(path.basename(dir), 'CadCraft Studio-0.7.0-portable');
// With the build's feed (0.7, one endpoint): edition portable — Studio only tells (updates.rs Notify), never replaces itself.
const told = buildFolder({ out: path.join(tmp, 'out2'), name: 'CadCraft Studio', version: '0.7.0', exe: 'CadCraft Studio', updates: { endpoint: 'https://getcadcraft.com/api/v1/updates/{{target}}/{{arch}}/{{current_version}}?app=studio&edition={edition}', pubkey: 'k', edition: 'installer' }, from: { exePath: path.join(tmp, 'studio.exe'), stage, wv2, shellDir: HERE } });
const tj = JSON.parse(fs.readFileSync(path.join(told, 'shell.json'), 'utf8'));
eq([tj.updates.edition, tj.portable, checkFolder(told, { exe: 'CadCraft Studio' }).length], ['portable', true, 0], 'with a feed: edition portable, and the folder still checks clean');
ok(/never replaces itself/.test(fs.readFileSync(path.join(told, 'README.txt'), 'utf8')) && /tell you when a newer/.test(fs.readFileSync(path.join(told, 'README.txt'), 'utf8')), 'its README says it tells, never replaces itself');

// What the checker must catch.
fs.writeFileSync(path.join(dir, 'shell.json'), JSON.stringify({ app: 'studio', portable: true, updates: { endpoint: 'x' } }));
ok(checkFolder(dir, { exe: 'CadCraft Studio' }).some((p) => /update feed/.test(p)), 'an update feed is refused');
fs.writeFileSync(path.join(dir, 'shell.json'), JSON.stringify({ app: 'studio' }));
ok(checkFolder(dir, { exe: 'CadCraft Studio' }).some((p) => /portable is not true/.test(p)), 'the flag is required');
fs.mkdirSync(path.join(dir, 'data'));
ok(checkFolder(dir, { exe: 'CadCraft Studio' }).some((p) => /data\/ is in the zip/.test(p)), 'someone\'s data never ships');
fs.rmSync(path.join(dir, 'webview2'), { recursive: true });
ok(checkFolder(dir, { exe: 'CadCraft Studio' }).some((p) => /webview2/.test(p)), 'the runtime is required');

// CE.10 (0.8): nothing in the shell may write to the PC's profile behind a portable copy's back. The only places that may name a
// per-user folder are portable.rs (data_file falls back to it for an install), the window-state plugin's first-start probe in
// lib.rs (skipped when portable) and carry.rs (SD.8's one-time copy, switched off when portable). Everything else goes through
// portable::data_file / webview_dir. A new use fails here until it is looked at.
const SRC = path.join(HERE, 'src-tauri/src');
const PROFILE = /\b(app_data_dir|app_config_dir|app_local_data_dir|app_cache_dir|app_log_dir|data_local_dir|config_dir|cache_dir|home_dir)\b/;
const ALLOWED = new Set(['portable.rs', 'lib.rs', 'carry.rs']);
const hits = [];
(function walk(d) {
  for (const e of fs.readdirSync(d, { withFileTypes: true })) {
    const f = path.join(d, e.name);
    if (e.isDirectory()) walk(f);
    else if (e.name.endsWith('.rs') && !ALLOWED.has(e.name)) {
      fs.readFileSync(f, 'utf8').split('\n').forEach((l, i) => { if (!l.trim().startsWith('//') && PROFILE.test(l)) hits.push(`${e.name}:${i + 1}`); });
    }
  }
})(SRC);
eq(hits, [], 'no module outside portable/lib/carry names a per-user folder');
const lib = fs.readFileSync(path.join(SRC, 'lib.rs'), 'utf8');
ok(/if portable::is_portable\(\) \{\s*builder\s*\} else \{\s*builder\.plugin\(tauri_plugin_window_state/.test(lib), 'the window-state plugin (writes to the profile) is off when portable');
ok(/carry::Carry::new\(app, suite && !portable::is_portable\(\)\)/.test(lib), 'the 0.4 carry-over is off when portable');
ok(/!portable::is_portable\(\) && matches!\(e, tauri::WindowEvent::Moved/.test(lib), 'the debounced place save is off when portable');
const pr = fs.readFileSync(path.join(SRC, 'portable.rs'), 'utf8');
ok(/APPIMAGE_MARKER: &str = "portable"/.test(pr) && /APPIMAGE_DATA: &str = "cadcraft-data"/.test(pr), 'Linux AppImage marker and data folder names');

fs.rmSync(tmp, { recursive: true, force: true });
console.log(`portable: ${pass} passed, 0 failed`);
