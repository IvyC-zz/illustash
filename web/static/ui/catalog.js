// The 资料库 (desktop layout, #199): the characters' shelf and list, the works tree with its watch switches, and the
// dialogs to edit a work and add new characters. What they read and change is core/catalog.js.
import {t} from '../core/i18n.js';
import {esc, number} from '../core/format.js';
import {prefs} from '../core/prefs.js';
import {api, session, readError} from '../core/session.js';
import {state} from '../core/state.js';
import {nameTip} from '../core/labels.js';
import {pollIndex, indexAction} from '../core/jobs.js';
import {remoteNames, remoteNote} from '../core/settings.js';
import {catalog, recentOf, loadActivity as readActivity, searchCharacters, loadTagged, loadWorkTree, loadLocalLinks, castWorks, PSEUDO_NOTES, BUCKETS, castRows,
  WORK_NAMES, workName, loadIndex, searchWorks, shelfRows, saveWatch, watchTarget, saveWork} from '../core/catalog.js';
import {$, toast, toastMore, error, readFailure, wireFolds, reconcileSections, searchIcon} from './dom.js';
import {chooseWork} from './dialog.js';
import {combo, openPicker, toggleMascot} from './palette.js';
import {namesAllowButton} from './settings.js';
import {charHash, charRoute, go} from '../core/route.js';
import {fetchChar, refreshMeta} from '../core/meta.js';
import {closeInspector} from './detail.js';
import {renderHeading} from './shell.js';

// ---------- characters: 书架 (work tabs, posters, tiers) and 列表 ----------
// What the 角色 page shows: shelf or list, which work tab, a search's results, the list's sort and bucket.
export const cast = {mode: prefs.get('castMode', 'shelf'), tab: 'all', list: null, sort: 'count', dir: -1, bucket: 'all', pop: false, popQ: ''};
export async function renderIndex() {
  $('grid').hidden = true; $('empty').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = true; $('charIndex').hidden = false; closeInspector(); $('selectionBar').hidden = true;
  $('resultCount').textContent = '';
  if (state.route.section === 'works') { renderShelf(); return; }
  $('hint').textContent = t('点角色，在图库里只看 TA');
  cast.list = null; cast.pop = false;
  if (state.chars.some(c => c.summary)) {
    try { await refreshMeta(); } catch (e) { error(e.message); return; }
    if (state.route.page !== 'characters') return;
  }
  drawCast();
  loadTagged().then(() => drawCast()).catch(e => error(readError(e)));
  await loadWorkTree().catch(e => error(readError(e)));
  loadActivity(); refreshLocalLinks();
}
// Own characters whose name is a Danbooru tag (asks Danbooru with those names only; none → no request).
export async function refreshLocalLinks() {
  if (await loadLocalLinks() && state.route.page === 'characters') renderHeading();
}
export async function loadActivity() {
  let fresh; try { fresh = await readActivity(); } catch (e) { error(readError(e)); return; }
  if (fresh && state.route.page === 'characters') drawCast();
}
export async function searchIndex() {
  const route = state.route, token = session.token;
  const q = $('charSearch').value.trim();
  if (!q) { cast.list = null; drawCast(); return; }
  let list; try { list = await searchCharacters(q); } catch (e) { if (route === state.route && token === session.token && q === $('charSearch').value.trim()) readFailure($('charIndex'), e); return; }
  if (route === state.route && token === session.token && q === $('charSearch').value.trim()) { cast.list = list; drawCast(); }
}
// Kept for callers that refresh the page after metadata changes.
export function renderIndexBody() { if (state.route.page !== 'characters') return; if (state.route.section === 'works') drawShelf(); else { drawCast(); loadActivity(); } }
const cover = (c, size = 'thumb') => c?.cover ? `<img src="/image?id=${encodeURIComponent(c.cover)}&size=${size}" alt="" loading="lazy" decoding="async">` : '';
const avatar = (c, px) => `<i class="cast-av" style="width:${px}px;height:${px}px">${cover(c)}</i>`;
// ---------- a character as a fan of its pictures (#293) ----------
// A work's fan holds three of its characters; a character's holds three of its own pictures: its cover in front and
// its two newest others behind. One local read per character, kept until the data or the filter changes.
const fans = new Map();
const fanKey = c => `${c.id}|${state.nsfw}|${state.readEpoch}`;
/** The pictures of c's fan if they are read already, else null. */
export const fanNow = c => (c?.cover && fans.get(fanKey(c))?.done) || null;
export function fanPictures(c) {
  if (!c?.cover) return Promise.resolve([]);
  const key = fanKey(c);
  if (!fans.has(key)) {
    const entry = {done: null};
    entry.read = api('/api/character/stats?' + new URLSearchParams({id: c.id, nsfw: state.nsfw, exclude: c.cover}))
      .then(s => [c.cover, ...(s.recent || []).filter(u => u !== c.cover).slice(0, 2)], () => [c.cover])
      .then(pics => (entry.done = pics));
    fans.set(key, entry);
  }
  return fans.get(key).read;
}
// The cards back to front, as the work fan draws them: second, third, then the cover on top. A missing card stays
// empty, and an empty card is not drawn.
export const fanCards = (pics, size = 'thumb') => [pics[1], pics[2], pics[0]].map(u => `<i>${u ? `<img src="/image?id=${encodeURIComponent(u)}&size=${size}" alt="" decoding="async">` : ''}</i>`).join('');
// A poster deals its fan on hover: read on the first hover, drawn into the poster's empty fan.
function dealFan(a) {
  const box = a.querySelector('.poster-fan'), c = state.charMap.get(a.dataset.fan);
  if (!box || box.dataset.state || !c) return;
  box.dataset.state = 'reading';
  fanPictures(c).then(pics => {
    if (pics.length < 2) { box.dataset.state = 'single'; return; }
    box.innerHTML = fanCards(pics, a.closest('.cast-board') && !a.previousElementSibling ? 'medium' : 'thumb');
    box.dataset.state = 'ready'; a.classList.add('fanned');
  });
}
function poster(c, sub, rank = 0, big = false) {
  const r = recentOf(c), skins = (c.variants || []).filter(v => v.count).length;
  return `<a class="cast-poster" href="${charHash(c.id)}" title="${esc(nameTip(c))}" data-fan="${esc(c.id)}"><span class="img">${cover(c, big ? 'medium' : 'thumb')}<span class="cast-fan poster-fan" aria-hidden="true"></span></span>${rank ? `<span class="rank">${rank}</span>` : ''}${r ? `<span class="pill ${rank ? 'r' : 'l'}"><i></i>+${number(r)}</span>` : ''}${!rank && skins ? `<span class="pill r">${t('{n} 个皮肤', {n: skins})}</span>` : ''}<span class="cap"><b>${c.kind === 'oc' ? `<em>${t('原创')}</em>` : ''}${esc(c.name)}</b><small>${sub}</small></span></a>`;
}
const fan = chars => `<span class="cast-fan">${[1, 2, 0].map(i => `<i>${cover(chars[i])}</i>`).join('')}</span>`;
function tiers(list, showWork) {
  const big = list.filter(c => c.count >= 20), mid = list.filter(c => c.count >= 5 && c.count < 20), small = list.filter(c => c.count < 5);
  const sub = c => (showWork && c.work_name ? esc(c.work_name) + ' · ' : '') + t('{n} 张', {n: number(c.count)});
  return (big.length ? `<section class="cast-sec"><h3>${t('主力')}<small>${t('20 张以上 · {n} 位', {n: number(big.length)})}</small></h3><div class="cast-posters">${big.map(c => poster(c, sub(c))).join('')}</div></section>` : '') +
    (mid.length ? `<section class="cast-sec"><h3>${t('常见')}<small>${t('5–19 张 · {n} 位', {n: number(mid.length)})}</small></h3><div class="cast-roster">${mid.map(c => `<a class="cast-rs" href="${charHash(c.id)}" title="${esc(nameTip(c))}">${avatar(c, 72)}<b>${esc(c.name)}</b><small>${showWork && c.work_name ? esc(c.work_name) + ' · ' : ''}${t('{n} 张', {n: c.count})}</small></a>`).join('')}</div></section>` : '') +
    (small.length ? `<section class="cast-sec"><h3>${t('偶尔')}<small>${t('不到 5 张 · {n} 位', {n: number(small.length)})}</small></h3><div class="cast-chips">${small.map(c => `<a class="cast-chip" href="${charHash(c.id)}" title="${esc(nameTip(c))}">${avatar(c, 22)}${esc(c.name)}<small>${c.count}</small></a>`).join('')}</div></section>` : '');
}
// 总览, the biggest works, then 原创角色 and 未归作品 at the end (#293: they are not works; 最近 30 天 lives in 总览).
function castTabs(works) {
  const real = works.filter(w => !w.pseudo), top = real.slice(0, 14);
  const current = !['all', 'recent'].includes(cast.tab) && !works.some(w => w.pseudo && w.key === cast.tab) && !top.some(w => w.key === cast.tab) ? works.find(w => w.key === cast.tab) : null;
  const tab = (key, label, sub, lead = '', title = '') => `<button class="cast-tab${lead ? '' : ' plain'}${title ? ' pseudo' : ''}${cast.tab === key ? ' on' : ''}" data-tab="${esc(key)}"${title ? ` title="${esc(title)}"` : ''}>${lead}${esc(label)}${sub ? `<small>${sub}</small>` : ''}</button>`;
  return `<div class="cast-tabs"><div class="cast-tabrow">${tab('all', t('总览'))}${[...(current ? [current] : []), ...top].map(w => tab(w.key, w.name, number(w.images), avatar(w.chars[0], 24))).join('')}${works.filter(w => w.pseudo).map(w => tab(w.key, w.name, number(w.images), '', PSEUDO_NOTES[w.key])).join('')}</div>
    <button class="cast-allw${cast.pop ? ' on' : ''}" id="castAll" aria-expanded="${cast.pop}">${t('全部作品 {n}', {n: number(works.filter(w => !w.pseudo).length)})}<svg viewBox="0 0 20 20" aria-hidden="true"><path d="m6 8 4 4 4-4"/></svg></button>${cast.pop ? castPop(works) : ''}</div>`;
}
function castPop(works) {
  const q = cast.popQ.trim().toLowerCase(), list = works.filter(w => !q || w.name.toLowerCase().includes(q));
  return `<div class="cast-pop" id="castPop"><div class="search-wrap">${searchIcon()}<input type="search" id="castPopQ" placeholder="${t('在 {n} 部作品里找', {n: number(works.filter(w => !w.pseudo).length)})}" value="${esc(cast.popQ)}" autocomplete="off"></div>
    <div class="cast-wcols">${list.map(w => `<button class="cast-wrow${w.pseudo ? ' pseudo' : ''}" data-tab="${esc(w.key)}"${w.pseudo ? ` title="${esc(PSEUDO_NOTES[w.key])}"` : ''}>${avatar(w.chars[0], 20)}<span>${esc(w.name)}</span><small>${number(w.images)}</small></button>`).join('') || `<p class="muted-note">${t('没有匹配的作品')}</p>`}</div></div>`;
}
function castPane(works) {
  if (cast.list) return `<div class="cast-head"><div><h2>${t('搜索结果')}</h2><div class="cast-tags"><span>${t('{n} 位角色', {n: number(cast.list.length)})}</span></div></div></div>${cast.list.length ? tiers(cast.list, true) : `<p class="empty">${t('没有匹配的角色')}</p>`}`;
  if (cast.tab === 'all') {
    const top = [...state.chars].sort((a, b) => b.count - a.count).slice(0, 9);
    const rec = state.chars.filter(c => recentOf(c)).sort((a, b) => recentOf(b) - recentOf(a)), max = rec[0] ? recentOf(rec[0]) : 1;
    return `<section class="cast-sec"><h3>${t('你最常看的')}<small>${t('按收藏张数')}</small></h3><div class="cast-board">${top.map((c, i) => poster(c, (c.work_name ? esc(c.work_name) + ' · ' : '') + t('{n} 张', {n: number(c.count)}), i + 1, i === 0)).join('')}</div></section>
      ${rec.length ? `<section class="cast-sec"><h3>${t('最近 30 天在收')}<small>${t('{n} 位角色有新图', {n: number(rec.length)})}</small><button class="link-button strong" data-tab="recent">${t('全部')} ›</button></h3><div class="cast-recent">${rec.slice(0, 24).map(c => `<a class="cast-rc" href="${charHash(c.id)}" title="${esc(nameTip(c))}"><span class="ring" style="--p:${Math.max(8, Math.round(recentOf(c) / max * 100))}">${avatar(c, 66)}<em>+${number(recentOf(c))}</em></span><b>${esc(c.name)}</b><small>${esc(c.work_name || '')}</small></a>`).join('')}</div></section>` : ''}
      <section class="cast-sec"><h3>${t('作品')}<small>${t('{n} 部 · 按张数', {n: number(works.filter(w => !w.pseudo).length)})}</small></h3><div class="cast-works">${works.slice(0, 24).map(w => `<button class="cast-wt" data-tab="${esc(w.key)}">${fan(w.chars)}<b>${esc(w.name)}</b><small>${t('{n} 位 · {images} 张', {n: number(w.chars.length), images: number(w.images)})}${w.recent ? ` · <em>+${number(w.recent)}</em>` : ''}</small></button>`).join('')}</div></section>`;
  }
  if (cast.tab === 'recent') {
    const rec = state.chars.filter(c => recentOf(c)).sort((a, b) => recentOf(b) - recentOf(a));
    return `<div class="cast-head"><div><h2>${t('最近 30 天在收')}</h2><div class="cast-tags"><span class="hot">${t('{n} 位角色', {n: number(rec.length)})}</span><span>${t('按新增张数')}</span></div></div></div>${rec.length ? `<section class="cast-sec"><div class="cast-posters">${rec.slice(0, 120).map(c => poster(c, (c.work_name ? esc(c.work_name) + ' · ' : '') + t('近 30 天 +{n}', {n: number(recentOf(c))}))).join('')}</div></section>` : `<p class="empty">${t('最近 30 天没有新图。')}</p>`}`;
  }
  const w = works.find(x => x.key === cast.tab);
  if (!w) { cast.tab = 'all'; return castPane(works); }
  const real = !['oc', 'other'].includes(w.key), info = state.index?.works?.find(x => x.id === w.key);
  const manage = real && info ? `<div class="cast-manage">${watchSwitch(info)}<button class="quiet" data-shelf-edit="${esc(w.key)}">${t('编辑作品…')}</button></div>` : '';
  return `<div class="cast-head">${fan(w.chars)}<div><h2>${esc(w.name)}</h2><div class="cast-tags"><span>${t('{n} 位角色', {n: number(w.chars.length)})}</span><span>${t('{n} 张', {n: number(w.images)})}</span>${w.recent ? `<span class="hot">${t('近 30 天 +{n}', {n: number(w.recent)})}</span>` : ''}</div>${w.pseudo ? `<p class="cast-note">${esc(PSEUDO_NOTES[w.key])}</p>` : ''}${manage}</div>${real ? `<button class="primary" data-work-lib="${esc(w.key)}">${t('在图库里看全部')}</button>` : ''}</div>${tiers(w.chars, false)}`;
}
function castList() {
  const base = cast.list || state.chars, rows = castRows(base, cast);
  const max = Math.max(1, ...base.map(c => c.count)), shown = rows.slice(0, cast.limit || 300);
  const th = (k, label) => `<th data-sort="${k}" class="${cast.sort === k ? 'on' : ''}" aria-sort="${cast.sort === k ? (cast.dir < 0 ? 'descending' : 'ascending') : 'none'}">${label}${cast.sort === k ? (cast.dir < 0 ? ' ↓' : ' ↑') : ''}</th>`;
  return `<div class="cast-buckets">${BUCKETS.map(([k, label, f]) => `<button data-bucket="${k}" class="${cast.bucket === k ? 'on' : ''}">${label}<small>${number(base.filter(f).length)}</small></button>`).join('')}</div>
    <table class="cast-table"><thead><tr>${th('name', t('角色'))}${th('work', t('作品'))}${th('count', t('张数'))}${th('recent', t('近 30 天'))}${th('last', t('最后收藏'))}${th('skins', t('皮肤'))}</tr></thead><tbody>
    ${shown.map(c => { const a = catalog.activity.get(c.id), skins = (c.variants || []).filter(v => v.count).length, orig = c.tag && c.tag !== c.name ? c.tag.replaceAll('_', ' ') : '';
      return `<tr data-char="${esc(c.id)}"><td><div class="cast-who">${avatar(c, 36)}<span><b>${esc(c.name)}</b>${orig ? `<small>${esc(orig)}</small>` : ''}</span></div></td><td>${esc(c.kind === 'oc' ? t('原创角色') : c.work_name || '—')}</td><td><div class="cast-bar"><i style="width:${Math.max(4, Math.log(c.count + 1) / Math.log(max + 1) * 110)}px"></i>${number(c.count)}</div></td><td>${a?.recent ? `<span class="cast-plus">+${number(a.recent)}</span>` : '<span class="dim">—</span>'}</td><td class="mono">${esc(a?.last || '')}</td><td>${skins || '<span class="dim">—</span>'}</td></tr>`; }).join('')}
    </tbody></table>${rows.length > shown.length ? `<button class="quiet cast-more" id="castMore">${t('再显示 300 位（共 {n} 位）', {n: number(rows.length)})}</button>` : ''}`;
}
export function drawCast() {
  if (state.route.page !== 'characters' || state.route.section) return;
  const works = castWorks(), root = $('charIndex');
  // Prefixed: a bare 'shelf' would pick up the 作品 page's .shelf (narrow flex column, #274).
  root.className = 'char-index cast cast-' + cast.mode;
  reconcileSections(root, cast.mode === 'list' ? [['list', `<div class="cast-listview">${castList()}</div>`]]
    : [...(cast.list ? [] : [['tabs', castTabs(works)]]), ['pane', `<div class="cast-pane">${castPane(works)}</div>`]]);
  const foot = (state.tagged || []).length ? `<p class="cast-tagged">${t('不算角色、只当标签：')}${state.tagged.map(x => `<a href="${charHash(x.id)}">${esc(x.name)}</a><small>${number(x.count)}</small>`).join('')}<button class="link-button" data-tagged-manage>${t('管理…')}</button></p>` : '';
  let el = root.querySelector('.cast-tagged'); if (el) el.remove();
  if (foot) root.insertAdjacentHTML('beforeend', foot);
  const tagged = $('taggedButton'); if (tagged) tagged.innerHTML = t('只当标签') + (state.tagged?.length ? ` <small>${number(state.tagged.length)}</small>` : '');
  if (cast.pop) $('castPopQ')?.focus();
}

// 只当标签 (#293): mascots and player stand-ins (the default ones included) are tags, not characters. One place to see
// them all, turn a character into one, and turn one back.
export async function openTagged() {
  const d = document.createElement('dialog'); d.className = 'palette profile-dialog tagged-dialog';
  d.innerHTML = `<form method="dialog"><div class="dialog-top"><h2>${t('只当标签的角色')}</h2><button type="button" class="icon" data-close aria-label="${t('关闭')}">×</button></div>
    <p class="muted-note">${t('吉祥物、玩家化身（老师、博士、训练员这类）可以只当标签：不算角色，不出现在角色列表里，同框也不算多人图；图上仍挂着这个标签，可以按它筛选。')}</p>
    <div class="tagged-list" id="taggedList"></div>
    <div class="dialog-actions"><button type="button" class="quiet" id="taggedAdd">${t('把一个角色设为只当标签…')}</button><div class="spacer"></div><button value="ok" class="primary">${t('完成')}</button></div></form>`;
  const list = () => {
    const rows = state.tagged || [];
    d.querySelector('#taggedList').innerHTML = rows.length ? rows.map(x => `<div class="tagged-row"><a href="${charHash(x.id)}" data-close>${esc(x.name)}</a><small>${t('{n} 张', {n: number(x.count)})}</small><div class="spacer"></div><button type="button" class="quiet small" data-untag="${esc(x.id)}">${t('改回角色')}</button></div>`).join('')
      : `<p class="muted-note">${t('现在没有只当标签的角色。')}</p>`;
  };
  const refresh = async () => { await loadTagged().catch(e => error(readError(e))); if (d.open) list(); drawCast(); };
  d.addEventListener('click', async e => {
    if (e.target.closest('[data-close]')) { d.close(); return; }
    const b = e.target.closest('[data-untag]'); if (!b) return;
    b.disabled = true;
    const x = state.tagged.find(r => r.id === b.dataset.untag);
    await toggleMascot({id: x.id, name: x.name, mascot: true}); await refresh();
  });
  d.querySelector('#taggedAdd').onclick = () => openPicker({title: t('把一个角色设为只当标签'), jump: true, exclude: (state.tagged || []).map(x => x.id),
    sentence: e => t('{name} 不再算角色，只当标签', {name: e.name}),
    pick: async e => { await toggleMascot({id: e.id, name: e.name, mascot: false}); await refresh(); if (!d.open) d.showModal(); }});
  d.addEventListener('close', () => d.remove());
  document.body.append(d); list(); d.showModal();
  refresh();
}

// ---------- 资料库 · 作品: the work tree, its Danbooru links and the watch list (#198) ----------
// Watching a work means the index check asks Danbooru (work name only) for characters the local index lacks.
export const shelf = {q: '', filter: 'all'};
export async function renderShelf() {
  const route = state.route, token = session.token;
  $('hint').textContent = t('点作品名看它的角色 · 检查关注作品的新角色前会确认联网');
  drawShelf();
  try { await loadIndex(); } catch (e) { if (route === state.route && token === session.token) readFailure($('charIndex'), e); return; }
  if (route !== state.route || token !== session.token) return;
  if (state.route.page === 'characters' && state.route.section === 'works') { renderHeading(); drawShelf(); }
}
// A watch toggle needs a Danbooru work tag the person has checked; the index check searches by that tag.
function watchSwitch(w) {
  const on = state.index?.watch.works.includes(w.id);
  return `<label class="shelf-watch${w.watchable ? '' : ' off'}" title="${w.watchable ? t('定期检查 Danbooru 上这部作品还没收录的角色（只发送作品名）') : t('先在「编辑作品」里核对 Danbooru 作品 tag，才能关注')}"><input type="checkbox" class="switch" data-watch="${esc(w.id)}" ${on ? 'checked' : ''} ${w.watchable || on ? '' : 'disabled'}><span>${t('关注新角色')}</span></label>`;
}
function workLink(w) {
  if (w.tag) return `<a class="shelf-link ok" href="https://danbooru.donmai.us/posts?tags=${encodeURIComponent(w.tag)}" target="_blank" rel="noreferrer noopener" title="${t('在 Danbooru 查看')}">${esc(w.tag)}</a>`;
  if (w.status === 'pending') return `<span class="shelf-link warn">${t('链接待确认')}</span>`;
  return `<span class="shelf-link">${t('未关联 Danbooru')}</span>`;
}
export function drawShelf() {
  if (state.route.page !== 'characters' || state.route.section !== 'works') return;
  const root = $('charIndex'), x = state.index;
  root.className = 'char-index shelf';
  if (!x?.works) { root.innerHTML = `<p class="muted-note shelf-empty">${t('正在读取作品…')}</p>`; return; }
  const missing = x.check?.missing || [], oldTags = x.watch.pending_tags || [];
  const fresh = w => w.tag ? missing.filter(r => r.copyright === w.tag).length : 0;
  const people = new Map();
  for (const c of state.chars) if (c.work_id) people.set(c.work_id, (people.get(c.work_id) || 0) + 1);
  const {rows: shown, watched, pending: pendingN} = shelfRows(x, shelf);
  const chip = (k, label, n) => `<button class="chip-filter${shelf.filter === k ? ' on' : ''}" data-shelf-filter="${k}">${label}${n ? ` <span>${number(n)}</span>` : ''}</button>`;
  const todo = oldTags.length ? `<details class="shelf-todo" data-fold-key="shelfTodo"${prefs.get('settingsOpen', {}).shelfTodo ? ' open' : ''}><summary data-more="${t('展开')}" data-less="${t('收起')}"><b>${t('待处理')}</b><span>${t('{n} 个以前关注的 Danbooru 作品还没对应到本地作品，对应后才会检查它们的新角色', {n: number(oldTags.length)})}</span></summary>
      ${oldTags.map(tag => `<div class="shelf-todo-row"><span class="mono">${esc(tag)}</span><span class="muted-note">${esc(WORK_NAMES[tag] || '')}</span><div class="spacer"></div><button class="quiet small" data-shelf-adopt="${esc(tag)}">${t('对应到作品…')}</button></div>`).join('')}</details>` : '';
  const found = missing.length ? `<div class="shelf-found"><span class="local-dot busy"></span><span>${t('上次检查发现 {n} 个还没收录的角色', {n: `<b>${number(missing.length)}</b>`})}</span><div class="spacer"></div><button class="primary" data-index-add>${t('查看并添加…')}</button></div>` : '';
  const row = ([w, depth]) => {
    const n = fresh(w), chars = people.get(w.id) || 0;
    return `<div class="shelf-row" style="--depth:${depth}">
      <button class="shelf-name" data-shelf-open="${esc(w.id)}" title="${t('看这部作品的角色')}"><b>${esc(w.name)}</b>${w.qualifier ? `<small>${esc(w.qualifier)}</small>` : ''}</button>
      ${workLink(w)}
      <span class="shelf-num">${chars ? t('{n} 位角色', {n: number(chars)}) + ' · ' : ''}${t('{n} 张', {n: number(w.count || 0)})}</span>
      ${n ? `<button class="shelf-new" data-index-add="${esc(w.tag)}" title="${t('Danbooru 上有、本地索引还没有的角色')}">${t('+{n} 个新角色', {n: number(n)})}</button>` : '<span></span>'}
      ${watchSwitch(w)}
      <button class="quiet small" data-shelf-edit="${esc(w.id)}">${t('编辑…')}</button></div>`;
  };
  root.innerHTML = `${todo}${found}
    <div class="shelf-bar"><div class="chips-row">${chip('all', t('全部'), x.works.length)}${chip('watched', t('已关注'), watched.size)}${pendingN ? chip('pending', t('链接待确认'), pendingN) : ''}</div><div class="spacer"></div>
      <span class="watch-add"><input id="watchAdd" value="${esc(watchDraft)}" placeholder="${t('关注 Danbooru 上的作品…')}" spellcheck="false" autocomplete="off" role="combobox" aria-expanded="false" aria-controls="watchMenu"><div id="watchMenu" class="tag-popover watch-menu" role="listbox" hidden></div></span></div>
    <div class="shelf-list">${shown.map(row).join('') || `<p class="muted-note shelf-empty">${x.works.length ? t('没有符合条件的作品') : t('还没有作品。给角色填写所属作品，或点右上角「新建作品」。')}</p>`}</div>`;
  wireWatchAdd(x, adoptWatch); wireFolds(root);
}
async function saveWatchList(works) {
  try { await saveWatch(works); }
  catch (e) { toast(e.message); }
  renderIndexBody(); renderHeading();
}
// From the Danbooru search: an existing local work with that tag, or one the person picks or creates for it.
async function adoptWatch(r) {
  try {
    const id = await watchTarget(r, (title, works) => chooseWork(title, works, '', t('新建作品')));
    if (id === null) return;
    watchDraft = '';
    if (!state.index.watch.works.includes(id)) await saveWatchList([...state.index.watch.works, id]); else renderIndexBody();
  } catch (e) { toast(e.message); }
}
// After pictures are set to characters (#293): when they belong to one work, the toast offers to watch it for new
// characters, or says it is watched already. A work without a checked Danbooru tag opens its dialog to find one.
export async function offerWatch(ids) {
  const chars = await Promise.all(ids.map(id => state.charMap.get(id) || fetchChar(id).catch(() => null)));
  const works = [...new Set(chars.map(c => c?.kind === 'oc' ? null : c?.work_id).filter(Boolean))];
  if (works.length !== 1) return;
  let x = state.index; if (!x?.works) { try { x = await loadIndex(); } catch { return; } }
  const w = x?.works?.find(y => y.id === works[0]); if (!w) return;
  if (x.watch.works.includes(w.id)) { toastMore(' · ' + t('「{name}」已关注', {name: w.name})); return; }
  toastMore('', w.watchable ? t('关注「{name}」的新角色', {name: w.name}) : t('关注「{name}」…', {name: w.name}), async () => {
    if (!w.watchable) { const r = await workDialog(w, {watch: true}); if (r?.watch) toast(t('已关注「{name}」，检查新角色前会确认联网', {name: r.name})); return; }
    try { await saveWatch([...state.index.watch.works, w.id]); toast(t('已关注「{name}」，检查新角色前会确认联网', {name: w.name})); renderIndexBody(); renderHeading(); }
    catch (e) { toast(e.message); }
  });
}
export async function createWork() {
  const w = await workDialog(null);
  if (w) toast(w.watch ? t('已新建作品「{name}」并关注它的新角色', {name: w.name}) : t('已新建作品「{name}」', {name: w.name}));
}
// One dialog for a new work and for editing one: a Danbooru search that fills the tag (and the name, when empty), the
// name, the tag the person has checked, an optional parent, and (with a tag) watching it for new characters (#293).
function workDialog(w, {watch = false} = {}) {
  const works = state.index?.works || [], watched = new Set(state.index?.watch?.works || []);
  return new Promise(resolve => {
    const d = document.createElement('dialog'); d.className = 'palette profile-dialog work-dialog';
    const parents = works.filter(x => !w || x.id !== w.id);
    d.innerHTML = `<form method="dialog"><div class="dialog-top"><h2>${w ? t('编辑作品') : t('新建作品')}</h2><button type="button" class="icon" data-close aria-label="${t('关闭')}">×</button></div>
      ${watch && !w?.tag ? `<p class="dialog-lead">${t('关注要按 Danbooru 上的作品 tag 检查。先在下面找到这部作品，选中后保存就会关注。')}</p>` : ''}
      <label class="field"><span>${t('在 Danbooru 上找')}</span><div class="combo"><input id="wdFind" autocomplete="off" spellcheck="false" placeholder="${t('输入作品名，中文、日文、英文都行')}"><div class="combo-menu" id="wdFindMenu" hidden></div></div>
        <small>${t('选中后自动填好下面的 tag，名称空着时也一起填上。只发送你输入的文字。')}</small></label>
      <label class="field"><span>${t('名称')}</span><input id="wdName" name="title" value="${esc(w?.name || '')}" maxlength="120" required autocomplete="off">
        <small>${t('界面上显示的名字，用你习惯的叫法就行，和 Danbooru tag 无关，以后随时能改。')}</small></label>
      <label class="field"><span>${t('Danbooru 作品 tag')}<em class="opt">${t('可不填')}</em></span><input id="wdTag" name="tag" value="${esc(w?.tag || '')}" placeholder="${t('例如 blue_archive')}" spellcheck="false" autocomplete="off">
        <small id="wdTagNote"></small></label>
      <label class="field"><span>${t('上级作品')}<em class="opt">${t('可不填')}</em></span><select id="wdParent" name="parent"><option value="">${t('无')}</option>${parents.map(x => `<option value="${esc(x.id)}" ${w?.parent_id === x.id ? 'selected' : ''}>${esc(x.name)}</option>`).join('')}</select>
        <small>${t('只在这部作品属于某个系列时填，比如「Fate/Grand Order」的上级是「Fate 系列」。在图库里按上级作品筛选时，下级作品的图也算在内。')}</small></label>
      <label class="check-row" id="wdWatchRow"><input type="checkbox" id="wdWatch"><span><b>${t('关注这部作品的新角色')}</b><small>${t('定期检查 Danbooru 上这部作品还没收录的角色（只发送作品名），检查前会确认联网')}</small></span></label>
      <div class="dialog-actions"><button type="button" class="quiet" data-close>${t('取消')}</button><button value="ok" class="primary">${w ? t('保存') : t('新建')}</button></div></form>`;
    const q = sel => d.querySelector(sel), already = w && watched.has(w.id);
    let filled = '';  // the name the search filled in, replaced by the next pick
    const tagNote = () => {
      const tag = q('#wdTag').value.trim(), owner = tag && works.find(x => x.tag === tag && x.id !== w?.id);
      q('#wdTagNote').innerHTML = owner ? `<span class="warn">${t('图库里的「{name}」已经用了这个 tag', {name: esc(owner.name)})}</span>`
        : w?.status === 'pending' && !tag ? t('链接还没确认：核对过就填上 tag；留空保存表示不关联。')
        : t('用上面的搜索填，或手动填你核对过的 tag，不会按名字自动关联。填了才能关注这部作品的新角色。');
      q('#wdWatchRow').hidden = !tag || already; if (!tag) q('#wdWatch').checked = false;
    };
    q('#wdTag').oninput = tagNote; tagNote();
    q('#wdFind').addEventListener('keydown', e => { if (e.key === 'Enter') e.preventDefault(); });
    combo(q('#wdFind'), q('#wdFindMenu'), (text, show) => {
      if (!text) { show([]); return; }
      show([{label: 'Danbooru', items: [], note: t('正在查 Danbooru…（只发送你输入的文字）')}]);
      remoteNames('/api/works?' + new URLSearchParams({q: text, remote: 1})).then(r => show([r.found
        ? {label: 'Danbooru', items: r.found.map(x => { const mine = works.find(y => y.tag === x.tag && y.id !== w?.id);
          return {tag: x.tag, label: workName(x.tag) || x.tag, detail: x.tag, extra: mine ? t('图库里已有：{name}', {name: mine.name}) : t('D 站 {n}', {n: number(x.posts)})}; }),
          note: r.found.length ? '' : t('没有找到这个作品。可以换个写法，比如英文或日文名')}
        : {label: 'Danbooru', items: [], note: remoteNote(r), allow: r.unauthorized}]));
    }, r => {
      q('#wdTag').value = r.tag;
      const name = q('#wdName');
      if (!name.value.trim() || name.value === filled) name.value = filled = r.label;
      tagNote(); q('#wdFind').value = '';
      if (!already && (!w || watch)) q('#wdWatch').checked = true;
    });
    for (const b of d.querySelectorAll('[data-close]')) b.onclick = () => d.close();
    d.onclose = async () => {
      const data = new FormData(q('form')), ok = d.returnValue === 'ok', watch = q('#wdWatch').checked; d.remove();
      if (!ok) { resolve(null); return; }
      const name = String(data.get('title')).trim(), parent = data.get('parent'), tag = String(data.get('tag')).trim();
      try {
        const id = await saveWork(w?.id, {name, parent, tag});
        if (watch && tag && !state.index.watch.works.includes(id)) await saveWatch([...state.index.watch.works, id]);
        await loadWorkTree().catch(e => error(readError(e)));
        await refreshMeta().catch(e => error(readError(e)));
        renderIndexBody(); renderHeading();
        resolve({id, name, watch});
      } catch (err) { toast(err.message); resolve(null); }
    };
    document.body.append(d); d.showModal(); (w && !watch ? q('#wdName') : q('#wdFind')).focus();
  });
}
// Characters the last index check found on Danbooru for watched works; adding one downloads ~20 public reference pictures.
export function openIndexAdd(tag = '') {
  const token = session.token, x = state.index, list = (x?.check?.missing || []).filter(r => !tag || r.copyright === tag);
  if (!list.length) return;
  const d = document.createElement('dialog'); d.className = 'palette profile-dialog index-add';
  d.innerHTML = `<form method="dialog"><div class="dialog-top"><h2>${t('添加新角色')}</h2><button type="button" class="icon" data-close aria-label="${t('关闭')}">×</button></div>
    <p class="muted-note">${t('这些角色在 Danbooru 上有、本地索引还没有。添加时每个角色下载约 20 张公开参考图（约 {mb} MB），在本机算好特征后重新扫描图库。点 tag 可以先去 Danbooru 看看。', {mb: x.mb_per_character})}</p>
    <div class="index-add-list"><table class="settings-table index-table"><tr><th><input type="checkbox" id="iaAll" aria-label="${t('全选')}" checked></th><th>${t('角色 tag')}</th><th>${t('作品')}</th><th>${t('D 站图数')}</th></tr>
      ${list.map(r => `<tr><td><input type="checkbox" data-pick="${esc(r.short_tag)}" ${indexSkip.has(r.short_tag) ? '' : 'checked'}></td><td><a class="link" href="https://danbooru.donmai.us/posts?tags=${encodeURIComponent(r.tag)}" target="_blank" rel="noreferrer noopener">${esc(r.tag)}</a></td><td>${esc(x.works.find(w => w.tag === r.copyright)?.name || workName(r.copyright))}</td><td>${number(r.post_count)}</td></tr>`).join('')}</table></div>
    <div class="dialog-actions"><span class="muted-note" id="iaNote"></span><div class="spacer"></div><button value="cancel" class="quiet">${t('取消')}</button><button value="ok" class="primary" id="iaApply"></button></div></form>`;
  const q = s => d.querySelector(s), picks = () => [...d.querySelectorAll('[data-pick]')];
  const note = () => {
    const n = picks().filter(b => b.checked).length, mb = n * x.mb_per_character;
    q('#iaApply').textContent = t('添加 {n} 个（约 {mb} MB）', {n: number(n), mb: mb >= 10 ? Math.round(mb) : mb.toFixed(1)}); q('#iaApply').disabled = !n;
    q('#iaNote').textContent = mb > 500 ? t('下载量超过 500 MB，网络不方便时可以分几次添加') : '';
    indexSkip = new Set([...indexSkip, ...picks().filter(b => !b.checked).map(b => b.dataset.pick)].filter(t => !picks().some(b => b.checked && b.dataset.pick === t)));
  };
  for (const b of picks()) b.onchange = note;
  q('#iaAll').onchange = e => { for (const b of picks()) b.checked = e.target.checked; note(); };
  q('[data-close]').onclick = () => d.close();
  d.onclose = async () => {
    const tags = picks().filter(b => b.checked).map(b => b.dataset.pick), ok = d.returnValue === 'ok'; d.remove();
    if (!ok || !tags.length || token !== session.token) return;
    try { await indexAction({action: 'apply', tags, allow_network: true}); toast(t('开始添加 {n} 个角色，进度在任务面板里', {n: number(tags.length)})); pollIndex(); renderIndexBody(); }
    catch (e) { toast(e.message); }
  };
  document.body.append(d); note(); d.showModal();
}

// ---------- watching a work: the picker under the works tree ----------
let indexSkip = new Set(), watchDraft = '';
// Work picker: works your library already has (instant, local names and aliases), then Danbooru
// copyright tags for the typed text (only the text goes out). Picking one yields an exact tag.
export function wireWatchAdd(x, add) {
  const token = session.token, input = $('watchAdd'), menu = $('watchMenu'), watched = new Set(x.watch.works);
  let list = [], index = 0, gen = 0, localTimer = 0, remoteTimer = 0;
  const row = (r, i) => `<button class="picker-item watch-option${i === index ? ' on' : ''}" data-i="${i}" role="option" ${watched.has(r.id) || (r.id && !r.watchable) ? 'disabled' : ''}>
    <span class="watch-name">${esc(r.name || workName(r.tag))}</span><small>${esc(r.tag || t('D 站上还没有这个作品'))}</small>
    <em>${watched.has(r.id) ? t('已关注') : [r.count ? t('你有 {n} 张', {n: number(r.count)}) : '', r.posts ? t('D 站 {n}', {n: number(r.posts)}) : ''].filter(Boolean).join(' · ')}</em></button>`;
  const render = (sections, note = '') => {
    list = sections.flatMap(sec => sec.items); index = Math.max(0, list.findIndex(r => !watched.has(r.id)));
    let i = 0;
    menu.innerHTML = sections.filter(sec => sec.items.length).map(sec => `<div class="palette-label">${sec.label}</div>` + sec.items.map(r => row(r, i++)).join('')).join('') + (note ? `<p class="muted-note watch-note">${note}</p>` : '');
    menu.hidden = !menu.innerHTML; input.setAttribute('aria-expanded', String(!menu.hidden));
  };
  const highlight = () => { for (const b of menu.querySelectorAll('[data-i]')) b.classList.toggle('on', Number(b.dataset.i) === index); menu.querySelector('.on')?.scrollIntoView({block: 'nearest'}); };
  const search = async () => {
    const q = input.value.trim(), g = ++gen;
    let local; try { local = await searchWorks(q); } catch (e) { if (g === gen && token === session.token && input === $('watchAdd')) render([], esc(readError(e))); return; }
    if (g !== gen || token !== session.token || input !== $('watchAdd')) return;
    if (!q) { render([{label: t('你的图库里还没关注的作品'), items: local.filter(r => !watched.has(r.id)).slice(0, 10)}]); return; }
    const mine = local.slice(0, 8);
    render([{label: t('你的图库里有'), items: mine}], t('正在查 Danbooru…（只发送你输入的文字）'));
    clearTimeout(remoteTimer);
    remoteTimer = setTimeout(async () => {
      const result = await remoteNames('/api/works?' + new URLSearchParams({q, remote: 1})), remote = result.found;
      if (g !== gen || token !== session.token || input !== $('watchAdd')) return;
      const posts = new Map((remote || []).map(r => [r.tag, r.posts]));
      for (const r of mine) if (posts.has(r.tag)) r.posts = posts.get(r.tag);
      const known = new Set(mine.map(r => r.tag));
      const others = (remote || []).filter(r => !known.has(r.tag));
      render([{label: t('你的图库里有'), items: mine}, {label: 'Danbooru', items: others}],
        remote === null ? (result.unauthorized ? esc(remoteNote(result)) + ' ' + namesAllowButton : esc(t('{reason}，可以稍后再试', {reason: remoteNote(result)}))) : !mine.length && !others.length ? t('没有找到这个作品。可以换个写法，比如英文或日文名') : '');
    }, 450);
  };
  const pick = i => { const r = list[i]; if (!r || watched.has(r.id) || (r.id && !r.watchable)) return; menu.hidden = true; add(r); };
  input.onfocus = () => { clearTimeout(localTimer); search(); };
  input.oninput = e => { watchDraft = e.target.value; clearTimeout(localTimer); localTimer = setTimeout(search, 150); };
  input.onblur = () => setTimeout(() => { menu.hidden = true; input.setAttribute('aria-expanded', 'false'); }, 150);
  input.onkeydown = e => {
    if (e.isComposing) return;
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); if (list.length) { index = (index + (e.key === 'ArrowDown' ? 1 : -1) + list.length) % list.length; highlight(); } }
    else if (e.key === 'Escape') { menu.hidden = true; }
    else if (e.key === 'Enter') {
      e.preventDefault();
      if (!menu.hidden && list[index] && !watched.has(list[index].id)) pick(index);

    }
  };
  menu.onmousedown = e => e.preventDefault();  // keep focus in the input while clicking an option
  menu.onclick = e => { const b = e.target.closest('[data-i]'); if (b) pick(Number(b.dataset.i)); };
  if (document.activeElement === input || watchDraft) { input.focus(); input.setSelectionRange(input.value.length, input.value.length); }
}

// The 资料库 pages' handlers. Called once, when the page starts.
export function wireCatalog() {
  for (const kind of ['mouseover', 'focusin']) $('charIndex').addEventListener(kind, e => { const a = e.target.closest?.('.cast-poster[data-fan]'); if (a) dealFan(a); });
  $('charIndex').addEventListener('click', e => {
    if (state.route.page !== 'characters') return;
    const hit = e.target;
    if (hit.closest('[data-tagged-manage]')) { openTagged(); return; }
    const tab = hit.closest('[data-tab]');
    if (tab) { cast.tab = tab.dataset.tab; cast.pop = false; cast.popQ = ''; drawCast(); $('viewport').scrollTop = 0; $('charIndex').querySelector('.cast-tab.on')?.scrollIntoView({inline: 'center', block: 'nearest'}); return; }
    if (hit.closest('#castAll')) { cast.pop = !cast.pop; drawCast(); return; }
    if (cast.pop && !hit.closest('#castPop')) { cast.pop = false; drawCast(); return; }
    const lib = hit.closest('[data-work-lib]'); if (lib) { go({page: 'library', work: lib.dataset.workLib}); return; }
    const s = hit.closest('[data-sort]');
    if (s) { const k = s.dataset.sort; cast.dir = cast.sort === k ? -cast.dir : (k === 'name' || k === 'work' ? 1 : -1); cast.sort = k; drawCast(); return; }
    const b = hit.closest('[data-bucket]'); if (b) { cast.bucket = b.dataset.bucket; cast.limit = 300; drawCast(); return; }
    if (hit.closest('#castMore')) { cast.limit = (cast.limit || 300) + 300; drawCast(); return; }
    const row = hit.closest('tr[data-char]'); if (row) go(charRoute(row.dataset.char));
  });
  $('charIndex').addEventListener('input', e => { if (e.target.id === 'castPopQ') { cast.popQ = e.target.value; const pos = e.target.selectionStart; drawCast(); const i = $('castPopQ'); i?.setSelectionRange(pos, pos); } });
  document.addEventListener('keydown', e => { if (e.key === 'Escape' && cast.pop && state.route.page === 'characters') { cast.pop = false; drawCast(); } });
  $('charIndex').addEventListener('click', async e => {
    if (state.route.page !== 'characters') return;
    const hit = e.target, works = state.index?.works || [];
    const f = hit.closest('[data-shelf-filter]'); if (f) { shelf.filter = f.dataset.shelfFilter; drawShelf(); return; }
    const open = hit.closest('[data-shelf-open]'); if (open) { cast.tab = open.dataset.shelfOpen; go({page: 'characters'}); return; }
    const edit = hit.closest('[data-shelf-edit]'); if (edit) { const w = works.find(x => x.id === edit.dataset.shelfEdit); if (w) workDialog(w); return; }
    const adopt = hit.closest('[data-shelf-adopt]'); if (adopt) { adoptWatch({tag: adopt.dataset.shelfAdopt, name: WORK_NAMES[adopt.dataset.shelfAdopt]}); return; }
    const add = hit.closest('[data-index-add]'); if (add) openIndexAdd(add.dataset.indexAdd || '');
  });
  $('charIndex').addEventListener('change', e => {
    const b = e.target.closest('[data-watch]'); if (!b || !state.index) return;
    const id = b.dataset.watch, list = state.index.watch.works.filter(w => w !== id);
    saveWatchList(b.checked ? [...list, id] : list);
  });
}
