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
 *   info() → {shell, app, name, version, platform, windowId, suite, store, portable}   store: a store package (the store updates it); portable: the zip (no updater, data beside the program)
 *   account.status() / signIn() / cancel() / signOut() → {state: signed_out|waiting|signed_in, user{name,email},
 *     code, verificationUri, expiresAt, reason, message}       account.onChange(cb)   account.open() (the sign-in window)
 *   cloud.request({method, path, body, contentType}) → {status, headers, body: Uint8Array, json()}
 *     only /api/v1/sync/…, /api/v1/me, /api/v1/me/avatar and /api/v1/live/{check,rooms,end,assertion}; the shell adds the
 *     sign-in. The token never reaches the page (C.2).
 *   windows.open({app, paths?, search?}) → {id}  windows.list() → [{id, app, title, focused}]  windows.focus(id)
 *   windows.focusHub()  windows.onChange(cb({type: opened|closed|focus, id}))      suite mode only (Studio; hub opens)
 *   windows.closeSelf()  (an app window closes itself; the hub is closed by the person)
 *   windows.launchApps() → [slug] (each once)   windows.onLaunchApp(cb(slug))   hub only: the per-app shortcuts
 *     ("CadCraft", … in the Start menu / app menu, `--app=<slug>`, SD.6) open Studio straight into that app
 *   control.start() → {url, port, token}  control.stop()  control.server() → {running, url, port, token}
 *   control.newToken() → token (drops every session)  control.dropSession(id)
 *   control.onRequest(cb(message, session {id}, reply(answer | null)))   Studio's local MCP server (X.5); hub window only
 *   links.launch() → ['ccstudio://join/<ROOM>#<secret>', …] (each once)   links.onOpen(cb(url))   Studio's hub only:
 *     invite links the system opened Studio with (LV.25), untouched; the fragment is the session key, never log it
 *   printers {list() → {accepts: 'pdf'|'png', printers: [{name, default, papers[]}]}, print(bytes, {printer, copies, paper, landscape, title})}
 *                 (0.7 D7.20, print.rs; computers only)
 *   renderNodes (LAN render nodes, render_nodes.rs, 0.7 CU.7; Studio's app windows on computers; the shell finds nodes and
 *     carries bytes, the pages do the handshake): advertise(tag) → stopAdvertising() → onConnection(cb(transport)) →
 *     browse() → [{tag, name, host, port}] · onNodes(cb(list)) → unsubscribe · connect({host, port}) → transport
 *     transport = {send(Uint8Array), close(), onmessage, onclose}; mDNS `_ccrender._tcp` (the 2-character tag only),
 *     `ws://<private address>:<port>/render`, ≤ 8 MiB a message, one peer at a time per node
 *   updates.status() → {available (this copy has an update source), auto, autoAllowed, source (feed | notify | package),
 *     route (store | appinstaller | sideloaded: an MSIX), edition, version, platform, busy, last {at, outcome, latest, notes}}
 *     updates.check() (asks before installing; 'current' | 'declined' | 'download' | 'installing' | …); a failure's
 *     `err.code`: no_build | network | feed | signature | install | not_store (the hub words each; D7.54, 0.7 MSIX)
 *   updates.setAuto(bool)                                                                              (W.7)
 *   live (Studio Live, studio_live.rs; Studio builds with the `live` feature; app windows, browse also the hub):
 *     transport('local' | 'join', {app, schema, url}) → a live-client transport ((handlers) → {send(bytes), close()})
 *       'local' = this window's own session (started on first use), 'join' = a socket the shell opens (ws://<lan>:<port>/live
 *       or the site's relay join URL; pages from Studio's origin may not open LAN sockets themselves). encrypts: false.
 *     session() → {running, port, addrs, app}   advertise(tag | {room}, {app})   unadvertise(tag | {room})
 *     browse(tag | {room}, ms) → [{addr, port, app}]   relay(relayUrl, hostTicket)   relayStop()
 *     exportState() → Uint8Array   startFrom(bytes) → session()      host handoff (LV.17)
 *     signIn(aud) → {assertion, picture} | null   the signed-in name for a joiner's hello (LV.19, SI.8), via cloud.request
 *
 * Errors reject with an Error whose `code` is not_allowed | not_found | permission_denied | error. The shell refuses
 * any path the person did not open, save or pick a folder for (not_allowed).
 *
 * Tablets (docs/0.5-PART3.md TB.1; flags.mobile): Studio on Android and iPad has ONE web view. The hub is the page, and
 * `windows.open()` shows the app as a full-screen view inside it (a same-origin iframe) instead of a window; the views
 * are this file's own (`views` below), so the hub and the apps are unchanged:
 *   windows.open/list/focus/focusHub/onChange as on the desktop (ids `app-<slug>-<n>`), windows.close(id) (tablets only),
 *   the system Back gesture leaves a view for the hub (one history entry while a view shows), an app's
 *   windows.focusHub()/closeSelf() from inside it, account.open() shows the sign-in page as a view.
 * Inside a view the bridge talks to the shell through the top frame and knows its own id (info().windowId, launchFiles()
 * = what the hub opened it with). Files are documents there, never paths: `cc-doc:/<key>/<name>` (files.rs). There is no
 * updater (the store updates Studio), no local MCP server (control) and no pop-up windows; `<a download>` asks where to
 * save, and links that leave Studio open in the person's browser. `live` works in each view with its own id (TB.5).
 */
(() => {
  'use strict';
  const flags = window.__ccShellFlags || {};
  const mobile = flags.mobile === true;
  // Already here: a frame's window outlives its first (about:blank) document, so a new document only needs its listeners.
  if (window.cadcraftDesktop) { if (typeof window.__ccDocHooks === 'function') window.__ccDocHooks(); return; }
  // A view inside the hub's page (tablets): only one level deep, only under our own top frame.
  let top = null;
  if (window.top !== window) {
    if (!mobile || window.parent !== window.top) return;
    try { top = window.top.__ccViews || null; } catch { top = null; }
    if (!top) return;
  }
  const T = top ? window.top.__TAURI_INTERNALS__ : window.__TAURI_INTERNALS__;
  if (!T || typeof T.invoke !== 'function') return;

  const fail = (e) => {
    const msg = typeof e === 'string' ? e : (e && e.message) || String(e);
    const m = /^([a-z_]+): /.exec(msg);
    const err = new Error(msg);
    err.code = m ? m[1] : 'error';
    throw err;
  };
  // Tablets (TB.5): a view's Studio Live commands carry its own id, since every view shares the one window's label.
  let liveView = null;
  const inv = (cmd, args, opts) => {
    const v = liveView && cmd.startsWith('cc_live_') ? liveView() : null;
    if (v) {
      if (ArrayBuffer.isView(args) || (args && typeof args.b64 === 'string')) opts = Object.assign({}, opts, { headers: Object.assign({}, opts && opts.headers, { 'x-cc-view': v }) });
      else args = Object.assign({}, args, { view: v });
    }
    return T.invoke(cmd, args, opts);
  };
  const call = (cmd, args, opts) => inv(cmd, args, opts).catch(fail);
  // Bytes for a raw-body command. Tablets: Tauri's IPC on Android carries JSON only (its WebView can't read a request
  // body), so bytes go as base64 there (files.rs body_bytes); the desktop sends them as they are.
  const toB64 = (u8) => {
    let s = '';
    for (let i = 0; i < u8.length; i += 0x8000) s += String.fromCharCode.apply(null, u8.subarray(i, i + 0x8000));
    return btoa(s);
  };
  const wire = (u8) => (mobile ? { b64: toB64(u8) } : u8);
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
  let forward = null;   // tablets: the top frame passes account changes on to its views (hubSide)
  Object.defineProperty(window, '__ccDesktopAccount', { value: (s) => { if (accountListener) accountListener(s); if (forward) forward(s); } });

  const cloudRequest = async (o = {}) => {
    const method = String(o.method || 'GET').toUpperCase();
    let body = o.body === undefined || o.body === null ? new Uint8Array(0) : o.body;
    let ctype = o.contentType || 'application/octet-stream';
    if (typeof body === 'string') { body = new TextEncoder().encode(body); ctype = o.contentType || 'text/plain; charset=utf-8'; }
    else if (!(body instanceof Uint8Array || ArrayBuffer.isView(body) || body instanceof ArrayBuffer)) {
      body = new TextEncoder().encode(JSON.stringify(body)); ctype = 'application/json';
    }
    const raw = await call('cc_cloud_request', wire(bytesOf(body)), { headers: { 'x-cc-method': method, 'x-cc-path': String(o.path || ''), 'x-cc-content-type': ctype } });
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

  // Studio: ccstudio:// join links (links.rs) — handed to the hub untouched; never logged.
  let linkListener = null;
  const linksWaiting = [];
  Object.defineProperty(window, '__ccDesktopLink', {
    value: (urls) => {
      if (!Array.isArray(urls) || !urls.length) return;
      if (linkListener) urls.forEach((u) => linkListener(String(u))); else linksWaiting.push(...urls.map(String));
    },
  });

  // Studio: apps named by a per-app shortcut (`--app=<slug>`, SD.6), for the hub to open.
  let appListener = null;
  const appsWaiting = [];
  Object.defineProperty(window, '__ccDesktopApp', {
    value: (slugs) => {
      if (!Array.isArray(slugs) || !slugs.length) return;
      if (appListener) slugs.forEach((a) => appListener(String(a))); else appsWaiting.push(...slugs.map(String));
    },
  });

  // ---- Studio Live (studio_live.rs). The page's sends go out one batch at a time (order matters: frames are counted), and
  // the shell answers a long poll with whatever arrived: records kind (0 open, 1 message, 2 close) ‖ u32 length ‖ payload.
  const adArgs = (tag) => (tag && typeof tag === 'object' ? { room: String(tag.room || '') } : { tag: String(tag || '') });
  const liveTransport = (kind, o) => (h) => {
    let id = null, closed = false, busy = false, opened = false;
    const out = [];
    const flush = () => {
      if (busy || closed || id === null || !out.length) return;
      busy = true;
      const batch = out.splice(0);
      const body = new Uint8Array(batch.reduce((n, b) => n + 4 + b.length, 0));
      const dv = new DataView(body.buffer);
      let p = 0;
      for (const b of batch) { dv.setUint32(p, b.length); body.set(b, p + 4); p += 4 + b.length; }
      inv('cc_live_send', wire(body), { headers: { 'x-cc-live': String(id) } }).catch(() => {}).finally(() => { busy = false; flush(); });
    };
    // The other end closed: the shell forgets the connection too (for the window's own session, that ends it).
    const end = (reason, code) => { if (closed) return; closed = true; if (id !== null) inv('cc_live_close', { id }).catch(() => {}); h.close(reason || '', code || 0); };
    (async () => {
      try {
        id = await inv('cc_live_open', { args: { kind: String(kind), url: String(o.url || ''), app: String(o.app || ''), schema: o.schema === undefined ? null : Number(o.schema) } });
      } catch (e) { end(String((e && e.message) || e), 0); return; }
      if (closed) { inv('cc_live_close', { id }).catch(() => {}); return; }
      flush();
      while (!closed) {
        let raw;
        try { raw = await inv('cc_live_recv', { id }); } catch (e) { end('gone', 0); return; }
        const u8 = raw instanceof Uint8Array ? raw : new Uint8Array(raw);
        const dv = new DataView(u8.buffer, u8.byteOffset, u8.byteLength);
        for (let p = 0; p + 5 <= u8.length && !closed;) {
          const k = u8[p], n = dv.getUint32(p + 1), body = u8.slice(p + 5, p + 5 + n);
          p += 5 + n;
          if (k === 0) { opened = true; h.open(); }
          else if (k === 1) h.message(body);
          else { let c = {}; try { c = JSON.parse(new TextDecoder().decode(body)); } catch { c = {}; } end(c.reason, c.code); }
        }
      }
    })();
    return {
      send(b) { if (!closed) { out.push(bytesOf(b).slice()); flush(); } },
      close() { if (closed) return; closed = true; if (id !== null) inv('cc_live_close', { id }).catch(() => {}); },
      get connecting() { return !opened && !closed; },
    };
  };
  // ---- LAN render nodes (render_nodes.rs, 0.7 CU.7). The shell finds nodes and carries bytes; the pages do the handshake and
  // the encryption. A transport is {send(Uint8Array), close(), onmessage, onclose}: messages that arrive before onmessage is
  // set wait for it, and onclose fires once, after the last message. Sends go out one at a time (order matters).
  const renderTransport = (id) => {
    let closed = false, closeFired = false, busy = false, onmessage = null, onclose = null;
    const out = [], inbox = [];
    const deliver = () => {
      while (onmessage && inbox.length) { try { onmessage(inbox.shift()); } catch { /* the page's handler's problem */ } }
      if (closed && !inbox.length && onclose && !closeFired) { closeFired = true; try { onclose(); } catch { /* same */ } }
    };
    const flush = () => {
      if (busy || closed || !out.length) return;
      busy = true;
      inv('cc_render_send', wire(out.shift()), { headers: { 'x-cc-render': String(id) } }).catch(() => end()).finally(() => { busy = false; flush(); });
    };
    const end = () => { if (closed) return; closed = true; inv('cc_render_close', { id }).catch(() => {}); deliver(); };
    (async () => {
      while (!closed) {
        let raw;
        try { raw = await inv('cc_render_recv', { id }); } catch { end(); return; }
        const u8 = raw instanceof Uint8Array ? raw : new Uint8Array(raw);
        const dv = new DataView(u8.buffer, u8.byteOffset, u8.byteLength);
        for (let p = 0; p + 5 <= u8.length && !closed;) {
          const k = u8[p], n = dv.getUint32(p + 1), body = u8.slice(p + 5, p + 5 + n);
          p += 5 + n;
          if (k === 1) { inbox.push(body); deliver(); } else if (k === 2) { end(); return; }
        }
      }
    })();
    return {
      send(b) { if (!closed) { out.push(bytesOf(b).slice()); flush(); } },
      close() { if (closed) return; closed = true; inv('cc_render_close', { id }).catch(() => {}); deliver(); },
      get onmessage() { return onmessage; }, set onmessage(f) { onmessage = typeof f === 'function' ? f : null; deliver(); },
      get onclose() { return onclose; }, set onclose(f) { onclose = typeof f === 'function' ? f : null; deliver(); },
    };
  };
  const renderNodes = (() => {
    const conns = new Set(), waiting = [], nodeSubs = new Set();
    let acceptOn = false, poll = 0, last = '';
    const hand = (t) => { if (conns.size) conns.forEach((cb) => { try { cb(t); } catch { /* the page's problem */ } }); else waiting.push(t); };
    const acceptLoop = async () => {
      if (acceptOn) return;
      acceptOn = true;
      try { for (;;) { const id = await inv('cc_render_accept'); if (id === null || id === undefined) break; hand(renderTransport(id)); } } catch { /* the node stopped */ }
      acceptOn = false;
    };
    const browse = async (ms = 1500) => {
      const list = await call('cc_render_browse', { args: { ms: Math.max(100, Math.min(5000, Number(ms) || 1500)) } });
      return Array.isArray(list) ? list : [];
    };
    const tick = async () => {
      let list = [];
      try { list = await browse(); } catch { list = []; }
      const key = JSON.stringify(list);
      if (key !== last) { last = key; nodeSubs.forEach((cb) => { try { cb(list); } catch { /* same */ } }); }
    };
    return Object.freeze({
      advertise: async (tag) => { await call('cc_render_advertise', { args: { tag: String(tag || '') } }); acceptLoop(); },
      stopAdvertising: () => call('cc_render_unadvertise'),
      onConnection: (cb) => {
        if (typeof cb !== 'function') return () => {};
        conns.add(cb);
        while (waiting.length) cb(waiting.shift());
        return () => conns.delete(cb);
      },
      browse: () => browse(),
      onNodes: (cb) => {
        if (typeof cb !== 'function') return () => {};
        nodeSubs.add(cb);
        if (!poll) { poll = setInterval(tick, 5000); }
        tick();
        return () => { nodeSubs.delete(cb); if (!nodeSubs.size && poll) { clearInterval(poll); poll = 0; last = ''; } };
      },
      connect: async (o = {}) => renderTransport(await call('cc_render_connect', { args: { host: String((o && o.host) || ''), port: Number(o && o.port) || 0 } })),
    });
  })();
  // LV.19 / SI.8: the site signs this connection's audience for the signed-in account; the 64 × 64 picture travels as its
  // exact bytes (its sha256 is in the assertion). Signed out, offline or refused: null (the host sees the typed name).
  const liveSignIn = async (aud) => {
    try {
      const r = await cloudRequest({ method: 'POST', path: '/api/v1/live/assertion', body: { aud: String(aud) } });
      if (r.status !== 200) return null;
      const j = r.json();
      const d = j && j.data ? j.data : j;
      if (!d || typeof d.assertion !== 'string') return null;
      let picture = null;
      if (d.picture_sha256) {
        const p = await cloudRequest({ method: 'GET', path: '/api/v1/me/avatar?size=64' }).catch(() => null);
        if (p && p.status === 200 && p.body.length && p.body.length <= 16384) picture = p.body;
      }
      return { assertion: d.assertion, picture };
    } catch { return null; }
  };

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
      await call('cc_write_file', wire(bytesOf(data)), { headers: { 'x-cc-path': encodeURIComponent(String(path)) } });
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
    links: Object.freeze({
      launch: () => call('cc_links_launch'),
      onOpen: (cb) => {
        linkListener = typeof cb === 'function' ? cb : null;
        while (linkListener && linksWaiting.length) linkListener(linksWaiting.shift());
      },
    }),
    windows: Object.freeze({
      open: (o = {}) => call('cc_windows_open', { args: { app: String(o.app || ''), paths: (o.paths || []).map(String), search: String(o.search || '') } }),
      list: () => call('cc_windows_list'),
      focus: (id) => call('cc_windows_focus', { id: String(id) }),
      focusHub: () => call('cc_windows_focus_hub'),
      closeSelf: () => call('cc_close_window'),
      onChange: (cb) => { windowsListener = typeof cb === 'function' ? cb : null; },
      launchApps: () => call('cc_launch_apps'),
      onLaunchApp: (cb) => {
        appListener = typeof cb === 'function' ? cb : null;
        while (appListener && appsWaiting.length) appListener(appsWaiting.shift());
      },
    }),
    control: Object.freeze({
      start: () => call('cc_control_start'),
      stop: () => call('cc_control_stop'),
      server: () => call('cc_control_server'),
      newToken: () => call('cc_control_new_token'),
      dropSession: (id) => call('cc_control_drop_session', { id: String(id) }),
      onRequest: (cb) => { controlListener = typeof cb === 'function' ? cb : null; },
    }),
    live: Object.freeze({
      encrypts: false,
      transport: (kind, o = {}) => liveTransport(kind, o),
      session: () => call('cc_live_session'),
      advertise: (tag, o = {}) => call('cc_live_advertise', { args: Object.assign(adArgs(tag), { app: String(o.app || '') }) }),
      unadvertise: (tag) => call('cc_live_unadvertise', { args: adArgs(tag) }),
      browse: (tag, ms = 1500) => call('cc_live_browse', { args: Object.assign(adArgs(tag), { ms: Math.max(100, Math.min(5000, Number(ms) || 1500)) }) }),
      relay: (url, ticket) => call('cc_live_relay', { args: { url: String(url || ''), ticket: String(ticket || '') } }),
      relayStop: () => call('cc_live_relay_stop'),
      exportState: async () => { const r = await call('cc_live_export'); return r instanceof Uint8Array ? r : new Uint8Array(r); },
      startFrom: (bytes) => call('cc_live_start_from', wire(bytesOf(bytes))),
      signIn: liveSignIn,
    }),
    renderNodes,
    // Printing (0.7 D7.20, print.rs): the system's printers, and a plotted sheet sent to one. `accepts` says what to send:
    // 'pdf' (CUPS: Linux, macOS) or 'png' (Windows prints an image through .NET).
    printers: Object.freeze({
      list: () => call('cc_printers_list'),
      print: (bytes, job = {}) => call('cc_print', wire(bytesOf(bytes)), { headers: { 'x-cc-print': encodeURIComponent(JSON.stringify({
        printer: String(job.printer || ''), copies: Math.max(1, Math.min(99, Math.round(Number(job.copies) || 1))), paper: String(job.paper || ''),
        landscape: !!job.landscape, title: String(job.title || '') })) } }),
    }),
    updates: Object.freeze({
      status: () => call('cc_updates_status'),
      check: () => call('cc_updates_check'),
      setAuto: (on) => call('cc_updates_set_auto', { on: !!on }),
    }),
  };
  // Tablets: no updater (the store updates it), no local MCP server, Live with TB.5; views instead of windows.
  if (mobile) {
    delete bridge.updates;
    delete bridge.printers;
    delete bridge.control;
    tabletBridge(bridge);
  }
  // A build without sign-in (store builds, shell.json account:false) has no account or cloud at all.
  if (flags.account === false) { delete bridge.account; delete bridge.cloud; }
  if (flags.suite !== true) { delete bridge.windows; delete bridge.control; delete bridge.links; }
  if (flags.suite !== true || flags.live !== true || mobile) delete bridge.renderNodes;   // computers only (CU.7)
  if (flags.suite !== true || flags.live !== true) delete bridge.live;
  else if (flags.account === false && bridge.live) { const { signIn, ...rest } = bridge.live; bridge.live = Object.freeze(rest); }   // eslint-disable-line no-unused-vars
  Object.defineProperty(window, 'cadcraftDesktop', { value: Object.freeze(bridge), enumerable: true });

  if (mobile) return;

  // Full screen (the app bar's screen modes, site assets/js/app-shell.js). Windows and Linux: element fullscreen fills
  // only the web view, so the window follows it. macOS (flags.nativeFullscreen; GH.1, desktop-shell#1): WKWebView's
  // element fullscreen already puts the page in a full-screen window of WebKit's own, and taking the app's window full
  // screen on top of that made WebKit leave again at once (the in-app button went in and straight back out; the green
  // button, the window's own full screen, worked). So there the page's request goes to the window's own full screen
  // instead, reported back through the standard API (nativeFullscreen below).
  if (flags.nativeFullscreen === true) nativeFullscreen();
  else {
    document.addEventListener('fullscreenchange', () => {
      T.invoke('cc_set_fullscreen', { on: !!document.fullscreenElement }).catch(() => {});
    });
  }

  // window.open('') means about:blank (HTML spec), and the panels window opens that way. WebKitGTK hands the shell the
  // empty string, which is not a URL, so the shell refused the window on Linux (found with DR.5). Say it in full.
  const open = window.open;
  window.open = function (url, ...rest) {
    return open.call(this, url === undefined || url === null || url === '' ? 'about:blank' : url, ...rest);
  };

  // ---- macOS full screen (GH.1) ----------------------------------------------------------------------------------------
  // documentElement.requestFullscreen() → the window's own full screen (cc_set_fullscreen, the green button's path),
  // resolved once the window says it is full screen; then document.fullscreenElement is the document element and
  // fullscreenchange fires, as in a browser. exitFullscreen() and Esc leave it. Leaving any other way (the green button,
  // Window › Exit Full Screen, Ctrl+Cmd+F) is noticed on the next resize (or within a second) and reported the same way,
  // so app-shell.js drops back to normal. Other elements keep WebKit's own element fullscreen (no app uses one).
  function nativeFullscreen() {
    const EP = window.Element && window.Element.prototype;
    const DP = window.Document && window.Document.prototype;
    if (!EP || !DP) return;
    const doc = document;
    const ownRequest = EP.requestFullscreen;
    const ownExit = DP.exitFullscreen;
    const ownElement = Object.getOwnPropertyDescriptor(DP, 'fullscreenElement');
    const webkitElement = () => (ownElement && ownElement.get ? ownElement.get.call(doc) : null);
    let full = null;      // the document element while the window is full screen for the page
    let asking = null;    // a request on its way (one at a time)
    let poll = 0;
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    const isFull = () => T.invoke('cc_is_fullscreen').then((v) => v === true, () => false);
    const changed = () => {
      const root = doc.documentElement;
      const ev = new Event('fullscreenchange', { bubbles: true });
      if (root) root.dispatchEvent(ev); else doc.dispatchEvent(ev);
    };
    const drop = () => {
      if (!full) return;
      full = null;
      clearInterval(poll);
      poll = 0;
      changed();
    };
    const check = async () => { if (full && !asking && !(await isFull())) drop(); };

    EP.requestFullscreen = function (opts) {
      if (this !== doc.documentElement) {
        return ownRequest ? ownRequest.call(this, opts) : Promise.reject(new TypeError('Full screen is not available'));
      }
      if (full) return Promise.resolve();
      if (asking) return asking;
      asking = (async () => {
        try {
          await T.invoke('cc_set_fullscreen', { on: true });
          let on = false;
          for (let i = 0; i < 40 && !on; i++) { on = await isFull(); if (!on) await sleep(75); }   // ≤ 3 s
          if (!on) throw new TypeError('The window did not go full screen');
          full = this;
          poll = setInterval(check, 1000);
          changed();
        } finally {
          asking = null;
        }
      })();
      return asking;
    };
    DP.exitFullscreen = function () {
      if (this !== doc || !full) {
        return ownExit && webkitElement() ? ownExit.call(this) : Promise.reject(new TypeError('Not in full screen'));
      }
      drop();
      return T.invoke('cc_set_fullscreen', { on: false }).then(() => undefined, () => undefined);
    };
    Object.defineProperty(DP, 'fullscreenElement', {
      configurable: true,
      enumerable: true,
      get() { return this === doc && full ? full : (ownElement && ownElement.get ? ownElement.get.call(this) : null); },
    });
    Object.defineProperty(DP, 'fullscreenEnabled', { configurable: true, enumerable: true, get() { return true; } });

    let settle = 0;
    window.addEventListener('resize', () => { clearTimeout(settle); settle = setTimeout(check, 250); });
    // Esc leaves full screen, as in a browser (the apps still see the key).
    window.addEventListener('keydown', (e) => {
      if (full && e.key === 'Escape' && !e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey) doc.exitFullscreen().catch(() => {});
    }, true);
  }

  // ---- Tablets (TB.1) ------------------------------------------------------------------------------------------------
  function err(code, msg) { const e = new Error(`${code}: ${msg}`); e.code = code; return e; }

  function tabletBridge(b) {
    if (top) viewSide(b); else hubSide(b);
    const hooks = [saveDownloads(b), linksOut()];
    // Document listeners go on each document the window shows (see the top of this file).
    const install = () => { if (document.__ccHooked) return; document.__ccHooked = true; hooks.forEach((h) => h(document)); };
    Object.defineProperty(window, '__ccDocHooks', { value: install });
    install();
  }

  /** The top frame (the hub's page): the views. */
  function hubSide(b) {
    const views = new Map();    // id → {id, app, frame}
    const launch = new Map();   // id → paths for its launchFiles(), each once
    let shown = null;           // the view on screen; null = the hub
    let n = 0;
    let changed = null;
    const notify = (type, id) => { if (changed) { try { changed({ type, id }); } catch { /* the hub's own */ } } };
    let box = null;
    const layer = () => {
      if (box) return box;
      box = document.createElement('div');
      box.setAttribute('data-cc-views', '');
      box.style.cssText = 'position:fixed;inset:0;z-index:2147483600;visibility:hidden;background:Canvas';
      box.inert = true;
      document.documentElement.append(box);
      return box;
    };
    const titleOf = (v) => { try { return v.frame.contentDocument.title || ''; } catch { return ''; } };
    // One history entry while a view shows, so the system Back gesture (Android) returns to the hub.
    const show = (id, fromHistory = false) => {
      const was = shown;
      shown = id && views.has(id) ? id : null;
      for (const v of views.values()) {
        const on = v.id === shown;
        v.frame.style.visibility = on ? 'visible' : 'hidden';
        v.frame.inert = !on;
      }
      const L = layer();
      L.style.visibility = shown ? 'visible' : 'hidden';
      L.inert = !shown;
      if (!fromHistory) {
        if (shown && !was) history.pushState({ ccView: shown }, '');
        else if (shown && was) history.replaceState({ ccView: shown }, '');
        else if (!shown && was && history.state && history.state.ccView) history.back();
      }
      if (shown) { try { views.get(shown).frame.contentWindow.focus(); } catch { /* not loaded yet */ } }
      if (shown !== was) notify('focus', shown || 'main');
    };
    window.addEventListener('popstate', (e) => {
      const v = e.state && e.state.ccView;
      show(v && views.has(v) ? v : null, true);
    });
    const frameFor = (id, src, title) => {
      const frame = document.createElement('iframe');
      frame.name = id;
      frame.title = title;
      frame.setAttribute('allow', 'fullscreen; clipboard-read; clipboard-write');
      frame.style.cssText = 'position:absolute;inset:0;width:100%;height:100%;border:0;visibility:hidden;background:Canvas';
      frame.inert = true;
      frame.src = src;
      // A frame's window can outlive its first (about:blank) document, and the shell's start-of-document scripts may not
      // run again for the real one: its document listeners are put in place here too (once per document), as soon as
      // that document exists (hookSoon, after the frame is in the page) and again at load.
      frame.addEventListener('load', () => { try { frame.contentWindow.__ccDocHooks(); } catch { /* not ours */ } });
      return frame;
    };
    const hookSoon = (frame) => {
      let first = null;
      try { first = frame.contentDocument; } catch { return; }
      let n = 0;
      const t = setInterval(() => {
        let d = null;
        try { d = frame.contentDocument; } catch { clearInterval(t); return; }
        if (d && d !== first && d.URL !== 'about:blank') {
          clearInterval(t);
          try { frame.contentWindow.__ccDocHooks(); } catch { /* not ours */ }
        } else if (++n > 1500) clearInterval(t);
      }, 20);
    };
    // Does the page in the view have unsaved work? Its own beforeunload handler says so (the apps' "Leave site?").
    const dirty = (v) => {
      try {
        const w = v.frame.contentWindow;
        const ev = new w.Event('beforeunload', { cancelable: true });
        w.dispatchEvent(ev);
        return ev.defaultPrevented;
      } catch { return false; }
    };
    const close = (id, o = {}) => {
      const v = views.get(id);
      if (!v) return false;
      if (!o.force && dirty(v)) return false;
      if (shown === id) show(null);
      views.delete(id);
      // Its Studio Live session ends as a closed window's does.
      if (id !== 'account') T.invoke('cc_live_view_closed', { view: id }).catch(() => {});
      launch.delete(id);
      v.frame.remove();
      if (id !== 'account') notify('closed', id);
      return true;
    };
    const list = () => [{ id: 'main', app: 'studio', title: document.title, focused: shown === null }]
      .concat([...views.values()].filter((v) => v.app).map((v) => ({ id: v.id, app: v.app, title: titleOf(v), focused: shown === v.id })));
    Object.defineProperty(window, '__ccViews', {
      value: Object.freeze({
        idOf: (w) => { for (const v of views.values()) if (v.frame.contentWindow === w) return v.id; return null; },
        takeLaunch: (id) => { const p = launch.get(id) || []; launch.delete(id); return p.slice(); },
        list,
        focus: (id) => show(id),
        close: (id, o) => close(id, o),
        openAccount: () => {
          if (!views.has('account')) {
            const frame = frameFor('account', '/.shell/account.html', 'account');
            views.set('account', { id: 'account', app: null, frame });
            layer().append(frame);
            hookSoon(frame);
          }
          show('account');
        },
      }),
    });
    // The shell's own callbacks reach the top frame only; account changes go on to every view.
    forward = (s) => { for (const v of views.values()) { try { v.frame.contentWindow.__ccDesktopAccount(s); } catch { /* not loaded yet */ } } };
    b.windows = Object.freeze({
      open: async (o = {}) => {
        const app = String(o.app || '');
        const search = String(o.search || '');
        if (!/^[a-z0-9]{1,32}$/.test(app) || app === 'studio') throw err('error', 'unknown app');
        if (search && !/^\?[\x21-\x7e]{0,511}$/.test(search)) throw err('error', 'bad search');
        if (/[#\\]/.test(search)) throw err('error', 'bad search');
        // Beside the hub's page (the suite's root: / in the shell).
        const page = new URL(`apps/${app}/index.html`, location.href).pathname;
        const src = page + search;
        const head = await fetch(page, { method: 'HEAD' }).catch(() => null);
        if (!head || !head.ok) throw err('error', `unknown app ${app}`);
        const id = `app-${app}-${++n}`;
        const paths = (o.paths || []).map(String);
        if (paths.length) launch.set(id, paths);
        const frame = frameFor(id, src, app);
        views.set(id, { id, app, frame });
        layer().append(frame);
        hookSoon(frame);
        frame.addEventListener('load', () => notify('title', id));
        show(id);
        notify('opened', id);
        return { id };
      },
      list: async () => list(),
      focus: async (id) => { if (id !== 'main' && !views.has(String(id))) throw err('error', 'unknown window'); show(id === 'main' ? null : String(id)); },
      focusHub: async () => show(null),
      /** Tablets only: close a view. false = the app has unsaved work (ask the person, then {force: true}). */
      close: async (id, o = {}) => close(String(id), { force: !!o.force }),
      closeSelf: async () => {},
      onChange: (cb) => { changed = typeof cb === 'function' ? cb : null; },
      launchApps: b.windows ? b.windows.launchApps : async () => [],
      onLaunchApp: b.windows ? b.windows.onLaunchApp : () => {},
    });
    if (b.account) b.account = Object.freeze(Object.assign({}, b.account, { open: async () => window.__ccViews.openAccount() }));
  }

  /** A view (an app, or the sign-in page) inside the hub's page. */
  function viewSide(b) {
    const me = () => top.idOf(window);
    liveView = me;
    const info = b.info;
    b.info = async () => Object.assign({}, await info(), { windowId: me() || 'view' });
    b.launchFiles = async () => top.takeLaunch(me());
    b.onOpenFiles = () => {};   // files Studio is asked to open go to the hub, which opens a view for them
    delete b.links;              // ccstudio:// links are the hub's
    if (b.windows) {
      b.windows = Object.freeze({
        open: async () => { throw err('not_allowed', 'only the hub opens windows'); },
        list: async () => top.list(),
        focus: async (id) => top.focus(id === 'main' ? null : String(id)),
        focusHub: async () => top.focus(null),
        closeSelf: async () => { top.close(me(), { force: true }); },
        onChange: () => {},
        launchApps: async () => [],
        onLaunchApp: () => {},
      });
    }
    if (b.account) {
      b.account = Object.freeze(Object.assign({}, b.account, {
        open: async () => top.openAccount(),
        closeWindow: async () => { top.close(me(), { force: true }); },
      }));
    }
  }

  /** `<a download>` of the page's own data (blob:, data:) asks where to save, as host.js's saves do: the web view has no
   *  Downloads folder of its own. Blobs are remembered when made, since pages often revoke the URL right after click(). */
  function saveDownloads(b) {
    const blobs = new Map();
    const create = URL.createObjectURL;
    const revoke = URL.revokeObjectURL;
    URL.createObjectURL = function (o) { const u = create.call(URL, o); if (o instanceof Blob) blobs.set(u, o); return u; };
    URL.revokeObjectURL = function (u) { revoke.call(URL, u); setTimeout(() => blobs.delete(u), 60000); };
    const ours = (a) => !!a && a.hasAttribute('download') && /^(blob|data):/i.test(a.href || '');
    const save = async (a) => {
      const href = a.href;
      const name = a.getAttribute('download') || 'download';
      const blob = blobs.get(href) || (await (await fetch(href)).blob());
      const path = await b.saveDialog({ defaultPath: name });
      if (path) await b.writeFile(path, new Uint8Array(await blob.arrayBuffer()));
    };
    const click = HTMLAnchorElement.prototype.click;
    HTMLAnchorElement.prototype.click = function () {
      if (ours(this) && !this.isConnected) { save(this).catch(() => {}); return; }
      return click.call(this);
    };
    return (d) => d.addEventListener('click', (e) => {
      const a = e.target && e.target.closest ? e.target.closest('a[download]') : null;
      if (!ours(a)) return;
      e.preventDefault();
      save(a).catch(() => {});
    }, true);
  }

  /** Links that leave Studio open in the person's browser — from a view too, which would otherwise navigate itself away.
   *  No pop-up windows on a tablet: window.open of our own pages gives null (the apps keep their panels docked). */
  function linksOut() {
    const outside = (href) => {
      try {
        const u = new URL(String(href), location.href);
        return u.origin !== location.origin && /^(https?|mailto):$/.test(u.protocol) ? u.href : null;
      } catch { return null; }
    };
    window.open = function (url) {
      const out = outside(url);
      if (out) T.invoke('cc_open_url', { url: out }).catch(() => {});
      return null;
    };
    return (d) => d.addEventListener('click', (e) => {
      if (e.defaultPrevented) return;
      const a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
      const url = a && !a.hasAttribute('download') ? outside(a.getAttribute('href')) : null;
      if (!url) return;
      e.preventDefault();
      T.invoke('cc_open_url', { url }).catch(() => {});
    });
  }
})();
