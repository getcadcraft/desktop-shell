// SPDX-License-Identifier: Apache-2.0
// lang.test.mjs — the desktop shell's language files (lang/*.json) and their loader (bridge/shell-i18n.js). No webview:
//   node test/lang.test.mjs
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
let pass = 0, fail = 0;
const ok = (c, what, extra = '') => { if (c) { pass++; console.log(`  ✓ ${what}`); } else { fail++; console.log(`  ✗ ${what} ${extra}`); } };

const files = fs.readdirSync(path.join(ROOT, 'lang')).filter((f) => f.endsWith('.json'));
const L = Object.fromEntries(files.map((f) => [f.replace(/\.json$/, ''), JSON.parse(fs.readFileSync(path.join(ROOT, 'lang', f), 'utf8'))]));
const keys = (o) => Object.keys(o).filter((k) => !k.startsWith('@')).sort();
const fills = (v) => [...new Set((typeof v === 'object' ? Object.values(v).join(' ') : String(v)).match(/\{\w+\}/g) || [])].sort().join();

console.log('desktop shell language files');
ok(L.en && L['es-MX'], 'en.json and es-MX.json');
for (const [loc, o] of Object.entries(L)) ok(o['@locale'] === loc, `${loc}: @locale matches the file name`);
ok(/borrador/.test(L['es-MX']['@name']) && /Not yet reviewed/.test(L['es-MX']['@note'] || ''), 'es-MX is marked as a draft (borrador, not reviewed)');
for (const loc of Object.keys(L).filter((l) => l !== 'en')) {
  ok(JSON.stringify(keys(L[loc])) === JSON.stringify(keys(L.en)), `${loc}: exactly en's keys`, JSON.stringify({ missing: keys(L.en).filter((k) => !(k in L[loc])), extra: keys(L[loc]).filter((k) => !(k in L.en)) }));
  const bad = keys(L.en).filter((k) => k in L[loc] && fills(L.en[k]).replace('{n}', '') !== fills(L[loc][k]).replace('{n}', ''));
  ok(!bad.length, `${loc}: the same fills as English`, bad.join(', '));
  ok(keys(L.en).every((k) => typeof L.en[k] !== 'object' || (typeof L[loc][k] === 'object' && 'other' in L[loc][k])), `${loc}: plurals have "other"`);
}
// Every key the shell's pages use exists in English.
const pages = ['account.html', 'carry.html'].map((f) => fs.readFileSync(path.join(ROOT, 'bridge', f), 'utf8')).join('\n');
const used = new Set([...pages.matchAll(/data-t="([^"]+)"/g), ...pages.matchAll(/\bt\('([a-z][\w.]+)'/g)].map((m) => m[1]).filter((k) => !k.endsWith('.')));
for (const r of (pages.match(/REASONS = \[([^\]]+)\]/) || [, ''])[1].matchAll(/'(\w+)'/g)) used.add('desktop.reason.' + r[1]);
const missing = [...used].filter((k) => !(k in L.en));
ok(used.size > 20 && !missing.length, `every key the pages use is in en.json (${used.size})`, missing.join(', '));
const unused = keys(L.en).filter((k) => !used.has(k));
ok(!unused.length, 'no key in en.json goes unused', unused.join(', '));
// The protocol serves every file it names.
const proto = fs.readFileSync(path.join(ROOT, 'src-tauri/src/protocol.rs'), 'utf8');
const served = [...proto.matchAll(/include_bytes!\("\.\.\/\.\.\/([^"]+)"\)/g)].map((m) => m[1]);
ok(served.includes('lang/en.json') && served.includes('lang/es-MX.json') && served.includes('bridge/shell-i18n.js') && served.every((f) => fs.existsSync(path.join(ROOT, f))), 'protocol.rs serves the loader and both files');

// The loader: which language, plurals, fills, fallback to English.
const src = fs.readFileSync(path.join(ROOT, 'bridge/shell-i18n.js'), 'utf8');
async function run({ stored = null, cookie = '', langs = ['en-US'] }) {
  const nodes = [{ a: 'desktop.account.get_code', textContent: '' }];
  const doc = { cookie, documentElement: {}, querySelectorAll: () => nodes.map((n) => ({ getAttribute: () => n.a, set textContent(v) { n.textContent = v; } })) };
  const win = {};
  const ctx = vm.createContext({ window: win, document: doc, navigator: { languages: langs, language: langs[0] },
    localStorage: { getItem: () => stored }, Intl, Promise, Object, String, Number, JSON,
    fetch: async (u) => { const l = u.match(/lang\/(.+)\.json$/)[1]; return { ok: !!L[l], json: async () => L[l] }; } });
  vm.runInContext(src, ctx);
  const r = await win.ccShellI18n.ready;
  return { ...r, node: nodes[0].textContent, html: doc.documentElement.lang };
}
let r = await run({});
ok(r.locale === 'en' && r.node === 'Get a code' && r.html === 'en', 'English by default; data-t filled');
r = await run({ langs: ['es-AR', 'en'] });
ok(r.locale === 'es-MX' && r.node === L['es-MX']['desktop.account.get_code'], 'the system\'s es-AR → es-MX (by language)');
r = await run({ stored: 'en', langs: ['es-MX'] });
ok(r.locale === 'en', 'Studio\'s own choice (cadcraft.studio.lang) wins over the system');
r = await run({ cookie: 'x=1; cc_lang=es-MX', langs: ['en'] });
ok(r.locale === 'es-MX', 'the apps\' cookie cc_lang next');
ok(r.t('desktop.account.waiting.left', { n: 1 }) === 'El código vale 1 minuto más.' && r.t('desktop.account.waiting.left', { n: 4 }) === 'El código vale 4 minutos más.', 'plurals with {n}');
r = await run({});
ok(r.t('desktop.account.waiting.left', { n: 1 }) === 'The code works for 1 more minute.' && r.t('desktop.account.waiting.left', { n: 7 }) === 'The code works for 7 more minutes.', 'English plurals');
ok(r.t('nope.key') === 'nope.key', 'an unknown key shows itself (the test above makes sure none is used)');

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
