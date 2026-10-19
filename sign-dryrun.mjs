// SPDX-License-Identifier: Apache-2.0
// sign-dryrun.mjs — prove each platform's build signs the moment its key is there (0.6 SIGN), without the real keys where
// they don't exist yet, and check what a signed file carries.
//
//   node sign-dryrun.mjs windows --edition <suite folder>   a real Studio build (installer version 0.0.99, no WebView2 inside)
//                                                           with CC_SIGN_COMMAND = sign-azure.sh in its dry mode: a throwaway
//                                                           self-made certificate through the same jsign call. Checks Tauri
//                                                           called the signer for the app, the uninstaller and the installer,
//                                                           and that the installer carries the signature. The build moves to
//                                                           stage/sign-dryrun/ (never out/); out/SHA256SUMS is left as it was.
//   node sign-dryrun.mjs android                            the release key (secrets/android, never printed or copied):
//                                                           signs a COPY of the last unsigned AAB with jarsigner, verifies it,
//                                                           and compares the certificate with the published APK's
//   node sign-dryrun.mjs linux                              a throwaway OpenPGP key signs a copy of a release folder's
//                                                           SHA256SUMS through stores/linux/sign-sums.sh, then verifies it
//   node sign-dryrun.mjs verify <file.exe|.msi>             who signed a Windows file (subject, issuer, digest), from its
//                                                           Authenticode table — for the real signed build too
//
// macOS and iOS sign on the Mac (Xcode / Tauri's APPLE_* variables); their dry run is build.mjs there: see SIGNING.md.
// Everything temporary goes under stage/sign-dryrun/ (git-ignored) and the throwaway keys are deleted at the end.

import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawnSync } from 'node:child_process';

const HERE = path.dirname(new URL(import.meta.url).pathname);
const CC_HOME = process.env.CC_HOME || path.resolve(HERE, '..');
const WORK = path.join(HERE, 'stage', 'sign-dryrun');
const argv = process.argv.slice(2);
const opt = (k, d) => { const i = argv.indexOf(k); return i >= 0 ? argv.splice(i, 2)[1] : d; };
const EDITION = opt('--edition', null);
const [what, arg] = argv;

let pass = 0, fail = 0;
const ok = (c, msg, extra = '') => { if (c) { pass++; console.log(`  ✓ ${msg}`); } else { fail++; console.log(`  ✗ ${msg} ${extra}`); } };
const run = (cmd, args, o = {}) => {
  const r = spawnSync(cmd, args, { encoding: 'utf8', maxBuffer: 1 << 26, ...o });
  if (r.status !== 0 && !o.allowFail) throw new Error(`${cmd} ${args[0] || ''} → exit ${r.status}: ${(r.stderr || '').slice(-400)}`);
  return r;
};
const sha256 = (f) => crypto.createHash('sha256').update(fs.readFileSync(f)).digest('hex');
/** Variables from an env file, without printing them. */
const envFile = (f) => Object.fromEntries(fs.readFileSync(f, 'utf8').split('\n').filter((l) => /^[A-Z_][A-Z0-9_]*=/.test(l))
  .map((l) => { const i = l.indexOf('='); return [l.slice(0, i), l.slice(i + 1).replace(/^["']|["']$/g, '')]; }));

/** The Authenticode signature of a PE file (security data directory → WIN_CERTIFICATE → PKCS#7), or null. */
export function authenticode(file) {
  const b = fs.readFileSync(file);
  if (b.readUInt16LE(0) !== 0x5a4d) return null;
  const pe = b.readUInt32LE(0x3c);
  if (b.readUInt32LE(pe) !== 0x00004550) return null;
  const opt = pe + 24, magic = b.readUInt16LE(opt);
  const dirs = opt + (magic === 0x20b ? 112 : 96);
  const off = b.readUInt32LE(dirs + 4 * 8), size = b.readUInt32LE(dirs + 4 * 8 + 4);
  if (!off || !size || off + size > b.length) return null;
  const len = b.readUInt32LE(off), type = b.readUInt16LE(off + 6);
  if (type !== 0x0002) return { type };
  const der = b.subarray(off + 8, off + len);
  fs.mkdirSync(WORK, { recursive: true });
  const tmp = path.join(WORK, `p7-${process.pid}.der`);
  fs.writeFileSync(tmp, der);
  try {
    const certs = run('openssl', ['pkcs7', '-inform', 'DER', '-in', tmp, '-print_certs', '-noout'], { allowFail: true }).stdout;
    const subject = certs.match(/subject=(.*)/)?.[1]?.trim() || '';
    const issuer = certs.match(/issuer=(.*)/)?.[1]?.trim() || '';
    const asn = run('openssl', ['asn1parse', '-inform', 'DER', '-in', tmp], { allowFail: true }).stdout;
    const digest = asn.match(/:(sha256|sha384|sha512|sha1)\s*$/m)?.[1] || '';
    const timestamped = /1\.2\.840\.113549\.1\.9\.16\.2\.14|signingTime|countersignature|1\.3\.6\.1\.4\.1\.311\.3\.3\.1/.test(asn);
    return { type, bytes: der.length, subject, issuer, digest, timestamped };
  } finally { fs.rmSync(tmp, { force: true }); }
}

function windows() {
  if (!EDITION) throw new Error('windows: --edition <suite folder> (e.g. ../studio/out/studio-suite/v0.5)');
  const jar = process.env.JSIGN_JAR || '/opt/jsign/current/jsign-7.5.jar';
  if (!fs.existsSync(jar)) throw new Error(`no jsign at ${jar} (JSIGN_JAR)`);
  fs.rmSync(WORK, { recursive: true, force: true });
  fs.mkdirSync(WORK, { recursive: true });
  // A throwaway code-signing certificate, made here and deleted at the end.
  const key = path.join(WORK, 'dry.key'), crt = path.join(WORK, 'dry.crt'), p12 = path.join(WORK, 'dry.p12'), log = path.join(WORK, 'signed.log');
  run('openssl', ['req', '-x509', '-newkey', 'rsa:3072', '-nodes', '-keyout', key, '-out', crt, '-days', '2', '-subj', '/CN=CadCraft dry run - not trusted/O=CadCraft test',
    '-addext', 'extendedKeyUsage=codeSigning', '-addext', 'keyUsage=digitalSignature']);
  run('openssl', ['pkcs12', '-export', '-inkey', key, '-in', crt, '-out', p12, '-name', 'dry-run', '-passout', 'pass:dry-run']);
  const sumsFile = path.join(HERE, 'out/SHA256SUMS');
  const sumsBefore = fs.existsSync(sumsFile) ? fs.readFileSync(sumsFile) : null;
  const VERSION = '0.0.99';
  console.log('== Windows: Studio built with the signer in dry mode (several minutes)');
  const env = { ...process.env, CC_SIGN_COMMAND: `${path.join(HERE, 'sign-azure.sh')} %1`, SIGN_DRY_KEYSTORE: p12, SIGN_DRY_PASSWORD: 'dry-run', SIGN_DRY_LOG: log, JSIGN_JAR: jar };
  for (const k of ['AZURE_CLIENT_SECRET', 'TAURI_SIGNING_PRIVATE_KEY']) delete env[k];   // a dry run never reaches Azure or the updater key
  const logFile = path.join(WORK, 'build.log');
  const r = spawnSync('node', [path.join(HERE, 'build.mjs'), 'studio', '--edition', EDITION, '--target', 'windows', '--version', VERSION],
    { cwd: HERE, env, stdio: ['ignore', fs.openSync(logFile, 'w'), fs.openSync(logFile, 'a')] });
  const name = `CadCraft Studio-${VERSION}-setup.exe`;
  const built = path.join(HERE, 'out', name), kept = path.join(WORK, name);
  if (fs.existsSync(built)) fs.renameSync(built, kept);
  if (sumsBefore) fs.writeFileSync(sumsFile, sumsBefore); else fs.rmSync(sumsFile, { force: true });
  ok(r.status === 0, `build finished (log ${path.relative(HERE, logFile)})`, `exit ${r.status}`);
  const signedList = fs.existsSync(log) ? fs.readFileSync(log, 'utf8').trim().split('\n').filter(Boolean) : [];
  const base = signedList.map((f) => path.basename(f));
  ok(base.some((f) => /^CadCraft Studio\.exe$/i.test(f)), 'the signer was called for the app itself (CadCraft Studio.exe)', JSON.stringify(base));
  // NSIS signs the uninstaller while it is still a temporary file (!uninstfinalize: makensisXXXXXX), before packing it.
  ok(base.some((f) => /uninstall/i.test(f) || /^makensis\w+$/.test(f)), 'the signer was called for the uninstaller (NSIS\'s temporary makensis… file)', JSON.stringify(base));
  ok(base.some((f) => /\.dll$/i.test(f)), 'and for the NSIS plugin DLLs inside the installer', JSON.stringify(base));
  ok(base.some((f) => /setup\.exe$/i.test(f)), 'the signer was called for the installer', JSON.stringify(base));
  if (fs.existsSync(kept)) {
    const sig = authenticode(kept);
    ok(sig && sig.type === 2 && /CadCraft dry run/.test(sig.subject), `the installer carries an Authenticode signature (${sig?.subject || 'none'}, ${sig?.digest || '?'})`);
    console.log(`  → ${path.relative(HERE, kept)} (${(fs.statSync(kept).size / 1048576).toFixed(1)} MB; a dry-run build: never publish it)`);
  } else ok(false, `the installer was built (${name})`);
  ok(!fs.existsSync(built) && (sumsBefore === null || fs.readFileSync(sumsFile).equals(sumsBefore)), 'out/ left as it was (no dry-run file, SHA256SUMS unchanged)');
  for (const f of [key, crt, p12]) fs.rmSync(f, { force: true });
}

function android() {
  const sec = path.join(CC_HOME, 'secrets/android/credentials.env');
  if (!fs.existsSync(sec)) throw new Error(`no ${sec}`);
  const k = envFile(sec);
  for (const v of ['CC_ANDROID_KEYSTORE', 'CC_ANDROID_KEYSTORE_PASSWORD', 'CC_ANDROID_KEY_ALIAS', 'CC_ANDROID_KEY_PASSWORD']) if (!k[v]) throw new Error(`${sec} lacks ${v}`);
  const aab = fs.readdirSync(path.join(HERE, 'out')).filter((f) => /-unsigned\.aab$/.test(f)).map((f) => path.join(HERE, 'out', f))
    .sort((a, b) => fs.statSync(b).mtimeMs - fs.statSync(a).mtimeMs)[0];
  if (!aab) throw new Error('no out/*-unsigned.aab to try the key on');
  fs.mkdirSync(WORK, { recursive: true });
  const copy = path.join(WORK, 'dry-run.aab');
  fs.copyFileSync(aab, copy);
  console.log(`== Android: the release key on a copy of ${path.basename(aab)}`);
  // Passwords through the environment (jarsigner :env), never on a command line.
  const env = { ...process.env, CC_KS_PASS: k.CC_ANDROID_KEYSTORE_PASSWORD, CC_KEY_PASS: k.CC_ANDROID_KEY_PASSWORD };
  run('jarsigner', ['-storepass:env', 'CC_KS_PASS', '-keypass:env', 'CC_KEY_PASS', '-sigalg', 'SHA256withRSA', '-digestalg', 'SHA-256',
    '-keystore', path.resolve(path.dirname(sec), path.basename(k.CC_ANDROID_KEYSTORE)), copy, k.CC_ANDROID_KEY_ALIAS], { env });
  const v = run('jarsigner', ['-verify', '-certs', copy], { allowFail: true });
  ok(v.status === 0 && /jar verified/.test(v.stdout), 'the signed AAB verifies (jarsigner)');
  const fp = (out) => out.match(/SHA-?256[^:]*:\s*([0-9A-F:]{95})/i)?.[1]?.replace(/:/g, '').toLowerCase();
  const ks = run('keytool', ['-list', '-v', '-storepass:env', 'CC_KS_PASS', '-keystore', path.resolve(path.dirname(sec), path.basename(k.CC_ANDROID_KEYSTORE)), '-alias', k.CC_ANDROID_KEY_ALIAS], { env });
  const keyFp = fp(ks.stdout);
  const apk = fs.readdirSync(path.join(HERE, 'out')).find((f) => /^CadCraft-Studio-\d[\d.]*\.apk$/.test(f));
  const tools = '/opt/android-sdk/build-tools';
  const apksigner = fs.existsSync(tools) ? path.join(tools, fs.readdirSync(tools).sort().pop(), 'apksigner') : 'apksigner';
  if (apk) {
    const a = run(apksigner, ['verify', '--print-certs', path.join(HERE, 'out', apk)], { allowFail: true });
    const apkFp = a.stdout.match(/certificate SHA-256 digest:\s*([0-9a-f]{64})/)?.[1];
    ok(keyFp && apkFp === keyFp, `the published APK (${apk}) is signed by the same key (certificate SHA-256 ${keyFp?.slice(0, 8)}…${keyFp?.slice(-4)})`);
  }
  fs.rmSync(copy, { force: true });
}

function linux() {
  const rel = arg || path.join(CC_HOME, 'stores/out/linux');
  if (!fs.existsSync(path.join(rel, 'SHA256SUMS'))) throw new Error(`no SHA256SUMS in ${rel}`);
  fs.rmSync(WORK, { recursive: true, force: true });
  const home = path.join(WORK, 'gnupg'), dir = path.join(WORK, 'linux');
  fs.mkdirSync(home, { recursive: true, mode: 0o700 });
  fs.mkdirSync(dir, { recursive: true });
  // A copy of the folder's sums and the files they name (hard links: no extra space, originals untouched).
  const lines = fs.readFileSync(path.join(rel, 'SHA256SUMS'), 'utf8').split('\n').filter(Boolean);
  const keep = lines.filter((l) => fs.existsSync(path.join(rel, l.slice(66))));
  for (const l of keep) fs.linkSync(path.join(rel, l.slice(66)), path.join(dir, l.slice(66)));
  fs.writeFileSync(path.join(dir, 'SHA256SUMS'), keep.join('\n') + '\n');
  console.log(`== Linux: a throwaway OpenPGP key signs the sums of ${keep.length} files from ${rel}`);
  const env = { ...process.env, GNUPGHOME: home };
  run('gpg', ['--batch', '--pinentry-mode', 'loopback', '--passphrase', '', '--quick-gen-key', 'CadCraft dry run <dry-run@getcadcraft.invalid>', 'ed25519', 'sign', '1d'], { env });
  const fpr = run('gpg', ['--batch', '--with-colons', '--list-secret-keys'], { env }).stdout.match(/^fpr:+([0-9A-F]{40}):/m)[1];
  const s = run(path.join(CC_HOME, 'stores/linux/sign-sums.sh'), [dir], { env: { ...env, CC_GPG_KEY: fpr }, allowFail: true });
  ok(s.status === 0 && fs.existsSync(path.join(dir, 'SHA256SUMS.asc')), 'sign-sums.sh wrote SHA256SUMS.asc', s.stderr.slice(-300));
  const v = run('gpg', ['--batch', '--verify', path.join(dir, 'SHA256SUMS.asc'), path.join(dir, 'SHA256SUMS')], { env, allowFail: true });
  ok(v.status === 0 && /Good signature/.test(v.stderr), 'gpg --verify: Good signature');
  const c = run('sha256sum', ['--quiet', '-c', 'SHA256SUMS'], { cwd: dir, allowFail: true });
  ok(c.status === 0, 'sha256sum -c: every file matches');
  fs.appendFileSync(path.join(dir, 'SHA256SUMS'), '0'.repeat(64) + '  tampered.bin\n');
  const t = run('gpg', ['--batch', '--verify', path.join(dir, 'SHA256SUMS.asc'), path.join(dir, 'SHA256SUMS')], { env, allowFail: true });
  ok(t.status !== 0, 'a changed SHA256SUMS fails the signature');
  fs.writeFileSync(path.join(dir, 'tampered.bin'), 'x');
  const u = run(path.join(CC_HOME, 'stores/linux/sign-sums.sh'), [dir], { env: { ...env, CC_GPG_KEY: fpr }, allowFail: true });
  ok(u.status === 3, 'sign-sums.sh refuses sums that do not match the files');
  fs.rmSync(WORK, { recursive: true, force: true });
}

try {
  if (what === 'windows') windows();
  else if (what === 'android') android();
  else if (what === 'linux') linux();
  else if (what === 'verify' && arg) {
    const s = authenticode(arg);
    if (!s) { console.log(`${arg}: not signed`); process.exit(1); }
    console.log(JSON.stringify(s, null, 2));
    process.exit(0);
  } else { console.error('usage: node sign-dryrun.mjs windows --edition <suite> | android | linux [folder] | verify <file>'); process.exit(2); }
} catch (e) { fail++; console.error(`!! ${e.message}`); }
console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
