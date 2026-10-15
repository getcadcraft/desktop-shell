/* SPDX-License-Identifier: Apache-2.0 */
/**
 * shell-i18n.js — words for the desktop shell's own pages (.shell/account.html, .shell/carry.html), served from the
 * executable with lang/<locale>.json (protocol.rs). The language: Studio's own choice (localStorage
 * cadcraft.studio.lang, set in Studio's Settings; same origin), else the apps' (cookie cc_lang), else the system's,
 * matched exactly then by language alone; else English. A key missing from a translation falls back to English.
 *
 *   window.ccShellI18n.ready → Promise<{t(key, fills?), locale}>; elements with data-t="key" are filled in.
 *   Plurals: an object chosen by the fill n: "=<n>" first, then the locale's category ("one", "other").
 */
(() => {
  'use strict';
  const AVAILABLE = ['en', 'es-MX'];
  const match = (tag) => {
    if (!tag) return null;
    const t = String(tag).trim();
    return AVAILABLE.find((a) => a.toLowerCase() === t.toLowerCase())
      || AVAILABLE.find((a) => a.split('-')[0].toLowerCase() === t.split('-')[0].toLowerCase()) || null;
  };
  const cookie = () => { const m = document.cookie.match(/(?:^|;\s*)cc_lang=([^;]*)/); return m ? decodeURIComponent(m[1]) : null; };
  let stored = null;
  try { stored = localStorage.getItem('cadcraft.studio.lang'); } catch { /* private storage */ }
  const locale = [stored, cookie(), ...(navigator.languages || [navigator.language])].map(match).find(Boolean) || 'en';
  const load = (l) => fetch(`/.shell/lang/${l}.json`).then((r) => (r.ok ? r.json() : {})).catch(() => ({}));
  const ready = Promise.all([load('en'), locale === 'en' ? {} : load(locale)]).then(([en, own]) => {
    const strings = { ...en, ...own };
    const rules = new Intl.PluralRules(locale);
    const t = (key, fills = {}) => {
      let v = strings[key];
      if (v === undefined || v === null) return key;
      if (typeof v === 'object') v = v['=' + fills.n] ?? v[rules.select(Number(fills.n))] ?? v.other ?? key;
      return String(v).replace(/\{(\w+)\}/g, (m, k) => (k in fills ? String(fills[k]) : m));
    };
    document.documentElement.lang = locale;
    document.querySelectorAll('[data-t]').forEach((n) => { n.textContent = t(n.getAttribute('data-t')); });
    return { t, locale };
  });
  Object.defineProperty(window, 'ccShellI18n', { value: Object.freeze({ ready }) });
})();
