// SPDX-License-Identifier: Apache-2.0
// NET.3 (Lucas, Discord 2026-11-12): a strict list of the hosts each app's pages may reach in the desktop app, as a CSP connect-src
// header (hosts.json is the data). build.mjs --strict-hosts adds it to shell.json `headers`; src-tauri/src/protocol.rs sends those
// headers with every response, workers included, so no Rust change is needed.
//   connectSrc(slug | 'studio') → 'connect-src …'   ·   strictHeaders(slug) → { 'Content-Security-Policy': … }
// The app's own origin is 'self' (http://cadcraft.localhost on Windows/Android, cadcraft://localhost on macOS/Linux, both named too);
// Tauri's IPC is ipc://localhost or http://ipc.localhost; blob: and data: keep in-page files (fetch(blobUrl)) working.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
export const HOSTS = JSON.parse(fs.readFileSync(path.join(HERE, 'hosts.json'), 'utf8'));
const BASE = ["'self'", 'cadcraft://localhost', 'http://cadcraft.localhost', 'https://cadcraft.localhost', 'ipc:', 'http://ipc.localhost', 'blob:', 'data:'];

/** Every host an app (or 'studio' = all eight) may reach: the always list, the app's own, the add-on lists. Sorted, no repeats. */
export function hostsFor(slug, h = HOSTS) {
  const apps = slug === 'studio' || slug === 'supercadcraft' ? Object.keys(h.apps) : [slug];
  if (!apps.every((a) => Object.hasOwn(h.apps, a))) throw new Error('hosts.json has no list for ' + slug);
  const out = new Set(Object.keys(h.always));
  for (const a of apps) for (const k of Object.keys(h.apps[a])) out.add(k);
  for (const list of Object.values(h.addons || {})) for (const k of Object.keys(list)) out.add(k);
  return [...out].sort();
}

export function connectSrc(slug, h = HOSTS) {
  return 'connect-src ' + [...BASE, ...hostsFor(slug, h).map((x) => 'https://' + x)].join(' ');
}

export const strictHeaders = (slug, h = HOSTS) => ({ 'Content-Security-Policy': connectSrc(slug, h) });
