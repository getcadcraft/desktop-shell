// SPDX-License-Identifier: Apache-2.0
// The launcher behind `npx @getcadcraft/studio`: fetch CadCraft Studio's Linux AppImage from getcadcraft.com (once per
// version), check it (SHA-256 from the release manifest + the updater signature), keep it in the cache and start it.
// Studio's own update check takes over from there; this package changes only when the launcher does.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import { hashFile, verifySignature, CADCRAFT_PUBKEY } from './verify.mjs';

export const SITE = 'https://getcadcraft.com';
const UA = 'cadcraft-studio-npm';

/** Settings from the environment. A different site (tests, mirrors) must be https, or http on this machine only. */
export function settings(env = process.env) {
  const base = (env.CADCRAFT_SITE || SITE).replace(/\/+$/, '');
  const u = new URL(base);
  const loopback = ['127.0.0.1', 'localhost', '[::1]'].includes(u.hostname);
  if (u.protocol !== 'https:' && !(u.protocol === 'http:' && loopback)) throw new Error(`CADCRAFT_SITE must be https (got ${base})`);
  const cacheRoot = env.CADCRAFT_CACHE || path.join(env.XDG_CACHE_HOME || path.join(os.homedir(), '.cache'), 'cadcraft', 'studio');
  // A test key is honoured only against a site on this machine, so it can never replace CadCraft's key for real downloads.
  const pubkey = loopback && env.CADCRAFT_TEST_PUBKEY ? env.CADCRAFT_TEST_PUBKEY : CADCRAFT_PUBKEY;
  return { base, cacheRoot, pubkey };
}

/** Can this machine run the Linux AppImage? → null (yes) or the reason, in words. */
export function systemProblem({ platform = process.platform, arch = process.arch, env = process.env } = {}) {
  if (platform !== 'linux') {
    const where = platform === 'win32' ? 'Windows' : platform === 'darwin' ? 'macOS' : platform;
    return `This launcher is for Linux. For ${where}, download CadCraft Studio from ${SITE}/download, or use every app in your browser at ${SITE}/use.`;
  }
  if (arch !== 'x64') return `CadCraft Studio for Linux is built for 64-bit x86 (x86_64) only; this machine is ${arch}. Every app runs in your browser at ${SITE}/use.`;
  if (!env.DISPLAY && !env.WAYLAND_DISPLAY) return 'No desktop session (DISPLAY or WAYLAND_DISPLAY is not set): Studio opens windows, so run this from a desktop.';
  return null;
}

/** True when the AppImage can mount itself (FUSE 2 present); otherwise it is told to extract and run. */
export function hasFuse() {
  if (!fs.existsSync('/dev/fuse')) return false;
  const r = spawnSync('ldconfig', ['-p'], { encoding: 'utf8' });
  if (r.status === 0) return /libfuse\.so\.2\b/.test(r.stdout);
  return ['/usr/lib/x86_64-linux-gnu/libfuse.so.2', '/lib/x86_64-linux-gnu/libfuse.so.2', '/usr/lib64/libfuse.so.2', '/usr/lib/libfuse.so.2'].some((p) => fs.existsSync(p));
}

async function getJson(url) {
  const r = await fetch(url, { headers: { 'User-Agent': UA, Accept: 'application/json' } });
  if (r.status === 204) return null;
  if (!r.ok) throw new Error(`${url} answered ${r.status}`);
  return r.json();
}

/** Ask the site's update endpoint for the newest AppImage → {version, url, signature, file, sha256}. */
export async function newestRelease(s) {
  const offer = await getJson(`${s.base}/api/v1/updates/linux/x86_64/0.0.0?edition=appimage`);
  if (!offer || !offer.url || !offer.signature || !/^\d+\.\d+\.\d+$/.test(String(offer.version))) throw new Error('the site offers no Linux AppImage right now');
  const url = new URL(offer.url);
  if (url.origin !== new URL(s.base).origin) throw new Error(`the download is not on ${s.base}: ${offer.url}`);
  const file = decodeURIComponent(url.pathname.split('/').pop());
  const dir = url.pathname.slice(0, url.pathname.lastIndexOf('/'));
  const manifest = await getJson(`${url.origin}${dir}/manifest.json`);
  const entry = (manifest?.files || []).find((f) => f.file === file);
  if (!entry || !/^[0-9a-f]{64}$/.test(entry.sha256 || '')) throw new Error(`the release manifest does not list ${file}`);
  return { version: offer.version, url: url.href, signature: offer.signature, file, sha256: entry.sha256 };
}

/** The newest AppImage already in the cache → {version, path} or null. */
export function cached(s) {
  let best = null;
  for (const v of fs.existsSync(s.cacheRoot) ? fs.readdirSync(s.cacheRoot) : []) {
    if (!/^\d+\.\d+\.\d+$/.test(v)) continue;
    const p = path.join(s.cacheRoot, v, 'CadCraft-Studio.AppImage');
    if (!fs.existsSync(path.join(s.cacheRoot, v, 'ok')) || !fs.existsSync(p)) continue;
    if (!best || cmp(v, best.version) > 0) best = { version: v, path: p };
  }
  return best;
}

const cmp = (a, b) => { const x = a.split('.').map(Number), y = b.split('.').map(Number); for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] - y[i]; return 0; };

/** Download, check and store one release. Nothing is kept unless both checks pass. → path */
export async function fetchRelease(s, rel, say = () => {}) {
  const dir = path.join(s.cacheRoot, rel.version);
  fs.mkdirSync(dir, { recursive: true });
  const part = path.join(dir, 'download.part');
  say(`Downloading CadCraft Studio ${rel.version} for Linux…`);
  const r = await fetch(rel.url, { headers: { 'User-Agent': UA } });
  if (!r.ok || !r.body) throw new Error(`${rel.url} answered ${r.status}`);
  const total = Number(r.headers.get('content-length')) || 0;
  const out = fs.createWriteStream(part);
  let got = 0, shown = -1;
  for await (const chunk of r.body) {
    got += chunk.length;
    if (!out.write(chunk)) await new Promise((res) => out.once('drain', res));
    const pct = total ? Math.floor((got / total) * 10) : -1;
    if (pct !== shown && pct >= 0) { shown = pct; say(`  ${pct * 10}% of ${Math.round(total / 1048576)} MB`); }
  }
  await new Promise((res, rej) => out.end((e) => (e ? rej(e) : res())));
  try {
    const h = await hashFile(part);
    if (h.sha256 !== rel.sha256) throw new Error('the download does not match the SHA-256 in the release manifest');
    const v = verifySignature(h, rel.signature, s.pubkey, part);
    if (!v.ok) throw new Error(`the download's signature is not CadCraft's: ${v.why}`);
  } catch (e) {
    fs.rmSync(part, { force: true });
    throw e;
  }
  const dest = path.join(dir, 'CadCraft-Studio.AppImage');
  fs.renameSync(part, dest);
  fs.chmodSync(dest, 0o755);
  fs.writeFileSync(path.join(dir, 'ok'), JSON.stringify({ version: rel.version, sha256: rel.sha256, file: rel.file, at: new Date().toISOString() }) + '\n');
  say('Checked: SHA-256 and CadCraft signature match.');
  return dest;
}

/** Keep the newest $keep versions in the cache; remove the rest (disk space). */
export function prune(s, keep = 2) {
  if (!fs.existsSync(s.cacheRoot)) return [];
  const vs = fs.readdirSync(s.cacheRoot).filter((v) => /^\d+\.\d+\.\d+$/.test(v)).sort(cmp).reverse();
  const gone = vs.slice(keep);
  for (const v of gone) fs.rmSync(path.join(s.cacheRoot, v), { recursive: true, force: true });
  return gone;
}

/** Make sure a checked AppImage is in the cache → {version, path, fresh}. Offline (or the site down) = the cached one. */
export async function ensure(s, { offline = false, say = () => {} } = {}) {
  const have = cached(s);
  if (offline) {
    if (!have) throw new Error('--offline, and nothing is in the cache yet: run once with a connection first');
    return { ...have, fresh: false };
  }
  let rel;
  try { rel = await newestRelease(s); } catch (e) {
    if (have) { say(`Could not check for a newer version (${e.message}); starting ${have.version} from the cache.`); return { ...have, fresh: false }; }
    throw e;
  }
  if (have && cmp(have.version, rel.version) >= 0) return { ...have, fresh: false };
  const p = await fetchRelease(s, rel, say);
  prune(s);
  return { version: rel.version, path: p, fresh: true };
}

/** Start the AppImage with the person's arguments; resolves with its exit code. */
export function run(appImage, args, { env = process.env, fuse = hasFuse() } = {}) {
  const e = { ...env };
  if (!fuse) e.APPIMAGE_EXTRACT_AND_RUN = '1';   // AppImage runtime: unpack to a temp folder instead of mounting with FUSE
  return new Promise((resolve) => {
    const child = spawn(appImage, args, { stdio: 'inherit', env: e });
    child.on('error', (err) => { console.error(`cadcraft-studio: could not start Studio: ${err.message}`); resolve(1); });
    child.on('exit', (code, sig) => resolve(code ?? (sig ? 1 : 0)));
  });
}
