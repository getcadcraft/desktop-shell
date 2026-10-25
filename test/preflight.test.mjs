// SPDX-License-Identifier: Apache-2.0
// sign-preflight.mjs says what is missing per platform and never prints a value.   node test/preflight.test.mjs
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { preflight, PLATFORMS } from '../sign-preflight.mjs';

let pass = 0, fail = 0;
const ok = (c, m, extra) => { if (c) pass++; else { fail++; console.log('  ✗ ' + m + (extra ? ' ' + extra : '')); } };
const yes = () => true;
const by = (r, k) => r.find((p) => p.key === k);

ok(PLATFORMS.map((p) => p.key).join() === 'windows,macos,android,linux,updater,msix', 'every platform and the updater key and the MSIX identity are checked');
const none = preflight({}, yes);
ok(none.every((p) => !p.ready && p.missing.length), 'an empty environment: nothing is ready, everything is named');
ok(by(none, 'windows').missing.length === 7 && by(none, 'android').missing.length === 4, 'Windows needs seven values, Android four');
const win = { AZURE_TENANT_ID: 'a', AZURE_CLIENT_ID: 'b', AZURE_CLIENT_SECRET: 'c', AZURE_SIGNING_ENDPOINT: 'd', AZURE_SIGNING_ACCOUNT: 'e', AZURE_SIGNING_PROFILE: 'f', JSIGN_JAR: '/x/jsign.jar' };
ok(by(preflight(win, yes), 'windows').ready, 'all seven set (the jar exists): Windows is ready');
ok(by(preflight(win, () => false), 'windows').missing.map((m) => m.problem).join() === 'set, but no such file', 'a jar path that does not exist is not ready');
const { AZURE_SIGNING_PROFILE, ...noProfile } = win;
ok(by(preflight(noProfile, yes), 'windows').missing.map((m) => m.var).join() === 'AZURE_SIGNING_PROFILE', 'a missing profile (the identity-validation wait) is the one thing named');
const apple = { APPLE_SIGNING_IDENTITY: 'Developer ID Application: X (KXB5X8ZLX7)', APPLE_TEAM_ID: 'KXB5X8ZLX7', APPLE_ID: 'a@b', APPLE_PASSWORD: 'p' };
ok(by(preflight(apple, yes), 'macos').ready, 'macOS with an Apple ID + app password instead of the API key is ready');
ok(!by(preflight({ APPLE_TEAM_ID: 'KXB5X8ZLX7', APPLE_ID: 'a@b', APPLE_PASSWORD: 'p' }, yes), 'macos').ready, 'but not without the signing identity');

// The command line: exit code, and no values in the output.
const env = { ...process.env, AZURE_CLIENT_SECRET: 'SECRET-VALUE-123', CC_ANDROID_KEYSTORE_PASSWORD: 'PW-VALUE-456' };
const r = spawnSync(process.execPath, [path.join(path.dirname(new URL(import.meta.url).pathname), '..', 'sign-preflight.mjs')], { encoding: 'utf8', env });
ok(r.status === 1 && /✗ Windows/.test(r.stdout), 'the command exits 1 while something is missing');
ok(!/SECRET-VALUE-123|PW-VALUE-456/.test(r.stdout + r.stderr), 'it never prints a value');

console.log(`${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);
