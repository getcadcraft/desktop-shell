// SPDX-License-Identifier: Apache-2.0
// msix/layout.mjs — the folder MakeAppx packs into CadCraft Studio's .msix (0.7 MSIX).
//
//   <out>/msix/<name>-<version>/        the package layout
//     <exe>.exe  app/  shell.json  THIRD_PARTY.md  LICENSE   what the install folder holds today
//     AppxManifest.xml                  msix/manifest.mjs
//     Assets/*.png                      the tiles, drawn from the app icons (Pillow, as make-icon.py)
//
// Nothing here signs or packs: `makeappx pack /d <layout> /p <file>.msix` is a Windows SDK tool (msix/PACK.md; the Store
// signs the package itself, so no certificate of ours is needed for the Store). Cross-building the layout on Linux is how the
// manifest and the tiles get checked without a Windows machine; `checkLayout` below is that check.
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { manifest, tiles, msixVersion } from './manifest.mjs';

/**
 * The package identity. Two packages, two identities (a package is known by Name + Publisher, and the Publisher must equal the
 * signing certificate's subject):
 *   store — what Partner Center reserved (MSIX_IDENTITY_NAME, MSIX_PUBLISHER, MSIX_PUBLISHER_DISPLAY); the Store signs it.
 *   web   — the website's package, signed by us with the Azure certificate: MSIX_WEB_PUBLISHER = that certificate's subject
 *           exactly (MSIX_WEB_IDENTITY_NAME default CadCraft.Studio, MSIX_WEB_PUBLISHER_DISPLAY default the company).
 * With none given the layout is made with a visible stand-in, never shipped (`reserved: false`).
 */
export function identityFrom(env = process.env, kind = 'store') {
  if (kind === 'web') {
    const publisher = env.MSIX_WEB_PUBLISHER;
    const name = env.MSIX_WEB_IDENTITY_NAME || 'CadCraft.Studio';
    const display = env.MSIX_WEB_PUBLISHER_DISPLAY || 'The Data Design Group, Inc.';
    if (publisher) return { identityName: name, publisher, publisherDisplay: display, reserved: true };
    return { identityName: name, publisher: 'CN=NO-SIGNING-CERTIFICATE-NOT-FOR-PUBLISHING', publisherDisplay: display, reserved: false };
  }
  const name = env.MSIX_IDENTITY_NAME, publisher = env.MSIX_PUBLISHER, display = env.MSIX_PUBLISHER_DISPLAY;
  if (name && publisher && display) return { identityName: name, publisher, publisherDisplay: display, reserved: true };
  if (name || publisher || display) throw new Error('MSIX_IDENTITY_NAME, MSIX_PUBLISHER and MSIX_PUBLISHER_DISPLAY come together (Partner Center › Product identity)');
  return { identityName: 'CadCraft.StudioUnreserved', publisher: 'CN=UNRESERVED-NOT-FOR-THE-STORE', publisherDisplay: 'The Data Design Group, Inc.', reserved: false };
}

export function buildLayout({ out, name, version, exe, exePath, stage, apps, fileTypes, shellDir, python = 'python3', iconFor, identity, kind = 'store', shell }) {
  const dir = path.join(out, 'msix', `${name.replace(/\s+/g, '-')}-${version}${kind === 'web' ? '' : '-store'}`);
  fs.rmSync(dir, { recursive: true, force: true });
  fs.mkdirSync(path.join(dir, 'Assets'), { recursive: true });
  fs.copyFileSync(exePath, path.join(dir, exe));
  fs.cpSync(path.join(stage, 'app'), path.join(dir, 'app'), { recursive: true, dereference: true });
  // shell.json: the stage's, or (the website package from the installer's stage) the one build.mjs gives for this edition.
  if (shell) fs.writeFileSync(path.join(dir, 'shell.json'), JSON.stringify(shell, null, 2));
  else fs.copyFileSync(path.join(stage, 'shell.json'), path.join(dir, 'shell.json'));
  for (const f of ['THIRD_PARTY.md', 'LICENSE']) if (fs.existsSync(path.join(shellDir, f))) fs.copyFileSync(path.join(shellDir, f), path.join(dir, f));
  // Tiles: the hub's mark and one per app (iconFor(slug|null) → a PNG path), cut to the sizes the manifest names.
  const cut = `
import sys
from PIL import Image
src, dst, w, h = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
im = Image.open(src).convert('RGBA')
side = min(w, h)
sq = im.resize((side, side), Image.LANCZOS)
canvas = Image.new('RGBA', (w, h), (0, 0, 0, 0))
canvas.paste(sq, ((w - side) // 2, (h - side) // 2), sq)
canvas.save(dst)
`;
  for (const [file, [w, h], slug] of tiles(apps)) {
    const src = iconFor(slug);
    const r = spawnSync(python, ['-c', cut, src, path.join(dir, 'Assets', file), String(w), String(h)], { encoding: 'utf8' });
    if (r.status !== 0) throw new Error(`tile ${file}: ${(r.stderr || '').trim().split('\n').pop()}`);
  }
  const xml = manifest({
    ...identity, version, displayName: name, exe, apps, fileTypes,
    description: 'All eight CadCraft apps in one install.',
  });
  fs.writeFileSync(path.join(dir, 'AppxManifest.xml'), xml);
  return dir;
}

/** Every file the manifest names exists, and shell.json fits the package's kind. → problems[]
 *  store: a store build (no sign-in until decision 11, store: true) whose updates entry, if any, is the msix-store edition with
 *         no endpoint — the Store updates it (Studio asks StoreContext; the Tauri updater never runs inside a package).
 *  web:   the website's package: sign-in as the installer, updates entry msix-web (App Installer updates it). */
export function checkLayout(dir, kind = 'store') {
  const problems = [];
  const xml = fs.readFileSync(path.join(dir, 'AppxManifest.xml'), 'utf8');
  for (const m of xml.matchAll(/(?:Executable|Square150x150Logo|Square44x44Logo|Wide310x150Logo)="([^"]+)"/g)) {
    const f = path.join(dir, ...m[1].split('\\'));
    if (!fs.existsSync(f)) problems.push(`the manifest names ${m[1]}, which is not in the layout`);
  }
  for (const m of xml.matchAll(/<Logo>([^<]+)<\/Logo>/g)) if (!fs.existsSync(path.join(dir, ...m[1].split('\\')))) problems.push(`the manifest names ${m[1]}, which is not in the layout`);
  if (!fs.existsSync(path.join(dir, 'app'))) problems.push('no app/ folder');
  if (!fs.existsSync(path.join(dir, 'shell.json'))) problems.push('no shell.json');
  else {
    const sj = JSON.parse(fs.readFileSync(path.join(dir, 'shell.json'), 'utf8'));
    const u = sj.updates;
    if (kind === 'store') {
      if (u && (u.edition !== 'msix-store' || u.endpoint)) problems.push('shell.json: the Store package has an update feed: the Store updates a package (edition msix-store, no endpoint)');
      if (sj.account !== false) problems.push('shell.json: account is not false (store builds have no sign-in until decision 11)');
      if (sj.store !== true) problems.push('shell.json: not a store build (build with --store)');
    } else {
      if (!u || u.edition !== 'msix-web') problems.push('shell.json: the website package needs updates.edition msix-web');
      if (sj.store) problems.push('shell.json: the website package is not a store build');
      if (sj.portable) problems.push('shell.json: a package is never portable');
    }
  }
  const ver = /Version="([^"]+)"/.exec(xml.slice(xml.indexOf('<Identity')))?.[1];
  try { msixVersion(ver); } catch { problems.push(`bad version ${ver}`); }
  if (!/ProcessorArchitecture="x64"/.test(xml)) problems.push('not x64');
  return problems;
}
