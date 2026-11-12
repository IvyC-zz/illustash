// 页面的框架（桌面布局，#199）：按地址切到对应页面、侧栏的计数、每个页面的标题和工具栏。页面在哪由 core/route.js
// 负责；计数的数据由 core/meta.js 负责。
import {workKey} from '../core/catalog.js';
import {dateText, esc, number, shortcut} from '../core/format.js';
import {t} from '../core/i18n.js';
import {REASONS, nameTip} from '../core/labels.js';
import {fetchChar, onMeta, setChar} from '../core/meta.js';
import {prefs} from '../core/prefs.js';
import {implement, pageOf, viewOf} from '../core/pages.js';
import {cleanup, defaultSort, go, hashFor, parseHash, replaceRoute, setRoute} from '../core/route.js';
import {api, readError} from '../core/session.js';
import {state} from '../core/state.js';
import {cast, createWork, drawCast, drawShelf, openTagged, renderIndexBody, searchIndex, shelf} from './catalog.js';
import {$, error, searchIcon, workSmall} from './dom.js';
import {renderSettings, settingsSection} from './settings.js';
import {artistName} from './artists.js';
import {closeInspector, closeViewer} from './detail.js';
import {filters, reload, resetForRoute, selectAll, short, sweep, toggleFit, toggleSelecting} from './grid.js';
import {guideTick} from './guide.js';
import {createLayer} from './overlay.js';
import {dayLabel} from './home.js';
import {editProfile, markOc, mergeCharacter, openLocalLinks, openRelate, splitOc, toggleMascot, toggleOc} from './palette.js';
import {nearText} from './review.js';

// 搜索框和标签菜单的防抖。
let searchTimer = 0, pickerTimer = 0;
export const navCount = (id, n, title = '') => { $(id).textContent = n ? number(n) : ''; $(id).title = title; };
// 一次元数据读取在屏幕上改动什么：侧栏的计数，以及显示它们的标题或页面。
function metaShown({error: e, total, directory, queue}) {
  if (e) { error(readError(e)); return; }
  // 图库计的是你能看到的；「整理」一节计的是还需要你处理的。
  navCount('countLibrary', total); navCount('countCharacters', state.chars.length);
  navCount('countRecent', state.recentCount); navCount('countHome', state.recentCount, t('最近一次同步新加入的图'));
  navCount('countUnmatched', state.pools.unmatched, t('还没认出角色的图'));
  navCount('countAudit', state.pools.audit, t('随机抽查还剩几张')); $('navAudit').hidden = !state.pools.audit && viewOf(state.route).nav !== 'audit';
  navCount('countDuplicates', state.dupInfo.mixed, t('角色不一致的副本组，对齐后才会折叠'));
  navCount('countExcluded', state.pendingCount, t('像截图、等你确认的图'));
  if (queue) { navCount('countIdentity', state.identityCount, t('长得像、可能是同一人或同一人另一形态的角色对')); $('navIdentity').hidden = !state.identityCount && viewOf(state.route).nav !== 'identity'; }
  // 角色还没读到时（state.char 为空）算图库页：这一刻不重画。
  const view = viewOf(state.route, state.char);
  if (view.onMeta) view.onMeta({directory}); else renderChrome();
}
export async function applyRoute() {
  const route = parseHash(); route.sort = route.sort || defaultSort(route);
  if (route.legacy) { delete route.legacy; history.replaceState(null, '', hashFor(route)); }
  const samePage = state.route.page === route.page && state.route.id === route.id && state.route.char === route.char;   // 切换子视图仍是同一页
  // 换页（含同一页换子视图、换参数）：大图关掉，因为它下面的列表要重读、序号不再对得上；选择和光标复位；
  // 详情栏只在换了页面或角色时关。大图是被换页关掉的，不动焦点。
  closeViewer('silent');
  setRoute(route); resetForRoute();
  if (!samePage) closeInspector();
  $('sidebar').classList.remove('mobile-open');
  const page = pageOf(route);
  for (const a of document.querySelectorAll('.nav')) a.classList.toggle('active', a.dataset.page === page.nav);
  $('settingsLink').classList.toggle('active', page.nav === 'settings');
  setChar(null);
  state.groupNear = null;
  if (page.prepare && await page.prepare(route) === false) return;
  if (route.char) {
    let character; try { character = await fetchChar(route.char); } catch (e) { if (state.route === route) error(readError(e)); return; }
    if (state.route !== route) return;
    setChar(character || {id: route.char, name: t('角色'), work: '', count: 0, unchecked: 0, variants: []});
  }
  $('homePage').hidden = page.area !== 'homePage';
  if (page.guideTick) guideTick(page.guideTick);
  renderChrome();
  if (page.render) page.render({samePage}); else await reload(true);   // 没有自己渲染入口的页面是网格页
}
// ---------- 外框：标题、工具栏 ----------
// 同一个侧栏入口的子视图在它的标题下显示成标签页，而不是塞在角落里的链接。
function subTabs(r) {
  const tab = (href, label, n, on, title = '') => `<a class="chip-filter${on ? ' on' : ''}" href="${href}"${title ? ` title="${esc(title)}"` : ''}>${label}${n ? ` <span>${n}</span>` : ''}</a>`;
  const p = state.pools, row = tabs => `<div class="chips-row">${tabs.join('')}</div>`;
  // 页签组和本页对应的页签由页面登记（core/pages.js 的 tabs、tab）。
  const at = viewOf(r).tab, all = at === 'all';
  switch (viewOf(r).tabs) {
    case 'unmatched': return row([
      tab('#/groups', t('按组整理'), '', at === 'groups', t('长得像的未识别图分成一组，一组一组确认')),
      tab('#/unmatched', t('全部未识别'), number(p.unmatched), all),
      tab('#/skipped', t('已跳过'), p.skipped ? number(p.skipped) : '', at === 'skipped')]);
    case 'duplicates': return row([
      tab('#/duplicates', t('全部'), t('{n} 组', {n: number(state.dupInfo.groups)}), all && !r.dup),
      tab('#/duplicates?dup=mixed', t('角色不一致'), t('{n} 组', {n: number(state.dupInfo.mixed)}), all && r.dup === 'mixed', t('几份副本标的角色不同；对齐后自动折叠')),
      tab('#/similar', t('相似的图'), '', at === 'similar', t('不是同一个文件，但画的是同一张'))]);
    case 'excluded': return row([
      tab('#/excluded', t('全部'), number(state.exclInfo.count), all && !r.reason),
      ...Object.entries(REASONS).filter(([k]) => state.exclInfo.reasons?.[k]).map(([k, label]) => tab('#/excluded?reason=' + k, label, number(state.exclInfo.reasons[k]), all && r.reason === k)),
      ...(state.pendingCount || at === 'pending' ? [tab('#/pending', t('待确认'), number(state.pendingCount), at === 'pending', t('像截图但画面里有人物，规则拿不准'))] : [])]);
  }
  return '';
}
export function renderChrome() {
  renderHeading();
  const r = state.route, bar = $('toolbar'), tabs = subTabs(r), view = viewOf(r);
  if (view.toolbar === 'none') { bar.innerHTML = ''; return; }
  if (view.toolbar === 'tabs') { bar.innerHTML = tabs; return; }
  if (view.toolbar === 'index') {
    // 资料库：「角色」用来浏览，「作品」用来管理；两者各自搜索自己的列表。
    const works = r.section === 'works';
    const views = `<div class="segmented lib-views" role="tablist" aria-label="${t('资料库')}">${[['', t('角色')], ['works', t('作品')]].map(([v, label]) => `<a role="tab" href="#/characters${v ? '/' + v : ''}" class="${(r.section || '') === v ? 'on' : ''}" aria-selected="${(r.section || '') === v}">${label}</a>`).join('')}</div>`;
    bar.innerHTML = views + `<div class="search-wrap wide">${searchIcon()}<input type="search" id="charSearch" value="${esc(works ? shelf.q : '')}" placeholder="${works ? t('搜索作品') : t('搜索角色或作品')}" aria-label="${works ? t('搜索作品') : t('搜索角色')}"></div><div class="spacer"></div>` +
      (works ? `<button class="quiet" id="newWork">${t('新建作品')}</button>` : `<button class="quiet" id="taggedButton" title="${t('吉祥物、玩家化身这类不算角色、只当标签的，在这里看全部、设置和改回')}">${t('只当标签')}${state.tagged?.length ? ` <small>${number(state.tagged.length)}</small>` : ''}</button><div class="segmented" id="charMode">${[['shelf', t('书架')], ['list', t('列表')]].map(([v, label]) => `<button data-mode="${v}" class="${cast.mode === v ? 'on' : ''}">${label}</button>`).join('')}</div>`);
    if (works) {
      $('charSearch').oninput = e => { shelf.q = e.target.value; drawShelf(); };
      $('newWork').onclick = createWork;
      return;
    }
    $('charSearch').oninput = () => { clearTimeout(searchTimer); searchTimer = setTimeout(searchIndex, 200); };
    $('taggedButton').onclick = openTagged;
    for (const b of $('charMode').children) b.onclick = () => { cast.mode = b.dataset.mode; prefs.set('castMode', cast.mode); for (const x of $('charMode').children) x.classList.toggle('on', x === b); drawCast(); };
    return;
  }
  const tagPage = !!(r.char && state.char?.mascot);
  const SORT_LABELS = {suspicious: t('可疑优先'), newest: t('最新加入'), oldest: t('最早加入'), hinted: t('有线索优先')};
  const sorts = view.sorts.filter(v => !(tagPage && v === 'suspicious')).map(v => [v, SORT_LABELS[v]]);
  const skins = r.char && !r.maybe ? (state.char?.variants || []).filter(v => v.count) : [];
  const variants = skins.length ? `<div class="chips-row">${[{id: '', name: t('全部'), count: state.char.count}, ...skins].map(v => `<button class="chip-filter${(r.variant || '') === v.id ? ' on' : ''}" data-variant="${esc(v.id)}">${esc(v.name)} <span>${number(v.count)}</span></button>`).join('')}</div>` : '';
  const maybeChip = r.char && !tagPage && (state.maybeCount || r.maybe || state.doubtCount || r.doubt) ? `<div class="chips-row">${state.maybeCount || r.maybe ? `<button class="chip-filter${r.maybe ? ' on' : ''}" id="maybeChip" title="${t('机器觉得像 TA，但算给了别人或没算：确认「是 TA」或「不是 TA」')}">${t('可能也是 TA')} <span>${number(state.maybeCount)}</span></button>` : ''}${state.doubtCount || r.doubt ? `<button class="chip-filter${r.doubt ? ' on' : ''}" id="doubtChip" title="${t('单人图里，另一个角色的证据几乎一样强：多半两个都不对，看一眼再决定')}">${t('可能认错')} <span>${number(state.doubtCount)}</span></button>` : ''}</div>` : '';
  const filtersHTML = view.toolbar === 'library' ? `<div class="search-wrap">${searchIcon()}<input type="search" id="search" placeholder="${t('搜索角色、别名或文件名')}" aria-label="${t('搜索图库')}" value="${esc(r.q)}"><kbd>/</kbd></div>` +
    (r.char ? `<button class="quiet on" id="charPill" title="${t('清除角色筛选')}">${t('角色 · {name}', {name: esc(short(state.char?.name || ''))})} ×</button>` : '') +
    (r.utag ? `<button class="quiet on" id="utagPill" title="${t('清除标签筛选')}"># ${esc(r.utag)} ×</button>` : '') +
    (r.day ? `<button class="quiet on" id="dayPill" title="${t('清除日期筛选')}">${t('{day} 加入', {day: esc(dayLabel(r.day))})} ×</button>` : '') +
    `<div class="tag-control"><button id="tagButton" class="quiet${r.tag ? ' on' : ''}">${r.tag ? t('标签 · {tag}', {tag: esc(r.tag.replaceAll('_', ' '))}) + ' ×' : t('标签')}</button><div id="tagPopover" class="menu menu-drop" hidden><input id="tagSearch" type="search" placeholder="${t('我的标签，或 white_hair 这类画面标签')}" aria-label="${t('搜索标签')}"><div id="tagOptions"></div></div></div>` + maybeChip + variants : tabs;
  bar.innerHTML = filtersHTML +
    `<div class="spacer"></div><select id="sort" aria-label="${t('排序')}">${sorts.map(([v, label]) => `<option value="${v}"${r.sort === v ? ' selected' : ''}>${label}</option>`).join('')}</select>` +
    `<button class="quiet" id="selectButton" title="${cleanup() ? t('选中全部符合条件的图片') : t('进入选择模式')}">${cleanup() ? `${t('全选')} <kbd>${shortcut('A')}</kbd>` : t('选择')}</button>` +
    `<div class="icon-seg" id="fitToggle" role="group" aria-label="${t('显示方式')}"><button data-fit="0" class="${state.fit ? '' : 'on'}" title="${t('方格：裁成正方形（R）')}" aria-label="${t('方格')}">${FIT_ICONS.square}</button><button data-fit="1" class="${state.fit ? 'on' : ''}" title="${t('原始比例：完整显示整张图（R）')}" aria-label="${t('原始比例')}">${FIT_ICONS.fit}</button></div>`;
  $('sort').onchange = e => { if (view.lead) prefs.set('sortUnmatched', e.target.value); replaceRoute({sort: e.target.value}); reload(true); };
  $('selectButton').onclick = () => cleanup() ? selectAll() : toggleSelecting();
  for (const b of $('fitToggle').querySelectorAll('[data-fit]')) b.onclick = () => { if (Boolean(Number(b.dataset.fit)) !== state.fit) toggleFit(); };
  for (const b of bar.querySelectorAll('[data-variant]')) b.onclick = () => go({...r, variant: b.dataset.variant});
  $('maybeChip')?.addEventListener('click', () => go({...r, maybe: r.maybe ? '' : '1', variant: '', check: '', doubt: ''}));
  $('doubtChip')?.addEventListener('click', () => go({...r, doubt: r.doubt ? '' : '1', variant: '', check: '', maybe: ''}));
  if (view.toolbar === 'library') {
    $('search').oninput = e => { clearTimeout(searchTimer); searchTimer = setTimeout(() => { replaceRoute({q: e.target.value.trim()}); reload(true); }, 250); };
    $('tagButton').onclick = () => { if (r.tag) { replaceRoute({tag: ''}); renderChrome(); reload(true); return; } setMenu(tagMenu, $('tagPopover').hidden); if (!$('tagPopover').hidden) { $('tagSearch').focus(); loadTagOptions(); } };
    $('charPill')?.addEventListener('click', () => go({...r, char: '', variant: '', maybe: '', check: '', sort: ''}));
    $('utagPill')?.addEventListener('click', () => go({...r, utag: ''}));
    $('dayPill')?.addEventListener('click', () => go({...r, day: ''}));
    $('tagSearch').oninput = () => { clearTimeout(pickerTimer); pickerTimer = setTimeout(loadTagOptions, 120); };
    $('tagOptions').onclick = e => {
      const b = e.target.closest('[data-value],[data-utag]'); if (!b) return;
      if (b.dataset.utag) { go({...r, utag: b.dataset.utag}); return; }
      replaceRoute({tag: b.dataset.value}); renderChrome(); reload(true);
    };
  }
}
// 标签菜单和角色页的更多菜单是非模态浮层：Esc、点外关闭由浮层管理器（ui/overlay.js）统一处理。它们的元素随工具栏
// 和标题重画而换掉，所以层按 id 找元素；重画后旧的层自己会从栈里清掉。
const menuLayer = (id, popover, button) => {
  const layer = createLayer({kind: 'popover', id, el: () => $(popover), anchor: () => $(button), onDismiss: reason => setMenu(layer, false, reason)});
  layer.popover = popover;
  return layer;
};
const tagMenu = menuLayer('tag-menu', 'tagPopover', 'tagButton'), moreMenu = menuLayer('more-menu', 'morePopover', 'moreButton');
function setMenu(layer, open, reason) {
  const el = $(layer.popover); if (el) el.hidden = !open;
  if (open) layer.open(); else layer.close(reason);
}
const FIT_ICONS = {
  square: '<svg viewBox="0 0 20 20" aria-hidden="true"><rect x="3" y="3" width="6" height="6" rx="1.5"/><rect x="11" y="3" width="6" height="6" rx="1.5"/><rect x="3" y="11" width="6" height="6" rx="1.5"/><rect x="11" y="11" width="6" height="6" rx="1.5"/></svg>',
  fit: '<svg viewBox="0 0 20 20" aria-hidden="true"><rect x="2.5" y="3.5" width="9" height="5.5" rx="1.5"/><rect x="13" y="3.5" width="4.5" height="5.5" rx="1.5"/><rect x="2.5" y="11" width="4.5" height="5.5" rx="1.5"/><rect x="8.5" y="11" width="9" height="5.5" rx="1.5"/></svg>',
};
// 一个标签菜单：先是你自己的标签，然后是机器给图片打的标签（WD general tags）。
async function loadTagOptions() {
  const q = $('tagSearch').value.trim(), low = q.toLowerCase();
  const mine = state.userTags.filter(t => t.tag !== state.route.utag && (!q || t.tag.toLowerCase().includes(low))).slice(0, 12);
  try { const tags = await api('/api/general-tags?q=' + encodeURIComponent(q)); if (q !== $('tagSearch').value.trim()) return;
    $('tagOptions').innerHTML = (mine.length ? `<div class="palette-label">${t('我的标签')}</div>${mine.map(x => `<button class="menu-item" data-utag="${esc(x.tag)}"># ${esc(x.tag)} <small>${number(x.count)}</small></button>`).join('')}` : '') +
      (tags.length ? `<div class="palette-label">${t('画面标签')}</div>${tags.map(x => `<button class="menu-item" data-value="${esc(x)}">${esc(x.replaceAll('_', ' '))}</button>`).join('')}` : '') || `<p>${t('没有匹配的标签')}</p>`;
  } catch (e) { $('tagOptions').textContent = e.message; }
}
// 角色页的标题：名称、张数、未检查、检查日期，右侧是细分、标为已检查和 ⋯ 菜单。
function drawCharHeading(h, r, c) {
  const tagOnly = !!c.mascot;   // 只当标签：没有要复核的，所以没有检查、清扫、OC 这些控件
  const checked = !tagOnly && c.reviewed_at ? ' · ' + t('上次检查 {date}', {date: dateText(c.reviewed_at.slice(0, 4), c.reviewed_at.slice(5, 7), c.reviewed_at.slice(8, 10))}) : '';
  const unchecked = !tagOnly && (c.unchecked || r.check) ? ` · <a class="unchecked-link${r.check ? ' on' : ''}" href="${hashFor({...r, check: r.check ? '' : '1', maybe: ''})}" title="${r.check ? t('显示全部') : t('只看机器识别、还没检查过的图')}">${t('未检查 {n}', {n: number(c.unchecked)})}${r.check ? ' ×' : ''}</a>` : '';
  h.innerHTML = `<div class="heading-main"><h1 class="tag-title" title="${esc(nameTip(c))}">${esc(c.name)}${c.mascot ? '' : workSmall(c)}</h1><p>${c.mascot ? `<span class="oc-badge" title="${c.mascot_origin === 'default' ? t('默认设置：老师、博士、训练员这类没有固定形象的玩家化身和吉祥物，默认只当标签。') : ''}${t('不算角色：不出现在角色列表里，也不算多人图；图上仍挂着这个标签。在「⋯」里可以改回角色')}">${t('只当标签')}</span> ` : ''}${t('{n} 张', {n: number(c.count)})}${unchecked}${checked}${!c.tag && c.kind !== 'oc' && !c.label_only ? ` · <button class="link-button" id="profileNudge" title="${t('关联后自动补中文名、别名和所属作品')}">${t('关联 Danbooru…')}</button>` : ''}</p></div>
    <div class="heading-actions">${c.label_only ? `<button class="quiet" id="splitOcButton" title="${t('把长得像的图分成一组，一组一键设为独立的原创角色')}">${t('按相似细分')}</button>` : ''}${tagOnly ? '' : `<button class="quiet" id="sweepButton" ${c.unchecked ? '' : 'disabled'} title="${t('把剩下的机器识别都记为已检查；以后新识别的图会重新计入未检查')}">${t('标为已检查')}</button>`}<div class="tag-control more-control"><button class="quiet" id="moreButton" title="${t('更多操作')}" aria-label="${t('更多操作')}">⋯</button><div id="morePopover" class="menu menu-drop more-menu" hidden>${c.label_only || tagOnly ? '' : `<button class="menu-item" id="ocButton" title="${t('画师自己的原创角色，不属于任何作品')}">${c.kind === 'oc' ? t('取消原创角色') : t('标为原创角色')}</button>`}${c.label_only ? '' : `<button class="menu-item" id="mascotButton" title="${c.mascot ? t('恢复成角色：出现在角色列表里，同框就算多人图') : t('像佩洛洛这样的吉祥物、玩家化身：不算角色，不算多人图；图上仍挂着这个标签，可以按它筛选')}">${c.mascot ? t('算作角色') : t('不算角色，只当标签')}</button>`}<button class="menu-item" id="profileButton" title="${t('名称、别名、所属作品、Danbooru 关联')}">${t('编辑资料…')}</button><button class="menu-item" id="mergeButton" title="${t('和另一个角色是同一个人：把图和人工结论都并过去（可撤销）')}">${t('合并到另一个角色…')}</button></div></div></div>`;
  $('sweepButton')?.addEventListener('click', sweep);
  $('moreButton').onclick = () => setMenu(moreMenu, $('morePopover').hidden);
  $('mergeButton').onclick = () => { setMenu(moreMenu, false); mergeCharacter(c); };
  $('profileButton').onclick = () => { setMenu(moreMenu, false); editProfile(c); };
  $('profileNudge')?.addEventListener('click', () => editProfile(c));
  $('ocButton')?.addEventListener('click', () => { setMenu(moreMenu, false); toggleOc(c); });
  $('mascotButton')?.addEventListener('click', () => { setMenu(moreMenu, false); toggleMascot(c); });
  $('splitOcButton')?.addEventListener('click', () => splitOc(c));
}
export function renderHeading() {
  const r = state.route, h = $('heading'), c = state.char, view = viewOf(r, c);
  if (view.drawHeading) { view.drawHeading(h, r, c); return; }
  const n = state.totalReady ? number(state.total) : '…';
  const clear = ` <a class="link" href="#/library">${t('清除筛选')}</a>`;
  const [title, sub, links = ''] = view.heading(r, {n, clear});
  h.innerHTML = `<div class="heading-main"><h1>${title}</h1><p>${sub}</p></div>${links}`;
  $('localLinksButton')?.addEventListener('click', openLocalLinks);
  $('sgroupRelate')?.addEventListener('click', async () => openRelate(await api('/api/ids?' + new URLSearchParams({...filters(), limit: 12}))));
  $('groupOc')?.addEventListener('click', () => markOc({filter: filters(), exclude: [], confirm: state.total > 500}, false, true));
}

// 页面这一部分注册的处理函数。页面启动时调用一次。
export function wireShell() {
  onMeta(metaShown);
  window.addEventListener('hashchange', applyRoute);
}

// ---------- 登记页面 ----------
// 标题（页面名 → [标题, 副标题, 右侧链接]）、计数到达后重画什么、进入前要先读的数据。
implement({
  character: {drawHeading: drawCharHeading},
  home: {heading: (r, {n, clear}) => [t('首页'), t('按加入日期分组') + (state.recentCount ? ' · ' + t('最近一次同步新加入 {n} 张', {n: number(state.recentCount)}) : '')]},
  library: {heading: (r, {n, clear}) => [r.artist ? t('画师 · {name}', {name: esc(artistName(r.artist))}) : r.day ? t('{day} 加入', {day: esc(dayLabel(r.day))}) : r.work ? esc((state.workTree || []).find(w => w.id === r.work)?.name || t('作品筛选')) : r.utag ? '# ' + esc(r.utag) : t('图库'), t('{n} 张', {n}) + (r.work ? ' · ' + t('包含所有子作品') + clear : r.artist ? ' · ' + t('按画作比对找到') + clear : '')]},
  characters: {heading: (r, {n, clear}) => [t('资料库'), r.section === 'works' ? t('{n} 部作品', {n: number(state.index?.works?.length || 0)}) + (state.index?.watch?.works.length ? ' · ' + t('关注 {n} 部', {n: number(state.index.watch.works.length)}) : '') : t('{n} 位角色 · {works} 部作品', {n: number(state.chars.length), works: number(new Set(state.chars.map(workKey).filter(k => k !== 'oc' && k !== 'other')).size)}), !r.section && state.localLinks?.length ? `<span class="heading-links"><button class="quiet" id="localLinksButton" title="${t('关联后自动补中文名、别名和作品；和已有角色重复的可以合并')}">${t('{n} 个自建角色能对上 Danbooru', {n: number(state.localLinks.length)})}</button></span>` : '']},
  recent: {heading: (r, {n, clear}) => [t('新加入'), t('最近一次同步加入的 {n} 张，已自动识别', {n: number(state.recentCount)})]},
  audit: {heading: (r, {n, clear}) => [t('随机抽查'), t('还剩 {left} / {n} 张 · 鼠标移到角色 tag 上点 ✓ / ✗，或选中多张按 Y / N；打开大图也会问「是 TA 吗？」', {left: number(state.pools.audit || 0), n}) + (state.nsfw ? ` · <b>${t('R18 筛选开着：抽查要看全部才准，按 H 切换')}</b>` : '')]},
  unmatched: {heading: (r, {n, clear}) => [t('未识别'), t('{n} 张 · 选中后设为角色，或跳过', {n})]},
  skipped: {heading: (r, {n, clear}) => [t('未识别'), t('已跳过 {n} 张 · 可以恢复到未识别', {n})]},
  groups: {heading: (r, {n, clear}) => [t('未识别'), t('长得像的图分成一组，一组一组过 · 每确认一组，系统就学会认 TA 的一种样子')]},
  group: {heading: (r, {n, clear}) => [t('第 {id} 组', {id: esc(r.id)}), t('{n} 张长得像的未识别图', {n}) + (state.groupTraits?.length ? ' · ' + state.groupTraits.map(esc).join(' · ') : ''),
      `<span class="heading-links">${state.total ? `<button class="quiet" id="groupOc" title="${t('整组都是同一个原创角色：新建一个原创角色，把这组图都设为它')}">${t('整组标为原创角色')}</button>` : ''}${state.groupNear ? `<a class="quiet link" href="#/g/${state.groupNearId}">${esc(state.groupNear)} →</a>` : ''}</span>`]},
  duplicates: {heading: (r, {n, clear}) => [t('重复'), r.dup ? t('角色不一致 {n} 组 · 对齐角色后自动折叠', {n: number(state.dupInfo.mixed)}) : t('{groups} 组 · {n} 张 · 修改任一份会同步到整组', {groups: number(state.dupInfo.groups), n: number(state.dupInfo.count || 0)})]},
  similar: {heading: (r, {n, clear}) => [t('重复'), t('画的是同一张、但不是同一个文件 · 只在本机比较')]},
  'artist-audit': {heading: (r, {n, clear}) => [t('画师抽查'), t('验收「找回画师」：固定的一组随机图，逐张判断机器找到的画师对不对')]},
  identity: {heading: (r, {n, clear}) => [t('身份整理'), t('{n} 对长得像的角色 · 按涉及图片数排序 · 每一步都可撤销', {n: number(state.identityCount || 0)})]},
  sgroup: {heading: (r, {n, clear}) => [t('相似组 {id}', {id: esc(r.id)}), t('{n} 张 · 选中两张以上可以判断关系', {n}), state.total >= 2 && state.total <= 12 ? `<span class="heading-links"><button class="quiet" id="sgroupRelate">${t('判断关系')}</button></span>` : '']},
  excluded: {heading: (r, {n, clear}) => [t('已排除'), t('{n} 张 · 只是标记，不动 Photos', {n})]},
  pending: {heading: (r, {n, clear}) => [t('已排除'), t('待确认 {n} 张 · 像截图但有人物，排除或保留', {n})]},
  settings: {heading: (r, {n, clear}) => [t('设置'), settingsSection()[2]]},
});
implement({
  // 图库页不因计数变化重画；资料库只重画目录；设置页重画整页；其余重画标题和工具栏。
  library: {onMeta: () => {}},
  characters: {onMeta: ({directory}) => { if (directory && (state.route.section || !$('charSearch')?.value)) renderIndexBody(state.chars); }},
  settings: {onMeta: () => renderSettings()},
  // 一组图的页面要先读分组：相近的一组和这组的特征。
  group: {prepare: async route => {
    let result; try { result = await api('/api/clusters?nsfw=' + state.nsfw); } catch (e) { if (state.route === route) error(readError(e)); return false; }
    if (state.route !== route) return false;  // 分组加载期间已经导航走了：这个回应不属于任何页面
    const g = result.clusters.find(x => String(x.cid) === route.id);
    state.groupNear = g?.near ? nearText(g) : null; state.groupNearId = g?.near || null; state.groupTraits = g?.traits || [];
  }},
});
