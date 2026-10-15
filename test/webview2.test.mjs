// SPDX-License-Identifier: Apache-2.0
// webview2.test.mjs — the WebView2 runtime-age rule (webview2.mjs, SD.2) on a stand-in feed. No network:
//   node test/webview2.test.mjs
import { checkRuntime, runtimeVersion, stableVersions } from '../webview2.mjs';

let pass = 0, fail = 0;
const ok = (c, what, extra = '') => { if (c) { pass++; console.log(`  ✓ ${what}`); } else { fail++; console.log(`  ✗ ${what} ${extra}`); } };

const rel = (v, platform = 'Windows', arch = 'x64') => ({ ProductVersion: v, Platform: platform, Architecture: arch });
const feed = [
  { Product: 'Beta', Releases: [rel('156.0.4300.1')] },
  { Product: 'Stable', Releases: [
    rel('154.0.4258.62'), rel('155.0.4283.45'), rel('154.0.4258.53'), rel('153.0.4234.48'),
    rel('155.0.4283.99', 'MacOS', 'universal'), rel('155.0.4283.98', 'Windows', 'arm64'), rel('not.a.version'),
  ] },
];

console.log('webview2 runtime check');
ok(runtimeVersion('/x/Microsoft.WebView2.FixedVersionRuntime.155.0.4283.45.x64') === '155.0.4283.45', 'version from the folder name');
ok(runtimeVersion('155.0.4283.45') === '155.0.4283.45', 'bare version');
ok(runtimeVersion('/x/webview2') === null, 'no version in the name → null');
ok(stableVersions(feed).join() === '155.0.4283.45,154.0.4258.62,154.0.4258.53,153.0.4234.48', 'Stable, Windows x64 only, newest first');

let r = checkRuntime('155.0.4283.45', feed);
ok(r.ok && r.newestOfMajor === '155.0.4283.45', 'newest of its major → ok', r.message);
r = checkRuntime('154.0.4258.53', feed);
ok(!r.ok && r.newestOfMajor === '154.0.4258.62' && /older than 154\.0\.4258\.62/.test(r.message), 'older patch of its major → refused', r.message);
r = checkRuntime('154.0.4258.62', feed);
ok(r.ok && /major 155 is out/.test(r.message), 'newest of an older major → ok, with a note that a newer major is out', r.message);
r = checkRuntime('150.0.4000.1', feed);
ok(!r.ok && /no longer lists major 150/.test(r.message), 'a major Microsoft no longer lists → refused', r.message);
r = checkRuntime('155.0.4283.46', feed);
ok(r.ok, 'newer than the feed (fixed runtime out before the list) → ok');
ok(!checkRuntime('x', feed).ok, 'not a version → refused');
let threw = false; try { checkRuntime('155.0.4283.45', { nope: 1 }); } catch { threw = true; }
ok(threw, 'a feed of the wrong shape throws (build refuses)');

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
