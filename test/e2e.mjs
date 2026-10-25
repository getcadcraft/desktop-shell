// SPDX-License-Identifier: Apache-2.0
// e2e.mjs — the desktop shell end to end on Linux (WebKitGTK), driven through WebKitWebDriver: the real shell binary
// (debug build, so dialogs take answers from CC_TEST_ANSWERS), a real browser edition, a real DWG opened as if by
// double-click. Checks what W.1 asks (WASM, module workers, IndexedDB, secure context, COOP/COEP for PartCraft,
// downloads, fullscreen, service worker) and the bridge (W.2, W2.15) incl. the grant rules and host.js on top of it.
//
//   xvfb-run -a node test/e2e.mjs [--edition <dir>] [--partcraft <dir>] [--suite <dir>] [--shots <dir>]
// Needs: cargo build (target/debug/cadcraft-desktop), WebKitWebDriver (apt webkit2gtk-driver), and the site's checkout:
//   CADCRAFT_SITE    the site's folder (editions packed by its packer, the test DWG, agents' MCP checker, the sign-in helper)
//   CADCRAFT_STUDIO  the studio repo's folder (its packed suite, out/studio-suite/vdev); unset = the Studio checks skip
// Windows (WebView2) is checked by hand on real machines: docs, W.8.

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawn, execFileSync } from 'node:child_process';

const HERE = path.dirname(new URL(import.meta.url).pathname);
const ROOT = path.resolve(HERE, '..');
const SITE = process.env.CADCRAFT_SITE;
if (!SITE) { console.error('CADCRAFT_SITE is not set: point it at the site\'s folder (this test needs its editions and fixtures)'); process.exit(1); }
const arg = (k, d) => { const i = process.argv.indexOf(k); return i > 0 ? process.argv[i + 1] : d; };
const EDITION = arg('--edition', `${SITE}/tmp/desktop-editions/cadcraft/browser-edition/vdev`);
const BUILD = (/\/v(\d+\.\d+(?:\.\d+)?)\/?$/.exec(EDITION) || [])[1] || 'dev';   // the edition's build folder: dev or a release (0.4)
const SUITE = arg('--suite', process.env.CADCRAFT_STUDIO ? path.join(process.env.CADCRAFT_STUDIO, 'out/studio-suite/vdev') : '');
const PARTCRAFT = arg('--partcraft', `${SITE}/tmp/desktop-editions/partcraft/browser-edition/vdev`);
const SHOTS = arg('--shots', null);
const BIN = process.env.CC_E2E_BIN || path.join(ROOT, 'src-tauri/target/debug/cadcraft-desktop');
const DWG = `${SITE}/fixtures/dwg/kitchen-equipment.dwg`;

// Xvfb has no window manager. --fullscreen starts matchbox (apt matchbox-window-manager) for that check only: it
// maximizes every window, which would hide the pop-up sizes and places checked below.
const wm = process.argv.includes('--fullscreen') && process.env.DISPLAY && fs.existsSync('/usr/bin/matchbox-window-manager')
  ? spawn('/usr/bin/matchbox-window-manager', ['-use_titlebar', 'no'], { stdio: 'ignore' }) : null;

let pass = 0, fail = 0;

const ok = (cond, what, extra = '') => {
  if (cond) { pass++; console.log(`  ✓ ${what}`); } else { fail++; console.log(`  ✗ ${what} ${extra}`); }
};

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'cc-desktop-e2e-'));
const work = path.join(tmp, 'files');
fs.mkdirSync(work);
const answers = path.join(tmp, 'answers.txt');
const answer = (line) => fs.writeFileSync(answers, line + '\n');

// A Secret Service for the shell's credential store (headless boxes have none): gnome-keyring with a "login"
// collection, as a desktop session has, kept in tmp. Stopped again at the end (only the daemon started here).
// Started before any shell runs: the first credential-store lookup would otherwise auto-start a bare one.
let keyringPid = null;
if (process.argv.includes('--signin') && process.env.DBUS_SESSION_BUS_ADDRESS && fs.existsSync('/usr/bin/gnome-keyring-daemon')) {
  const env = { ...process.env, XDG_DATA_HOME: path.join(tmp, 'keyring') };
  const pids = () => { try { return execFileSync('pgrep', ['-x', 'gnome-keyring-d']).toString().split('\n').filter(Boolean); } catch { return []; } };
  const before = new Set(pids());
  execFileSync('/usr/bin/gnome-keyring-daemon', ['--login'], { env, input: 'e2e', stdio: ['pipe', 'ignore', 'ignore'] });
  execFileSync('/usr/bin/gnome-keyring-daemon', ['--start', '--components=secrets'], { env, stdio: 'ignore' });
  keyringPid = pids().filter((p) => !before.has(p));
  await new Promise((r) => setTimeout(r, 1500)); // let it register on the bus
  try { execFileSync('gdbus', ['call', '--session', '--dest', 'org.freedesktop.secrets', '--object-path', '/org/freedesktop/secrets',
    '--method', 'org.freedesktop.Secret.Service.SetAlias', 'default', '/org/freedesktop/secrets/collection/login'], { stdio: 'ignore' }); } catch { /* already the default */ }
}


function stage(name, edition, headers = {}) {
  const dir = path.join(tmp, 'stage-' + name);
  fs.cpSync(edition, path.join(dir, 'app'), { recursive: true, dereference: true });
  const rel = JSON.parse(fs.readFileSync(path.join(edition, 'release.json'), 'utf8'));
  fs.writeFileSync(path.join(dir, 'shell.json'), JSON.stringify({ app: name, name: rel.name || name, version: String(rel.release || 'dev'), headers }));
  return path.join(dir, 'app');
}

let PORT = 4400 + Math.floor(Math.random() * 400);
let BASE = `http://127.0.0.1:${PORT}`;
async function wd(method, url, body) {
  const r = await fetch(BASE + url, { method, headers: { 'content-type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
  const j = await r.json();
  if (j.value && j.value.error) throw new Error(`${url}: ${j.value.error}: ${j.value.message}`);
  return j.value;
}

let driverLog = '';
// The shell is single-instance (a second start hands its files to the running one and exits — a double-click), so a
// shell left over from the last section would swallow the next one's start. Make sure none is running first.
function stopShells() {
  const name = path.basename(BIN).slice(0, 15);
  for (let i = 0; i < 50; i++) {
    let pids = [];
    try { pids = execFileSync('pgrep', ['-x', name]).toString().split('\n').filter(Boolean); } catch { return; }
    for (const p of pids) { try { process.kill(Number(p), i < 20 ? 'SIGTERM' : 'SIGKILL'); } catch {} }
    execFileSync('sleep', ['0.2']);
  }
}
function startDriver(root, dataHome = path.join(tmp, 'data')) {
  stopShells();
  PORT += 1; BASE = `http://127.0.0.1:${PORT}`; // a fresh port: the last driver may still be letting go of its own
  // CC_LIVE_BIND (debug builds): Studio Live test sessions listen on loopback only, never on a public interface.
  const env = { ...process.env, TAURI_WEBVIEW_AUTOMATION: 'true', CADCRAFT_DESKTOP_ROOT: root, CC_TEST_ANSWERS: answers, CC_LIVE_BIND: '127.0.0.1',
    XDG_DATA_HOME: dataHome, XDG_CONFIG_HOME: path.join(tmp, 'config'), XDG_CACHE_HOME: path.join(tmp, 'cache') };
  const p = spawn('WebKitWebDriver', [`--port=${PORT}`], { env, stdio: ['ignore', 'pipe', 'pipe'] });
  driverLog = '';
  const keep = (d) => { driverLog = (driverLog + d).slice(-4000); };
  p.stdout.on('data', keep); p.stderr.on('data', keep);
  p.on('exit', (code, sig) => keep(`\n[WebKitWebDriver exited: ${code ?? sig}]`));
  return p;
}

async function session(args) {
  let up = false;
  for (let i = 0; i < 300; i++) { // up to 30 s: WebKitWebDriver starts slowly on a busy machine
    try { await fetch(BASE + '/status'); up = true; break; } catch { await new Promise((r) => setTimeout(r, 100)); }
  }
  if (!up) throw new Error(`WebKitWebDriver on port ${PORT} never answered: ${driverLog.trim().split('\n').slice(-5).join(' | ') || '(no output)'}`);
  const s = await wd('POST', '/session', { capabilities: { alwaysMatch: { 'webkitgtk:browserOptions': { binary: BIN, args } } } });
  const id = s.sessionId;
  await wd('POST', `/session/${id}/timeouts`, { script: 90000 });
  const run = async (body, ...a) => {
    const script = `const __done = arguments[arguments.length - 1];
      (async (...args) => { ${body} })(...Array.prototype.slice.call(arguments, 0, -1))
        .then((r) => __done({ ok: true, r }), (e) => __done({ ok: false, e: String((e && e.message) || e), code: e && e.code }));`;
    return wd('POST', `/session/${id}/execute/async`, { script, args: a });
  };
  const shot = async (name) => {
    if (!SHOTS) return;
    fs.mkdirSync(SHOTS, { recursive: true });
    fs.writeFileSync(path.join(SHOTS, name + '.png'), Buffer.from(await wd('GET', `/session/${id}/screenshot`), 'base64'));
  };
  return { id, run, shot, end: () => wd('DELETE', `/session/${id}`) };
}

const waitFor = async (fn, ms = 20000) => {
  const t = Date.now();
  while (Date.now() - t < ms) { if (await fn()) return true; await new Promise((r) => setTimeout(r, 200)); }
  return false;
};

/**
 * Studio Live (studio_live.rs, cadcraftDesktop.live): the CadCraft window hosts a session in-process; a Node client joins
 * over the LAN with the code (and a signed-in name: join.aud must be its connection's audience); a second client joins
 * from the same window through the shell's own socket ('join' transport); the hub finds the session by mDNS (code tag and
 * an invite room's tag); the session port serves the suite to browser joiners (LV.12); cloud.request takes the new paths
 * only; ending stops the server. Uses the app's own live-client.js / live-crypto.js and a small model (no app adapter).
 */
async function studioLive(s, appHandle, hubHandle) {
  console.log('Studio Live');
  const go = (h) => wd('POST', `/session/${s.id}/window`, { handle: h });
  await go(appHandle);
  const setup = await s.run(`const desk = window.cadcraftDesktop;
    const main = document.querySelector('script[type="module"][src*="/assets/apps/"]');
    const folder = new URL('.', new URL(main.getAttribute('src'), document.baseURI)).href;
    const L = await import(folder + 'live-client.js'), C = await import(folder + 'live-crypto.js');
    const until = async (f, ms = 15000) => { const t = Date.now(); while (!f()) { if (Date.now() - t > ms) throw new Error('timed out'); await new Promise((r) => setTimeout(r, 20)); } };
    const toy = () => { const model = new Map(); let cb = null; const a = { app: 'cadcraft', schema: 1, model,
      onLocalChange(f) { cb = f; }, edit(label, ops) { const inverse = a.invert(ops); a.apply(ops); return cb && cb({ label, ops, inverse }); },
      invert(ops) { return ops.map((o) => (o.op === 'add' ? { op: 'del', id: o.id } : { op: 'add', id: o.id, obj: model.get(o.id) || {} })).reverse(); },
      apply(ops) { for (const o of ops) { if (o.op === 'add') model.set(o.id, o.obj); else model.delete(o.id); } },
      touched(ops) { return ops.map((o) => ({ k: 'e:' + o.id, m: o.op === 'add' ? 'c' : 'd' })); },
      snapshot() { return L.pack([...model.entries()]); }, load(b) { model.clear(); for (const [k, v] of L.unpack(b)) model.set(k, v); },
      keys() { return [...model.keys()].sort().join(','); } }; return a; };
    const a = toy(), joins = [], codes = [];
    const H = new L.LiveClient({ transport: desk.live.transport('local', { app: 'cadcraft', schema: 1 }), adapter: a, name: 'Host', role: 'host', secure: false });
    H.on('join', (b) => joins.push(b)); H.on('code', (b) => codes.push(b));
    H.connect();
    await until(() => H.status === 'live');
    H.invite({});
    await until(() => codes.length);
    a.edit('add', [{ op: 'add', id: 'h1', obj: { n: 1 } }]);
    const sess = await desk.live.session();
    let second = null; try { desk.live.transport('local', {})({ open() {}, message() {}, close(r) { second = r; } }); await until(() => second !== null, 5000); } catch { second = 'threw'; }
    await desk.live.advertise(codes[0].tag, { app: 'cadcraft' });
    await desk.live.advertise({ room: 'K7QX9M2B' }, { app: 'cadcraft' });
    window.__liveE2E = { L, C, toy, until, H, a, joins, desk };
    return { code: codes[0].code, tag: codes[0].tag, sess, second, roomTag: C.roomTag('K7QX9M2B') };`);
  if (!setup.ok) { ok(false, 'the app window hosts a session', setup.e); return; }
  const { code, tag, sess, second, roomTag } = setup.r;
  ok(sess.running && sess.port > 0 && sess.app === 'cadcraft' && JSON.stringify(sess.addrs) === '["127.0.0.1"]',
    'transport(\'local\') starts this window\'s session: session() → running, port, app; bound to loopback here (CC_LIVE_BIND)', JSON.stringify(sess));
  // Nothing of the test session is reachable on this machine's other interfaces (Studio itself binds 0.0.0.0 for the LAN).
  const outward = Object.values(os.networkInterfaces()).flat().filter((i) => i && i.family === 'IPv4' && !i.internal).map((i) => i.address);
  const reach = await Promise.all(outward.map((a) => fetch(`http://${a}:${sess.port}/apps/cadcraft/`, { signal: AbortSignal.timeout(2000) }).then(() => a, () => null)));
  ok(outward.length === 0 || reach.every((x) => x === null), 'the test session does not answer on ' + (outward.join(', ') || 'any other interface'), reach.filter(Boolean).join(', '));
  ok(/busy/.test(String(second)), 'one session per window: a second local transport is refused', String(second));
  const url = `ws://${sess.addrs[0]}:${sess.port}/live`;
  // LV.12: the session port serves Studio's suite to browser joiners, pages marked cc-live.
  const page = await fetch(`http://${sess.addrs[0]}:${sess.port}/apps/cadcraft/`).then(async (r) => ({ st: r.status, t: await r.text() })).catch((e) => ({ st: 0, t: String(e) }));
  ok(page.st === 200 && page.t.includes('name="cc-live"'), 'the session port serves the suite to browser joiners (cc-live page)', String(page.st));
  // A second client joins by LAN: a Node process with the code, signed in (a stand-in assertion).
  await import(path.join(SITE, 'tools/live-crypto/node-hook.mjs'));
  const NC = await import(path.join(SITE, 'public_html/assets/js/live-crypto.js'));
  const NL = await import(path.join(SITE, 'public_html/assets/js/live-client.js'));
  const nm = new Map(); let nodeAud = null;
  const na = { app: 'cadcraft', schema: 1, onLocalChange(f) { na.cb = f; }, invert: (ops) => ops.map((o) => ({ op: 'del', id: o.id })), apply(ops) { for (const o of ops) o.op === 'add' ? nm.set(o.id, o.obj) : nm.delete(o.id); },
    touched: (ops) => ops.map((o) => ({ k: 'e:' + o.id, m: o.op === 'add' ? 'c' : 'd' })), snapshot: () => NL.pack([...nm.entries()]), load(b) { nm.clear(); for (const [k, v] of NL.unpack(b)) nm.set(k, v); } };
  const J = new NL.LiveClient({ transport: NL.webSocketTransport(url), adapter: na, name: 'Node joiner', device: 'e2e', code, crypto: NC,
    signIn: async (aud) => { nodeAud = aud; return { assertion: 'v1.e2e.sig' }; } });
  J.connect();
  const card = (await s.run(`const E = window.__liveE2E; await E.until(() => E.joins.length); const b = E.joins[0]; E.H.approve(b.c, 'editor');
    return { c: b.c, name: b.name, via: b.via, aud: b.aud, assertion: b.assertion };`)).r;
  ok(card && card.name === 'Node joiner' && card.via === 'lan', 'the host sees the approve card (on this network)', JSON.stringify(card));
  ok(card && nodeAud && card.aud === nodeAud && card.assertion === 'v1.e2e.sig', 'join carries the signed-in assertion and this connection\'s aud (LV.19)');
  ok(await waitFor(async () => J.status === 'live' && nm.has('h1'), 15000), 'the LAN joiner is in, with the host\'s model (snapshot from the host window)', J.status);
  const inc = na.cb({ label: 'add', ops: [{ op: 'add', id: 'n1', obj: { n: 2 } }], inverse: [{ op: 'del', id: 'n1' }] }) && nm.set('n1', { n: 2 });
  ok(inc && await waitFor(async () => (await s.run(`return window.__liveE2E.a.keys();`)).r === 'h1,n1', 10000), 'its edit reaches the host');
  // A Studio joiner: the shell opens the socket ('join' transport), the page does the handshake.
  const sj = await s.run(`const E = window.__liveE2E; const b = E.toy(); const n = E.joins.length;
    const J2 = new E.L.LiveClient({ transport: E.desk.live.transport('join', { url: ${JSON.stringify(url)} }), adapter: b, name: 'Studio joiner', code: ${JSON.stringify(code)}, crypto: E.C });
    J2.connect();
    await E.until(() => E.joins.length > n); E.H.approve(E.joins[n].c, 'viewer');
    await E.until(() => J2.status === 'live' && b.keys() === 'h1,n1');
    let bad = null; try { E.desk.live.transport('join', { url: 'ws://192.168.1.20:1/other' })({ open() {}, message() {}, close(r) { bad = r; } }); await E.until(() => bad !== null, 5000); } catch { bad = 'threw'; }
    E.J2 = J2; return { role: J2.role, keys: b.keys(), bad };`);
  ok(sj.ok && sj.r.role === 'viewer' && sj.r.keys === 'h1,n1', 'a Studio joiner through the shell\'s socket (join transport) gets in and catches up', JSON.stringify(sj.r || sj.e));
  ok(sj.ok && /not_allowed/.test(sj.r.bad), 'the join transport opens only live addresses', sj.r && sj.r.bad);
  // mDNS from the hub: the code's tag, and an invite room's tag (LV.26).
  await go(hubHandle);
  const md = (await s.run(`const L = window.cadcraftDesktop.live;
    const t0 = Date.now(); const a = await L.browse(${JSON.stringify(tag)}, 3000); const ms = Date.now() - t0;
    const b = await L.browse({ room: 'K7QX9M2B' }, 3000);
    const c = await L.browse('ZZ', 400);
    let hubSession = null; try { await L.session(); hubSession = 'given'; } catch (e) { hubSession = e.code; }
    const C = window.cadcraftDesktop.cloud; const code = async (p) => { try { await C.request({ path: p }); return 'ok'; } catch (e) { return e.code; } };
    return { a, b, c, ms, hubSession, avatar: await code('/api/v1/me/avatar?size=64'), check: await code('/api/v1/live/check?app=cadcraft'), lib: await code('/api/v1/library/items') };`)).r;
  ok(md && md.a.some((f) => f.port === sess.port && f.app === 'cadcraft'), `mDNS: the hub finds the session by its code's tag (${md && md.ms} ms)`, JSON.stringify(md && md.a));
  ok(md && md.b.some((f) => f.port === sess.port), 'mDNS: and by an invite room\'s tag (LV.26)', JSON.stringify(md && md.b));
  ok(md && md.c.length === 0, 'an unknown tag finds nothing');
  ok(md && md.hubSession === 'not_allowed', 'only app windows host');
  ok(md && md.avatar !== 'not_allowed' && md.check !== 'not_allowed' && md.lib === 'not_allowed', 'cloud.request takes /api/v1/me/avatar and /api/v1/live/* (signed out here), still not the library', JSON.stringify(md));
  // The host ends: everyone is told, the server stops with the window's host connection.
  await go(appHandle);
  const end = (await s.run(`const E = window.__liveE2E; E.H.end(); await E.until(() => E.H.final && E.J2.final);
    await new Promise((r) => setTimeout(r, 300)); return { h: E.H.status, j2: E.J2.status, s: await E.desk.live.session() };`)).r;
  ok(end && end.h === 'ended' && end.j2 === 'ended' && end.s.running === false, 'End: everyone ends, the window\'s session is gone', JSON.stringify(end));
  ok(await waitFor(async () => J.final, 10000) && J.status === 'ended', 'the LAN joiner ended too (keeps its copy)', J.status);
  J.stop();
  // The server itself stops a few seconds after (so `end` reached everyone): the port no longer answers.
  await new Promise((r) => setTimeout(r, 4000));
  const gone = await fetch(`http://${sess.addrs[0]}:${sess.port}/apps/cadcraft/`).then(() => false, () => true);
  ok(gone, 'the session port is closed after the end');

  // Host handoff (LV.17) into Studio: a headless host (live-server) hands its session to this window, which carries on from
  // the state with exportState's twin startFrom (a real session server in the shell); the old host follows with its ticket.
  const TOK = 'e2e-' + Math.random().toString(36).slice(2);
  const srvBin = path.join(SITE, 'tools/live-test/server/target/release/live-server');
  if (!fs.existsSync(srvBin)) { console.log('  · (no live-server: the handoff checks skip; cargo build --release in tools/live-test/server)'); return; }
  const old = spawn(srvBin, ['--bind', '127.0.0.1:0', '--app', 'cadcraft', '--token', TOK, '--auto-approve'], { stdio: ['pipe', 'pipe', 'ignore'] });
  try {
    const oldAddr = JSON.parse(await new Promise((r) => { let b = ''; old.stdout.on('data', (d) => { b += d; if (b.includes('\n')) r(b.split('\n')[0]); }); })).addr;
    const om = new Map();
    const oa = { app: 'cadcraft', schema: 1, onLocalChange(f) { oa.cb = f; }, invert: (ops) => ops.map((o) => ({ op: 'del', id: o.id })), apply(ops) { for (const o of ops) o.op === 'add' ? om.set(o.id, o.obj) : om.delete(o.id); },
      touched: (ops) => ops.map((o) => ({ k: 'e:' + o.id, m: o.op === 'add' ? 'c' : 'd' })), snapshot: () => NL.pack([...om.entries()]), load(b) { om.clear(); for (const [k, v] of NL.unpack(b)) om.set(k, v); } };
    const N = new NL.LiveClient({ transport: NL.webSocketTransport(`ws://${oldAddr}/live`), adapter: oa, name: 'Old host', role: 'host', token: TOK, secure: false, crypto: NC });
    const ncodes = []; N.on('code', (b) => ncodes.push(b.code));
    N.connect();
    if (!(await waitFor(async () => N.status === 'live', 10000))) { ok(false, 'a headless host'); return; }
    N.invite({});
    await waitFor(async () => ncodes.length > 0, 5000);
    oa.cb({ label: 'add', ops: [{ op: 'add', id: 'o1', obj: {} }], inverse: [{ op: 'del', id: 'o1' }] }); om.set('o1', {});
    const joined = (await s.run(`const E = window.__liveE2E; const b = E.toy();
      const J = new E.L.LiveClient({ transport: E.desk.live.transport('join', { url: ${JSON.stringify(`ws://${oldAddr}/live`)} }), adapter: b, name: 'Studio', code: ${JSON.stringify(ncodes[0] || '')}, crypto: E.C,
        relocate: (url) => E.desk.live.transport('join', { url }),
        takeOver: async (state) => { const st = await E.desk.live.startFrom(state); return { addrs: st.addrs, port: st.port, transport: E.desk.live.transport('local', { app: 'cadcraft', schema: 1 }) }; } });
      J.connect(); await E.until(() => J.status === 'live' && b.keys() === 'o1'); E.J = J; E.jb = b; return J.c;`)).r;
    ok(typeof joined === 'number', 'this window joins a headless host with a code');
    N.handover(joined);
    const took = (await s.run(`const E = window.__liveE2E; await E.until(() => E.J.role === 'host' && E.J.status === 'live', 20000);
      return { role: E.J.role, s: await E.desk.live.session() };`)).r;
    ok(took && took.role === 'host' && took.s.running && took.s.port > 0, 'handed over: startFrom runs the session in the shell and this window is its host', JSON.stringify(took));
    ok(await waitFor(async () => N.relocated && N.status === 'live' && N.role === 'editor', 15000), 'the old host follows to Studio with its ticket, as an editor', N.status + ' ' + N.role);
    oa.cb({ label: 'add', ops: [{ op: 'add', id: 'o2', obj: {} }], inverse: [{ op: 'del', id: 'o2' }] }); om.set('o2', {});
    ok(await waitFor(async () => (await s.run(`return window.__liveE2E.jb.keys();`)).r === 'o1,o2', 10000), 'its edit reaches the new host in Studio');
    const st2 = (await s.run(`const E = window.__liveE2E; E.J.end(); await E.until(() => E.J.final); await new Promise((r) => setTimeout(r, 300)); return await E.desk.live.session();`)).r;
    ok(st2 && st2.running === false && await waitFor(async () => N.final, 10000), 'End from the new host ends it for everyone');
    N.stop();
  } finally { old.stdin.end(); old.kill(); }
}

// ---------------------------------------------------------------------------------------------------------- CadCraft
const launched = path.join(work, 'kitchen equipment.dwg');
fs.copyFileSync(DWG, launched);
const dwgSize = fs.statSync(launched).size;
let rectBefore = null;
const ccRoot = stage('cadcraft', EDITION);
let driver = startDriver(ccRoot);
try {
  console.log('CadCraft edition in the shell');
  const s = await session([launched]);
  ok(await waitFor(async () => (await s.run(`return document.readyState === 'complete' && !!document.querySelector('main');`)).r), 'page loads');
  const env = (await s.run(`return {href: location.href, secure: isSecureContext, subtle: !!(crypto && crypto.subtle),
    bridge: window.cadcraftDesktop && window.cadcraftDesktop.version, frozen: Object.isFrozen(window.cadcraftDesktop),
    tauriGlobal: typeof window.__TAURI__, sw: 'serviceWorker' in navigator};`)).r;
  ok(env.href.startsWith('cadcraft://localhost/'), 'served by the cadcraft protocol', env.href);
  ok(env.secure && env.subtle, 'secure context (crypto.subtle)');
  ok(env.bridge === 1 && env.frozen, 'window.cadcraftDesktop v1, frozen');
  const bar = (await s.run(`const c = document.querySelector('.app-bar-area'); const o = document.querySelector('[data-offline]'); return {chip: c && c.textContent.trim(), desk: document.documentElement.classList.contains('is-desktop'), offline: !o || o.hidden};`)).r;
  ok(bar.chip === 'Desktop' && bar.desk && bar.offline, 'the app bar says Desktop, not Browser edition, and has no Save for offline (' + JSON.stringify(bar) + ')');
  ok(env.tauriGlobal === 'undefined', 'no window.__TAURI__ global (apps never talk to Tauri)');
  const info = (await s.run(`return window.cadcraftDesktop.info();`)).r;
  ok(info && info.app === 'cadcraft' && info.platform === 'linux', 'info()', JSON.stringify(info));

  const upd = (await s.run(`const u = window.cadcraftDesktop.updates; return {st: await u.status(), check: await u.check()};`)).r;
  ok(upd.st.available === false && upd.check === 'off', 'no update feed in this build → nothing is ever checked (W.7)', JSON.stringify(upd));

  const errs = (await s.run(`return [...document.querySelectorAll('[data-app-error], .app-error')].map(e => e.textContent).filter(Boolean);`)).r;
  ok(errs.length === 0, 'no app error banner', errs.join(' | '));
  await s.shot('01-cadcraft');

  // Libraries and headers through the protocol.
  const wasm = (await s.run(`
    const urls = performance.getEntriesByType('resource').map(e => e.name);
    const r = await fetch('./assets/apps/cadcraft/${BUILD}/manifest.json'); const m = await r.json();
    const ctype = r.headers.get('content-type');
    const nf = (await fetch('./assets/nope.js')).status;
    const up = (await fetch('./../../etc/passwd')).status;
    return {ctype, nf, up, wasm: m.wasm || null, n: urls.length};`)).r;
  ok(wasm.ctype.startsWith('application/json'), 'JSON served with its type');
  ok(wasm.nf === 404 && wasm.up === 404, 'missing files 404, no way out of the app folder');
  const wasmOk = (await s.run(`
    const bytes = new Uint8Array([0,97,115,109,1,0,0,0,1,7,1,96,2,127,127,1,127,3,2,1,0,7,7,1,3,97,100,100,0,0,10,9,1,7,0,32,0,32,1,106,11]);
    const m = await WebAssembly.instantiate(bytes);
    const f = [...document.querySelectorAll('script[type=importmap]')].map(s => s.textContent).join('');
    const w = (f.match(/"([^"]+\\.wasm)"/) || [])[1] || null;
    let streaming = null;
    if (w) { const r = await fetch(w); streaming = r.headers.get('content-type'); }
    return {add: m.instance.exports.add(2, 3), streaming};`)).r;
  ok(wasmOk.add === 5, 'WebAssembly runs');
  if (wasmOk.streaming) ok(wasmOk.streaming === 'application/wasm', '.wasm served as application/wasm');

  // Module worker: cc-dwg reads the launched DWG in its worker.
  // CadCraft calls host.launchFiles() itself at start (cad-files W2.18), so the launched DWG is the app's by now: it must
  // be open in the drawing. (Apps that don't take it leave it for us.)
  const took = await waitFor(async () => (await s.run(`const c = window.cadcraft; const n = c && c.doc && c.doc.entities ? (c.doc.entities.size ?? c.doc.entities.length ?? 0) : 0; return n > 0;`)).r, 30000);
  const launch = (await s.run(`return await window.cadcraftDesktop.launchFiles();`)).r;
  ok(took || (Array.isArray(launch) && launch[0] === launched), 'the file the app was started with opens in it (double-click, W.4)', JSON.stringify({ took, launch }));
  ok((await s.run(`return await window.cadcraftDesktop.launchFiles();`)).r.length === 0, 'launch files are handed over once');
  const read = await s.run(`
    const d = window.cadcraftDesktop;
    const bytes = await d.readFile(args[0]);
    const st = await d.stat(args[0]);
    const m = await import('./assets/apps/cadcraft/${BUILD}/dwg/read-async.js');
    const model = await m.readDwgAsync(bytes);
    return {n: bytes.length, isU8: bytes instanceof Uint8Array, st, worker: m.dwgWorkerState ? m.dwgWorkerState() : null,
      version: model && (model.version || model.header && model.header.version) || null, ents: model && model.entities ? model.entities.length : null};`, launched);
  ok(read.ok && read.r.n === dwgSize && read.r.isU8, 'readFile() of the launched DWG', read.e || '');
  ok(read.ok && read.r.st && read.r.st.size === dwgSize && read.r.st.mtime > 0, 'stat()');
  ok(read.ok && read.r.worker && /worker/i.test(JSON.stringify(read.r.worker)), 'DWG read in a module worker', JSON.stringify(read.r && read.r.worker));

  // Grants.
  const refused = await s.run(`return await window.cadcraftDesktop.readFile('/etc/hostname');`);
  ok(!refused.ok && refused.code === 'not_allowed', 'readFile of a file nobody opened is refused (not_allowed)', JSON.stringify(refused));
  const refusedW = await s.run(`return await window.cadcraftDesktop.writeFile(args[0], new Uint8Array([1]));`, path.join(work, 'sneaky.txt'));
  ok(!refusedW.ok && refusedW.code === 'not_allowed' && !fs.existsSync(path.join(work, 'sneaky.txt')), 'writeFile to an unchosen path is refused');
  const refusedL = await s.run(`return await window.cadcraftDesktop.listDir(args[0]);`, work);
  ok(!refusedL.ok && refusedL.code === 'not_allowed', 'listDir of an unchosen folder is refused');

  // Save in place (atomic) through host.js, as the apps will.
  const saved = await s.run(`
    const { getHost } = await import('./assets/apps/cadcraft/${BUILD}/host.js');
    const host = getHost({ app: 'cadcraft' });
    const [f] = (await host.launchFiles()).concat([]);
    const target = f || { path: args[0], name: 'x', writable: true };
    const r = await host.save({ target: { ...target, path: args[0] }, data: new TextEncoder().encode('saved in place'), name: 'kitchen equipment.dwg' });
    return {kind: host.kind, caps: host.caps, how: r.how, ok: r.ok};`, launched);
  ok(saved.ok && saved.r.kind === 'desktop' && saved.r.caps.saveInPlace, 'host.js runs as kind desktop');
  ok(saved.ok && saved.r.how === 'written' && fs.readFileSync(launched, 'utf8') === 'saved in place', 'host.save() writes the file in place', JSON.stringify(saved));
  ok(fs.readdirSync(work).every((n) => !n.endsWith('.tmp')), 'no temp file left beside it');

  // Dialogs (answers from CC_TEST_ANSWERS in this debug build).
  const other = path.join(work, 'other.dxf');
  fs.writeFileSync(other, '0\nEOF\n');
  answer(other);
  const opened = (await s.run(`return await window.cadcraftDesktop.openDialog({multiple: false, filters: [{name: 'Drawings', extensions: ['dwg', 'dxf']}]});`)).r;
  ok(opened && opened[0] === other, 'openDialog() → the picked path');
  ok((await s.run(`return (await window.cadcraftDesktop.readFile(args[0])).length;`, other)).r === 6, 'a picked file can be read');
  answer('');
  ok((await s.run(`return await window.cadcraftDesktop.openDialog({});`)).r === null, 'a cancelled dialog → null');
  const newFile = path.join(work, 'new plan.dxf');
  answer(newFile);
  const sv = await s.run(`const p = await window.cadcraftDesktop.saveDialog({defaultPath: 'Plan.dxf'}); await window.cadcraftDesktop.writeFile(p, new TextEncoder().encode('dxf')); return p;`);
  ok(sv.ok && sv.r === newFile && fs.readFileSync(newFile, 'utf8') === 'dxf', 'saveDialog() + writeFile() write a new file');

  // Folders (W2.15).
  fs.mkdirSync(path.join(work, 'set/sheets'), { recursive: true });
  fs.writeFileSync(path.join(work, 'set/Job.ccsheets'), '{}');
  fs.writeFileSync(path.join(work, 'set/sheets/A-101.dwg'), 'x');
  answer(path.join(work, 'set'));
  const folder = await s.run(`const d = window.cadcraftDesktop; const p = await d.openFolderDialog({title: 'Sheet set folder'});
    const top = await d.listDir(p); const sub = await d.listDir(p + '/sheets'); const b = await d.readFile(p + '/sheets/A-101.dwg');
    await d.writeFile(p + '/sheets/A-102.dwg', new Uint8Array([7])); return {p, top, sub, n: b.length};`);
  ok(folder.ok && folder.r.p === path.join(work, 'set'), 'openFolderDialog() → the folder');
  ok(folder.ok && JSON.stringify(folder.r.top) === JSON.stringify([{ name: 'sheets', dir: true }, { name: 'Job.ccsheets', dir: false }]), 'listDir(): folders first, then names', JSON.stringify(folder.r && folder.r.top));
  ok(folder.ok && folder.r.n === 1 && fs.existsSync(path.join(work, 'set/sheets/A-102.dwg')), 'files inside a chosen folder can be read and written');
  const esc = await s.run(`return await window.cadcraftDesktop.listDir(args[0] + '/../');`, path.join(work, 'set'));
  ok(!esc.ok && esc.code === 'not_allowed', 'no ".." out of a chosen folder');

  // Downloads → a save dialog.
  const dl = path.join(work, 'export.csv');
  answer(dl);
  await s.run(`const a = document.createElement('a'); a.href = URL.createObjectURL(new Blob(['a,b\\n1,2\\n'], {type: 'text/csv'}));
    a.download = 'export.csv'; document.body.append(a); a.click(); a.remove();`);
  ok(await waitFor(() => fs.existsSync(dl) && fs.readFileSync(dl, 'utf8') === 'a,b\n1,2\n', 10000), 'a download asks where to save and writes there');

  // Storage.
  const store = (await s.run(`
    localStorage.setItem('cc-e2e', '1');
    const db = await new Promise((res, rej) => { const r = indexedDB.open('cc-e2e', 1); r.onupgradeneeded = () => r.result.createObjectStore('s'); r.onsuccess = () => res(r.result); r.onerror = () => rej(r.error); });
    await new Promise((res) => { const t = db.transaction('s', 'readwrite'); t.objectStore('s').put({ok: 1}, 'k'); t.oncomplete = res; });
    const v = await new Promise((res) => { db.transaction('s').objectStore('s').get('k').onsuccess = (e) => res(e.target.result); });
    let sw = null; try { sw = !!(await navigator.serviceWorker.register('./sw.js', {scope: './'})); } catch (e) { sw = 'refused: ' + e.message; }
    return {ls: localStorage.getItem('cc-e2e'), idb: v && v.ok, sw};`)).r;
  ok(store.ls === '1' && store.idb === 1, 'localStorage and IndexedDB work');
  console.log(`  · service worker on the cadcraft protocol: ${store.sw} (not needed in the desktop app: everything is local)`);

  // Double-click while running: a second start hands its file to this window.
  await s.run(`window.__ccE2E = []; window.cadcraftDesktop.onOpenFiles((p) => window.__ccE2E.push(...p));`);
  const second = spawn(BIN, [other], { env: { ...process.env, CADCRAFT_DESKTOP_ROOT: '/nonexistent' }, stdio: 'ignore' });
  const got = await waitFor(async () => (await s.run(`return window.__ccE2E.length > 0;`)).r, 10000);
  ok(got && (await s.run(`return window.__ccE2E[0];`)).r === other, 'a second start (double-click) opens its file in the running window');
  second.kill();
  // Fullscreen needs a real click. Opt-in: under Xvfb + WebDriver, WebKitGTK's element fullscreen crashes the page
  // (with or without a window manager or compositing) — check on a real desktop instead.
  if (process.argv.includes('--fullscreen')) {
  let step = 'inject';
  try {
    await s.run(`const b = document.createElement('button'); b.id = 'cc-e2e-fs'; b.textContent = 'fs'; b.style.cssText = 'position:fixed;top:0;left:0;z-index:99999';
      b.onclick = () => document.documentElement.requestFullscreen().then(() => (b.dataset.fs = 'yes'), (e) => (b.dataset.fs = 'no: ' + e.message)); document.body.append(b);`);
    step = 'find';
    const el = await wd('POST', `/session/${s.id}/element`, { using: 'css selector', value: '#cc-e2e-fs' });
    step = 'click';
    await wd('POST', `/session/${s.id}/element/${Object.values(el)[0]}/click`, {});
    step = 'wait';
    await waitFor(async () => (await s.run(`return document.getElementById('cc-e2e-fs').dataset.fs || '';`)).r !== '', 5000);
    const fsr = (await s.run(`const r = document.getElementById('cc-e2e-fs').dataset.fs; await new Promise((res) => setTimeout(res, 500));
      const full = innerWidth >= screen.width && innerHeight >= screen.height; if (document.fullscreenElement) await document.exitFullscreen(); return {r, full};`)).r;
    ok(fsr.r === 'yes', 'Fullscreen API', fsr.r);
    ok(fsr.full, 'the window covers the screen in fullscreen');
  } catch (e) { ok(false, 'Fullscreen API', `(${step}) ${e.message}`); }
  }

  // W.4b: pop-ups (the panels window) open from a script without a click, take their size and place, and a place on a
  // screen that is gone is pulled back onto a connected one.
  const pop = await s.run(`
    const w = window.open('about:blank', 'cc-e2e-panels', 'popup,left=40,top=50,width=420,height=320');
    if (!w) return {opened: false};
    await new Promise((r) => setTimeout(r, 1500));
    const r = {opened: true, x: w.screenX, y: w.screenY, w: w.innerWidth, h: w.innerHeight, opener: w.opener === window};
    w.close();
    const far = window.open('about:blank', 'cc-e2e-far', 'popup,left=20000,top=20000,width=300,height=200');
    await new Promise((res) => setTimeout(res, 1500));
    r.far = far ? {x: far.screenX, y: far.screenY} : null; r.sw = screen.width; r.sh = screen.height;
    if (far) far.close();
    return r;`);
  ok(pop.ok && pop.r.opened && pop.r.opener, 'window.open without a click opens an app window (window.opener kept)', JSON.stringify(pop));
  ok(pop.ok && pop.r.w === 420 && pop.r.h === 320, 'it takes the size it asked for', JSON.stringify(pop.r));
  ok(pop.ok && Math.abs(pop.r.x - 40) <= 8 && Math.abs(pop.r.y - 50) <= 40, 'and the place it asked for', JSON.stringify(pop.r));
  ok(pop.ok && pop.r.far && pop.r.far.x < pop.r.sw && pop.r.far.y < pop.r.sh, 'a place off every screen is pulled back on screen', JSON.stringify(pop.r && pop.r.far));

  // DR.5: a pop-up its page closes goes away as a native window, not only its web view (WebDriver can't tell the two
  // apart, so the debug shell lists its own windows).
  const nativeWins = async () => (await s.run(`return await window.__TAURI_INTERNALS__.invoke('cc_test_windows');`)).r || [];
  const onlyMain = async () => waitFor(async () => (await nativeWins()).join() === 'main', 5000);
  ok(await onlyMain(), 'w.close() destroys the pop-up\'s window (DR.5)', JSON.stringify(await nativeWins()));
  // The movable panels (app-panels.js): dock the last panel of a panels window back, and "Put panels back".
  const blank = (await s.run(`const w = window.open('', 'cc-e2e-blank', 'popup=yes,width=300,height=200'); const r = !!w;
    await new Promise((res) => setTimeout(res, 1000)); if (w) w.close(); return r;`)).r;
  ok(blank, 'window.open(\'\') opens a window (about:blank; the panels window opens that way)');
  ok(await onlyMain(), 'and it closes as a window too', JSON.stringify(await nativeWins()));
  // A hand-off between apps opens a real page of ours (BimCraft's Takeoff → TakeoffCraft, ?handoff=<id>): the pop-up is
  // built on about:blank and the engine loads the page into it (built on the page itself, Studio 0.6 closed on Windows).
  const page = (await s.run(`const u = location.pathname + '?handoff=00000000-0000-4000-8000-000000000000';
    const w = window.open(u, '_blank'); if (!w) return null;
    let at = ''; for (let i = 0; i < 50 && !/handoff=/.test(at); i++) { await new Promise((r) => setTimeout(r, 200)); try { at = w.location.search; } catch { at = 'cross'; } }
    w.close(); return at;`)).r;
  ok(/handoff=0{8}-/.test(page || ''), 'window.open of one of our pages opens it, query kept', JSON.stringify(page));
  ok(await onlyMain(), 'and it closes as a window too', JSON.stringify(await nativeWins()));
  const movable = (await s.run(`return !!(window.ccShellPanels && window.ccShellPanels.panels().length);`)).r;
  if (!movable) ok(false, 'the edition has movable panels (DR.5 docking checks)');
  else {
    const open = async (all) => (await s.run(`const P = window.ccShellPanels; const ids = P.panels().map((p) => p.id);
      const n = P.openWindow(${all ? 'null' : '[ids[0]]'}, 'new'); await new Promise((r) => setTimeout(r, 1500));
      return {n, ids, wins: P.windows()};`)).r;
    let o = await open(false);
    const popped = (await nativeWins()).filter((l) => l.startsWith('popup-')).length;
    ok(o && o.wins.length === 1 && popped === 1, 'a panel popped out opens one panels window', JSON.stringify({ o, popped }));
    await s.run(`window.ccShellPanels.move(${JSON.stringify(o && o.ids[0])}, 'left'); await new Promise((r) => setTimeout(r, 500));`);
    ok(await onlyMain(), 'its last panel docked to a side → the panels window is gone (DR.5)', JSON.stringify(await nativeWins()));
    o = await open(true);
    ok(o && o.wins.length === 1 && (await nativeWins()).some((l) => l.startsWith('popup-')), 'every panel popped out again');
    await s.run(`window.ccShellPanels.closeWindow(); await new Promise((r) => setTimeout(r, 500));`);
    ok(await onlyMain(), '"Put panels back" → the panels window is gone (DR.5)', JSON.stringify(await nativeWins()));
    ok((await s.run(`return window.ccShellPanels.panels().every((p) => !p.inWindow);`)).r, 'and every panel is back in the main window');
    await s.run(`window.ccShellPanels.reset();`);
  }

  // Main window place, kept for the next start (checked below).
  await wd('POST', `/session/${s.id}/window/rect`, { x: 30, y: 40, width: 1100, height: 700 });
  await new Promise((r) => setTimeout(r, 800));
  rectBefore = await wd('GET', `/session/${s.id}/window/rect`);
  await s.shot('02-cadcraft-after');
  // Close the window as a person would (the shell keeps its place on a normal exit), then end the session.
  await wd('DELETE', `/session/${s.id}/window`).catch(() => {});
  await new Promise((r) => setTimeout(r, 1500));
  await s.end().catch(() => {});
} catch (e) {
  fail++; console.log('  ✗ ' + e.message);
} finally {
  driver.kill();
}


// ---------------------------------------------------------------------------------------------- CadCraft, started again
// This time shell.json names an update feed (W.7), at an address that does not exist: offered, and fails cleanly.
{
  const sj = path.join(path.dirname(ccRoot), 'shell.json');
  const cfg = JSON.parse(fs.readFileSync(sj, 'utf8'));
  cfg.updates = { endpoint: 'https://getcadcraft.com/nonexistent-cadcraft-feed/latest.json', pubkey: process.env.CC_TEST_PUBKEY || 'dW50cnVzdGVkIGNvbW1lbnQ6IHRlc3Q=', auto: false };
  fs.writeFileSync(sj, JSON.stringify(cfg));
}
driver = startDriver(ccRoot);
try {
  console.log('CadCraft started again');
  const s = await session([]);
  ok(await waitFor(async () => (await s.run(`return document.readyState === 'complete';`)).r), 'page loads');
  const rect = await wd('GET', `/session/${s.id}/window/rect`);
  ok(rectBefore && rect.width === rectBefore.width && rect.height === rectBefore.height, 'the main window comes back at its size (W.4b)', JSON.stringify({ before: rectBefore, after: rect }));
  ok(rectBefore && Math.abs(rect.x - rectBefore.x) <= 40 && Math.abs(rect.y - rectBefore.y) <= 60, 'and its place', JSON.stringify({ before: rectBefore, after: rect }));
  const again = await s.run(`return (await window.cadcraftDesktop.readFile(args[0])).length;`, path.join(work, 'other.dxf'));
  ok(again.ok && again.r === 6, 'a file opened last time still reopens (recent files after a restart)', JSON.stringify(again));
  const still = await s.run(`return await window.cadcraftDesktop.readFile('/etc/hostname');`);
  ok(!still.ok && still.code === 'not_allowed', 'and nothing else became reachable');
  ok((await s.run(`return localStorage.getItem('cc-e2e');`)).r === '1', 'storage survives a restart (same origin)');
  const u2 = await s.run(`const u = window.cadcraftDesktop.updates; const st = await u.status(); let c; try { c = await u.check(); } catch (e) { c = 'error: ' + e.message; } return {st, c};`);
  ok(u2.ok && u2.r.st.available === true && u2.r.st.auto === false, 'a build with a feed offers updates, automatic checks off by default', JSON.stringify(u2));
  ok(u2.ok && /^error: /.test(u2.r.c), 'a feed that is not there fails cleanly (nothing installed)', JSON.stringify(u2.r && u2.r.c));
  const u3 = (await s.run(`return await window.cadcraftDesktop.updates.setAuto(true);`)).r;
  ok(u3.auto === true, 'the person can turn automatic checks on');
  await s.end();
} catch (e) {
  fail++; console.log('  ✗ ' + e.message);
} finally {
  driver.kill();
}



// ------------------------------------------------------------------------- A build without sign-in (stores: account:false)
{
  const sj = path.join(path.dirname(ccRoot), 'shell.json');
  const cfg = JSON.parse(fs.readFileSync(sj, 'utf8'));
  delete cfg.updates;
  cfg.account = false;
  fs.writeFileSync(sj, JSON.stringify(cfg));
  driver = startDriver(ccRoot);
  try {
    console.log('A build without sign-in (account: false)');
    const t0 = Date.now();
    const s = await session([]);
    ok(await waitFor(async () => (await s.run(`return document.readyState === 'complete';`)).r), 'page loads');
    console.log(`  · window up and page loaded in ${((Date.now() - t0) / 1000).toFixed(1)} s`);
    const d = (await s.run(`const d = window.cadcraftDesktop; return {account: 'account' in d, cloud: 'cloud' in d, files: typeof d.readFile};`)).r;
    ok(!d.account && !d.cloud && d.files === 'function', 'no account and no cloud on the page; files work', JSON.stringify(d));
    const direct = await s.run(`return await window.__TAURI_INTERNALS__.invoke('cc_account_sign_in');`);
    ok(!direct.ok && /^off/.test(direct.e), 'the shell itself refuses sign-in', JSON.stringify(direct));
    const win = await s.run(`return await window.__TAURI_INTERNALS__.invoke('cc_account_window');`);
    ok(!win.ok && /^off/.test(win.e), 'and will not open the sign-in window', JSON.stringify(win));
    const cl = await s.run(`return await window.__TAURI_INTERNALS__.invoke('cc_cloud_request', new Uint8Array(0), {headers: {'x-cc-path': '/api/v1/me'}});`);
    ok(!cl.ok && /^off/.test(cl.e), 'nor reach the cloud', JSON.stringify(cl));
    await s.end().catch(() => {});
  } catch (e) {
    fail++; console.log('  ✗ ' + e.message);
  } finally {
    driver.kill();
  }
}

// ------------------------------------------------------------------------------------------------- Sign-in (C.2, opt-in)
// --signin: against the live getcadcraft.com with the site's own test member, approved in-process through the site's
// DeviceAuth by a helper that lives with the site (not in this repo): $CADCRAFT_SITE/tools/desktop-test/device-helper.php.
// Creates and revokes real device tokens for that member.
if (process.argv.includes('--signin')) {
  const helperFile = path.join(SITE, 'tools/desktop-test/device-helper.php');
  if (!fs.existsSync(helperFile)) { console.error('--signin needs the site\'s helper at ' + helperFile); process.exit(1); }
  const helper = (...a) => JSON.parse(execFileSync('sudo', ['-u', 'www-data', 'php', helperFile, ...a]).toString());
  const fx = helper('up');
  { const sj = path.join(path.dirname(ccRoot), 'shell.json'); const c = JSON.parse(fs.readFileSync(sj, 'utf8')); delete c.account; fs.writeFileSync(sj, JSON.stringify(c)); }
  driver = startDriver(ccRoot);
  try {
    console.log('Signing in to getcadcraft.com (C.2)');
    const s = await session([]);
    await waitFor(async () => (await s.run(`return document.readyState === 'complete';`)).r);
    const A = (expr) => s.run(`const a = window.cadcraftDesktop.account; return await (${expr});`);
    ok((await A('a.status()')).r.state === 'signed_out', 'starts signed out');
    const early = await s.run(`return await window.cadcraftDesktop.cloud.request({path: '/api/v1/me'});`);
    ok(!early.ok && early.code === 'signed_out', 'the cloud is refused while signed out', JSON.stringify(early));

    const signIn = async () => {
      const w = (await A('a.signIn()')).r;
      ok(w && w.state === 'waiting' && /^[A-Z0-9]{4}-[A-Z0-9]{4}$/.test(w.code || ''), 'a short code is shown', JSON.stringify(w));
      ok(helper('approve', w.code).code === 'ok', 'approved on the website');
      const done = await waitFor(async () => (await A('a.status()')).r.state === 'signed_in', 30000);
      const st = (await A('a.status()')).r;
      ok(done && st.user && st.user.email === fx.email, 'signed in, as the right person', JSON.stringify(st));
      return st;
    };
    const st = await signIn();
    const leaks = (await s.run(`
      const seen = [JSON.stringify(localStorage), JSON.stringify(sessionStorage), document.cookie, JSON.stringify(window.cadcraftDesktop)];
      const dbs = indexedDB.databases ? await indexedDB.databases() : [];
      return {ccu: seen.some((x) => /ccu_/.test(x)), status: /ccu_/.test(JSON.stringify(args[0])), dbs: dbs.map((d) => d.name)};`, st)).r;
    ok(!leaks.ccu && !leaks.status, 'the token is nowhere the page can see', JSON.stringify(leaks));
    const tk = helper('tokens');
    ok(tk.active === 1 && tk.scopes[0] === 'sync' && /CadCraft/.test(tk.names[0]), 'one device token on the website, scope sync, named for the app', JSON.stringify(tk));

    const me = await s.run(`const r = await window.cadcraftDesktop.cloud.request({path: '/api/v1/me'}); return {status: r.status, email: r.json().data.user.email};`);
    ok(me.ok && me.r.status === 200 && me.r.email === fx.email, 'cloud.request reaches /api/v1/me with the shell adding the sign-in', JSON.stringify(me));
    const sync = await s.run(`const r = await window.cadcraftDesktop.cloud.request({path: '/api/v1/sync/status'}); return {status: r.status, ws: (r.json().data || {}).workspaces};`);
    ok(sync.ok && sync.r.status === 200 && Array.isArray(sync.r.ws), 'and the sync API', JSON.stringify(sync));
    for (const bad of ['/api/v1/library/items', '/account', '/api/v1/sync/%2e%2e/logout', 'https://example.com/api/v1/sync/status', '/api/v1/sync/../logout']) {
      const r = await s.run(`return await window.cadcraftDesktop.cloud.request({path: args[0]});`, bad);
      ok(!r.ok && r.code === 'not_allowed', `refused: ${bad}`, JSON.stringify(r));
    }

    helper('revoke'); // signed out on the website (account/security)
    const after = await s.run(`const r = await window.cadcraftDesktop.cloud.request({path: '/api/v1/sync/status'}); return r.status;`);
    const st2 = (await A('a.status()')).r;
    ok(after.r === 401 && st2.state === 'signed_out' && st2.reason === 'revoked', 'signed out on the website → signed out here, and told why', JSON.stringify({ after, st2 }));

    await signIn();
    const out = (await A('a.signOut()')).r;
    ok(out.state === 'signed_out' && !out.message, 'sign out', JSON.stringify(out));
    ok(helper('tokens').active === 0, 'sign out revoked the token on the website');

    const c = (await A('a.signIn()')).r;
    const cancelled = (await A('a.cancel()')).r;
    ok(c.state === 'waiting' && cancelled.state === 'signed_out', 'cancel stops waiting');
    await new Promise((r) => setTimeout(r, 6000));
    ok((await A('a.status()')).r.state === 'signed_out' && helper('tokens').active === 0, 'and nothing signs in afterwards');

    await A('a.signIn()');
    await A('a.open()');
    const handles = await waitFor(async () => (await wd('GET', `/session/${s.id}/window/handles`)).length >= 2, 8000);
    ok(handles, 'the sign-in window opens');
    if (handles && SHOTS) {
      const hs = await wd('GET', `/session/${s.id}/window/handles`);
      const mainHandle = await wd('GET', `/session/${s.id}/window`);
      await wd('POST', `/session/${s.id}/window`, { handle: hs.find((h) => h !== mainHandle) });
      await new Promise((r) => setTimeout(r, 800));
      const code = (await s.run(`return document.querySelector('[data-code]').textContent;`)).r;
      ok(/^[A-Z0-9]{4}-[A-Z0-9]{4}$/.test(code), 'it shows the code large', code);
      await s.shot('04-sign-in-window');
      await wd('POST', `/session/${s.id}/window`, { handle: mainHandle });
    }
    await A('a.cancel()');
    await s.end().catch(() => {});
  } catch (e) {
    fail++; console.log('  ✗ ' + e.message);
  } finally {
    driver.kill();
    for (const p of keyringPid || []) { try { process.kill(Number(p)); } catch {} }
    helper('down');
  }
}


// --------------------------------------------------------------------------------------------- Studio (suite mode, hub)
if (SUITE && fs.existsSync(path.join(SUITE, 'index.html')) && fs.existsSync(path.join(SUITE, 'apps/cadcraft/index.html'))) {
  const dir = path.join(tmp, 'stage-studio');
  fs.cpSync(SUITE, path.join(dir, 'app'), { recursive: true, dereference: true });
  // The same header rule build.mjs uses: COOP/COEP for the whole origin when any app in the suite wants isolation.
  const isolate = fs.readdirSync(path.join(SUITE, 'apps')).flatMap((a) => {
    const d = path.join(SUITE, 'apps', a, 'assets/apps', a);
    return fs.existsSync(d) ? fs.readdirSync(d).map((v) => path.join(d, v, 'manifest.json')).filter((f) => fs.existsSync(f)) : [];
  }).map((f) => JSON.parse(fs.readFileSync(f, 'utf8')).isolate).find(Boolean);
  const headers = isolate ? { 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': isolate } : {};
  fs.writeFileSync(path.join(dir, 'shell.json'), JSON.stringify({ app: 'studio', name: 'CadCraft Studio', version: 'dev', suite: true, headers }));
  const dwg2 = path.join(work, 'studio plan.dwg');
  fs.copyFileSync(DWG, dwg2);
  driver = startDriver(path.join(dir, 'app'));
  try {
    console.log('Studio (suite mode)');
    // ccstudio:// invite links (links.rs): one good link and two that must be dropped, beside the launch file.
    const LINK = 'ccstudio://join/AB3K7MNP#abcdefghijklmnopqrstuv', LINK2 = 'ccstudio://join/ZX9Q2RTV#ABCDEFGHIJKLMNOPQRSTUV_-12';
    const s = await session([dwg2, LINK, 'ccstudio://join/bad#x', 'ccstudio://host/AB3K7MNP#abcdefghijklmnopqrstuv']);
    ok(await waitFor(async () => (await s.run(`return document.readyState === 'complete';`)).r), 'the hub loads');
    const info = (await s.run(`return await window.cadcraftDesktop.info();`)).r;
    ok(info.suite === true && info.windowId === 'main', 'info(): suite, windowId main', JSON.stringify(info));
    if (isolate) {
      const h = (await s.run(`const r = await fetch('./index.html'); return {coep: r.headers.get('cross-origin-embedder-policy'), errs: [...document.querySelectorAll('.app-error,[data-app-error]')].length};`)).r;
      ok(h.coep === isolate && h.errs === 0, `the suite runs under COEP ${isolate} (PartCraft is in it)`, JSON.stringify(h));
    }
    await s.run(`window.__ccE2EWin = []; window.cadcraftDesktop.windows.onChange((e) => window.__ccE2EWin.push(e));`);
    // The hub takes the launch file and routes the .dwg to CadCraft in a window of its own.
    const routed = await waitFor(async () => (await s.run(`return (await window.cadcraftDesktop.windows.list()).some((w) => w.app === 'cadcraft');`)).r, 20000);
    const list1 = (await s.run(`return await window.cadcraftDesktop.windows.list();`)).r;
    ok(routed, 'a .dwg Studio was started with opens in a CadCraft window (routed by the hub)', JSON.stringify(list1));
    const mainHandle = await wd('GET', `/session/${s.id}/window`);
    const handles = await wd('GET', `/session/${s.id}/window/handles`);
    const other = handles.find((h) => h !== mainHandle);
    if (other) {
      await wd('POST', `/session/${s.id}/window`, { handle: other });
      await waitFor(async () => (await s.run(`return document.readyState === 'complete' && !!window.cadcraftDesktop;`)).r, 20000);
      const inApp = (await s.run(`const d = window.cadcraftDesktop; const i = await d.info();
        let opened = null; try { await d.windows.open({app: 'modelcraft'}); opened = 'yes'; } catch (e) { opened = e.code; }
        let ctl = null; try { await d.control.start(); ctl = 'started'; } catch (e) { ctl = e.code; }
        const bc = new BroadcastChannel('cc-e2e'); bc.postMessage({from: i.windowId});
        return {i, href: location.href, opened, ctl};`)).r;
      ok(/^app-cadcraft-\d+$/.test(inApp.i.windowId) && inApp.href.includes('/apps/cadcraft/index.html'), 'the app window runs CadCraft, with its own id', JSON.stringify(inApp));
      ok(inApp.opened === 'not_allowed', 'only the hub opens windows');
      let inAppLinks = null; try { await s.run(`return await window.cadcraftDesktop.links.launch();`).then((x) => { inAppLinks = x.ok ? 'given' : x.code; }); } catch { inAppLinks = 'error'; }
      ok(inAppLinks === 'not_allowed', 'only the hub takes ccstudio:// links', inAppLinks);
      ok(inApp.ctl === 'not_allowed', 'only the hub runs the MCP server');
      await studioLive(s, other, mainHandle);
      await wd('POST', `/session/${s.id}/window`, { handle: mainHandle });
    }
    // ccstudio:// links: the good one, once, untouched; the others dropped; a second start's link reaches onOpen.
    const lk = (await s.run(`const L = window.cadcraftDesktop.links; const a = await L.launch(); const b = await L.launch();
      window.__ccE2ELinks = []; L.onOpen((u) => window.__ccE2ELinks.push(u)); return {a, b};`)).r;
    // The hub's Join card (live-studio's hub-join.js) takes the launch link itself as soon as it loads; then this call finds
    // none left, and the card has answered (it asks for a name first). Either way: once, and the link never shown.
    const joinMsg = (await s.run(`const m = document.querySelector('[data-live-join-msg]'); return m && !m.hidden ? m.textContent : '';`)).r;
    ok(lk && lk.b.length === 0 && (JSON.stringify(lk.a) === JSON.stringify([LINK]) || (lk.a.length === 0 && joinMsg && !joinMsg.includes('abcdefghijklmnopqrstuv'))),
      'Studio started with a ccstudio:// link hands it to the hub once, untouched; malformed ones are dropped', JSON.stringify({ lk, joinMsg }));
    const secondLink = spawn(BIN, [LINK2, 'ccstudio://join/AB3K7MNP#bad"secret'], { env: { ...process.env, CADCRAFT_DESKTOP_ROOT: '/nonexistent' }, stdio: 'ignore' });
    const gotLink = await waitFor(async () => (await s.run(`return window.__ccE2ELinks.length > 0;`)).r, 10000);
    ok(gotLink && JSON.stringify((await s.run(`await new Promise((r) => setTimeout(r, 500)); return window.__ccE2ELinks;`)).r) === JSON.stringify([LINK2]), 'a link opened while Studio runs reaches the hub (and only the good one)');
    secondLink.kill();
    // Per-app shortcuts (SD.6): `--app=<slug>` opens Studio straight into that app, at start and on a second start.
    const hasApp = async (a) => (await s.run(`return (await window.cadcraftDesktop.windows.list()).some((w) => w.app === '${a}');`)).r;
    const secondApp = spawn(BIN, ['--app=bimcraft', '--app=nope'], { env: { ...process.env, CADCRAFT_DESKTOP_ROOT: '/nonexistent' }, stdio: 'ignore' });
    ok(await waitFor(() => hasApp('bimcraft'), 20000), 'a second start with --app=bimcraft opens BimCraft in the running Studio');
    secondApp.kill();
    ok(!(await s.run(`return (await window.cadcraftDesktop.windows.list()).some((w) => w.app === 'nope');`)).r, 'an unknown --app is ignored');
    ok(!(await s.run(`return document.title;`)).r.includes('#'), 'no link in the window title');
    // Desktop shell language files: the shell's own sign-in window follows Studio's language (cadcraft.studio.lang).
    const hubHandle = await wd('GET', `/session/${s.id}/window`);
    const before = new Set(await wd('GET', `/session/${s.id}/window/handles`));
    await s.run(`localStorage.setItem('cadcraft.studio.lang', 'es-MX'); await window.cadcraftDesktop.account.open();`);
    let acctHandle = null;
    await waitFor(async () => { acctHandle = (await wd('GET', `/session/${s.id}/window/handles`)).find((h) => !before.has(h)); return !!acctHandle; }, 10000);
    if (!acctHandle) ok(false, 'the sign-in window opens');
    else {
      await wd('POST', `/session/${s.id}/window`, { handle: acctHandle });
      const es = await waitFor(async () => (await s.run(`const h = [...document.querySelectorAll('h1')].find((x) => x.offsetParent); return !!h && h.textContent === 'Iniciar sesión en getcadcraft.com' && document.documentElement.lang === 'es-MX';`)).r, 10000);
      ok(es, 'the shell\'s sign-in window speaks Studio\'s language (es-MX draft)');
      await s.run(`window.cadcraftDesktop.account.closeWindow();`).catch(() => {});
      await wd('POST', `/session/${s.id}/window`, { handle: hubHandle });
    }
    await s.run(`localStorage.removeItem('cadcraft.studio.lang');`);
    // Direct calls from the hub.
    const r = (await s.run(`const W = window.cadcraftDesktop.windows;
      const bc = new BroadcastChannel('cc-e2e'); window.__ccE2EBc = []; bc.onmessage = (e) => window.__ccE2EBc.push(e.data);
      const a = await W.open({app: 'markupcraft', search: '?e2e=1'});
      const bad = [];
      for (const o of [{app: 'nope'}, {app: '../x'}, {app: 'cadcraft', search: 'x#y'}, {app: 'cadcraft', paths: ['/etc/hostname']}]) {
        try { await W.open(o); bad.push('opened'); } catch (e) { bad.push(e.code || 'error'); }
      }
      await W.focus(a.id); await W.focusHub();
      let unknown = null; try { await W.focus('popup-0'); } catch (e) { unknown = 'refused'; }
      return {a, bad, unknown, list: await W.list()};`)).r;
    ok(/^app-markupcraft-\d+$/.test(r.a.id) && r.list.some((w) => w.id === r.a.id && w.app === 'markupcraft'), 'windows.open → a MarkupCraft window, listed', JSON.stringify(r));
    ok(r.list[0].id === 'main' && r.list[0].app === 'studio', 'the hub is listed first as studio');
    ok(r.bad.join() === 'error,error,error,not_allowed', 'unknown apps, bad queries and unreachable files are refused', r.bad.join());
    ok(r.unknown === 'refused', 'focus() only on hub/app windows');
    // An app window posts on the channel when it loads (a second CadCraft window, opened here, says hello).
    await s.run(`const id = (await window.cadcraftDesktop.windows.open({app: 'cadcraft'})).id; window.__ccE2ESecond = id;`);
    const second = (await s.run(`return window.__ccE2ESecond;`)).r;
    const hs2 = await wd('GET', `/session/${s.id}/window/handles`);
    // Find the new window: switch to each non-hub handle until its windowId matches.
    let target = null;
    for (const h of hs2.filter((h) => h !== mainHandle)) {
      await wd('POST', `/session/${s.id}/window`, { handle: h });
      await waitFor(async () => (await s.run(`return !!window.cadcraftDesktop;`)).r, 15000);
      if ((await s.run(`return (await window.cadcraftDesktop.info()).windowId;`)).r === second) { target = h; break; }
    }
    if (target) {
      await s.run(`new BroadcastChannel('cc-e2e').postMessage({from: 'second'});`);
      await wd('POST', `/session/${s.id}/window/rect`, { x: 60, y: 70, width: 1000, height: 650 });
      await new Promise((res) => setTimeout(res, 600));
      // Closed the way a person closes it (the close event runs), not torn down by WebDriver.
      await s.run(`setTimeout(() => window.cadcraftDesktop.windows.closeSelf(), 50); return true;`).catch(() => {});
      await new Promise((res) => setTimeout(res, 1500));
    }
    await wd('POST', `/session/${s.id}/window`, { handle: mainHandle });
    const heard = await waitFor(async () => (await s.run(`return window.__ccE2EBc.some((m) => m.from === 'second');`)).r, 5000);
    ok(heard, 'windows hear each other over BroadcastChannel (one origin)');
    const evs = (await s.run(`return window.__ccE2EWin;`)).r;
    ok(evs.some((e) => e.type === 'opened' && e.id === second) && evs.some((e) => e.type === 'closed' && e.id === second), 'the hub hears windows open and close', JSON.stringify(evs));
    // X.5: Studio's local MCP server, checked by agents' own checker (tools/mcp/test/check-server.mjs).
    const srv = (await s.run(`const C = window.cadcraftDesktop.control;
      C.onRequest((msg, session, reply) => {
        if (msg.id === undefined) return reply(null);
        const result = msg.method === 'initialize'
          ? {protocolVersion: '2025-06-18', capabilities: {tools: {}}, serverInfo: {name: 'e2e stand-in hub', version: '1'}}
          : {tools: [], session: session.id};
        reply({jsonrpc: '2.0', id: msg.id, result});
      });
      return await C.start();`)).r;
    ok(srv && /^http:\/\/127\.0\.0\.1:\d+\/mcp$/.test(srv.url) && srv.token.length === 43, 'control.start() → a 127.0.0.1 URL and a token', JSON.stringify({ url: srv && srv.url }));
    const checker = path.join(SITE, 'tools/mcp/test/check-server.mjs');
    if (fs.existsSync(checker)) {
      let out = '';
      try { out = execFileSync('node', [checker, srv.url, srv.token, '--verbose'], { timeout: 60000 }).toString(); } catch (e) { out = String(e.stdout || '') + String(e.stderr || ''); }
      const m = /(\d+) passed, (\d+) failed/.exec(out) || /pass[^\d]*(\d+)[^\d]+fail[^\d]*(\d+)/i.exec(out);
      ok(m && Number(m[2]) === 0 && Number(m[1]) >= 10, `agents' check-server passes against the native server (${m ? m[1] + '/' + (Number(m[1]) + Number(m[2])) : '?'})`, out.split('\n').filter((l) => /FAIL/.test(l)).join(' | ') || out.slice(-300));
    }
    const nt = (await s.run(`const C = window.cadcraftDesktop.control; const old = (await C.server()).token; const t = await C.newToken(); return {changed: t !== old && t.length === 43, running: (await C.server()).running};`)).r;
    ok(nt.changed && nt.running, 'newToken() replaces the token');
    const stopped = (await s.run(`const C = window.cadcraftDesktop.control; const t = (await C.server()).token; await C.stop(); return {running: (await C.server()).running, t};`)).r;
    let refused = false;
    try { await fetch(srv.url, { method: 'POST' }); } catch { refused = true; }
    ok(stopped.running === false && refused, 'stop() closes the port');
    const again = (await s.run(`const C = window.cadcraftDesktop.control; const r = await C.start(); await C.stop(); return r.token;`)).r;
    ok(again === stopped.t, 'start() again keeps the same token for the run (assistants stay set up)');
    // The next CadCraft window comes back where the last one closed.
    await s.run(`window.__ccE2EThird = (await window.cadcraftDesktop.windows.open({app: 'cadcraft'})).id;`);
    const third = (await s.run(`return window.__ccE2EThird;`)).r;
    let rect = null;
    for (const h of (await wd('GET', `/session/${s.id}/window/handles`)).filter((h) => h !== mainHandle)) {
      await wd('POST', `/session/${s.id}/window`, { handle: h });
      await waitFor(async () => (await s.run(`return !!window.cadcraftDesktop;`)).r, 15000);
      if ((await s.run(`return (await window.cadcraftDesktop.info()).windowId;`)).r === third) { rect = await wd('GET', `/session/${s.id}/window/rect`); break; }
    }
    ok(rect && rect.width === 1000 && rect.height === 650, 'a new CadCraft window takes the size the last one closed with', JSON.stringify(rect));
    await wd('POST', `/session/${s.id}/window`, { handle: mainHandle });
    await s.end().catch(() => {});
  } catch (e) {
    fail++; console.log('  ✗ ' + e.message);
  } finally {
    driver.kill();
  }

  // SD.8: Studio brings over a separate 0.4 app's data at its first start. Debug builds are all com.getcadcraft.cadcraft,
  // so an "old BimCraft install" is a profile made by this shell and renamed to com.getcadcraft.bimcraft.
  console.log('Studio: the separate apps\' data comes over (SD.8)');
  const home = path.join(tmp, 'carry-data');
  const own = path.join(home, 'com.getcadcraft.cadcraft'), old = path.join(home, 'com.getcadcraft.bimcraft');
  const kept = path.join(work, 'kept plan.dwg');
  fs.copyFileSync(DWG, kept);
  const idb = `const open = (n, v, up) => new Promise((ok, no) => { const r = v ? indexedDB.open(n, v) : indexedDB.open(n); r.onupgradeneeded = () => up && up(r.result); r.onsuccess = () => ok(r.result); r.onerror = () => no(r.error); });
    const req = (r) => new Promise((ok, no) => { r.onsuccess = () => ok(r.result); r.onerror = () => no(r.error); });`;
  const snap = (d) => { const out = []; const walk = (x) => { for (const e of fs.readdirSync(x, { withFileTypes: true })) { const p = path.join(x, e.name); if (e.isDirectory()) walk(p); else { const st = fs.statSync(p); out.push(`${path.relative(d, p)} ${st.size} ${st.mtimeMs}`); } } }; walk(d); return out.sort().join('\n'); };
  try {
    // 1. Studio's own profile already has a value and a record of its own.
    driver = startDriver(path.join(dir, 'app'), home);
    let s = await session([]);
    await waitFor(async () => (await s.run(`return document.readyState === 'complete';`)).r);
    await s.run(`${idb} localStorage.setItem('cc-e2e-both', 'studio');
      const db = await open('cc-e2e-db', 1, (d) => d.createObjectStore('files', { keyPath: 'id' }));
      await req(db.transaction('files', 'readwrite').objectStore('files').put({ id: 'a', note: 'studio' })); db.close();`);
    await s.end().catch(() => {}); driver.kill(); stopShells();
    fs.renameSync(own, own + '.studio');
    // 2. The old app: its own values, a database at version 2 with a Blob and a Date, a hand-over, and a granted file.
    driver = startDriver(ccRoot, home);
    s = await session([kept]);
    await waitFor(async () => (await s.run(`return document.readyState === 'complete';`)).r);
    const made = await s.run(`${idb} localStorage.setItem('cc-e2e-old', '1'); localStorage.setItem('cc-e2e-both', 'old');
      const db = await open('cc-e2e-db', 2, (d) => { d.createObjectStore('files', { keyPath: 'id' }); d.createObjectStore('kv'); });
      const tx = db.transaction(['files', 'kv'], 'readwrite');
      tx.objectStore('files').put({ id: 'a', note: 'old' });
      tx.objectStore('files').put({ id: 'b', blob: new Blob(['hello'], { type: 'text/plain' }), when: new Date(5), bytes: new Uint8Array([1, 2, 3]), tags: new Set(['x']), $odd: 1 });
      tx.objectStore('kv').put('v', 'k1');
      await new Promise((ok) => { tx.oncomplete = ok; }); db.close();
      const h = await open('cadcraft-handoff', 1, (d) => d.createObjectStore('h')); await req(h.transaction('h', 'readwrite').objectStore('h').put(1, 'x')); h.close();
      return (await window.cadcraftDesktop.readFile(args[0])).length;`, kept);
    ok(made.ok && made.r > 0, 'an old per-app profile with values, a database and a granted file', JSON.stringify(made));
    await s.end().catch(() => {}); driver.kill(); stopShells();
    fs.renameSync(own, old);
    fs.renameSync(own + '.studio', own);
    const before = snap(old);
    // 3. Studio's first start with it beside: the carry page, then the hub.
    driver = startDriver(path.join(dir, 'app'), home);
    s = await session([]);
    const hub = await waitFor(async () => (await s.run(`return location.pathname === '/index.html' && document.readyState === 'complete';`)).r, 90000);
    ok(hub, 'Studio opens its hub after bringing the data over');
    const got = (await s.run(`${idb} const db = await open('cc-e2e-db'); const v = db.version;
      const st = db.transaction(['files', 'kv']); const a = await req(st.objectStore('files').get('a')); const b = await req(st.objectStore('files').get('b'));
      const k1 = await req(st.objectStore('kv').get('k1')); db.close();
      const dbs = (await indexedDB.databases()).map((d) => d.name);
      let read = null; try { read = (await window.cadcraftDesktop.readFile(args[0])).length; } catch (e) { read = e.code || 'error'; }
      return { old: localStorage.getItem('cc-e2e-old'), both: localStorage.getItem('cc-e2e-both'), v, a, k1,
        blob: b && b.blob instanceof Blob ? await b.blob.text() : null, type: b && b.blob && b.blob.type, when: b && b.when instanceof Date ? b.when.getTime() : null,
        bytes: b && b.bytes instanceof Uint8Array ? [...b.bytes] : null, tags: b && b.tags instanceof Set ? [...b.tags] : null, odd: b && b.$odd,
        handoff: dbs.includes('cadcraft-handoff'), read };`, kept)).r;
    ok(got && got.old === '1' && got.both === 'studio', 'localStorage came over; a key Studio already had keeps Studio\'s value', JSON.stringify(got));
    ok(got && got.v === 2 && got.k1 === 'v' && got.a && got.a.note === 'studio', 'IndexedDB: raised to the old version (2, not past it), the missing store made, Studio\'s own record kept', JSON.stringify(got));
    ok(got && got.blob === 'hello' && got.type === 'text/plain' && got.when === 5 && JSON.stringify(got.bytes) === '[1,2,3]' && JSON.stringify(got.tags) === '["x"]' && got.odd === 1, 'a record with a Blob, a Date, bytes and a Set comes over intact', JSON.stringify(got));
    ok(got && got.handoff === false, 'hand-overs between apps are not carried');
    ok(got && got.read > 0, 'a file the old app had opened still reopens in Studio (grants merged)', JSON.stringify(got && got.read));
    const carried = JSON.parse(fs.readFileSync(path.join(own, 'carried.json'), 'utf8'));
    ok(carried.bimcraft && carried.bimcraft.done === true && carried.bimcraft.tries === 1, 'carried.json records it, once', JSON.stringify(carried));
    ok(snap(old) === before && !fs.existsSync(path.join(own, 'carry', 'bimcraft')), 'the old profile is untouched; the working copy is gone');
    await s.end().catch(() => {}); driver.kill(); stopShells();
    // 4. The next start goes straight to the hub (here from a per-app shortcut: SD.6's --app=<slug>).
    driver = startDriver(path.join(dir, 'app'), home);
    s = await session(['--app=modelcraft']);
    const direct = await waitFor(async () => (await s.run(`return location.pathname;`)).r === '/index.html', 20000);
    const again = JSON.parse(fs.readFileSync(path.join(own, 'carried.json'), 'utf8'));
    ok(direct && again.bimcraft.tries === 1, 'the next start opens the hub straight away (carried once)');
    ok(await waitFor(async () => (await s.run(`return (await window.cadcraftDesktop.windows.list()).some((w) => w.app === 'modelcraft');`)).r, 20000),
      'Studio started with --app=modelcraft (a per-app shortcut) opens a ModelCraft window');
    await s.end().catch(() => {});
  } catch (e) {
    fail++; console.log('  ✗ SD.8: ' + e.message);
  } finally {
    driver.kill();
  }
}

// ---------------------------------------------------------------------------------------------------------- PartCraft
if (fs.existsSync(PARTCRAFT)) {
  const man = JSON.parse(fs.readFileSync(path.join(PARTCRAFT, 'assets/apps/partcraft', fs.readdirSync(path.join(PARTCRAFT, 'assets/apps/partcraft'))[0], 'manifest.json'), 'utf8'));
  const headers = man.isolate ? { 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': man.isolate } : {};
  driver = startDriver(stage('partcraft', PARTCRAFT, headers));
  try {
    console.log('PartCraft edition in the shell (threads)');
    const s = await session([]);
    ok(await waitFor(async () => (await s.run(`return document.readyState === 'complete';`)).r), 'page loads');
    const iso = (await s.run(`const h = (await fetch('./index.html')).headers;
      return {coi: self.crossOriginIsolated, sab: typeof SharedArrayBuffer, coop: h.get('cross-origin-opener-policy'), coep: h.get('cross-origin-embedder-policy')};`)).r;
    ok(!man.isolate || (iso.coop === 'same-origin' && iso.coep === man.isolate), 'COOP/COEP headers sent by the protocol', JSON.stringify(iso));
    // WebKitGTK never isolates a custom-scheme page, whatever its headers; PartCraft's pool then runs single-threaded
    // (edition D.5). WebView2 (http://cadcraft.localhost) is checked on Windows (W.8).
    console.log(`  · crossOriginIsolated on WebKitGTK: ${iso.coi} (SharedArrayBuffer ${iso.sab}) — PartCraft single-threaded on Linux`);
    ok(await waitFor(async () => (await s.run(`return !!document.querySelector('main canvas');`)).r, 20000), 'PartCraft draws (canvas up)');
    await s.shot('03-partcraft');
    await s.end();
  } catch (e) {
    fail++; console.log('  ✗ ' + e.message);
  } finally {
    driver.kill();
  }
}

if (wm) wm.kill();
if (process.env.CC_E2E_KEEP) console.log('kept ' + tmp); else fs.rmSync(tmp, { recursive: true, force: true });
console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
