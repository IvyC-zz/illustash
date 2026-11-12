'use strict';
// 样式清单页自己的脚本（#318）：主题切换、按 census.js 画令牌表、占位图。
// 它从不生成应用组件：index.html 里的样例是纯 HTML，方便直接阅读和照抄。

// ---- 主题：跟随系统 / 亮 / 暗。设置 html[data-theme]，和应用里 web/static/theme.js 设的是同一个属性 ----
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
  try { b.dataset.sgTheme ? localStorage.setItem(THEME_KEY, b.dataset.sgTheme) : localStorage.removeItem(THEME_KEY); } catch { /* 隐私模式不能写 */ }
  applyTheme();
});
applyTheme();

// ---- 占位图：应用里的封面和缩略图是 <img>，这里画成渐变色块 ----
const PH = ['#b9bdf3|#f0d4e4', '#a9d8cf|#e9e2b8', '#d0b5ec|#a9c6f0', '#f0c9a8|#e6a9b8', '#a9c6f0|#c9f0d8'];
function placeholders() {
  for (const img of document.querySelectorAll('img[data-ph]')) {
    const [a, b] = PH[Number(img.dataset.ph) % PH.length].split('|');
    img.src = 'data:image/svg+xml,' + encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" width="120" height="150"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="${a}"/><stop offset="1" stop-color="${b}"/></linearGradient></defs><rect width="120" height="150" fill="url(#g)"/></svg>`);
    img.alt = '';
  }
}

// ---- 令牌表，数据来自 census.js（python3 tools/styleguide/census.py 生成） ----
const C = window.CENSUS;
const $ = id => document.getElementById(id);
const esc = s => String(s).replace(/[&<>"]/g, c => ({'&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;'}[c]));
// 主题块里不是颜色的变量：不画色块，列在“非颜色的变量”表里
const NON_COLOR = new Set(['--sh', '--mono', '--ease', '--sheet-shadow', '--select-arrow', '--btn-hover-filter']);
const bar = (n, max) => `<i class="sg-bar" style="width:${Math.max(3, Math.round(n / max * 90))}px"></i>`;
// 去掉 var(...) 和单独的 0 之后还有数字，就是写死的值
const literal = v => /\d/.test(String(v).replace(/var\([^)]*\)/g, '').replace(/(^|[^\w.])0(?![\w.%])/g, '$1'));

function colorTokens() {
  const {light, dark, uses, groups} = C.themeVars;
  // 两栏各自带一整套变量值，色块直接写 var(--名字)：引用别的变量的值（如 --tone-lilac-ink:var(--muted)）也按本栏的主题解析
  const vars = theme => Object.entries(theme).map(([k, v]) => `${k}:${v}`).join(';');
  const panel = (theme, cls, title) => `<div class="${cls}" style="${esc(vars(theme))}"><h5>${title}</h5>` + groups.map(g => {
    const names = g.names.filter(k => !NON_COLOR.has(k));
    if (!names.length) return '';
    return `<h5>${esc(g.title)}</h5><div class="sg-swatches">${names.map(k => {
      const same = cls === 'sg-pane-dark' && !(k in dark);
      return `<div class="sg-sw"><span style="background:var(${k})"></span><div><b>${k}</b><small>${same ? '同亮色' : esc(theme[k])} · ${uses[k] || 0} 处</small></div></div>`;
    }).join('')}</div>`;
  }).join('') + '</div>';
  $('tok-colors').innerHTML = `<div class="sg-themes">${panel(light, 'sg-pane-light', '亮色 :root')}${panel({...light, ...dark}, 'sg-pane-dark', '暗色 :root[data-theme=dark]')}</div>`;
  const other = Object.entries(light).filter(([k]) => NON_COLOR.has(k)).map(([k, v]) =>
    `<tr><td><code>${k}</code></td><td class="ex">${esc(v)}</td><td class="ex">${k in dark ? esc(dark[k]) : '同亮色'}</td><td class="n">${uses[k] || 0} 处</td></tr>`).join('');
  $('tok-other').innerHTML = `<table class="sg-table"><tr><th>变量</th><th>亮色</th><th>暗色</th><th>引用</th></tr>${other}` +
    `<tr><td><code>--tour-dim</code></td><td class="ex">只在暗色定义；亮色用引用处的回退值 rgba(14,14,24,.46)</td><td class="ex">${esc(dark['--tour-dim'] || '')}</td><td class="n">${uses['--tour-dim'] || 0} 处</td></tr></table>`;
  const lit = C.literalHexColors;
  $('tok-literal').innerHTML =
    `<p class="sg-cap">app.css 里不经变量、直接写死的十六进制颜色：<b>${lit.kinds}</b> 种取值，共 <b>${lit.uses}</b> 处；另有 <b>${C.rgbaCalls}</b> 处 rgb()/rgba() 调用（含变量块里的定义）。` +
    `暗色模式另有 <b>${C.darkOverrideRules}</b> 条单独覆盖规则（选择器含 <code>data-theme=dark</code>，不含变量块本身）。下面按出现次数列前 24 种。</p>` +
    `<div class="sg-scroll"><table class="sg-table"><tr><th></th><th>取值</th><th>处</th><th>例（选择器）</th></tr>${lit.table.slice().sort((a, b) => b.uses - a.uses).slice(0, 24).map(r =>
      `<tr><td><span class="sg-sw" style="display:block"><span style="background:${r.value}"></span></span></td><td><code>${r.value}</code></td><td class="n">${r.uses}</td><td class="ex">${r.examples.map(esc).join(' · ')}</td></tr>`).join('')}</table></div>`;
}

// 一组令牌的档位表：名字、值、样子、引用次数
function ladder(id, family, sample, note = '') {
  const rows = C.tokens[family] || [], max = Math.max(1, ...rows.map(r => r.uses));
  const total = rows.reduce((n, r) => n + r.uses, 0);
  $(id).innerHTML = `<p class="sg-cap"><b>${rows.length}</b> 档，共被引用 <b>${total}</b> 次。${note}</p>` +
    `<div class="sg-scroll"><table class="sg-table"><tr><th>令牌</th><th>值</th><th>样子</th><th>引用</th><th></th></tr>${rows.map(r =>
      `<tr><td><code>var(${r.name})</code></td><td class="ex">${esc(r.value)}</td><td>${sample(r)}</td><td class="n">${r.uses}</td><td>${bar(r.uses, max)}</td></tr>`).join('')}</table></div>`;
}

// 某类属性里仍然写死的取值（档位之外），没有就说没有
function leftovers(id, key, head, render = () => '', strip = v => v) {
  const d = C[key], rows = d.table.filter(r => literal(strip(r.value)));
  const n = rows.reduce((s, r) => s + r.uses, 0);
  if (!rows.length) { $(id).innerHTML = `<p class="sg-cap">${d.uses} 处声明全部只用令牌（或 0）。</p>`; return; }
  const max = Math.max(...rows.map(r => r.uses));
  $(id).innerHTML = `<p class="sg-cap">${d.uses} 处声明里，<b>${n}</b> 处、<b>${rows.length}</b> 种取值仍写死数值（其余只用令牌或 0）。这些是存量，不要照抄；“例”是用到它的前几个选择器。</p>` +
    `<div class="sg-scroll"><table class="sg-table"><tr><th>${head}</th><th>样子</th><th>处</th><th></th><th>例（选择器）</th></tr>${rows.map(r =>
      `<tr><td><code>${esc(r.value)}</code></td><td>${render(r.value)}</td><td class="n">${r.uses}</td><td>${bar(r.uses, max)}</td><td class="ex">${r.examples.map(esc).join(' · ')}</td></tr>`).join('')}</table></div>`;
}

function scalarTokens() {
  const px = v => /^[\d.]+px$/.test(v);
  const text = r => `<span style="font-size:var(${r.name})">Aa 图库 Illustash</span>`;
  ladder('tok-fs', 'fs', text);
  ladder('tok-fs-display', 'fsDisplay', r => `<span style="font-size:min(var(${r.name}),40px);font-weight:700;line-height:1.1">Aa 图库</span>`, '样子超过 40px 的按 40px 画。');
  leftovers('tok-fontsize', 'fontSize', '字号', v => { const s = v.replace(' (font 简写)', ''); return px(s) ? `<span style="font-size:${s}">Aa 图库</span>` : ''; });
  ladder('tok-radius-ladder', 'radius', r => `<span class="sg-rad"><span style="border-radius:var(${r.name})"></span></span>`);
  leftovers('tok-radius', 'radius', '圆角', v => px(v) ? `<span class="sg-rad"><span style="border-radius:${v}"></span></span>` : '');
  ladder('tok-space', 'space', r => `<span class="sg-gapbar"><i style="width:var(${r.name})"></i></span>`);
  leftovers('tok-gap', 'gap', 'gap', v => px(v) ? `<span class="sg-gapbar"><i style="width:${v}"></i></span>` : '');
  ladder('tok-h-ladder', 'h', r => `<span class="sg-bar" style="width:96px;height:var(${r.name});opacity:.25;border-radius:var(--radius-8)"></span>`);
  leftovers('tok-height', 'height', '高度', v => { const m = v.match(/: ([\d.]+px)$/); return m ? `<span class="sg-bar" style="width:${Math.min(160, parseFloat(m[1]))}px;height:${Math.min(parseFloat(m[1]), 28)}px;opacity:.25;border-radius:4px"></span>` : ''; }, v => v.split(': ')[1]);
  const z = (C.tokens.z || []).slice().sort((a, b) => Number(b.value) - Number(a.value));
  $('tok-z').innerHTML = `<p class="sg-cap"><b>${z.length}</b> 档。从上到下是层叠从高到低；${literal(C.zIndex.table.map(r => r.value).join(' ')) ? '<b>另有写死数值的 z-index</b>，见下表。' : `${C.zIndex.uses} 处 z-index 全部只用令牌。`}</p>` +
    `<div class="sg-zstack">${z.map(r => `<div><b>${esc(r.value)}</b><span><code>var(${r.name})</code> · 引用 ${r.uses} 次</span></div>`).join('')}</div>`;
  const pad = C.padding, padLit = pad.table.filter(r => literal(r.value));
  $('tok-padding').innerHTML =
    `<p class="sg-cap"><code>padding</code> 简写共 <b>${pad.kinds}</b> 种取值组合，<b>${pad.uses}</b> 处声明（不含 padding-left 等单边写法）。下面是出现次数最多的前 ${pad.table.length} 种；` +
    `其中 <b>${padLit.length}</b> 种仍写死数值（标“写死”）。</p>` +
    `<div class="sg-scroll"><table class="sg-table"><tr><th>padding</th><th>处</th><th></th><th>例（选择器）</th></tr>${pad.table.map(r =>
      `<tr><td><code>${esc(r.value)}</code></td><td class="n">${r.uses}</td><td>${literal(r.value) ? '<span class="sg-tag dup">写死</span>' : ''}</td><td class="ex">${r.examples.map(esc).join(' · ')}</td></tr>`).join('')}</table></div>`;
  $('census-meta').textContent = `统计对象：${C.source}，${C.lines} 行、${C.rules} 条规则（含 @media 内的规则），sha256 ${C.sha256.slice(0, 12)}`;
}

// ---- census.js 是否比 app.css 旧？只有经 http 打开时能比（file:// 页面读不了别的文件） ----
async function staleCheck() {
  if (location.protocol === 'file:') return;
  try {
    const css = await (await fetch('../../web/static/app.css', {cache: 'no-store'})).arrayBuffer();
    const digest = crypto.subtle ? [...new Uint8Array(await crypto.subtle.digest('SHA-256', css))].map(b => b.toString(16).padStart(2, '0')).join('') : '';
    const stale = digest ? digest !== C.sha256 : new TextDecoder().decode(css).split('\n').length !== C.lines;
    if (stale) $('sg-stale').classList.add('on');
  } catch { /* 不是从仓库目录起的服务器：没有可比的 */ }
}

placeholders();
if (C) { colorTokens(); scalarTokens(); staleCheck(); } else $('sg-stale').classList.add('on');
