// SPDX-License-Identifier: Apache-2.0
// Tests for the npm launcher: a local site on 127.0.0.1 with a test key (download, checks, cache, offline, tampering),
// plus — when a published AppImage and its signature are on this machine (CADCRAFT_REAL_APPIMAGE + CADCRAFT_REAL_SIG) —
// CadCraft's real key against the real file. Nothing here talks to getcadcraft.com.
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { hashFile, verifySignature, CADCRAFT_PUBKEY, parsePubkey } from '../lib/verify.mjs';
import { settings, systemProblem, ensure, cached, prune, run } from '../lib/launcher.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
let passed = 0, failed = 0;
async function t(name, fn) {
  try { await fn(); passed++; console.log(`  ok   ${name}`); } catch (e) { failed++; console.log(`  FAIL ${name}\n       ${e.stack?.split('\n').slice(0, 3).join('\n       ')}`); }
}

// ---- a minisign key and signer (the format Tauri's updater writes), for test files only ----
function makeKey() {
  const { publicKey, privateKey } = crypto.generateKeyPairSync('ed25519');
  const raw = publicKey.export({ format: 'der', type: 'spki' }).subarray(12);
  const id = crypto.randomBytes(8);
  const pub = Buffer.concat([Buffer.from('Ed'), id, raw]);
  const pubB64 = Buffer.from(`untrusted comment: minisign public key: TEST\n${pub.toString('base64')}\n`).toString('base64');
  const sign = (buf, trusted = `timestamp:${Math.floor(Date.now() / 1000)}\tfile:test.AppImage`) => {
    const h = crypto.createHash('blake2b512').update(buf).digest();
    const s = crypto.sign(null, h, privateKey);
    const sig = Buffer.concat([Buffer.from('ED'), id, s]);
    const g = crypto.sign(null, Buffer.concat([s, Buffer.from(trusted)]), privateKey);
    return Buffer.from(`untrusted comment: signature from tauri secret key\n${sig.toString('base64')}\ntrusted comment: ${trusted}\n${g.toString('base64')}\n`).toString('base64');
  };
  return { pubB64, sign };
}

const key = makeKey();
const other = makeKey();
const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'cc-npm-studio-'));
const out = path.join(tmp, 'ran.txt');
const appimage = (v) => Buffer.from(`#!/bin/sh\n# fake CadCraft Studio ${v}\nprintf '%s|' "${v}" "$APPIMAGE_EXTRACT_AND_RUN" "$@" > '${out}'\nexit 7\n`);

// The fake site: /api/v1/updates/… → offer; /downloads/<x.y>/manifest.json; /downloads/<x.y>/<file>.
const site = { version: '0.8.0', body: appimage('0.8.0'), sigBody: null, shaBody: null, down: false, served: 0, origin: '' };
const server = http.createServer((req, res) => {
  if (site.down) { res.writeHead(503); return res.end(); }
  const xy = site.version.split('.').slice(0, 2).join('.');
  const file = `CadCraft-Studio-${xy}-x86_64.AppImage`;
  if (req.url.startsWith('/api/v1/updates/linux/x86_64/')) {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    return res.end(JSON.stringify({ version: site.version, url: `${site.origin}/downloads/${xy}/${file}`, signature: key.sign(site.sigBody ?? site.body) }));
  }
  if (req.url === `/downloads/${xy}/manifest.json`) {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    return res.end(JSON.stringify({ files: [{ file, sha256: crypto.createHash('sha256').update(site.shaBody ?? site.body).digest('hex') }] }));
  }
  if (req.url === `/downloads/${xy}/${file}`) { site.served++; res.writeHead(200, { 'Content-Length': site.body.length }); return res.end(site.body); }
  res.writeHead(404); res.end();
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
site.origin = `http://127.0.0.1:${server.address().port}`;
const env = { CADCRAFT_SITE: site.origin, CADCRAFT_CACHE: path.join(tmp, 'cache'), CADCRAFT_TEST_PUBKEY: key.pubB64 };
const s = settings(env);

console.log('npm launcher (@getcadcraft/studio)');

await t('the built-in key is desktop-shell/updater-key.pub (the key Studio\'s updater trusts)', () => {
  const file = path.join(here, '../../../updater-key.pub');
  if (fs.existsSync(file)) assert.equal(CADCRAFT_PUBKEY, fs.readFileSync(file, 'utf8').trim());
  assert.equal(parsePubkey(CADCRAFT_PUBKEY).keyId.length, 8);
});

await t('settings: https only, except http on this machine; a test key only for a site on this machine', () => {
  assert.throws(() => settings({ CADCRAFT_SITE: 'http://example.com' }), /must be https/);
  assert.equal(settings({ CADCRAFT_SITE: 'https://mirror.example', CADCRAFT_TEST_PUBKEY: key.pubB64 }).pubkey, CADCRAFT_PUBKEY);
  assert.equal(s.pubkey, key.pubB64);
  assert.equal(settings({}).base, 'https://getcadcraft.com');
});

await t('systemProblem: Linux x64 with a desktop only, with a way forward for everyone else', () => {
  assert.equal(systemProblem({ platform: 'linux', arch: 'x64', env: { DISPLAY: ':0' } }), null);
  assert.equal(systemProblem({ platform: 'linux', arch: 'x64', env: { WAYLAND_DISPLAY: 'wayland-0' } }), null);
  assert.match(systemProblem({ platform: 'win32', arch: 'x64', env: {} }), /For Windows, download .*\/download/);
  assert.match(systemProblem({ platform: 'darwin', arch: 'arm64', env: {} }), /For macOS/);
  assert.match(systemProblem({ platform: 'linux', arch: 'arm64', env: { DISPLAY: ':0' } }), /x86_64\) only; this machine is arm64/);
  assert.match(systemProblem({ platform: 'linux', arch: 'x64', env: {} }), /No desktop session/);
});

await t('first run downloads, checks and caches the AppImage', async () => {
  const got = await ensure(s);
  assert.equal(got.version, '0.8.0'); assert.equal(got.fresh, true); assert.equal(site.served, 1);
  assert.ok(fs.statSync(got.path).mode & 0o100, 'executable');
  assert.deepEqual(fs.readdirSync(path.join(s.cacheRoot, '0.8.0')).sort(), ['CadCraft-Studio.AppImage', 'ok']);
});

await t('a second run uses the cache (no download)', async () => {
  const got = await ensure(s);
  assert.equal(got.fresh, false); assert.equal(site.served, 1);
});

await t('run passes the files through and returns Studio\'s exit code; no FUSE → extract and run', async () => {
  const code = await run(cached(s).path, ['plan.dwg', '--flag'], { fuse: false });
  assert.equal(code, 7);
  assert.equal(fs.readFileSync(out, 'utf8'), '0.8.0|1|plan.dwg|--flag|');
  await run(cached(s).path, [], { fuse: true, env: { PATH: process.env.PATH } });
  assert.equal(fs.readFileSync(out, 'utf8'), '0.8.0||');
});

await t('a newer release replaces it; the cache keeps the newest two', async () => {
  for (const v of ['0.8.1', '0.9.0']) { site.version = v; site.body = appimage(v); assert.equal((await ensure(s)).version, v); }
  assert.deepEqual(fs.readdirSync(s.cacheRoot).sort(), ['0.8.1', '0.9.0']);
});

await t('site down → starts the cached version; --offline never asks', async () => {
  site.down = true;
  const msgs = [];
  const got = await ensure(s, { say: (m) => msgs.push(m) });
  assert.equal(got.version, '0.9.0'); assert.match(msgs.join('\n'), /Could not check/);
  const n = site.served;
  assert.equal((await ensure(s, { offline: true })).version, '0.9.0'); assert.equal(site.served, n);
  site.down = false;
});

await t('a file that does not match the manifest SHA-256 is refused and not kept', async () => {
  site.version = '1.0.0'; site.body = appimage('1.0.0'); site.shaBody = appimage('other');
  await assert.rejects(ensure(s), /does not match the SHA-256/);
  assert.equal(cached(s).version, '0.9.0');
  assert.ok(!fs.existsSync(path.join(s.cacheRoot, '1.0.0', 'download.part')));
  site.shaBody = null;
});

await t('a file signed for different bytes is refused', async () => {
  site.version = '1.0.1'; site.body = appimage('1.0.1'); site.sigBody = appimage('evil');
  await assert.rejects(ensure(s), /signature is not CadCraft's: the signature does not match the file/);
  site.sigBody = null;
});

await t('a file signed with another key is refused, even with the right SHA-256', async () => {
  const h = await hashFile(cached(s).path);
  const r = verifySignature(h, other.sign(fs.readFileSync(cached(s).path)), key.pubB64);
  assert.equal(r.ok, false); assert.equal(r.why, 'signed with another key');
});

await t('a changed trusted comment is refused', async () => {
  const buf = fs.readFileSync(cached(s).path);
  const lines = Buffer.from(key.sign(buf), 'base64').toString().split('\n');
  lines[2] = 'trusted comment: timestamp:1\tfile:something-else';
  const r = verifySignature(await hashFile(cached(s).path), Buffer.from(lines.join('\n')).toString('base64'), key.pubB64);
  assert.equal(r.why, 'the trusted comment was changed');
});

await t('a download from another origin than the site is refused', async () => {
  const bad = http.createServer((req, res) => { res.writeHead(200, { 'Content-Type': 'application/json' }); res.end(JSON.stringify({ version: '2.0.0', url: 'https://evil.example/x.AppImage', signature: 'eA==' })); });
  await new Promise((r) => bad.listen(0, '127.0.0.1', r));
  const s2 = settings({ ...env, CADCRAFT_SITE: `http://127.0.0.1:${bad.address().port}`, CADCRAFT_CACHE: path.join(tmp, 'c2') });
  await assert.rejects(ensure(s2), /not on http:\/\/127\.0\.0\.1/);
  bad.close();
});

await t('prune(0) empties the cache', () => {
  prune(s, 0); assert.equal(cached(s), null);
});

await t('cli: --help, --package-version, and a clear refusal without a desktop', () => {
  const cli = path.join(here, '../cli.mjs');
  const h = spawnSync(process.execPath, [cli, '--help'], { encoding: 'utf8' });
  assert.equal(h.status, 0); assert.match(h.stdout, /npx @getcadcraft\/studio/);
  const v = spawnSync(process.execPath, [cli, '--package-version'], { encoding: 'utf8' });
  assert.equal(v.stdout.trim(), JSON.parse(fs.readFileSync(path.join(here, '../package.json'), 'utf8')).version);
  const n = spawnSync(process.execPath, [cli], { encoding: 'utf8', env: { ...env, PATH: process.env.PATH, HOME: tmp } });
  assert.equal(n.status, 2); assert.match(n.stderr, /No desktop session/);
});

await t('the package ships only the launcher (npm pack --dry-run)', () => {
  const r = spawnSync('npm', ['pack', '--dry-run', '--json'], { cwd: path.join(here, '..'), encoding: 'utf8' });
  assert.equal(r.status, 0, r.stderr);
  const files = JSON.parse(r.stdout)[0].files.map((f) => f.path).sort();
  assert.deepEqual(files, ['LICENSE', 'README.md', 'cli.mjs', 'lib/launcher.mjs', 'lib/verify.mjs', 'package.json']);
});

const realFile = process.env.CADCRAFT_REAL_APPIMAGE, realSig = process.env.CADCRAFT_REAL_SIG;
if (realFile && realSig) {
  await t(`CadCraft's real key verifies the published ${path.basename(realFile)}`, async () => {
    const r = verifySignature(await hashFile(realFile), realSig);
    assert.equal(r.ok, true, r.why);
    const tampered = await hashFile(realFile); tampered.blake2b[0] ^= 1;
    assert.equal(verifySignature(tampered, realSig).ok, false);
  });
} else console.log('  skip real-file check (set CADCRAFT_REAL_APPIMAGE and CADCRAFT_REAL_SIG)');

server.close();
fs.rmSync(tmp, { recursive: true, force: true });
console.log(`\n${failed ? 'FAILED' : 'PASSED'}: ${passed} passed, ${failed} failed`);
process.exit(failed ? 1 : 0);
