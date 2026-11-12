'use strict';
/* Live demos for Volant's website: a working model of the browser (command
   bar, blocking, Reader, sleeping tabs), an interactive blocker, a launch race
   and the motion around them. Plain DOM, no libraries. Loops only run while
   they're on screen, and everything holds still for reduced motion. */
(() => {
  const doc = document;
  doc.documentElement.classList.add('js');
  const still = matchMedia('(prefers-reduced-motion: reduce)').matches;
  const $ = (sel, root = doc) => root.querySelector(sel);
  const $$ = (sel, root = doc) => Array.from(root.querySelectorAll(sel));
  const sleep = (ms) => new Promise((ok) => setTimeout(ok, ms));
  const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

  const svg = (d, w = 2) => `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="${w}" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${d}</svg>`;
  const IC = {
    back: svg('<path d="M15 5l-7 7 7 7"/>'),
    fwd: svg('<path d="M9 5l7 7-7 7"/>'),
    reload: svg('<path d="M20 12a8 8 0 1 1-2.3-5.7M20 4v5h-5"/>'),
    search: svg('<circle cx="11" cy="11" r="6.5"/><path d="m16 16 4.5 4.5"/>'),
    lock: svg('<rect x="5" y="10" width="14" height="10" rx="2.5"/><path d="M8 10V7a4 4 0 0 1 8 0v3"/>'),
    shield: '<svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M12 2.5 3.5 5.7v6.1c0 5.3 3.7 8.6 8.5 9.7 4.8-1.1 8.5-4.4 8.5-9.7V5.7z"/></svg>',
    reader: svg('<path d="M5 5h14M5 10h14M5 15h9M5 20h6"/>'),
    plus: svg('<path d="M12 5v14M5 12h14"/>'),
    gear: svg('<circle cx="12" cy="12" r="3"/><path d="M12 2.5v3M12 18.5v3M4.6 4.6l2.1 2.1M17.3 17.3l2.1 2.1M2.5 12h3M18.5 12h3M4.6 19.4l2.1-2.1M17.3 6.7l2.1-2.1"/>'),
    sidebar: svg('<rect x="3.5" y="4.5" width="17" height="15" rx="3"/><path d="M9.5 4.5v15"/>'),
    clock: svg('<circle cx="12" cy="12" r="8"/><path d="M12 7.5V12l3 2"/>'),
    tab: svg('<rect x="3.5" y="5.5" width="17" height="13" rx="3"/><path d="M3.5 9.5h17"/>'),
    moon: svg('<path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z"/>'),
    pencil: svg('<path d="M4 20h4L19 9l-4-4L4 16z"/>'),
    cloud: svg('<path d="M7 18h10a4 4 0 0 0 .5-8A6 6 0 0 0 6 11a3.5 3.5 0 0 0 1 7z"/>'),
    cursor: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 3l14 8.2-6.2 1.4 3.6 6.6-2.6 1.4-3.6-6.6L5 18.2z" fill="#fff" stroke="#111" stroke-width="1.4" stroke-linejoin="round"/></svg>',
  };
  const markSvg = () => (typeof logo === 'function' ? logo() : '');
  const wordSvg = () => (typeof wordmark === 'function' ? wordmark() : '<b>Volant</b>');

  // ---------- visibility ----------
  /** Calls fn(true/false) as el comes into and out of view. */
  function watch(el, fn, threshold = 0.25) {
    if (!('IntersectionObserver' in window)) { fn(true); return; }
    new IntersectionObserver((es) => es.forEach((e) => fn(e.isIntersecting)), { threshold }).observe(el);
  }
  function once(el, fn, threshold = 0.3) {
    let done = false;
    watch(el, (v) => { if (v && !done) { done = true; fn(); } }, threshold);
  }
  /** A clock for a looping demo: waits that pause while it's off screen or the tab is hidden. */
  function clock(el) {
    let seen = false;
    watch(el, (v) => { seen = v; }, 0.15);
    return async (ms) => {
      await sleep(ms);
      while (!seen || doc.hidden) await sleep(300);
    };
  }

  // ---------- a fixed-size stage, scaled to fit ----------
  function stage(host, w, h, cls = '') {
    const st = doc.createElement('div');
    st.className = 'stage';
    st.style.setProperty('--w', w);
    st.style.setProperty('--h', h);
    const win = doc.createElement('div');
    win.className = 'win ' + cls;
    win.setAttribute('aria-hidden', 'true');
    st.appendChild(win);
    host.appendChild(st);
    const fit = () => st.style.setProperty('--k', st.clientWidth / w);
    fit();
    if ('ResizeObserver' in window) new ResizeObserver(fit).observe(st);
    else addEventListener('resize', fit);
    return { st, win, k: () => st.clientWidth / w };
  }

  // ---------- shared scenes ----------
  const homeScene = () => `
    <section class="scene home show">
      <div class="weather">${IC.cloud.replace('<svg', '<svg style="width:22px;height:22px;color:#a1a1a6"')}<div><b>8°</b><small>Cloudy · Toronto</small></div></div>
      <div class="wm">${wordSvg()}</div>
      <div class="field">${IC.search}<span>Search DuckDuckGo or enter an address</span></div>
      <div class="favs">
        ${[['N', '#e5484d', 'News'], ['M', '#2563eb', 'Mail'], ['F', '#a855f7', 'Figures'], ['W', '#0ea5e9', 'Weather'], ['R', '#16a34a', 'Recipes']]
          .map(([l, c, n]) => `<div class="fav"><b><i style="--c:${c}">${l}</i></b>${n}</div>`).join('')}
      </div>
      <div class="pencil">${IC.pencil}</div>
    </section>`;
  const articleScene = (extra = '') => `
    <section class="scene article ${extra}">
      <div class="ad top"><em>AD</em>Summer sale · up to 70% off<small>shop.example</small></div>
      <div class="site"><i></i>The Daily Example<span><u>World</u><u>Science</u><u>Travel</u></span></div>
      <div class="main">
        <div class="by">Science · 6 min read</div>
        <h4>How jet engines turn air into thrust</h4>
        <div class="ln"></div><div class="ln m"></div><div class="ln s"></div>
        <div class="ad inl"><em>AD</em>Try Premium free for 30 days<small>video.example</small></div>
        <div class="img"></div>
        <div class="ln"></div><div class="ln m"></div><div class="ln"></div><div class="ln s"></div>
      </div>
      <div class="rail"><div class="ad box"><em>AD</em>You won't believe<br>what's next<small>clicks.example</small></div><div class="ln"></div><div class="ln s"></div><div class="ln m"></div><div class="ln"></div></div>
      <div class="ad pop"><em>AD</em>Don't miss out!<small>popup.example</small></div>
    </section>`;
  const readerScene = () => `
    <section class="scene reader-view"><div class="col">
      <div class="src">The Daily Example · 6 min read</div>
      <h4>How jet engines turn air into thrust</h4>
      <div class="ln"></div><div class="ln"></div><div class="ln s"></div>
      <div class="img"></div>
      <div class="ln"></div><div class="ln"></div><div class="ln"></div><div class="ln s"></div>
    </div></section>`;

  /** A fake pointer that moves to elements and clicks them. */
  function pointer(win, k) {
    const cur = doc.createElement('div');
    cur.className = 'cursor';
    cur.innerHTML = IC.cursor;
    win.appendChild(cur);
    let x = 0, y = 0;
    const place = (nx, ny) => { x = nx; y = ny; cur.style.transform = `translate(${x}px,${y}px)`; };
    const at = (el, fx = 0.5, fy = 0.5) => {
      const r = el.getBoundingClientRect(), w = win.getBoundingClientRect(), s = k() || 1;
      return [(r.left - w.left + r.width * fx) / s, (r.top - w.top + r.height * fy) / s];
    };
    return {
      place,
      move: (el, fx, fy) => place(...at(el, fx, fy)),
      async click(el, wait, fx, fy) {
        place(...at(el, fx, fy));
        await wait(850);
        cur.classList.add('down');
        const rp = doc.createElement('i');
        rp.className = 'ripple';
        rp.style.left = x + 'px';
        rp.style.top = y + 'px';
        win.appendChild(rp);
        setTimeout(() => rp.remove(), 600);
        await wait(140);
        cur.classList.remove('down');
      },
    };
  }

  /** Counts el's text up from 0 to n. */
  function countTo(el, n, ms = 900, fmt = (v) => Math.round(v).toLocaleString('en-US')) {
    if (still) { el.textContent = fmt(n); return; }
    const t0 = performance.now();
    const step = (t) => {
      const p = Math.min(1, (t - t0) / ms);
      el.textContent = fmt(n * (1 - Math.pow(1 - p, 3)));
      if (p < 1) requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  }

  // ---------- 1. the hero: Volant, working ----------
  const HISTORY = [
    ['How jet engines turn air into thrust', 'news.example/science/jet-engines', 'clock'],
    ['Jet engine', 'encyclopedia.example/wiki/Jet_engine', 'clock'],
    ['Flight tracker', 'flights.example', 'tab'],
    ['Weekend forecast', 'weather.example/toronto', 'tab'],
    ['Design review notes', 'docs.example/design-review', 'clock'],
    ['Inbox (3)', 'mail.example', 'tab'],
    ['Jetstream maps', 'weather.example/jetstream', 'clock'],
    ['Engine sizes, explained', 'cars.example/engines', 'clock'],
    ['Recipes: weeknight pasta', 'recipes.example/pasta', 'clock'],
    ['Rust: the book', 'docs.example/rust/book', 'clock'],
  ];
  const mark = (text, q) => {
    const i = text.toLowerCase().indexOf(q.toLowerCase());
    return i < 0 || !q ? esc(text) : esc(text.slice(0, i)) + '<mark>' + esc(text.slice(i, i + q.length)) + '</mark>' + esc(text.slice(i + q.length));
  };
  /** Volant's command bar: matches across words in titles and addresses. */
  function cmdRows(q, max = 4) {
    const words = q.toLowerCase().split(/\s+/).filter(Boolean);
    const hits = HISTORY.filter(([t, u]) => words.every((w) => (t + ' ' + u).toLowerCase().includes(w))).slice(0, max - 1);
    const rows = [`<div class="row">${IC.search}<span class="tt">${esc(q)}</span><span class="u">Search DuckDuckGo</span></div>`];
    for (const [t, u, kind] of hits) {
      const w = words.find((w) => t.toLowerCase().includes(w)) || '';
      rows.push(`<div class="row">${IC[kind]}<span class="tt">${mark(t, w)}</span><span class="u">${kind === 'tab' ? 'Switch to tab' : esc(u.split('/')[0])}</span></div>`);
    }
    return { html: rows.join(''), count: hits.length };
  }

  function hero(host) {
    const { win, k } = stage(host, 1040, 620);
    const TABS = [['V', 'New Tab', 'transparent', true], ['M', 'Inbox (3)', '#2563eb'], ['F', 'Design review notes', '#a855f7'], ['W', 'Weekend forecast', '#0ea5e9'], ['T', 'Flight tracker', '#f97316']];
    win.innerHTML = `
      <aside class="side">
        <div class="lights"><i></i><i></i><i></i></div>
        <div class="navs">${IC.back}${IC.fwd}${IC.reload}</div>
        <div class="addr"><span class="lead">${IC.search}</span><span class="host">New Tab</span><span class="btn-ic reader" style="opacity:0">${IC.reader}</span><span class="btn-ic shield" style="opacity:0">${IC.shield}</span></div>
        <div class="newtab">${IC.plus}New Tab</div>
        <div class="tabs">${TABS.map(([l, n, c, sel]) => `<div class="tab${sel ? ' sel' : ''}"><b style="--c:${c}">${l === 'V' ? markSvg().replace('<svg', '<svg style="width:16px;height:16px"') : l}</b><span>${esc(n)}</span><i class="zz">${IC.moon}</i></div>`).join('')}</div>
        <div class="foot">${IC.gear}<span class="upd">Update available</span>${IC.sidebar}</div>
      </aside>
      <main class="pane">
        ${homeScene()}${articleScene('blocked')}${readerScene()}
        <div class="scrim"></div>
        <div class="cmd"><div class="in">${IC.search}<span class="q"></span><span class="caret"></span></div><div class="rows"></div><div class="meta"></div></div>
        <div class="toast"><i>${IC.shield}</i><div class="msg"></div></div>
      </main>`;
    const P = pointer(win, k);
    const wait = clock(host);
    const el = {
      home: $('.home', win), article: $('.article', win), reader: $('.reader-view', win),
      host: $('.host', win), lead: $('.lead', win), shield: $('.shield', win), readerBtn: $('.reader', win),
      q: $('.cmd .q', win), rows: $('.cmd .rows', win), meta: $('.cmd .meta', win), toast: $('.toast', win), msg: $('.toast .msg', win),
      tabs: $$('.tab', win), newtab: $('.newtab', win),
    };
    const chips = $$('.caption span', host.parentNode);
    const chip = (key) => chips.forEach((c) => c.classList.toggle('on', c.dataset.k === key));
    const scene = (name) => ['home', 'article', 'reader'].forEach((s) => el[s].classList.toggle('show', s === name));
    const toast = (html, icon = 'shield') => { $('i', el.toast).innerHTML = IC[icon]; el.msg.innerHTML = html; el.toast.classList.add('show'); };
    const firstTab = el.tabs[0];
    const tabTitle = (t) => { $('span', firstTab).textContent = t; };

    const reset = () => {
      scene('home');
      el.host.textContent = 'New Tab';
      el.lead.innerHTML = IC.search;
      el.readerBtn.style.opacity = 0;
      el.shield.style.opacity = 0;
      el.shield.classList.remove('off');
      el.readerBtn.classList.remove('on');
      el.article.classList.add('blocked');
      el.toast.classList.remove('show');
      el.tabs.forEach((t) => t.classList.remove('asleep'));
      tabTitle('New Tab');
      $('b', firstTab).innerHTML = markSvg().replace('<svg', '<svg style="width:16px;height:16px"');
      $('b', firstTab).style.setProperty('--c', 'transparent');
      win.classList.remove('cmd-open');
      el.q.textContent = '';
      el.rows.innerHTML = '';
      chip('');
    };

    if (still) { // one calm frame: an article, blocked, with the shield on
      reset(); scene('article'); el.host.textContent = 'news.example'; el.lead.innerHTML = IC.lock;
      el.shield.style.opacity = 1; el.readerBtn.style.opacity = 1; tabTitle('How jet engines turn air into thrust');
      P.place(860, 560); return;
    }

    (async () => {
      P.place(700, 520);
      for (;;) {
        reset();
        await wait(1300);
        // ⌘L, and type: suggestions from history and open tabs as you go.
        chip('cmd');
        P.move(el.newtab, 0.6, 0.5);
        win.classList.add('cmd-open');
        await wait(350);
        const query = 'jet engines';
        for (let i = 1; i <= query.length; i++) {
          const q = query.slice(0, i);
          el.q.textContent = q;
          const r = cmdRows(q);
          el.rows.innerHTML = r.html;
          el.meta.textContent = `${r.count} from history and open tabs · as you type`;
          await wait(70 + Math.random() * 60);
        }
        await wait(450);
        const rows = $$('.row', el.rows);
        if (rows[1]) { rows[1].classList.add('sel'); await wait(500); }
        win.classList.remove('cmd-open');
        // The page loads with blocking already on: no ads ever arrive.
        chip('block');
        tabTitle('How jet engines turn air into thrust');
        $('b', firstTab).textContent = 'D';
        $('b', firstTab).style.setProperty('--c', '#e5484d');
        el.host.textContent = 'news.example';
        el.lead.innerHTML = IC.lock;
        el.shield.style.opacity = 1;
        el.readerBtn.style.opacity = 1;
        scene('article');
        await wait(500);
        toast('<div><b class="cnt">0</b> ads and trackers blocked<small>Powered by uBlock</small></div>');
        countTo($('.cnt', el.msg), 23, 1100);
        await wait(2300);
        el.toast.classList.remove('show');
        // The shield: blocking off for this site, and back on.
        await P.click(el.shield, wait);
        el.shield.classList.add('off');
        el.article.classList.remove('blocked');
        toast('<div>Blocking off for news.example<small>The site is trusted until you turn it back on</small></div>');
        await wait(2000);
        await P.click(el.shield, wait);
        el.shield.classList.remove('off');
        el.article.classList.add('blocked');
        toast('<div>Blocking on for news.example<small>Powered by uBlock</small></div>');
        await wait(1500);
        el.toast.classList.remove('show');
        // Reader.
        chip('reader');
        await P.click(el.readerBtn, wait);
        el.readerBtn.classList.add('on');
        scene('reader');
        await wait(2600);
        await P.click(el.readerBtn, wait);
        el.readerBtn.classList.remove('on');
        scene('article');
        await wait(900);
        // Tabs you haven't looked at give their memory back.
        chip('sleep');
        P.move(el.tabs[3], 0.75, 0.5);
        for (const t of [el.tabs[2], el.tabs[3]]) { t.classList.add('asleep'); await wait(450); }
        toast(`<div>2 tabs asleep<small>They hand their memory back, and wake when you return</small></div>`, 'moon');
        await wait(2600);
        el.toast.classList.remove('show');
        await wait(600);
        // Fade out to a new tab, and round again.
        win.style.transition = 'opacity .4s';
        win.style.opacity = 0;
        await wait(450);
        reset();
        win.style.opacity = 1;
      }
    })();
  }

  // ---------- 2. the blocker, to try ----------
  const REQUESTS = [
    ['news.example/article', 'page'], ['news.example/style.css', 'page'], ['adserver.example/banner.js', 'ad'],
    ['metrics.example/collect?id=…', 'tracker'], ['news.example/app.js', 'page'], ['pixel.example/p.gif', 'tracker'],
    ['cdn.adnet.example/video-ad.mp4', 'ad'], ['images.example/engine.jpg', 'page'], ['fingerprint.example/fp.js', 'tracker'],
    ['popup.example/offer.js', 'ad'], ['social.example/share-widget.js', 'tracker'], ['adserver.example/slot?rail', 'ad'],
    ['news.example/fonts/serif.woff2', 'page'], ['beacon.example/heartbeat', 'tracker'], ['clicks.example/recommend.js', 'ad'],
  ];
  function blocker(root) {
    const pagecard = $('.pagecard', root);
    const { win } = stage(pagecard, 720, 470, 'compact');
    win.innerHTML = `<div class="top"><div class="lights"><i></i><i></i><i></i></div><div class="addr"><span class="lead">${IC.lock}</span><span class="host">news.example/science/jet-engines</span><span class="btn-ic shield">${IC.shield}</span></div></div>
      <main class="pane">${articleScene('show')}</main>`;
    const art = $('.article', win), sh = $('.shield', win);
    const log = $('.log', root), nb = $('.n-blocked', root), na = $('.n-ads', root), btn = $('.master', root);
    let on = false, run = 0;
    const set = async (v) => {
      on = v;
      const my = ++run;
      root.classList.toggle('on', on);
      btn.setAttribute('aria-pressed', on);
      art.classList.toggle('blocked', on);
      sh.classList.toggle('off', !on);
      log.innerHTML = '';
      let blocked = 0;
      nb.textContent = '0';
      countTo(na, on ? 4 : 0, 500);
      // Reload: the page's requests stream in, and the blocker answers each.
      for (const [url, kind] of REQUESTS) {
        if (my !== run) return;
        const stop = on && kind !== 'page';
        if (stop) blocked++;
        const row = doc.createElement('div');
        row.innerHTML = `<span>${esc(url)}</span><b class="${stop ? 'x' : ''}">${stop ? 'blocked' : 'loaded'}</b>`;
        log.prepend(row);
        nb.textContent = blocked;
        await sleep(still ? 0 : 110);
      }
    };
    btn.addEventListener('click', () => set(!on));
    set(false);
    once(root, async () => { await sleep(900); if (!on) set(true); }, 0.4);
  }

  // ---------- 3. the launch race ----------
  const BENCH = {
    launch: [['Volant', 0.30], ['Chrome', 1.47], ['Firefox', 2.31], ['Safari', 3.64]],
    size: [['Volant', 2], ['Safari', 29], ['Firefox', 521], ['Chrome', 732]],
    mem: [['Safari', 169], ['Volant', 320], ['Chrome', 515], ['Firefox', 821]],
  };
  function race(root) {
    const lanes = BENCH.launch;
    const max = Math.max(...lanes.map((l) => l[1]));
    root.innerHTML = `<div class="head"><h3>Launch to window, in real time</h3><span class="clock">0.00 s</span><button class="again" type="button">Run again</button></div>` +
      lanes.map(([who, t]) => `<div class="lane${who === 'Volant' ? ' us' : ''}"><span class="who">${who}</span><div class="track"><div class="run"><i class="win-ic"></i></div></div><span class="t">–</span></div>`).join('') +
      `<p class="note">Each bar runs for as long as that browser took to put its first window on screen, in the latest benchmark on GitHub's Apple Silicon Macs.</p>`;
    const clk = $('.clock', root), rows = $$('.lane', root);
    let raf = 0;
    const go = () => {
      cancelAnimationFrame(raf);
      rows.forEach((r) => { r.classList.remove('done'); $('.run', r).style.width = '0'; $('.t', r).textContent = '–'; });
      const t0 = performance.now();
      const frame = (now) => {
        const s = still ? max : (now - t0) / 1000;
        clk.textContent = Math.min(s, max).toFixed(2) + ' s';
        lanes.forEach(([, t], i) => {
          const r = rows[i];
          $('.run', r).style.width = (Math.min(s, t) / max) * 100 + '%';
          if (s >= t && !r.classList.contains('done')) { r.classList.add('done'); $('.t', r).textContent = t.toFixed(2) + ' s'; }
        });
        if (s < max) raf = requestAnimationFrame(frame);
      };
      raf = requestAnimationFrame(frame);
    };
    $('.again', root).addEventListener('click', go);
    once(root, go, 0.5);
  }

  // ---------- 4. bar charts that grow in ----------
  function chart(el, { rows, unit, digits }) {
    const max = Math.max(...rows.map((r) => r[1]));
    el.innerHTML = rows.map(([who, v]) => {
      const pct = Math.max(2, (v / max) * 100);
      const label = v.toFixed(digits) + unit;
      const inside = pct > 30;
      return `<div class="bar${who === 'Volant' ? ' us' : ''}"><span class="who">${who}</span><div class="track">` +
        (inside ? `<div class="fill" data-w="${pct}">${label}</div>` :
          `<div style="display:flex;align-items:center;gap:10px;height:100%"><div class="fill" data-w="${pct}"></div><div class="fill out">${label}</div></div>`) +
        `</div></div>`;
    }).join('');
    const card = el.closest('.chart') || el;
    const grow = () => { card.classList.add('in'); $$('.fill[data-w]', el).forEach((f) => { f.style.width = f.dataset.w + '%'; }); };
    if (still) grow();
    else { $$('.fill[data-w]', el).forEach((f) => { f.style.width = '0'; }); once(card, grow, 0.4); }
  }

  // ---------- 5. sleeping tabs ----------
  function sleeping(root) {
    const TABS = [['D', 'How jet engines turn air into thrust', '#e5484d', 182], ['M', 'Inbox (3)', '#2563eb', 146], ['F', 'Design review notes', '#a855f7', 214],
      ['W', 'Weekend forecast', '#0ea5e9', 96], ['T', 'Flight tracker', '#f97316', 168], ['R', 'Recipes: weeknight pasta', '#16a34a', 121]];
    const top = Math.max(...TABS.map((t) => t[3]));
    root.innerHTML = TABS.map(([l, n, c]) => `<div class="row"><b style="--c:${c}">${l}</b><span class="nm">${esc(n)}</span><span class="m"><i></i></span><span class="mb"></span></div>`).join('') +
      `<div class="total"><span>Memory for these tabs (illustration)</span><b class="sum"></b></div>`;
    const rows = $$('.row', root), sum = $('.sum', root);
    const state = TABS.map(() => true);
    let total = 0;
    const draw = () => {
      let t = 0;
      rows.forEach((r, i) => {
        const mb = state[i] ? TABS[i][3] : 2;
        t += mb;
        r.classList.toggle('asleep', !state[i]);
        $('i', r).style.width = (mb / top) * 100 + '%';
        $('.mb', r).textContent = state[i] ? mb + ' MB' : 'asleep';
      });
      countFrom(sum, total, t);
      total = t;
    };
    const countFrom = (el, a, b) => {
      if (still) { el.textContent = b + ' MB'; return; }
      const t0 = performance.now();
      const f = (now) => { const p = Math.min(1, (now - t0) / 700); el.textContent = Math.round(a + (b - a) * p) + ' MB'; if (p < 1) requestAnimationFrame(f); };
      requestAnimationFrame(f);
    };
    rows[0].classList.add('active');
    draw();
    if (still) { [2, 3, 4, 5].forEach((i) => { state[i] = false; }); draw(); return; }
    const wait = clock(root);
    (async () => {
      for (;;) {
        await wait(1600);
        for (const i of [5, 3, 4, 2]) { state[i] = false; draw(); await wait(1100); }
        await wait(1400);
        // Come back to one: it wakes and reloads.
        rows[0].classList.remove('active'); rows[3].classList.add('active');
        state[3] = true; draw();
        await wait(1800);
        rows[3].classList.remove('active'); rows[0].classList.add('active');
        state.fill(true); draw();
      }
    })();
  }

  // ---------- 6. command bar on its own ----------
  function commandBar(host) {
    const { win } = stage(host, 640, 400, 'compact');
    win.innerHTML = `<div class="top"><div class="lights"><i></i><i></i><i></i></div><div class="addr"><span class="lead">${IC.search}</span><span class="host">New Tab</span></div></div>
      <main class="pane">${homeScene()}<div class="scrim"></div>
      <div class="cmd" style="top:36px;width:500px;margin-left:-250px"><div class="in">${IC.search}<span class="q"></span><span class="caret"></span></div><div class="rows"></div><div class="meta"></div></div></main>`;
    win.classList.add('cmd-open');
    const q = $('.q', win), rows = $('.rows', win), meta = $('.meta', win);
    const show = (s) => { q.textContent = s; const r = cmdRows(s, 5); rows.innerHTML = s ? r.html : ''; meta.textContent = s ? `${r.count} from history and open tabs` : 'History, open tabs and search, in one field'; if (s) $('.row', rows).classList.add('sel'); };
    if (still) { show('jet'); return; }
    const wait = clock(host);
    (async () => {
      for (;;) {
        for (const word of ['jet', 'weather', 'design notes', 'rust book']) {
          for (let i = 1; i <= word.length; i++) { show(word.slice(0, i)); await wait(90 + Math.random() * 70); }
          await wait(1500);
          for (let i = word.length - 1; i >= 0; i--) { show(word.slice(0, i)); await wait(28); }
          await wait(350);
        }
      }
    })();
  }

  // ---------- 7. Reader ----------
  function reader(host) {
    const { win, k } = stage(host, 640, 420, 'compact');
    win.innerHTML = `<div class="top"><div class="lights"><i></i><i></i><i></i></div><div class="addr"><span class="lead">${IC.lock}</span><span class="host">news.example/science/jet-engines</span><span class="btn-ic reader">${IC.reader}</span><span class="btn-ic shield">${IC.shield}</span></div></div>
      <main class="pane">${articleScene('show')}${readerScene()}</main>`;
    const art = $('.article', win), rd = $('.reader-view', win), btn = $('.reader', win);
    art.classList.remove('blocked');
    const P = pointer(win, k);
    if (still) { rd.classList.add('show'); art.classList.remove('show'); btn.classList.add('on'); return; }
    const wait = clock(host);
    (async () => {
      P.place(420, 330);
      for (;;) {
        await wait(1400);
        await P.click(btn, wait);
        btn.classList.add('on'); art.classList.remove('show'); rd.classList.add('show');
        await wait(2800);
        await P.click(btn, wait);
        btn.classList.remove('on'); rd.classList.remove('show'); art.classList.add('show');
      }
    })();
  }

  // ---------- wire it all up ----------
  const markEl = $('#mark');
  if (markEl && typeof mountWordmark === 'function') mountWordmark(markEl, 22);
  $$('[data-demo]').forEach((el) => {
    const kind = el.dataset.demo;
    try {
      if (kind === 'hero') hero(el);
      else if (kind === 'blocker') blocker(el);
      else if (kind === 'race') race(el);
      else if (kind === 'sleep') sleeping(el);
      else if (kind === 'cmd') commandBar(el);
      else if (kind === 'reader') reader(el);
    } catch (e) { console.error('demo', kind, e); }
  });
  const UNITS = { launch: [' s', 2], size: [' MB', 0], mem: [' MB', 0] };
  for (const [key, rows] of Object.entries(BENCH)) {
    const el = doc.getElementById('c-' + key);
    if (el) chart(el, { rows, unit: UNITS[key][0], digits: UNITS[key][1] });
  }
  // Numbers count up as they come into view: <b data-count="240" data-suffix="k">.
  $$('[data-count]').forEach((el) => {
    const n = Number(el.dataset.count), dec = Number(el.dataset.dec || 0), small = el.querySelector('small');
    const tail = small ? small.outerHTML : '';
    const fmt = (v) => (dec ? v.toFixed(dec) : Math.round(v).toLocaleString('en-US')) + tail;
    if (still) return;
    el.innerHTML = fmt(0);
    once(el, () => {
      const t0 = performance.now();
      const f = (now) => { const p = Math.min(1, (now - t0) / 1300); el.innerHTML = fmt(n * (1 - Math.pow(1 - p, 3))); if (p < 1) requestAnimationFrame(f); };
      requestAnimationFrame(f);
    }, 0.6);
  });
  // Sections slide up into place as you reach them.
  const REVEAL = '.page-head .wrap > *, section .kicker, section h2, section > .wrap > .lead, .grid > *, .numbers > *, .charts > *, .split > *, .steps > li, details, .table, .credit, .shot, .bdemo, .race, .pipe .node, .mini, .install, .final .ctas';
  $$(REVEAL).forEach((el) => {
    if (el.closest('.hero')) return;
    el.classList.add('reveal');
    const sibs = Array.from(el.parentNode.children).filter((c) => c.matches(REVEAL));
    const i = sibs.indexOf(el);
    if (i > 0 && i < 4) el.classList.add('d' + i);
    once(el, () => el.classList.add('in'), 0.12);
  });
})();
