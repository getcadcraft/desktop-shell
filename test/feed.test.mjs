// SPDX-License-Identifier: Apache-2.0
// The update entries (0.7: one update endpoint on getcadcraft.com): one entry per signed file and edition, a release gate.
//   node test/feed.test.mjs
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { EDITIONS, RELEASE_EDITIONS, PACKAGE_EDITIONS, addEntry, list, check } from '../updater-feed.mjs';

let pass = 0, fail = 0;
const ok = (c, m, extra) => { if (c) pass++; else { fail++; console.log('  ✗ ' + m + (extra ? ' ' + extra : '')); } };
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'cc-feed-'));
const put = (n, text) => { const f = path.join(tmp, n); fs.writeFileSync(f, text); return f; };

ok(Object.keys(EDITIONS).join() === 'installer,portable,deb,appimage,dmg', 'the updater editions');
ok(PACKAGE_EDITIONS.join() === 'msix-web,msix-store', 'the MSIX editions are Windows\' (no updater entry)');
ok(RELEASE_EDITIONS.join() === 'installer,portable,deb,appimage', 'a complete release: both Windows files and both Linux files (the Mac tarball is not published yet)');

const out = path.join(tmp, 'out');
const exe = put('CadCraft Studio-0.7-setup.exe', 'MZ installer bytes');
const r = addEntry({ out, slug: 'studio', version: '0.7.0', edition: 'installer', target: 'windows', arch: 'x86_64', published: 'CadCraft-Studio-0.7-setup.exe', file: exe, sig: put('i.sig', 'SIGI\n'), notes: 'CadCraft Studio 0.7' });
const e = JSON.parse(fs.readFileSync(r.file, 'utf8'));
ok(path.basename(path.dirname(r.file)) === 'studio-0.7.0' && path.basename(r.file) === 'installer-windows-x86_64.json', 'an entry lands in out/updates/<slug>-<version>/<edition>-<target>-<arch>.json');
ok(e.format === 'cadcraft-update-entry' && e.signature === 'SIGI' && e.file === 'CadCraft-Studio-0.7-setup.exe' && e.version === '0.7.0', 'it names the published file, the installer version and the signature (trimmed)');
ok(e.sha256 === crypto.createHash('sha256').update('MZ installer bytes').digest('hex') && e.bytes === 18, 'it pins the exact bytes (the site checks the published copy against them)');
for (const [ed, tg, f] of [['portable', 'windows', 'CadCraft-Studio-0.7-portable.zip'], ['deb', 'linux', 'CadCraft-Studio-0.7-amd64.deb'], ['appimage', 'linux', 'CadCraft-Studio-0.7-x86_64.AppImage']]) {
  addEntry({ out, slug: 'studio', version: '0.7.0', edition: ed, target: tg, arch: 'x86_64', published: f, file: put(f, ed), sig: put(ed + '.sig', 'S' + ed) });
}
const all = list({ out, slug: 'studio', version: '0.7.0' });
ok(all.length === 4 && check(all).problems.length === 0, 'four editions: the release gate passes');
const c = check(all.filter((x) => x.edition !== 'deb'));
ok(c.missing.join() === 'deb' && c.problems.some((p) => /deb: missing/.test(p)), 'a missing edition fails the gate, by name');
ok(check(all.filter((x) => x.edition !== 'deb'), { allowMissing: ['deb'] }).problems.length === 0, 'left out on purpose: passes');
ok(check([{ ...all[0], signature: '' }, ...all.slice(1)]).problems.some((p) => /no signature/.test(p)), 'an entry without a signature fails');
ok(list({ out, slug: 'studio', version: '0.7.1' }).length === 0, 'entries are per version');

let threw = '';
try { addEntry({ out, slug: 'studio', version: '0.7.0', edition: 'msix-web', target: 'windows', arch: 'x86_64', published: 'x.msix', file: exe, sig: put('m.sig', 'x') }); } catch (x) { threw = x.message; }
ok(/unknown edition/.test(threw), 'an MSIX edition has no updater entry (Windows updates it)', threw);
threw = ''; try { addEntry({ out, slug: 'studio', version: '0.7', edition: 'installer', target: 'windows', arch: 'x86_64', published: 'x.exe', file: exe, sig: put('v.sig', 'x') }); } catch (x) { threw = x.message; }
ok(/x\.y\.z/.test(threw), 'the version is the installer\'s own x.y.z (what the signature carries)', threw);
threw = ''; try { addEntry({ out, slug: 'studio', version: '0.7.0', edition: 'installer', target: 'windows', arch: 'x86_64', published: 'Has Space.exe', file: exe, sig: put('n.sig', 'x') }); } catch (x) { threw = x.message; }
ok(/published name/.test(threw), 'the published name has no spaces (as /download names files)', threw);
threw = ''; try { addEntry({ out, slug: 'studio', version: '0.7.0', edition: 'installer', target: 'windows', arch: 'x86_64', published: 'a.exe', file: exe, sig: put('e.sig', '') }); } catch (x) { threw = x.message; }
ok(/empty/.test(threw), 'an empty signature file is refused', threw);

const cli = (...a) => spawnSync(process.execPath, [path.join(path.dirname(new URL(import.meta.url).pathname), '..', 'updater-feed.mjs'), ...a], { encoding: 'utf8' });
const good = cli('check', 'studio', '0.7.0', '--out', out);
ok(good.status === 0 && /every edition/.test(good.stdout), 'check passes on the complete set', good.stderr);
fs.rmSync(path.join(out, 'updates', 'studio-0.7.0', 'deb-linux-x86_64.json'));
const bad = cli('check', 'studio', '0.7.0', '--out', out);
ok(bad.status === 1 && /deb: missing/.test(bad.stderr), 'without the .deb entry: exit 1 and why', bad.stderr);
ok(/installer\twindows-x86_64\t0\.7\.0/.test(cli('list', 'studio', '0.7.0', '--out', out).stdout), 'list shows each entry');
ok(cli('nonsense').status === 2, 'unknown command: usage, exit 2');

// The shell's side of the endpoint: build.mjs writes {edition} for the shell to fill, and the shell never takes http unless it is
// a test build on this computer (src-tauri/src/updates.rs tests); the default endpoint is the site's one service.
const bm = fs.readFileSync(path.join(path.dirname(new URL(import.meta.url).pathname), '..', 'build.mjs'), 'utf8');
ok(bm.includes("https://getcadcraft.com/api/v1/updates/{{target}}/{{arch}}/{{current_version}}?app={slug}&edition={edition}"), 'build.mjs: the default endpoint is getcadcraft.com\'s update service');
ok(!/not with the updater \(Tauri forces the download bootstrapper\)/.test(bm), 'build.mjs: the fixed-runtime installer may carry the updater (v2 artifacts keep the fixed runtime)');
ok(/UPDATE_TEST \? path\.join\(HERE, 'out', 'update-test'\)/.test(bm) && /only an --update-test build may use one/.test(bm), 'build.mjs: http feeds only in --update-test builds, which land outside out/');

fs.rmSync(tmp, { recursive: true, force: true });
console.log(`${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
