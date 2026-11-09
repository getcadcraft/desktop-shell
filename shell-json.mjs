// SPDX-License-Identifier: Apache-2.0
// shell.json for a desktop build (build.mjs writes it beside app/; src-tauri/src/lib.rs ShellConfig reads it).
//   {app, name, version, headers, updates?, account?, suite?, store?, licence?}
// `licence: "supercad"` (SuperCad Studio's sign-in check, src-tauri/src/licence.rs) is written for supercadcraft ONLY:
// CadCraft Studio, the eight free apps and their store builds never get the key.

/** Which products carry a licence check, and which kind. */
export const LICENCE = Object.freeze({ supercadcraft: 'supercad' });

export function shellJson({ slug, name, version, headers = {}, updates, account, suite, store }) {
  return {
    app: slug, name, version, headers, updates, account,
    suite: suite || undefined, store: store || undefined,
    licence: Object.hasOwn(LICENCE, slug) ? LICENCE[slug] : undefined,
  };
}
