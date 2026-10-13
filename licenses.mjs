// SPDX-License-Identifier: Apache-2.0
// licenses.mjs — the license gate for the shell itself (docs/0.4-PART2.md L.2): every Rust crate linked into the shipped
// executable must be permissive. Crates reached only through proc macros or build scripts never ship and are skipped.
// Writes THIRD_PARTY.md (each crate's own license/notice files), which build.mjs installs beside the executable.
//
//   node licenses.mjs [--target x86_64-pc-windows-msvc] [--out THIRD_PARTY.md]      exit 1 = a crate failed the gate

import fs from 'node:fs';
import path from 'node:path';
import { execFileSync } from 'node:child_process';

const HERE = path.dirname(new URL(import.meta.url).pathname);
const arg = (k, d) => { const i = process.argv.indexOf(k); return i > 0 ? process.argv[i + 1] : d; };
const TARGET = arg('--target', 'x86_64-pc-windows-msvc');
const OUT = arg('--out', path.join(HERE, 'THIRD_PARTY.md'));

// The allowlist (L.2) plus Unicode-3.0 (the Unicode data licence, permissive) and BSL-1.0 / MIT-0 / CC0-1.0.
const ALLOW = ['MIT', 'MIT-0', 'Apache-2.0', 'BSD-2-Clause', 'BSD-3-Clause', 'ISC', 'Zlib', 'Unlicense', '0BSD',
  'Unicode-3.0', 'BSL-1.0', 'CC0-1.0', 'Apache-2.0 WITH LLVM-exception'];

/** SPDX expression → the permissive choice, or null. Handles "A OR B", "A AND B", "A/B" (old style for OR), parens. */
function choose(expr) {
  const e = expr.replace(/\s*\/\s*/g, ' OR ').trim();
  const strip = (s) => s.replace(/^\((.*)\)$/, '$1').trim();
  const ors = splitTop(strip(e), ' OR ');
  for (const alt of ors) {
    const ands = splitTop(strip(alt), ' AND ').map(strip);
    if (ands.every((a) => (a.includes(' OR ') ? choose(a) : ALLOW.includes(a)))) return alt;
  }
  return null;
}
function splitTop(s, sep) {
  const out = []; let depth = 0, cur = '';
  for (let i = 0; i < s.length; i++) {
    if (s[i] === '(') depth++; else if (s[i] === ')') depth--;
    if (depth === 0 && s.startsWith(sep, i)) { out.push(cur); cur = ''; i += sep.length - 1; } else cur += s[i];
  }
  out.push(cur);
  return out.map((x) => x.trim()).filter(Boolean);
}

const meta = JSON.parse(execFileSync('cargo', ['metadata', '--format-version', '1', '--filter-platform', TARGET],
  { cwd: path.join(HERE, 'src-tauri'), env: { ...process.env, PATH: `${process.env.HOME}/.cargo/bin:${process.env.PATH}` }, maxBuffer: 1 << 28 }));
const pkgs = new Map(meta.packages.map((p) => [p.id, p]));
const nodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]));
// What links into the executable: cargo tree with real feature resolution, normal edges, nothing behind a proc macro.
const tree = execFileSync('cargo', ['tree', '-e', 'normal,no-proc-macro', '--target', TARGET, '--prefix', 'none', '--format', '{p}'],
  { cwd: path.join(HERE, 'src-tauri'), env: { ...process.env, PATH: `${process.env.HOME}/.cargo/bin:${process.env.PATH}` }, maxBuffer: 1 << 26 }).toString();
const want = new Set(tree.split('\n').map((l) => l.replace(/ \(\*\)$/, '').replace(/ \(.*\)$/, '').trim()).filter(Boolean));
const root = meta.resolve.root;
const shipped = new Set([...pkgs.values()].filter((p) => p.id !== root && want.has(`${p.name} v${p.version}`)).map((p) => p.id));

const rows = [];
const bad = [];
for (const id of shipped) {
  const p = pkgs.get(id);
  const local = !p.source; // our own crates (patches/)
  const lic = p.license || (p.license_file ? `file:${p.license_file}` : 'NONE');
  const pick = choose(lic);
  if (!pick) bad.push(`${p.name} ${p.version}: ${lic}`);
  const dir = path.dirname(p.manifest_path);
  const files = fs.readdirSync(dir).filter((f) => /^(LICEN[CS]E|COPYING|NOTICE|COPYRIGHT|UNLICENSE)/i.test(f)).sort();
  rows.push({ name: p.name, version: p.version, lic, pick, repo: p.repository || '', local, dir, files });
}
rows.sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));

const md = [];
md.push('# Third-party software in the CadCraft desktop shell', '');
md.push(`The desktop executable (target ${TARGET}) is built from these Rust crates. Each is used under the license shown`);
md.push('(for a choice of licenses, the one in "used under"); its own license and notice files follow. The app inside');
md.push('the install (`app/`) lists its own libraries in `app/THIRD_PARTY.md`. WebView2 is part of Windows and is not shipped.', '');
md.push('| Crate | Version | License | Used under |', '|---|---|---|---|');
for (const r of rows) md.push(`| ${r.name}${r.local ? ' (CadCraft)' : ''} | ${r.version} | ${r.lic} | ${r.pick || '**not allowed**'} |`);
md.push('');
for (const r of rows) {
  md.push(`## ${r.name} ${r.version}`, '', `License: ${r.lic}${r.repo ? ` · ${r.repo}` : ''}`, '');
  if (!r.files.length) md.push('(no license file in the crate; the license named above applies)', '');
  for (const f of r.files) md.push('```text', fs.readFileSync(path.join(r.dir, f), 'utf8').trim(), '```', '');
}
fs.writeFileSync(OUT, md.join('\n'));

console.log(`${rows.length} crates ship in the executable (${TARGET}); THIRD_PARTY.md written (${(fs.statSync(OUT).size / 1024).toFixed(0)} KB)`);
if (bad.length) { console.log('NOT ALLOWED:\n  ' + bad.join('\n  ')); process.exit(1); }
console.log('license gate: clean');
