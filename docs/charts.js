'use strict';
/* Benchmark results from the latest run of scripts/bench/run.sh in CI, drawn
   as flat bar charts into any element with an id below. */
const BENCH = {
  launch: { rows: [['Volant', 0.30], ['Chrome', 1.47], ['Firefox', 2.31], ['Safari', 3.64]], unit: ' s', digits: 2 },
  size: { rows: [['Volant', 2], ['Safari', 29], ['Firefox', 521], ['Chrome', 732]], unit: ' MB', digits: 0 },
  mem: { rows: [['Safari', 169], ['Volant', 320], ['Chrome', 515], ['Firefox', 821]], unit: ' MB', digits: 0 },
};
function drawChart(el, { rows, unit, digits }) {
  const max = Math.max(...rows.map((r) => r[1]));
  el.innerHTML = rows.map(([who, v]) => {
    const pct = Math.max(2, (v / max) * 100);
    const label = v.toFixed(digits) + unit;
    const inside = pct > 30;
    return `<div class="bar${who === 'Volant' ? ' us' : ''}"><span class="who">${who}</span><div class="track">` +
      (inside ? `<div class="fill" style="width:${pct}%">${label}</div>` :
        `<div style="display:flex;align-items:center;gap:10px;height:100%"><div class="fill" style="width:${pct}%"></div><div class="fill out">${label}</div></div>`) +
      `</div></div>`;
  }).join('');
}
for (const [key, data] of Object.entries(BENCH)) {
  const el = document.getElementById('c-' + key);
  if (el) drawChart(el, data);
}
