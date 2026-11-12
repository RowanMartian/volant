/* Volant's element picker ("Block Element…"), in the spirit of uBlock Origin's.
   Hover to highlight, click to pick, adjust, then Block: Volant saves
   `site##selector` to My filters. The UI lives in a closed shadow root so the
   page can't style or script it. Written for Volant (MIT). */
(function (nonce) {
  'use strict';
  if (window.__volantPicker) return;
  window.__volantPicker = true;
  const doc = document;
  const host = doc.createElement('volant-picker');
  host.style.cssText = 'all:initial;position:fixed;inset:0;z-index:2147483647;pointer-events:none';
  const root = host.attachShadow({ mode: 'closed' });
  root.innerHTML = `<style>
    *{box-sizing:border-box;font:13px/1.4 -apple-system,BlinkMacSystemFont,system-ui,sans-serif}
    .hl{position:fixed;background:rgba(31,149,255,.22);border-radius:6px;pointer-events:none;transition:all .06s}
    .hl.many{background:rgba(31,149,255,.14)}
    .panel{position:fixed;right:18px;bottom:18px;width:min(420px,calc(100vw - 36px));background:#1d1c24;color:#f5f5f7;border-radius:16px;padding:14px;pointer-events:auto}
    .top{display:flex;align-items:center;gap:8px;margin-bottom:10px}
    .top b{font-weight:650;flex:1}
    .top small{color:#a1a1aa}
    input{width:100%;height:34px;border:0;border-radius:9px;padding:0 10px;background:#2c2b34;color:#f5f5f7;font:12.5px ui-monospace,SFMono-Regular,Menlo,monospace;outline:none}
    .row{display:flex;gap:8px;margin-top:10px}
    button{height:32px;border:0;border-radius:9px;padding:0 14px;background:#2c2b34;color:#f5f5f7;font-weight:600;cursor:pointer}
    button:hover{background:#383742}
    button.go{background:#1f95ff;color:#fff;margin-left:auto}
    button.go:hover{background:#46a8ff}
    .hint{color:#a1a1aa;font-size:12px}
    .badge{font-size:10.5px;color:#a1a1aa}
  </style>
  <div class="panel" hidden><div class="top"><b>Block element</b><small class="count"></small></div>
    <input class="sel" spellcheck="false" aria-label="Filter">
    <div class="row"><button class="up" title="Pick the element around it">Wider</button><button class="down" title="Back to a smaller element">Narrower</button>
    <button class="cancel">Cancel</button><button class="go">Block</button></div>
    <div class="row"><span class="badge">Powered by uBlock</span></div></div>
  <div class="tip panel" style="width:auto"><span class="hint">Click the part of the page to block · Esc to cancel</span></div>`;
  (doc.body || doc.documentElement).appendChild(host);
  const $ = (s) => root.querySelector(s);
  const panel = $('.panel'), tip = $('.tip'), input = $('.sel'), count = $('.count');
  let highlights = [];
  let picked = null;
  const chain = [];

  const clearHl = () => { highlights.forEach((h) => h.remove()); highlights = []; };
  const highlight = (els) => {
    clearHl();
    for (const el of els.slice(0, 50)) {
      const r = el.getBoundingClientRect();
      if (!r.width || !r.height) continue;
      const h = doc.createElement('div');
      h.className = 'hl' + (els.length > 1 ? ' many' : '');
      Object.assign(h.style, { left: r.left + 'px', top: r.top + 'px', width: r.width + 'px', height: r.height + 'px' });
      root.appendChild(h);
      highlights.push(h);
    }
  };
  const stable = (s) => s && s.length < 40 && !/\d{4,}/.test(s) && !/^[a-z0-9]{8,}$/i.test(s);
  const selectorFor = (el) => {
    if (el.id && stable(el.id) && /^[A-Za-z][\w-]*$/.test(el.id)) return '#' + CSS.escape(el.id);
    let s = el.localName;
    const classes = Array.from(el.classList).filter(stable).slice(0, 3);
    if (classes.length) s += classes.map((c) => '.' + CSS.escape(c)).join('');
    for (const a of ['data-ad', 'data-testid', 'aria-label', 'role']) if (el.hasAttribute(a) && stable(el.getAttribute(a))) { s += `[${a}="${CSS.escape(el.getAttribute(a))}"]`; break; }
    if (s === el.localName && el.parentElement && el.parentElement !== doc.body) s = selectorFor(el.parentElement) + ' > ' + s;
    return s;
  };
  const matches = (sel) => { try { return Array.from(doc.querySelectorAll(sel)).filter((e) => !host.contains(e)); } catch { return []; } };
  const show = () => {
    const els = matches(input.value);
    count.textContent = els.length === 1 ? '1 element' : `${els.length} elements`;
    highlight(els);
  };
  const pick = (el) => {
    picked = el;
    input.value = selectorFor(el);
    tip.hidden = true;
    panel.hidden = false;
    show();
  };

  const under = (e) => {
    host.style.display = 'none';
    const el = doc.elementFromPoint(e.clientX, e.clientY);
    host.style.display = '';
    return el && el !== doc.documentElement && el !== doc.body ? el : null;
  };
  const onMove = (e) => { if (picked) return; const el = under(e); if (el) highlight([el]); };
  const onClick = (e) => {
    if (e.composedPath().includes(host)) return;
    e.preventDefault(); e.stopPropagation();
    if (picked) return;
    const el = under(e);
    if (el) pick(el);
  };
  const stop = (e) => { if (!e.composedPath().includes(host) && !picked) { e.preventDefault(); e.stopPropagation(); } };
  const finish = () => {
    clearHl();
    host.remove();
    window.__volantPicker = false;
    removeEventListener('mousemove', onMove, true);
    removeEventListener('click', onClick, true);
    for (const t of ['mousedown', 'mouseup', 'pointerdown', 'pointerup']) removeEventListener(t, stop, true);
    removeEventListener('keydown', onKey, true);
  };
  const block = () => {
    const sel = input.value.trim();
    if (!sel || matches(sel).length === 0) return;
    for (const el of matches(sel)) el.style.setProperty('display', 'none', 'important');
    try { window.webkit.messageHandlers.volantPicker.postMessage(JSON.stringify({ nonce, selector: sel, host: location.hostname })); } catch {}
    finish();
  };
  const onKey = (e) => {
    if (e.key === 'Escape') { e.preventDefault(); finish(); }
    else if (e.key === 'Enter' && picked) { e.preventDefault(); block(); }
  };
  addEventListener('mousemove', onMove, true);
  addEventListener('click', onClick, true);
  for (const t of ['mousedown', 'mouseup', 'pointerdown', 'pointerup']) addEventListener(t, stop, true);
  addEventListener('keydown', onKey, true);
  input.addEventListener('input', show);
  $('.up').onclick = () => { if (picked && picked.parentElement && picked.parentElement !== doc.body) { chain.push(picked); pick(picked.parentElement); } };
  $('.down').onclick = () => { if (chain.length) pick(chain.pop()); };
  $('.cancel').onclick = finish;
  $('.go').onclick = block;
})
