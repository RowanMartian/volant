/* Volant's cosmetic filtering for a page: the hide selectors WebKit's rule
   compiler can't take, and uBlock Origin's procedural filters (`:has-text`,
   `:upward`, `:remove`, `:style`, ...). Written for Volant (MIT), following
   uBlock Origin's filter syntax. */
(function (hide, procedural) {
  'use strict';
  const doc = document;
  // A per-page attribute name, so pages can't target Volant's marker.
  const mark = 'v' + Math.random().toString(36).slice(2, 9);

  // ---------- plain selectors: one CSS rule each, so a bad one can't sink the rest ----------
  // Volant's own rule first, so no odd selector from a list can swallow it.
  const sheet = [`[${mark}]{display:none!important}`];
  for (const sel of hide) sheet.push(`${sel}{display:none!important}`);

  // ---------- procedural filters ----------
  const OPS = ['has-text', 'upward', 'remove', 'style', 'matches-css', 'matches-css-before', 'matches-css-after', 'min-text-length', 'xpath',
    'matches-attr', 'matches-path', 'matches-media', 'others', 'watch-attr', 'remove-attr', 'remove-class', 'has', 'not', 'if', 'if-not', 'spath',
    '-abp-has', '-abp-contains', 'contains', 'nth-ancestor', 'matches-prop'];
  const OP_RE = new RegExp(':(' + OPS.map((o) => o.replace(/[-]/g, '\\-')).join('|') + ')\\(', 'g');

  const toRegex = (s) => {
    const m = /^\/(.+)\/([imsu]*)$/.exec(s.trim());
    if (m) { try { return new RegExp(m[1], m[2]); } catch { return null; } }
    return null;
  };
  const matcher = (s) => {
    const re = toRegex(s);
    if (re) return (t) => re.test(t);
    const plain = s.trim();
    return (t) => t.includes(plain);
  };
  /** The argument of `:op(` starting at index `i` (just after the paren); balanced. */
  const argAt = (s, i) => {
    let depth = 1, j = i, quote = '';
    for (; j < s.length; j++) {
      const c = s[j];
      if (quote) { if (c === '\\') j++; else if (c === quote) quote = ''; continue; }
      if (c === '"' || c === "'") quote = c;
      else if (c === '(') depth++;
      else if (c === ')' && --depth === 0) break;
    }
    return [s.slice(i, j), j + 1];
  };
  const hasProceduralInside = (s) => OPS.filter((o) => o !== 'has' && o !== 'not').some((o) => s.includes(':' + o + '('));

  /** Compiles a procedural selector into a function: root → elements. */
  const compile = (src) => {
    const s = src.trim();
    const tasks = [];
    let css = '';
    let i = 0;
    let action = null;
    while (i < s.length) {
      OP_RE.lastIndex = i;
      const m = OP_RE.exec(s);
      if (!m) { css += s.slice(i); break; }
      const name = m[1];
      const [arg, next] = argAt(s, m.index + m[0].length);
      // Native :has()/:not() stay in the CSS part unless they hold procedural operators.
      if ((name === 'has' || name === 'not') && !hasProceduralInside(arg)) {
        css += s.slice(i, next);
        i = next;
        continue;
      }
      css += s.slice(i, m.index);
      // A filter may start with an operator (`:xpath(...)`); then there's no CSS step.
      if (css.trim()) tasks.push({ op: 'css', arg: css.trim() });
      css = '';
      tasks.push({ op: name, arg });
      i = next;
    }
    if (css.trim()) tasks.push({ op: 'css', arg: css.trim() });
    const last = tasks[tasks.length - 1];
    if (last && ['remove', 'style', 'remove-attr', 'remove-class'].includes(last.op)) action = tasks.pop();
    const steps = tasks.map((t) => step(t)).filter(Boolean);
    return { run: (root) => steps.reduce((nodes, f) => f(nodes, root), null), action };
  };

  const step = ({ op, arg }) => {
    switch (op) {
      case 'css':
        return (nodes) => {
          if (nodes === null) {
            if (!arg) return [];
            try { return Array.from(doc.querySelectorAll(arg)); } catch { return []; }
          }
          // A CSS continuation after a procedural step: `div:has-text(x) > span`.
          const rel = /^[>+~\s]/.test(arg) ? ':scope ' + arg : arg;
          const out = [];
          for (const n of nodes) {
            try {
              if (/^[>+~\s]/.test(arg)) out.push(...n.querySelectorAll(rel));
              else if (n.matches(arg)) out.push(n);
            } catch {}
          }
          return out;
        };
      case 'has-text': case 'contains': case '-abp-contains': {
        const t = matcher(arg);
        return (nodes) => (nodes || all()).filter((n) => t(n.textContent || ''));
      }
      case 'min-text-length': {
        const min = Number(arg) || 0;
        return (nodes) => (nodes || all()).filter((n) => (n.textContent || '').length >= min);
      }
      case 'upward': case 'nth-ancestor': {
        const k = Number(arg);
        return (nodes) => {
          const out = new Set();
          for (const n of nodes || []) {
            let p = n;
            if (k > 0) { for (let j = 0; j < k && p; j++) p = p.parentElement; }
            else { try { p = n.parentElement && n.parentElement.closest(arg); } catch { p = null; } }
            if (p && p !== doc.documentElement && p !== doc.body) out.add(p);
          }
          return Array.from(out);
        };
      }
      case 'matches-css': case 'matches-css-before': case 'matches-css-after': {
        const pseudo = op === 'matches-css' ? null : '::' + op.slice(12);
        const i = arg.indexOf(':');
        if (i === -1) return null;
        const prop = arg.slice(0, i).trim();
        const t = matcher(arg.slice(i + 1).trim());
        return (nodes) => (nodes || all()).filter((n) => { try { return t(getComputedStyle(n, pseudo).getPropertyValue(prop)); } catch { return false; } });
      }
      case 'matches-attr': {
        const i = arg.indexOf('=');
        const nameM = matcher((i === -1 ? arg : arg.slice(0, i)).replace(/^"|"$/g, ''));
        const valM = i === -1 ? null : matcher(arg.slice(i + 1).replace(/^"|"$/g, ''));
        return (nodes) => (nodes || all()).filter((n) => Array.from(n.attributes).some((a) => nameM(a.name) && (!valM || valM(a.value))));
      }
      case 'matches-path': {
        const t = matcher(arg);
        return (nodes) => (t(location.pathname + location.search) ? (nodes || []) : []);
      }
      case 'matches-media': {
        return (nodes) => { try { return matchMedia(arg).matches ? (nodes || []) : []; } catch { return []; } };
      }
      case 'xpath': {
        return (nodes) => {
          const out = [];
          for (const ctx of nodes || [doc]) {
            try {
              const r = doc.evaluate(arg, ctx, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
              for (let j = 0; j < r.snapshotLength; j++) { const n = r.snapshotItem(j); if (n.nodeType === 1) out.push(n); }
            } catch {}
          }
          return out;
        };
      }
      case 'has': case 'if': case '-abp-has': {
        const sub = compile(/^[>+~]/.test(arg.trim()) ? arg : ':scope ' + arg);
        return (nodes) => (nodes || all()).filter((n) => withinCount(sub, n) > 0);
      }
      case 'not': case 'if-not': {
        const sub = compile(arg);
        return (nodes) => {
          const bad = new Set(sub.run(doc));
          return (nodes || all()).filter((n) => !bad.has(n));
        };
      }
      case 'spath':
        return step({ op: 'css', arg });
      case 'others':
        return (nodes) => {
          const keep = new Set();
          for (const n of nodes || []) {
            for (let p = n; p; p = p.parentElement) keep.add(p);
            n.querySelectorAll('*').forEach((d) => keep.add(d));
          }
          return all().filter((n) => !keep.has(n) && n !== doc.head && !doc.head.contains(n));
        };
      case 'watch-attr':
        return (nodes) => nodes || [];
      default:
        return null;
    }
  };
  const all = () => Array.from(doc.querySelectorAll('body *'));
  const withinCount = (sub, n) => {
    try {
      const found = sub.run(n);
      return found.filter((f) => f !== n && n.contains(f)).length;
    } catch { return 0; }
  };

  const filters = [];
  for (const src of procedural) {
    // `selector:style(css)` with no other procedural parts is just a style rule.
    const st = /^(.*):style\((.*)\)$/.exec(src);
    if (st && !hasProceduralInside(st[1])) { sheet.push(`${st[1]}{${st[2]}}`); continue; }
    if (/:remove\(\)$/.test(src) && !hasProceduralInside(src.slice(0, -9))) { filters.push({ run: () => { try { return Array.from(doc.querySelectorAll(src.slice(0, -9))); } catch { return []; } }, action: { op: 'remove' } }); continue; }
    try { filters.push(compile(src)); } catch {}
  }

  const applyAll = () => {
    for (const f of filters) {
      let nodes;
      try { nodes = f.run(doc) || []; } catch { continue; }
      for (const n of nodes) {
        const a = f.action;
        if (!a) { if (!n.hasAttribute(mark)) n.setAttribute(mark, ''); continue; }
        try {
          if (a.op === 'remove') n.remove();
          else if (a.op === 'style') n.style.cssText += ';' + a.arg;
          else if (a.op === 'remove-attr') { const m = matcher(a.arg); for (const at of Array.from(n.attributes)) if (m(at.name)) n.removeAttribute(at.name); }
          else if (a.op === 'remove-class') { const m = matcher(a.arg); for (const c of Array.from(n.classList)) if (m(c)) n.classList.remove(c); }
        } catch {}
      }
    }
  };

  // The style sheet goes in right away (document start), so nothing flashes on screen.
  const style = doc.createElement('style');
  style.textContent = sheet.join('\n');
  const attach = () => {
    const root = doc.head || doc.documentElement;
    if (!root) return false;
    if (style.parentNode !== root) root.appendChild(style);
    return true;
  };
  if (!attach()) {
    // Before <html> exists: attach as soon as it does.
    const mo = new MutationObserver(() => { if (attach()) mo.disconnect(); });
    mo.observe(doc, { childList: true, subtree: true });
  }

  let installed = false;
  const install = () => {
    attach();
    if (installed || filters.length === 0 || !doc.documentElement) return;
    installed = true;
    applyAll();
    let queued = false;
    // Batched to one pass per frame; the timer covers pages that aren't drawing frames.
    const run = () => { if (queued) { queued = false; applyAll(); } };
    const mo = new MutationObserver(() => {
      if (queued) return;
      queued = true;
      requestAnimationFrame(run);
      setTimeout(run, 120);
    });
    mo.observe(doc.documentElement, { childList: true, subtree: true, attributes: true, attributeFilter: ['class', 'id', 'style'] });
  };
  if (doc.readyState === 'loading') doc.addEventListener('DOMContentLoaded', install, { once: true });
  else install();
  // Once more when everything has loaded, for content added without a mutation we watch.
  addEventListener('load', () => { install(); if (installed) applyAll(); }, { once: true });
})
