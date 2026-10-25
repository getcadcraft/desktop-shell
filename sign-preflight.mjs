#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
// sign-preflight.mjs — which platforms can be signed from THIS environment, and what is missing for each (0.7 SIGN).
// Reads only the environment (and that the files it names exist); never prints a value, never contacts anyone, signs
// nothing. Run it before a release build, with the same variables the build will see:
//
//   set -a; . /var/www/CADCRAFT/secrets/android/credentials.env; eval "$(grep -E '^(AZURE_[A-Z_]+|JSIGN_JAR|CC_UPDATE_[A-Z]+)=' /var/www/CADCRAFT/.env)"; set +a
//   node sign-preflight.mjs            exit 0 when every platform is ready, 1 when something is missing (it says what)
//   node sign-preflight.mjs --json     the same as data
import fs from 'node:fs';

/** Every check is [variable, what it is]; `file` marks a value that must name an existing file. */
export const PLATFORMS = [
  { key: 'windows', label: 'Windows installer (Azure Artifact Signing + jsign)', needs: [
    ['AZURE_TENANT_ID', 'the Azure tenant'], ['AZURE_CLIENT_ID', 'the app registration cadcraft-build-signer'],
    ['AZURE_CLIENT_SECRET', 'its secret (the owner adds it; expires 2027-10-14)'], ['AZURE_SIGNING_ENDPOINT', 'e.g. eus.codesigning.azure.net'],
    ['AZURE_SIGNING_ACCOUNT', 'cadcraft-signing'], ['AZURE_SIGNING_PROFILE', 'cadcraft-public (exists after identity validation)'],
    ['JSIGN_JAR', 'jsign 7.5 or later', 'file'] ] },
  { key: 'macos', label: 'macOS (Developer ID + notarization), on the Mac', needs: [
    ['APPLE_SIGNING_IDENTITY', 'Developer ID Application: … (KXB5X8ZLX7)'], ['APPLE_TEAM_ID', 'KXB5X8ZLX7'],
    ['APPLE_API_ISSUER', 'App Store Connect API issuer id'], ['APPLE_API_KEY', 'its key id'], ['APPLE_API_KEY_PATH', 'AuthKey_<id>.p8', 'file'] ],
    alt: [['APPLE_ID', ''], ['APPLE_PASSWORD', 'app-specific password'], ['APPLE_TEAM_ID', '']] },
  { key: 'android', label: 'Android (our release key)', needs: [
    ['CC_ANDROID_KEYSTORE', 'the .jks', 'file'], ['CC_ANDROID_KEYSTORE_PASSWORD', ''], ['CC_ANDROID_KEY_ALIAS', ''], ['CC_ANDROID_KEY_PASSWORD', ''] ] },
  { key: 'linux', label: 'Linux (OpenPGP over SHA256SUMS)', needs: [['GNUPGHOME', 'the keyring folder', 'file'], ['CC_GPG_KEY', 'the release key\'s fingerprint']] },
  { key: 'updater', label: 'Studio\'s updater (minisign key for every platform\'s update file)', needs: [
    ['CC_UPDATE_PUBKEY', 'the PUBLIC key (cargo tauri signer generate): goes into shell.json'],
    ['TAURI_SIGNING_PRIVATE_KEY', 'the private key (CI secret / this build only)'] ] },
  { key: 'msix', label: 'Microsoft Store MSIX (the Store signs it; only the identity is ours)', needs: [
    ['MSIX_IDENTITY_NAME', 'Partner Center › Product identity'], ['MSIX_PUBLISHER', 'the CN=… string'], ['MSIX_PUBLISHER_DISPLAY', 'publisher display name'] ] },
];

export function preflight(env = process.env, exists = fs.existsSync) {
  return PLATFORMS.map((p) => {
    const miss = (list) => list.filter(([v, , kind]) => !env[v] || (kind === 'file' && !exists(env[v]))).map(([v, what, kind]) => ({ var: v, what, problem: !env[v] ? 'not set' : 'set, but no such file' }));
    let missing = miss(p.needs);
    // macOS may use an Apple ID + app-specific password instead of the API key.
    if (p.alt && missing.length && !miss(p.alt).length && env.APPLE_SIGNING_IDENTITY) missing = miss([p.needs[0], p.needs[1]]);
    return { key: p.key, label: p.label, ready: missing.length === 0, missing };
  });
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const r = preflight();
  if (process.argv.includes('--json')) console.log(JSON.stringify(r, null, 2));
  else for (const p of r) {
    console.log(`${p.ready ? '✓' : '✗'} ${p.label}`);
    for (const m of p.missing) console.log(`    ${m.var}: ${m.problem}${m.what ? ' — ' + m.what : ''}`);
  }
  process.exit(r.every((p) => p.ready) ? 0 : 1);
}
