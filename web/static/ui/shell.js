// 页面的框架（桌面布局，#199）：按地址切到对应页面、侧栏的计数、每个页面的标题和工具栏。页面在哪由 core/route.js
// 负责；计数的数据由 core/meta.js 负责。
import {workKey} from '../core/catalog.js';
import {dayLabel, esc, isMac, localDate, number, shortcut} from '../core/format.js';
import {t} from '../core/i18n.js';
import {REASONS, nameTip} from '../core/labels.js';
import {fetchChar, onMeta, setChar} from '../core/meta.js';
import {prefs} from '../core/prefs.js';
import {implement, pageOf, viewOf} from '../core/pages.js';
import {canGoBack, cleanup, defaultSort, go, hashFor, historyEntry, noteHistory, parseHash, replaceRoute, setRoute, sortKey, stepHistory} from '../core/route.js';
import {api, readError} from '../core/session.js';
import {state} from '../core/state.js';
import {navFor} from '../core/favorites.js';
import {cast, createWork, drawCast, drawShelf, openTagged, renderIndexBody, searchIndex, shelf} from './catalog.js';
import {dirDrawn, dirHeading, dirToolbar, onDir, wireDirToolbar} from './artist_dir.js';
import {$, emptyLink, error, helpTip, searchIcon, showArea, wireHelpTip, workSmall} from './dom.js';
import {renderSettings, settingsSection} from './settings.js';
import {closeInspector, closeViewer} from './detail.js';
import {filters, markSelectButton, placeFor, rememberPlace, reload, resetForRoute, selectAll, setNsfw, short, sweep, toggleFit, toggleSelecting} from './grid.js';
import {guideTick} from './guide.js';
import {createLayer} from './overlay.js';
import {filterBar, wireFilterBar} from './filters.js';
import {saveButton, wireGalleries, wireSaveButton} from './galleries.js';
import {editProfile, markOc, mergeCharacter, openLocalLinks, openRelate, splitOc, toggleMascot, toggleOc} from './palette.js';

// 画师筛选的标题里显示的画师名：第一次用到时去读，读到后重画标题。
const artistNames = {};
function artistName(id) {
  if (!(id in artistNames)) {
    artistNames[id] = '…';
    api('/api/artist?' + new URLSearchParams({id})).then(r => {
      artistNames[id] = r.name || t('未知画师');
      if (state.route.artist !== id) return;
      renderHeading();
    }).catch(e => error(readError(e)));
  }
  return artistNames[id];
}
// 搜索框的防抖。
let searchTimer = 0;
export const navCount = (id, n, title = '') => { $(id).textContent = n ? number(n) : ''; $(id).title = title; };
// 一次元数据读取在屏幕上改动什么：侧栏的计数，以及显示它们的标题或页面。
function metaShown({error: e, directory, queue}) {
  if (e) { error(readError(e)); return; }
  // 侧栏只在首页和「整理」一节计数，计的是还需要你处理的；点进去落在显示同一个数的页签上（#386 的 2-40）：
  // 有角色不一致的副本组时「重复」去「角色不一致」，有待确认的图时「已排除」去「待确认」。
  navCount('countHome', state.recentCount, t('最近一次同步新加入的图'));
  navCount('countUnmatched', state.pools.unmatched, t('还没认出角色的图'));
  navCount('countAudit', state.pools.audit, t('随机抽查还剩几张')); $('navAudit').hidden = !state.pools.audit && viewOf(state.route).nav !== 'audit';
  navCount('countDuplicates', state.dupInfo.mixed, t('角色不一致的副本组'));
  navCount('countExcluded', state.pendingCount, t('像截图、待确认的图'));
  $('navDuplicates').href = state.dupInfo.mixed ? hashFor({page: 'duplicates', dup: 'mixed'}) : '#/duplicates';
  $('navExcluded').href = state.pendingCount ? '#/pending' : '#/excluded';
  if (queue) { navCount('countIdentity', state.identityCount, t('长得像、可能是同一人或同一人另一个造型的角色对')); $('navIdentity').hidden = !state.identityCount && viewOf(state.route).nav !== 'identity'; }
  // 角色还没读到时（state.char 为空）算图库页：这一刻不重画。
  const view = viewOf(state.route, state.char);
  if (view.onMeta) view.onMeta({directory}); else renderChrome();
}
export async function applyRoute() {
  const route = parseHash(); route.sort = route.sort || defaultSort(route);
  const legacy = route.legacy; delete route.legacy;
  // 要离开的是自己渲染的页面（首页、资料库、设置……）：按历史记录的序号记下它滚到哪里，后退、前进回到这一条时恢复（#526）。
  if (pageOf(state.route).render) panelScroll.set(historyEntry(), $('viewport').scrollTop);
  const traversed = noteHistory(legacy ? hashFor(route) : undefined);   // 旧地址就地改成新的；这一条记下序号，后退不退出页面
  $('backButton').hidden = !canGoBack();
  const samePage = state.route.page === route.page && state.route.id === route.id && state.route.char === route.char;   // 切换子视图仍是同一页
  // 换页（含同一页换子视图、换参数）：大图关掉，因为它下面的列表要重读、序号不再对得上；选择和光标复位；
  // 详情栏只在换了页面或角色时关。大图是被换页关掉的，不动焦点。
  closeViewer('silent');
  // 再记下要离开的网格页停在哪里，回到这个地址时恢复（#402）。放在关大图之后：关大图让详情栏跟到大图里最后看的那张、
  // 把光标那张滚进视野，记下的详情栏、光标和位置才是同一张。
  rememberPlace();
  setRoute(route); resetForRoute();
  if (!samePage) closeInspector();
  $('sidebar').classList.remove('mobile-open');
  const page = pageOf(route);
  // 侧栏「收藏」是只按收藏筛选的图库（#447），这时亮它、不亮「图库」。
  const nav = navFor(route, page);
  for (const a of document.querySelectorAll('.nav')) a.classList.toggle('active', a.dataset.page === nav);
  $('settingsLink').classList.toggle('active', page.nav === 'settings');
  setChar(null);
  if (page.prepare && await page.prepare(route) === false) return;
  if (route.char) {
    let character; try { character = await fetchChar(route.char); } catch (e) { if (state.route === route) error(readError(e)); return; }
    if (state.route !== route) return;
    setChar(character || {id: route.char, name: t('角色'), work: '', count: 0, unchecked: 0, variants: []});
  }
  showArea(page.area);
  if (page.guideTick) guideTick(page.guideTick);
  renderChrome();
  // 自己渲染的页面（首页、资料库、设置、按组整理……）换到时从顶部开始，不沿用上一页的滚动位置；后退、前进回到记过
  // 位置的那一条时，画完再滚回去（restore 让首页按上次读到的天数读，见 ui/home.js）。同一页换子视图、刷新时由页面自己
  // 决定留在哪里。网格页由 reload 定位（回到记过的地址时恢复，否则回顶部）。
  const saved = traversed ? panelScroll.get(historyEntry()) : undefined;
  if (page.render && !samePage) $('viewport').scrollTop = 0;
  if (!page.render) { await reload(true, placeFor(route)); return; }   // 没有自己渲染入口的页面是网格页
  const drawn = page.render({samePage, restore: saved !== undefined});
  if (saved === undefined) return;
  await drawn;
  if (state.route === route) $('viewport').scrollTop = saved;
}
// 自己渲染的页面在各条历史记录里停在哪里：{序号: scrollTop}。
const panelScroll = new Map();
// ---------- 外框：标题、工具栏 ----------
// 同一个侧栏入口的子视图在它的标题下显示成标签页，而不是塞在角落里的链接。
function subTabs(r) {
  const tab = (href, label, n, on, title = '') => `<a class="chip-filter${on ? ' on' : ''}" href="${href}"${title ? ` title="${esc(title)}"` : ''}>${label}${n ? ` <span>${n}</span>` : ''}</a>`;
  const p = state.pools, row = tabs => `<div class="chips-row">${tabs.join('')}</div>`;
  // 页签组和本页对应的页签由页面登记（core/pages.js 的 tabs、tab）。
  const at = viewOf(r).tab, all = at === 'all';
  switch (viewOf(r).tabs) {
    case 'unmatched': return row([
      // 未识别的总数写在按组整理上：侧栏「未识别」落在这里，组里的和没成组的合起来就是它。
      tab('#/groups', t('按组整理'), p.unmatched ? number(p.unmatched) : '', at === 'groups', t('按长相分组的未识别图')),
      tab('#/unmatched', t('全部未识别'), '', all),
      tab('#/skipped', t('已跳过'), p.skipped ? number(p.skipped) : '', at === 'skipped')]);
    case 'duplicates': return row([
      tab('#/duplicates', t('全部'), t('{n} 组', {n: number(state.dupInfo.groups)}), all && !r.dup),
      tab('#/duplicates?dup=mixed', t('角色不一致'), t('{n} 组', {n: number(state.dupInfo.mixed)}), all && r.dup === 'mixed', t('几份副本标的角色不同')),
      tab('#/similar', t('相似的图'), '', at === 'similar', t('同一幅画的不同文件'))]);
    case 'excluded': return row([
      tab('#/excluded', t('全部'), number(state.exclInfo.count), all && !r.reason),
      ...Object.entries(REASONS).filter(([k]) => state.exclInfo.reasons?.[k]).map(([k, label]) => tab('#/excluded?reason=' + k, label, number(state.exclInfo.reasons[k]), all && r.reason === k)),
      ...(state.pendingCount || at === 'pending' ? [tab('#/pending', t('待确认'), number(state.pendingCount), at === 'pending', t('像截图，但有人物'))] : [])]);
  }
  return '';
}
export function renderChrome() {
  renderHeading();
  const r = state.route, bar = $('toolbar'), tabs = subTabs(r), view = viewOf(r);
  if (view.toolbar === 'none') { bar.innerHTML = ''; return; }
  if (view.toolbar === 'tabs') { bar.innerHTML = tabs; return; }
  if (view.toolbar === 'index') {
    // 资料库：「角色」「画师」用来浏览，「作品」用来管理；各自搜索自己的列表。画师的工具栏在 ui/artist_dir.js（#448）。
    const works = r.section === 'works';
    const views = `<div class="segmented lib-views" role="tablist" aria-label="${t('资料库')}">${[['', t('角色')], ['works', t('作品')], ['artists', t('画师')]].map(([v, label]) => `<a role="tab" href="#/characters${v ? '/' + v : ''}" class="${(r.section || '') === v ? 'on' : ''}" aria-selected="${(r.section || '') === v}">${label}</a>`).join('')}</div>`;
    if (r.section === 'artists') { bar.innerHTML = views + dirToolbar(); wireDirToolbar(); return; }
    bar.innerHTML = views + `<div class="search-wrap wide">${searchIcon()}<input type="search" id="charSearch" spellcheck="false" autocorrect="off" autocapitalize="off" value="${esc(works ? shelf.q : '')}" placeholder="${works ? t('搜索作品') : t('搜索角色或作品')}" aria-label="${works ? t('搜索作品') : t('搜索角色')}"></div><div class="spacer"></div>` +
      (works ? `<button class="quiet" id="newWork">${t('新建作品')}</button>` : `<button class="quiet" id="taggedButton" title="${t('不算角色、只当标签的吉祥物和玩家化身')}">${t('只当标签')}${state.tagged?.length ? ` <small>${number(state.tagged.length)}</small>` : ''}</button><div class="segmented" id="charMode">${[['shelf', t('书架')], ['list', t('列表')]].map(([v, label]) => `<button data-mode="${v}" class="${cast.mode === v ? 'on' : ''}">${label}</button>`).join('')}</div>`);
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
  const SORT_LABELS = {suspicious: t('可疑优先'), newest: t('最新加入'), oldest: t('最早加入'), hinted: t('有线索优先'), similar: t('最相似')};
  const sorts = view.sorts.filter(v => !(tagPage && v === 'suspicious')).map(v => [v, SORT_LABELS[v]]);
  const skins = r.char && !r.maybe ? (state.char?.variants || []).filter(v => v.count) : [];
  const variants = skins.length ? `<div class="chips-row">${[{id: '', name: t('全部'), count: state.char.count}, ...skins].map(v => `<button class="chip-filter${(r.variant || '') === v.id ? ' on' : ''}" data-variant="${esc(v.id)}">${esc(v.name)} <span>${number(v.count)}</span></button>`).join('')}</div>` : '';
  const maybeChip = r.char && !tagPage && (state.maybeCount || r.maybe || state.doubtCount || r.doubt) ? `<div class="chips-row">${state.maybeCount || r.maybe ? `<button class="chip-filter${r.maybe ? ' on' : ''}" id="maybeChip" title="${t('机器觉得像 TA，但算给了别人或没算的图')}">${t('可能也是 TA')} <span>${number(state.maybeCount)}</span></button>` : ''}${state.doubtCount || r.doubt ? `<button class="chip-filter${r.doubt ? ' on' : ''}" id="doubtChip" title="${t('单人图里，另一个角色的证据几乎一样强的图')}">${t('可能认错')} <span>${number(state.doubtCount)}</span></button>` : ''}</div>` : '';
  // 生效的筛选各是一个胶囊，点 × 只清它自己（#386 的 2-30）。角色页的角色胶囊在这里；其余条件是筛选条（ui/filters.js，#445），
  // 旧的单值参数（tag、utag、work、artist、day）也显示在那里。
  const pill = (id, label, title) => `<button class="quiet on" id="${id}" title="${title}">${label} ×</button>`;
  const filtersHTML = view.toolbar === 'library' ? `<div class="search-wrap">${searchIcon()}<input type="search" id="search" spellcheck="false" autocorrect="off" autocapitalize="off" placeholder="${t('搜索角色、别名或文件名')}" aria-label="${t('搜索图库')}" value="${esc(r.q)}"><kbd>/</kbd></div>` +
    (r.char ? pill('charPill', t('角色 · {name}', {name: esc(short(state.char?.name || ''))}), t('清除角色筛选')) : '') +
    filterBar(r) + saveButton(r) + maybeChip + variants : tabs;
  bar.innerHTML = filtersHTML +
    `<div class="spacer"></div><select id="sort" aria-label="${t('排序')}">${sorts.map(([v, label]) => `<option value="${v}"${r.sort === v ? ' selected' : ''}>${label}</option>`).join('')}</select>` +
    `<button class="quiet" id="selectButton" title="${cleanup() ? t('选中全部符合条件的图片') : t('进入选择模式')}">${cleanup() ? `${t('全选')} <kbd>${shortcut('A')}</kbd>` : t('选择')}</button>` +
    `<div class="icon-seg" id="fitToggle" role="group" aria-label="${t('显示方式')}"><button data-fit="0" class="${state.fit ? '' : 'on'}" title="${t('方格：裁成正方形（R）')}" aria-label="${t('方格')}">${FIT_ICONS.square}</button><button data-fit="1" class="${state.fit ? 'on' : ''}" title="${t('原始比例：完整显示整张图（R）')}" aria-label="${t('原始比例')}">${FIT_ICONS.fit}</button></div>`;
  // 改了排序就记成这一类页面的默认值，离开再回来还是它（#386 的 2-26）；记在哪个键由 core/route.js 的 sortKey 定。
  $('sort').onchange = e => { prefs.set(sortKey(r), e.target.value); replaceRoute({sort: e.target.value}); reload(true); };
  $('selectButton').onclick = () => cleanup() ? selectAll() : toggleSelecting();
  markSelectButton();
  for (const b of $('fitToggle').querySelectorAll('[data-fit]')) b.onclick = () => { if (Boolean(Number(b.dataset.fit)) !== state.fit) toggleFit(); };
  for (const b of bar.querySelectorAll('[data-variant]')) b.onclick = () => go({...r, variant: b.dataset.variant});
  $('maybeChip')?.addEventListener('click', () => go({...r, maybe: r.maybe ? '' : '1', variant: '', check: '', doubt: ''}));
  $('doubtChip')?.addEventListener('click', () => go({...r, doubt: r.doubt ? '' : '1', variant: '', check: '', maybe: ''}));
  if (view.toolbar === 'library') {
    $('search').oninput = e => { clearTimeout(searchTimer); searchTimer = setTimeout(() => { replaceRoute({q: e.target.value.trim()}); reload(true); }, 250); };
    wireFilterBar(r); wireSaveButton(r);
    $('charPill')?.addEventListener('click', () => go({...r, char: '', variant: '', maybe: '', check: '', doubt: '', sort: ''}));
  }
}
// 角色页的更多菜单是非模态浮层：Esc、点外关闭由浮层管理器（ui/overlay.js）统一处理。它的元素随标题重画而换掉，所以层
// 按 id 找元素；重画后旧的层自己会从栈里清掉。打开时焦点进第一项，↑ ↓ 在各项之间移动焦点（menu，#422）。筛选条的
// 「＋ 条件」选择器在 ui/filters.js。
const menuLayer = (id, popover, button, menu) => {
  const layer = createLayer({kind: 'popover', id, el: () => $(popover), anchor: () => $(button), menu, focusIn: 'first', onDismiss: reason => setMenu(layer, false, reason)});
  layer.popover = popover;
  return layer;
};
const moreMenu = menuLayer('more-menu', 'morePopover', 'moreButton', true);
function setMenu(layer, open, reason) {
  const el = $(layer.popover); if (el) el.hidden = !open;
  if (open) layer.open(); else layer.close(reason);
}
const FIT_ICONS = {
  square: '<svg viewBox="0 0 20 20" aria-hidden="true"><rect x="3" y="3" width="6" height="6" rx="1.5"/><rect x="11" y="3" width="6" height="6" rx="1.5"/><rect x="3" y="11" width="6" height="6" rx="1.5"/><rect x="11" y="11" width="6" height="6" rx="1.5"/></svg>',
  fit: '<svg viewBox="0 0 20 20" aria-hidden="true"><rect x="2.5" y="3.5" width="9" height="5.5" rx="1.5"/><rect x="13" y="3.5" width="4.5" height="5.5" rx="1.5"/><rect x="2.5" y="11" width="4.5" height="5.5" rx="1.5"/><rect x="8.5" y="11" width="9" height="5.5" rx="1.5"/></svg>',
};
// 角色页的标题：名称、张数、未检查、检查日期，右侧是细分、标为已检查和 ⋯ 菜单。
function drawCharHeading(h, r, c) {
  const tagOnly = !!c.mascot;   // 只当标签：没有要复核的，所以没有检查、清扫、OC 这些控件
  const checked = !tagOnly && c.reviewed_at ? ' · ' + t('上次检查 {date}', {date: localDate(c.reviewed_at)}) : '';   // 后端存的是 UTC（#386 的 2-15）
  const unchecked = !tagOnly && (c.unchecked || r.check) ? ` · <a class="unchecked-link${r.check ? ' on' : ''}" href="${hashFor({...r, check: r.check ? '' : '1', maybe: ''})}" title="${r.check ? t('显示全部') : t('只看机器识别、还没检查过的图')}">${t('未检查 {n}', {n: number(c.unchecked)})}${r.check ? ' ×' : ''}</a>` : '';
  h.innerHTML = `<div class="heading-main"><h1 class="tag-title" title="${esc(nameTip(c))}">${esc(c.name)}${c.mascot ? '' : workSmall(c)}</h1><p>${c.mascot ? `<span class="oc-badge" title="${c.mascot_origin === 'default' ? t('老师、博士、训练员这类没有固定形象的玩家化身和吉祥物默认只当标签。') : ''}${t('不算角色：不出现在角色列表里，也不算多人图；图上仍挂着这个标签')}">${t('只当标签')}</span> ` : ''}${t('{n} 张', {n: number(c.count)})}${unchecked}${checked}${!c.tag && c.kind !== 'oc' && !c.label_only ? ` · <button class="link-button" id="profileNudge" title="${t('补上中文名、别名和所属作品')}">${t('关联 Danbooru…')}</button>` : ''}</p></div>
      <div class="heading-actions">${c.label_only ? `<button class="quiet" id="splitOcButton" title="${t('按长相分组，每组设为一个新的原创角色')}">${t('按相似细分')}</button>` : ''}${tagOnly ? '' : `<button class="quiet" id="sweepButton" ${c.unchecked ? '' : 'disabled'} title="${t('把剩下的机器识别都记为已检查；以后新识别的图会重新计入未检查')}">${t('标为已检查')}</button>`}<div class="tag-control more-control"><button class="quiet" id="moreButton" title="${t('更多操作')}" aria-label="${t('更多操作')}">⋯</button><div id="morePopover" class="menu menu-drop more-menu" hidden>${c.label_only || tagOnly ? '' : `<button class="menu-item" id="ocButton" title="${t('画师自己的原创角色，不属于任何作品')}">${c.kind === 'oc' ? t('取消原创角色') : t('标为原创角色')}</button>`}${c.label_only ? '' : `<button class="menu-item" id="mascotButton" title="${c.mascot ? t('恢复成角色：出现在角色列表里，同框就算多人图') : t('像佩洛洛这样的吉祥物、玩家化身：不算角色，不算多人图；图上仍挂着这个标签，可以按它筛选')}">${c.mascot ? t('算作角色') : t('不算角色，只当标签')}</button>`}<button class="menu-item" id="profileButton" title="${t('名称、别名、所属作品、Danbooru 关联')}">${t('编辑资料…')}</button><button class="menu-item" id="mergeButton" title="${t('把这个角色的图和人工结论并到另一个角色')}">${t('合并到另一个角色…')}</button></div></div></div>`;
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
  const [title, heading, links = ''] = view.heading(r, {n});
  // 撤销以后在等后台学习落定（#404）：计数还是撤销前的，落定后一次重读成最终值。
  const sub = state.learning ? [heading, t('正在更新')].filter(Boolean).join(' · ') : heading;
  // 标题下面只放数量、状态和必须一眼看到的警告；这一页是什么、怎么操作收进标题旁的「?」（#527）。网格页的操作提示
  // 就是注册表里的底栏提示（底栏默认隐藏），一个「·」分出一行，英文行首大写。
  const hints = view.helpHint ? (view.hint?.(r) || '').split(' · ').filter(Boolean).map(x => esc(x.charAt(0).toLocaleUpperCase() + x.slice(1))) : [];
  const [helpButton, helpPop] = helpTip([...(view.help?.(r) || []), ...hints]);
  h.innerHTML = `<div class="heading-main"><h1>${title}${helpButton}</h1><p>${sub}</p>${helpPop}</div>${links}`;
  wireHelpTip(); view.wireHeading?.(h, r);
  $('localLinksButton')?.addEventListener('click', openLocalLinks);
  $('sgroupRelate')?.addEventListener('click', async () => openRelate(await api('/api/ids?' + new URLSearchParams({...filters(), limit: 12}))));
  $('groupOc')?.addEventListener('click', () => markOc({filter: filters(), exclude: [], confirm: state.total > 500}, false, true));
}

// 页面这一部分注册的处理函数。页面启动时调用一次。
export function wireShell() {
  onMeta(metaShown);
  wireGalleries();
  dirDrawn.on(() => { if (onDir(state.route)) renderHeading(); });
  // 后退（#526）：标题左边的「‹」（只在能后退时显示）、Alt+← 或 ⌘[（键盘处理在 ui/grid.js）、鼠标侧键，都走
  // core/route.js 的 stepHistory，只在这个图库的页面之间走。鼠标侧键在 mouseup 时拦下浏览器自己的后退，否则
  // 它会退出图库页面。
  $('backButton').onclick = () => stepHistory(-1);
  $('backButton').title = t('后退（{key}）', {key: isMac ? '⌘[' : 'Alt+←'});
  window.addEventListener('mouseup', e => { if (e.button === 3 || e.button === 4) { e.preventDefault(); stepHistory(e.button === 3 ? -1 : 1); } });
  $('empty').addEventListener('click', e => {
    const act = e.target.closest('[data-empty]')?.dataset.empty;
    if (act === 'nsfw') setNsfw('');
    else if (act) viewOf(state.route).emptyAct?.(act);
  });
  window.addEventListener('hashchange', applyRoute);
}

// ---------- 登记页面 ----------
// 标题（页面名 → [标题, 副标题, 右侧链接]）、计数到达后重画什么、进入前要先读的数据。
implement({
  character: {drawHeading: drawCharHeading},
  home: {heading: (r, {n}) => [t('首页'), state.recentCount ? t('最近一次同步新加入 {n} 张', {n: number(state.recentCount)}) : '']},
  library: {heading: (r, {n}) => [r.artist ? t('画师 · {name}', {name: esc(artistName(r.artist))}) : r.day ? t('{day} 加入', {day: esc(dayLabel(r.day))}) : r.work ? esc((state.workTree || []).find(w => w.id === r.work)?.name || t('作品筛选')) : r.utag ? '# ' + esc(r.utag) : t('图库'), t('{n} 张', {n})]},
  characters: {heading: (r, {n}) => [t('资料库'), r.section === 'artists' ? dirHeading() : r.section === 'works' ? t('{n} 部作品', {n: number(state.index?.works?.length || 0)}) + (state.index?.watch?.works.length ? ' · ' + t('关注 {n} 部', {n: number(state.index.watch.works.length)}) : '') : t('{n} 位角色 · {works} 部作品', {n: number(state.chars.length), works: number(new Set(state.chars.map(workKey).filter(k => k !== 'oc' && k !== 'other')).size)}), !r.section && state.localLinks?.length ? `<span class="heading-links"><button class="quiet" id="localLinksButton" title="${t('补上中文名、别名和作品，和已有角色重复的可以合并')}">${t('{n} 个自建角色能对上 Danbooru', {n: number(state.localLinks.length)})}</button></span>` : '']},
  audit: {heading: (r, {n}) => [t('随机抽查'), t('还剩 {left} / {n} 张', {left: number(state.pools.audit || 0), n}) + (state.nsfw ? ` · <b>${t('R18 筛选开着：抽查要看全部才准，按 H 切换')}</b>` : '')]},
  unmatched: {heading: (r, {n}) => [t('未识别'), '']},
  skipped: {heading: (r, {n}) => [t('未识别'), '']},
  groups: {heading: (r, {n}) => [t('未识别'), '']},
  // 一组整理完了，标题不写「0 张」，由空状态给去处（#386 的 2-12）。
  group: {heading: (r, {n}) => [t('第 {id} 组', {id: esc(r.id)}), [state.totalReady && !state.total ? '' : t('{n} 张长得像的未识别图', {n}), ...(state.groupTraits || []).map(esc)].filter(Boolean).join(' · '),
      `<span class="heading-links">${state.total ? `<button class="quiet" id="groupOc" title="${t('新建一个原创角色，把这组图都设为它')}">${t('整组标为原创角色')}</button>` : ''}${state.groupNear ? `<a class="quiet link" href="#/g/${state.groupNearId}">${esc(state.groupNear)} →</a>` : ''}</span>`]},
  duplicates: {heading: (r, {n}) => [t('重复'), r.dup ? '' : t('{n} 张', {n: number(state.dupInfo.count || 0)})]},
  similar: {heading: (r, {n}) => [t('重复'), '']},
  'artist-audit': {heading: (r, {n}) => [t('画师抽查'), state.nsfw ? `<b>${t('这页不按 R18 筛选：R18 的图也在里面')}</b>` : '']},
  identity: {heading: (r, {n}) => [t('身份整理'), t('{n} 对长得像的角色', {n: number(state.identityCount || 0)})]},
  excluded: {heading: (r, {n}) => [t('已排除'), '']},
  pending: {heading: (r, {n}) => [t('已排除'), '']},
  settings: {heading: (r, {n}) => [t('设置'), settingsSection()[2]]},
});
// 各页「?」里的说明（#527）：help(r) 是这一页的说明，一行一句；helpHint 表示再接上注册表里的操作提示。设置的副标题
// 本来就是这一节的内容清单，留在原处；资料库、角色页的副标题都是数量和状态，没有「?」。
implement({
  home: {help: () => [t('按加入日期分组。')]},
  library: {help: r => r.work ? [t('包含所有子作品的图。')] : r.artist ? [t('画师是按画作比对找到的。')] : [], helpHint: true},
  audit: {help: () => [t('鼠标移到角色 tag 上，点 ✓ 或 ✗ 回答。'), t('选中多张后按 Y（是）或 N（不是）。'), t('打开大图也可以回答。'), t('看不清的可以跳过，换一张。')]},
  unmatched: {helpHint: true},
  skipped: {help: () => [t('选中的图可以恢复到未识别。')], helpHint: true},
  groups: {help: () => [t('按长相分组的未识别图，可以整组确认。'), t('点一下缩略图，把它排除在这组之外。'), t('长按缩略图，或鼠标停在上面按 Space，看大图。'), t('组头的按钮对整组生效。')]},
  group: {helpHint: true},
  duplicates: {help: () => [t('同色角标是同一组。'), t('组里的角色不一致时不折叠，对齐以后折叠成一张。')]},
  similar: {help: () => [t('同一幅画的不同文件。'), t('点一组进去看。')]},
  'artist-audit': {help: () => [t('一组固定的随机图，逐张核对「找回画师」找到的画师。'), t('每次打开都是同一批。')]},
  identity: {help: () => [t('按涉及的图片数排序。'), t('高亮的按钮是外部资料建议的做法。')]},
  sgroup: {help: () => [t('选中两张以上可以判断它们的关系。')], helpHint: true},
  excluded: {helpHint: true},
  pending: {help: () => [t('像截图，但有人物。')], helpHint: true},
});
implement({
  // 图库页不因计数变化重画；资料库只重画目录；设置页重画整页；其余重画标题和工具栏。
  library: {onMeta: () => {}},
  characters: {onMeta: ({directory}) => { if (directory && (state.route.section || !$('charSearch')?.value)) renderIndexBody(state.chars); }},
  settings: {onMeta: () => renderSettings()},
});
// 网格空了时的去处（#386 的 2-12、2-16）：待确认、已跳过清空了回到同一入口的主页签；「只看 R18」时图库空了给「显示全部」。
// 一组整理完了的去处由 ui/review.js 登记。按钮由 wireShell 里 #empty 的点击处理。
const showAll = r => state.nsfw === 'only' ? `<button class="quiet" data-empty="nsfw">${t('显示全部')}</button>` : '';
implement({
  library: {emptyNext: showAll},
  character: {emptyNext: showAll},
  like: {emptyNext: showAll},
  skipped: {emptyNext: () => emptyLink('#/groups', t('回到按组整理'))},
  pending: {emptyNext: () => emptyLink('#/excluded', t('回到已排除'))},
});
