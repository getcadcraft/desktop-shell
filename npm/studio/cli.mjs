#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// npx @getcadcraft/studio [options] [files…] — see README.md.
import fs from 'node:fs';
import { settings, systemProblem, ensure, run, cached, prune } from './lib/launcher.mjs';

const OWN = new Set(['--help', '-h', '--offline', '--where', '--clean', '--quiet', '--package-version']);
const argv = process.argv.slice(2);
const dd = argv.indexOf('--');
const mine = (dd === -1 ? argv : argv.slice(0, dd)).filter((a) => OWN.has(a));
const pass = dd === -1 ? argv.filter((a) => !OWN.has(a)) : [...argv.slice(0, dd).filter((a) => !OWN.has(a)), ...argv.slice(dd + 1)];
const has = (f) => mine.includes(f);
const say = has('--quiet') ? () => {} : (m) => console.error(m);

const HELP = `CadCraft Studio for Linux, through npm.

  npx @getcadcraft/studio [options] [files…]

Downloads CadCraft Studio's AppImage from getcadcraft.com the first time (and when a newer version is out),
checks its SHA-256 and CadCraft's signature, keeps it in ~/.cache/cadcraft/studio and starts it. Files you
name are opened in CadCraft Studio. Anything after -- goes to it unchanged.

Options
  --offline           start the version in the cache without asking the site
  --where             print where the cache is and which version it holds
  --clean             remove every cached version
  --quiet             no progress messages
  --package-version  this npm package's version (not CadCraft Studio's)
  -h, --help          this help

Linux on 64-bit x86 with a desktop session. Other systems: https://getcadcraft.com/download
or every app in the browser: https://getcadcraft.com/use`;

async function main() {
  if (has('--help') || has('-h')) { console.log(HELP); return 0; }
  if (has('--package-version')) {
    console.log(JSON.parse(fs.readFileSync(new URL('./package.json', import.meta.url), 'utf8')).version);
    return 0;
  }
  const s = settings();
  if (has('--where')) { const c = cached(s); console.log(s.cacheRoot + (c ? `\n${c.version}  ${c.path}` : '\n(empty)')); return 0; }
  if (has('--clean')) { prune(s, 0); console.log(`Removed every cached version from ${s.cacheRoot}`); return 0; }
  const problem = systemProblem();
  if (problem) { console.error(`cadcraft-studio: ${problem}`); return 2; }
  const got = await ensure(s, { offline: has('--offline'), say });
  if (!got.fresh) say(`Starting CadCraft Studio ${got.version}.`);
  return run(got.path, pass);
}

main().then((code) => process.exit(code), (e) => { console.error(`cadcraft-studio: ${e.message}`); process.exit(1); });
