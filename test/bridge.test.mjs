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
    case 'cc_printers_list': return { accepts: 'pdf', printers: [{ name: 'Office', default: true, papers: ['A4'] }] };
    case 'cc_print': return 'request id 7 (1 file)';
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
ok(d.printers && typeof d.printers.list === 'function' && typeof d.printers.print === 'function', 'printers: list and print (0.7 D7.20)');
{
  const l = await d.printers.list();
  ok(l.accepts === 'pdf' && l.printers[0].name === 'Office', 'printers.list gives what to send and the printers');
  await d.printers.print(new Uint8Array([37, 80, 68, 70]), { printer: 'Office', copies: 500, paper: 'A4', landscape: 1, title: 'Plan' });
  const c = calls.filter((x) => x.cmd === 'cc_print').pop();
  const job = JSON.parse(decodeURIComponent(c.opts.headers['x-cc-print']));
  ok(job.printer === 'Office' && job.copies === 99 && job.paper === 'A4' && job.landscape === true && job.title === 'Plan', 'printers.print sends the job as a header, copies held to 1–99', JSON.stringify(job));
  ok(c.args instanceof Uint8Array && c.args.length === 4, 'and the bytes as the body');
  ok(/mobile\) \{[\s\S]*delete bridge\.printers/.test(fs.readFileSync(path.join(HERE, '../bridge/bridge.js'), 'utf8')), 'tablets have no printers (the bridge removes it)');
}
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

// macOS (flags.nativeFullscreen, GH.1): the page's full screen is the window's own, never WebKit's element fullscreen
// with the window toggled on top of it. A small DOM stand-in: Element/Document prototypes with WebKit's own methods.
{
  const fcalls = [];
  let winFull = false;          // the window's full screen, as the shell reports it
  let webkitAsked = 0;
  const finvoke = async (cmd, args) => {
    fcalls.push({ cmd, args });
    if (cmd === 'cc_set_fullscreen') { winFull = !!args.on; return null; }
    if (cmd === 'cc_is_fullscreen') return winFull;
    if (cmd === 'cc_info') return { platform: 'macos' };
    return null;
  };
  class Ev { constructor(type, o = {}) { this.type = type; this.bubbles = !!o.bubbles; } }
  class El { constructor(doc) { this.doc = doc; } dispatchEvent(ev) { if (ev.bubbles) this.doc.dispatchEvent(ev); return true; } }
  El.prototype.requestFullscreen = function () { webkitAsked++; return Promise.resolve(); };
  class Doc {
    constructor() { this.ls = {}; this.documentElement = new El(this); }
    addEventListener(t, f) { (this.ls[t] = this.ls[t] || []).push(f); }
    dispatchEvent(ev) { (this.ls[ev.type] || []).forEach((f) => f(ev)); return true; }
  }
  Object.defineProperty(Doc.prototype, 'fullscreenElement', { configurable: true, get() { return null; } });
  Doc.prototype.exitFullscreen = function () { return Promise.reject(new TypeError('webkit: not in full screen')); };
  const fdoc = new Doc();
  const wls = {};
  const fw = { __TAURI_INTERNALS__: { invoke: finvoke }, __ccShellFlags: { nativeFullscreen: true }, Element: El, Document: Doc,
    addEventListener: (t, f) => { (wls[t] = wls[t] || []).push(f); }, open() {} };
  fw.top = fw;
  vm.runInContext(fs.readFileSync(path.join(HERE, '../bridge/bridge.js'), 'utf8'), vm.createContext({ window: fw, document: fdoc,
    Event: Ev, setTimeout, clearTimeout, setInterval, clearInterval, Promise, Uint8Array, ArrayBuffer, DataView, Error, TypeError,
    Object, Array, String, JSON, encodeURIComponent, TextEncoder, TextDecoder }));
  let changes = 0;
  fdoc.addEventListener('fullscreenchange', () => { changes++; });
  ok(fw.cadcraftDesktop && !fdoc.ls.fullscreenchange.some((f) => /cc_set_fullscreen/.test(String(f))),
    'macOS: no element-fullscreen → window toggle (the GH.1 double path)');
  ok(fdoc.fullscreenEnabled === true && fdoc.fullscreenElement === null, 'macOS: fullscreenEnabled, nothing full screen yet');
  const root = fdoc.documentElement;
  const p1 = root.requestFullscreen({ navigationUI: 'hide' });
  const p2 = root.requestFullscreen();
  ok(p1 === p2, 'macOS: a second request while one is on its way is the same request');
  await p1;
  ok(winFull && fcalls.some((c) => c.cmd === 'cc_set_fullscreen' && c.args.on === true), 'macOS: requestFullscreen → the window\'s own full screen');
  ok(webkitAsked === 0, 'macOS: WebKit\'s element fullscreen is not used for the document');
  ok(fdoc.fullscreenElement === root && changes === 1, 'macOS: fullscreenElement = the document element, one fullscreenchange');
  await new Promise((r) => setTimeout(r, 1100));
  ok(fdoc.fullscreenElement === root && changes === 1, 'macOS: it stays full screen (no immediate exit)');
  await fdoc.exitFullscreen();
  ok(!winFull && fdoc.fullscreenElement === null && changes === 2, 'macOS: exitFullscreen → window back, fullscreenchange');
  let rej = null;
  try { await fdoc.exitFullscreen(); } catch (x) { rej = x; }
  ok(rej instanceof TypeError, 'macOS: exitFullscreen when not full screen rejects, as in a browser');
  // The person leaves with the green button / Window menu: noticed on the resize and reported.
  await root.requestFullscreen();
  winFull = false;
  wls.resize.forEach((f) => f());
  await new Promise((r) => setTimeout(r, 400));
  ok(fdoc.fullscreenElement === null && changes === 4, 'macOS: leaving with the green button → fullscreenchange, fullscreenElement null');
  // Without a resize the one-second check notices too.
  await root.requestFullscreen();
  winFull = false;
  await new Promise((r) => setTimeout(r, 1200));
  ok(fdoc.fullscreenElement === null && changes === 6, 'macOS: and without a resize, within a second');
  // Esc leaves full screen.
  await root.requestFullscreen();
  wls.keydown.forEach((f) => f({ key: 'Escape' }));
  await new Promise((r) => setTimeout(r, 20));
  ok(!winFull && fdoc.fullscreenElement === null && changes === 8, 'macOS: Esc leaves full screen');
  wls.keydown.forEach((f) => f({ key: 'Escape' }));
  ok(changes === 8, 'macOS: Esc when not full screen does nothing');
  // A window that never goes full screen: the request is refused (app-shell.js says so) and nothing changes.
  const stuck = async (cmd, args) => (cmd === 'cc_is_fullscreen' ? false : finvoke(cmd, args));
  fw.__TAURI_INTERNALS__.invoke = stuck;
  rej = null;
  try { await root.requestFullscreen(); } catch (x) { rej = x; }
  fw.__TAURI_INTERNALS__.invoke = finvoke;
  ok(rej instanceof TypeError && fdoc.fullscreenElement === null && changes === 8, 'macOS: a window that does not go full screen → rejected, no change');
  // Other elements keep WebKit's own element fullscreen.
  await new El(fdoc).requestFullscreen();
  ok(webkitAsked === 1, 'macOS: other elements → WebKit\'s own element fullscreen');
}

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

// LAN render nodes (0.7 CU.7): cadcraftDesktop.renderNodes on two bridges joined by a stand-in network — a node that
// advertises and accepts, a client that connects, bytes both ways, order, buffering before onmessage, one close, no
// renderNodes outside Studio's desktop windows.
{
  console.log('cadcraftDesktop.renderNodes (Studio)');
  const net = { nextId: 1, conns: new Map(), nodes: new Map(), nodeWait: null, ads: [] };
  const rec = (k, b = new Uint8Array(0)) => { const r = new Uint8Array(5 + b.length); r[0] = k; new DataView(r.buffer).setUint32(1, b.length); r.set(b, 5); return r; };
  const q = () => { const o = { items: [], wake: null, push(x) { o.items.push(x); if (o.wake) { const w = o.wake; o.wake = null; w(); } }, async take() { if (!o.items.length) await new Promise((r) => { o.wake = r; }); return o.items.splice(0); } }; return o; };
  const shell = (name) => {
    const calls = [];
    const mine = new Set();
    const invoke2 = async (cmd, args, opts) => {
      calls.push({ cmd, args, opts });
      switch (cmd) {
        case 'cc_render_advertise': net.ads.push({ name, tag: args.args.tag }); net.nodes.set(name, { accept: q() }); return { port: 9000 };
        case 'cc_render_unadvertise': { const n = net.nodes.get(name); if (n) { n.accept.push(null); net.nodes.delete(name); } return null; }
        case 'cc_render_accept': { const n = net.nodes.get(name); if (!n) return null; const [id] = await n.accept.take(); return id; }
        case 'cc_render_browse': return [{ tag: 'K7', name: 'K7', host: '192.168.1.20', port: 9000 }];
        case 'cc_render_connect': {
          if (args.args.host === '8.8.8.8') throw 'not_allowed: this network\'s computers only';
          const a = net.nextId++, b = net.nextId++;   // a = the client's end, b = the node's
          const ca = { inbox: q(), peer: b, owner: name }, cb = { inbox: q(), peer: a, owner: 'node' };
          net.conns.set(a, ca); net.conns.set(b, cb);
          ca.inbox.push(rec(0)); cb.inbox.push(rec(0));
          net.nodes.get('node').accept.push(b);
          mine.add(a); return a;
        }
        case 'cc_render_recv': {
          const c = net.conns.get(args.id); if (!c) return rec(2, new TextEncoder().encode('{"reason":"gone","code":0}')).buffer;
          const all = await c.inbox.take(); const n = all.reduce((x, y) => x + y.length, 0); const u = new Uint8Array(n); let p = 0;
          for (const r of all) { u.set(r, p); p += r.length; }
          return u.buffer;
        }
        case 'cc_render_send': {
          const c = net.conns.get(Number(opts.headers['x-cc-render'])); if (!c) throw 'closed: no such connection';
          await new Promise((r) => setTimeout(r, 3));
          net.conns.get(c.peer).inbox.push(rec(1, new Uint8Array(args)));
          return null;
        }
        case 'cc_render_close': { const c = net.conns.get(args.id); if (c) { net.conns.delete(args.id); const o = net.conns.get(c.peer); if (o) o.inbox.push(rec(2, new TextEncoder().encode('{"reason":"closed","code":1000}'))); } return null; }
        default: return invoke(cmd, args, opts);
      }
    };
    return { calls, invoke2 };
  };
  const mkRn = (flags, sh) => {
    const w = { __TAURI_INTERNALS__: { invoke: sh.invoke2 }, __ccShellFlags: flags };
    w.top = w;
    vm.runInContext(fs.readFileSync(path.join(HERE, '../bridge/bridge.js'), 'utf8'),
      vm.createContext({ window: w, document: { addEventListener() {} }, Uint8Array, ArrayBuffer, DataView, Error, TypeError, Object, Array, String, JSON, encodeURIComponent, TextEncoder, TextDecoder, Math, Number, setTimeout, setInterval, clearInterval, Promise }));
    return w.cadcraftDesktop;
  };
  const wait = async (f, ms = 3000) => { const t0 = Date.now(); while (!f() && Date.now() - t0 < ms) await new Promise((r) => setTimeout(r, 5)); return f(); };
  const studio = { account: true, suite: true, live: true };
  ok(!('renderNodes' in mkRn({ account: true, suite: false, live: true }, shell('x'))) && !('renderNodes' in mkRn({ account: true, suite: true, live: false }, shell('x'))), 'renderNodes only in Studio builds with the live feature');
  // Tablets run the bridge's hub-side view code, which needs a page: test/tablet.mjs covers that in Chromium; here the source.
  ok(/mobile\) delete bridge\.renderNodes/.test(fs.readFileSync(path.join(HERE, '../bridge/bridge.js'), 'utf8')), 'and the bridge removes it on tablets (computers only; the Rust side is cfg(desktop) too)');

  const nodeSh = shell('node'), cliSh = shell('client');
  const N = mkRn(studio, nodeSh).renderNodes, C = mkRn(studio, cliSh).renderNodes;
  ok(Object.isFrozen(N) && ['advertise', 'stopAdvertising', 'onConnection', 'browse', 'onNodes', 'connect'].every((k) => typeof N[k] === 'function'), 'the contract: six functions, frozen');

  // The node offers; a connection that arrives before anyone listens waits for the first listener.
  await N.advertise('k7');
  ok(nodeSh.calls.find((c) => c.cmd === 'cc_render_advertise').args.args.tag === 'k7', 'advertise passes the tag (the shell checks and upper-cases it)');
  const t = await C.connect({ host: '192.168.1.20', port: 9000 });
  ok(cliSh.calls.find((c) => c.cmd === 'cc_render_connect').args.args.host === '192.168.1.20' && cliSh.calls.find((c) => c.cmd === 'cc_render_connect').args.args.port === 9000, 'connect passes host and port');
  const nodeEnd = []; let nodeT = null;
  const off = N.onConnection((tr) => { nodeT = tr; tr.onmessage = (b) => nodeEnd.push(Array.from(b).join('.')); });
  ok(await wait(() => nodeT), 'the node\'s page is handed the transport (a late listener still gets it)');
  const back = []; t.onmessage = (b) => back.push(b[0]);

  // Bytes: order kept, one send in flight at a time, both ways.
  for (let i = 1; i <= 12; i++) t.send(new Uint8Array([i, 0, 255]));
  ok(await wait(() => nodeEnd.length === 12), 'twelve messages reach the node');
  ok(nodeEnd.map((x) => Number(x.split('.')[0])).join() === Array.from({ length: 12 }, (_, i) => i + 1).join() && nodeEnd[0] === '1.0.255', 'in order, bytes untouched');
  nodeT.send(new Uint8Array([42]));
  ok(await wait(() => back.length === 1) && back[0] === 42, 'and back to the client');

  // The first client leaves; the next one connects (the node hands its page a new transport for it).
  const sendsBefore = cliSh.calls.filter((c) => c.cmd === 'cc_render_send').length;
  t.close(); t.send(new Uint8Array([9]));
  ok(cliSh.calls.filter((c) => c.cmd === 'cc_render_send').length === sendsBefore && cliSh.calls.some((c) => c.cmd === 'cc_render_close'), 'close() tells the shell; nothing is sent after it');
  const first = nodeT;
  const lateSh = shell('late'); const LN = mkRn(studio, lateSh).renderNodes;
  const t2 = await LN.connect({ host: '10.0.0.5', port: 9000 });
  ok(await wait(() => nodeT !== first), 'a second peer → a second transport for the node\'s page');
  // Messages that arrive before onmessage is set wait for it; the close comes after the last of them, once.
  nodeT.send(new Uint8Array([7])); nodeT.send(new Uint8Array([8]));
  await new Promise((r) => setTimeout(r, 40));
  const early = []; let closes = 0, closedAfter = null;
  t2.onclose = () => { closes++; closedAfter = early.length; };
  nodeT.close();
  await new Promise((r) => setTimeout(r, 40));
  ok(closes === 0, 'onclose waits while messages are still undelivered');
  t2.onmessage = (b) => early.push(b[0]);
  ok(await wait(() => closes === 1), 'the other end leaving → onclose');
  ok(early.join() === '7,8' && closedAfter === 2, 'the early messages arrive first, the close after the last one');
  await new Promise((r) => setTimeout(r, 40));
  ok(closes === 1, 'onclose fires once');

  // Errors carry the shell's code; browse and onNodes.
  let err = null; try { await C.connect({ host: '8.8.8.8', port: 9000 }); } catch (e) { err = e; }
  ok(err && err.code === 'not_allowed', 'a refusal from the shell → err.code', err && err.message);
  ok((await C.browse())[0].tag === 'K7', 'browse() → the nodes found');
  const lists = []; const unsub = C.onNodes((l) => lists.push(l));
  ok(await wait(() => lists.length === 1) && lists[0][0].host === '192.168.1.20', 'onNodes delivers the first list at once');
  unsub();
  off();
  await N.stopAdvertising();
  ok(nodeSh.calls.some((c) => c.cmd === 'cc_render_unadvertise'), 'stopAdvertising reaches the shell');
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
