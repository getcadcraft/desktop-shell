#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// updater-feed.mjs — the update entries of one release, for getcadcraft.com's update endpoint (0.7: one endpoint for every
// edition; owner 2026-10-24).
//
// Every edition and version asks ONE endpoint: https://getcadcraft.com/api/v1/updates/{{target}}/{{arch}}/{{current_version}}
// ?edition=<edition> (the site's UpdateFeed service; 204 = up to date, else Tauri's {version, notes, pub_date, url,
// signature}). The service answers only with files that are published on /download, and it needs each file's updater
// signature: that is what this script records. Each build writes one entry per file it signed (a fragment under
// out/updates/<slug>-<version>/), on the machine that built it; the site's `Downloads::publish()` reads the fragments of the
// version it publishes (desktop-shell/out and stores/out/linux), keeps those whose file it published with the same SHA-256,
// checks each signature against the owner's public key and writes downloads/<x.y>/updates.json.
//
//   node updater-feed.mjs add <slug> <version> <edition> <target> <arch> <published-file> <file> <sig-file> [--notes text] [--out dir]
//   node updater-feed.mjs list <slug> <version> [--out dir]          → the entries, one line each
//   node updater-feed.mjs check <slug> <version> [--allow-missing e,…] [--out dir]
//
// `check` is the release gate: every edition below must have an entry (with a signature and a SHA-256), or the people on it get
// no update; leave one out on purpose with --allow-missing (it is then said, never silent).
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';

const HERE = path.dirname(new URL(import.meta.url).pathname);
/** Editions the endpoint knows (shell.json updates.edition), with the platform each is for. msix-* are updated by Windows
 *  (App Installer / the Store): they have no updater entry (the MSIX itself is the file, served through the .appinstaller). */
export const EDITIONS = {
  installer: { target: 'windows', arch: 'x86_64' },
  portable: { target: 'windows', arch: 'x86_64' },
  deb: { target: 'linux', arch: 'x86_64' },
  appimage: { target: 'linux', arch: 'x86_64' },
  dmg: { target: 'darwin', arch: ['aarch64', 'x86_64'] },
};
export const PACKAGE_EDITIONS = ['msix-web', 'msix-store'];
/** What a complete Studio release carries. Not dmg yet: the Mac's updater installs the .app.tar.gz, which /download does not
 *  publish, so the endpoint answers 204 to the Mac until that file is a published kind (an entry is still recorded). */
export const RELEASE_EDITIONS = ['installer', 'portable', 'deb', 'appimage'];
const TARGETS = ['windows', 'linux', 'darwin'];
const ARCHES = ['x86_64', 'aarch64'];

const fragDir = (out, slug, version) => path.join(out, 'updates', `${slug}-${version}`);

export function addEntry({ out = path.join(HERE, 'out'), slug, version, edition, target, arch, published, file, sig, notes }) {
  if (!EDITIONS[edition]) throw new Error(`unknown edition ${edition} (one of ${Object.keys(EDITIONS).join(', ')})`);
  if (!TARGETS.includes(target) || !ARCHES.includes(arch)) throw new Error(`bad target/arch ${target}/${arch}`);
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error(`version must be x.y.z (the installer's own version), not ${version}`);
  if (!/^[A-Za-z0-9._-]+$/.test(published)) throw new Error(`published name ${published}: letters, digits, . _ - only`);
  const signature = fs.readFileSync(sig, 'utf8').trim();
  if (!signature) throw new Error(`${sig} is empty`);
  const sha256 = crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
  const d = fragDir(out, slug, version);
  fs.mkdirSync(d, { recursive: true });
  const entry = { format: 'cadcraft-update-entry', app: slug, version, edition, target, arch, file: published, sha256, bytes: fs.statSync(file).size, signature, notes: notes || undefined };
  const f = path.join(d, `${edition}-${target}-${arch}.json`);
  fs.writeFileSync(f, JSON.stringify(entry, null, 2) + '\n');
  return { file: f, entry };
}

export function list({ out = path.join(HERE, 'out'), slug, version }) {
  const d = fragDir(out, slug, version);
  return fs.existsSync(d) ? fs.readdirSync(d).filter((f) => f.endsWith('.json')).sort().map((f) => JSON.parse(fs.readFileSync(path.join(d, f), 'utf8'))) : [];
}

/** → { missing: [edition], problems: [text] } for the entries of one release (from one or more out folders). */
export function check(entries, { allowMissing = [] } = {}) {
  const problems = [];
  const have = new Set(entries.map((e) => e.edition));
  const missing = RELEASE_EDITIONS.filter((e) => !have.has(e) && !allowMissing.includes(e));
  for (const e of entries) {
    if (!EDITIONS[e.edition]) problems.push(`${e.edition}: not an edition the endpoint knows`);
    if (!e.signature) problems.push(`${e.edition}: no signature`);
    if (!/^[0-9a-f]{64}$/.test(String(e.sha256 || ''))) problems.push(`${e.edition}: no SHA-256`);
    if (!/^\d+\.\d+\.\d+$/.test(String(e.version || ''))) problems.push(`${e.edition}: version ${e.version} is not x.y.z`);
  }
  for (const m of missing) problems.push(`${m}: missing — people on that edition would get no update`);
  return { missing, problems };
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const argv = process.argv.slice(2);
  const opt = (k) => { const i = argv.indexOf(k); return i >= 0 ? argv.splice(i, 2)[1] : undefined; };
  const notes = opt('--notes'), outs = (opt('--out') || path.join(HERE, 'out')).split(','), allow = (opt('--allow-missing') || '').split(',').filter(Boolean);
  const [cmd, ...a] = argv;
  try {
    if (cmd === 'add' && a.length === 8) {
      const r = addEntry({ out: outs[0], slug: a[0], version: a[1], edition: a[2], target: a[3], arch: a[4], published: a[5], file: a[6], sig: a[7], notes });
      console.log(`update entry ${r.entry.edition} ${r.entry.target}-${r.entry.arch} → ${path.relative(process.cwd(), r.file)}`);
    } else if (cmd === 'list' && a.length === 2) {
      for (const o of outs) for (const e of list({ out: o, slug: a[0], version: a[1] })) console.log(`${e.edition}\t${e.target}-${e.arch}\t${e.version}\t${e.file}\t${e.sha256.slice(0, 12)}`);
    } else if (cmd === 'check' && a.length === 2) {
      const entries = outs.flatMap((o) => list({ out: o, slug: a[0], version: a[1] }));
      const r = check(entries, { allowMissing: allow });
      for (const p of r.problems) console.error('  ✗ ' + p);
      for (const p of allow) console.warn(`  ! ${p} left out on purpose`);
      if (r.problems.length) process.exit(1);
      console.log(`every edition has its entry (${entries.length})`);
    } else {
      console.error('usage: updater-feed.mjs add <slug> <version> <edition> <target> <arch> <published> <file> <sig> | list <slug> <version> | check <slug> <version>');
      process.exit(2);
    }
  } catch (e) { console.error('!! ' + e.message); process.exit(1); }
}
