/* SPDX-License-Identifier: Apache-2.0 */
/**
 * bridge.js — `window.cadcraftDesktop`, the desktop half of host.js (docs/0.4-PART2.md W.2; contract in
 * getcadcraft.com/public_html/assets/js/host.js). The shell runs this in the app's main frame before the page's own
 * scripts. It speaks only to the shell's own commands (src-tauri/src/files.rs); apps never see Tauri.
 *
 *   version 1
 *   openDialog({multiple, filters:[{name, extensions}], title}) → paths | null
 *   saveDialog({defaultPath, filters, title})                   → path | null
 *   openFolderDialog({title})                                   → path | null          (W2.15, sheet sets)
 *   readFile(path) → Uint8Array            writeFile(path, Uint8Array) → undefined (atomic, done by the shell)
 *   stat(path) → {size, mtime} | null      listDir(path) → [{name, dir}]  (folders first, then by name)
 *   launchFiles() → paths (each once)      onOpenFiles(cb(paths))          (double-click while running)
 *   info() → {shell, app, name, version, platform}
 *   account.status() / signIn() / cancel() / signOut() → {state: signed_out|waiting|signed_in, user{name,email},
 *     code, verificationUri, expiresAt, reason, message}       account.onChange(cb)   account.open() (the sign-in window)
 *   cloud.request({method, path, body, contentType}) → {status, headers, body: Uint8Array, json()}
 *     only /api/v1/sync/… and /api/v1/me; the shell adds the sign-in. The token never reaches the page (C.2).
 *   windows.open({app, paths?, search?}) → {id}  windows.list() → [{id, app, title, focused}]  windows.focus(id)
 *   windows.focusHub()  windows.onChange(cb({type: opened|closed|focus, id}))      suite mode only (Studio; hub opens)
 *   windows.closeSelf()  (an app window closes itself; the hub is closed by the person)
 *   control.start() → {url, port, token}  control.stop()  control.server() → {running, url, port, token}
 *   control.newToken() → token (drops every session)  control.dropSession(id)
 *   control.onRequest(cb(message, session {id}, reply(answer | null)))   Studio's local MCP server (X.5); hub window only
 *   updates.status() → {available (the build has a feed), auto}   updates.check() (asks before installing)
 *   updates.setAuto(bool)                                                                              (W.7)
 *
 * Errors reject with an Error whose `code` is not_allowed | not_found | permission_denied | error. The shell refuses
 * any path the person did not open, save or pick a folder for (not_allowed).
 */
(() => {
  'use strict';
  if (window.top !== window || window.cadcraftDesktop) return;
  const T = window.__TAURI_INTERNALS__;
  if (!T || typeof T.invoke !== 'function') return;

  const fail = (e) => {
    const msg = typeof e === 'string' ? e : (e && e.message) || String(e);
    const m = /^([a-z_]+): /.exec(msg);
    const err = new Error(msg);
    err.code = m ? m[1] : 'error';
    throw err;
  };
  const call = (cmd, args, opts) => T.invoke(cmd, args, opts).catch(fail);
  const bytesOf = (data) => {
    if (data instanceof Uint8Array) return data;
    if (ArrayBuffer.isView(data)) return new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
    if (data instanceof ArrayBuffer) return new Uint8Array(data);
    throw new TypeError('writeFile needs bytes (Uint8Array)');
  };

  let windowsListener = null;
  Object.defineProperty(window, '__ccDesktopWindows', { value: (ev) => { if (windowsListener) windowsListener(ev); } });

  let controlListener = null;
  Object.defineProperty(window, '__ccDesktopControl', {
    value: (id, msg, session) => {
      let done = false;
      const reply = (answer) => {
        if (done) return;
        done = true;
        T.invoke('cc_control_reply', { id, answer: answer === undefined ? null : answer }).catch(() => {});
      };
      if (!controlListener) { done = true; T.invoke('cc_control_reply', { id, answer: null, missing: true }).catch(() => {}); return; }
      try { controlListener(msg, session, reply); } catch (e) {
        reply({ jsonrpc: '2.0', id: msg && msg.id !== undefined ? msg.id : null, error: { code: -32603, message: 'Internal error' } });
      }
    },
  });

  let accountListener = null;
  Object.defineProperty(window, '__ccDesktopAccount', { value: (s) => { if (accountListener) accountListener(s); } });

  const cloudRequest = async (o = {}) => {
    const method = String(o.method || 'GET').toUpperCase();
    let body = o.body === undefined || o.body === null ? new Uint8Array(0) : o.body;
    let ctype = o.contentType || 'application/octet-stream';
    if (typeof body === 'string') { body = new TextEncoder().encode(body); ctype = o.contentType || 'text/plain; charset=utf-8'; }
    else if (!(body instanceof Uint8Array || ArrayBuffer.isView(body) || body instanceof ArrayBuffer)) {
      body = new TextEncoder().encode(JSON.stringify(body)); ctype = 'application/json';
    }
    const raw = await call('cc_cloud_request', bytesOf(body), { headers: { 'x-cc-method': method, 'x-cc-path': String(o.path || ''), 'x-cc-content-type': ctype } });
    const u8 = raw instanceof Uint8Array ? raw : new Uint8Array(raw);
    const dv = new DataView(u8.buffer, u8.byteOffset, u8.byteLength);
    const status = dv.getUint16(0);
    const n = dv.getUint32(2);
    const headers = JSON.parse(new TextDecoder().decode(u8.subarray(6, 6 + n)));
    const out = u8.slice(6 + n);
    return { status, headers, body: out, json: () => JSON.parse(new TextDecoder().decode(out)) };
  };

  let listener = null;
  const waiting = [];
  Object.defineProperty(window, '__ccDesktopOpen', {
    value: (paths) => {
      if (!Array.isArray(paths) || !paths.length) return;
      if (listener) listener(paths.slice()); else waiting.push(paths.slice());
    },
  });

  const bridge = {
    version: 1,
    openDialog: (o = {}) => call('cc_open_dialog', { opts: { multiple: !!o.multiple, filters: o.filters || [], title: o.title || null } }),
    saveDialog: (o = {}) => call('cc_save_dialog', { opts: { defaultPath: o.defaultPath || null, filters: o.filters || [], title: o.title || null } }),
    openFolderDialog: (o = {}) => call('cc_open_folder_dialog', { opts: { title: o.title || null } }),
    readFile: async (path) => {
      const r = await call('cc_read_file', { path: String(path) });
      return r instanceof Uint8Array ? r : new Uint8Array(r instanceof ArrayBuffer ? r : r);
    },
    writeFile: async (path, data) => {
      await call('cc_write_file', bytesOf(data), { headers: { 'x-cc-path': encodeURIComponent(String(path)) } });
    },
    stat: (path) => call('cc_stat', { path: String(path) }),
    listDir: (path) => call('cc_list_dir', { path: String(path) }),
    launchFiles: () => call('cc_launch_files'),
    onOpenFiles: (cb) => {
      listener = typeof cb === 'function' ? cb : null;
      while (listener && waiting.length) listener(waiting.shift());
    },
    info: () => call('cc_info'),
    account: Object.freeze({
      status: () => call('cc_account_status'),
      signIn: () => call('cc_account_sign_in'),
      cancel: () => call('cc_account_cancel'),
      signOut: () => call('cc_account_sign_out'),
      openDevicePage: () => call('cc_account_open_device'),
      open: () => call('cc_account_window'),
      closeWindow: () => call('cc_close_window'),
      onChange: (cb) => { accountListener = typeof cb === 'function' ? cb : null; },
    }),
    cloud: Object.freeze({ request: cloudRequest }),
    windows: Object.freeze({
      open: (o = {}) => call('cc_windows_open', { args: { app: String(o.app || ''), paths: (o.paths || []).map(String), search: String(o.search || '') } }),
      list: () => call('cc_windows_list'),
      focus: (id) => call('cc_windows_focus', { id: String(id) }),
      focusHub: () => call('cc_windows_focus_hub'),
      closeSelf: () => call('cc_close_window'),
      onChange: (cb) => { windowsListener = typeof cb === 'function' ? cb : null; },
    }),
    control: Object.freeze({
      start: () => call('cc_control_start'),
      stop: () => call('cc_control_stop'),
      server: () => call('cc_control_server'),
      newToken: () => call('cc_control_new_token'),
      dropSession: (id) => call('cc_control_drop_session', { id: String(id) }),
      onRequest: (cb) => { controlListener = typeof cb === 'function' ? cb : null; },
    }),
    updates: Object.freeze({
      status: () => call('cc_updates_status'),
      check: () => call('cc_updates_check'),
      setAuto: (on) => call('cc_updates_set_auto', { on: !!on }),
    }),
  };
  // A build without sign-in (store builds, shell.json account:false) has no account or cloud at all.
  const flags = window.__ccShellFlags || {};
  if (flags.account === false) { delete bridge.account; delete bridge.cloud; }
  if (flags.suite !== true) { delete bridge.windows; delete bridge.control; }
  Object.defineProperty(window, 'cadcraftDesktop', { value: Object.freeze(bridge), enumerable: true });

  // Element fullscreen (the app bar's screen modes) fills only the webview unless the window goes fullscreen with it.
  document.addEventListener('fullscreenchange', () => {
    T.invoke('cc_set_fullscreen', { on: !!document.fullscreenElement }).catch(() => {});
  });
})();
