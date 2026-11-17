// SPDX-License-Identifier: Apache-2.0
// NET.3: hosts.json is complete and the CSP it makes is well formed.
//   node test/hosts.test.mjs            the CSP shape (no site needed)
//   CADCRAFT_SITE=/path/to/getcadcraft.com node test/hosts.test.mjs   also scans each app's dev source: every request URL's host must
//                                       be on that app's list (or `always`, an add-on list, or `reference` = cited, never requested)
import fs from 'node:fs';
import path from 'node:path';
import { HOSTS, hostsFor, connectSrc, strictHeaders } from '../hosts.mjs';

let fail = 0;
const ok = (c, m, x) => { console.log((c ? '  ok   ' : '  FAIL ') + m + (c || x === undefined ? '' : ' ' + JSON.stringify(x))); if (!c) fail++; };

console.log('== the policy');
const bim = connectSrc('bimcraft');
ok(bim.startsWith("connect-src 'self' ") && /\bipc: http:\/\/ipc\.localhost\b/.test(bim) && /\bblob: data:/.test(bim), "own origin, Tauri IPC, blob: and data: are allowed", bim);
for (const h of ['getcadcraft.com', 'earthquake.usgs.gov', 'power.larc.nasa.gov', 'cdn.proj.org']) ok(bim.includes('https://' + h), 'BimCraft may reach ' + h);
ok(!bim.includes('overpass-api.de'), 'BimCraft may not reach a CivilCraft-only host');
ok(!/\*|http:\/\/(?!cadcraft\.localhost|ipc\.localhost)/.test(bim), 'no wildcards and no plain http to the internet');
const studio = hostsFor('studio');
ok(Object.keys(HOSTS.apps).every((a) => hostsFor(a).every((h) => studio.includes(h))), 'Studio (the suite) carries every app\'s hosts');
ok(Object.keys(strictHeaders('studio')).join() === 'Content-Security-Policy', 'one header, Content-Security-Policy');
let threw = false; try { hostsFor('nosuchapp'); } catch { threw = true; }
ok(threw, 'an app with no list stops the build instead of shipping no policy');
const listed = new Set([...Object.keys(HOSTS.always), ...Object.values(HOSTS.apps).flatMap(Object.keys), ...Object.values(HOSTS.addons).flatMap(Object.keys)]);
ok(HOSTS.reference.every((h) => !listed.has(h)), 'no host is both allowed and "reference only"');

const SITE = process.env.CADCRAFT_SITE;
if (!SITE) console.log('== source scan skipped (set CADCRAFT_SITE)');
else {
  console.log('== every request URL in the apps is on a list');
  const REQ = /fetch\(|url: *[`'"]https|request|_BASE|BASE *=|CDN|endpoint|return [`'"]https/;
  const HOST = /[`'"]https:\/\/([a-zA-Z0-9.-]+\.[a-z]{2,})/g;
  const walk = (d, out = []) => {
    if (!fs.existsSync(d)) return out;
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) { if (!['help', 'lang', 'i18n', 'node_modules'].includes(e.name)) walk(p, out); }
      else if (e.name.endsWith('.js') && !e.name.endsWith('.min.js') && fs.statSync(p).size < 2e6) out.push(p);
    }
    return out;
  };
  const shared = walk(path.join(SITE, 'public_html/assets/js'));
  for (const app of Object.keys(HOSTS.apps)) {
    const allowed = new Set([...hostsFor(app), ...HOSTS.reference]);
    const missing = new Map();
    for (const f of [...walk(path.join(SITE, 'public_html/assets/apps', app, 'dev')), ...shared]) {
      for (const line of fs.readFileSync(f, 'utf8').split('\n')) {
        if (!REQ.test(line)) continue;
        for (const m of line.matchAll(HOST)) if (!allowed.has(m[1]) && !missing.has(m[1])) missing.set(m[1], path.relative(SITE, f));
      }
    }
    ok(missing.size === 0, app + ': no request to a host off its list', Object.fromEntries(missing));
  }
}
console.log(fail ? fail + ' failed' : 'all passed');
process.exit(fail ? 1 : 0);
