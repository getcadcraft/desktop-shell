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

// Studio (suite + live): cadcraftDesktop.live against a stand-in shell — an echo connection through the long poll, sends in
// order one batch at a time, the close record, and signIn through cloud.request.
{
  const lcalls = [];
  const queue = [];        // records waiting for cc_live_recv
  let wake = null, inFlight = 0, maxInFlight = 0;
  const rec = (k, b = new Uint8Array(0)) => { const r = new Uint8Array(5 + b.length); r[0] = k; new DataView(r.buffer).setUint32(1, b.length); r.set(b, 5); return r; };
  const push = (r) => { queue.push(r); if (wake) { const w = wake; wake = null; w(); } };
  const linvoke = async (cmd, args, opts) => {
    lcalls.push({ cmd, args, opts });
    switch (cmd) {
      case 'cc_live_open': setTimeout(() => push(rec(0)), 0); return 7;
      case 'cc_live_recv': {
        if (!queue.length) await new Promise((r) => { wake = r; });
        const all = queue.splice(0); const n = all.reduce((a, b) => a + b.length, 0); const u = new Uint8Array(n); let p = 0;
        for (const r of all) { u.set(r, p); p += r.length; }
        return u.buffer;
      }
      case 'cc_live_send': {
        inFlight++; maxInFlight = Math.max(maxInFlight, inFlight);
        await new Promise((r) => setTimeout(r, 5));
        const u = new Uint8Array(args); const dv = new DataView(u.buffer);
        for (let p = 0; p < u.length;) { const n = dv.getUint32(p); push(rec(1, u.slice(p + 4, p + 4 + n))); p += 4 + n; }
        inFlight--; return null;
      }
      case 'cc_live_close': push(rec(2, new TextEncoder().encode(JSON.stringify({ reason: 'closed', code: 0 })))); return null;
      case 'cc_live_session': return { running: true, port: 5000, addrs: ['192.168.1.20'], app: 'cadcraft' };
      case 'cc_live_browse': return [{ addr: '192.168.1.20', port: 5000, app: 'cadcraft' }];
      case 'cc_live_advertise': case 'cc_live_unadvertise': case 'cc_live_relay': case 'cc_live_relay_stop': return null;
      case 'cc_cloud_request': {
        const path = opts.headers['x-cc-path'];
        const json = path === '/api/v1/live/assertion' ? { data: { assertion: 'v1.a.b', picture_sha256: 'ab' } } : null;
        const body = json ? new TextEncoder().encode(JSON.stringify(json)) : new Uint8Array([82, 73, 70, 70]);
        const head = new TextEncoder().encode('{}');
        const out = new Uint8Array(6 + head.length + body.length);
        new DataView(out.buffer).setUint16(0, 200); new DataView(out.buffer).setUint32(2, head.length);
        out.set(head, 6); out.set(body, 6 + head.length);
        return out.buffer;
      }
      default: return invoke(cmd, args, opts);
    }
  };
  const mkWin = (flags) => {
    const w = { __TAURI_INTERNALS__: { invoke: linvoke }, __ccShellFlags: flags };
    w.top = w;
    vm.runInContext(fs.readFileSync(path.join(HERE, '../bridge/bridge.js'), 'utf8'),
      vm.createContext({ window: w, document: { addEventListener() {} }, Uint8Array, ArrayBuffer, DataView, Error, TypeError, Object, Array, String, JSON, encodeURIComponent, TextEncoder, TextDecoder, Math, Number, setTimeout, Promise }));
    return w.cadcraftDesktop;
  };
  console.log('cadcraftDesktop.live (Studio)');
  ok(!('live' in mkWin({ account: true, suite: true, live: false })) && !('live' in mkWin({ account: true, suite: false, live: true })), 'live only in Studio builds with the live feature');
  const L = mkWin({ account: true, suite: true, live: true }).live;
  ok(L && Object.isFrozen(L) && L.encrypts === false && typeof L.transport === 'function', 'live present, frozen, encrypts false');
  const got = []; let opened = 0, closedWith = null;
  const t = L.transport('local', { app: 'cadcraft', schema: 1 })({ open: () => opened++, message: (b) => got.push(b[0]), close: (r, c) => { closedWith = [r, c]; } });
  for (let i = 1; i <= 20; i++) t.send(new Uint8Array([i]));
  const t0 = Date.now();
  while (got.length < 20 && Date.now() - t0 < 3000) await new Promise((r) => setTimeout(r, 5));
  ok(opened === 1 && got.join() === Array.from({ length: 20 }, (_, i) => i + 1).join(), 'open, then 20 messages back in order', got.join());
  ok(maxInFlight === 1 && lcalls.filter((c) => c.cmd === 'cc_live_send').length < 20, 'one send batch in flight at a time');
  const op = lcalls.find((c) => c.cmd === 'cc_live_open');
  ok(op.args.args.kind === 'local' && op.args.args.app === 'cadcraft' && op.args.args.schema === 1, 'open carries kind, app, schema');
  t.close();
  ok(lcalls.some((c) => c.cmd === 'cc_live_close' && c.args.id === 7) && closedWith === null, 'close() tells the shell (and no close event back to the page that closed it)');
  ok((await L.session()).port === 5000 && (await L.browse('K7', 1500))[0].addr === '192.168.1.20', 'session() and browse()');
  await L.advertise({ room: 'K7QX9M2B' }, { app: 'cadcraft' });
  const ad = lcalls.filter((c) => c.cmd === 'cc_live_advertise').pop();
  ok(ad.args.args.room === 'K7QX9M2B' && !ad.args.args.tag && ad.args.args.app === 'cadcraft', 'advertise({room}) passes the room (the shell makes its tag)');
  await L.browse('K7', 99999);
  ok(lcalls.filter((c) => c.cmd === 'cc_live_browse').pop().args.args.ms === 5000, 'browse waits at most 5 s');
  const si = await L.signIn('A'.repeat(43));
  const cr2 = lcalls.filter((c) => c.cmd === 'cc_cloud_request');
  ok(si && si.assertion === 'v1.a.b' && si.picture instanceof Uint8Array && si.picture.length === 4, 'signIn → the assertion and the 64 × 64 picture');
  ok(cr2[0].opts.headers['x-cc-path'] === '/api/v1/live/assertion' && cr2[1].opts.headers['x-cc-path'] === '/api/v1/me/avatar?size=64', 'signIn asks the assertion, then the picture');
  ok(!('signIn' in mkWin({ account: false, suite: true, live: true }).live), 'a build without sign-in has no signIn');
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
