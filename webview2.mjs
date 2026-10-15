// SPDX-License-Identifier: Apache-2.0
// webview2.mjs — is the WebView2 Fixed Version runtime we are about to ship current? (docs/0.5-PLAN.md SD.2)
//
// A fixed runtime never updates itself, so an installer built with an old one ships old Chromium security bugs. Microsoft
// publishes a Fixed Version runtime for each Edge Stable release, and keeps the newest of each recent major for download.
// The rule build.mjs applies: refuse a runtime older than the newest Edge Stable release of the same major (Windows x64).
// A newer major being out is reported, not refused (Microsoft may not have its fixed runtime up yet).
//
//   node webview2.mjs <runtime folder | version>     prints the verdict (exit 0 current, 1 refused, 2 feed unreachable)

import path from 'node:path';

export const FEED = 'https://edgeupdates.microsoft.com/api/products?view=enterprise';

/** "155.0.4283.45" → [155, 0, 4283, 45], or null. */
export const parseVersion = (v) => {
  const m = String(v || '').match(/^(\d+)\.(\d+)\.(\d+)\.(\d+)$/);
  return m ? m.slice(1).map(Number) : null;
};
export const compare = (a, b) => { for (let i = 0; i < 4; i++) if (a[i] !== b[i]) return a[i] - b[i]; return 0; };

/** The runtime's version from its folder name (Microsoft.WebView2.FixedVersionRuntime.<v>.x64) or a bare version. */
export const runtimeVersion = (dirOrVersion) => {
  const s = path.basename(String(dirOrVersion)).match(/(\d+\.\d+\.\d+\.\d+)/)?.[1];
  return s && parseVersion(s) ? s : null;
};

/** Edge Stable versions for Windows x64 from the feed's JSON (newest first). */
export function stableVersions(feed) {
  if (!Array.isArray(feed)) throw new Error('unexpected feed shape');
  const stable = feed.find((p) => p && p.Product === 'Stable');
  if (!stable || !Array.isArray(stable.Releases)) throw new Error('no Stable channel in the feed');
  return stable.Releases
    .filter((r) => r && r.Platform === 'Windows' && r.Architecture === 'x64' && parseVersion(r.ProductVersion))
    .map((r) => r.ProductVersion)
    .sort((a, b) => compare(parseVersion(b), parseVersion(a)));
}

/** The verdict for one runtime version against the feed: {ok, version, newestOfMajor, newest, message}. */
export function checkRuntime(version, feed) {
  const v = parseVersion(version);
  if (!v) return { ok: false, version, message: `not a WebView2 runtime version: ${version}` };
  const all = stableVersions(feed);
  const same = all.filter((x) => parseVersion(x)[0] === v[0]);
  const newest = all[0] || null;
  if (!same.length) {
    return { ok: false, version, newest, message: `WebView2 ${version}: Microsoft no longer lists major ${v[0]} — use a current Fixed Version runtime (newest Edge Stable: ${newest})` };
  }
  const newestOfMajor = same[0];
  if (compare(v, parseVersion(newestOfMajor)) < 0) {
    return { ok: false, version, newestOfMajor, newest, message: `WebView2 ${version} is older than ${newestOfMajor}, the newest of major ${v[0]}: download that Fixed Version runtime and rebuild (security fixes)` };
  }
  const note = newest && parseVersion(newest)[0] > v[0] ? ` (note: major ${parseVersion(newest)[0]} is out, ${newest}; move to it once Microsoft has its Fixed Version runtime)` : '';
  return { ok: true, version, newestOfMajor, newest, message: `WebView2 ${version} is the newest of major ${v[0]}${note}` };
}

/** Fetches the feed (Node 18+ fetch). Throws on any network or HTTP failure. */
export async function fetchFeed(url = FEED, timeoutMs = 20000) {
  const r = await fetch(url, { signal: AbortSignal.timeout(timeoutMs) });
  if (!r.ok) throw new Error(`${url} → HTTP ${r.status}`);
  return r.json();
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const v = runtimeVersion(process.argv[2] || '');
  if (!v) { console.error('usage: node webview2.mjs <runtime folder | version>'); process.exit(1); }
  let feed;
  try { feed = await fetchFeed(); } catch (e) { console.error(`cannot reach Microsoft's Edge release list: ${e.message}`); process.exit(2); }
  const r = checkRuntime(v, feed);
  console.log(r.message);
  process.exit(r.ok ? 0 : 1);
}
