#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// tablet.mjs — Studio as the tablets run it (docs/0.5-PART3.md TB.1), in Chromium at tablet size with touch: the packed
// suite served at the shell's own origin layout (the hub at /, apps at /apps/<slug>/, the shell's pages at /.shell/),
// bridge.js injected into EVERY frame as Android does (addDocumentStartJavaScript) with flags.mobile, and a stand-in for
// the shell's commands (documents are `cc-doc:/<key>/<name>`, as files.rs hands them out). Checks the views (apps as
// full-screen views in the hub's page, Back, switching, closing with unsaved work), a document opened and saved from an
// app through host.js, `<a download>`, links out, the sign-in page as a view, account changes reaching every view.
// Fails on any console error.
//   CADCRAFT_STUDIO=<studio repo> CADCRAFT_SITE=<site> node test/tablet.mjs [--suite <dir>] [--shots <dir>] [--verbose]
// Needs: the suite (the studio repo's out/studio-suite/vdev, or --suite), playwright-core in the studio repo's
// node_modules with its cached Chromium, and the site's fixtures (a DWG).
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';

const HERE = path.dirname(new URL(import.meta.url).pathname);
const SHELL = path.resolve(HERE, '..');
const arg = (k) => { const i = process.argv.indexOf(k); return i > 0 ? process.argv[i + 1] : null; };
const VERBOSE = process.argv.includes('--verbose');
const STUDIO = process.env.CADCRAFT_STUDIO || '';
const SITE = process.env.CADCRAFT_SITE || '';
if (!STUDIO || !SITE) { console.error('set CADCRAFT_STUDIO (the studio repo) and CADCRAFT_SITE (the site, for its fixtures)'); process.exit(1); }
const SUITE = path.resolve(arg('--suite') || path.join(STUDIO, 'out/studio-suite/vdev'));
if (!fs.existsSync(path.join(SUITE, 'index.html'))) { console.error('no suite at ' + SUITE); process.exit(1); }
const SHOTS = arg('--shots') ? path.resolve(arg('--shots')) : null;
if (SHOTS) fs.mkdirSync(SHOTS, { recursive: true });
const DWG = fs.readFileSync(path.join(SITE, 'fixtures/dwg/kitchen-equipment.dwg'));

let pass = 0, fail = 0;
const ok = (c, m) => { if (c) { pass++; if (VERBOSE) console.log('  ok  ' + m); } else { fail++; console.log('  FAIL ' + m); } return !!c; };
const section = (s) => console.log('\n' + s);

// ---- the shell's origin: the suite at /, the shell's own pages at /.shell/ (protocol.rs SHELL_PAGES) ----------------
const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.mjs': 'text/javascript', '.css': 'text/css', '.json': 'application/json',
  '.wasm': 'application/wasm', '.svg': 'image/svg+xml', '.png': 'image/png', '.ico': 'image/x-icon', '.woff2': 'font/woff2', '.woff': 'font/woff' };
const SHELL_PAGES = { '/.shell/account.html': 'bridge/account.html', '/.shell/i18n.js': 'bridge/shell-i18n.js', '/.shell/lang/en.json': 'lang/en.json', '/.shell/lang/es-MX.json': 'lang/es-MX.json' };
const srv = http.createServer((req, res) => {
  const u = new URL(req.url, 'http://x');
  let file = SHELL_PAGES[u.pathname] ? path.join(SHELL, SHELL_PAGES[u.pathname]) : null;
  if (!file) {
    let rel = decodeURIComponent(u.pathname.slice(1));
    if (rel === '' || rel.endsWith('/')) rel += 'index.html';
    file = path.resolve(SUITE, rel);
    if (!file.startsWith(SUITE + path.sep)) file = null;
  }
  if (!file || !fs.existsSync(file) || !fs.statSync(file).isFile()) { res.writeHead(404); res.end(); return; }
  res.writeHead(200, { 'Content-Type': TYPES[path.extname(file).toLowerCase()] || 'application/octet-stream', 'Cache-Control': 'no-store' });
  if (req.method === 'HEAD') { res.end(); return; }
  fs.createReadStream(file).pipe(res);
});
await new Promise((r) => srv.listen(0, '127.0.0.1', r));
const ORIGIN = `http://127.0.0.1:${srv.address().port}`;

// ---- the stand-in shell (top frame only; views reach it through the top frame, as on a tablet) --------------------
const STANDIN = `(() => {
  if (window.top !== window) return;
  const s = window.__standin = { docs: new Map(), answers: [], calls: [], account: { state: 'signed_out' }, n: 0 };
  const key = () => (++s.n).toString(16).padStart(16, '0');
  const need = (p) => { if (!s.docs.has(p)) throw 'not_allowed: the app can reach only files you opened or saved, or folders you chose'; };
  window.__TAURI_INTERNALS__ = { invoke: async (cmd, args, opts) => {
    // Bytes come as base64 on a tablet (Android's IPC is JSON only): bridge.js wire(), files.rs body_bytes.
    const b64 = args && typeof args.b64 === 'string' ? Uint8Array.from(atob(args.b64), (c) => c.charCodeAt(0)) : null;
    s.calls.push({ cmd, args: b64 ? { b64: b64.length } : args instanceof Uint8Array ? { bytes: args.length } : args, headers: opts && opts.headers });
    switch (cmd) {
      case 'cc_info': return { windowId: 'main', suite: true, shell: '0.4.0', app: 'studio', name: 'CadCraft Studio', version: 'dev', platform: 'android', store: false, mobile: true };
      case 'cc_launch_files': case 'cc_launch_apps': case 'cc_links_launch': return [];
      case 'cc_open_dialog': { const a = s.answers.shift(); if (!a) return null; return a.map((d) => { const p = 'cc-doc:/' + key() + '/' + d.name; s.docs.set(p, d.bytes); return p; }); }
      case 'cc_save_dialog': { const a = s.answers.shift(); if (!a) return null; const p = 'cc-doc:/' + key() + '/' + a[0].name; s.docs.set(p, new Uint8Array(0)); return p; }
      case 'cc_read_file': need(args.path); return s.docs.get(args.path).slice().buffer;
      case 'cc_write_file': { const p = decodeURIComponent(opts.headers['x-cc-path']); need(p); if (!b64) throw 'error: expected base64 on a tablet'; s.docs.set(p, b64); return null; }
      case 'cc_stat': need(args.path); return { size: s.docs.get(args.path).length, mtime: 1760400000000 };
      case 'cc_account_status': return s.account;
      case 'cc_open_url': return null;
      case 'cc_live_session': return { running: false, port: 0, addrs: [], app: '', view: (args && args.view) || null };
      case 'cc_live_view_closed': return null;
      default: throw 'error: not in the stand-in: ' + cmd;
    }
  } };
})();`;
const FLAGS = 'window.__ccShellFlags=Object.freeze({account:true,suite:true,live:true,mobile:true});';
const BRIDGE = fs.readFileSync(path.join(SHELL, 'bridge/bridge.js'), 'utf8');

const { chromium } = await import(path.join(STUDIO, 'node_modules/playwright-core/index.mjs'));
const cacheDir = path.join(os.homedir(), '.cache/ms-playwright');
const cbuild = fs.readdirSync(cacheDir).filter((d) => /^chromium-\d+$/.test(d)).sort((a, b) => parseInt(b.split('-')[1]) - parseInt(a.split('-')[1]))[0];
const exe = ['chrome-linux64/chrome', 'chrome-linux/chrome'].map((s) => path.join(cacheDir, cbuild, s)).find((p) => fs.existsSync(p));
const browser = await chromium.launch({ executablePath: exe, args: ['--no-sandbox'] });
// A 10-inch Android tablet held sideways.
const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 2, hasTouch: true, isMobile: true,
  userAgent: 'Mozilla/5.0 (Linux; Android 14; Pixel Tablet) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36' });
await ctx.addInitScript(STANDIN);
await ctx.addInitScript(FLAGS);
await ctx.addInitScript(BRIDGE);
// Nothing leaves the shell's origin in this test (the hub's news and catalog reads are answered empty).
await ctx.route((u) => !u.href.startsWith(ORIGIN) && !/^(data|blob|about):/.test(u.protocol), (r) => r.fulfill({ status: 404, body: '' }));
const problems = [];
const page = await ctx.newPage();
page.on('console', (m) => { if (m.type() === 'error' && !/Failed to load resource.*404/.test(m.text())) problems.push(m.text()); });
page.on('pageerror', (e) => problems.push(e.message));
const shot = async (n) => { if (SHOTS) await page.screenshot({ path: path.join(SHOTS, n + '.png') }); };
const waitFor = async (fn, ms = 15000) => { const t0 = Date.now(); while (Date.now() - t0 < ms) { try { if (await fn()) return true; } catch { /* not yet */ } await new Promise((r) => setTimeout(r, 150)); } return false; };
const frameOf = (id) => page.frames().find((f) => f.name() === id);
const shown = () => page.evaluate(() => {
  const L = document.querySelector('[data-cc-views]');
  if (!L || L.style.visibility !== 'visible') return 'main';
  const f = [...L.querySelectorAll('iframe')].find((x) => x.style.visibility === 'visible');
  return f ? f.name : '?';
});

section('The hub on a tablet');
await page.goto(ORIGIN + '/index.html?space=paper');
await page.waitForFunction(() => window.cadcraftStudioHub && document.querySelector('[data-tile]'), null, { timeout: 30000 }).catch(() => {});
const info = await page.evaluate(async () => ({ info: await window.cadcraftDesktop.info(), keys: Object.keys(window.cadcraftDesktop), win: Object.keys(window.cadcraftDesktop.windows || {}) }));
ok(info.info.platform === 'android' && info.info.mobile === true, 'info(): platform android, mobile');
ok(!info.keys.includes('updates') && !info.keys.includes('control') && info.keys.includes('live'), 'no updater, no local MCP server; Studio Live is there: ' + info.keys.join(','));
ok(['open', 'list', 'focus', 'focusHub', 'close', 'onChange'].every((k) => info.win.includes(k)), 'windows: open/list/focus/focusHub/close/onChange');
ok(await page.evaluate(() => document.documentElement.classList.contains('is-tablet') && /Tablet/.test(document.querySelector('.studio-bar').textContent)), 'the hub says Tablet (TB.4)');
await shot('01-hub');

section('An app as a view');
const events = [];
await page.exposeFunction('__ccTestEvent', (e) => events.push(e));
await page.evaluate(() => window.cadcraftDesktop.windows.onChange((e) => window.__ccTestEvent(e)));
const opened = await page.evaluate(async () => (await window.cadcraftStudioHub.openApp('cadcraft')) || null);
ok(opened && opened.id === 'app-cadcraft-1', 'the hub opens CadCraft → view app-cadcraft-1');
ok(await waitFor(async () => (await shown()) === 'app-cadcraft-1'), 'the view covers the hub');
ok(await page.evaluate(() => history.state && history.state.ccView === 'app-cadcraft-1'), 'one history entry while it shows');
let cad = null;
ok(await waitFor(async () => { cad = frameOf('app-cadcraft-1'); return cad && (await cad.evaluate(() => !!(window.cadcraft && window.cadcraft.doc))); }, 30000), 'CadCraft runs in the view');
const vinfo = await cad.evaluate(async () => ({ id: (await window.cadcraftDesktop.info()).windowId, keys: Object.keys(window.cadcraftDesktop), studio: !!window.cadcraftStudio }));
ok(vinfo.id === 'app-cadcraft-1', 'inside the view, info().windowId is its own id');
ok(!vinfo.keys.includes('links') && vinfo.keys.includes('windows'), 'the view gets windows (focusHub, closeSelf) but not the hub\'s links');
ok(vinfo.studio, 'Studio\'s window channel runs in the view (window.js)');
ok(await waitFor(() => cad.evaluate(() => (document.querySelector('.app-bar-area') || {}).textContent === 'Tablet')), 'the app bar\'s chip says Tablet');
await shot('02-cadcraft-view');

section('A document through host.js (cc-doc paths)');
await page.evaluate((b64) => { window.__standin.answers.push([{ name: 'kitchen-equipment.dwg', bytes: Uint8Array.from(atob(b64), (c) => c.charCodeAt(0)) }]); }, DWG.toString('base64'));
await page.locator('iframe[name="app-cadcraft-1"]').click({ position: { x: 640, y: 400 } }).catch(() => {});
await page.keyboard.press('Control+o');
ok(await waitFor(() => cad.evaluate(() => window.cadcraft.doc.entities.size > 50), 30000), 'Ctrl+O in CadCraft → the picked document opens (host.js through the bridge)');
const picked = await page.evaluate(() => [...window.__standin.docs.keys()].find((p) => p.endsWith('/kitchen-equipment.dwg')) || null);
ok(/^cc-doc:\/[0-9a-f]+\/kitchen-equipment\.dwg$/.test(picked || ''), 'the page saw a cc-doc path named after the document: ' + picked);
ok(await cad.evaluate(() => window.cadcraft.doc.meta.source === 'kitchen-equipment.dwg'), 'the app takes the drawing\'s name from it');
await shot('03-cadcraft-drawing');
await page.keyboard.press('Control+s');
ok(await waitFor(() => page.evaluate((p) => { const b = window.__standin.docs.get(p); return b && b.length > 1000 && b.length !== 0 && window.__standin.calls.some((c) => c.cmd === 'cc_write_file' && decodeURIComponent(c.headers['x-cc-path']) === p); }, picked), 20000),
  'Ctrl+S writes the document back in place (no save picker)');
ok(!(await page.evaluate(() => window.__standin.calls.some((c) => c.cmd === 'cc_save_dialog'))), 'and asked nothing');
let refused = null;
try { await cad.evaluate(() => window.cadcraftDesktop.readFile('cc-doc:/00000000000000ff/x.dwg')); } catch (e) { refused = String(e); }
ok(/not_allowed/.test(refused || ''), 'an unknown document is refused');

section('Studio Live in a view (TB.5)');
const ls = await cad.evaluate(() => window.cadcraftDesktop.live.session());
ok(ls && ls.view === 'app-cadcraft-1', 'a view\'s Live calls carry its own id (every view shares the one window)');
ok(await page.evaluate(async () => (await window.cadcraftDesktop.live.session()).view === null), 'the hub\'s carry none');

section('Back, switching, closing');
await page.goBack();
ok(await waitFor(async () => (await shown()) === 'main'), 'Back → the hub');
ok(await page.evaluate(async () => (await window.cadcraftDesktop.windows.list()).map((w) => w.id).join()) === 'main,app-cadcraft-1', 'the app stays open behind the hub');
// tangentsoft, 2026-10-15: Back by accident, then the app's tile again must come back to the open drawing, not a new empty view
const again = await page.evaluate(async () => (await window.cadcraftStudioHub.openApp('cadcraft')) || null);
ok(again && again.id === 'app-cadcraft-1' && await waitFor(async () => (await shown()) === 'app-cadcraft-1'), 'the app\'s tile again → back to the same view (its drawing), not a second one');
ok(await cad.evaluate(() => window.cadcraft.doc.entities.size > 50), 'with the drawing still open');
await page.goBack();
ok(await waitFor(async () => (await shown()) === 'main'), 'Back → the hub again');
await page.evaluate(() => window.cadcraftDesktop.windows.focus('app-cadcraft-1'));
ok(await waitFor(async () => (await shown()) === 'app-cadcraft-1'), 'windows.focus(id) shows it again');
await cad.click('[data-studio-hub]').catch(() => {});
ok(await waitFor(async () => (await shown()) === 'main'), 'the app bar\'s Studio button → the hub');
const mk = await page.evaluate(async () => (await window.cadcraftStudioHub.openApp('markupcraft')) || null);
ok(mk && mk.id === 'app-markupcraft-2' && await waitFor(async () => (await shown()) === 'app-markupcraft-2'), 'a second app → its own view');
ok(await page.evaluate(async () => (await window.cadcraftDesktop.windows.list()).length) === 3, 'list(): the hub and two views');
await page.evaluate(() => window.cadcraftDesktop.windows.focus('app-cadcraft-1'));
ok(await waitFor(async () => (await shown()) === 'app-cadcraft-1'), 'switching views keeps one history entry');
await page.goBack();
ok(await waitFor(async () => (await shown()) === 'main'), 'and Back still lands on the hub');
await cad.evaluate(() => { window.addEventListener('beforeunload', (e) => { if (window.__ccTestDirty) { e.preventDefault(); e.returnValue = ''; } }); window.__ccTestDirty = true; });
ok(await page.evaluate(() => window.cadcraftDesktop.windows.close('app-cadcraft-1')) === false, 'close() refuses an app with unsaved work');
ok(await page.evaluate(() => /Open apps/.test(document.getElementById('studio-win-h').textContent)), 'the hub\'s list of open apps is the switcher');
await page.click('[data-close-window="app-cadcraft-1"]');
ok(await waitFor(() => page.evaluate(() => document.querySelector('[data-confirm]').open)), 'Close on an app with unsaved work asks first');
await page.click('[data-confirm-cancel]');
ok(!!frameOf('app-cadcraft-1'), 'Cancel keeps it open');
await page.click('[data-close-window="app-cadcraft-1"]');
await waitFor(() => page.evaluate(() => document.querySelector('[data-confirm]').open));
await page.click('[data-confirm-go]');
ok(await waitFor(() => !frameOf('app-cadcraft-1')), 'Close anyway closes it');
ok(await page.evaluate(() => window.__standin.calls.some((c) => c.cmd === 'cc_live_view_closed' && c.args.view === 'app-cadcraft-1')), 'closing a view ends its Live session');
ok(events.some((e) => e.type === 'opened' && e.id === 'app-cadcraft-1') && events.some((e) => e.type === 'closed' && e.id === 'app-cadcraft-1'), 'onChange: opened, closed');

section('Downloads, links, pop-ups');
const mu = frameOf('app-markupcraft-2');
await page.evaluate(() => { window.__standin.answers.push([{ name: 'list.csv' }]); });
await mu.evaluate(() => { const a = document.createElement('a'); a.href = URL.createObjectURL(new Blob(['a,b\n1,2\n'])); a.download = 'list.csv'; a.click(); URL.revokeObjectURL(a.href); });
ok(await waitFor(() => page.evaluate(() => [...window.__standin.docs.entries()].some(([p, b]) => p.endsWith('/list.csv') && b.length === 8))), '<a download> (revoked at once) → save picker + write');
const before = mu.url();
await mu.evaluate(() => { const a = document.createElement('a'); a.href = 'https://getcadcraft.com/tutorials'; a.textContent = 'x'; document.body.append(a); a.click(); a.remove(); });
const linked = await waitFor(() => page.evaluate(() => window.__standin.calls.some((c) => c.cmd === 'cc_open_url' && c.args.url === 'https://getcadcraft.com/tutorials')), 5000);
ok(linked, 'a link out → the person\'s browser (cc_open_url)' + (linked ? '' : ' ' + await mu.evaluate(() => JSON.stringify({ hooked: document.__ccHooked, fn: typeof window.__ccDocHooks, url: location.href, desk: !!window.cadcraftDesktop })).catch((e) => e.message)));
ok(mu.url() === before, 'and the view stays where it was');
ok(await mu.evaluate(() => window.open('') === null && window.open('/apps/cadcraft/index.html') === null), 'window.open of our own pages → null (no pop-up windows)');

section('Sign-in page and account changes');
await mu.evaluate(() => window.cadcraftDesktop.account.open());
ok(await waitFor(async () => (await shown()) === 'account' && !!frameOf('account')), 'account.open() → the sign-in page as a view');
ok(await page.evaluate(async () => !(await window.cadcraftDesktop.windows.list()).some((w) => w.id === 'account')), 'it is not listed with the apps');
await mu.evaluate(() => { window.cadcraftDesktop.account.onChange((s) => { window.__ccTestAccount = s.state; }); });
await page.evaluate(() => window.__ccDesktopAccount({ state: 'signed_in' }));
ok(await waitFor(() => mu.evaluate(() => window.__ccTestAccount === 'signed_in')), 'an account change reaches every view');
await shot('04-account-view');
await frameOf('account').evaluate(() => window.cadcraftDesktop.account.closeWindow()).catch(() => {});   // the frame goes away mid-call
ok(await waitFor(async () => (await shown()) === 'main' && !frameOf('account')), 'closeWindow() on the sign-in page closes its view');

section('Turned to portrait');
await page.setViewportSize({ width: 800, height: 1280 });
await new Promise((r) => setTimeout(r, 600));
ok(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth + 1), 'the hub fits the width (no sideways scrolling)');
await shot('05-hub-portrait');
await page.evaluate(() => window.cadcraftDesktop.windows.focus('app-markupcraft-2'));
await new Promise((r) => setTimeout(r, 800));
ok(await mu.evaluate(() => innerWidth === 800 && innerHeight === 1280), 'the view follows the turn');
await shot('06-markupcraft-portrait');
const cad2 = await page.evaluate(async () => (await window.cadcraftStudioHub.openApp('cadcraft')).id);
ok(await waitFor(() => frameOf(cad2) && frameOf(cad2).evaluate(() => !!(window.cadcraft && window.cadcraft.doc))), 'CadCraft opens in portrait');
await new Promise((r) => setTimeout(r, 800));
const cf = frameOf(cad2);
ok(await waitFor(() => cf.evaluate(() => document.documentElement.classList.contains('cc-sheets'))), 'upright: the side panels become sheets');
const stage = await cf.evaluate(() => { const m = document.querySelector('main'); const c = m.querySelector('canvas'); return { w: c ? c.getBoundingClientRect().width : 0, vw: innerWidth }; });
ok(stage.w > stage.vw * 0.9, 'the drawing gets the width: ' + Math.round(stage.w) + ' of ' + stage.vw);
await shot('07-cadcraft-portrait');
await cf.click('.app-bar-sheet >> nth=0');
ok(await waitFor(() => cf.evaluate(() => !!document.querySelector('aside.cc-sheet.cc-open'))), 'a panel button slides its sheet in');
await new Promise((r) => setTimeout(r, 400));   // the slide
ok(await cf.evaluate(() => { const r = document.querySelector('aside.cc-open').getBoundingClientRect(); return Math.round(r.left) === 0 && r.width > 300; }), 'the sheet sits at its edge');
ok(await cf.evaluate(() => { const b = document.querySelector('.app-bar'); return b.scrollWidth <= b.clientWidth + 1; }), 'upright, the app bar keeps one row');
await shot('08-cadcraft-sheet');
await cf.evaluate(() => { const c = document.querySelector('main canvas'); const r = c.getBoundingClientRect(); c.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, clientX: r.right - 20, clientY: r.top + 40 })); });
ok(await waitFor(() => cf.evaluate(() => !document.querySelector('aside.cc-sheet.cc-open'))), 'a tap on the drawing puts it away');
// one mechanism (0.5 final sweep): the shell's phone module decides by width — phone under 600 px, the tablet sheets otherwise
ok(await cf.evaluate(() => window.ccShellPhone.tablet() === true && window.ccShellPhone.tabletSheets() && !document.documentElement.classList.contains('cc-phone')), '800 px upright: the tablet sheets, not the phone layout');
await page.setViewportSize({ width: 560, height: 900 });
ok(await waitFor(() => cf.evaluate(() => document.documentElement.classList.contains('cc-phone') && !document.documentElement.classList.contains('cc-sheets')
  && !document.querySelector('aside.cc-sheet') && [...document.querySelectorAll('.app-bar-sheet')].every((b) => b.hidden) && !!document.querySelector('[data-phone-bar]'))), 'under 600 px: the phone layout (sheets from the bar at the bottom), never both');
await page.setViewportSize({ width: 800, height: 1280 });
ok(await waitFor(() => cf.evaluate(() => document.documentElement.classList.contains('cc-sheets') && !document.documentElement.classList.contains('cc-phone'))), 'back to 800 px: the tablet sheets again');
await page.setViewportSize({ width: 1280, height: 800 });
ok(await waitFor(() => cf.evaluate(() => !document.documentElement.classList.contains('cc-sheets') && !document.querySelector('aside.cc-sheet')
  && window.ccShellPanels.panels().every((p) => p.dock !== 'float'))), 'turned sideways: the panels dock back');

section('Problems');
ok(!problems.length, 'no console errors' + (problems.length ? ': ' + problems.slice(0, 5).join(' | ') : ''));

await browser.close();
srv.close();
console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
