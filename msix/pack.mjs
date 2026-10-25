// SPDX-License-Identifier: Apache-2.0
// msix/pack.mjs — pack an MSIX layout folder into a .msix on any system (0.7 MSIX), the job `makeappx pack` does on Windows.
//
// An MSIX is an OPC zip: the payload files, AppxManifest.xml, AppxBlockMap.xml (a SHA-256 for every 64 KiB block of every
// file, so Windows can check and stream it) and [Content_Types].xml, in that order. A signer (signtool, jsign) adds
// AppxSignature.p7x afterwards. What this writer does, as MakeAppx does:
//   - each file is stored or deflated; a deflated file is deflated block by block (64 KiB of input each, full flush between
//     them), so every block's compressed size can be listed (Block Size=…) and checked on its own;
//   - zip names are the OPC part names (percent-encoded, "/"), the block map names the file path (as is, "\");
//   - LfhSize = the local file header's size (30 + name + extra), which the block map must state;
//   - no data descriptors; the zip64 end records always (Windows' signature digest covers them); entries themselves are not
//     zip64 (refused above 65 535 entries or 4 GiB: Studio's package is ~2 000 files, ~100 MB).
// It is the Linux route for tests and CI checks; the Store and website builds can still be packed with makeappx on Windows
// (PACK.md). `unpackCheck()` reads a package back and checks every block hash (the tests use it).
import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';
import crypto from 'node:crypto';

const BLOCK = 65536;
/** Already compressed: stored, not deflated (MakeAppx does the same for these). */
const STORED = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'zip', 'gz', 'br', 'woff', 'woff2', 'mp4', 'webm', 'mp3', 'ogg', 'jxl', 'avif']);

const TYPES = {
  exe: 'application/x-msdownload', dll: 'application/x-msdownload', json: 'application/json', js: 'application/javascript',
  mjs: 'application/javascript', css: 'text/css', html: 'text/html', htm: 'text/html', svg: 'image/svg+xml', png: 'image/png',
  jpg: 'image/jpeg', jpeg: 'image/jpeg', gif: 'image/gif', webp: 'image/webp', ico: 'image/vnd.microsoft.icon', xml: 'text/xml',
  txt: 'text/plain', md: 'text/plain', wasm: 'application/wasm', woff: 'font/woff', woff2: 'font/woff2', ttf: 'font/ttf',
  otf: 'font/otf', pdf: 'application/pdf', zip: 'application/zip',
};

/** The OPC part name of a file path inside the package: "/" separators, each segment percent-encoded (RFC 3986 unreserved
 *  characters and a few sub-delims kept, as MakeAppx writes them). */
export function partName(rel) {
  return rel.split(/[\\/]/).map((seg) => [...Buffer.from(seg, 'utf8')].map((b) => {
    const c = String.fromCharCode(b);
    return /[A-Za-z0-9\-._~!$&'()*+,;=@]/.test(c) && b < 128 ? c : '%' + b.toString(16).toUpperCase().padStart(2, '0');
  }).join('')).join('/');
}

function walk(dir, base = dir, out = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, base, out);
    else if (e.isFile()) out.push(path.relative(base, p));
  }
  return out;
}

const xmlEsc = (s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

function dosTime(d = new Date(Date.UTC(2026, 0, 1))) {
  const time = (d.getUTCHours() << 11) | (d.getUTCMinutes() << 5) | (d.getUTCSeconds() >> 1);
  const date = ((d.getUTCFullYear() - 1980) << 9) | ((d.getUTCMonth() + 1) << 5) | d.getUTCDate();
  return { time, date };
}

let crcTable;
function crc32(buf, crc = 0) {
  if (!crcTable) {
    crcTable = new Uint32Array(256);
    for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; crcTable[n] = c >>> 0; }
  }
  crc = ~crc >>> 0;
  for (let i = 0; i < buf.length; i++) crc = crcTable[(crc ^ buf[i]) & 0xff] ^ (crc >>> 8);
  return ~crc >>> 0;
}

/** One file's zip data and its block map entry. */
function encode(name, data, compress) {
  const blocks = [];
  const parts = [];
  if (!compress) {
    for (let i = 0; i < data.length; i += BLOCK) blocks.push({ hash: crypto.createHash('sha256').update(data.subarray(i, i + BLOCK)).digest('base64') });
    parts.push(data);
  } else {
    const n = Math.max(1, Math.ceil(data.length / BLOCK));
    for (let i = 0; i < n; i++) {
      const raw = data.subarray(i * BLOCK, (i + 1) * BLOCK);
      const last = i === n - 1;
      const c = zlib.deflateRawSync(raw, { level: 9, finishFlush: last ? zlib.constants.Z_FINISH : zlib.constants.Z_FULL_FLUSH });
      parts.push(c);
      if (data.length) blocks.push({ hash: crypto.createHash('sha256').update(raw).digest('base64'), size: c.length });
    }
  }
  const body = Buffer.concat(parts);
  return { name, data, body, method: compress ? 8 : 0, crc: crc32(data), blocks };
}

/**
 * Pack `dir` (an MSIX layout with AppxManifest.xml) into `file`. → {file, files, bytes}
 */
export function pack(dir, file) {
  if (!fs.existsSync(path.join(dir, 'AppxManifest.xml'))) throw new Error(`${dir}: no AppxManifest.xml`);
  const rels = walk(dir).filter((r) => !/^(AppxBlockMap\.xml|\[Content_Types\]\.xml|AppxSignature\.p7x)$/i.test(r));
  // The manifest goes last among the payload (MakeAppx's order), then the block map and the content types.
  rels.sort((a, b) => (a === 'AppxManifest.xml') - (b === 'AppxManifest.xml'));
  if (rels.length + 2 > 65535) throw new Error('too many files for a zip without zip64');
  const entries = rels.map((rel) => {
    const data = fs.readFileSync(path.join(dir, rel));
    const ext = path.extname(rel).slice(1).toLowerCase();
    return { rel, ...encode(partName(rel), data, data.length > 0 && !STORED.has(ext)) };
  });
  // Block map: every payload file + the manifest.
  const bm = ['<?xml version="1.0" encoding="UTF-8" standalone="no"?>',
    '<BlockMap xmlns="http://schemas.microsoft.com/appx/2010/blockmap" HashMethod="http://www.w3.org/2001/04/xmlenc#sha256">'];
  for (const e of entries) {
    const lfh = 30 + Buffer.byteLength(e.name);
    const inner = e.blocks.map((b) => `<Block Hash="${b.hash}"${b.size !== undefined ? ` Size="${b.size}"` : ''}/>`).join('');
    bm.push(`<File Name="${xmlEsc(e.rel.split('/').join('\\'))}" Size="${e.data.length}" LfhSize="${lfh}">${inner}</File>`);
  }
  bm.push('</BlockMap>');
  const exts = new Map();
  const overrides = [];
  for (const e of entries) {
    const ext = path.extname(e.rel).slice(1);
    if (!ext) overrides.push(['/' + e.name, 'application/octet-stream']);
    else if (!exts.has(ext.toLowerCase())) exts.set(ext.toLowerCase(), [ext, TYPES[ext.toLowerCase()] || 'application/octet-stream']);
  }
  exts.delete('xml'); // AppxManifest.xml has its own override; other .xml files are text/xml below
  const ct = ['<?xml version="1.0" encoding="UTF-8" standalone="no"?>', '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'];
  for (const [, [ext, type]] of exts) ct.push(`<Default Extension="${xmlEsc(ext)}" ContentType="${type}"/>`);
  if (entries.some((e) => /\.xml$/i.test(e.rel) && e.rel !== 'AppxManifest.xml')) ct.push('<Default Extension="xml" ContentType="text/xml"/>');
  for (const [p, t] of overrides) ct.push(`<Override PartName="${xmlEsc(p)}" ContentType="${t}"/>`);
  ct.push('<Override PartName="/AppxManifest.xml" ContentType="application/vnd.ms-appx.manifest+xml"/>');
  ct.push('<Override PartName="/AppxBlockMap.xml" ContentType="application/vnd.ms-appx.blockmap+xml"/>');
  ct.push('</Types>');
  const meta = [encode('AppxBlockMap.xml', Buffer.from(bm.join('\r\n'), 'utf8'), true), encode('[Content_Types].xml', Buffer.from(ct.join('\r\n'), 'utf8'), true)];

  const { time, date } = dosTime();
  const fd = fs.openSync(file + '.part', 'w');
  let off = 0;
  const central = [];
  const write = (b) => { fs.writeSync(fd, b); off += b.length; };
  for (const e of [...entries, ...meta]) {
    const name = Buffer.from(e.name, 'utf8');
    if (off > 0xffffffff - e.body.length) throw new Error('the package is over 4 GiB (zip64 is not written)');
    const h = Buffer.alloc(30);
    h.writeUInt32LE(0x04034b50, 0); h.writeUInt16LE(20, 4); h.writeUInt16LE(0, 6); h.writeUInt16LE(e.method, 8);
    h.writeUInt16LE(time, 10); h.writeUInt16LE(date, 12); h.writeUInt32LE(e.crc, 14); h.writeUInt32LE(e.body.length, 18);
    h.writeUInt32LE(e.data.length, 22); h.writeUInt16LE(name.length, 26); h.writeUInt16LE(0, 28);
    central.push({ e, name, at: off });
    write(h); write(name); write(e.body);
  }
  const cdAt = off;
  for (const { e, name, at } of central) {
    const c = Buffer.alloc(46);
    c.writeUInt32LE(0x02014b50, 0); c.writeUInt16LE(20, 4); c.writeUInt16LE(20, 6); c.writeUInt16LE(0, 8); c.writeUInt16LE(e.method, 10);
    c.writeUInt16LE(time, 12); c.writeUInt16LE(date, 14); c.writeUInt32LE(e.crc, 16); c.writeUInt32LE(e.body.length, 20);
    c.writeUInt32LE(e.data.length, 24); c.writeUInt16LE(name.length, 28); c.writeUInt16LE(0, 30); c.writeUInt16LE(0, 32);
    c.writeUInt16LE(0, 34); c.writeUInt16LE(0, 36); c.writeUInt32LE(0, 38); c.writeUInt32LE(at, 42);
    write(c); write(name);
  }
  const cdSize = off - cdAt;
  // Always the zip64 end records, as MakeAppx writes them: Windows' signature check hashes the central directory together with
  // them (AXCD), so a package without them fails Get-AuthenticodeSignature with HashMismatch once signed (found on workstation11a).
  const z64At = off;
  const z = Buffer.alloc(56);
  z.writeUInt32LE(0x06064b50, 0); z.writeBigUInt64LE(44n, 4); z.writeUInt16LE(45, 12); z.writeUInt16LE(45, 14);
  z.writeUInt32LE(0, 16); z.writeUInt32LE(0, 20); z.writeBigUInt64LE(BigInt(central.length), 24); z.writeBigUInt64LE(BigInt(central.length), 32);
  z.writeBigUInt64LE(BigInt(cdSize), 40); z.writeBigUInt64LE(BigInt(cdAt), 48);
  write(z);
  const loc = Buffer.alloc(20);
  loc.writeUInt32LE(0x07064b50, 0); loc.writeUInt32LE(0, 4); loc.writeBigUInt64LE(BigInt(z64At), 8); loc.writeUInt32LE(1, 16);
  write(loc);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(0xffff, 8); end.writeUInt16LE(0xffff, 10);
  end.writeUInt32LE(0xffffffff, 12); end.writeUInt32LE(0xffffffff, 16);
  write(end);
  fs.closeSync(fd);
  fs.renameSync(file + '.part', file);
  return { file, files: entries.length, bytes: off };
}

/** Read a .msix back: the zip's entries, the block map's hashes against the bytes, the content types. → problems[] */
export function unpackCheck(file) {
  const buf = fs.readFileSync(file);
  const problems = [];
  const eocd = buf.lastIndexOf(Buffer.from([0x50, 0x4b, 0x05, 0x06]));
  if (eocd < 0) return ['not a zip'];
  let count = buf.readUInt16LE(eocd + 10), cdAt = buf.readUInt32LE(eocd + 16);
  if (cdAt === 0xffffffff || count === 0xffff) {   // zip64: the real values are in the zip64 end record
    const loc = buf.lastIndexOf(Buffer.from([0x50, 0x4b, 0x06, 0x07]), eocd);
    if (loc < 0) return ['zip64 end record missing'];
    const z = Number(buf.readBigUInt64LE(loc + 8));
    if (buf.readUInt32LE(z) !== 0x06064b50) return ['bad zip64 end record'];
    count = Number(buf.readBigUInt64LE(z + 32)); cdAt = Number(buf.readBigUInt64LE(z + 48));
  }
  const files = new Map();
  let p = cdAt;
  for (let i = 0; i < count; i++) {
    if (buf.readUInt32LE(p) !== 0x02014b50) { problems.push('bad central directory'); break; }
    const method = buf.readUInt16LE(p + 10), csize = buf.readUInt32LE(p + 20), usize = buf.readUInt32LE(p + 24);
    const nl = buf.readUInt16LE(p + 28), xl = buf.readUInt16LE(p + 30), cl = buf.readUInt16LE(p + 32), at = buf.readUInt32LE(p + 42);
    const name = buf.subarray(p + 46, p + 46 + nl).toString('utf8');
    const lnl = buf.readUInt16LE(at + 26), lxl = buf.readUInt16LE(at + 28);
    const start = at + 30 + lnl + lxl;
    files.set(name, { method, csize, usize, lfh: 30 + lnl + lxl, body: buf.subarray(start, start + csize) });
    p += 46 + nl + xl + cl;
  }
  const bmE = files.get('AppxBlockMap.xml');
  if (!bmE) return [...problems, 'no AppxBlockMap.xml'];
  const inflate = (e) => (e.method === 8 ? zlib.inflateRawSync(e.body) : e.body);
  const bm = inflate(bmE).toString('utf8');
  if (!files.has('[Content_Types].xml')) problems.push('no [Content_Types].xml');
  if (!files.has('AppxManifest.xml')) problems.push('no AppxManifest.xml');
  let listed = 0;
  for (const m of bm.matchAll(/<File Name="([^"]+)" Size="(\d+)" LfhSize="(\d+)">(.*?)<\/File>/g)) {
    listed++;
    const rel = m[1].replace(/&amp;/g, '&').replace(/&quot;/g, '"').split('\\').join('/');
    const e = files.get(partName(rel));
    if (!e) { problems.push(`${rel}: in the block map, not in the zip`); continue; }
    if (+m[3] !== e.lfh) problems.push(`${rel}: LfhSize ${m[3]} ≠ ${e.lfh}`);
    const data = inflate(e);
    if (data.length !== +m[2]) problems.push(`${rel}: size ${data.length} ≠ ${m[2]}`);
    const blocks = [...m[4].matchAll(/<Block Hash="([^"]+)"(?: Size="(\d+)")?\/>/g)];
    let compressedSum = 0;
    blocks.forEach((b, i) => {
      const h = crypto.createHash('sha256').update(data.subarray(i * BLOCK, (i + 1) * BLOCK)).digest('base64');
      if (h !== b[1]) problems.push(`${rel}: block ${i} hash differs`);
      if (b[2]) compressedSum += +b[2];
    });
    if (blocks.length !== Math.ceil(data.length / BLOCK)) problems.push(`${rel}: ${blocks.length} blocks for ${data.length} bytes`);
    if (e.method === 8 && data.length && compressedSum !== e.csize) problems.push(`${rel}: block sizes add to ${compressedSum}, not ${e.csize}`);
  }
  const payload = [...files.keys()].filter((n) => !/^(AppxBlockMap\.xml|\[Content_Types\]\.xml|AppxSignature\.p7x)$/.test(n));
  if (listed !== payload.length) problems.push(`${payload.length} files in the zip, ${listed} in the block map`);
  return problems;
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const [cmd, a, b] = process.argv.slice(2);
  if (cmd === 'pack' && a && b) { const r = pack(a, b); console.log(`→ ${r.file}: ${r.files} files, ${(r.bytes / 1048576).toFixed(1)} MB`); }
  else if (cmd === 'check' && a) { const p = unpackCheck(a); for (const x of p) console.error('  ✗ ' + x); if (p.length) process.exit(1); console.log('the package reads back whole'); }
  else { console.error('usage: msix/pack.mjs pack <layout dir> <out.msix> | check <file.msix>'); process.exit(2); }
}
