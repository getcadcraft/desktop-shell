// SPDX-License-Identifier: Apache-2.0
// bridge.test.mjs — bridge/bridge.js against a stand-in for the shell's invoke, and the site's host.js on top of it
// (the contract in host.js's header). No webview needed:  node test/bridge.test.mjs
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';

const HERE = path.dirname(new URL(import.meta.url).pathname);
// host.js: CADCRAFT_HOST_JS (any copy, e.g. an edition's), else the one in CADCRAFT_SITE; neither = its checks skip.
const HOST = process.env.CADCRAFT_HOST_JS
  || (process.env.CADCRAFT_SITE ? path.join(process.env.CADCRAFT_SITE, 'public_html/assets/js/host.js') : '');
let pass = 0, fail = 0;
const ok = (c, what, extra = '') => { if (c) { pass++; console.log(`  ✓ ${what}`); } else { fail++; console.log(`  ✗ ${what} ${extra}`); } };

// A stand-in shell: a tiny file system and the commands bridge.js calls.
const disk = new Map([['/home/me/a.dwg', new Uint8Array([1, 2, 3])]]);
const granted = new Set(['/home/me/a.dwg']);
const calls = [];
let answer = null;
const invoke = async (cmd, args, opts) => {
  calls.push({ cmd, args, opts });
  const need = (p) => { if (!granted.has(p)) throw 'not_allowed: the app can reach only files you opened or saved, or folders you chose'; };
  switch (cmd) {
    case 'cc_open_dialog': if (answer) answer.forEach((p) => granted.add(p)); return answer;
    case 'cc_save_dialog': if (answer) granted.add(answer[0]); return answer && answer[0];
    case 'cc_open_folder_dialog': return answer && answer[0];
    case 'cc_read_file': need(args.path); if (!disk.has(args.path)) throw 'not_found: gone'; return disk.get(args.path).buffer.slice(0);
    case 'cc_write_file': { const p = decodeURIComponent(opts.headers['x-cc-path']); need(p); disk.set(p, new Uint8Array(args)); return null; }
    case 'cc_stat': need(args.path); return disk.has(args.path) ? { size: disk.get(args.path).length, mtime: 1700000000000 } : null;
    case 'cc_list_dir': need(args.path); return [];
    case 'cc_launch_files': return ['/home/me/a.dwg'];
    case 'cc_info': return { shell: '0.4.0', app: 'cadcraft', platform: 'windows' };
    case 'cc_set_fullscreen': return null;
    case 'cc_account_status': return { state: 'signed_out' };
    case 'cc_cloud_request': {
      const head = new TextEncoder().encode(JSON.stringify({ 'content-type': 'application/json' }));
      const body = new TextEncoder().encode(JSON.stringify({ data: { path: opts.headers['x-cc-path'], method: opts.headers['x-cc-method'], n: args.length } }));
      const out = new Uint8Array(6 + head.length + body.length);
      new DataView(out.buffer).setUint16(0, 200); new DataView(out.buffer).setUint32(2, head.length);
      out.set(head, 6); out.set(body, 6 + head.length);
      return out.buffer;
    }
    default: throw `unknown command ${cmd}`;
  }
};

const listeners = {};
const window = { __TAURI_INTERNALS__: { invoke } };
window.top = window;
const document = { addEventListener: (t, f) => { listeners[t] = f; }, fullscreenElement: null };
const ctx = vm.createContext({ window, document, Uint8Array, ArrayBuffer, DataView, Error, TypeError, Object, Array, String, JSON, encodeURIComponent, TextEncoder, TextDecoder });
vm.runInContext(fs.readFileSync(path.join(HERE, '../bridge/bridge.js'), 'utf8'), ctx);
const d = window.cadcraftDesktop;

console.log('bridge.js');
ok(d && d.version === 1 && Object.isFrozen(d), 'window.cadcraftDesktop v1, frozen');
ok(typeof d.openFolderDialog === 'function' && typeof d.listDir === 'function', 'folder bridge present (W2.15)');
vm.runInContext(fs.readFileSync(path.join(HERE, '../bridge/bridge.js'), 'utf8'), ctx);
ok(window.cadcraftDesktop === d, 'running twice keeps the first bridge');

const bytes = await d.readFile('/home/me/a.dwg');
ok(bytes instanceof Uint8Array && bytes.length === 3, 'readFile → Uint8Array');
await d.writeFile('/home/me/a.dwg', new Uint8Array([9, 9]));
const w = calls.find((c) => c.cmd === 'cc_write_file');
ok(w.args instanceof Uint8Array && w.opts.headers['x-cc-path'] === encodeURIComponent('/home/me/a.dwg'), 'writeFile sends raw bytes + the path header');
ok(disk.get('/home/me/a.dwg').length === 2, 'writeFile wrote');
await d.writeFile('/home/me/a.dwg', new Uint8Array([1, 2, 3, 4]).subarray(1, 3));
ok(disk.get('/home/me/a.dwg').join() === '2,3', 'a typed-array view writes only its own bytes');
let e = null;
try { await d.readFile('/etc/passwd'); } catch (x) { e = x; }
ok(e instanceof Error && e.code === 'not_allowed', 'refusals reject with an Error, code not_allowed');
try { await d.writeFile('/home/me/a.dwg', 'text'); e = null; } catch (x) { e = x; }
ok(e instanceof TypeError, 'writeFile refuses a non-byte value');

let got = null;
window.__ccDesktopOpen(['/home/me/b.dwg']);
d.onOpenFiles((p) => { got = p; });
ok(got && got[0] === '/home/me/b.dwg', 'files opened before onOpenFiles are delivered once a listener exists');
window.__ccDesktopOpen(['/home/me/c.dwg']);
ok(got[0] === '/home/me/c.dwg', 'and later ones go straight to it');

ok(Object.isFrozen(d.account) && typeof d.account.signIn === 'function' && typeof d.cloud.request === 'function', 'account + cloud present (C.2), frozen');
ok((await d.account.status()).state === 'signed_out', 'account.status()');
const cr = await d.cloud.request({ method: 'post', path: '/api/v1/sync/save?drawing=x&base=1', body: new Uint8Array([1, 2, 3]) });
ok(cr.status === 200 && cr.headers['content-type'] === 'application/json' && cr.json().data.n === 3 && cr.json().data.method === 'POST',
  'cloud.request sends bytes and decodes status, headers and body');
const cj = await d.cloud.request({ method: 'POST', path: '/api/v1/sync/projects', body: { workspace: 'w', name: 'n' } });
ok(calls.filter((c) => c.cmd === 'cc_cloud_request').pop().opts.headers['x-cc-content-type'] === 'application/json' && cj.status === 200, 'an object body goes as JSON');
let seen = null;
d.account.onChange((st) => { seen = st; });
window.__ccDesktopAccount({ state: 'signed_in' });
ok(seen && seen.state === 'signed_in', 'account.onChange hears the shell');

document.fullscreenElement = {};
listeners.fullscreenchange();
ok(calls.some((c) => c.cmd === 'cc_set_fullscreen' && c.args.on === true), 'fullscreenchange → the window follows');

// A build without sign-in (shell.json account:false → window.__ccShellFlags) has no account or cloud.
{
  const w2 = { __TAURI_INTERNALS__: { invoke }, __ccShellFlags: { account: false } };
  w2.top = w2;
  vm.runInContext(fs.readFileSync(path.join(HERE, '../bridge/bridge.js'), 'utf8'),
    vm.createContext({ window: w2, document: { addEventListener() {} }, Uint8Array, ArrayBuffer, DataView, Error, TypeError, Object, Array, String, JSON, encodeURIComponent, TextEncoder, TextDecoder }));
  ok(w2.cadcraftDesktop && w2.cadcraftDesktop.version === 1 && !('account' in w2.cadcraftDesktop) && !('cloud' in w2.cadcraftDesktop),
    'account:false → no account, no cloud (files still there)');
}

// host.js on top of the bridge (skipped without the site: set CADCRAFT_SITE, or CADCRAFT_HOST_JS to any copy).
if (!HOST || !fs.existsSync(HOST)) {
  console.log(`(${HOST ? 'host.js not found at ' + HOST : 'CADCRAFT_SITE / CADCRAFT_HOST_JS not set'}: the host.js checks are skipped)\n\n${pass} passed, ${fail} failed`);
  process.exit(fail ? 1 : 0);
}
console.log('host.js on the bridge');
const { getHost } = await import(HOST);
const host = getHost({ app: 'cadcraft', desktop: d, win: null });
ok(host.kind === 'desktop' && host.caps.saveInPlace && host.caps.saveDialog, 'kind desktop, can save in place');
answer = ['/home/me/a.dwg'];
const [f] = await host.openFile({ types: [{ description: 'Drawings', extensions: ['.dwg', '.dxf'] }] });
const od = calls.filter((c) => c.cmd === 'cc_open_dialog').pop();
ok(JSON.stringify(od.args.opts.filters) === JSON.stringify([{ name: 'Drawings', extensions: ['dwg', 'dxf'] }]), 'open filters reach the shell without dots');
ok(f && f.path === '/home/me/a.dwg' && (await f.bytes()).length === 2, 'openFile → a HostFile read through the bridge');
const r = await host.save({ target: f, data: new Uint8Array([5, 5, 5]), name: 'a.dwg' });
ok(r.ok && r.how === 'written' && disk.get('/home/me/a.dwg').length === 3, 'save() writes in place');
answer = ['/home/me/new.dxf'];
const r2 = await host.saveAs({ data: 'DXF', name: 'new.dxf', types: [{ description: 'DXF', extensions: ['.dxf'] }] });
ok(r2.ok && r2.path === '/home/me/new.dxf' && new TextDecoder().decode(disk.get('/home/me/new.dxf')) === 'DXF', 'saveAs() → dialog + write');
answer = null;
const r3 = await host.saveAs({ data: 'x', name: 'x.dxf' });
ok(!r3.ok && r3.cancelled, 'a cancelled save dialog → cancelled');
const lf = await host.launchFiles();
ok(lf.length === 1 && lf[0].path === '/home/me/a.dwg', 'launchFiles() → HostFiles');

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
