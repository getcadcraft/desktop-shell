// SPDX-License-Identifier: Apache-2.0
// fullscreen-mac.mjs — GH.1 (desktop-shell#1): on macOS the app bar's screen-mode button must take the WINDOW full screen
// (the green button's path) and stay there. Runs the site's real app-shell.js on top of bridge.js with the macOS flag
// (nativeFullscreen) in Chromium, against a stand-in shell whose window full screen is a variable; Chromium's own
// Fullscreen API is watched and must never be used for the document.
//   CADCRAFT_SITE=… node test/fullscreen-mac.mjs
// Needs: the site's checkout (app-shell.js; unset = skipped), playwright-core in the site's node_modules (or
// CADCRAFT_STUDIO's), a Playwright Chromium in ~/.cache/ms-playwright. It is NOT macOS: the owner checks on a Mac.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const HERE = path.dirname(new URL(import.meta.url).pathname);
const SITE = process.env.CADCRAFT_SITE;
if (!SITE) { console.log('CADCRAFT_SITE unset: skipped'); process.exit(0); }
let pass = 0, fail = 0;
const ok = (c, what, extra = '') => { if (c) { pass++; console.log(`  ✓ ${what}`); } else { fail++; console.log(`  ✗ ${what} ${extra}`); } };

const pw = [path.join(SITE, 'node_modules/playwright-core/index.mjs'),
  process.env.CADCRAFT_STUDIO && path.join(process.env.CADCRAFT_STUDIO, 'node_modules/playwright-core/index.mjs')].find((p) => p && fs.existsSync(p));
if (!pw) { console.log('playwright-core not found: skipped'); process.exit(0); }
const { chromium } = await import(pw);
const cacheDir = path.join(os.homedir(), '.cache/ms-playwright');
const cbuild = fs.readdirSync(cacheDir).filter((d) => /^chromium-\d+$/.test(d)).sort((a, b) => parseInt(b.split('-')[1]) - parseInt(a.split('-')[1]))[0];
const exe = ['chrome-linux64/chrome', 'chrome-linux/chrome'].map((s) => path.join(cacheDir, cbuild, s)).find((p) => fs.existsSync(p));

// The stand-in shell: the window's full screen is window.__win.full; every command is logged.
const STANDIN = `(() => {
  const win = window.__win = { full: false, calls: [], webkit: 0 };
  const own = Element.prototype.requestFullscreen;
  Element.prototype.requestFullscreen = function (o) { win.webkit++; return own.call(this, o); };
  window.__TAURI_INTERNALS__ = { invoke: async (cmd, args) => {
    win.calls.push(cmd + (args && 'on' in args ? ':' + args.on : ''));
    if (cmd === 'cc_set_fullscreen') { setTimeout(() => { win.full = !!args.on; }, 120); return null; }   // the animation
    if (cmd === 'cc_is_fullscreen') return win.full;
    return null;
  } };
})();`;
const FLAGS = 'window.__ccShellFlags=Object.freeze({account:false,suite:false,live:false,mobile:false,nativeFullscreen:true});';
const BRIDGE = fs.readFileSync(path.join(HERE, '../bridge/bridge.js'), 'utf8');
const SHELL = fs.readFileSync(path.join(SITE, 'public_html/assets/js/app-shell.js'), 'utf8');
const PAGE = `<!doctype html><html><head><meta charset="utf-8"></head><body class="app-shell">
<header class="app-bar"><button type="button" class="app-bar-fs" data-fs-cycle hidden><i class="bi bi-fullscreen"></i><span class="app-bar-fs-label">Full screen</span></button></header>
<main id="main"><div role="toolbar">tools</div><section>drawing</section></main>
<script>${SHELL}</script></body></html>`;

const browser = await chromium.launch({ executablePath: exe, args: ['--no-sandbox'] });
try {
  const ctx = await browser.newContext({ viewport: { width: 1280, height: 800 } });
  await ctx.addInitScript(STANDIN);
  await ctx.addInitScript(FLAGS);
  await ctx.addInitScript(BRIDGE);
  const page = await ctx.newPage();
  const problems = [];
  page.on('pageerror', (e) => problems.push(e.message));
  await page.route('https://app.test/**', (r) => r.fulfill({ contentType: 'text/html', body: PAGE }));
  await page.goto('https://app.test/');
  const st = () => page.evaluate(() => ({ mode: window.ccShellScreen.mode(), win: window.__win.full, el: !!document.fullscreenElement,
    webkit: window.__win.webkit, calls: window.__win.calls.slice(), full: document.body.classList.contains('shell-full'),
    hidden: document.body.classList.contains('shell-autohide') }));
  const btn = page.locator('[data-fs-cycle]');

  ok(await btn.isVisible() && await page.evaluate(() => window.ccShellScreen.canFull), 'the screen-mode button shows, full screen offered');
  await btn.click();
  await page.waitForTimeout(400);
  let s = await st();
  ok(s.mode === 'full' && s.win && s.el && s.full, 'button → full screen: the window is full screen, app-shell in "full"', JSON.stringify(s));
  ok(s.webkit === 0, 'the web view\'s own element fullscreen was not used');
  await page.waitForTimeout(1500);
  s = await st();
  ok(s.mode === 'full' && s.win && s.calls.filter((c) => c === 'cc_set_fullscreen:false').length === 0, 'it stays full screen (GH.1: no immediate exit)', JSON.stringify(s));
  await btn.click();
  await page.waitForTimeout(100);
  s = await st();
  ok(s.mode === 'autohide' && s.hidden && s.win, 'button again → bars hidden, still full screen');
  await btn.click();
  await page.waitForTimeout(300);
  s = await st();
  ok(s.mode === 'normal' && !s.win && !s.el && !s.full, 'button again → normal, the window leaves full screen', JSON.stringify(s));

  await page.keyboard.press('Control+Shift+F');
  await page.waitForTimeout(400);
  s = await st();
  ok(s.mode === 'full' && s.win, 'Ctrl+Shift+F → full screen');
  await page.keyboard.press('Escape');
  await page.waitForTimeout(300);
  s = await st();
  ok(s.mode === 'normal' && !s.win, 'Esc → normal, window back', JSON.stringify(s));

  // The green button / Window › Exit Full Screen while in the in-app full screen: app-shell follows.
  await btn.click();
  await page.waitForTimeout(400);
  await page.evaluate(() => { window.__win.full = false; });
  await page.setViewportSize({ width: 1200, height: 760 });   // leaving full screen resizes the web view
  await page.waitForTimeout(500);
  s = await st();
  ok(s.mode === 'normal' && !s.el, 'leaving with the green button → app-shell back to normal', JSON.stringify(s));
  // Green button into full screen first, then the in-app button: no fight, stays.
  await page.evaluate(() => { window.__win.full = true; });
  await btn.click();
  await page.waitForTimeout(1500);
  s = await st();
  ok(s.mode === 'full' && s.win, 'green button first, then the in-app button → full, stays');
  ok(problems.length === 0, 'no page errors', problems.join('; '));
} finally {
  await browser.close();
}
console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
