// SPDX-License-Identifier: Apache-2.0
// The portable Windows zip (0.7 D7.53): the folder that is zipped, and the check that it is whole. Nothing here knows
// Windows: build.mjs hands in the built program, the staged app and the extracted fixed WebView2 runtime.
//
//   <Name>-<version>-portable/
//     <Name>.exe          the shell
//     app/                the suite (hub + eight apps), exactly what the installer carries
//     shell.json          the installer's, with "portable": true; its update feed (if the build has one) is edition portable:
//                         Studio only TELLS about a newer zip, it never replaces itself (src-tauri/src/updates.rs Notify)
//     webview2/           Microsoft's fixed runtime, unchanged; WebView2-NOTICE.txt beside it
//     THIRD_PARTY.md, LICENSE
//     README.txt          what portable means, where the data goes, how to update (by replacing the folder)
//     data/               made on first run: grants, window places, the web view's profile
import fs from 'node:fs';
import path from 'node:path';

export function readme({ name, version, tells = false }) {
  return [
    `${name} ${version}, portable`,
    '',
    `Unpack this folder anywhere and run "${name}.exe". Nothing is installed and nothing is written to Windows' settings.`,
    '',
    'Where your things are',
    '  data\\   beside the program: your settings, recent files, recovery copies and window places.',
    '           Copy the whole folder (data included) to take them with you; delete the folder to remove everything.',
    '',
    'Updates',
    ...(tells ? [
      '  This copy never replaces itself. Studio › Settings › Updates can check getcadcraft.com and tell you when a newer',
      '  portable zip is out (automatic checks only if you switch them on). To update, download the newest portable zip,',
      '  unpack it, and move your data\\ folder into the new one.',
    ] : [
      '  This copy never updates itself and never looks for an update. To update, download the newest portable zip,',
      '  unpack it, and move your data\\ folder into the new one.',
    ]),
    '',
    'Signing in',
    '  If you sign in to your CadCraft account, the sign-in is kept in this PC\'s Windows credential store, not in data\\.',
    '  Use Sign out before you leave a computer that is not yours.',
    '',
    'What is inside',
    '  webview2\\  Microsoft\'s Edge WebView2 runtime, unchanged (see WebView2-NOTICE.txt). It does not update itself.',
    '  app\\       the CadCraft apps. THIRD_PARTY.md and LICENSE list what is used and under which terms.',
    '',
  ].join('\r\n');
}

/** Writes the folder; returns its path. `from`: {exePath, stage, wv2, shellDir}. */
export function buildFolder({ out, name, version, exe, from, updates }) {
  const dir = path.join(out, 'portable', `${name}-${version}-portable`);
  fs.rmSync(dir, { recursive: true, force: true });
  fs.mkdirSync(dir, { recursive: true });
  fs.copyFileSync(from.exePath, path.join(dir, `${exe}.exe`));
  fs.cpSync(path.join(from.stage, 'app'), path.join(dir, 'app'), { recursive: true });
  const shell = JSON.parse(fs.readFileSync(path.join(from.stage, 'shell.json'), 'utf8'));
  delete shell.updates;
  delete shell.store;
  // The feed, as edition portable (the shell only tells; the endpoint answers for this edition).
  if (updates && updates.endpoint) shell.updates = { ...updates, edition: 'portable' };
  fs.writeFileSync(path.join(dir, 'shell.json'), JSON.stringify({ ...shell, portable: true }, null, 2));
  fs.cpSync(from.wv2, path.join(dir, 'webview2'), { recursive: true });
  // The notice is the installer's; in the zip "installer" would be wrong ("a newer … brings a newer runtime").
  fs.writeFileSync(path.join(dir, 'WebView2-NOTICE.txt'), fs.readFileSync(path.join(from.stage, 'WebView2-NOTICE.txt'), 'utf8').replace(/ installer brings/, ' download brings'));
  for (const f of ['THIRD_PARTY.md', 'LICENSE']) fs.copyFileSync(path.join(from.shellDir, f), path.join(dir, f));
  fs.writeFileSync(path.join(dir, 'README.txt'), readme({ name, version, tells: !!shell.updates }));
  return dir;
}

/** What is wrong with the folder (empty = whole). */
export function checkFolder(dir, { exe }) {
  const bad = [];
  const has = (p) => fs.existsSync(path.join(dir, p));
  for (const f of [`${exe}.exe`, 'app/index.html', 'shell.json', 'webview2/msedgewebview2.exe', 'WebView2-NOTICE.txt', 'THIRD_PARTY.md', 'LICENSE', 'README.txt']) {
    if (!has(f)) bad.push(`missing ${f}`);
  }
  if (has('shell.json')) {
    const s = JSON.parse(fs.readFileSync(path.join(dir, 'shell.json'), 'utf8'));
    if (s.portable !== true) bad.push('shell.json: portable is not true');
    if (s.updates && s.updates.edition !== 'portable') bad.push('shell.json: an update feed that is not edition portable (a portable copy never replaces itself)');
    if (s.store) bad.push('shell.json: store build');
  }
  if (has('data')) bad.push('data/ is in the zip (it is made on first run; never ship someone\'s data)');
  return bad;
}
