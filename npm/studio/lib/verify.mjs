// SPDX-License-Identifier: Apache-2.0
// Checks a downloaded file the same way Studio's own updater and the website do (getcadcraft.com UpdateFeed::verifySignature):
// a minisign signature (Tauri's updater format, base64 of the .sig text) made with CadCraft's updater key, plus the SHA-256
// the release's manifest.json lists. No dependencies: Node's crypto has BLAKE2b-512 and Ed25519.
import crypto from 'node:crypto';
import fs from 'node:fs';

// CadCraft's updater public key (desktop-shell/updater-key.pub; the same key Studio's updater trusts).
export const CADCRAFT_PUBKEY = 'dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEUwQUI2NkNGQ0QzOTEzMTcKUldRWEV6bk56MmFyNE9ISzR5UzBIR1kzV3BVYitJazA2cUJ0bklwUVp4dHN3KzBzMHA2OTdaNU4K';

const SPKI_ED25519 = Buffer.from('302a300506032b6570032100', 'hex');
const edKey = (raw32) => crypto.createPublicKey({ key: Buffer.concat([SPKI_ED25519, raw32]), format: 'der', type: 'spki' });
const lines = (txt, trim) => txt.split('\n').map((l) => (trim ? l.trim() : l.replace(/\s+$/, ''))).filter((l) => l.length);

/** Parse a base64 minisign public key file → {keyId (8 bytes), key (KeyObject)}; throws when it is not one. */
export function parsePubkey(b64) {
  const pl = lines(Buffer.from(b64, 'base64').toString('utf8'), true);
  const pk = Buffer.from(pl[1] || '', 'base64');
  if (pk.length !== 42 || pk.subarray(0, 2).toString('latin1') !== 'Ed') throw new Error('not a minisign public key');
  return { keyId: pk.subarray(2, 10), key: edKey(pk.subarray(10, 42)) };
}

/** Hash a file once for both checks → {sha256 (hex), blake2b (Buffer, 64 bytes)}. */
export async function hashFile(file) {
  const sha = crypto.createHash('sha256');
  const b2 = crypto.createHash('blake2b512');
  for await (const chunk of fs.createReadStream(file, { highWaterMark: 1 << 20 })) { sha.update(chunk); b2.update(chunk); }
  return { sha256: sha.digest('hex'), blake2b: b2.digest() };
}

/**
 * Verify a minisign signature over a file whose hashes are already known.
 * → {ok, why, trusted} (trusted = the signed trusted comment, e.g. "timestamp:…\tfile:…")
 */
export function verifySignature(hashes, sigB64, pubB64 = CADCRAFT_PUBKEY, fileForLegacy = null) {
  const bad = (why) => ({ ok: false, why, trusted: null });
  let pub;
  try { pub = parsePubkey(pubB64); } catch (e) { return bad(e.message); }
  const sl = lines(Buffer.from(String(sigB64 || '').trim(), 'base64').toString('utf8'), false);
  if (sl.length < 4 || !sl[2].startsWith('trusted comment: ')) return bad('not a minisign signature');
  const sig = Buffer.from(sl[1], 'base64');
  const global = Buffer.from(sl[3], 'base64');
  if (sig.length !== 74 || global.length !== 64) return bad('malformed signature');
  if (!sig.subarray(2, 10).equals(pub.keyId)) return bad('signed with another key');
  const alg = sig.subarray(0, 2).toString('latin1');
  let msg;
  if (alg === 'ED') msg = hashes.blake2b;
  else if (alg === 'Ed' && fileForLegacy) msg = fs.readFileSync(fileForLegacy);
  else return bad('unknown signature algorithm');
  const s64 = sig.subarray(10, 74);
  if (!crypto.verify(null, msg, pub.key, s64)) return bad('the signature does not match the file');
  const trusted = sl[2].slice('trusted comment: '.length);
  if (!crypto.verify(null, Buffer.concat([s64, Buffer.from(trusted, 'utf8')]), pub.key, global)) return bad('the trusted comment was changed');
  return { ok: true, why: '', trusted };
}
