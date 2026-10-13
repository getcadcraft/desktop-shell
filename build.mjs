// SPDX-License-Identifier: Apache-2.0
// build.mjs — desktop installers (docs/0.4-PART2.md W.3/W.4/W.5): one shell build + one app's browser edition + a small
// per-app config. No app code lives in the shell; the app is copied in as `app/` beside the executable.
//
//   node build.mjs <slug…|all> [--build dev|x.y] [--edition <dir>] [--target windows|linux] [--allow-gate]
//   node build.mjs studio --edition <suite folder|zip>     Studio: the hub + eight apps in one install (suite mode, hub H.9;
//                                                          the suite comes from the studio repo's pack/suite.mjs)
//
//   --build      which app build to pack (default dev). The edition comes from the site's own packer:
//                sudo -u www-data php devshell/pack.php <slug> … --edition browser --build <b>
//                (needs CADCRAFT_SITE = the site's folder; with --edition the site is not needed)
//   --edition    use an edition already packed: a folder, or a release zip (one slug only) — how CI gets it
//   --target     windows (default: NSIS .exe cross-compiled with cargo-xwin) | linux (unbundled binary, for tests)
//   --allow-gate build even when the license/secrets gate fails (spikes only; the result must never ship)
//   --store      a store package (S.3): shell.json account:false (no sign-in until decision 11), never an update feed
//                (the store updates it), out/<Name>-<release>-store-setup.exe. Give it the store edition with --edition.
//
// Out: out/<Name>-<version>-setup.exe (+ <Name>-<version>.msi when built on Windows) + out/SHA256SUMS (real checksums).
// On Linux: NSIS only, cross-compiled with cargo-xwin. On Windows (CI, .github/workflows/windows.yml): NSIS + MSI (WiX).
// Signing (W.6): set CC_SIGN_COMMAND to a command Tauri runs per file, %1 = the file (e.g. Azure Trusted Signing:
//   trusted-signing-cli -e <endpoint> -a <account> -c <profile> -d CadCraft %1). Unset = unsigned (SmartScreen warns).
// Updates (W.7, off unless all are set): CC_UPDATE_PUBKEY (the owner's updater public key) turns the feed on in
// shell.json — https://github.com/getcadcraft/<slug>/releases/latest/download/latest.json, or CC_UPDATE_ENDPOINT
// ({slug} is replaced); CC_UPDATE_AUTO=1 makes automatic checks the default (decision 10). TAURI_SIGNING_PRIVATE_KEY
// (+ _PASSWORD; CI secret only) signs the installer for the updater and writes out/<slug>-latest.json.

import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawnSync } from 'node:child_process';

const HERE = path.dirname(new URL(import.meta.url).pathname);
// The site's folder, only needed to pack an edition (no --edition). No default: the site is not part of this repo.
const site = () => {
  const s = process.env.CADCRAFT_SITE;
  if (!s) throw new Error('CADCRAFT_SITE is not set: point it at the site\'s folder to pack an edition, or pass --edition <dir|zip>');
  return s;
};
const ALL = ['cadcraft', 'bimcraft', 'modelcraft', 'markupcraft', 'takeoffcraft', 'civilcraft', 'detailcraft', 'partcraft'];

const argv = process.argv.slice(2);
const opt = (k, d) => { const i = argv.indexOf(k); return i >= 0 ? argv.splice(i, 2)[1] : d; };
const flag = (k) => { const i = argv.indexOf(k); return i >= 0 ? (argv.splice(i, 1), true) : false; };
const BUILD = opt('--build', 'dev');
const EDITION = opt('--edition', null);
const TARGET = opt('--target', 'windows');
const ALLOW_GATE = flag('--allow-gate');
const STORE = flag('--store');
const slugs = argv.includes('all') ? ALL : argv;
if (slugs.includes('studio') && (slugs.length !== 1 || !EDITION)) { console.error('studio builds alone, from --edition <suite>'); process.exit(1); }
if (!slugs.length || slugs.some((s) => !ALL.includes(s) && s !== 'studio')) {
  console.error('usage: node build.mjs <slug…|all> [--build dev|x.y] [--edition <dir>] [--target windows|linux] [--allow-gate]');
  process.exit(1);
}
if (EDITION && slugs.length !== 1) { console.error('--edition takes one slug'); process.exit(1); }
if (!EDITION) try { site(); } catch (e) { console.error(e.message); process.exit(1); }

const APPS = JSON.parse(fs.readFileSync(path.join(HERE, 'apps.json'), 'utf8'));
// Studio registers every app's types once: own ones as the default, polite ones when free, the rest (.pdf too) Open with.
{
  const own = new Set(), polite = new Set(), all = new Set();
  for (const a of ALL) {
    const f = APPS[a].files;
    f.own.forEach((x) => own.add(x)); f.polite.forEach((x) => polite.add(x));
    [...f.own, ...f.polite, ...f.openWith].forEach((x) => all.add(x));
  }
  for (const x of own) polite.delete(x);
  APPS.studio = { files: { own: [...own], polite: [...polite], openWith: [...all].filter((x) => !own.has(x) && !polite.has(x)) } };
}
const SHELL_VERSION = JSON.parse(fs.readFileSync(path.join(HERE, 'src-tauri/tauri.conf.json'), 'utf8')).version;
const run = (cmd, args, o = {}) => {
  const r = spawnSync(cmd, args, { stdio: 'inherit', ...o });
  if (r.status !== 0 && !o.allowFail) throw new Error(`${cmd} ${args.join(' ')} → exit ${r.status}`);
  return r.status;
};
const cargoEnv = { ...process.env, PATH: `${process.env.HOME}/.cargo/bin:${process.env.PATH}`, XWIN_ACCEPT_LICENSE: '1' };

/** The browser edition for one app: packed by the site's own packer (and its license/secrets gate). */
function edition(slug) {
  if (EDITION && /\.zip$/i.test(EDITION)) {
    const dir = path.join(HERE, 'stage', '_zip', slug);
    fs.rmSync(dir, { recursive: true, force: true });
    fs.mkdirSync(dir, { recursive: true });
    if (process.platform === 'win32') run('tar', ['-xf', path.resolve(EDITION), '-C', dir]);
    else run('unzip', ['-q', path.resolve(EDITION), '-d', dir]);
    const find = (d) => fs.existsSync(path.join(d, 'index.html')) && fs.existsSync(path.join(d, 'release.json')) ? d
      : fs.readdirSync(d, { withFileTypes: true }).filter((e) => e.isDirectory()).map((e) => find(path.join(d, e.name))).find(Boolean);
    const root = find(dir);
    if (!root) throw new Error(`${slug}: no edition (index.html + release.json) in ${EDITION}`);
    return root;
  }
  if (EDITION) return path.resolve(EDITION);
  const SITE = site();
  const out = path.join(SITE, 'tmp/desktop-editions', slug);
  const status = run('sudo', ['-u', 'www-data', 'php', path.join(SITE, 'devshell/pack.php'), slug, out, '--edition', 'browser', '--build', BUILD, '--force'], { allowFail: true });
  if (status === 2 && !ALLOW_GATE) throw new Error(`${slug}: the license/secrets gate failed — not building an installer from it`);
  if (status !== 0 && status !== 2) throw new Error(`${slug}: pack failed (${status})`);
  const dir = path.join(out, 'browser-edition', `v${BUILD}`);
  if (!fs.existsSync(path.join(dir, 'index.html'))) throw new Error(`${slug}: no edition at ${dir}`);
  return dir;
}

/** Version for the installer: a release "0.4" → 0.4.0; dev builds carry the shell's version and say "dev" in the file name. */
const semver = (release) => (/^\d+\.\d+(\.\d+)?$/.test(release) ? (release.split('.').length === 2 ? `${release}.0` : release) : SHELL_VERSION);

/** NSIS hook: polite file associations (W.4). Every type → "Open with"; `own` types → our default; `polite` types → our
 *  default only when nothing else claimed them. Uninstall takes back only what is still ours. */
function hooks(slug, name, exe) {
  const f = APPS[slug].files;
  const types = APPS._types;
  const all = [...f.own, ...f.polite, ...f.openWith];
  const cls = (ext) => `CadCraft.${slug}.${ext}`;
  const L = [];
  L.push('; Generated by desktop-shell/build.mjs — file types for ' + name + ' (docs/0.4-PART2.md W.4).');
  L.push('!macro NSIS_HOOK_POSTINSTALL');
  L.push(`  WriteRegStr SHCTX "Software\\Classes\\Applications\\${exe}.exe" "FriendlyAppName" "${name}"`);
  L.push(`  WriteRegStr SHCTX "Software\\Classes\\Applications\\${exe}.exe\\shell\\open\\command" "" '"$INSTDIR\\${exe}.exe" "%1"'`);
  for (const ext of all) {
    L.push(`  WriteRegStr SHCTX "Software\\Classes\\${cls(ext)}" "" "${types[ext] || ext.toUpperCase() + ' file'}"`);
    L.push(`  WriteRegStr SHCTX "Software\\Classes\\${cls(ext)}\\DefaultIcon" "" '"$INSTDIR\\${exe}.exe",0'`);
    L.push(`  WriteRegStr SHCTX "Software\\Classes\\${cls(ext)}\\shell\\open" "" "Open with ${name}"`);
    L.push(`  WriteRegStr SHCTX "Software\\Classes\\${cls(ext)}\\shell\\open\\command" "" '"$INSTDIR\\${exe}.exe" "%1"'`);
    L.push(`  WriteRegStr SHCTX "Software\\Classes\\.${ext}\\OpenWithProgids" "${cls(ext)}" ""`);
    L.push(`  WriteRegStr SHCTX "Software\\Classes\\Applications\\${exe}.exe\\SupportedTypes" ".${ext}" ""`);
  }
  for (const ext of f.own) L.push(`  WriteRegStr SHCTX "Software\\Classes\\.${ext}" "" "${cls(ext)}"`);
  for (const ext of f.polite) {
    L.push(`  ReadRegStr $R0 HKCR ".${ext}" ""`);
    L.push(`  StrCmp $R0 "" 0 +2`);
    L.push(`    WriteRegStr SHCTX "Software\\Classes\\.${ext}" "" "${cls(ext)}"`);
  }
  L.push(`  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'`);
  L.push('!macroend');
  L.push('!macro NSIS_HOOK_PREUNINSTALL');
  for (const ext of all) {
    L.push(`  ReadRegStr $R0 SHCTX "Software\\Classes\\.${ext}" ""`);
    L.push(`  StrCmp $R0 "${cls(ext)}" 0 +2`);
    L.push(`    DeleteRegValue SHCTX "Software\\Classes\\.${ext}" ""`);
    L.push(`  DeleteRegValue SHCTX "Software\\Classes\\.${ext}\\OpenWithProgids" "${cls(ext)}"`);
    L.push(`  DeleteRegKey /ifempty SHCTX "Software\\Classes\\.${ext}\\OpenWithProgids"`);
    L.push(`  DeleteRegKey /ifempty SHCTX "Software\\Classes\\.${ext}"`);
    L.push(`  DeleteRegKey SHCTX "Software\\Classes\\${cls(ext)}"`);
  }
  L.push(`  DeleteRegKey SHCTX "Software\\Classes\\Applications\\${exe}.exe"`);
  L.push(`  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'`);
  L.push('!macroend');
  return L.join('\r\n') + '\r\n';
}

// The shell's own license gate (its Rust crates) before anything is built; writes THIRD_PARTY.md for the install.
const triple = TARGET === 'windows' ? 'x86_64-pc-windows-msvc' : 'x86_64-unknown-linux-gnu';
run('node', [path.join(HERE, 'licenses.mjs'), '--target', triple, '--out', path.join(HERE, 'THIRD_PARTY.md')]);

const sha256 = (file) => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
fs.mkdirSync(path.join(HERE, 'out'), { recursive: true });

const failed = [];
for (const slug of slugs) {
  try {
  const ed = edition(slug);
  const rel = JSON.parse(fs.readFileSync(path.join(ed, 'release.json'), 'utf8'));
  const name = rel.name;
  const release = String(rel.release);
  const version = semver(release);
  const exe = name; // CadCraft.exe, BimCraft.exe, …
  console.log(`== ${slug}: ${name} ${release} (installer version ${version})`);

  // Stage: app/ + shell.json + icons + hooks + the Tauri config merged over src-tauri/tauri.conf.json.
  const stage = path.join(HERE, 'stage', slug);
  fs.rmSync(stage, { recursive: true, force: true });
  fs.mkdirSync(stage, { recursive: true });
  fs.cpSync(ed, path.join(stage, 'app'), { recursive: true, dereference: true });
  // Web-server files mean nothing inside the desktop app (the service worker is refused on the shell's origin anyway).
  for (const f of ['.htaccess', 'web.config', 'INSTALL.md', 'sw.js']) fs.rmSync(path.join(stage, 'app', f), { force: true });
  const suite = rel.format === 'cadcraft-suite';
  if (suite !== (slug === 'studio')) throw new Error(`${slug}: ${suite ? 'a suite folder needs the slug studio' : 'studio needs a suite folder'}`);
  // Headers for the whole origin: one app's manifest, or (Studio) any of the suite's apps that wants isolation.
  const manifests = suite
    ? fs.readdirSync(path.join(stage, 'app/apps')).flatMap((a) => {
      const d = path.join(stage, 'app/apps', a, 'assets/apps', a);
      return fs.existsSync(d) ? fs.readdirSync(d).map((v) => path.join(d, v, 'manifest.json')).filter((f) => fs.existsSync(f)) : [];
    })
    : [path.join(stage, 'app/assets/apps', slug, release, 'manifest.json')];
  const isolate = manifests.map((f) => JSON.parse(fs.readFileSync(f, 'utf8')).isolate).find(Boolean);
  const headers = isolate ? { 'Cross-Origin-Opener-Policy': 'same-origin', 'Cross-Origin-Embedder-Policy': isolate } : {};
  const updates = !STORE && process.env.CC_UPDATE_PUBKEY ? {
    endpoint: (process.env.CC_UPDATE_ENDPOINT || 'https://github.com/getcadcraft/{slug}/releases/latest/download/latest.json').replaceAll('{slug}', slug),
    pubkey: process.env.CC_UPDATE_PUBKEY,
    auto: process.env.CC_UPDATE_AUTO === '1',
  } : undefined;
  const account = STORE ? false : undefined;
  fs.writeFileSync(path.join(stage, 'shell.json'), JSON.stringify({ app: slug, name, version: release, headers, updates, account, suite: suite || undefined }, null, 2));
  run('python3', [path.join(HERE, 'make-icon.py'), slug, path.join(stage, 'icon.png')]);
  run('cargo', ['tauri', 'icon', path.join(stage, 'icon.png'), '-o', path.join(stage, 'icons')], { cwd: path.join(HERE, 'src-tauri'), env: cargoEnv, stdio: 'ignore' });
  fs.writeFileSync(path.join(stage, 'hooks.nsh'), hooks(slug, name, exe));
  const rel2 = (p) => path.relative(path.join(HERE, 'src-tauri'), p);
  const config = {
    productName: name,
    mainBinaryName: exe,
    version,
    identifier: `com.getcadcraft.${slug}`,
    bundle: {
      ...(process.env.TAURI_SIGNING_PRIVATE_KEY ? { createUpdaterArtifacts: true } : {}),
      shortDescription: name,
      icon: ['32x32.png', '128x128.png', 'icon.ico'].map((f) => rel2(path.join(stage, 'icons', f))),
      resources: {
        [rel2(path.join(stage, 'app')) + '/']: 'app/',
        [rel2(path.join(stage, 'shell.json'))]: 'shell.json',
        [rel2(path.join(HERE, 'THIRD_PARTY.md'))]: 'THIRD_PARTY.md',
        [rel2(path.join(HERE, 'LICENSE'))]: 'LICENSE',
      },
      windows: {
        nsis: { installMode: 'currentUser', installerHooks: rel2(path.join(stage, 'hooks.nsh')) },
        ...(process.env.CC_SIGN_COMMAND ? { signCommand: process.env.CC_SIGN_COMMAND } : {}),
      },
    },
  };
  // Signing for the updater needs the public key in the bundler's config too (the shell itself reads shell.json).
  if (process.env.TAURI_SIGNING_PRIVATE_KEY && !STORE) {
    if (!updates) throw new Error('TAURI_SIGNING_PRIVATE_KEY without CC_UPDATE_PUBKEY: set both, or neither');
    config.plugins = { updater: { pubkey: updates.pubkey, endpoints: [updates.endpoint] } };
  }
  const cfgFile = path.join(stage, 'tauri.override.json');
  fs.writeFileSync(cfgFile, JSON.stringify(config, null, 2));

  const tauriDir = path.join(HERE, 'src-tauri');
  if (TARGET === 'windows') {
    const native = process.platform === 'win32';
    // MSI (WiX) needs a numeric version: only releases get one.
    const bundles = native && /^\d+\.\d+(\.\d+)?$/.test(release) ? 'nsis,msi' : 'nsis';
    const args = native ? ['tauri', 'build', '--bundles', bundles, '--config', cfgFile]
      : ['tauri', 'build', '--runner', 'cargo-xwin', '--target', 'x86_64-pc-windows-msvc', '--bundles', bundles, '--config', cfgFile];
    const base0 = path.join(tauriDir, native ? 'target/release/bundle' : 'target/x86_64-pc-windows-msvc/release/bundle');
    for (const sub of ['nsis', 'msi']) { // signatures from an earlier signed build must never be mistaken for this one's
      const d = path.join(base0, sub);
      if (fs.existsSync(d)) for (const f of fs.readdirSync(d)) if (f.endsWith('.sig')) fs.rmSync(path.join(d, f));
    }
    run('cargo', args, { cwd: tauriDir, env: cargoEnv });
    const base = path.join(tauriDir, native ? 'target/release/bundle' : 'target/x86_64-pc-windows-msvc/release/bundle');
    const pick = (sub, ext) => {
      const dir = path.join(base, sub);
      return fs.existsSync(dir) ? fs.readdirSync(dir).filter((f) => f.endsWith(ext) && f.startsWith(name + '_' + version)).map((f) => path.join(dir, f))[0] : null;
    };
    const exeOut = pick('nsis', '.exe');
    if (!exeOut) throw new Error(`${slug}: no NSIS installer under ${base}`);
    const tagOut = STORE ? '-store' : '';
    const outs = [[exeOut, `${name}-${release}${tagOut}-setup.exe`]];
    if (bundles.includes('msi')) {
      const msi = pick('msi', '.msi');
      if (!msi) throw new Error(`${slug}: no MSI under ${base}`);
      outs.push([msi, `${name}-${release}${tagOut}.msi`]);
    }
    for (const [from, to] of outs) {
      const dest = path.join(HERE, 'out', to);
      fs.copyFileSync(from, dest);
      console.log(`   → ${path.relative(HERE, dest)} (${(fs.statSync(dest).size / 1048576).toFixed(1)} MB)`);
    }
    // The updater feed entry (W.7): only with the signing key, for the NSIS installer as published on the release.
    if (!STORE && process.env.TAURI_SIGNING_PRIVATE_KEY && fs.existsSync(exeOut + '.sig')) {
      const tag = `${slug}-v${version}`;
      const feed = {
        version, notes: `${name} ${release}`, pub_date: new Date().toISOString(),
        platforms: { 'windows-x86_64': {
          signature: fs.readFileSync(exeOut + '.sig', 'utf8').trim(),
          url: `https://github.com/getcadcraft/${slug}/releases/download/${tag}/${encodeURIComponent(`${name}-${release}-setup.exe`)}`,
        } },
      };
      fs.writeFileSync(path.join(HERE, 'out', `${slug}-latest.json`), JSON.stringify(feed, null, 2));
      console.log(`   → out/${slug}-latest.json (upload as latest.json on the ${tag} release)`);
    }
  } else {
    run('cargo', ['tauri', 'build', '--no-bundle', '--config', cfgFile], { cwd: tauriDir, env: cargoEnv });
  }
  } catch (e) {
    console.error(`!! ${slug}: ${e.message}`);
    failed.push(slug);
  }
}

const sums = fs.readdirSync(path.join(HERE, 'out')).filter((f) => /\.(exe|msi)$/.test(f)).sort()
  .map((f) => `${sha256(path.join(HERE, 'out', f))}  ${f}`);
fs.writeFileSync(path.join(HERE, 'out/SHA256SUMS'), sums.join('\n') + '\n');
console.log(sums.join('\n'));
if (failed.length) { console.error(`not built: ${failed.join(', ')}`); process.exit(1); }
