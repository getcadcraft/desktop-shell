// SPDX-License-Identifier: Apache-2.0
// apple.test.mjs — the iPhone + iPad project as committed (0.6 IPH.1): device family, orientations, signing team, the privacy
// manifest and its place in the Xcode project. Static checks on Linux (no Xcode); build.mjs's checkAppleBundle() checks the
// built app on the Mac. Plists are read with python3's plistlib.
//   node test/apple.test.mjs
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const HERE = path.dirname(new URL(import.meta.url).pathname);
const TAURI = path.join(HERE, '../src-tauri');
const GEN = path.join(TAURI, 'gen/apple');
const DDG_TEAM = 'KXB5X8ZLX7';        // The Data Design Group Inc (owner, 2026-10-15) — never a personal team

let pass = 0, fail = 0;
const ok = (c, what, extra = '') => { if (c) { pass++; console.log(`  ✓ ${what}`); } else { fail++; console.log(`  ✗ ${what} ${extra}`); } };
const read = (f) => fs.readFileSync(f, 'utf8');
const plist = (f) => {
  const r = spawnSync('python3', ['-c', 'import json,plistlib,sys; print(json.dumps(plistlib.load(open(sys.argv[1],"rb")), default=str))', f], { encoding: 'utf8' });
  if (r.status !== 0) throw new Error(`${f}: ${r.stderr.trim()}`);
  return JSON.parse(r.stdout);
};
const PHONE = ['UIInterfaceOrientationPortrait', 'UIInterfaceOrientationLandscapeLeft', 'UIInterfaceOrientationLandscapeRight'];
const PAD = ['UIInterfaceOrientationPortrait', 'UIInterfaceOrientationPortraitUpsideDown', 'UIInterfaceOrientationLandscapeLeft', 'UIInterfaceOrientationLandscapeRight'];
const same = (a, b) => [...(a || [])].sort().join() === [...b].sort().join();

console.log('Apple project (iPhone + iPad)');
const yml = read(path.join(GEN, 'project.yml'));
const pbx = read(path.join(GEN, 'cadcraft-desktop.xcodeproj/project.pbxproj'));
ok(/TARGETED_DEVICE_FAMILY:\s*"1,2"/.test(yml) && (yml.match(/TARGETED_DEVICE_FAMILY/g) || []).length === 1, 'project.yml: TARGETED_DEVICE_FAMILY "1,2"');
const fams = [...pbx.matchAll(/TARGETED_DEVICE_FAMILY = "([^"]*)";/g)].map((m) => m[1]);
ok(fams.length === 2 && fams.every((f) => f === '1,2'), 'pbxproj: debug and release both "1,2"', JSON.stringify(fams));
const teams = [...pbx.matchAll(/DEVELOPMENT_TEAM = "?([A-Z0-9]+)"?;/g)].map((m) => m[1]);
ok(teams.length >= 2 && teams.every((t) => t === DDG_TEAM), `pbxproj: signing team ${DDG_TEAM} (DDG) everywhere`, JSON.stringify(teams));
ok(!/DEVELOPMENT_TEAM|developmentTeam/.test(yml.replace(/#.*$/gm, '')) || yml.includes(DDG_TEAM), 'project.yml names no other team');
const iosConf = JSON.parse(read(path.join(TAURI, 'tauri.ios.conf.json')));
ok(iosConf.bundle?.iOS?.developmentTeam === DDG_TEAM && iosConf.identifier === 'com.getcadcraft.studio', 'tauri.ios.conf.json: DDG team, bundle id');
const minOs = iosConf.bundle.iOS.minimumSystemVersion;
const targets = [...pbx.matchAll(/IPHONEOS_DEPLOYMENT_TARGET = ([0-9.]+);/g)].map((m) => m[1]);
ok(targets.length && targets.every((t) => t === minOs) && yml.includes(`iOS: ${minOs}`), `deployment target ${minOs} everywhere`, JSON.stringify(targets));
ok(/PRODUCT_BUNDLE_IDENTIFIER = com\.getcadcraft\.studio;/.test(pbx), 'pbxproj: bundle id');

const info = plist(path.join(GEN, 'cadcraft-desktop_iOS/Info.plist'));
const extra = plist(path.join(TAURI, 'Info.ios.plist'));
ok(!('UIDeviceFamily' in info) && !('UIDeviceFamily' in extra), 'no UIDeviceFamily in either plist (Xcode sets it from the build setting)');
ok(same(info.UISupportedInterfaceOrientations, PHONE) && same(extra.UISupportedInterfaceOrientations, PHONE), 'iPhone: upright + both sideways, no upside-down');
ok(same(info['UISupportedInterfaceOrientations~ipad'], PAD) && same(extra['UISupportedInterfaceOrientations~ipad'], PAD), 'iPad: all four');
ok(info.UIRequiresFullScreen === false && extra.UIRequiresFullScreen === false, 'UIRequiresFullScreen false (iPad multitasking)');
ok(extra.NSLocalNetworkUsageDescription && (extra.NSBonjourServices || []).includes('_ccstudio._tcp'), 'local network text + Bonjour _ccstudio._tcp (Studio Live)');
ok(extra.LSSupportsOpeningDocumentsInPlace === true && extra.UIFileSharingEnabled === false, 'documents open in place; no iTunes file sharing');
ok(info.LSRequiresIPhoneOS === true && same(info.UIRequiredDeviceCapabilities, ['arm64', 'metal']), 'LSRequiresIPhoneOS, arm64 + metal');
ok(info.UILaunchStoryboardName === 'LaunchScreen' && fs.existsSync(path.join(GEN, 'LaunchScreen.storyboard')), 'launch storyboard (required for iPhone screen sizes)');
ok(!('ITSAppUsesNonExemptEncryption' in extra), 'export compliance left to the owner and counsel (IPAD.md)');

const priv = plist(path.join(GEN, 'cadcraft-desktop_iOS/PrivacyInfo.xcprivacy'));
ok(priv.NSPrivacyTracking === false && Array.isArray(priv.NSPrivacyTrackingDomains) && !priv.NSPrivacyTrackingDomains.length, 'privacy manifest: no tracking');
const reasons = Object.fromEntries((priv.NSPrivacyAccessedAPITypes || []).map((t) => [t.NSPrivacyAccessedAPIType, t.NSPrivacyAccessedAPITypeReasons]));
ok(same(reasons.NSPrivacyAccessedAPICategoryFileTimestamp, ['3B52.1', 'C617.1']), 'file timestamps: picked documents + own container');
ok(same(reasons.NSPrivacyAccessedAPICategorySystemBootTime, ['35F9.1']), 'boot time: elapsed time inside the app');
const ref = pbx.match(/(\w{24}) \/\* PrivacyInfo\.xcprivacy \*\/ = \{isa = PBXFileReference;/)?.[1];
const bf = ref && pbx.match(new RegExp(`(\\w{24}) /\\* PrivacyInfo\\.xcprivacy in Resources \\*/ = \\{isa = PBXBuildFile; fileRef = ${ref} `))?.[1];
const resPhase = pbx.match(/isa = PBXResourcesBuildPhase;[\s\S]*?files = \(([\s\S]*?)\);/)?.[1] || '';
const iosGroup = pbx.match(/\/\* cadcraft-desktop_iOS \*\/ = \{\s*isa = PBXGroup;\s*children = \(([\s\S]*?)\);/)?.[1] || '';
ok(ref && bf && resPhase.includes(bf) && iosGroup.includes(ref), 'pbxproj: privacy manifest referenced, in its group and in Resources');
ok(/path: cadcraft-desktop_iOS\b/.test(yml), 'project.yml takes the cadcraft-desktop_iOS folder (xcodegen keeps the manifest)');
const ids = [...pbx.matchAll(/^\t\t(\w{24}) \/\*/gm)].map((m) => m[1]);
ok(ids.length === new Set(ids).size, 'pbxproj: object ids unique');

console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
