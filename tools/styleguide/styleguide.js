'use strict';
// The style guide's own script (#318): theme switch, the token tables drawn from census.js, placeholder pictures.
// It never builds a component: the samples in index.html are plain HTML so they can be read and copied as they are.

// ---- theme: 系统 / 亮 / 暗. Sets html[data-theme], exactly what web/static/theme.js sets in the app ----
const THEME_KEY = 'sg.theme';
const media = matchMedia('(prefers-color-scheme: dark)');
const readPref = () => { try { return localStorage.getItem(THEME_KEY) || ''; } catch { return ''; } };
function applyTheme() {
  const pref = readPref();
  document.documentElement.dataset.theme = pref === 'dark' || (pref !== 'light' && media.matches) ? 'dark' : 'light';
  for (const b of document.querySelectorAll('[data-sg-theme]')) b.classList.toggle('on', b.dataset.sgTheme === pref);
}
media.addEventListener('change', applyTheme);
document.addEventListener('click', e => {
  const b = e.target.closest('[data-sg-theme]'); if (!b) return;
  try { b.dataset.sgTheme ? localStorage.setItem(THEME_KEY, b.dataset.sgTheme) : localStorage.removeItem(THEME_KEY); } catch { /* private mode */ }
  applyTheme();
});
applyTheme();

// ---- placeholder pictures: the app's covers and thumbnails are <img>; here they are coloured blocks ----
const PH = ['#b9bdf3|#f0d4e4', '#a9d8cf|#e9e2b8', '#d0b5ec|#a9c6f0', '#f0c9a8|#e6a9b8', '#a9c6f0|#c9f0d8'];
function placeholders() {
  for (const img of document.querySelectorAll('img[data-ph]')) {
    const [a, b] = PH[Number(img.dataset.ph) % PH.length].split('|');
    img.src = 'data:image/svg+xml,' + encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" width="120" height="150"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${a}"/><stop offset="1" stop-color="${b}"/></linearGradient></defs><rect width="120" height="150" fill="url(#g)"/></svg>`);
    img.alt = '';
  }
}

// ---- tokens drawn from census.js (python3 tools/styleguide/census.py) ----
const C = window.CENSUS;
const esc = s => String(s).replace(/[&<>"]/g, c => ({'&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;'}[c]));
const NON_COLOR = new Set(['--sh', '--mono', '--ease', '--sheet-shadow']);
const swatch = (name, value, uses) => `<div class="sg-sw"><span style="background:${esc(value)}"></span><div><b>${name}</b><small>${esc(value)}${uses != null ? ` · ${uses} 处` : ''}</small></div></div>`;
const bar = (n, max) => `<i class="sg-bar" style="width:${Math.max(3, Math.round(n / max * 90))}px"></i>`;

function colorTokens() {
  if (!C) return;
  const {light, dark, uses} = C.themeVars;
  const rows = theme => Object.entries(theme).filter(([k]) => !NON_COLOR.has(k)).map(([k, v]) => swatch(k, v, uses[k] || 0)).join('');
  document.getElementById('tok-colors').innerHTML =
    `<div class="sg-themes"><div class="l"><h5>亮色 :root</h5><div class="sg-swatches">${rows(light)}</div></div>` +
    `<div class="d"><h5>暗色 :root[data-theme=dark]（只列重新定义的变量；用 var() 的地方自动换）</h5><div class="sg-swatches">${rows(dark)}</div></div></div>`;
  const other = Object.entries(light).filter(([k]) => NON_COLOR.has(k)).map(([k, v]) => `<tr><td><code>${k}</code></td><td class="ex">${esc(v)}</td><td class="n">${uses[k] || 0} 处</td></tr>`).join('');
  document.getElementById('tok-other').innerHTML = `<table class="sg-table"><tr><th>变量</th><th>值（亮色）</th><th>引用</th></tr>${other}` +
    `<tr><td><code>--tour-dim</code></td><td class="ex">只在暗色定义：${esc(dark['--tour-dim'] || '')}；亮色用引用处的回退值 rgba(14,14,24,.46)</td><td class="n">${uses['--tour-dim'] || 0} 处</td></tr></table>`;
  const lit = C.literalHexColors;
  document.getElementById('tok-literal').innerHTML =
    `<p class="sg-cap">app.css 里不经变量、直接写死的十六进制颜色：<b>${lit.kinds}</b> 种取值，共 <b>${lit.uses}</b> 处；另有 <b>${C.rgbaCalls}</b> 处 rgb()/rgba() 调用。` +
    `暗色模式另有 <b>${C.darkOverrideRules}</b> 条单独覆盖规则（选择器含 <code>data-theme=dark</code>，不含上面那个变量块）。下面按出现次数列前 24 种。</p>` +
    `<div class="sg-scroll"><table class="sg-table"><tr><th></th><th>取值</th><th>处</th><th>例（选择器）</th></tr>${lit.table.slice().sort((a, b) => b.uses - a.uses).slice(0, 24).map(r => `<tr><td><span class="sg-sw" style="display:block"><span style="background:${r.value}"></span></span></td><td><code>${r.value}</code></td><td class="n">${r.uses}</td><td class="ex">${r.examples.map(esc).join(' · ')}</td></tr>`).join('')}</table></div>`;
}

function table(id, key, render, head = '取值') {
  const d = C[key], max = Math.max(...d.table.map(r => r.uses));
  document.getElementById(id).innerHTML =
    `<p class="sg-cap"><b>${d.kinds}</b> 种取值，共 <b>${d.uses}</b> 处声明。按“${head}”从小到大；“例”是用到它的前几个选择器。</p>` +
    `<div class="sg-scroll"><table class="sg-table"><tr><th>${head}</th><th>样子</th><th>处</th><th></th><th>例（选择器）</th></tr>${d.table.map(r =>
      `<tr><td><code>${esc(r.value)}</code></td><td>${render(r.value)}</td><td class="n">${r.uses}</td><td>${bar(r.uses, max)}</td><td class="ex">${r.examples.map(esc).join(' · ')}</td></tr>`).join('')}</table></div>`;
}

function scalarTokens() {
  if (!C) return;
  colorTokens();
  const px = v => /^[\d.]+px$/.test(v) ? v : '';
  table('tok-fontsize', 'fontSize', v => px(v.replace(' (font 简写)', '')) ? `<span style="font-size:${v.replace(' (font 简写)', '')}">Aa 图库 Illustash</span>` : '<span class="sg-cap">不是固定字号</span>', '字号');
  table('tok-radius', 'radius', v => /^[\d.]+px$/.test(v) || v === '50%' ? `<span class="sg-rad"><span style="border-radius:${v}"></span></span>` : '<span class="sg-cap">复合写法</span>', '圆角');
  table('tok-gap', 'gap', v => /^[\d.]+px$/.test(v) ? `<span class="sg-gapbar"><i style="width:${v}"></i><i style="width:${v};margin-left:0"></i></span>` : '', 'gap');
  table('tok-height', 'height', v => { const m = v.match(/: ([\d.]+px)$/); return m ? `<span class="sg-bar" style="width:${Math.min(160, parseFloat(m[1]))}px;height:${Math.min(parseFloat(m[1]), 28)}px;opacity:.25;border-radius:4px"></span>` : ''; }, '高度');
  const z = C.zIndex;
  document.getElementById('tok-z').innerHTML =
    `<p class="sg-cap"><b>${z.kinds}</b> 种 z-index 取值，共 <b>${z.uses}</b> 处。从上到下是层叠从高到低；同一数值在不同组件里并不等于它们互相“同层”，只是数字相同。</p>` +
    `<div class="sg-zstack">${z.table.slice().reverse().map(r => `<div><b>${r.value}</b><span>${r.uses} 处 · ${r.examples.map(esc).join(' · ')}</span></div>`).join('')}</div>`;
  const pad = C.padding;
  document.getElementById('tok-padding').innerHTML =
    `<p class="sg-cap"><code>padding</code> 简写共 <b>${pad.kinds}</b> 种取值组合，<b>${pad.uses}</b> 处声明（不含 padding-left 等单边写法）。下面是出现次数最多的前 24 种；其余基本各出现 1 次。</p>` +
    `<div class="sg-scroll"><table class="sg-table"><tr><th>padding</th><th>处</th><th>例（选择器）</th></tr>${pad.table.map(r => `<tr><td><code>${esc(r.value)}</code></td><td class="n">${r.uses}</td><td class="ex">${r.examples.map(esc).join(' · ')}</td></tr>`).join('')}</table></div>`;
  document.getElementById('census-meta').textContent = `统计对象：${C.source}，${C.lines} 行、${C.rules} 条规则（含 @media 内的规则）`;
}

// ---- is census.js older than app.css? Only answerable over http (a file:// page may not read another file) ----
async function staleCheck() {
  if (!C || location.protocol === 'file:') return;
  try {
    const css = await (await fetch('../../web/static/app.css', {cache: 'no-store'})).text();
    if (css.split('\n').length !== C.lines) document.getElementById('sg-stale').classList.add('on');
  } catch { /* not served from the repository: nothing to compare with */ }
}

placeholders();
scalarTokens();
staleCheck();
