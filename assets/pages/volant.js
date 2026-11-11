'use strict';
/* Shared by Volant's own pages. Talks to the browser through a message handler
   that only pages on volant:// can use. */
/* The Volant mark: a V with a jet flying through it. Each copy gets its own
   ids so several can share a page. */
const MARK = '<defs><path id="vj" d="M256 54 C266 66 280 116 284 172 L414 346 L414 378 L288 346 L292 398 L338 434 L338 452 L256 434 L174 452 L174 434 L220 398 L224 346 L98 378 L98 346 L228 172 C232 116 246 66 256 54 Z" transform="translate(348 226) rotate(88) scale(0.38) translate(-256 -256)"/><mask id="vcut" maskUnits="userSpaceOnUse" x="0" y="0" width="512" height="512"><rect width="512" height="512" fill="#fff"/><use href="#vj" fill="#000" stroke="#000" stroke-width="110" stroke-linejoin="round"/></mask></defs><path d="M120 130 L256 384 L392 130" fill="none" stroke="currentColor" stroke-width="66" stroke-linecap="round" stroke-linejoin="round" mask="url(#vcut)"/><use href="#vj" fill="currentColor" stroke="currentColor" stroke-width="16" stroke-linejoin="round"/>';
let markCount = 0;
function logo() {
  const n = ++markCount;
  return `<svg viewBox="0 0 512 512" aria-hidden="true">${MARK.replaceAll('vj', 'vj' + n).replaceAll('vcut', 'vcut' + n)}</svg>`;
}

const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

/* Data the browser filled in when it served this page. Opened as a plain file
   (while designing), the page falls back to sample data. */
const V = (() => {
  try { return JSON.parse(document.getElementById('volant-data').textContent); } catch { return window.VOLANT_SAMPLE || {}; }
})();

const volant = {
  live: !!(window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.volant),
  send(cmd, value = '') {
    if (this.live) window.webkit.messageHandlers.volant.postMessage(cmd + '\n' + value);
    else console.log('[volant]', cmd, value);
  },
};

/* Letter tile used where a site has no icon. Colour comes from the host name. */
const TILE_COLORS = ['#1f6bff', '#7b5cf5', '#e0457b', '#f26b3a', '#e2a400', '#23a559', '#0fa5a5', '#5b6b82'];
function tileColor(host) {
  let h = 0;
  for (const c of host) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return TILE_COLORS[h % TILE_COLORS.length];
}
function siteIcon(host, url, size = 40) {
  const letter = esc((host.replace(/^www\./, '')[0] || '•').toUpperCase());
  const origin = (() => { try { return new URL(url).origin; } catch { return ''; } })();
  const img = origin ? `<img src="${esc(origin)}/favicon.ico" alt="" onload="this.parentNode.classList.add('has-img')" onerror="this.remove()">` : '';
  return `<span class="site-ic" style="--c:${tileColor(host)};--s:${size}px"><b>${letter}</b>${img}</span>`;
}

function shortcutKeys(s) {
  return s.split(' ').map((k) => `<kbd>${esc(k)}</kbd>`).join('');
}
