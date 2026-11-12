/* Volant's scriptlets, for filters written in uBlock Origin's `##+js(...)` syntax.
   Written for Volant (MIT); names, aliases and arguments follow uBlock Origin's
   so its filter lists work unchanged. Runs in the page, before the page's own
   scripts. Every call is isolated: one failing scriptlet never breaks a page. */
(function (calls) {
  'use strict';
  if (!Array.isArray(calls) || calls.length === 0) return;
  const W = window;
  const noop = () => {};

  // ---------- helpers ----------
  /** uBO patterns: '' matches all, /re/flags is a regex, anything else is a substring. */
  const pattern = (p) => {
    if (p === undefined || p === '' || p === '*') return { test: () => true, all: true };
    let not = false;
    if (p.startsWith('!')) { not = true; p = p.slice(1); }
    let re;
    const m = /^\/(.+)\/([gimsuy]*)$/.exec(p);
    try { re = m ? new RegExp(m[1], m[2]) : null; } catch { re = null; }
    const test = re ? (s) => re.test(String(s)) : (s) => String(s).includes(p);
    return { test: (s) => test(s) !== not, all: false };
  };
  const constant = (v) => {
    switch (v) {
      case 'undefined': return undefined;
      case 'false': return false;
      case 'true': return true;
      case 'null': return null;
      case "''": case '""': case 'emptyStr': return '';
      case 'emptyArr': case '[]': return [];
      case 'emptyObj': case '{}': return {};
      case 'noopFunc': return function () {};
      case 'trueFunc': return () => true;
      case 'falseFunc': return () => false;
      case 'throwFunc': return () => { throw new Error(); };
      case 'noopCallbackFunc': return () => noop;
      case 'noopThisFunc': return function () { return this; };
      case 'NaN': return NaN;
      case 'Infinity': return Infinity;
      case 'yes': case 'no': case 'on': case 'off': case 'accept': case 'reject': case 'accepted': case 'rejected': return v;
      default:
        if (/^-?\d+(\.\d+)?$/.test(v)) return Number(v);
        return undefined;
    }
  };
  const isConstant = (v) => v !== undefined && (constant(v) !== undefined || v === 'undefined');
  /** Calls `onReady` once `chain` (a.b.c) can be reached, trapping each missing link. */
  const onChain = (root, chain, handler) => {
    const parts = chain.split('.');
    const walk = (owner, i) => {
      const prop = parts[i];
      if (i === parts.length - 1) { handler(owner, prop); return; }
      const v = owner[prop];
      if (v instanceof Object || (typeof v === 'object' && v !== null)) { walk(v, i + 1); return; }
      // Not there yet: wait for it to be assigned.
      let value = v;
      try {
        Object.defineProperty(owner, prop, {
          configurable: true,
          get() { return value; },
          set(nv) {
            value = nv;
            if (nv instanceof Object) walk(nv, i + 1);
          },
        });
      } catch {}
    };
    walk(root, 0);
  };
  const abortError = () => new ReferenceError('Volant blocked this script');
  const onDom = (fn) => {
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', fn, { once: true });
    else fn();
  };
  const observe = (fn) => {
    let queued = false;
    const run = () => { queued = false; try { fn(); } catch {} };
    onDom(run);
    const mo = new MutationObserver(() => { if (!queued) { queued = true; requestAnimationFrame(run); } });
    const start = () => mo.observe(document.documentElement, { childList: true, subtree: true, attributes: true });
    if (document.documentElement) start(); else document.addEventListener('readystatechange', start, { once: true });
  };

  // ---------- scriptlets ----------
  const S = {};

  S['set-constant'] = (chain, value) => {
    if (!chain || !isConstant(value)) return;
    const v = constant(value);
    onChain(W, chain, (owner, prop) => {
      try {
        Object.defineProperty(owner, prop, { configurable: false, get: () => v, set: noop });
      } catch {
        try { owner[prop] = v; } catch {}
      }
    });
  };

  S['abort-on-property-read'] = (chain) => {
    if (!chain) return;
    onChain(W, chain, (owner, prop) => {
      try { Object.defineProperty(owner, prop, { configurable: false, get() { throw abortError(); }, set: noop }); } catch {}
    });
  };

  S['abort-on-property-write'] = (chain) => {
    if (!chain) return;
    onChain(W, chain, (owner, prop) => {
      try { Object.defineProperty(owner, prop, { configurable: false, get: () => undefined, set() { throw abortError(); } }); } catch {}
    });
  };

  S['abort-current-script'] = (chain, needle, context) => {
    if (!chain) return;
    const n = pattern(needle);
    const c = pattern(context);
    const thisScript = document.currentScript;
    onChain(W, chain, (owner, prop) => {
      let value = owner[prop];
      const check = () => {
        const s = document.currentScript;
        if (!s || s === thisScript) return;
        const text = s.src ? s.src : s.textContent;
        if (n.test(text) && c.test(s.src || text)) throw abortError();
      };
      try {
        Object.defineProperty(owner, prop, {
          configurable: true,
          get() { check(); return value; },
          set(v) { check(); value = v; },
        });
      } catch {}
    });
  };

  S['abort-on-stack-trace'] = (chain, needle) => {
    if (!chain) return;
    const n = pattern(needle);
    onChain(W, chain, (owner, prop) => {
      let value = owner[prop];
      const check = () => { if (n.test(new Error().stack || '')) throw abortError(); };
      try {
        Object.defineProperty(owner, prop, { configurable: true, get() { check(); return value; }, set(v) { check(); value = v; } });
      } catch {}
    });
  };

  S['window.open-defuser'] = (needle, delay, decoy) => {
    let not = false;
    if (needle === '0') { not = true; needle = ''; } // old syntax: '0' = everything except
    const n = pattern(needle);
    const open = W.open;
    W.open = new Proxy(open, {
      apply(target, self, args) {
        const url = String(args[0] || '');
        if (n.test(url) === not) return Reflect.apply(target, self, args);
        if (decoy) {
          const f = document.createElement(decoy === 'obj' ? 'object' : 'iframe');
          f.style.display = 'none';
          (document.body || document.documentElement).appendChild(f);
          setTimeout(() => f.remove(), (Number(delay) || 1) * 1000);
          return f.contentWindow;
        }
        return new Proxy(W, { get: (t, p) => (p === 'closed' ? false : typeof t[p] === 'function' ? noop : t[p]), set: () => true });
      },
    });
  };

  const timerFilter = (name, needle, delay) => {
    const n = pattern(needle);
    let not = false;
    if (delay && delay.startsWith('!')) { not = true; delay = delay.slice(1); }
    const d = delay ? Number(delay) : NaN;
    const original = W[name];
    W[name] = new Proxy(original, {
      apply(target, self, args) {
        const [fn, ms] = args;
        const text = typeof fn === 'function' ? fn.toString() : String(fn);
        const delayMatch = isNaN(d) || ((Number(ms) || 0) === d) !== not;
        if (n.test(text) && delayMatch) return 0;
        return Reflect.apply(target, self, args);
      },
    });
  };
  S['no-setTimeout-if'] = (needle, delay) => timerFilter('setTimeout', needle, delay);
  S['no-setInterval-if'] = (needle, delay) => timerFilter('setInterval', needle, delay);

  const timerBoost = (name, needle, delay, boost) => {
    const n = pattern(needle);
    const d = delay === '*' || delay === undefined || delay === '' ? null : Number(delay);
    let b = Number(boost);
    if (!(b > 0)) b = 0.05;
    b = Math.min(Math.max(b, 0.001), 50);
    const original = W[name];
    W[name] = new Proxy(original, {
      apply(target, self, args) {
        const [fn, ms] = args;
        if (n.test(typeof fn === 'function' ? fn.toString() : String(fn)) && (d === null || Number(ms) === d)) args[1] = (Number(ms) || 0) * b;
        return Reflect.apply(target, self, args);
      },
    });
  };
  S['adjust-setTimeout'] = (needle, delay, boost) => timerBoost('setTimeout', needle, delay, boost);
  S['adjust-setInterval'] = (needle, delay, boost) => timerBoost('setInterval', needle, delay || '1000', boost);

  S['prevent-addEventListener'] = (type, needle) => {
    const t = pattern(type);
    const n = pattern(needle);
    const add = EventTarget.prototype.addEventListener;
    EventTarget.prototype.addEventListener = new Proxy(add, {
      apply(target, self, args) {
        const [ty, fn] = args;
        const text = typeof fn === 'function' ? fn.toString() : String(fn);
        if (t.test(String(ty)) && n.test(text)) return undefined;
        return Reflect.apply(target, self, args);
      },
    });
  };

  const requestMatches = (props, url, init) => {
    if (!props || props === '*') return true;
    const want = {};
    for (const pair of props.split(/\s+/)) {
      const i = pair.indexOf(':');
      if (i === -1) want.url = pattern(pair);
      else want[pair.slice(0, i)] = pattern(pair.slice(i + 1));
    }
    const have = { url: String(url), method: (init && init.method) || 'GET', body: (init && init.body) || '', mode: (init && init.mode) || '' };
    return Object.entries(want).every(([k, p]) => p.test(have[k] !== undefined ? have[k] : ''));
  };
  S['prevent-fetch'] = (props, body) => {
    const f = W.fetch;
    if (typeof f !== 'function') return;
    W.fetch = new Proxy(f, {
      apply(target, self, args) {
        const [res, init] = args;
        const url = res instanceof Request ? res.url : res;
        if (!requestMatches(props, url, init || (res instanceof Request ? res : undefined))) return Reflect.apply(target, self, args);
        const text = body === 'emptyArr' ? '[]' : body === 'emptyObj' ? '{}' : '';
        return Promise.resolve(new Response(text, { status: 200, statusText: 'OK' }));
      },
    });
  };

  S['prevent-xhr'] = (props) => {
    const X = W.XMLHttpRequest;
    if (!X) return;
    const blocked = new WeakSet();
    const open = X.prototype.open;
    const send = X.prototype.send;
    X.prototype.open = function (method, url, ...rest) {
      if (requestMatches(props, url, { method })) blocked.add(this);
      return open.call(this, method, url, ...rest);
    };
    X.prototype.send = function (...args) {
      if (!blocked.has(this)) return send.apply(this, args);
      const xhr = this;
      for (const [k, v] of [['readyState', 4], ['status', 200], ['statusText', 'OK'], ['responseText', ''], ['response', '']]) {
        try { Object.defineProperty(xhr, k, { value: v, configurable: true }); } catch {}
      }
      setTimeout(() => {
        for (const ev of ['readystatechange', 'load', 'loadend']) {
          try { xhr.dispatchEvent(new Event(ev)); } catch {}
        }
      }, 1);
    };
  };

  S['noeval-if'] = (needle) => {
    const n = pattern(needle);
    W.eval = new Proxy(W.eval, { apply(t, self, args) { return n.test(String(args[0])) ? undefined : Reflect.apply(t, self, args); } });
  };
  S['noeval'] = () => { W.eval = new Proxy(W.eval, { apply: () => undefined }); };

  const prune = (obj, paths, required) => {
    if (!obj || typeof obj !== 'object') return obj;
    const has = (o, path) => path.split('.').reduce((a, k) => (a && typeof a === 'object' && k in a ? a[k] : undefined), o) !== undefined;
    if (required && !required.split(/\s+/).every((p) => has(obj, p))) return obj;
    for (const path of paths.split(/\s+/).filter(Boolean)) {
      const keys = path.split('.');
      const drop = (o, i) => {
        if (!o || typeof o !== 'object') return;
        const k = keys[i];
        if (k === '[]' || k === '*') { for (const v of Object.values(o)) drop(v, i + 1); return; }
        if (i === keys.length - 1) { delete o[k]; return; }
        drop(o[k], i + 1);
      };
      drop(obj, 0);
    }
    return obj;
  };
  S['json-prune'] = (paths, required) => {
    if (!paths) return;
    const parse = JSON.parse;
    JSON.parse = new Proxy(parse, { apply(t, self, args) { return prune(Reflect.apply(t, self, args), paths, required); } });
    const json = Response.prototype.json;
    Response.prototype.json = new Proxy(json, { apply(t, self, args) { return Reflect.apply(t, self, args).then((o) => prune(o, paths, required)); } });
  };
  S['json-prune-fetch-response'] = (paths, required, ...rest) => {
    if (!paths || typeof W.fetch !== 'function') return;
    let props = '';
    for (let i = 0; i < rest.length - 1; i += 2) if (rest[i] === 'propsToMatch') props = rest[i + 1];
    const f = W.fetch;
    W.fetch = new Proxy(f, {
      apply(target, self, args) {
        const p = Reflect.apply(target, self, args);
        const url = args[0] instanceof Request ? args[0].url : args[0];
        if (!requestMatches(props, url, args[1])) return p;
        return p.then((res) => res.clone().json().then((o) => new Response(JSON.stringify(prune(o, paths, required)), { status: res.status, statusText: res.statusText, headers: res.headers })).catch(() => res));
      },
    });
  };

  const textNodes = (nodeName, needle, replace) => {
    const n = pattern(needle);
    const tag = (nodeName || '').toUpperCase();
    const fix = (node) => {
      const host = node.nodeType === 3 ? node.parentNode : node;
      if (!host || (tag && tag !== '#TEXT' && host.nodeName !== tag)) return;
      const text = node.textContent;
      if (!n.test(text)) return;
      if (replace === undefined) node.textContent = '';
      else node.textContent = text.replace(n.all ? /^[\s\S]*$/ : new RegExp(needle.startsWith('/') ? needle.slice(1, needle.lastIndexOf('/')) : needle.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'g'), replace);
    };
    const mo = new MutationObserver((records) => {
      for (const r of records) for (const node of r.addedNodes) {
        if (node.nodeType === 3 || (node.nodeType === 1 && node.childNodes.length <= 1)) fix(node);
        if (node.nodeType === 1 && node.querySelectorAll && tag) node.querySelectorAll(tag.toLowerCase()).forEach(fix);
      }
    });
    const start = () => mo.observe(document.documentElement || document, { childList: true, subtree: true });
    start();
    onDom(() => { if (tag) document.querySelectorAll(tag.toLowerCase()).forEach(fix); setTimeout(() => mo.disconnect(), 20000); });
  };
  S['remove-node-text'] = (nodeName, needle) => textNodes(nodeName, needle);
  S['replace-node-text'] = (nodeName, needle, replacement) => textNodes(nodeName, needle, replacement || '');

  S['remove-attr'] = (attrs, selector, behavior) => {
    if (!attrs) return;
    const names = attrs.split(/\s*\|\s*/);
    const sel = selector || names.map((a) => `[${a}]`).join(',');
    const run = () => { try { document.querySelectorAll(sel).forEach((el) => names.forEach((a) => el.removeAttribute(a))); } catch {} };
    if (behavior && behavior.includes('stay')) observe(run); else { onDom(run); setTimeout(run, 1000); }
  };
  S['remove-class'] = (classes, selector, behavior) => {
    if (!classes) return;
    const names = classes.split(/\s*\|\s*/);
    const sel = selector || names.map((c) => `.${CSS.escape(c)}`).join(',');
    const run = () => { try { document.querySelectorAll(sel).forEach((el) => el.classList.remove(...names)); } catch {} };
    if (behavior && behavior.includes('stay')) observe(run); else { onDom(run); setTimeout(run, 1000); }
  };
  S['set-attr'] = (selector, attr, value = '') => {
    if (!selector || !attr || /^on/i.test(attr)) return;
    observe(() => document.querySelectorAll(selector).forEach((el) => el.getAttribute(attr) !== value && el.setAttribute(attr, value)));
  };

  const storageItem = (store, key, value) => {
    if (!key) return;
    try {
      const s = W[store];
      if (value === '$remove$') { for (const k of Object.keys(s)) if (pattern(key).test(k)) s.removeItem(k); return; }
      const v = constant(value);
      if (v === undefined && value !== 'undefined') return;
      s.setItem(key, typeof v === 'object' ? JSON.stringify(v) : String(v));
    } catch {}
  };
  S['set-local-storage-item'] = (key, value) => storageItem('localStorage', key, value);
  S['set-session-storage-item'] = (key, value) => storageItem('sessionStorage', key, value);

  S['set-cookie'] = (name, value, path) => {
    if (!name || !isConstant(value) && !/^[\w-]{0,30}$/.test(value || '')) return;
    const v = typeof constant(value) === 'undefined' ? value : String(constant(value));
    document.cookie = `${encodeURIComponent(name)}=${encodeURIComponent(v)}; path=${path === 'none' ? '' : path || '/'}`;
  };
  S['remove-cookie'] = (needle) => {
    const n = pattern(needle);
    const run = () => {
      for (const c of document.cookie.split(';')) {
        const name = c.split('=')[0].trim();
        if (!name || !n.test(name)) continue;
        const host = location.hostname;
        for (const d of ['', host, '.' + host, '.' + host.split('.').slice(-2).join('.')]) {
          document.cookie = `${name}=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/` + (d ? `; domain=${d}` : '');
        }
      }
    };
    run();
    W.addEventListener('beforeunload', run);
  };

  S['href-sanitizer'] = (selector, source) => {
    if (!selector) return;
    const pick = (a) => {
      if (!source || source === 'text') return a.textContent.trim();
      if (source.startsWith('[') && source.endsWith(']')) return a.getAttribute(source.slice(1, -1));
      if (source.startsWith('?')) { try { return new URL(a.href).searchParams.get(source.slice(1)); } catch { return null; } }
      return null;
    };
    observe(() => document.querySelectorAll(selector).forEach((a) => {
      const v = pick(a);
      if (v && /^https?:\/\//.test(v) && a.href !== v) a.setAttribute('href', v);
    }));
  };

  S['nowebrtc'] = () => {
    const Stub = function () { throw new DOMException('WebRTC is turned off', 'NotAllowedError'); };
    for (const name of ['RTCPeerConnection', 'webkitRTCPeerConnection']) if (W[name]) W[name] = Stub;
  };

  S['disable-newtab-links'] = () => {
    document.addEventListener('click', (e) => {
      const a = e.target && e.target.closest && e.target.closest('a[target]');
      if (a && a.target !== '_self') { a.removeAttribute('target'); }
    }, true);
  };

  S['prevent-refresh'] = (delay) => {
    onDom(() => document.querySelectorAll('meta[http-equiv="refresh" i]').forEach((m) => m.remove()));
    void delay;
  };

  S['window-close-if'] = (needle) => {
    if (pattern(needle).test(location.href)) W.close();
  };

  S['no-requestAnimationFrame-if'] = (needle) => {
    const n = pattern(needle);
    const raf = W.requestAnimationFrame;
    W.requestAnimationFrame = new Proxy(raf, { apply(t, self, args) { return n.test(String(args[0])) ? 0 : Reflect.apply(t, self, args); } });
  };

  S['bab-defuser'] = () => {
    const stub = { onDetected: () => stub, onNotDetected: (f) => { try { f(); } catch {} return stub; }, check: noop, emitEvent: noop, clearEvent: noop, setOption: noop };
    for (const name of ['blockAdBlock', 'BlockAdBlock', 'fuckAdBlock', 'FuckAdBlock', 'sniffAdBlock', 'SniffAdBlock']) {
      try { Object.defineProperty(W, name, { configurable: true, get: () => (name[0] === name[0].toUpperCase() ? function () { return stub; } : stub), set: noop }); } catch {}
    }
  };

  // ---------- uBlock Origin's names for these ----------
  const ALIAS = {
    'set': 'set-constant', 'set-constant.js': 'set-constant',
    'aopr': 'abort-on-property-read', 'abort-on-property-read.js': 'abort-on-property-read',
    'aopw': 'abort-on-property-write', 'abort-on-property-write.js': 'abort-on-property-write',
    'acs': 'abort-current-script', 'acis': 'abort-current-script', 'abort-current-inline-script': 'abort-current-script', 'abort-current-script.js': 'abort-current-script', 'abort-current-inline-script.js': 'abort-current-script',
    'aost': 'abort-on-stack-trace', 'abort-on-stack-trace.js': 'abort-on-stack-trace',
    'nowoif': 'window.open-defuser', 'no-window-open-if': 'window.open-defuser', 'window.open-defuser.js': 'window.open-defuser', 'no-window-open-if.js': 'window.open-defuser',
    'nostif': 'no-setTimeout-if', 'prevent-setTimeout': 'no-setTimeout-if', 'setTimeout-defuser': 'no-setTimeout-if', 'no-setTimeout-if.js': 'no-setTimeout-if', 'prevent-setTimeout.js': 'no-setTimeout-if',
    'nosiif': 'no-setInterval-if', 'prevent-setInterval': 'no-setInterval-if', 'setInterval-defuser': 'no-setInterval-if', 'no-setInterval-if.js': 'no-setInterval-if', 'prevent-setInterval.js': 'no-setInterval-if',
    'nano-stb': 'adjust-setTimeout', 'nano-setTimeout-booster': 'adjust-setTimeout', 'adjust-setTimeout.js': 'adjust-setTimeout', 'nano-setTimeout-booster.js': 'adjust-setTimeout',
    'nano-sib': 'adjust-setInterval', 'nano-setInterval-booster': 'adjust-setInterval', 'adjust-setInterval.js': 'adjust-setInterval', 'nano-setInterval-booster.js': 'adjust-setInterval',
    'aeld': 'prevent-addEventListener', 'addEventListener-defuser': 'prevent-addEventListener', 'prevent-addEventListener.js': 'prevent-addEventListener', 'addEventListener-defuser.js': 'prevent-addEventListener',
    'no-fetch-if': 'prevent-fetch', 'no-fetch-if.js': 'prevent-fetch', 'prevent-fetch.js': 'prevent-fetch',
    'no-xhr-if': 'prevent-xhr', 'no-xhr-if.js': 'prevent-xhr', 'prevent-xhr.js': 'prevent-xhr',
    'noeval-if': 'noeval-if', 'prevent-eval-if': 'noeval-if', 'noeval-if.js': 'noeval-if', 'noeval.js': 'noeval', 'silent-noeval.js': 'noeval', 'silent-noeval': 'noeval',
    'json-prune.js': 'json-prune', 'json-prune-fetch-response.js': 'json-prune-fetch-response',
    'rmnt': 'remove-node-text', 'remove-node-text.js': 'remove-node-text',
    'rpnt': 'replace-node-text', 'replace-node-text.js': 'replace-node-text', 'trusted-rpnt': 'replace-node-text', 'trusted-replace-node-text': 'replace-node-text',
    'ra': 'remove-attr', 'remove-attr.js': 'remove-attr',
    'rc': 'remove-class', 'remove-class.js': 'remove-class',
    'set-attr.js': 'set-attr',
    'set-local-storage-item.js': 'set-local-storage-item', 'set-session-storage-item.js': 'set-session-storage-item',
    'set-cookie.js': 'set-cookie',
    'cookie-remover': 'remove-cookie', 'cookie-remover.js': 'remove-cookie', 'remove-cookie.js': 'remove-cookie',
    'href-sanitizer.js': 'href-sanitizer',
    'nowebrtc.js': 'nowebrtc',
    'disable-newtab-links.js': 'disable-newtab-links',
    'refresh-defuser': 'prevent-refresh', 'refresh-defuser.js': 'prevent-refresh', 'prevent-refresh.js': 'prevent-refresh',
    'close-window': 'window-close-if', 'window-close-if.js': 'window-close-if',
    'norafif': 'no-requestAnimationFrame-if', 'no-requestAnimationFrame-if.js': 'no-requestAnimationFrame-if', 'prevent-requestAnimationFrame': 'no-requestAnimationFrame-if',
    'nobab': 'bab-defuser', 'bab-defuser.js': 'bab-defuser', 'nobab.js': 'bab-defuser',
  };

  for (const [name, ...args] of calls) {
    const fn = S[ALIAS[name] || name];
    if (!fn) continue;
    try { fn(...args); } catch {}
  }
})
