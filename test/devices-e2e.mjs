// SPDX-License-Identifier: Apache-2.0
// devices-e2e.mjs — 3D mice and MIDI controllers in Studio's own window, end to end on Linux (0.8 ID.8): the real shell binary
// (debug build) in WebKitGTK through WebKitWebDriver, a real packed suite, and a stand-in 3D mouse and MIDI controller: files
// laid out as the kernel's hidraw and ALSA raw MIDI nodes (CC_TEST_DEVICES, debug builds only; src-tauri/src/devices.rs).
// What it proves: WebKitGTK has neither door; Studio's bridge gives the page both; the page layer (assets/js/app-devices-ui.js,
// unchanged) connects the 3D mouse, reads its reports and learns a MIDI knob; an unplug is noticed; a 3D mouse Linux does not
// let the person read says so and offers the rule file. What it cannot prove: a real device, the real kernel nodes.
//
//   xvfb-run -a node test/devices-e2e.mjs --suite <packed Studio suite folder (…/vdev)> [--site <site checkout>] [--app cadcraft]
// --site overlays the checkout's current assets/js/app-devices*.js and the shell's English words on the staged suite (a suite
// packed before a change to them). Needs: cargo build (target/debug/cadcraft-desktop), WebKitWebDriver, xvfb.

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawn, execFileSync } from 'node:child_process';

const HERE = path.dirname(new URL(import.meta.url).pathname);
const ROOT = path.resolve(HERE, '..');
const arg = (k, d) => { const i = process.argv.indexOf(k); return i > 0 ? process.argv[i + 1] : d; };
const SUITE = arg('--suite', '');
const SITE = arg('--site', '');
const APP = arg('--app', 'cadcraft');
const BIN = process.env.CC_E2E_BIN || path.join(ROOT, 'src-tauri/target/debug/cadcraft-desktop');
if (!SUITE || !fs.existsSync(path.join(SUITE, 'apps', APP, 'index.html'))) { console.error('usage: xvfb-run -a node test/devices-e2e.mjs --suite <packed suite folder> [--site <site checkout>] [--app cadcraft]'); process.exit(1); }
if (!fs.existsSync(BIN)) { console.error(`no debug build at ${BIN}: cargo build in src-tauri first`); process.exit(1); }

let pass = 0, fail = 0;
const ok = (cond, what, extra = '') => { if (cond) { pass++; console.log(`  ✓ ${what}`); } else { fail++; console.log(`  ✗ ${what} ${extra}`); } };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const waitFor = async (fn, ms = 20000) => { const t = Date.now(); for (;;) { let v = null; try { v = await fn(); } catch { v = null; } if (v) return v; if (Date.now() - t > ms) return null; await sleep(100); } };

const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'cc-devices-e2e-'));

// ---- the staged suite: hard links (nothing is copied), then the files under test replaced by fresh copies
const stage = path.join(tmp, 'stage');
fs.mkdirSync(stage);
execFileSync('cp', ['-al', SUITE, path.join(stage, 'app')]);
if (SITE) {
  const fresh = ['assets/js/app-devices.js', 'assets/js/app-devices-ui.js', 'assets/js/app-devices-studio.js', 'assets/lang/shell/en.json'];
  const all = execFileSync('find', [path.join(stage, 'app'), '-type', 'f', '(', '-name', 'app-devices*.js', '-o', '-path', '*/lang/shell/en.json', ')']).toString().split('\n').filter(Boolean);
  for (const f of all) {
    const src = fresh.find((r) => f.endsWith('/' + r));
    if (!src) continue;
    fs.rmSync(f);   // a hard link: never write through it
    fs.copyFileSync(path.join(SITE, 'public_html', src), f);
  }
}
const isolate = fs.readdirSync(path.join(SUITE, 'apps')).flatMap((a) => {
  const d = path.join(SUITE, 'apps', a, 'assets/apps', a);
  return fs.existsSync(d) ? fs.readdirSync(d).map((v) => path.join(d, v, 'manifest.json')).filter((f) => fs.existsSync(f)) : [];
}).map((f) => JSON.parse(fs.readFileSync(f, 'utf8')).isolate).find(Boolean);
fs.writeFileSync(path.join(stage, 'shell.json'), JSON.stringify({ app: 'studio', name: 'CadCraft Studio', version: 'dev', suite: true,
  headers: isolate ? { 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': isolate } : {} }));

// ---- the stand-in devices
const DEV = path.join(tmp, 'devices');
for (const d of ['sys', 'dev', 'snd', 'proc']) fs.mkdirSync(path.join(DEV, d), { recursive: true });
// A 3D mouse's report descriptor, shortened: Generic Desktop / Multi-axis Controller; report 1 = three 16-bit axes, 2 = three more, 3 = buttons.
const SPACE = Buffer.from([0x05, 0x01, 0x09, 0x08, 0xa1, 0x01,
  0xa1, 0x00, 0x85, 0x01, 0x16, 0xa2, 0xfe, 0x26, 0x5e, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x75, 0x10, 0x95, 0x03, 0x81, 0x06, 0xc0,
  0xa1, 0x00, 0x85, 0x02, 0x09, 0x33, 0x09, 0x34, 0x09, 0x35, 0x75, 0x10, 0x95, 0x03, 0x81, 0x06, 0xc0,
  0xa1, 0x00, 0x85, 0x03, 0x05, 0x09, 0x19, 0x01, 0x29, 0x02, 0x75, 0x01, 0x95, 0x02, 0x81, 0x02, 0xc0, 0xc0]);
const KEYBOARD = Buffer.from([0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0xc0]);
function hidNode(n, id, name, desc, { node = true } = {}) {
  const dir = path.join(DEV, 'sys', `hidraw${n}`, 'device');
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, 'uevent'), `DRIVER=hid-generic\nHID_ID=${id}\nHID_NAME=${name}\nHID_PHYS=usb-1\nHID_UNIQ=\n`);
  fs.writeFileSync(path.join(dir, 'report_descriptor'), desc);
  if (node) fs.writeFileSync(path.join(DEV, 'dev', `hidraw${n}`), '');
}
hidNode(0, '0003:000004D9:0000A067', 'A Keyboard', KEYBOARD);
hidNode(1, '0003:0000256F:0000C635', '3Dconnexion SpaceMouse Compact', SPACE);
const MOUSE = path.join(DEV, 'dev', 'hidraw1');
fs.mkdirSync(path.join(DEV, 'proc', 'card1'));
fs.writeFileSync(path.join(DEV, 'proc', 'card1', 'midi0'), 'Knob Box\n\nOutput 0\n  Tx bytes     : 0\nInput 0\n  Rx bytes     : 0\n');
fs.writeFileSync(path.join(DEV, 'snd', 'midiC1D0'), '');
const KNOBS = path.join(DEV, 'snd', 'midiC1D0');
const i16 = (...v) => { const b = Buffer.alloc(v.length * 2); v.forEach((x, i) => b.writeInt16LE(x, i * 2)); return b; };

// ---- WebDriver
let PORT = 4800 + Math.floor(Math.random() * 300);
let BASE = `http://127.0.0.1:${PORT}`;
async function wd(method, url, body) {
  const r = await fetch(BASE + url, { method, headers: { 'content-type': 'application/json' }, body: body ? JSON.stringify(body) : undefined });
  const j = await r.json();
  if (j.value && j.value.error) throw new Error(`${url}: ${j.value.error}: ${j.value.message}`);
  return j.value;
}
function stopShells() {
  const name = path.basename(BIN).slice(0, 15);
  for (let i = 0; i < 50; i++) {
    let pids = [];
    try { pids = execFileSync('pgrep', ['-x', name]).toString().split('\n').filter(Boolean); } catch { return; }
    for (const p of pids) { try { process.kill(Number(p), i < 20 ? 'SIGTERM' : 'SIGKILL'); } catch { /* gone */ } }
    execFileSync('sleep', ['0.2']);
  }
}
stopShells();
let driverLog = '';
const driver = spawn('WebKitWebDriver', [`--port=${PORT}`], { stdio: ['ignore', 'pipe', 'pipe'], env: { ...process.env, TAURI_WEBVIEW_AUTOMATION: 'true',
  CADCRAFT_DESKTOP_ROOT: path.join(stage, 'app'), CC_TEST_DEVICES: DEV, CC_LIVE_BIND: '127.0.0.1',
  XDG_DATA_HOME: path.join(tmp, 'data'), XDG_CONFIG_HOME: path.join(tmp, 'config'), XDG_CACHE_HOME: path.join(tmp, 'cache') } });
driver.stdout.on('data', (d) => { driverLog = (driverLog + d).slice(-4000); });
driver.stderr.on('data', (d) => { driverLog = (driverLog + d).slice(-4000); });

let sid = null;
const run = async (body, ...a) => {
  const script = `const __done = arguments[arguments.length - 1];
    (async (...args) => { ${body} })(...Array.prototype.slice.call(arguments, 0, -1))
      .then((r) => __done({ ok: true, r }), (e) => __done({ ok: false, e: String((e && e.message) || e), code: e && e.code }));`;
  return wd('POST', `/session/${sid}/execute/async`, { script, args: a });
};

try {
  if (!(await waitFor(async () => { await fetch(BASE + '/status'); return true; }, 30000))) throw new Error('WebKitWebDriver never answered: ' + driverLog.trim().split('\n').slice(-3).join(' | '));
  sid = (await wd('POST', '/session', { capabilities: { alwaysMatch: { 'webkitgtk:browserOptions': { binary: BIN, args: [] } } } })).sessionId;
  await wd('POST', `/session/${sid}/timeouts`, { script: 60000 });

  console.log('Studio hub (WebKitGTK)');
  await waitFor(async () => (await run(`return !!window.cadcraftDesktop;`)).r, 20000);
  const hub = (await run(`const d = window.cadcraftDesktop.devices; return { has: !!d, keys: d ? Object.keys(d).sort().join() : '', hid: await d.hidList(), blocked: await d.hidBlocked(), midi: await d.midiList(), rule: await d.hidRule() };`));
  ok(hub.ok && hub.r.has && hub.r.keys === 'hidBlocked,hidClose,hidList,hidOpen,hidRecv,hidRule,midiClose,midiList,midiOpen,midiRecv', 'Studio offers cadcraftDesktop.devices', JSON.stringify(hub).slice(0, 300));
  ok(hub.ok && hub.r.hid.length === 1 && hub.r.hid[0].productName === '3Dconnexion SpaceMouse Compact' && hub.r.hid[0].vendorId === 0x256f && hub.r.hid[0].usagePage === 1 && hub.r.hid[0].usage === 8,
    'the shell lists the 3D mouse and not the keyboard', JSON.stringify(hub.r && hub.r.hid));
  ok(hub.ok && hub.r.midi.length === 1 && hub.r.midi[0].name === 'Knob Box' && hub.r.blocked === 0, 'the shell lists the MIDI input; nothing is blocked', JSON.stringify(hub.r && hub.r.midi));
  ok(hub.ok && /TAG\+="uaccess"/.test(hub.r.rule) && /256f/.test(hub.r.rule), 'the shell carries the udev rule file\'s text');

  console.log(`An app window (${APP})`);
  const hubHandle = await wd('GET', `/session/${sid}/window`);
  const before = new Set(await wd('GET', `/session/${sid}/window/handles`));
  await run(`await window.cadcraftDesktop.windows.open({ app: '${APP}' });`);
  const appHandle = await waitFor(async () => (await wd('GET', `/session/${sid}/window/handles`)).find((h) => !before.has(h)), 30000);
  if (!appHandle) throw new Error('the app window never opened');
  await wd('POST', `/session/${sid}/window`, { handle: appHandle });
  const layer = await waitFor(async () => (await run(`return !!window.ccShellDevices;`)).r, 60000);
  ok(layer, 'the app\'s input-device layer is running (window.ccShellDevices)');
  const doors = (await run(`return { hid: !!navigator.hid, studio: !!(navigator.hid && navigator.hid.studio), fallback: !!(navigator.hid && navigator.hid.fallback), midi: typeof navigator.requestMIDIAccess, nativeHid: 'hid' in Navigator.prototype, nativeMidi: 'requestMIDIAccess' in Navigator.prototype, ua: navigator.userAgent };`)).r;
  ok(doors && !doors.nativeHid && !doors.nativeMidi, 'WebKitGTK itself has no WebHID and no Web MIDI', JSON.stringify(doors));
  ok(doors && doors.hid && doors.studio && !doors.fallback && doors.midi === 'function', 'Studio\'s two doors are installed for the page');

  // the page layer's own connect, as the Settings button calls it
  ok(((await run(`return (await navigator.hid.getDevices()).length;`)).r) === 0, 'no 3D mouse before the person connects one');
  const con = (await run(`return await window.ccShellDevices.connectSpace();`));
  ok(con.ok && con.r.ok === true && con.r.name === '3Dconnexion SpaceMouse Compact', 'Connect a 3D mouse: connected, by name', JSON.stringify(con).slice(0, 300));
  await run(`const [d] = await navigator.hid.getDevices(); window.__reps = []; d.addEventListener('inputreport', (e) => { const a = []; for (let i = 0; i + 1 < e.data.byteLength; i += 2) a.push(e.data.getInt16(i, true)); window.__reps.push([e.reportId, a, e.data.byteLength]); });`);
  fs.appendFileSync(MOUSE, Buffer.concat([Buffer.from([1]), i16(350, -350, 120)]));
  await sleep(60);
  fs.appendFileSync(MOUSE, Buffer.concat([Buffer.from([2]), i16(-10, 20, -30)]));
  await sleep(60);
  fs.appendFileSync(MOUSE, Buffer.from([3, 0x01, 0x00]));
  const reps = await waitFor(async () => { const r = (await run(`return window.__reps;`)).r; return r && r.length >= 3 ? r : null; }, 8000);
  ok(reps && JSON.stringify(reps[0]) === '[1,[350,-350,120],6]' && JSON.stringify(reps[1]) === '[2,[-10,20,-30],6]' && reps[2][0] === 3 && reps[2][2] === 2,
    'the 3D mouse\'s reports reach the page: push, twist and buttons, each with its report id', JSON.stringify(reps));
  const parsed = (await run(`const D = await import(new URL('assets/js/app-devices.js', [...document.scripts].map((s) => s.src).find((u) => /assets\\/js\\//.test(u)).replace(/assets\\/js\\/.*$/, '')).href);
    const st = { t: [0, 0, 0], r: [0, 0, 0], buttons: 0 }; const v = (id, a) => { const b = new DataView(new ArrayBuffer(a.length * 2)); a.forEach((x, i) => b.setInt16(i * 2, x, true)); return D.parseSpaceReport(id, b, st); };
    return [v(1, [350, -350, 120]), v(2, [-10, 20, -30]), st];`));
  ok(parsed.ok && parsed.r[0] && parsed.r[1] && parsed.r[2].t.join() === '350,-350,120' && parsed.r[2].r.join() === '-10,20,-30', 'and the page layer\'s own parser reads them (parseSpaceReport)', JSON.stringify(parsed).slice(0, 300));
  const seen = (await run(`const s = window.ccShellDevices.seen && window.ccShellDevices.seen(); return JSON.stringify(s || null);`)).r;
  ok(/SpaceMouse/.test(String(seen)), 'the layer marks the 3D mouse as seen', String(seen).slice(0, 200));

  // MIDI: connect, then Learn hears a knob
  const cm = (await run(`return await window.ccShellDevices.connectMidi();`));
  ok(cm.ok && cm.r.ok === true && cm.r.count === 1, 'Connect MIDI: one input', JSON.stringify(cm).slice(0, 300));
  ok(((await run(`return window.ccShellDevices.midiInputs().join();`)).r) === 'Knob Box', 'the input is listed by its name');
  await run(`window.__learned = null; window.ccShellDevices.learnMidi(10000).then((m) => { window.__learned = m || 'timeout'; });`);
  await sleep(400);   // the layer opened the input when it hooked onmidimessage; give the reader a moment
  fs.appendFileSync(KNOBS, Buffer.from([0xf8, 0xb2, 21, 0xf8, 77, 0xfe]));   // a knob on channel 3, controller 21, value 77, with clock bytes around it
  const learned = await waitFor(async () => (await run(`return window.__learned;`)).r, 8000);
  ok(learned && learned.type === 'cc' && learned.ch === 2 && learned.n === 21 && learned.v === 77, 'Learn hears the knob: controller 21 on channel 3, value 77', JSON.stringify(learned));
  // system exclusive never reaches the page: Learn, fed a sysex message and then a note, hears the note
  await run(`window.__learned = null; window.ccShellDevices.learnMidi(10000).then((m) => { window.__learned = m || 'timeout'; });`);
  fs.appendFileSync(KNOBS, Buffer.from([0xf0, 0x7e, 0x7f, 0x06, 0x01, 0xf7, 0x91, 60, 100]));
  const note = await waitFor(async () => (await run(`return window.__learned;`)).r, 8000);
  ok(note && note.type === 'note' && note.ch === 1 && note.n === 60, 'system exclusive never reaches the page; the note after it does', JSON.stringify(note));
  const same = (await run(`const a = await navigator.requestMIDIAccess({ sysex: false }), b = await navigator.requestMIDIAccess(); return a === b && a.inputs.size === 1 && a.outputs.size === 0 && a.sysexEnabled === false;`)).r;
  ok(same === true, 'a window has one MIDI access, input only');
  let sysex = (await run(`try { await navigator.requestMIDIAccess({ sysex: true }); return 'granted'; } catch (e) { return e.name; }`)).r;
  ok(sysex === 'NotSupportedError', 'a page asking for system exclusive access is refused', String(sysex));

  // a second window reads the same 3D mouse without taking it from the first
  await wd('POST', `/session/${sid}/window`, { handle: hubHandle });
  const two = (await run(`const D = window.cadcraftDesktop.devices; const [r] = await D.hidList(); await D.hidOpen(r.id); return r.id;`));
  fs.appendFileSync(MOUSE, Buffer.concat([Buffer.from([1]), i16(5, 6, 7)]));
  const hubGot = await waitFor(async () => { const r = (await run(`return await window.cadcraftDesktop.devices.hidRecv(args[0]);`, two.r)).r; return r && r.length ? r : null; }, 8000);
  ok(hubGot && hubGot[0].reportId === 1 && hubGot[0].bytes.join() === '5,0,6,0,7,0', 'a second window gets its own copy of the reports', JSON.stringify(hubGot));
  await wd('POST', `/session/${sid}/window`, { handle: appHandle });
  const both = await waitFor(async () => { const r = (await run(`return window.__reps;`)).r; return r && r.length >= 4 ? r : null; }, 8000);
  ok(both && JSON.stringify(both[3]) === '[1,[5,6,7],6]', 'and the first window still gets them too', JSON.stringify(both && both.slice(3)));

  // unplug
  await run(`window.__lost = 0; navigator.hid.addEventListener('disconnect', () => { window.__lost++; });`);
  fs.rmSync(MOUSE);
  fs.rmSync(path.join(DEV, 'sys', 'hidraw1'), { recursive: true });
  const lost = await waitFor(async () => (await run(`return window.__lost;`)).r, 10000);
  ok(lost >= 1, 'unplugging the 3D mouse is noticed (disconnect)');
  const none = (await run(`return await window.ccShellDevices.connectSpace();`));
  ok(none.ok && none.r.ok === false && none.r.why === 'nodevice', 'Connect with no 3D mouse plugged in says so', JSON.stringify(none).slice(0, 200));

  // a 3D mouse Linux does not let the person read: its sysfs entry is there, its node cannot be opened
  hidNode(4, '0003:0000256F:0000C652', '3Dconnexion Universal Receiver', SPACE, { node: false });
  const blocked = (await run(`return await window.ccShellDevices.connectSpace();`));
  ok(blocked.ok && blocked.r.ok === false && blocked.r.why === 'blocked' && /TAG\+="uaccess"/.test(blocked.r.rule || ''), 'a 3D mouse Linux will not let us read: "blocked", with the rule file to save', JSON.stringify(blocked).slice(0, 200));
  // The words are written into each app's page when the suite is packed: checked only when this suite has them.
  if (fs.readFileSync(path.join(SUITE, 'apps', APP, 'index.html'), 'utf8').includes('shell.dev.sm.why.blocked')) {
    const words = (await run(`return [window.ccShellT('shell.dev.sm.why.blocked'), window.ccShellT('shell.dev.sm.why.nodevice'), window.ccShellT('shell.dev.sm.rule')];`)).r;
    ok(words && words.every((w) => w && !/^shell\./.test(w)), 'the three new messages have their words', JSON.stringify(words));
  } else console.log('  - the three new messages: not checked (this suite was packed before they were added; bin/test-devices-studio.mjs checks en.json)');
} catch (e) {
  fail++;
  console.log(`  ✗ stopped: ${e && e.message}`);
  console.log('    driver: ' + driverLog.trim().split('\n').slice(-4).join(' | '));
} finally {
  if (sid) await wd('DELETE', `/session/${sid}`).catch(() => {});
  driver.kill();
  stopShells();
  fs.rmSync(tmp, { recursive: true, force: true });
}
console.log(`\ndevices-e2e: ${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
