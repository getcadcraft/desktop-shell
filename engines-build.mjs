// SPDX-License-Identifier: Apache-2.0
// engines-build.mjs — the opt-in outside-program engines of a Studio build (0.8 RE.3 / RE.4; owner 2026-10-29: "build them opt-in").
//   node build.mjs studio --edition <suite> --with-engines blender[,cantera]
// Off in every build we ship: engines.rs only knows an engine whose cargo feature is on (Blender is GPL: the program is never ours to bundle, and
// the scripts that drive it are shipped only in a build that asks for it). This file turns the list into what build.mjs needs: the cargo features
// to switch on, and the resource files (the engine's scripts, from ./engines/<id>/) that go to <resources>/engines/<id>/ — the only place
// engines.rs runs a script from. Problems come back as words; build.mjs stops on any.
//   planEngines(list, { root, cargoToml }) → { features: [id], resources: { '<path relative to src-tauri>': 'engines/<id>/<file>' }, problems: [text] }
import fs from 'node:fs';
import path from 'node:path';

/** id → the scripts that must be in engines/<id>/ (copied from the site by tools/render/engines/sync.mjs). */
export const ENGINES = {
  blender: { feature: 'blender', files: ['cc_blender_render.py', 'cc_blender_open.py'] },
  cantera: { feature: 'cantera', files: ['cc_cantera_equilibrate.py'] },
};

export function planEngines(list, { root, cargoToml = path.join(root, 'src-tauri/Cargo.toml') } = {}) {
  const out = { features: [], resources: {}, problems: [] };
  const toml = fs.existsSync(cargoToml) ? fs.readFileSync(cargoToml, 'utf8') : '';
  const featureBlock = (toml.match(/\[features\]([\s\S]*?)(\n\[|$)/) || [, ''])[1];
  for (const id of [...new Set(list)]) {
    const e = ENGINES[id];
    if (!e) { out.problems.push(`--with-engines: unknown engine "${id}" (known: ${Object.keys(ENGINES).join(', ')})`); continue; }
    if (!new RegExp(`^\\s*${e.feature}\\s*=`, 'm').test(featureBlock)) { out.problems.push(`--with-engines ${id}: src-tauri/Cargo.toml has no cargo feature "${e.feature}"`); continue; }
    const missing = e.files.filter((f) => !fs.existsSync(path.join(root, 'engines', id, f)));
    if (missing.length) { out.problems.push(`--with-engines ${id}: engines/${id}/ lacks ${missing.join(', ')} (copy them from the site: node tools/render/engines/sync.mjs)`); continue; }
    out.features.push(e.feature);
    for (const f of e.files) out.resources[path.relative(path.join(root, 'src-tauri'), path.join(root, 'engines', id, f)).split(path.sep).join('/')] = `engines/${id}/${f}`;
  }
  return out;
}
