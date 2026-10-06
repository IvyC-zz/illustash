'use strict';
// Illustash front end: plain JS, no build step. Machine identities count by default;
// the user only removes what is wrong. Every write is undoable.
const $ = id => document.getElementById(id);
const esc = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
// Keep displayed modifiers consistent across static help and dynamic messages.
const isMac = /Mac|iPhone|iPad|iPod/.test(navigator.userAgentData?.platform || navigator.platform || '');
const shortcut = key => isMac ? `⌘${key}` : `Ctrl+${key}`;
function platformHints(root) {
  if (isMac) return;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  while (walker.nextNode()) walker.currentNode.nodeValue = walker.currentNode.nodeValue.replace(/⌘(?=\s|$)/g, 'Ctrl').replaceAll('⌘', 'Ctrl+');
  for (const el of root.querySelectorAll('[title]')) el.title = el.title.replace(/⌘(?=\s|$)/g, 'Ctrl').replaceAll('⌘', 'Ctrl+');
}
platformHints(document.body);
const number = n => Number(n || 0).toLocaleString();
const work = w => (w || '').replaceAll('_', ' ');
// Display names come from the server (中文 → 日文 → readable English); the original tag only shows on hover.
const nameTip = c => !c ? '' : c.tag && c.tag !== c.name ? c.tag + (c.name_source === 'model' ? ' · 机器翻译' : c.name_source === 'wiki' ? ' · 来自 Danbooru 别名' : '') : c.name;
// Zoom is a continuous target column width; the grid snaps to whole columns and animates between them.
const SIZE_MIN = 72, SIZE_MAX = 480, MAX_HEIGHT = 3e7;
const sliderToSize = v => Math.round(SIZE_MIN * Math.pow(SIZE_MAX / SIZE_MIN, v / 100));
const sizeToSlider = s => Math.round(100 * Math.log(s / SIZE_MIN) / Math.log(SIZE_MAX / SIZE_MIN));
const PAGE = 100;
const prefs = {
  get(key, fallback) { try { const v = localStorage.getItem('pi.' + key); return v === null ? fallback : JSON.parse(v); } catch { return fallback; } },
  set(key, value) { try { localStorage.setItem('pi.' + key, JSON.stringify(value)); } catch {} },
};

const state = {
  route: {page: 'library'}, token: '', undo: null, busy: false,
  chars: [], charMap: new Map(), pins: [], char: null, pools: {unmatched: 0, skipped: 0}, dupInfo: {groups: 0, mixed: 0}, exclInfo: {count: 0, reasons: {}}, pendingCount: 0, sync: {status: 'off'}, syncHistory: [], settings: null, recentCount: 0, maybeCount: 0, userTags: [],
  items: [], total: 0, readEpoch: 0, pages: new Set(), pending: new Set(), generation: 0, dataVersion: 0, gridKey: '',
  cols: 1, cellW: 180, rowH: 250, gap: 12, size: Math.min(SIZE_MAX, Math.max(SIZE_MIN, prefs.get('size', 185))), fit: prefs.get('fit', false),
  selecting: false, sel: {all: false, ids: new Set(), exclude: new Set()}, anchor: -1, cursor: -1,
  focused: null, detail: null, viewer: -1, armed: null, nsfw: ['', 'hide', 'only'].includes(prefs.get('nsfw', '')) ? prefs.get('nsfw', '') : '',
};
let abortList, toastTimer, searchTimer, pageTimer, pickerTimer, detailGen = 0;

async function api(path, options = {}) {
  const response = await fetch(path, {...options, headers: {'Content-Type': 'application/json', 'X-CSRF-Token': state.token, ...options.headers}});
  const result = await response.json();
  if (!response.ok) throw new Error(result.error || '请求失败');
  return result;
}
async function post(path, body) {
  const result = await api(path, {method: 'POST', body: JSON.stringify(body)});
  if (path === '/api/suggest') return result;  // read-only; POST only carries the selection
  state.countDirty = true; state.readEpoch++;
  // Prevent an older metadata read from overwriting the just-completed mutation.
  if (metaPending) { metaRevision++; metaFull ||= path !== '/api/tag'; }
  return result;
}
function error(message) { $('errorBanner').textContent = message; $('errorBanner').hidden = !message; }
function readError(e) { return '读取失败：' + (e?.message || String(e)); }
function readFailure(node, e) { if (node) { node.hidden = false; node.innerHTML = `<p class="empty" role="alert">${esc(readError(e))}</p>`; } }
async function checkIndex() {
  const token = state.token;
  if (!await confirmAction('本次检查会联网查询 Danbooru，只发送关注作品的 tag，不发送图库图片。允许本次检查联网吗？', '检查新角色') || token !== state.token) return false;
  const result = await post('/api/index', {action: 'check', allow_network: true});
  if (token !== state.token) return false;
  state.index = result; return true;
}

// Name text queries (tag and work names only) need this library's consent: the backend refuses with
// this exact message until it is given. Any other failure is a real network or service error.
const NAMES_UNAUTHORIZED = '显示名联网尚未授权';
const NAMES_NOTE = '允许这个图库联网查询名称吗？\n\n会把你输入的角色名、作品名，以及图库里角色的 Danbooru tag 和作品名发给 Danbooru 和 Bangumi，用来搜索角色、作品和补全中文名。不会发送图片。\n\n允许后对这个图库一直有效，可以在「设置 › 网络与隐私」里随时撤销。';
// A Danbooru name lookup: its results, or why it failed.
const remoteNames = path => api(path).then(found => ({found}), e => ({found: null, unauthorized: e.message === NAMES_UNAUTHORIZED, reason: e.message}));
const remoteNote = r => r.unauthorized ? '查 Danbooru 需要先允许这个图库联网查询名称（只发送文字）' : `Danbooru 暂时连不上：${r.reason}`;
const namesAllowButton = '<button type="button" class="link-button" data-names-allow>允许查询…</button>';
async function setNamesNetwork(allow) {
  if (allow && !await confirmAction(NAMES_NOTE, '允许联网查询名称')) return false;
  try {
    state.names = {...state.names, ...await api('/api/names', {method: 'POST', body: JSON.stringify({action: 'network', allow})})};
  } catch (err) { toast(err.message); return false; }
  toast(allow ? '已允许这个图库联网查询名称' : '已撤销名称联网查询');
  return true;
}
// 设置 › 网络与隐私: grant or revoke, then redraw the card.
document.addEventListener('click', async e => {
  const b = e.target.closest('[data-names-network]');
  if (b && await setNamesNetwork(b.dataset.namesNetwork === '1')) renderSettings(false);
});
// The 允许查询… button under a search: once allowed, search again for what was typed.
document.addEventListener('click', async e => {
  const b = e.target.closest('[data-names-allow]');
  if (!b) return;
  e.preventDefault();
  const input = b.closest('dialog, .combo, .watch-add, #picker')?.querySelector('input');
  if (await setNamesNetwork(true)) { input?.focus(); input?.dispatchEvent(new Event('input', {bubbles: true})); }
});

// ---------- toast with undo ----------
function toast(message, withUndo = false) {
  $('toastText').textContent = message; $('toastUndo').hidden = !withUndo; $('toast').hidden = false;
  clearTimeout(toastTimer); toastTimer = setTimeout(() => { $('toast').hidden = true; }, withUndo ? 6000 : 3500);
}
$('toast').addEventListener('mouseenter', () => clearTimeout(toastTimer));
$('toast').addEventListener('mouseleave', () => { toastTimer = setTimeout(() => { $('toast').hidden = true; }, 2500); });
$('toastUndo').onclick = () => undo();
function setUndo(value) {
  state.undo = value; $('undoButton').disabled = !value || state.busy;
  $('undoButton').title = value ? `撤销：${value.label}（${shortcut("Z")}）` : '暂无可撤销操作';
}

// ---------- routing ----------
function parseHash() {
  const [path, query = ''] = location.hash.replace(/^#/, '').split('?');
  const parts = path.split('/').filter(Boolean), q = new URLSearchParams(query);
  const route = {page: 'library', q: q.get('q') || '', tag: q.get('tag') || '', utag: q.get('utag') || '', dup: q.get('dup') || '', reason: q.get('reason') || '', kind: q.get('kind') || '', maybe: q.get('maybe') || '', sort: q.get('sort') || '',
    work: q.get('work') || '', day: q.get('day') || '', artist: q.get('artist') || '', char: q.get('char') || '', variant: q.get('variant') || '', check: q.get('check') || '', doubt: q.get('doubt') || ''};
  // Opening the app lands on 首页; a bare query (#/?artist=… and other old links) still means the library.
  if (!parts.length && !query) route.page = 'home';
  // 资料库 (#/characters) and 设置 have sub-views of their own address: #/characters/works, #/settings/library.
  else if (parts[0] === 'characters' || parts[0] === 'settings') Object.assign(route, {page: parts[0], section: parts[1] || ''});
  else if (['home', 'unmatched', 'skipped', 'duplicates', 'excluded', 'pending', 'recent', 'audit', 'groups', 'similar', 'identity', 'artist-audit'].includes(parts[0])) route.page = parts[0];
  else if (parts[0] === 's' && parts[1]) Object.assign(route, {page: 'sgroup', id: decodeURIComponent(parts[1])});
  else if (parts[0] === 'g' && parts[1]) Object.assign(route, {page: 'group', id: decodeURIComponent(parts[1])});
  // Old character pages (#/c/id/skin) are now the library filtered to that character.
  else if (parts[0] === 'c' && parts[1]) Object.assign(route, {char: decodeURIComponent(parts[1]), variant: parts[2] ? decodeURIComponent(parts[2]) : '', legacy: true});
  if (route.page !== 'library') Object.assign(route, {char: '', variant: '', check: '', doubt: '', artist: ''});
  return route;
}
function hashFor(r) {
  let path = r.page === 'group' ? `#/g/${encodeURIComponent(r.id)}` : r.page === 'sgroup' ? `#/s/${encodeURIComponent(r.id)}` : '#/' + r.page + (r.section ? '/' + r.section : '');
  const q = new URLSearchParams();
  if (r.work) q.set('work', r.work); if (r.day) q.set('day', r.day); if (r.artist) q.set('artist', r.artist);
  if (r.char) q.set('char', r.char); if (r.variant) q.set('variant', r.variant); if (r.check) q.set('check', r.check); if (r.doubt) q.set('doubt', r.doubt);
  if (r.q) q.set('q', r.q); if (r.dup) q.set('dup', r.dup); if (r.reason) q.set('reason', r.reason); if (r.kind) q.set('kind', r.kind); if (r.maybe) q.set('maybe', r.maybe); if (r.tag) q.set('tag', r.tag); if (r.utag) q.set('utag', r.utag); if (r.sort && r.sort !== defaultSort(r)) q.set('sort', r.sort);
  return path + (q.toString() ? '?' + q : '');
}
const LEAD_PAGES = ['unmatched', 'skipped', 'group'];
const defaultSort = r => r.char ? prefs.get('sortCharacter', 'suspicious') : LEAD_PAGES.includes(r.page) ? prefs.get('sortUnmatched', prefs.get('sortLibrary', 'newest')) : prefs.get('sortLibrary', 'newest');
function go(route) { location.hash = hashFor(route); }
// A character is a library filter, not a page of its own.
const charRoute = (id, variant = '') => ({page: 'library', char: id, variant});
const charHash = (id, variant = '') => hashFor(charRoute(id, variant));
function replaceRoute(patch) { Object.assign(state.route, patch); history.replaceState(null, '', hashFor(state.route)); }
// Cleanup views (a character filter, or a review pool): click selects.
const cleanup = () => !!state.route.char || ['unmatched', 'skipped', 'excluded', 'pending', 'group', 'audit'].includes(state.route.page);
// Browsing pages: click opens details, selection is opt-in.
const browsing = () => !state.route.char && ['library', 'duplicates', 'recent'].includes(state.route.page);
// Pages that replace the image grid with their own panel.
const panelPage = page => page === 'home' || page === 'characters' || page === 'settings' || page === 'groups' || page === 'similar' || page === 'identity';

async function applyRoute() {
  const route = parseHash(); route.sort = route.sort || defaultSort(route);
  if (route.legacy) { delete route.legacy; history.replaceState(null, '', hashFor(route)); }
  const samePage = state.route.page === route.page && state.route.id === route.id && state.route.char === route.char;   // a sub-view switch stays the same page
  state.route = route; state.selecting = false; clearSelection(); state.cursor = -1; state.totalReady = false;
  if (!samePage) closeInspector();
  $('sidebar').classList.remove('mobile-open');
  for (const a of document.querySelectorAll('.nav')) a.classList.toggle('active', a.dataset.page === route.page || (route.page === 'skipped' && a.dataset.page === 'unmatched') || (route.page === 'pending' && a.dataset.page === 'excluded') || (['groups', 'group'].includes(route.page) && a.dataset.page === 'unmatched') || (['similar', 'sgroup'].includes(route.page) && a.dataset.page === 'duplicates'));
  $('settingsLink').classList.toggle('active', route.page === 'settings');
  state.char = null;
  state.groupNear = null;
  if (route.page === 'group') {
    let result; try { result = await api('/api/clusters?nsfw=' + state.nsfw); } catch (e) { error(readError(e)); return; }
    const g = result.clusters.find(x => String(x.cid) === route.id);
    state.groupNear = g?.near ? nearText(g) : null; state.groupNearId = g?.near || null; state.groupTraits = g?.traits || [];
  }
  if (route.char) {
    let character; try { character = await fetchChar(route.char); } catch (e) { if (state.route === route) error(readError(e)); return; }
    if (state.route !== route) return;
    state.char = character || {id: route.char, name: '角色', work: '', count: 0, unchecked: 0, variants: []};
  }
  $('homePage').hidden = route.page !== 'home';
  renderChrome();
  if (route.page === 'home') renderHome(); else if (route.page === 'characters') renderIndex(); else if (route.page === 'settings') renderSettings(true, !samePage); else if (route.page === 'groups') renderGroups(); else if (route.page === 'similar') renderSimilar(); else if (route.page === 'identity') renderIdentity(); else if (route.page === 'artist-audit') renderArtistAudit(); else await reload(true);
}
window.addEventListener('hashchange', applyRoute);

// ---------- sidebar & counts ----------
const fetchChar = async id => (await api('/api/characters?' + new URLSearchParams({id, nsfw: state.nsfw})))[0];
const navCount = (id, n, title = '') => { $(id).textContent = n ? number(n) : ''; $(id).title = title; };
let metaPending = null, metaRevision = 0, metaFull = false;
// After an edit the sidebar counts and character list (~0.6 s of server work) can wait until the edits stop:
// answering Y / N in a row then costs one refresh, not one per answer.
let metaTimer = null, metaTagsOnly = true;
function metaSoon(tagsOnly) {
  metaTagsOnly &&= tagsOnly; clearTimeout(metaTimer);
  metaTimer = setTimeout(() => { const t = metaTagsOnly; metaTagsOnly = true; refreshMeta({tagsOnly: t}).catch(e => error(readError(e))); }, 700);
}
function refreshMeta({tagsOnly = false, boot = null} = {}) {
  metaRevision++; metaFull ||= !tagsOnly;
  const bootEpoch = state.readEpoch, bootMode = state.nsfw;
  if (!metaPending) metaPending = Promise.resolve().then(async () => {
    try {
      for (;;) {
        const revision = metaRevision, full = metaFull; metaFull = false;
        try { await refreshMetaOnce(full, revision, bootEpoch === state.readEpoch && bootMode === state.nsfw ? boot : null); }
        catch (e) { if (revision === metaRevision) throw e; }
        boot = null;
        if (revision === metaRevision) return;
        // A write during an in-flight read needs a trailing fresh read, not its stale result.
        metaFull ||= full;
      }
    } finally { metaPending = null; }
  });
  return metaPending;
}
async function refreshMetaOnce(full, revision, bootSeed) {
  if (!full) {
    const tags = await api('/api/user-tags');
    if (revision === metaRevision) state.userTags = tags;
    return;
  }
  const nsfw = new URLSearchParams({nsfw: state.nsfw});
  const charId = state.char?.id;
  const directory = state.route.page === 'characters';
  const [boot, pools, chars, tags, queue, detail] = await Promise.all([bootSeed || api('/api/bootstrap?' + nsfw), api('/api/review-pools?' + nsfw), api('/api/characters?min=1&' + nsfw + (directory ? '' : '&summary=1')), api('/api/user-tags'), api('/api/identity/count?' + nsfw).catch(e => { error(readError(e)); return null; }), charId && !directory ? fetchChar(charId) : null]);
  const currentChar = charId && (detail || (directory && (chars.find(c => c.id === charId) || await fetchChar(charId))));
  if (revision !== metaRevision || nsfw.get('nsfw') !== state.nsfw) return;
  state.userTags = tags;
  state.token = boot.token; setUndo(boot.undo);
  state.pools = Object.fromEntries(pools.map(p => [p.kind, p.count]));
  state.chars = chars; state.charMap = new Map(chars.map(c => [c.id, c]));
  state.workNames = new Map(chars.filter(c => c.work && c.work_name).map(c => [c.work, c.work_name]));
  state.dupInfo = pools.find(p => p.kind === 'duplicates') || state.dupInfo;
  state.exclInfo = pools.find(p => p.kind === 'excluded') || state.exclInfo;
  state.pendingCount = state.pools.pending || 0;
  state.recentCount = state.pools.recent || 0;
  // Library counts what you can see; the 整理 section counts what still needs you.
  navCount('countLibrary', boot.total); navCount('countCharacters', chars.length);
  navCount('countRecent', state.recentCount); navCount('countHome', state.recentCount, '最近一次同步新加入的图');
  navCount('countUnmatched', state.pools.unmatched, '还没认出角色的图');
  navCount('countAudit', state.pools.audit, '随机抽查还剩几张'); $('navAudit').hidden = !state.pools.audit && state.route.page !== 'audit';
  navCount('countDuplicates', state.dupInfo.mixed, '角色不一致的副本组，对齐后才会折叠');
  navCount('countExcluded', state.pendingCount, '像截图、等你确认的图');
  if (queue) { state.identityCount = queue.total; navCount('countIdentity', queue.total, '长得像、可能是同一人或同一人另一形态的角色对'); $('navIdentity').hidden = !queue.total && state.route.page !== 'identity'; }
  if (state.char) { state.char = (state.char.id === charId && currentChar) || state.char; renderChrome(); }
  else if (state.route.page === 'settings') renderSettings();
  else if (!['characters', 'library'].includes(state.route.page)) renderChrome();
  if (state.route.page === 'characters') {
    if (!directory) { refreshMeta().catch(e => error(readError(e))); return; }
    if (state.route.section || !$('charSearch')?.value) renderIndexBody(state.chars);
  }
}

// ---------- chrome: heading, toolbar ----------
// Sub-views of one sidebar entry show as tabs under its heading instead of links tucked in a corner.
function subTabs(r) {
  const tab = (href, label, n, on, title = '') => `<a class="chip-filter${on ? ' on' : ''}" href="${href}"${title ? ` title="${esc(title)}"` : ''}>${label}${n ? ` <span>${n}</span>` : ''}</a>`;
  const p = state.pools, row = tabs => `<div class="chips-row">${tabs.join('')}</div>`;
  if (['unmatched', 'skipped', 'groups', 'group'].includes(r.page)) return row([
    tab('#/groups', '按组整理', '', ['groups', 'group'].includes(r.page), '长得像的未识别图分成一组，一组一组确认'),
    tab('#/unmatched', '全部未识别', number(p.unmatched), r.page === 'unmatched'),
    tab('#/skipped', '已跳过', p.skipped ? number(p.skipped) : '', r.page === 'skipped')]);
  if (['duplicates', 'similar', 'sgroup'].includes(r.page)) return row([
    tab('#/duplicates', '全部', number(state.dupInfo.groups) + ' 组', r.page === 'duplicates' && !r.dup),
    tab('#/duplicates?dup=mixed', '角色不一致', number(state.dupInfo.mixed) + ' 组', r.page === 'duplicates' && r.dup === 'mixed', '几份副本标的角色不同；对齐后自动折叠'),
    tab('#/similar', '相似的图', '', ['similar', 'sgroup'].includes(r.page), '不是同一个文件，但画的是同一张')]);
  if (['excluded', 'pending'].includes(r.page)) return row([
    tab('#/excluded', '全部', number(state.exclInfo.count), r.page === 'excluded' && !r.reason),
    ...Object.entries(REASONS).filter(([k]) => state.exclInfo.reasons?.[k]).map(([k, t]) => tab('#/excluded?reason=' + k, t, number(state.exclInfo.reasons[k]), r.page === 'excluded' && r.reason === k)),
    ...(state.pendingCount || r.page === 'pending' ? [tab('#/pending', '待确认', number(state.pendingCount), r.page === 'pending', '像截图但画面里有人物，规则拿不准')] : [])]);
  return '';
}
function renderChrome() {
  renderHeading();
  const r = state.route, bar = $('toolbar'), tabs = subTabs(r);
  if (r.page === 'settings') { bar.innerHTML = ''; return; }
  if (r.page === 'groups' || r.page === 'similar') { bar.innerHTML = tabs; return; }
  if (r.page === 'identity' || r.page === 'home') { bar.innerHTML = ''; return; }
  if (r.page === 'characters') {
    // 资料库: 角色 browses, 作品 manages; both search their own list.
    const works = r.section === 'works';
    const views = `<div class="segmented lib-views" role="tablist" aria-label="资料库">${[['', '角色'], ['works', '作品']].map(([v, t]) => `<a role="tab" href="#/characters${v ? '/' + v : ''}" class="${(r.section || '') === v ? 'on' : ''}" aria-selected="${(r.section || '') === v}">${t}</a>`).join('')}</div>`;
    bar.innerHTML = views + `<div class="search-wrap wide">${searchIcon()}<input type="search" id="charSearch" value="${esc(works ? shelf.q : '')}" placeholder="${works ? '搜索作品' : '搜索角色或作品'}" aria-label="${works ? '搜索作品' : '搜索角色'}"></div><div class="spacer"></div>` +
      (works ? '<button class="quiet" id="newWork">新建作品</button>' : `<div class="segmented" id="charMode">${[['shelf', '书架'], ['list', '列表']].map(([v, t]) => `<button data-mode="${v}" class="${cast.mode === v ? 'on' : ''}">${t}</button>`).join('')}</div>`);
    if (works) {
      $('charSearch').oninput = e => { shelf.q = e.target.value; drawShelf(); };
      $('newWork').onclick = createWork;
      return;
    }
    $('charSearch').oninput = () => { clearTimeout(searchTimer); searchTimer = setTimeout(searchIndex, 200); };
    for (const b of $('charMode').children) b.onclick = () => { cast.mode = b.dataset.mode; prefs.set('castMode', cast.mode); for (const x of $('charMode').children) x.classList.toggle('on', x === b); drawCast(); };
    return;
  }
  const tagPage = !!(r.char && state.char?.mascot);
  const sorts = r.char && !tagPage ? [['suspicious', '可疑优先'], ['newest', '最新加入'], ['oldest', '最早加入']] : [['newest', '最新加入'], ['oldest', '最早加入']];
  if (!r.char && LEAD_PAGES.includes(r.page)) sorts.unshift(['hinted', '有线索优先']);
  const skins = r.char && !r.maybe ? (state.char?.variants || []).filter(v => v.count) : [];
  const variants = skins.length ? `<div class="chips-row">${[{id: '', name: '全部', count: state.char.count}, ...skins].map(v => `<button class="chip-filter${(r.variant || '') === v.id ? ' on' : ''}" data-variant="${esc(v.id)}">${esc(v.name)} <span>${number(v.count)}</span></button>`).join('')}</div>` : '';
  const maybeChip = r.char && !tagPage && (state.maybeCount || r.maybe || state.doubtCount || r.doubt) ? `<div class="chips-row">${state.maybeCount || r.maybe ? `<button class="chip-filter${r.maybe ? ' on' : ''}" id="maybeChip" title="机器觉得像 TA，但算给了别人或没算：确认「是 TA」或「不是 TA」">可能也是 TA <span>${number(state.maybeCount)}</span></button>` : ''}${state.doubtCount || r.doubt ? `<button class="chip-filter${r.doubt ? ' on' : ''}" id="doubtChip" title="单人图里，另一个角色的证据几乎一样强：多半两个都不对，看一眼再决定">可能认错 <span>${number(state.doubtCount)}</span></button>` : ''}</div>` : '';
  const filtersHTML = r.page === 'library' ? `<div class="search-wrap">${searchIcon()}<input type="search" id="search" placeholder="搜索角色、别名或文件名" aria-label="搜索图库" value="${esc(r.q)}"><kbd>/</kbd></div>` +
    (r.char ? `<button class="quiet on" id="charPill" title="清除角色筛选">角色 · ${esc(short(state.char?.name || ''))} ×</button>` : '') +
    (r.utag ? `<button class="quiet on" id="utagPill" title="清除标签筛选"># ${esc(r.utag)} ×</button>` : '') +
    (r.day ? `<button class="quiet on" id="dayPill" title="清除日期筛选">${esc(dayLabel(r.day))} 加入 ×</button>` : '') +
    `<div class="tag-control"><button id="tagButton" class="quiet${r.tag ? ' on' : ''}">${r.tag ? '标签 · ' + esc(r.tag.replaceAll('_', ' ')) + ' ×' : '标签'}</button><div id="tagPopover" class="tag-popover" hidden><input id="tagSearch" type="search" placeholder="我的标签，或 white_hair 这类画面标签" aria-label="搜索标签"><div id="tagOptions"></div></div></div>` + maybeChip + variants : tabs;
  bar.innerHTML = filtersHTML +
    `<div class="spacer"></div><select id="sort" aria-label="排序">${sorts.map(([v, t]) => `<option value="${v}"${r.sort === v ? ' selected' : ''}>${t}</option>`).join('')}</select>` +
    `<button class="quiet" id="selectButton" title="${cleanup() ? '选中全部符合条件的图片' : '进入选择模式'}">${cleanup() ? `全选 <kbd>${shortcut('A')}</kbd>` : '选择'}</button>` +
    `<div class="icon-seg" id="fitToggle" role="group" aria-label="显示方式"><button data-fit="0" class="${state.fit ? '' : 'on'}" title="方格：裁成正方形（R）" aria-label="方格">${FIT_ICONS.square}</button><button data-fit="1" class="${state.fit ? 'on' : ''}" title="原始比例：完整显示整张图（R）" aria-label="原始比例">${FIT_ICONS.fit}</button></div>`;
  $('sort').onchange = e => { if (!r.char && LEAD_PAGES.includes(r.page)) prefs.set('sortUnmatched', e.target.value); replaceRoute({sort: e.target.value}); reload(true); };
  $('selectButton').onclick = () => cleanup() ? selectAll() : toggleSelecting();
  for (const b of $('fitToggle').querySelectorAll('[data-fit]')) b.onclick = () => { if (Boolean(Number(b.dataset.fit)) !== state.fit) toggleFit(); };
  for (const b of bar.querySelectorAll('[data-variant]')) b.onclick = () => go({...r, variant: b.dataset.variant});
  $('maybeChip')?.addEventListener('click', () => go({...r, maybe: r.maybe ? '' : '1', variant: '', check: '', doubt: ''}));
  $('doubtChip')?.addEventListener('click', () => go({...r, doubt: r.doubt ? '' : '1', variant: '', check: '', maybe: ''}));
  if (r.page === 'library') {
    $('search').oninput = e => { clearTimeout(searchTimer); searchTimer = setTimeout(() => { replaceRoute({q: e.target.value.trim()}); reload(true); }, 250); };
    $('tagButton').onclick = () => { if (r.tag) { replaceRoute({tag: ''}); renderChrome(); reload(true); return; } $('tagPopover').hidden = !$('tagPopover').hidden; if (!$('tagPopover').hidden) { $('tagSearch').focus(); loadTagOptions(); } };
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
const FIT_ICONS = {
  square: '<svg viewBox="0 0 20 20" aria-hidden="true"><rect x="3" y="3" width="6" height="6" rx="1.5"/><rect x="11" y="3" width="6" height="6" rx="1.5"/><rect x="3" y="11" width="6" height="6" rx="1.5"/><rect x="11" y="11" width="6" height="6" rx="1.5"/></svg>',
  fit: '<svg viewBox="0 0 20 20" aria-hidden="true"><rect x="2.5" y="3.5" width="9" height="5.5" rx="1.5"/><rect x="13" y="3.5" width="4.5" height="5.5" rx="1.5"/><rect x="2.5" y="11" width="4.5" height="5.5" rx="1.5"/><rect x="8.5" y="11" width="9" height="5.5" rx="1.5"/></svg>',
};
const searchIcon = () => '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7"><circle cx="10" cy="10" r="6.5"/><path d="m15 15 5 5"/></svg>';
// One tag menu: your own tags first, then the machine's picture tags (WD general tags).
async function loadTagOptions() {
  const q = $('tagSearch').value.trim(), low = q.toLowerCase();
  const mine = state.userTags.filter(t => t.tag !== state.route.utag && (!q || t.tag.toLowerCase().includes(low))).slice(0, 12);
  try { const tags = await api('/api/general-tags?q=' + encodeURIComponent(q)); if (q !== $('tagSearch').value.trim()) return;
    $('tagOptions').innerHTML = (mine.length ? `<div class="palette-label">我的标签</div>${mine.map(t => `<button class="picker-item" data-utag="${esc(t.tag)}"># ${esc(t.tag)} <small>${number(t.count)}</small></button>`).join('')}` : '') +
      (tags.length ? `<div class="palette-label">画面标签</div>${tags.map(t => `<button class="picker-item" data-value="${esc(t)}">${esc(t.replaceAll('_', ' '))}</button>`).join('')}` : '') || '<p>没有匹配的标签</p>';
  } catch (e) { $('tagOptions').textContent = e.message; }
}
document.addEventListener('click', e => {
  if (!e.target.closest('.tag-control') && $('tagPopover')) $('tagPopover').hidden = true;
  if (!e.target.closest('.more-control') && $('morePopover')) $('morePopover').hidden = true;
});

function renderHeading() {
  const r = state.route, h = $('heading'), c = state.char;
  if (r.char && c) {
    const tagOnly = !!c.mascot;   // 只当标签: nothing to review, so no check/sweep/OC controls
    const checked = !tagOnly && c.reviewed_at ? ` · 上次检查 ${c.reviewed_at.slice(5, 10).replace('-', ' 月 ')} 日` : '';
    const unchecked = !tagOnly && (c.unchecked || r.check) ? ` · <a class="unchecked-link${r.check ? ' on' : ''}" href="${hashFor({...r, check: r.check ? '' : '1', maybe: ''})}" title="${r.check ? '显示全部' : '只看机器识别、还没检查过的图'}">未检查 ${number(c.unchecked)}${r.check ? ' ×' : ''}</a>` : '';
    h.innerHTML = `<div class="heading-main"><h1 class="tag-title" title="${esc(nameTip(c))}">${esc(c.name)}</h1><p>${c.mascot ? `<span class="oc-badge" title="${c.mascot_origin === 'default' ? '默认设置：老师、博士、训练员这类没有固定形象的玩家化身和吉祥物，默认只当标签。' : ''}不算角色：不出现在角色列表里，也不算多人图；图上仍挂着这个标签。在「⋯」里可以改回角色">只当标签</span> ` : ''}${c.kind === 'oc' ? '<span class="oc-badge">原创</span> ' : c.work_id ? `<span title="${esc(c.work)}">${esc(c.work_name || workName(c.work))}</span> · ` : ''}${number(c.count)} 张${unchecked}${checked}${!c.tag && c.kind !== 'oc' && !c.label_only ? ' · <button class="link-button" id="profileNudge" title="关联后自动补中文名、别名和所属作品">关联 Danbooru…</button>' : ''}</p></div>
      <div class="heading-actions">${c.label_only ? '<button class="quiet" id="splitOcButton" title="把长得像的图分成一组，一组一键设为独立的原创角色">按相似细分</button>' : ''}${tagOnly ? '' : `<button class="quiet" id="sweepButton" ${c.unchecked ? '' : 'disabled'} title="把剩下的机器识别都记为已检查；以后新识别的图会重新计入未检查">标为已检查</button>`}<div class="tag-control more-control"><button class="quiet" id="moreButton" title="更多操作" aria-label="更多操作">⋯</button><div id="morePopover" class="tag-popover more-menu" hidden>${c.label_only || tagOnly ? '' : `<button class="picker-item" id="ocButton" title="画师自己的原创角色，不属于任何作品">${c.kind === 'oc' ? '取消原创角色' : '标为原创角色'}</button>`}${c.label_only ? '' : `<button class="picker-item" id="mascotButton" title="${c.mascot ? '恢复成角色：出现在角色列表里，同框就算多人图' : '像佩洛洛这样的吉祥物、玩家化身：不算角色，不算多人图；图上仍挂着这个标签，可以按它筛选'}">${c.mascot ? '算作角色' : '不算角色，只当标签'}</button>`}<button class="picker-item" id="profileButton" title="名称、别名、所属作品、Danbooru 关联">编辑资料…</button><button class="picker-item" id="mergeButton" title="和另一个角色是同一个人：把图和人工结论都并过去（可撤销）">合并到另一个角色…</button></div></div></div>`;
    $('sweepButton')?.addEventListener('click', sweep);
    $('moreButton').onclick = () => { $('morePopover').hidden = !$('morePopover').hidden; };
    $('mergeButton').onclick = () => { $('morePopover').hidden = true; mergeCharacter(c); };
    $('profileButton').onclick = () => { $('morePopover').hidden = true; editProfile(c); };
    $('profileNudge')?.addEventListener('click', () => editProfile(c));
    $('ocButton')?.addEventListener('click', () => { $('morePopover').hidden = true; toggleOc(c); });
    $('mascotButton')?.addEventListener('click', () => { $('morePopover').hidden = true; toggleMascot(c); });
    $('splitOcButton')?.addEventListener('click', () => splitOc(c));
    return;
  }
  const n = state.totalReady ? number(state.total) : '…';
  const meta = {home: ['首页', `按加入日期分组${state.recentCount ? ` · 最近一次同步新加入 ${number(state.recentCount)} 张` : ''}`], library: [r.artist ? '画师 · ' + esc(artistName(r.artist)) : r.day ? esc(dayLabel(r.day)) + ' 加入' : r.work ? esc((state.workTree || []).find(w => w.id === r.work)?.name || '作品筛选') : r.utag ? '# ' + esc(r.utag) : '图库', `${n} 张${r.work ? ' · 包含所有子作品 <a class="link" href="#/library">清除筛选</a>' : r.artist ? ' · 按画作比对找到 <a class="link" href="#/library">清除筛选</a>' : ''}`], characters: ['资料库', r.section === 'works' ? `${number(state.index?.works?.length || 0)} 部作品${state.index?.watch?.works.length ? ` · 关注 ${number(state.index.watch.works.length)} 部` : ''}` : `${number(state.chars.length)} 位角色 · ${number(new Set(state.chars.map(workKey)).size)} 部作品`],
    recent: ['新加入', `最近一次同步加入的 ${number(state.recentCount)} 张，已自动识别`],
    audit: ['随机抽查', `还剩 ${number(state.pools.audit || 0)} / ${n} 张 · 鼠标移到角色 tag 上点 ✓ / ✗，或选中多张按 Y / N；打开大图也会问「是 TA 吗？」${state.nsfw ? ' · <b>R18 筛选开着：抽查要看全部才准，按 H 切换</b>' : ''}`],
    unmatched: ['未识别', `${n} 张 · 选中后设为角色，或跳过`], skipped: ['未识别', `已跳过 ${n} 张 · 可以恢复到未识别`],
    groups: ['未识别', '长得像的图分成一组，一组一组过 · 每确认一组，系统就学会认 TA 的一种样子'],
    group: [`第 ${esc(r.id)} 组`, `${n} 张长得像的未识别图${state.groupTraits?.length ? ' · ' + state.groupTraits.map(esc).join(' · ') : ''}`],
    duplicates: ['重复', r.dup ? `角色不一致 ${number(state.dupInfo.mixed)} 组 · 对齐角色后自动折叠` : `${number(state.dupInfo.groups)} 组 · ${number(state.dupInfo.count || 0)} 张 · 修改任一份会同步到整组`],
    similar: ['重复', '画的是同一张、但不是同一个文件 · 只在本机比较'],
    'artist-audit': ['画师抽查', '验收「找回画师」：固定的一组随机图，逐张判断机器找到的画师对不对'],
    identity: ['身份整理', `${number(state.identityCount || 0)} 对长得像的角色 · 按涉及图片数排序 · 每一步都可撤销`],
    sgroup: [`相似组 ${esc(r.id)}`, `${n} 张 · 选中两张以上可以判断关系`],
    excluded: ['已排除', `${n} 张 · 只是标记，不动 Photos`], pending: ['已排除', `待确认 ${n} 张 · 像截图但有人物，排除或保留`],
    settings: ['设置', r.page === 'settings' ? settingsSection()[2] : '']}[r.page];
  const localLinks = r.page === 'characters' && !r.section && state.localLinks?.length ? `<span class="heading-links"><button class="quiet" id="localLinksButton" title="关联后自动补中文名、别名和作品；和已有角色重复的可以合并">${number(state.localLinks.length)} 个自建角色能对上 Danbooru</button></span>` : '';
  const links = localLinks ? localLinks : r.page === 'group' ? `<span class="heading-links">${state.total ? '<button class="quiet" id="groupOc" title="整组都是同一个原创角色：新建一个原创角色，把这组图都设为它">整组标为原创角色</button>' : ''}${state.groupNear ? `<a class="quiet link" href="#/g/${state.groupNearId}">${esc(state.groupNear)} →</a>` : ''}</span>`
    : r.page === 'sgroup' && state.total >= 2 && state.total <= 12 ? '<span class="heading-links"><button class="quiet" id="sgroupRelate">判断关系</button></span>' : '';
  h.innerHTML = `<div class="heading-main"><h1>${meta[0]}</h1><p>${meta[1]}</p></div>${links}`;
  $('localLinksButton')?.addEventListener('click', openLocalLinks);
  $('sgroupRelate')?.addEventListener('click', async () => openRelate(await api('/api/ids?' + new URLSearchParams({...filters(), limit: 12}))));
  $('groupOc')?.addEventListener('click', () => markOc({filter: filters(), exclude: [], confirm: state.total > 500}, false, true));
}

// ---------- asset loading ----------
function filters() {
  const r = state.route, sort = r.sort || defaultSort(r), f = {order: sort === 'suspicious' && r.char && state.char?.mascot ? 'newest' : sort};   // tag pages have no 可疑优先
  if (r.page === 'library') { if (r.day) f.day = r.day; if (r.work) f.work = r.work; if (r.artist) f.artist = r.artist; if (r.q) f.q = r.q; if (r.tag) f.tag = r.tag; if (r.utag) f.utag = r.utag; }
  if (r.char) { f.entity = r.char; if (r.maybe) f.status = 'maybe'; else if (r.doubt) f.status = 'ambiguous'; else { if (r.variant) f.variant = r.variant; if (r.check) f.status = 'machine'; } }
  if (r.page === 'unmatched' || r.page === 'skipped') f.pool = r.page;
  if (r.page === 'duplicates') { f.pool = 'duplicates'; if (r.dup) f.dup = r.dup; }
  if (r.page === 'excluded') { f.pool = 'excluded'; if (r.reason) f.reason = r.reason; }
  if (r.page === 'pending') f.pool = 'pending';
  if (r.page === 'recent') f.pool = 'recent';
  if (r.page === 'audit') f.pool = 'audit';
  if (r.page === 'group') { f.pool = 'cluster'; f.cluster = r.id; }
  if (r.page === 'sgroup') { f.pool = 'similar'; f.cluster = r.id; }
  if (state.nsfw) f.nsfw = state.nsfw;
  return f;
}
const pageUrl = (offset, count = true) => '/api/assets?' + new URLSearchParams({...filters(), offset, limit: PAGE, ...(count ? {} : {count: 0})});
function visibleOffsets() {
  const v = $('viewport'), first = rowStart(rowAt(v.scrollTop)), last = rowStart(rowAt(v.scrollTop + v.clientHeight) + 2);
  const offsets = []; for (let o = Math.floor(first / PAGE) * PAGE; o < Math.max(last, 1); o += PAGE) offsets.push(o);
  return offsets.slice(0, 4);
}
// Fetch the visible pages first, then swap data in one step: no skeleton flash after edits.
async function reload(reset = false) {
  if (panelPage(state.route.page)) return;
  const epoch = state.readEpoch, route = state.route;
  const gen = ++state.generation; abortList?.abort(); abortList = new AbortController(); state.pending.clear();
  $('charIndex').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = true; $('grid').hidden = false; error('');
  if (reset) { $('viewport').scrollTop = 0; $('resultCount').textContent = '正在读取…'; state.totalReady = false; }
  layout();
  try {
    const offsets = reset ? [0] : visibleOffsets();
    const pages = await Promise.all(offsets.map((o, i) => api(pageUrl(o, i === 0), {signal: abortList.signal})));
    if (gen !== state.generation || route !== state.route) return;
    if (epoch !== state.readEpoch) return reload(reset);
    state.total = pages[0].total; state.totalReady = true; state.countDirty = false; state.items = new Array(state.total); state.pages = new Set();
    pages.forEach((p, i) => { p.items.forEach((item, j) => { if (offsets[i] + j < state.total) state.items[offsets[i] + j] = item; }); state.pages.add(offsets[i]); });
    state.dataVersion++; state.gridKey = ''; renderGrid(); updateCount(); renderSelection();
    $('empty').hidden = !!state.total;
    const narrowed = state.route.q || state.route.tag || state.route.utag || state.route.check || state.route.doubt;
    $('empty').innerHTML = state.route.char && !narrowed ? '这个角色已经没有图片了。' : { unmatched: '全部认出来了 🎉', skipped: '没有跳过的图片。', duplicates: state.route.dup ? '所有副本的角色都一致。' : '没有重复的图片。', excluded: '没有被排除的图片。', pending: '没有待确认的图片。', group: '这一组都整理完了。', recent: '最近一次同步没有新图。', audit: '抽查做完了，谢谢！结果会用来衡量认人准不准。', library: '没有符合条件的图片。试试别的搜索或标签。'}[state.route.page] || '';
    renderHeading();
    if (state.route.char) refreshMaybe(gen);
  } catch (e) { if (e.name !== 'AbortError' && gen === state.generation && route === state.route) { error(e.message); $('resultCount').textContent = '读取失败'; } }
}
async function loadPage(offset) {
  if (state.pages.has(offset) || state.pending.has(offset) || offset >= state.total) return;
  if (state.countDirty) {
    if (!state.countReload) state.countReload = reload().finally(() => { state.countReload = null; });
    await state.countReload; return;
  }
  const gen = state.generation, epoch = state.readEpoch, route = state.route; state.pending.add(offset);
  try {
    const data = await api(pageUrl(offset, false), {signal: abortList?.signal}); if (gen !== state.generation || epoch !== state.readEpoch || route !== state.route) return;
    data.items.forEach((item, j) => { if (offset + j < state.total) state.items[offset + j] = item; });
    state.pages.add(offset); state.dataVersion++; renderGrid();
  } catch (e) { if (e.name !== 'AbortError' && gen === state.generation && route === state.route) error(e.message); }
  finally { if (gen === state.generation) state.pending.delete(offset); }
}
function updateCount() {
  const loaded = state.items.filter(Boolean).length;
  $('resultCount').textContent = `${number(state.total)} 张${loaded < state.total ? ` · 已加载 ${number(loaded)}` : ''}`;
  $('hint').textContent = state.route.char ? '点击或拖动选择 · Space 看大图 · ⌫ 不是 TA · C 换成' : {character: '点击或拖动选择 · Space 看大图 · ⌫ 不是 TA · C 换成', unmatched: '点击或拖动选择 · Space 看大图 · C 设为角色 · S 跳过',
    skipped: '点击或拖动选择 · Space 看大图', excluded: '点击或拖动选择 · Space 看大图 · X 恢复收录', pending: '点击或拖动选择 · Space 看大图 · X 排除', audit: '点一张或 Space 看大图 · Y 是 · N 不是 · 看不清可以跳过，换一张', duplicates: '同色角标为同一组 · 修改会同步到整组 · 角色不一致的组对齐后自动折叠', library: `点击查看 · 拖动或 ${isMac ? '⌘' : 'Ctrl'} 点击多选 · Space 看大图`}[state.route.page] || '';
}

// ---------- grid ----------
// Square crops, or justified rows like Photos: each row packs as many images as fit near a target
// height and stretches to the edge; the final row keeps the target height, left-aligned, so a short
// result never turns into a few giant images.
const isSquare = () => !state.fit;
// Box ratios are clamped so a panorama or long strip can't flatten or inflate its row; such images
// are shown whole inside their box (letterboxed) rather than cropped.
const FIT_MIN = .5, FIT_MAX = 2.4;
// Row geometry lives in reused typed arrays: rowStarts[r] is the first index of row r, rowTops[r] its top.
// Unloaded images use the mean ratio of loaded ones, so pages arriving move rows as little as possible.
let gridLayoutKey = '', fitRows = 0, fitEstimate = .75;
let rowStarts = new Int32Array(1), rowTops = new Float64Array(1), rowHeights = new Float64Array(0);
const rawRatio = item => item?.width > 0 && item?.height > 0 ? item.width / item.height : 0;
const imageRatio = item => { const r = rawRatio(item); return r ? Math.min(FIT_MAX, Math.max(FIT_MIN, r)) : fitEstimate; };
const rowCount = () => state.fit ? fitRows : Math.ceil(state.total / state.cols);
const rowStart = row => state.fit ? rowStarts[Math.max(0, Math.min(row, fitRows))] : Math.max(0, Math.min(row * state.cols, state.total));
function rowOf(i) {
  if (!state.fit) return Math.floor(i / state.cols);
  let lo = 0, hi = Math.max(0, fitRows - 1);
  while (lo < hi) { const mid = (lo + hi + 1) >>> 1; if (rowStarts[mid] <= i) lo = mid; else hi = mid - 1; }
  return lo;
}
function rowTop(row) { return state.fit ? rowTops[Math.max(0, Math.min(row, fitRows))] : row * state.rowH; }
function rowHeight(row) { return state.fit ? rowHeights[row] || state.rowH : state.rowH; }
function rowAt(y) {
  if (!state.fit) return Math.max(0, Math.floor(y / state.rowH));
  let lo = 0, hi = fitRows;
  while (lo < hi) { const mid = (lo + hi) >>> 1; if (rowTops[mid + 1] <= y) lo = mid + 1; else hi = mid; }
  return Math.min(lo, Math.max(0, fitRows - 1));
}
function cardRect(i) {
  const row = rowOf(i);
  if (!state.fit) return {left: (i % state.cols) * (state.cellW + state.gap), top: rowTop(row), width: state.cellW, height: state.rowH - state.gap};
  const h = rowHeight(row) - state.gap - captionH();
  let left = 0; for (let j = rowStart(row); j < i; j++) left += imageRatio(state.items[j]) * h + state.gap;
  return {left, top: rowTop(row), width: imageRatio(state.items[i]) * h, height: h + captionH()};
}
// Arrow up/down: the image in the neighbouring row whose centre is closest horizontally.
function verticalStep(i, dir) {
  if (!state.fit) return Math.max(0, Math.min(state.total - 1, i + dir * state.cols));
  const row = rowOf(i) + dir; if (row < 0 || row >= fitRows) return dir < 0 ? 0 : state.total - 1;
  const box = cardRect(i), x = box.left + box.width / 2;
  let best = rowStart(row), dist = Infinity;
  for (let j = rowStart(row); j < rowStart(row + 1); j++) { const b = cardRect(j), d = Math.abs(b.left + b.width / 2 - x); if (d < dist) { dist = d; best = j; } }
  return best;
}
const captionH = () => viewOpts.captions && browsing() && state.cellW >= 150 ? 28 : 0;
function toggleFit() {
  const v = $('viewport'), anchor = rowStart(rowAt(v.scrollTop));
  state.fit = !state.fit; prefs.set('fit', state.fit); layout();
  v.scrollTop = rowTop(rowOf(anchor)); renderGrid(true);
  for (const b of $('fitToggle')?.querySelectorAll('[data-fit]') || []) b.classList.toggle('on', Boolean(Number(b.dataset.fit)) === state.fit);
}
function packRows(width, target) {
  const n = state.total, items = state.items, gap = state.gap, extra = captionH() + gap;
  let sum = 0, known = 0;
  for (let i = 0; i < n; i++) { const r = rawRatio(items[i]); if (r) { sum += Math.min(FIT_MAX, Math.max(FIT_MIN, r)); known++; } }
  fitEstimate = known ? sum / known : .75;
  if (rowStarts.length < n + 2) { rowStarts = new Int32Array(n + 2); rowTops = new Float64Array(n + 2); rowHeights = new Float64Array(n + 1); }
  let row = 0, i = 0;
  rowTops[0] = 0; rowStarts[0] = 0;
  while (i < n) {
    const start = i; let total = 0;
    for (;;) {
      const r = imageRatio(items[i]), count = i - start + 1;
      if (count > 1 && (total + r) * target + gap * (count - 1) > width) {
        // Take the image that overflows only when that lands closer to the target height.
        const shrunk = (width - gap * (count - 1)) / (total + r), grown = (width - gap * (count - 2)) / total;
        if (target - shrunk < grown - target) { total += r; i++; }
        break;
      }
      total += r; i++;
      if (i >= n) break;
    }
    const natural = (width - gap * (i - start - 1)) / total;
    rowStarts[row] = start; rowHeights[row] = (i >= n ? Math.min(natural, target) : natural) + extra;
    rowTops[row + 1] = rowTops[row] + rowHeights[row]; row++;
  }
  rowStarts[row] = n; fitRows = row;
}
function layout() {
  const v = $('viewport'), width = Math.max(1, v.clientWidth - 2);
  state.gap = state.size < 130 ? 6 : 12;
  let cols = Math.max(1, Math.round((width + state.gap) / (state.size + state.gap)));
  const key = `${width}:${state.size}:${state.fit}:${state.total}:${state.dataVersion}:${viewOpts.captions}:${browsing()}`;
  if (key === gridLayoutKey) return;
  gridLayoutKey = key;
  for (;;) {
    state.cols = cols; state.cellW = Math.max(1, (width - state.gap * (cols - 1)) / cols);
    state.rowH = Math.round(state.cellW + captionH() + state.gap);
    // A 3:4 illustration (the common case) gets the same width as a square cell.
    if (state.fit) packRows(width, state.cellW * 4 / 3); else fitRows = 0;
    // Browsers stop scrolling past ~33M px; add columns rather than break the grid.
    if (rowTop(rowCount()) <= MAX_HEIGHT || cols > 40) break;
    cols++;
  }
}
function indexAt(clientX, clientY, clamp = true) {
  const rect = $('grid').getBoundingClientRect(), x = clientX - rect.left, y = clientY - rect.top;
  const row = rowAt(y), start = rowStart(row), end = rowStart(row + 1);
  let i = start;
  while (i < end - 1) { const box = cardRect(i); if (x < box.left + box.width + state.gap) break; i++; }
  if (!clamp && (x < 0 || y < 0 || y >= rowTop(rowCount()))) return -1;
  return clamp ? Math.max(0, Math.min(i, state.total - 1)) : i;
}
const REASONS = {photo: '真人/照片', screenshot: '截图', text: '文字图', manual: '手动排除'};
// How many images the machine thinks may also be this character (shown as a toggle on the character page).
async function refreshMaybe(gen) {
  const r = state.route, total = status => api('/api/assets/count?' + new URLSearchParams({entity: r.char, status, ...(state.nsfw ? {nsfw: state.nsfw} : {})})).then(x => x.total).catch(e => { error(readError(e)); return null; });
  const [count, doubt] = await Promise.all([r.maybe ? state.total : total('maybe'), r.doubt ? state.total : total('ambiguous')]);
  if (count === null || doubt === null || gen !== state.generation || (count === state.maybeCount && doubt === state.doubtCount)) return;
  state.maybeCount = count; state.doubtCount = doubt; renderChrome();
}
// Adjacent groups get different hues so the pairs read at a glance.
function dupHue(key) { let h = 0; for (const ch of key) h = (h * 31 + ch.charCodeAt(0)) >>> 0; return h % 12 * 30; }
// 随机抽查 cards: the image's character tags float at the bottom, the one being checked first and tinted. Hovering a
// tag shows ✓ / ✗: on the checked tag they answer the audit, on the others they confirm or remove that character.
function auditTags(item) {
  const a = item.audit, mark = {yes: '✓', no: '✗'}[a.answer] || '';
  const tag = (id, name, cls, title) => `<span class="atag ${cls}" title="${esc(title)}"><b>${mark && cls.includes('checked') ? `<i>${mark}</i>` : ''}${esc(short(name))}</b><button data-tagact="yes" data-entity="${esc(id)}" title="是 ${esc(name)}">✓</button><button data-tagact="no" data-entity="${esc(id)}" title="不是 ${esc(name)}">✗</button></span>`;
  const others = item.memberships.filter(m => m.id !== a.id).map(m => tag(m.id, m.name, m.status === 'confirmed' ? 'confirmed' : '', m.status === 'confirmed' ? '已确认' : '机器识别'));
  return `<span class="chips atags">${tag(a.id, a.name, 'checked ' + (a.answer || 'open'), a.answer ? `抽查已答：${a.answer === 'yes' ? '是' : '不是'}` : '抽查：是 TA 吗？')}${others.join('')}</span>`;
}
function cardHTML(item, i) {
  // In justified rows a wide image is wider than the nominal cell: size its thumbnail and caption by that.
  const r = state.route, ratio = state.fit ? imageRatio(item) : 1, size = state.cellW * Math.max(1, ratio) > 260 ? 'medium' : 'thumb';
  let extra = '';
  if (r.char) {
    const m = item.memberships.find(m => m.id === r.char);
    const chips = (m?.variants || []).map(v => `<span>${esc(v.name)}</span>`).join('');
    extra = (chips && state.cellW >= 120 ? `<span class="chips">${chips}</span>` : '') + (m?.status === 'machine' ? '<span class="machine-dot" title="机器识别，未检查"></span>' : '');
  }
  if (r.char && r.maybe && state.cellW >= 120) extra += `<span class="chips"><span>${item.memberships.length ? '现为 ' + esc(short(item.memberships[0].name)) : '未识别'}</span></span>`;
  if (r.page === 'excluded' && !r.reason && item.excluded && state.cellW >= 120) extra += `<span class="chips"><span>${REASONS[item.excluded] || ''}</span></span>`;
  // 随机抽查 / 未识别 are skimmed at small sizes: their one-line badge stays down to 70 px cells.
  if (r.page === 'audit' && item.audit && state.cellW >= 70) extra += auditTags(item);
  if (viewOpts.hints && !item.memberships.length && item.hint && state.cellW >= (r.page === 'unmatched' ? 70 : 120)) extra += `<span class="chips"><button class="hint-add" data-hint title="机器觉得可能是 ${esc(item.hint.name)}，点一下设为 TA（可撤销）">+ ${esc(short(item.hint.name))}</button></span>`;
  if (r.page === 'duplicates' && item.dup) extra += `<span class="dup-badge" style="--dup-hue:${dupHue(item.dup.grp)}" title="同一组共 ${item.dup.size} 份副本">${item.dup.pos}/${item.dup.size}</span>`;
  else if (item.dup && state.cellW >= 70) extra += `<span class="dup-badge folded" title="共 ${item.dup.size} 份相同副本，操作会同步到全部">×${item.dup.size}</span>`;
  // A narrow picture shows its first name and a count instead of cutting every name short; hover shows all.
  const names = item.memberships.map(m => m.name), narrow = state.fit && ratio * state.cellW * 4 / 3 < 170 && names.length > 1;
  const shown = narrow ? item.memberships.slice(0, 1) : item.memberships;
  const caption = captionH() ? `<div class="caption" title="${esc(names.join('、'))}">${shown.map(m => `<span class="${m.status}">${esc(m.name)}</span>`).join('<i>·</i>') || `<span class="none">${item.unknown ? '已跳过' : '未识别'}</span>`}${narrow ? `<small>+${names.length - 1}</small>` : ''}</div>` : '';
  const inner = `<img src="/image?id=${encodeURIComponent(item.uuid)}&size=${size}" loading="lazy" decoding="async" alt="" draggable="false">${extra}<span class="check" aria-hidden="true"></span>`;
  return `<div class="image-stage">${isSquare() ? inner : `<div class="fit-box${rawRatio(item) && rawRatio(item) !== ratio ? ' whole' : ''}">${inner}</div>`}</div>${caption}`;
}
function renderGrid(relayout = false) {
  if ($('grid').hidden) return;
  const v = $('viewport'), grid = $('grid');
  // Pin the first visible image: when rows above it repack (pages arriving, a new estimate) the view stays put.
  const anchorRow = rowAt(v.scrollTop), anchor = rowStart(anchorRow), within = v.scrollTop - rowTop(anchorRow);
  const oldLayout = gridLayoutKey;
  const before = [state.cols, state.rowH]; layout();
  if (state.fit && oldLayout !== gridLayoutKey && anchor < state.total) {
    const row = rowOf(anchor), top = rowTop(row) + Math.min(within, rowHeight(row) - 1);
    if (Math.abs(top - v.scrollTop) > .5) v.scrollTop = top;
  }
  if (relayout || before[0] !== state.cols || before[1] !== state.rowH) state.gridKey = '';
  const rows = rowCount(); grid.style.height = `${rowTop(rows)}px`;
  const first = Math.max(0, rowAt(v.scrollTop) - (state.fit ? 1 : 2)), last = Math.min(rows, state.fit ? rowAt(v.scrollTop + v.clientHeight) + 2 : Math.ceil((v.scrollTop + v.clientHeight) / state.rowH) + 2);
  const key = `${first}:${last}:${state.cols}:${state.rowH}:${state.dataVersion}:${state.fit}:${gridLayoutKey}`;
  if (key !== state.gridKey) {
    state.gridKey = key;
    const existing = new Map([...grid.children].map(el => [Number(el.dataset.index), el]));
    const keep = new Set();
    const layoutKey = `${state.cols}:${state.cellW}:${state.rowH}:${state.gap}:${state.fit}:${state.route.page}:${state.route.char}:${state.route.maybe}:${state.route.reason}`;
    for (let i = rowStart(first); i < rowStart(last); i++) {
      const item = state.items[i];
      let el = existing.get(i);
      if (el && el._item === item && el._layoutKey === layoutKey) {
        const box = cardRect(i);
        for (const [k, value] of Object.entries(box)) if (el.style[k] !== `${value}px`) el.style[k] = `${value}px`;
        keep.add(el); continue;
      }
      const previous = el; el = document.createElement('div');
      el._item = item; el._layoutKey = layoutKey;
      if (previous) previous.replaceWith(el);
      else grid.insertBefore(el, [...grid.children].find(child => Number(child.dataset.index) > i) || null);
      keep.add(el);
      const box = cardRect(i); Object.assign(el.style, Object.fromEntries(Object.entries(box).map(([k, value]) => [k, `${value}px`])));
      el.dataset.index = i;
      if (!item) { el.className = 'card skeleton'; continue; }
      el.className = 'card' + (isSquare() ? ' square' : ' fit'); el.dataset.id = item.uuid; el.setAttribute('role', 'button');
      el.setAttribute('aria-label', (item.memberships.map(m => m.name).join('、') || '未识别图片') + `，第 ${i + 1} 张`);
      el.innerHTML = cardHTML(item, i);
      el.querySelector('img').addEventListener('error', ev => { const msg = document.createElement('span'); msg.className = 'missing'; msg.textContent = '图片暂不可读'; ev.target.replaceWith(msg); }, {once: true});
    }
    for (const el of [...grid.children]) if (!keep.has(el)) el.remove();
  }
  syncCards();
  clearTimeout(pageTimer); const gen = state.generation;
  // Visible pages first, then about a screen above and below, so dimensions usually arrive before
  // their rows scroll into view and justified rows don't reshuffle on screen.
  const ahead = rowStart(Math.min(rows, rowAt(v.scrollTop + 2 * v.clientHeight) + 1)), behind = rowStart(rowAt(v.scrollTop - v.clientHeight));
  pageTimer = setTimeout(() => {
    if (gen !== state.generation) return;
    for (let o = Math.floor(rowStart(first) / PAGE) * PAGE; o < rowStart(last); o += PAGE) loadPage(o);
    for (let o = Math.floor(behind / PAGE) * PAGE; o < ahead; o += PAGE) loadPage(o);
  }, 60);
}
function syncCards() {
  $('grid').classList.toggle('selecting', state.selecting || selCount() > 0);
  for (const el of $('grid').children) {
    if (!el.dataset.id) continue;
    const on = isSelected(el.dataset.id);
    el.classList.toggle('selected', on); el.setAttribute('aria-pressed', on);
    el.classList.toggle('focused', el.dataset.id === state.focused);
    el.classList.toggle('cursor', Number(el.dataset.index) === state.cursor);
  }
}
let gridFrame = null;
function scheduleGrid() {
  if (gridFrame !== null) return;
  gridFrame = requestAnimationFrame(() => { gridFrame = null; renderGrid(); });
}
$('viewport').addEventListener('scroll', scheduleGrid, {passive: true});
new ResizeObserver(scheduleGrid).observe($('viewport'));

// ---------- zoom ----------
// Keep the image under the pointer (or the first visible row) in place, then let cards glide
// from their old rectangles to the new ones (FLIP), so each column step reads as one motion.
let zoomFrame = 0, zoomTask = null;
function requestZoom(task) { zoomTask = task; if (!zoomFrame) zoomFrame = requestAnimationFrame(() => { zoomFrame = 0; const t = zoomTask; zoomTask = null; t?.(); }); }
function setSize(size, clientX, clientY, fromSlider = false) {
  size = Math.max(SIZE_MIN, Math.min(SIZE_MAX, size));
  const v = $('viewport'), vr = v.getBoundingClientRect(), gr = $('grid').getBoundingClientRect();
  const x = clientX ?? gr.left + 2, y = clientY ?? vr.top + 2;
  const anchor = state.total ? indexAt(x, y) : 0;
  const anchorRow = rowOf(anchor), within = (y - gr.top - rowTop(anchorRow)) / rowHeight(anchorRow), offset = y - vr.top, oldScroll = v.scrollTop;
  const before = new Map();
  for (const el of $('grid').children) if (el.dataset.id) before.set(el.dataset.id, [parseFloat(el.style.left), parseFloat(el.style.top) - oldScroll, parseFloat(el.style.width), parseFloat(el.style.height)]);
  const old = [state.cols, state.rowH];
  state.size = size; prefs.set('size', Math.round(size));
  if (!fromSlider && $('zoom')) $('zoom').value = sizeToSlider(size);
  layout();
  if (old[0] === state.cols && old[1] === state.rowH) return;
  v.scrollTop = oldScroll === 0 && clientY === undefined ? 0 : Math.max(0, rowTop(rowOf(anchor)) + within * rowHeight(rowOf(anchor)) - offset);
  renderGrid(true);
  if (matchMedia('(prefers-reduced-motion: reduce)').matches) return;
  const scroll = v.scrollTop;
  for (const el of $('grid').children) {
    const from = before.get(el.dataset.id);
    if (!from) { el.animate([{opacity: 0}, {opacity: 1}], {duration: 200, easing: 'ease-out'}); continue; }
    const left = parseFloat(el.style.left), top = parseFloat(el.style.top) - scroll, w = parseFloat(el.style.width), h = parseFloat(el.style.height);
    el.animate([{transformOrigin: '0 0', transform: `translate(${from[0] - left}px,${from[1] - top}px) scale(${from[2] / w},${from[3] / h})`}, {transformOrigin: '0 0', transform: 'none'}],
      {duration: 240, easing: 'cubic-bezier(.2,.7,.2,1)'});
  }
}
// Column steps for keyboard: one level at a time.
function stepColumns(delta) {
  const width = $('viewport').clientWidth - 2, cols = Math.max(1, state.cols + delta);
  setSize((width + state.gap) / cols - state.gap);
}
let pinchFactor = 1, pinchPoint = null, gestureStart = 0;
$('viewport').addEventListener('wheel', e => {
  if (!e.ctrlKey && !e.metaKey) return;
  e.preventDefault(); pinchFactor *= Math.exp(-e.deltaY * .01); pinchPoint = [e.clientX, e.clientY];
  requestZoom(() => { const f = pinchFactor; pinchFactor = 1; setSize(state.size * f, ...pinchPoint); });
}, {passive: false});
$('viewport').addEventListener('gesturestart', e => { e.preventDefault(); gestureStart = state.size; });
$('viewport').addEventListener('gesturechange', e => { e.preventDefault(); const x = e.clientX, y = e.clientY, s = gestureStart * e.scale; requestZoom(() => setSize(s, x, y)); });

// ---------- selection ----------
const selCount = () => state.sel.all ? Math.max(0, state.total - state.sel.exclude.size) : state.sel.ids.size;
const isSelected = id => state.sel.all ? !state.sel.exclude.has(id) : state.sel.ids.has(id);
function setSelected(id, on) {
  if (state.sel.all) on ? state.sel.exclude.delete(id) : state.sel.exclude.add(id);
  else on ? state.sel.ids.add(id) : state.sel.ids.delete(id);
}
function clearSelection() { state.sel = {all: false, ids: new Set(), exclude: new Set()}; state.anchor = -1; state.armed = null; renderSelection(); }
function selectAll() { if (!state.total) return; state.sel = {all: true, ids: new Set(), exclude: new Set()}; state.selecting = true; renderSelection(); }
function toggleSelecting() { state.selecting = !state.selecting; if (!state.selecting) clearSelection(); renderSelection(); }
function selectionPayload() { return state.sel.all ? {filter: filters(), exclude: [...state.sel.exclude]} : {ids: [...state.sel.ids]}; }
async function selectRange(a, b, on = true) {
  const lo = Math.min(a, b), hi = Math.max(a, b);
  if (hi - lo + 1 > 5000) { toast(`一次最多连选 5,000 张；需要更多请用 ${shortcut('A')}`); return; }
  let ids = state.items.slice(lo, hi + 1);
  if (ids.length !== hi - lo + 1 || ids.some(x => !x)) {
    ids = await api('/api/ids?' + new URLSearchParams({...filters(), offset: lo, limit: hi - lo + 1}));
  } else ids = ids.map(x => x.uuid);
  for (const id of ids) setSelected(id, on);
  renderSelection();
}
function handleCardClick(index, e, onCheck = false) {
  const item = state.items[index]; if (!item) return;
  state.cursor = index;
  if (e.shiftKey && state.anchor >= 0) { selectRange(state.anchor, index, true); return; }
  // The corner circle always selects, like Photos; a plain click in the library opens details.
  if (browsing() && !onCheck && !state.selecting && !selCount() && !e.metaKey && !e.ctrlKey) { inspect(item.uuid); syncCards(); return; }
  if (browsing() && (onCheck || e.metaKey || e.ctrlKey)) state.selecting = true;
  setSelected(item.uuid, !isSelected(item.uuid)); state.anchor = index; renderSelection();
}

// Pointer: click, drag-select across cards (like Photos), marquee from empty space, autoscroll at edges.
let drag = null;
// One click on a card's「+ 角色」lead: set the image to that character, same as choosing it in the picker.
$('grid').addEventListener('click', e => {
  const t = e.target.closest('[data-tagact]');
  if (t) {
    const item = state.items[Number(t.closest('.card').dataset.index)], yes = t.dataset.tagact === 'yes', entity = t.dataset.entity;
    if (!item) return;
    if (item.audit && entity === item.audit.id) mutate('/api/audit/answer', {ids: [item.uuid], ...(yes ? {yes: [item.uuid]} : {no: [item.uuid]})}, false);
    else mutate('/api/change', {ids: [item.uuid], action: yes ? 'confirmed' : 'rejected', entity}, false);
    return;
  }
  const b = e.target.closest('[data-hint]'); if (!b) return;
  const item = state.items[Number(b.closest('.card').dataset.index)];
  if (item?.hint) mutate('/api/change', {ids: [item.uuid], action: 'confirmed', entity: item.hint.id}, LEAD_PAGES.includes(state.route.page));
});
$('grid').addEventListener('pointerdown', e => {
  if (e.button !== 0 || e.pointerType === 'touch' || e.target.closest('[data-hint],[data-tagact]')) return;
  const card = e.target.closest('.card[data-id]');
  drag = {x: e.clientX, y: e.clientY, start: card ? Number(card.dataset.index) : null, check: !!e.target.closest('.check'), moved: false, id: e.pointerId, base: null, lastY: e.clientY, lastX: e.clientX};
});
window.addEventListener('pointermove', e => {
  if (!drag || e.pointerId !== drag.id) return;
  drag.lastX = e.clientX; drag.lastY = e.clientY;
  if (!drag.moved && Math.hypot(e.clientX - drag.x, e.clientY - drag.y) < 6) return;
  if (!drag.moved) {
    drag.moved = true; state.selecting = state.selecting || browsing();
    drag.base = {all: state.sel.all, ids: new Set(state.sel.ids), exclude: new Set(state.sel.exclude)};
    const startItem = drag.start !== null ? state.items[drag.start] : null;
    drag.mode = startItem && isSelected(startItem.uuid) ? false : true;
    const gr = $('grid').getBoundingClientRect(); drag.gx = drag.x - gr.left; drag.gy = drag.y - gr.top;
    document.body.classList.add('dragging'); autoScroll();
  }
  applyDrag();
});
window.addEventListener('pointerup', e => {
  if (!drag || e.pointerId !== drag.id) return;
  const d = drag; drag = null; document.body.classList.remove('dragging'); $('marquee').hidden = true;
  if (!d.moved && d.start !== null) handleCardClick(d.start, e, d.check);
  else if (d.moved && d.start !== null) state.anchor = indexAt(e.clientX, e.clientY);
});
function applyDrag() {
  if (!drag?.moved) return;
  state.sel = {all: drag.base.all, ids: new Set(drag.base.ids), exclude: new Set(drag.base.exclude)};
  let indices = [];
  if (drag.start !== null) {
    const now = indexAt(drag.lastX, drag.lastY), lo = Math.min(drag.start, now), hi = Math.max(drag.start, now);
    for (let i = lo; i <= hi; i++) indices.push(i);
  } else {
    const gr = $('grid').getBoundingClientRect(), x2 = drag.lastX - gr.left, y2 = drag.lastY - gr.top;
    const [l, r, t, b] = [Math.min(drag.gx, x2), Math.max(drag.gx, x2), Math.min(drag.gy, y2), Math.max(drag.gy, y2)];
    Object.assign($('marquee').style, {left: `${l}px`, top: `${t}px`, width: `${r - l}px`, height: `${b - t}px`}); $('marquee').hidden = false;
    const r0 = rowAt(t), r1 = rowAt(b);
    for (let row = r0; row <= r1; row++) for (let i = rowStart(row); i < rowStart(row + 1); i++) {
      const box = cardRect(i);
      if (box.left <= r && box.left + box.width >= l && box.top <= b && box.top + box.height >= t) indices.push(i);
    }
  }
  for (const i of indices) if (state.items[i]) setSelected(state.items[i].uuid, drag.mode);
  renderSelection();
}
function autoScroll() {
  if (!drag?.moved) return;
  const vr = $('viewport').getBoundingClientRect(), edge = 48;
  const speed = drag.lastY < vr.top + edge ? -(vr.top + edge - drag.lastY) : drag.lastY > vr.bottom - edge ? drag.lastY - (vr.bottom - edge) : 0;
  if (speed) { $('viewport').scrollTop += speed * .6; applyDrag(); }
  requestAnimationFrame(autoScroll);
}
$('grid').addEventListener('dblclick', e => { const card = e.target.closest('.card[data-id]'); if (card) openViewer(Number(card.dataset.index)); });

// ---------- selection bar & actions ----------
function renderSelection() {
  syncCards();
  const n = selCount(), bar = $('selectionBar'), r = state.route;
  bar.hidden = !n; if (!n) { state.armed = null; return; }
  const big = n > 500, arm = key => state.armed === key ? ` armed` : '';
  const count = `<button class="icon" id="clearSel" title="取消选择（Esc）">×</button><span class="sel-count">${state.sel.all ? '已选全部 ' : '已选 '}${number(n)} 张</span><div class="bar-divider"></div>`;
  let buttons = '';
  if (r.char && r.maybe) {
    const name = short(state.char?.name || '该角色');
    buttons = `<button class="primary${arm('isme')}" data-act="isme">${state.armed === 'isme' ? `再点一次：${number(n)} 张设为 ${esc(name)}` : `是 ${esc(name)}`}</button><button class="quiet${arm('remove')}" data-act="remove">${state.armed === 'remove' ? `再点一次：${number(n)} 张不是` : '不是 TA'} <kbd>⌫</kbd></button>`;
  } else if (r.char) {
    const name = state.char?.name || '该角色';
    buttons = `<button class="primary${arm('remove')}" data-act="remove">${state.armed === 'remove' ? `再点一次：移出 ${number(n)} 张` : `不是 ${esc(short(name))}`} <kbd>⌫</kbd></button><button class="quiet" data-act="swap">换成… <kbd>C</kbd></button><button class="quiet" data-act="also">也是… <kbd>A</kbd></button>${state.char?.variants?.length ? '<button class="quiet" data-act="variant">子标签 <kbd>T</kbd></button>' : ''}`;
  } else if (r.page === 'audit') {
    // 随机抽查: each image asks about its own character (the dark badge); answer a whole selection at once.
    buttons = `<button class="primary" data-act="audit_yes" title="选中的图都是角标上的角色">都是 <kbd>Y</kbd></button><button class="quiet" data-act="audit_no" title="选中的图都不是角标上的角色">都不是 <kbd>N</kbd></button>`;
  } else if (r.page === 'unmatched' || r.page === 'group') {
    buttons = `<button class="primary" data-act="assign">设为角色… <kbd>C</kbd></button><button class="quiet" data-act="oc" title="画师自己的原创角色：统一标为「原创角色（未细分）」，以后需要时再按相似细分">原创角色</button><button class="quiet${arm('skip')}" data-act="skip">${state.armed === 'skip' ? `再点一次：跳过 ${number(n)} 张` : '跳过'} <kbd>S</kbd></button>`;
  } else if (r.page === 'excluded') {
    buttons = `<button class="primary${arm('include')}" data-act="include">${state.armed === 'include' ? `再点一次：恢复 ${number(n)} 张` : '恢复收录'} <kbd>X</kbd></button>`;
  } else if (r.page === 'pending') {
    buttons = `<button class="primary${arm('exclude')}" data-act="exclude">${state.armed === 'exclude' ? `再点一次：排除 ${number(n)} 张` : '排除'} <kbd>X</kbd></button><button class="quiet${arm('include')}" data-act="include">${state.armed === 'include' ? `再点一次：保留 ${number(n)} 张` : '保留'}</button>`;
  } else if (r.page === 'skipped') {
    buttons = `<button class="primary${arm('resume')}" data-act="resume">${state.armed === 'resume' ? `再点一次：恢复 ${number(n)} 张` : '恢复到未识别'}</button><button class="quiet" data-act="assign">设为角色… <kbd>C</kbd></button><button class="quiet" data-act="oc" title="画师自己的原创角色：统一标为「原创角色（未细分）」，以后需要时再按相似细分">原创角色</button>`;
  } else {
    buttons = `<button class="primary" data-act="assign">加角色… <kbd>C</kbd></button><button class="quiet" data-act="notlib" ${state.sel.all ? 'disabled title="全选时请进入角色页移出"' : ''}>不是… <kbd>⌫</kbd></button>`;
  }
  if (!['excluded', 'pending'].includes(r.page)) buttons += `<button class="quiet${arm('exclude')}" data-act="exclude" title="不是插画（截图、照片等）：从图库移到「已排除」">${state.armed === 'exclude' ? `再点一次：排除 ${number(n)} 张` : '排除'} <kbd>X</kbd></button>`;
  buttons += `<button class="quiet" data-act="tag">标签 <kbd>G</kbd></button>`;
  buttons += '<button class="quiet" data-act="findartist" title="用像素哈希和 IQDB 查这些图的画师（只发哈希文本，不发图片；一次最多 500 张）">查找画师</button>';
  if (!state.sel.all && n >= 2 && n <= 12) buttons += '<button class="quiet" data-act="relate" title="两两比较：同一张图、裁剪、差分、局部编辑，还是不同的图（只在本机计算）">判断关系</button>';
  bar.innerHTML = count + buttons + (big ? '<span class="bar-note">超过 500 张需再点一次确认</span>' : '');
  $('clearSel').onclick = () => { state.selecting = false; clearSelection(); };
  for (const b of bar.querySelectorAll('[data-act]')) b.onclick = () => runBarAction(b.dataset.act);
}
const short = name => name.length > 28 ? name.slice(0, 26) + '…' : name;
function needsArm(key) {
  if (selCount() <= 500 || state.armed === key) return false;
  state.armed = key; renderSelection(); setTimeout(() => { if (state.armed === key) { state.armed = null; renderSelection(); } }, 4000); return true;
}
async function answerAudit(yes) {
  const sel = state.sel;
  const ids = sel.all ? (await api('/api/ids?' + new URLSearchParams({...filters(), limit: 500}))).filter(u => !sel.exclude.has(u)) : [...sel.ids];
  if (ids.length) mutate('/api/audit/answer', {ids, ...(yes ? {yes: ids} : {no: ids})}, false, {advance: true});
}
function runBarAction(act) {
  const n = selCount(), r = state.route; if (!n || state.busy) return;
  const payload = () => ({...selectionPayload(), confirm: n > 500});
  const fromName = state.char?.name;
  if (act === 'audit_yes' || act === 'audit_no') { answerAudit(act === 'audit_yes'); return; }
  if (act === 'findartist') {
    const sel = state.sel;
    (sel.all ? api('/api/ids?' + new URLSearchParams({...filters(), limit: 500})).then(ids => ids.filter(u => !sel.exclude.has(u))) : Promise.resolve([...sel.ids].slice(0, 500)))
      .then(ids => ids.length && findArtists(ids, () => reload())).catch(err => toast(err.message));
    return;
  }
  if (act === 'remove') { if (!needsArm('remove')) mutate('/api/change', {...payload(), action: 'rejected', entity: r.char}, true); }
  else if (act === 'skip') { if (!needsArm('skip')) mutate('/api/change', {...payload(), action: 'unknown'}, true); }
  else if (act === 'isme') { if (!needsArm('isme')) mutate('/api/change', {...payload(), action: 'confirmed', entity: r.char}, true); }
  else if (act === 'exclude' || act === 'include') { if (!needsArm(act)) mutate('/api/curate', {...payload(), action: act}, true); }
  else if (act === 'resume') { if (!needsArm('resume')) mutate('/api/change', {...payload(), action: 'resume'}, true); }
  else if (act === 'swap') openPicker({title: `把 ${number(n)} 张换成…`, sentence: e => `把 ${number(n)} 张从 ${fromName} 换成 ${e.name}`, exclude: [r.char], suggest: selectionPayload(), multi: true,
    pick: list => mutate('/api/reassign', {...payload(), old: r.char, new: newIds(list)}, true)});
  else if (act === 'also') openPicker({title: `这 ${number(n)} 张也是…`, sentence: e => `给 ${number(n)} 张加上 ${e.name}`, exclude: [r.char], suggest: selectionPayload(), multi: true,
    pick: list => mutate('/api/change', {...payload(), action: 'confirmed', ...targets(list)}, false)});
  else if (act === 'assign') openPicker({title: browsing() ? `给 ${number(n)} 张加角色` : `把 ${number(n)} 张设为…`, sentence: e => `把 ${number(n)} 张设为 ${e.name}`, suggest: selectionPayload(), multi: true,
    pick: list => mutate('/api/change', {...payload(), action: 'confirmed', ...targets(list)}, !browsing())});
  else if (act === 'oc') markOc(payload(), true);
  else if (act === 'relate') openRelate([...state.sel.ids]);
  else if (act === 'variant') openVariantMenu(payload);
  else if (act === 'tag') openTagPicker(payload, n, state.sel.all ? null : state.items.filter(x => x && isSelected(x.uuid)));
  else if (act === 'notlib') {
    const present = new Map(); for (const id of state.sel.ids) { const item = state.items.find(x => x?.uuid === id); for (const m of item?.memberships || []) present.set(m.id, m); }
    if (!present.size) { toast('所选图片还没有角色'); return; }
    openPicker({title: '这些图不是…', items: [...present.values()], sentence: e => `把 ${e.name} 从 ${number(n)} 张中移出`, pick: e => mutate('/api/change', {...payload(), action: 'rejected', entity: e.id}, false)});
  }
}
// Human tags reuse the palette: existing tags, adopt a WD tag, or create one. Items already on
// every selected image are offered for removal.
function openTagPicker(payload, n, loaded, leaving = () => !!state.route.utag) {
  const present = new Map();
  for (const item of loaded || []) for (const t of item.user_tags || []) present.set(t, (present.get(t) || 0) + 1);
  openPicker({title: '标签', tagMode: true, present, placeholder: '输入标签，或搜索 WD 通用标签…',
    sentence: e => e.remove ? `从 ${number(n)} 张去掉标签 ${e.tag}` : `给 ${number(n)} 张加上标签 ${e.tag}`,
    pick: e => mutate('/api/tag', {...payload(), tag: e.tag, add: !e.remove}, e.remove && leaving())});
}
function openVariantMenu(payload) {
  const r = state.route, n = selCount(), items = [];
  if (r.variant) { const v = state.char.variants.find(v => v.id === r.variant); if (v) items.push({id: v.id, name: v.name, remove: true, hint: '从所选图片去掉'}); }
  for (const v of state.char.variants) if (v.id !== r.variant) items.push({id: v.id, name: v.name, hint: `${number(v.count)} 张`});
  openPicker({title: '子标签', items, placeholder: '筛选子标签', sentence: e => e.remove ? `从 ${number(n)} 张去掉 ${e.name}` : `给 ${number(n)} 张加上 ${e.name}`,
    pick: e => mutate('/api/change', {...payload(), action: e.remove ? 'remove_variant' : 'add_variant', entity: e.id}, !!e.remove)});
}

// Writes: fade leaving cards at once, close the gaps as soon as the server agrees,
// then swap in fresh data without losing the scroll position.
// Editing the image open in the viewer or the detail panel never moves you to another one: the list may reorder
// (suspicious-first puts confirmed images last) or drop it, but it stays on screen so several edits can follow,
// and ←/→ then continue from the neighbours it had before the first edit. Only 随机抽查 answers advance.
let hold = null;   // {uuid, item, prev, next}
function holdCurrent() {
  const uuid = state.viewer >= 0 ? state.viewerItem?.uuid : state.focused;
  if (!uuid) { hold = null; return; }
  if (hold?.uuid === uuid) return;
  const i = indexOf(uuid);
  hold = {uuid, item: state.items[i] || state.viewerItem, prev: state.items[i - 1]?.uuid, next: state.items[i + 1]?.uuid};
}
function currentItem() {
  const uuid = state.viewer >= 0 ? state.viewerItem?.uuid : state.focused;
  if (!uuid) return state.cursor >= 0 ? state.items[state.cursor] : null;
  const i = indexOf(uuid);
  return i >= 0 ? state.items[i] : uuid === hold?.uuid ? hold.item : state.viewerItem?.uuid === uuid ? state.viewerItem : null;
}
async function mutate(path, body, leaving, {advance = false} = {}) {
  if (state.busy) return; state.busy = true; document.body.classList.add('busy');
  const started = performance.now();
  const advanceFrom = advance && state.viewer >= 0 ? state.viewerItem?.uuid : null;
  if (advance) hold = null; else holdCurrent();
  const gone = leaving ? new Set(body.ids || state.items.filter(x => x && isSelected(x.uuid)).map(x => x.uuid)) : null;
  if (gone) for (const el of $('grid').children) if (gone.has(el.dataset.id)) el.classList.add('leaving');
  try {
    const r = await post(path, body); setUndo(r.undo);
    state.selecting = false; clearSelection();
    toast(r.undo.label, true);
    if (gone) { await new Promise(done => setTimeout(done, Math.max(0, 170 - (performance.now() - started)))); collapse(gone); }
    await reload(); if (state.route.page === 'home') renderHome(true); metaSoon(path === '/api/tag');
    if (state.focused) inspect(state.focused, true);
    if (state.viewer >= 0) {
      if (advance && advanceFrom && indexOf(advanceFrom) >= 0) showViewer(Math.min(indexOf(advanceFrom) + 1, state.total - 1));
      else if (advance || !hold) showViewer(Math.min(state.viewer, state.total - 1));
      else { const i = indexOf(hold.uuid); showViewer(i >= 0 ? i : state.viewer, i >= 0 ? state.items[i] : state.viewerItem); }
    }
    return true;
  } catch (e) { toast(e.message); for (const el of $('grid').querySelectorAll('.leaving')) el.classList.remove('leaving'); }
  finally { state.busy = false; document.body.classList.remove('busy'); setUndo(state.undo); }
}
function collapse(gone) {
  const kept = []; let removed = 0;
  for (let i = 0; i < state.total; i++) { const x = state.items[i]; if (x && gone.has(x.uuid)) { removed++; continue; } kept[i - removed] = x; }
  kept.length = state.total - removed; state.items = kept; state.total = kept.length; state.dataVersion++; renderGrid(true); updateCount();
}
async function undo() {
  if (!state.undo || state.busy) return; state.busy = true;
  try {
    const label = state.undo.label, r = await post('/api/undo', {id: state.undo.id}); setUndo(r.undo);
    toast('已撤销 · ' + label.replace(/^已/, '')); await reload(); refreshMeta().catch(e => error(readError(e)));
    if (state.route.page === 'identity') renderIdentity();
    if (state.route.page === 'groups') renderGroups(true);
    if (state.route.page === 'artist-audit') renderArtistAudit();
    if (state.focused) inspect(state.focused, true);
    if (state.viewer >= 0) { const i = indexOf(state.viewerItem?.uuid); showViewer(i >= 0 ? i : state.viewer, i >= 0 ? state.items[i] : state.viewerItem); }
  } catch (e) { toast(e.message); } finally { state.busy = false; setUndo(state.undo); }
}
async function sweep() {
  const c = state.char; if (!c?.unchecked) return;
  await mutate('/api/sweep', {entity: c.id}, false);
}

// ---------- command palette (换成 / 也是 / 设为角色 / 跳到角色) ----------
let palette = null;
// Multi pickers (设为角色 / 也是): ⇧+click, ⇧+↵ or a row's ＋ collects characters; ↵ applies all of them at once.
const targets = list => list.length > 1 ? {entities: list.map(e => e.id)} : {entity: list[0].id};
// 换成 accepts several characters; a single one is sent as before.
const newIds = list => list.length > 1 ? list.map(e => e.id) : list[0].id;
const stageKey = e => e.id || (e.remote ? 'tag:' + e.tag : 'new:' + (e.oc ? 'oc:' : '') + e.create);
async function openPicker(opts) {
  palette = {...opts, list: [], index: 0, generation: 0, staged: []};
  renderStaged();
  $('pickerSearch').value = ''; $('pickerSearch').placeholder = opts.placeholder || '输入角色 tag 或别名…';
  $('picker').showModal(); $('pickerSearch').focus();
  await paletteSearch();
}
async function paletteSearch() {
  const p = palette; if (!p) return;
  const gen = ++p.generation, q = $('pickerSearch').value.trim();
  let sections = [];
  if (p.tagMode) {
    const low = q.toLowerCase(), mine = state.userTags.filter(t => !q || t.tag.toLowerCase().includes(low));
    if (p.present.size && !q) sections.push({label: '去掉已有标签', items: [...p.present.keys()].map(t => ({tag: t, name: t, remove: true, hint: `所选图片中 ${number(p.present.get(t))} 张有`}))});
    const recent = prefs.get('tagPicks', []).filter(t => !q || t.toLowerCase().includes(low));
    if (recent.length) sections.push({label: '最近使用', items: recent.map(t => ({tag: t, name: t, hint: '我的标签'}))});
    const rest = mine.filter(t => !recent.includes(t.tag));
    if (rest.length) sections.push({label: '我的标签', items: rest.map(t => ({tag: t.tag, name: t.tag, hint: `${number(t.count)} 张`}))});
    if (q) {
      let wd; try { wd = await api('/api/general-tags?q=' + encodeURIComponent(q)); } catch (e) { if (gen === p.generation && palette === p) renderPalette([{label: '本地标签', items: [], note: readError(e)}], q); return; }
      if (gen !== p.generation) return;
      const known = new Set(state.userTags.map(t => t.tag));
      if (wd.length) sections.push({label: '采纳 WD 通用标签', items: wd.filter(t => !known.has(t)).slice(0, 8).map(t => ({tag: t, name: t, hint: '机器词表，采纳后成为你的标签'}))});
      if (!known.has(q) && !wd.includes(q)) sections.push({label: '', items: [{tag: q, name: q, create: q, hint: '新建标签'}]});
    }
  } else if (p.items) sections = [{label: '', items: p.items.filter(e => !q || e.name.toLowerCase().includes(q.toLowerCase()))}];
  else if (q) {
    let list; try { list = await api('/api/entities?q=' + encodeURIComponent(q)); } catch (e) { if (gen === p.generation && palette === p) renderPalette([{label: '本地角色', items: [], note: readError(e)}], q); return; }
    if (gen !== p.generation) return;
    const items = list.filter(e => !(p.exclude || []).includes(e.id)).map(e => ({...e, hint: e.matched ? '别名：' + e.matched : charHint(e), count: e.count}));
    sections = [{label: '', items}];
    if (!p.jump) {
      // Not in the library yet: Danbooru characters (typed text only) come in a moment; local-only creation stays last.
      const create = list.some(e => e.name.toLowerCase() === q.toLowerCase()) ? [] : [{create: q, name: q, hint: '只在本地新建，不关联 Danbooru'}, {create: q, oc: true, name: q, hint: '画师的原创角色，不属于任何作品'}];
      const remote = {label: 'Danbooru 角色 · 选中即新建并关联', items: [], note: '正在查 Danbooru…（只发送你输入的文字）'};
      sections.push(remote, {label: '', items: create});
      clearTimeout(p.remoteTimer);
      p.remoteTimer = setTimeout(async () => {
        const result = await remoteNames('/api/entities?' + new URLSearchParams({q, remote: 1}));
        if (gen !== p.generation || palette !== p) return;
        remote.items = (result.found || []).map(r => ({...r, remote: true, typed: q, hint: [r.work_name, `D 站 ${number(r.posts)} 张`].filter(Boolean).join(' · ')}));
        remote.note = result.found ? '' : result.unauthorized ? remoteNote(result) + '，也可以先只在本地新建' : remoteNote(result) + '，可以稍后再试，或先只在本地新建';
        remote.allow = result.unauthorized;
        // Untouched: the best match wins the highlight (the first local hit, else the first Danbooru result).
        renderPalette(sections, q, p.touched && p.index > 0);
      }, 380);
    }
  } else {
    if (p.suggest) {
      let sug; try { sug = await post('/api/suggest', p.suggest); } catch (e) { if (gen === p.generation && palette === p) renderPalette([{label: '本地候选', items: [], note: readError(e)}], q); return; }
      if (gen !== p.generation) return;
      if (sug.length) sections.push({label: '也可能是', items: sug.filter(e => !(p.exclude || []).includes(e.id)).map(e => ({...e, hint: e.count > 1 ? `${number(e.count)} 张的机器候选` : '机器候选'}))});
    }
    const seen = new Set(sections.flatMap(s => s.items.map(e => e.id)));
    const recent = prefs.get('picks', []).filter(e => !seen.has(e.id) && !(p.exclude || []).includes(e.id));
    if (recent.length) sections.push({label: '最近使用', items: recent}); recent.forEach(e => seen.add(e.id));
    const top = state.chars.filter(e => !seen.has(e.id) && !(p.exclude || []).includes(e.id)).slice(0, 8);
    if (top.length) sections.push({label: '常用', items: top.map(e => ({...e, hint: charHint(e)}))});
  }
  renderPalette(sections, q);
}
function renderPalette(sections, q, keep = false) {
  const p = palette, current = keep ? p.list[p.index] : null;
  p.list = sections.flatMap(s => s.items);
  p.index = current ? Math.max(0, p.list.indexOf(current)) : 0;
  // Local-only creation is a fallback: never the default when anything else matched.
  if (!current && p.list[p.index]?.create) p.index = Math.max(0, p.list.findIndex(e => !e.create));
  let n = 0;
  $('pickerResults').innerHTML = sections.filter(s => s.items.length || s.note).map(s => (s.label ? `<div class="palette-label">${s.label}</div>` : '') + (s.note && !s.items.length ? `<p class="palette-note">${esc(s.note)}${s.allow ? ' ' + namesAllowButton : ''}</p>` : '') + s.items.map(e => {
    const i = n++, char = e.id && state.charMap.get(e.id), cover = char?.cover;
    const staged = p.multi && p.staged.some(x => stageKey(x) === stageKey(e));
    const add = p.multi ? `<span class="palette-add${staged ? ' on' : ''}" data-add="${i}" title="${staged ? '已加入，再点一次取消' : '再加一个角色（⇧+点击）'}">${staged ? '✓' : '＋'}</span>` : '';
    return `<button class="palette-item${staged ? ' staged' : ''}" data-i="${i}" role="option">${cover ? `<img src="/image?id=${encodeURIComponent(cover)}" alt="" loading="lazy">` : `<span class="palette-glyph${e.remote ? ' remote' : ''}">${e.remote ? 'D' : e.create ? '＋' : e.remove ? '−' : e.tag ? '#' : '◌'}</span>`}<span class="palette-text"><span class="palette-name">${e.create ? (e.oc ? '新建原创角色「' : '新建「') + esc(e.name) + '」' : esc(e.name)}</span><small>${esc(e.hint || '')}${char && !e.hint?.includes('张') ? ` ${number(char.count)} 张` : ''}</small></span>${i < 9 && !q ? `<kbd>${i + 1}</kbd>` : ''}${add}</button>`;
  }).join('')).join('') || `<p class="palette-empty">${q ? '没有匹配的角色' : '输入角色 tag 或别名'}</p>`;
  for (const img of $('pickerResults').querySelectorAll('img')) img.addEventListener('error', () => img.replaceWith(Object.assign(document.createElement('span'), {className: 'palette-glyph', textContent: '◌'})), {once: true});
  highlight();
}
function highlight() {
  const p = palette; if (!p) return;
  for (const el of $('pickerResults').querySelectorAll('.palette-item')) el.classList.toggle('on', Number(el.dataset.i) === p.index);
  $('pickerResults').querySelector('.palette-item.on')?.scrollIntoView({block: 'nearest'});
  const e = p.list[p.index], chosen = p.multi ? finalList(e, p.touched) : e ? [e] : [];
  const sentence = chosen.length ? (p.sentence ? p.sentence({...chosen[0], name: chosen.map(x => x.name).join('、')}) : chosen.map(x => x.name).join('、')) : '';
  $('pickerFoot').innerHTML = chosen.length ? `<kbd>↵</kbd> ${esc(sentence)}${p.multi ? `<span class="palette-tip"><kbd>⇧↵</kbd> ${p.staged.length ? '继续加' : '多选'}</span>` : ''}` : `<span>${esc(p.title || '')}</span>`;
}
// Staged characters plus the highlighted one (unless it is already staged).
// Right after staging, ↵ applies the staged ones only; the highlight counts again once the user moves, types or clicks.
function finalList(e, include) {
  const p = palette, list = [...p.staged];
  if (e && (include || !list.length) && !e.remove && !list.some(x => stageKey(x) === stageKey(e))) list.push(e);
  return list;
}
function renderStaged() {
  const p = palette, box = $('pickerStaged');
  box.hidden = !p?.staged.length;
  box.innerHTML = (p?.staged || []).map((e, i) => `<button class="staged-chip" data-unstage="${i}" title="移除">${esc(e.create ? '新建「' + e.name + '」' : e.name)} ×</button>`).join('');
}
function stage(e) {
  const p = palette, k = stageKey(e), at = p.staged.findIndex(x => stageKey(x) === k);
  if (at >= 0) p.staged.splice(at, 1); else p.staged.push(e);
  p.touched = false; renderStaged(); $('pickerSearch').value = ''; $('pickerSearch').focus(); paletteSearch();
}
async function resolveEntity(e) {
  const entity = e.remote ? await post('/api/entities', {tag: e.tag, typed: e.typed})
    : e.create ? await post('/api/entities', e.oc ? {name: e.create, kind: 'oc'} : {name: e.create}) : e;
  if (!palette?.items) prefs.set('picks', [{id: entity.id, name: entity.name}, ...prefs.get('picks', []).filter(x => x.id !== entity.id)].slice(0, 8));
  return entity;
}
async function choose(i, more = false, explicit = false) {
  const p = palette, e = p?.list[i];
  if (p?.multi) {
    if (more) { if (e) stage(e); return; }
    const list = finalList(e, explicit || p.touched); if (!list.length) return;
    const entities = [];
    try { for (const x of list) entities.push(await resolveEntity(x)); } catch (err) { toast(err.message); return; }
    palette = null; $('picker').close(); p.pick(entities); return;
  }
  if (!e) return;
  if (p.tagMode) {
    if (!e.remove) prefs.set('tagPicks', [e.tag, ...prefs.get('tagPicks', []).filter(t => t !== e.tag)].slice(0, 8));
    palette = null; $('picker').close(); p.pick(e); return;
  }
  let entity; try { entity = await resolveEntity(e); } catch (err) { toast(err.message); return; }
  palette = null; $('picker').close(); p.pick(entity);
}
$('pickerSearch').addEventListener('input', () => { if (palette) palette.touched = true; clearTimeout(pickerTimer); pickerTimer = setTimeout(paletteSearch, 140); });
$('pickerSearch').addEventListener('keydown', e => {
  const p = palette; if (!p) return;
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') p.touched = true;
  if (e.key === 'ArrowDown') { e.preventDefault(); p.index = Math.min(p.list.length - 1, p.index + 1); highlight(); }
  else if (e.key === 'ArrowUp') { e.preventDefault(); p.index = Math.max(0, p.index - 1); highlight(); }
  else if (e.key === 'Enter') { e.preventDefault(); choose(p.index, e.shiftKey); }
  else if (/^[1-9]$/.test(e.key) && !$('pickerSearch').value) { e.preventDefault(); choose(Number(e.key) - 1, false, true); }
});
$('pickerResults').addEventListener('click', e => { const b = e.target.closest('[data-i]'); if (b) choose(Number(b.dataset.i), e.shiftKey || !!e.target.closest('[data-add]'), true); });
$('pickerStaged').addEventListener('click', e => { const b = e.target.closest('[data-unstage]'); if (b && palette) stage(palette.staged[Number(b.dataset.unstage)]); });
$('pickerResults').addEventListener('mousemove', e => { const b = e.target.closest('[data-i]'); if (b && palette && palette.index !== Number(b.dataset.i)) { palette.index = Number(b.dataset.i); highlight(); } });
$('picker').addEventListener('close', () => { palette = null; });
function mergeCharacter(c) {
  openPicker({title: `把 ${c.name} 合并到…`, placeholder: '输入同一个人的另一个角色', jump: true, exclude: [c.id],
    sentence: e => `把 ${c.name} 并入 ${e.name}：图、人工结论、子标签和别名都过去，可撤销`,
    pick: async e => {
      try { const r = await post('/api/merge', {source: c.id, target: e.id}); setUndo(r.undo); toast(r.undo.label, true); await refreshMeta(); go(charRoute(e.id)); }
      catch (err) { toast(err.message); }
    }});
}
// ---------- original characters ----------
// An original character (kind='oc'): an artist's own character, belonging to no work. Unnamed ones are 原创角色 N.
const charHint = e => e.kind === 'oc' ? '原创角色' : e.work_name || workName(e.work);
// Quick tagging uses the one shared「原创角色（未细分）」; a look-alike group (all one person) still gets its own OC.
async function markOc(body, leaving, separate = false) {
  let entity; try { entity = await post(separate ? '/api/entities' : '/api/oc/shared', {kind: 'oc'}); } catch (err) { toast(err.message); return; }
  prefs.set('picks', [{id: entity.id, name: entity.name}, ...prefs.get('picks', []).filter(x => x.id !== entity.id)].slice(0, 8));
  if (await mutate('/api/change', {...body, action: 'confirmed', entity: entity.id}, leaving) && state.route.page === 'group' && !state.total) go(charRoute(entity.id));
}
// Split the shared OC mark: look-alike groups computed on demand, each one click to its own OC (undoable).
async function splitOc(c) {
  const d = $('ocSplitDialog'), body = $('ocSplitBody');
  $('ocSplitTitle').textContent = `按相似细分 · ${c.name}`;
  body.innerHTML = '<p class="muted-note">正在比较长相…只在本机计算</p>';
  d.showModal();
  let r; try { r = await api('/api/oc/groups?' + new URLSearchParams({entity: c.id})); } catch (e) { body.innerHTML = `<p class="muted-note">${esc(readError(e))}</p>`; return; }
  const grouped = r.groups.reduce((n, g) => n + g.length, 0);
  body.innerHTML = `<p class="muted-note">${number(r.total)} 张里有 ${number(r.groups.length)} 组长得像（共 ${number(grouped)} 张）；其余的不像任何一张，留在这里。</p>` +
    (r.groups.map((g, i) => `<section class="oc-split-group" data-g="${i}"><div class="oc-split-thumbs">${g.slice(0, 8).map(u => `<img src="/image?id=${encodeURIComponent(u)}" alt="" loading="lazy">`).join('')}${g.length > 8 ? `<span>+${number(g.length - 8)}</span>` : ''}</div><div class="oc-split-meta"><span>${number(g.length)} 张</span><button class="quiet" data-split="${i}">设为新的原创角色</button></div></section>`).join('') || '<p class="palette-empty">没有长得像、能成组的图。可以在图库里选中后用「换成…」新建原创角色。</p>');
  for (const b of body.querySelectorAll('[data-split]')) b.onclick = async () => {
    const ids = r.groups[Number(b.dataset.split)]; b.disabled = true;
    let entity; try { entity = await post('/api/entities', {kind: 'oc'}); } catch (e) { toast(e.message); b.disabled = false; return; }
    if (await mutate('/api/reassign', {ids, old: c.id, new: entity.id}, true)) b.closest('.oc-split-group').remove();
    else b.disabled = false;
  };
}
$('ocSplitClose').onclick = () => $('ocSplitDialog').close();
// ---------- character profile (编辑资料) ----------
// A search box with a result menu: `search(q)` resolves to sections [{label, items, note}], `pick(item)` takes one.
function combo(input, menu, search, pick) {
  let list = [], index = 0, gen = 0, timer = 0;
  const render = sections => {
    list = sections.flatMap(x => x.items); index = Math.min(index, Math.max(0, list.length - 1));
    let i = 0;
    menu.innerHTML = sections.filter(x => x.items.length || x.note).map(x => (x.label ? `<div class="palette-label">${esc(x.label)}</div>` : '') + (x.note ? `<p class="palette-note">${esc(x.note)}${x.allow ? ' ' + namesAllowButton : ''}</p>` : '') +
      x.items.map(r => `<button type="button" class="picker-item watch-option${i === index ? ' on' : ''}" data-i="${i++}"><span class="watch-name">${esc(r.label)}</span><small>${esc(r.detail || '')}</small><em>${esc(r.extra || '')}</em></button>`).join('')).join('');
    menu.hidden = !menu.innerHTML;
  };
  const run = () => { const g = ++gen; clearTimeout(timer); timer = setTimeout(() => search(input.value.trim(), sections => { if (g === gen) render(sections); }), 160); };
  const choose = i => { const r = list[i]; if (!r) return; menu.hidden = true; pick(r); };
  input.addEventListener('input', () => { index = 0; run(); });
  input.addEventListener('focus', run);
  input.addEventListener('blur', () => setTimeout(() => { menu.hidden = true; }, 150));
  input.addEventListener('keydown', e => {
    if (menu.hidden || !list.length) return;
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); index = (index + (e.key === 'ArrowDown' ? 1 : list.length - 1)) % list.length; for (const b of menu.querySelectorAll('[data-i]')) b.classList.toggle('on', Number(b.dataset.i) === index); }
    else if (e.key === 'Enter') { e.preventDefault(); choose(index); }
    else if (e.key === 'Escape') { e.stopPropagation(); menu.hidden = true; }
  });
  menu.addEventListener('mousedown', e => { const b = e.target.closest('[data-i]'); if (b) { e.preventDefault(); choose(Number(b.dataset.i)); } });
}
// Local results first (instant), then Danbooru for the typed text only.
function twoStage(local, remote, extra = () => []) {
  return (q, show) => {
    local(q).then(mine => {
      if (!q) { show([{label: '', items: mine}, ...extra(q)]); return; }
      show([{label: '', items: mine}, {label: 'Danbooru', items: [], note: '正在查 Danbooru…（只发送你输入的文字）'}, ...extra(q)]);
      remote(q).then(result => {
        // A Danbooru entry the library already has shows once, as the local one.
        const found = result.found, fresh = found && found.filter(r => !mine.some(m => m.detail && m.detail === r.detail));
        show([{label: '', items: mine}, {label: 'Danbooru', items: fresh || [], allow: result.unauthorized,
          note: found === null ? remoteNote(result) : fresh.length ? '' : found.length ? '' : '没有找到，可以换英文或日文写法'}, ...extra(q)]);
      });
    }).catch(e => show([{label: '本地资料', items: [], note: readError(e)}]));
  };
}
async function editProfile(c) {
  let p; try { p = await api('/api/character/profile?' + new URLSearchParams({entity: c.id})); } catch (e) { toast(e.message); return; }
  const draft = {aliases: [...p.aliases], work: p.work_id ? {id: p.work_id, label: p.work_name} : null, tag: p.tag};
  const d = document.createElement('dialog'); d.className = 'palette profile-dialog';
  const autoName = p.tag && !p.user_name ? p.name : '';
  d.innerHTML = `<form method="dialog">
    <div class="dialog-top"><h2>编辑资料</h2><button type="button" class="icon" data-close aria-label="关闭">×</button></div>
    ${p.label_only ? '' : `<label class="field"><span>Danbooru 角色</span><div class="combo"><input id="pfTag" autocomplete="off" spellcheck="false" placeholder="搜索角色名或 tag，如 karen blue、カレン"><div class="combo-menu" id="pfTagMenu" hidden></div></div>
      <small id="pfTagNote"></small></label>`}
    <label class="field"><span>显示名</span><input id="pfName" maxlength="120" value="${esc(p.user_name)}" placeholder="${esc(autoName ? '留空：用自动名称「' + autoName + '」' : '角色名称')}">
      <small>${p.tag ? '关联了 Danbooru 时可以留空，中文名会自动补全' : '没有关联 Danbooru 时需要一个名称'}</small></label>
    <div class="field"><span>别名</span><div class="alias-box" id="pfAliases"></div><input id="pfAlias" maxlength="120" placeholder="输入别名后按回车，搜索时都能找到">
      <small>${p.other_aliases ? `另有 ${number(p.other_aliases)} 个来自 Danbooru 等来源的别名，会自动更新` : ''}</small></div>
    <label class="field"><span>所属作品</span>${p.oc ? '<small>原创角色不属于任何作品</small>' : `<div class="combo"><input id="pfWork" autocomplete="off" placeholder="搜索作品，找不到可以新建"><div class="combo-menu" id="pfWorkMenu" hidden></div></div><small id="pfWorkNote"></small>`}</label>
    <div class="dialog-actions"><button type="button" class="quiet" data-close>取消</button><button class="primary" id="pfSave">保存</button></div></form>`;
  document.body.append(d);
  const close = () => { d.close(); d.remove(); };
  for (const b of d.querySelectorAll('[data-close]')) b.onclick = close;
  d.addEventListener('close', () => d.remove());
  const q = sel => d.querySelector(sel);
  const aliases = () => { q('#pfAliases').innerHTML = draft.aliases.map((a, i) => `<button type="button" class="staged-chip" data-a="${i}" title="移除">${esc(a)} ×</button>`).join(''); };
  q('#pfAliases').onclick = e => { const b = e.target.closest('[data-a]'); if (b) { draft.aliases.splice(Number(b.dataset.a), 1); aliases(); } };
  q('#pfAlias').onkeydown = e => { const v = e.target.value.trim(); if (e.key === 'Enter') { e.preventDefault(); if (v && !draft.aliases.includes(v)) draft.aliases.push(v); e.target.value = ''; aliases(); } };
  aliases();
  const tagNote = () => { if (!q('#pfTagNote')) return; q('#pfTag').value = draft.tag ? draft.tag.replaceAll('_', ' ') : ''; q('#pfTagNote').innerHTML = draft.tag ? `已关联 <code>${esc(draft.tag)}</code> · <button type="button" class="link-button" id="pfUnlink">取消关联</button>` : '关联后自动补中文名、别名和所属作品'; q('#pfUnlink')?.addEventListener('click', () => { draft.tag = ''; tagNote(); }); };
  const workNote = () => { if (!q('#pfWork')) return; q('#pfWork').value = draft.work?.label || ''; q('#pfWorkNote').innerHTML = draft.work ? `${draft.work.create ? '保存时新建这个作品 · ' : draft.work.tag ? '保存时从 Danbooru 建立这个作品 · ' : ''}<button type="button" class="link-button" id="pfNoWork">不归属任何作品</button>` : (draft.tag && draft.tag !== p.tag && !p.work_id ? `留空：按 Danbooru 自动设置${draft.tagWork ? '（' + draft.tagWork + '）' : ''}` : '未归类'); q('#pfNoWork')?.addEventListener('click', () => { draft.work = null; workNote(); }); };
  tagNote(); workNote();
  if (q('#pfTag')) combo(q('#pfTag'), q('#pfTagMenu'), (text, show) => {
    if (!text) { show([]); return; }
    show([{label: 'Danbooru', items: [], note: '正在查 Danbooru…（只发送你输入的文字）'}]);
    remoteNames('/api/entities?' + new URLSearchParams({q: text, remote: 1})).then(({found, ...result}) => show([found
      ? {label: 'Danbooru', items: found.map(r => ({...r, label: r.name, detail: r.tag, extra: [r.work_name, `D 站 ${number(r.posts)}`].filter(Boolean).join(' · ')})), note: found.length ? '' : '没有找到，或已经在图库里（那就用「合并到另一个角色」）'}
      : {label: 'Danbooru', items: [], note: remoteNote(result), allow: result.unauthorized}]));
  }, r => { draft.tag = r.tag; draft.tagWork = r.work_name; tagNote(); workNote(); });
  if (q('#pfWork')) combo(q('#pfWork'), q('#pfWorkMenu'), twoStage(
    text => api('/api/works?' + new URLSearchParams({q: text})).then(ws => ws.slice(0, 8).map(w => ({id: w.id, label: w.name, detail: w.tag || '', extra: w.count ? `你有 ${number(w.count)} 张` : ''}))),
    text => remoteNames('/api/works?' + new URLSearchParams({q: text, remote: 1})).then(r => r.found ? {found: r.found.map(w => ({tag: w.tag, label: workName(w.tag) || w.tag, detail: w.tag, extra: `D 站 ${number(w.posts)}`}))} : r),
    text => text ? [{label: '', items: [{create: text, label: `新建作品「${text}」`, detail: '只在本地'}]}] : []),
    r => { draft.work = r; workNote(); });
  q('form').onsubmit = async e => {
    e.preventDefault();
    const body = {entity: p.id};
    const name = q('#pfName').value.trim(); if (name !== p.user_name) body.name = name;
    if (JSON.stringify(draft.aliases) !== JSON.stringify(p.aliases)) body.aliases = draft.aliases;
    if (q('#pfTag') && draft.tag !== p.tag) body.tag = draft.tag;
    try {
      if (q('#pfWork') && (draft.work?.id || null) !== (p.work_id || null)) {
        body.work = !draft.work ? '' : draft.work.create ? (await post('/api/works', {action: 'create', name: draft.work.create})).id
          : draft.work.tag ? (await post('/api/works', {action: 'from_tag', tag: draft.work.tag})).id : draft.work.id;
      }
      if (Object.keys(body).length === 1) { close(); return; }
      q('#pfSave').disabled = true;
      const r = await post('/api/character/profile', body);
      close(); setUndo(r.undo); toast(r.undo.label, true);
      state.localLinks = null; await refreshMeta(); renderHeading();
    } catch (err) { q('#pfSave').disabled = false; toast(err.message); }
  };
  d.showModal(); (q('#pfTag') && !p.tag ? q('#pfTag') : q('#pfName')).focus();
}
async function openLocalLinks() {
  const items = state.localLinks || [];
  const d = document.createElement('dialog'); d.className = 'palette profile-dialog';
  const links = items.filter(x => x.kind === 'link'), merges = items.filter(x => x.kind === 'merge');
  d.innerHTML = `<form method="dialog"><div class="dialog-top"><h2>关联自建角色</h2><button type="button" class="icon" data-close aria-label="关闭">×</button></div>
    ${links.length ? `<p class="muted-note">关联后自动补中文名、别名和所属作品，原来的名字留作别名，可以一步撤销。名字和 tag 完全一致的已勾选；标「可能是」的是按名字搜到的最像的一个，请确认后再勾。</p>
      <div class="link-list">${links.map(x => `<label class="link-row"><input type="checkbox" data-link="${esc(x.id)}"${x.guess ? '' : ' checked'}><span><b>${esc(x.name)}${x.guess ? ' <i class="guess">可能是</i>' : ''}</b><small>${esc(x.tag)}${x.work_name ? ' · ' + esc(x.work_name) : ''} · D 站 ${number(x.posts)} 张</small></span><em>${number(x.count)} 张</em></label>`).join('')}</div>` : ''}
    ${merges.length ? `<p class="muted-note">下面的自建角色和图库里已有的角色像是同一个，合并后图和人工结论都会过去（可撤销）：</p>
      <div class="link-list">${merges.map(x => `<div class="link-row"><span><b>${esc(x.name)}${x.guess ? ' <i class="guess">可能是</i>' : ''}</b><small>${number(x.count)} 张 → ${esc(x.target.name)} · ${esc(x.tag)}</small></span><button type="button" class="quiet" data-merge="${esc(x.id)}">合并到「${esc(x.target.name)}」</button></div>`).join('')}</div>` : ''}
    <div class="dialog-actions"><button type="button" class="quiet" data-close>关闭</button>${links.length ? '<button class="primary" id="llApply"></button>' : ''}</div></form>`;
  document.body.append(d);
  const close = () => { d.close(); d.remove(); };
  for (const b of d.querySelectorAll('[data-close]')) b.onclick = close;
  d.addEventListener('close', () => d.remove());
  const count = () => { const n = d.querySelectorAll('[data-link]:checked').length; if ($('llApply')) { $('llApply').textContent = `关联选中的 ${number(n)} 个`; $('llApply').disabled = !n; } };
  d.addEventListener('change', count);
  count();
  for (const b of d.querySelectorAll('[data-merge]')) b.onclick = async () => {
    const x = merges.find(m => m.id === b.dataset.merge); b.disabled = true;
    try { const r = await post('/api/merge', {source: x.id, target: x.target.id}); setUndo(r.undo); toast(r.undo.label, true); b.closest('.link-row').remove(); await refreshMeta(); refreshLocalLinks(); }
    catch (e) { b.disabled = false; toast(e.message); }
  };
  d.querySelector('form').onsubmit = async e => {
    e.preventDefault();
    const chosen = links.filter(x => d.querySelector(`[data-link="${CSS.escape(x.id)}"]`)?.checked).map(x => ({id: x.id, tag: x.tag}));
    if (!chosen.length) return;
    $('llApply').disabled = true; $('llApply').textContent = '正在关联…';
    try { const r = await post('/api/local-links', {items: chosen}); close(); setUndo(r.undo); toast(r.undo.label, true); await refreshMeta(); refreshLocalLinks(); }
    catch (err) { count(); toast(err.message); }
  };
  d.showModal();
}
async function toggleOc(c) {
  try {
    const r = await post('/api/kind', {entity: c.id, kind: c.kind === 'oc' ? '' : 'oc'});
    Object.assign(c, r); toast(r.kind === 'oc' ? `${c.name} 已标为原创角色` : `${c.name} 不再标为原创角色`);
    await refreshMeta(); renderHeading();
  } catch (err) { toast(err.message); }
}
async function toggleMascot(c) {
  try {
    const r = await post('/api/character/mascot', {entity: c.id, mascot: !c.mascot});
    c.mascot = r.mascot; toast(r.mascot ? `${c.name} 不再算角色，只当标签` : `${c.name} 已恢复成角色`);
    const [fresh] = await Promise.all([fetchChar(c.id), refreshMeta(), reload()]); if (fresh) Object.assign(c, fresh); renderHeading();
  } catch (err) { toast(err.message); }
}
// ---------- how selected images relate (web/relate.py) ----------
async function openRelate(ids) {
  const d = $('relateDialog');
  $('relateTitle').textContent = `${number(ids.length)} 张图的关系`;
  $('relateBody').innerHTML = '<p class="muted-note">正在逐对比较…只在本机计算</p>';
  d.showModal();
  let r; try { r = await post('/api/relate', {ids}); } catch (e) { $('relateBody').innerHTML = `<p class="muted-note">${esc(readError(e))}</p>`; return; }
  const no = u => ids.indexOf(u) + 1;
  const figure = (u, boxes) => `<figure class="rel-fig"><div class="rel-img"><img src="/image?id=${encodeURIComponent(u)}&size=medium" alt="">${boxes.map(b =>
    `<i class="${b.match ? 'match' : 'diff'}" style="left:${(b.x * 100).toFixed(2)}%;top:${(b.y * 100).toFixed(2)}%;width:${(b.w * 100).toFixed(2)}%;height:${(b.h * 100).toFixed(2)}%"></i>`).join('')}<span class="rel-no">${no(u)}</span></div>
    <figcaption>${r.sizes[u] ? `${number(r.sizes[u].width)} × ${number(r.sizes[u].height)}` : ''}</figcaption></figure>`;
  const shown = r.pairs.filter(p => p.kind !== 'unrelated'), rest = r.pairs.length - shown.length;
  const summary = [...r.same.map(g => `第 ${g.map(no).join('、')} 张是同一张图`), ...(r.missing.length ? [`${number(r.missing.length)} 张读不到原图（可能还在 iCloud），没有比较`] : [])];
  $('relateBody').innerHTML = `${summary.length ? `<p class="rel-summary">${summary.map(esc).join('；')}</p>` : ''}
    ${shown.map(p => `<section class="rel-pair"><div class="rel-head"><span class="rel-kind k-${p.kind}">${esc(p.label)}</span><span>${esc(p.note || '')}</span></div>
      <div class="rel-figs">${figure(p.a, p.boxes_a)}${figure(p.b, p.boxes_b)}</div></section>`).join('')}
    ${rest ? `<p class="muted-note">${shown.length ? '另外 ' : ''}${number(rest)} 对是不同的图</p>` : ''}
    <p class="dialog-note">红框：两张不一样的地方；蓝色虚线：小图在大图里的位置。逐像素比较得出，只在本机计算，结果不保存。</p>`;
}
$('relateClose').onclick = () => $('relateDialog').close();

function jumpToCharacter() { openPicker({title: '跳到角色', jump: true, sentence: e => '打开 ' + e.name, pick: e => go(charRoute(e.id))}); }
$('jumpButton').onclick = jumpToCharacter;

// ---------- inspector ----------
function closeInspector() { detailGen++; if (state.viewer < 0) hold = null; state.focused = null; state.detail = null; if (state.viewer < 0) state.question = null; $('inspector').hidden = true; markHomeFocus(); syncCards(); requestAnimationFrame(() => renderGrid()); }
function identityChips(d, compact = false) {
  const r = state.route;
  const chips = d.memberships.map(m => `<div class="identity"><span class="dot ${m.status}" title="${m.status === 'machine' ? '机器识别' : m.origin === 'album' ? '相簿' : '人工确认'} · ${esc(evidence(m.evidence))}"></span><a class="identity-name" href="${charHash(m.id)}" title="${esc(m.name)}">${esc(m.name)}</a><span class="identity-actions"><button class="icon small" data-swap="${esc(m.id)}" title="换成…（C）">⇄</button><button class="icon small" data-not="${esc(m.id)}" title="不是 TA（⌫）">×</button></span>
    ${compact ? '' : `<div class="variant-row">${(m.variants || []).map(v => `<span class="variant-chip">${esc(v.name)}<button data-unvariant="${esc(v.id)}" title="去掉子标签">×</button></span>`).join('')}${(state.charMap.get(m.id)?.variants || []).some(v => !(m.variants || []).some(x => x.id === v.id)) ? `<button class="variant-add" data-addvariant="${esc(m.id)}">＋ 子标签</button>` : ''}</div>`}</div>`).join('');
  const alts = (d.alternatives || []).slice(0, 9).map((a, i) => `<button class="alt${a.weak ? ' weak' : ''}" data-alt="${i}" title="${a.weak ? '弱线索（中档相似，不算归属）· ' : ''}${esc(evidence(a.evidence))}。点击：换成这个；⌥ 点击：也是"><kbd>${i + 1}</kbd>${esc(a.name)}</button>`).join('');
  return `<div class="identities">${chips || `<p class="none-yet">${d.unknown ? '已跳过，' : ''}还没有角色</p>`}<button class="add-identity" data-also="1">${d.memberships.length ? '＋ 也是…' : '＋ 设为角色…'}</button></div>` +
    (alts ? `<div class="alts"><h3>也可能是</h3><div class="alt-row">${alts}</div></div>` : '');
}
// PROTO = prototypes learned from the user's own verdicts (web/learn.py).
const SOURCE_NAMES = {CCIP: '外部索引', PROTO: '你的标注'};
const evidence = e => !e ? '' : [e.wd != null ? `WD ${e.wd.toFixed(2)}` : '', e.ccip != null ? `${e.sources?.includes('PROTO') && !e.sources.includes('CCIP') ? '你的标注' : '外部索引'} ${e.ccip.toFixed(3)}` : '', !('wd' in e) && e.sources?.length ? e.sources.map(s => SOURCE_NAMES[s] || s).join(' + ') : ''].filter(Boolean).join(' · ');
const INSP_ICONS = {
  prev: '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="m12 4.5-5.5 5.5 5.5 5.5"/></svg>',
  next: '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="m8 4.5 5.5 5.5L8 15.5"/></svg>',
  expand: '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M12 3.5h4.5V8M8 16.5H3.5V12M16.5 3.5 11 9M3.5 16.5 9 11"/></svg>',
  close: '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="m5 5 10 10M15 5 5 15"/></svg>',
};
// Collection rhythm of the image's character: cached per character until the next write or R18 change.
const statsCache = new Map();
async function loadCharStats(id, ident, gen) {
  const box = $('charStats'); if (!box || (!viewOpts.chart && !viewOpts.same)) return;
  const key = `${id}|${state.nsfw}|${state.readEpoch}`;
  let st = statsCache.get(key);
  if (!st) { try { st = await api('/api/character/stats?' + new URLSearchParams({id, nsfw: state.nsfw, exclude: ident})); } catch { return; } statsCache.set(key, st); }
  if (gen !== detailGen || !$('charStats')) return;
  const months = st.months || [], max = Math.max(1, ...months.map(m => m.count)), now = months.at(-1)?.count || 0;
  const recent = (st.recent || []).filter(u => u !== ident).slice(0, 4);
  $('charStats').innerHTML = (viewOpts.chart ? `<div class="cc-line"><span>收藏 <b>${number(st.count)}</b> 张${st.first ? ` · 始于 ${st.first.replace('-', '.')}` : ''}</span>${now ? `<em>本月 +${number(now)}</em>` : ''}</div>
    <div class="cc-bars">${months.map((m, i) => `<i style="height:${Math.max(8, m.count / max * 100)}%${i === months.length - 1 ? '' : ''}" class="${i === months.length - 1 ? 'now' : ''}" title="${m.month.replace('-', ' 年 ')} 月 · ${number(m.count)} 张"></i>`).join('')}</div>` : '') +
    (viewOpts.same && recent.length ? `<div class="cc-same"><h5>同一角色<a href="${charHash(id)}">全部 ›</a></h5><div>${recent.map(u => `<button data-copy="${esc(u)}" title="查看这张"><img src="/image?id=${encodeURIComponent(u)}&size=thumb" alt="" loading="lazy"></button>`).join('')}</div></div>` : '');
  for (const b of $('charStats').querySelectorAll('[data-copy]')) b.onclick = () => inspect(b.dataset.copy);
}
async function inspect(ident, quiet = false) {
  const gen = ++detailGen; state.focused = ident; $('inspector').hidden = false; markHomeFocus();
  if (!quiet) $('inspector').innerHTML = '<div class="loading-line">读取图片详情…</div>';
  syncCards(); requestAnimationFrame(() => renderGrid());
  try {
    const d = await api('/api/detail?id=' + encodeURIComponent(ident)); if (gen !== detailGen) return;
    state.detail = d;
    const primary = primaryOf(d), pc = primary && (state.charMap.get(primary.id) || primary);
    const original = pc?.tag && pc.tag !== pc.name ? pc.tag.replaceAll('_', ' ') : '';
    const path = pc ? [pc.kind === 'oc' ? '原创角色' : pc.work_name || workName(pc.work || ''), ...(primary.variants || []).map(v => v.name)].filter(Boolean) : [];
    const curate = d.excluded ? `<button class="btn" data-curate="include" title="已排除 · ${esc(REASONS[d.excluded.reason] || '')}${d.excluded.origin === 'rule' ? '（自动规则）' : ''}">恢复收录</button>`
      : d.pending ? `<button class="btn" data-curate="exclude">排除 <kbd>X</kbd></button><button class="btn" data-curate="include">保留</button>`
      : `<button class="btn" data-curate="exclude" title="截图、照片等不是插画的图">排除 <kbd>X</kbd></button>`;
    $('inspector').innerHTML = `<div class="inspector-head"><button class="icon" data-nav="-1" aria-label="上一张" title="上一张（←）">${INSP_ICONS.prev}</button><button class="icon" data-nav="1" aria-label="下一张" title="下一张（→）">${INSP_ICONS.next}</button><div class="spacer"></div><button class="icon" id="expand" title="大图（Space）" aria-label="大图">${INSP_ICONS.expand}</button><button class="icon" id="closeInspector" aria-label="关闭详情" title="关闭（Esc）">${INSP_ICONS.close}</button></div>
      <button class="preview" id="previewButton" title="大图查看"><img src="/image?id=${encodeURIComponent(ident)}&size=preview" alt="插画预览"></button>
      ${shouldAsk(d, ident) ? askHTML(d.question) : ''}
      ${pc ? `<section class="inspector-section cc-card"><a class="cc-name" href="${charHash(pc.id)}" title="在图库里只看 TA">${esc(pc.name)}</a>${original ? `<div class="cc-orig">${esc(original)}</div>` : ''}${path.length ? `<div class="cc-path">${path.map(esc).join(' · ')}</div>` : ''}${d.excluded ? `<div class="cc-note">已排除 · ${esc(REASONS[d.excluded.reason] || '')}</div>` : d.pending ? '<div class="cc-note">待确认：像截图</div>' : ''}
        <div id="charStats" class="cc-stats"></div></section>`
        : `<section class="inspector-section cc-card"><div class="cc-name muted">${d.unknown ? '已跳过' : '还没认出是谁'}</div>${d.excluded ? `<div class="cc-note">已排除 · ${esc(REASONS[d.excluded.reason] || '')}</div>` : d.pending ? '<div class="cc-note">待确认：像截图</div>' : ''}</section>`}
      <section class="inspector-section"><h3 class="section-title">角色</h3>${identityChips(d)}${(d.tag_identities || []).length ? `<div class="tag-ids"><span>标签</span>${d.tag_identities.map(t => `<a class="variant-chip" href="${charHash(t.id)}" title="只当标签、不算角色的身份；点开可以改回角色">${esc(t.name)}</a>`).join('')}</div>` : ''}</section>
      ${artistSection(d, ident)}
      <section class="inspector-section"><h3 class="section-title">我的标签</h3><div class="user-tags">${(d.user_tags || []).map(t => `<span class="variant-chip"><a href="#/?utag=${encodeURIComponent(t)}"># ${esc(t)}</a><button data-untag="${esc(t)}" title="去掉标签">×</button></span>`).join('')}<button class="variant-add" id="addTag">＋ 标签 <kbd>G</kbd></button></div></section>
      ${d.duplicates?.length ? `<section class="inspector-section"><h3 class="section-title">重复副本 · 共 ${d.duplicates.length + 1} 份</h3><div class="dup-list">${d.duplicates.map(x => `<button class="dup-copy" data-copy="${esc(x.uuid)}" title="查看这份副本"><img src="/image?id=${encodeURIComponent(x.uuid)}&size=thumb" alt="" loading="lazy"><span><b>${x.pos}/${x.size}</b> ${esc((x.added_at || '').slice(0, 10))}<br><small>${x.names.length ? x.names.map(esc).join(' · ') : '未识别'}</small></span></button>`).join('')}</div></section>` : ''}
      ${viewOpts.meta ? `<section class="inspector-section"><dl class="facts"><div><dt>尺寸</dt><dd class="mono">${number(d.width)} × ${number(d.height)}</dd></div><div><dt>加入</dt><dd class="mono">${esc((d.added_at || '').slice(0, 16).replace('T', ' '))}</dd></div><div><dt>文件</dt><dd title="${esc(d.filename)}">${esc(d.filename)}</dd></div><div><dt>文件形态</dt><dd>${d.src_kind === 'deriv' ? '本地副本' : '原图'}</dd></div></dl></section>
      <details class="inspector-section"><summary>画面标签</summary><div class="tags">${d.tags.map(t => `<button class="tag" data-tag="${esc(t.value)}" title="按此标签筛选图库 · WD ${t.conf.toFixed(2)}">${esc(t.value.replaceAll('_', ' '))}</button>`).join('') || '<p>暂无</p>'}</div></details>` : ''}
      <div class="inspector-actions">${d.memberships.length ? '<button class="btn pri" id="fixChar">更正角色 <kbd>C</kbd></button>' : '<button class="btn pri" data-also="1">设为角色… <kbd>C</kbd></button>'}${state.route.page === 'unmatched' && !d.memberships.length ? '<button class="btn" id="skipOne">跳过 <kbd>S</kbd></button>' : ''}${d.unknown ? '<button class="btn" id="resumeOne">恢复到未识别</button>' : ''}${curate}</div>`;
    if (pc) loadCharStats(pc.id, ident, gen);
    $('fixChar')?.addEventListener('click', () => swapOne(d, ident, primary.id));
    $('inspector').querySelector('.preview img').addEventListener('error', e => { e.target.parentElement.innerHTML = '<div class="missing">图片暂不可读，请检查 Photos 下载状态与访问权限</div>'; }, {once: true});
    $('closeInspector').onclick = closeInspector;
    $('expand').onclick = $('previewButton').onclick = () => openViewer(indexOf(ident));
    for (const b of $('inspector').querySelectorAll('[data-nav]')) b.onclick = () => step(Number(b.dataset.nav));
    for (const b of $('inspector').querySelectorAll('[data-tag]')) b.onclick = () => go({page: 'library', tag: b.dataset.tag});
    for (const b of $('inspector').querySelectorAll('[data-copy]')) b.onclick = () => inspect(b.dataset.copy);
    for (const b of $('inspector').querySelectorAll('[data-curate]')) b.onclick = () => mutate('/api/curate', {ids: [ident], action: b.dataset.curate}, b.dataset.curate === 'exclude' || ['excluded', 'pending'].includes(state.route.page));
    $('skipOne')?.addEventListener('click', () => singleAction('skip'));
    $('addTag').onclick = () => singleAction('tag');
    for (const b of $('inspector').querySelectorAll('[data-artist-verdict]')) b.onclick = () => {
      const v = b.dataset.artistVerdict || null;
      mutate('/api/artist/verdict', {ids: [ident], artist: b.dataset.artist, verdict: v, name: b.dataset.name}, v === 'no' && state.route.artist === b.dataset.artist);
    };
    $('inspector').querySelector('[data-artist-find]')?.addEventListener('click', () => findArtists([ident], () => inspect(ident, true)));
    for (const b of $('inspector').querySelectorAll('[data-artist-more]')) b.onclick = () => findArtistWorks(b.dataset.artistMore, b.dataset.name, () => inspect(ident, true));
    for (const b of $('inspector').querySelectorAll('[data-untag]')) b.onclick = () => mutate('/api/tag', {ids: [ident], tag: b.dataset.untag, add: false}, state.route.utag === b.dataset.untag);
    $('resumeOne')?.addEventListener('click', () => mutate('/api/change', {ids: [ident], action: 'resume'}, state.route.page === 'skipped'));
    bindIdentity($('inspector'), d, ident); bindAsk($('inspector'), d, ident);
  } catch (e) { if (gen === detailGen) { $('inspector').innerHTML = `<div class="loading-line">${esc(e.message)}<br><button class="quiet" id="closeInspector">关闭</button></div>`; $('closeInspector').onclick = closeInspector; } }
}
function bindIdentity(root, d, ident) {
  const leaves = id => state.route.char === id;
  for (const b of root.querySelectorAll('[data-not]')) b.onclick = () => mutate('/api/change', {ids: [ident], action: 'rejected', entity: b.dataset.not}, leaves(b.dataset.not));
  for (const b of root.querySelectorAll('[data-swap]')) b.onclick = () => swapOne(d, ident, b.dataset.swap);
  for (const b of root.querySelectorAll('[data-unvariant]')) b.onclick = () => mutate('/api/change', {ids: [ident], action: 'remove_variant', entity: b.dataset.unvariant}, !!state.route.variant);
  for (const b of root.querySelectorAll('[data-addvariant]')) b.onclick = async () => {
    const epoch = state.readEpoch, mode = state.nsfw;
    const c = await fetchChar(b.dataset.addvariant);
    if (!c || epoch !== state.readEpoch || mode !== state.nsfw || (state.focused !== ident && state.viewerItem?.uuid !== ident)) return;
    openPicker({title: '加子标签', items: c.variants.map(v => ({id: v.id, name: v.name, hint: `${number(v.count)} 张`})), placeholder: '筛选子标签', sentence: e => `给这张加上 ${e.name}`, pick: e => mutate('/api/change', {ids: [ident], action: 'add_variant', entity: e.id}, false)});
  };
  for (const b of root.querySelectorAll('[data-alt]')) b.onclick = e => pickAlternative(d, ident, Number(b.dataset.alt), e.altKey);
  for (const b of root.querySelectorAll('[data-also]')) b.onclick = () => openPicker({title: d.memberships.length ? '这张也是…' : '这张是…', sentence: e => (d.memberships.length ? '给这张加上 ' : '把这张设为 ') + e.name, exclude: d.memberships.map(m => m.id), suggest: {ids: [ident]}, multi: true,
    pick: list => mutate('/api/change', {ids: [ident], action: 'confirmed', ...targets(list)}, ['unmatched', 'skipped'].includes(state.route.page))});
}
// ---------- 主动提问：偶尔问一句「是 TA 吗？」 ----------
// While browsing, at most one question every few images, and none for the rest of the visit after three are passed by;
// cleanup pages (a character, 未识别…) always show it. Answers go to /api/ask/answer and feed the learner.
const ask = {cool: 0, ignored: 0, off: false, pending: null};
function shouldAsk(d, ident) {
  if (!viewOpts.ask || !d.question) return false;
  if (ask.pending === ident) return true;
  if (ask.pending) { ask.ignored++; ask.pending = null; if (ask.ignored >= 3) ask.off = true; }
  if (!cleanup()) {
    if (ask.off) return false;
    if (ask.cool > 0) { ask.cool--; return false; }
    ask.cool = 3;
  }
  ask.pending = ident; return true;
}
function askHTML(q, dark = false) {
  const why = q.kind === 'audit' ? '随机抽查' : q.kind === 'hint' ? `和你确认过的 TA 很像${q.current_name ? ` · 现在算作 ${esc(q.current_name)}` : ''}` : q.kind === 'ambiguous' ? `和 ${esc(q.rival_name)} 分不太清` : q.confirmed < 5 ? `再确认 ${5 - q.confirmed} 张就能学会认 TA` : '';
  return `<div class="ask-bar${dark ? ' on-dark' : ''}"><div class="ask-q">是 <b>${esc(q.name)}</b> 吗？${why ? `<small>${why}</small>` : ''}</div>
    <div class="ask-btns"><button class="ask-b yes" data-ask="yes">是 <kbd>Y</kbd></button><button class="ask-b" data-ask="no">不是 <kbd>N</kbd></button>${q.rival ? `<button class="ask-b" data-ask="switch" title="换成 ${esc(q.rival_name)}">是 ${esc(q.rival_name)}</button>` : ''}</div></div>`;
}
function bindAsk(root, d, ident) {
  state.question = root.querySelector('.ask-bar') ? {q: d.question, ident} : null;
  for (const b of root.querySelectorAll('[data-ask]')) b.onclick = () => answerAsk(b.dataset.ask);
}
function answerAsk(how) {
  const cur = state.question; if (!cur) return;
  const {q, ident} = cur; ask.pending = null; ask.ignored = 0;
  // 随机抽查: the answered image leaves the list, so the inspector moves on to the next one.
  if (state.route.page === 'audit' && state.viewer < 0 && state.focused === ident) { const i = indexOf(ident); state.focused = state.items[i + 1]?.uuid || state.items[i - 1]?.uuid || null; }
  const body = {ids: [ident], entity: q.entity, kind: q.kind, ...(how === 'yes' ? {yes: [ident]} : {no: [ident]}), ...(how === 'switch' ? {switch: q.rival} : {})};
  mutate('/api/ask/answer', body, how !== 'yes' && state.route.char === q.entity, {advance: state.route.page === 'audit'});
}
// The identity an action replaces: the current character page if it applies, else the first one.
const primaryOf = d => d.memberships.find(m => m.id === state.route.char) || d.memberships[0];
function swapOne(d, ident, from) {
  const old = d.memberships.find(m => m.id === from);
  openPicker({title: `换成…`, sentence: e => `把这张从 ${old.name} 换成 ${e.name}`, exclude: d.memberships.map(m => m.id), suggest: {ids: [ident]}, multi: true,
    pick: list => mutate('/api/reassign', {ids: [ident], old: from, new: newIds(list)}, state.route.char === from)});
}
function pickAlternative(d, ident, i, also = false) {
  const alt = d.alternatives?.[i]; if (!alt) return;
  const from = primaryOf(d);
  if (from && !also) mutate('/api/reassign', {ids: [ident], old: from.id, new: alt.id}, state.route.char === from.id);
  else mutate('/api/change', {ids: [ident], action: 'confirmed', entity: alt.id}, ['unmatched', 'skipped'].includes(state.route.page));
}
// Single-image shortcuts act on the focused/cursor image when nothing is selected.
function singleTarget() { return currentItem(); }
async function singleAction(kind) {
  const item = singleTarget(); if (!item) return;
  const r = state.route;
  if (kind === 'remove') {
    if (r.char) mutate('/api/change', {ids: [item.uuid], action: 'rejected', entity: r.char}, true);
    else if (item.memberships.length === 1) mutate('/api/change', {ids: [item.uuid], action: 'rejected', entity: item.memberships[0].id}, false);
    else if (item.memberships.length) openPicker({title: '这张不是…', items: item.memberships, sentence: e => `把 ${e.name} 从这张移出`, pick: e => mutate('/api/change', {ids: [item.uuid], action: 'rejected', entity: e.id}, false)});
  } else if (kind === 'tag') {
    const d = state.detail?.uuid === item.uuid ? state.detail : null;
    openTagPicker(() => ({ids: [item.uuid]}), 1, [{...item, user_tags: d?.user_tags || item.user_tags}], () => false);
  } else if (kind === 'skip' && r.page === 'unmatched') mutate('/api/change', {ids: [item.uuid], action: 'unknown'}, true);
  else if (kind === 'exclude' || kind === 'include') mutate('/api/curate', {ids: [item.uuid], action: kind}, true);
  else if (kind === 'swap' || kind === 'also') {
    const d = state.detail?.uuid === item.uuid ? state.detail : await api('/api/detail?id=' + encodeURIComponent(item.uuid));
    const from = primaryOf(d);
    if (kind === 'swap' && from) swapOne(d, item.uuid, from.id);
    else openPicker({title: from ? '这张也是…' : '这张是…', sentence: e => (from ? '给这张加上 ' : '把这张设为 ') + e.name, exclude: d.memberships.map(m => m.id), suggest: {ids: [item.uuid]},
      pick: e => mutate('/api/change', {ids: [item.uuid], action: 'confirmed', entity: e.id}, ['unmatched', 'skipped'].includes(r.page))});
  }
}
const indexOf = uuid => state.items.findIndex(x => x?.uuid === uuid);
async function step(delta) {
  const cur = state.viewer >= 0 ? state.viewerItem?.uuid : state.focused;
  if (hold && hold.uuid === cur && Math.abs(delta) === 1) {
    const target = indexOf(delta > 0 ? hold.next : hold.prev); hold = null;
    if (target >= 0) { state.cursor = target; ensureVisible(target); if (state.viewer >= 0) showViewer(target); else inspect(state.items[target].uuid); return; }
  }
  hold = null;
  const at = cur ? indexOf(cur) : -1;
  const from = at >= 0 ? at : state.viewer >= 0 ? state.viewer : state.cursor;
  const n = Math.max(0, Math.min(state.total - 1, from + delta)); if (n === from || n < 0) return;
  if (!state.items[n]) await loadPage(Math.floor(n / PAGE) * PAGE);
  if (!state.items[n]) return;
  state.cursor = n; ensureVisible(n);
  if (state.viewer >= 0) showViewer(n); else inspect(state.items[n].uuid);
}
function ensureVisible(i) {
  const v = $('viewport'), row = rowOf(i), top = rowTop(row), height = rowHeight(row);
  if (top < v.scrollTop) v.scrollTop = top; else if (top + height > v.scrollTop + v.clientHeight) v.scrollTop = top + height - v.clientHeight;
  syncCards();
}

// ---------- full-screen viewer ----------
const view = {scale: 1, x: 0, y: 0, fit: 1};
function openViewer(i) { if (i < 0 || !state.items[i]) return; state.viewer = i; $('viewer').hidden = false; document.body.classList.add('viewing'); showViewer(i); }
function closeViewer() { if (state.viewer < 0) return; hold = null; state.viewerItem = null; state.cursor = state.viewer; $('viewer').hidden = true; document.body.classList.remove('viewing'); state.viewer = -1; ensureVisible(state.cursor); }
async function showViewer(i, held = null) {
  if (i < 0 && !held) { closeViewer(); return; }
  if (!held && !state.items[i]) await loadPage(Math.floor(i / PAGE) * PAGE);
  const item = held || state.items[i]; if (!item) { closeViewer(); return; }
  const same = state.viewerItem?.uuid === item.uuid && !$('viewer').hidden;
  state.viewer = Math.max(0, i); state.cursor = state.viewer; state.viewerItem = item;
  if (same) { $('viewerCount').textContent = indexOf(item.uuid) >= 0 ? `${number(indexOf(item.uuid) + 1)} / ${number(state.total)}` : `已移出当前列表 · ← → 继续`; return refreshViewerBar(item, i); }
  const img = $('viewerImg'); img.style.transform = ''; img.hidden = false; $('viewerMissing').hidden = true; Object.assign(view, {scale: 1, x: 0, y: 0});
  img.onerror = () => { if (state.viewerItem?.uuid === item.uuid) { img.hidden = true; $('viewerMissing').hidden = false; } };
  img.src = `/image?id=${encodeURIComponent(item.uuid)}&size=preview`;
  const full = new Image(); full.onload = () => { if (state.viewerItem?.uuid === item.uuid) { img.src = full.src; $('viewerSize').textContent = `本地副本 ${full.naturalWidth} × ${full.naturalHeight}`; } }; full.src = `/image?id=${encodeURIComponent(item.uuid)}&size=full`;
  $('viewerSize').textContent = '';
  $('viewerCount').textContent = `${number(i + 1)} / ${number(state.total)}`;
  for (const d of [-2, -1, 1, 2]) { const x = state.items[i + d]; if (x) new Image().src = `/image?id=${encodeURIComponent(x.uuid)}&size=preview`; }
  return refreshViewerBar(item, i);
}
async function refreshViewerBar(item, i) {
  let d; try { d = await api('/api/detail?id=' + encodeURIComponent(item.uuid)); } catch (e) { if (state.viewerItem?.uuid === item.uuid) readFailure($('viewerBar'), e); return; }
  if (!d || state.viewerItem?.uuid !== item.uuid) return;
  state.detail = d; $('viewerBar').innerHTML = (shouldAsk(d, item.uuid) ? askHTML(d.question, true) : '') + identityChips(d, true);
  bindIdentity($('viewerBar'), d, item.uuid); bindAsk($('viewerBar'), d, item.uuid);
}
function applyView() { $('viewerImg').style.transform = `translate(${view.x}px,${view.y}px) scale(${view.scale})`; $('viewerStage').classList.toggle('zoomed', view.scale > 1.01); }
function zoomView(factor, cx, cy) {
  const stage = $('viewerStage').getBoundingClientRect(), px = cx - stage.left - stage.width / 2, py = cy - stage.top - stage.height / 2;
  const img = $('viewerImg'), limit = Math.max(2, (img.naturalWidth / img.clientWidth) * 2 || 2);
  const next = Math.max(1, Math.min(limit, view.scale * factor)), k = next / view.scale;
  view.x = px - (px - view.x) * k; view.y = py - (py - view.y) * k; view.scale = next;
  if (next === 1) { view.x = 0; view.y = 0; } applyView();
}
$('viewerStage').addEventListener('wheel', e => {
  e.preventDefault();
  if (e.ctrlKey || e.metaKey) zoomView(Math.exp(-e.deltaY / 200), e.clientX, e.clientY);
  else if (view.scale > 1) { view.x -= e.deltaX; view.y -= e.deltaY; applyView(); }
}, {passive: false});
let viewGesture = 1;
$('viewerStage').addEventListener('gesturestart', e => { e.preventDefault(); viewGesture = 1; });
$('viewerStage').addEventListener('gesturechange', e => { e.preventDefault(); zoomView(e.scale / viewGesture, e.clientX, e.clientY); viewGesture = e.scale; });
$('viewerStage').addEventListener('dblclick', e => {
  const img = $('viewerImg'), actual = img.naturalWidth / img.clientWidth;
  if (view.scale > 1.01) { Object.assign(view, {scale: 1, x: 0, y: 0}); applyView(); } else zoomView(Math.max(2, actual), e.clientX, e.clientY);
});
let pan = null;
$('viewerStage').addEventListener('pointerdown', e => { if (view.scale > 1) { pan = {x: e.clientX - view.x, y: e.clientY - view.y}; $('viewerStage').setPointerCapture(e.pointerId); } });
$('viewerStage').addEventListener('pointermove', e => { if (pan) { view.x = e.clientX - pan.x; view.y = e.clientY - pan.y; applyView(); } });
$('viewerStage').addEventListener('pointerup', () => { pan = null; });
$('viewerStage').addEventListener('click', e => { if (e.target === $('viewerStage') && view.scale <= 1) closeViewer(); });
$('viewerClose').onclick = closeViewer; $('viewerPrev').onclick = () => step(-1); $('viewerNext').onclick = () => step(1);

// ---------- characters: 书架 (work tabs, posters, tiers) and 列表 ----------
const cast = {mode: prefs.get('castMode', 'shelf'), tab: 'all', list: null, activity: new Map(), activityKey: '', sort: 'count', dir: -1, bucket: 'all', pop: false, popQ: ''};
async function renderIndex() {
  $('grid').hidden = true; $('empty').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = true; $('charIndex').hidden = false; closeInspector(); $('selectionBar').hidden = true;
  $('resultCount').textContent = '';
  if (state.route.section === 'works') { renderShelf(); return; }
  $('hint').textContent = '点角色，在图库里只看 TA';
  cast.list = null; cast.pop = false;
  if (state.chars.some(c => c.summary)) {
    try { await refreshMeta(); } catch (e) { error(e.message); return; }
    if (state.route.page !== 'characters') return;
  }
  drawCast();
  api('/api/tagged?' + new URLSearchParams({nsfw: state.nsfw})).then(t => { state.tagged = t; drawCast(); }).catch(e => error(readError(e)));
  state.workTree = await api('/api/works').catch(e => { error(readError(e)); return state.workTree || []; });
  loadActivity(); refreshLocalLinks();
}
// Own characters whose name is a Danbooru tag (asks Danbooru with those names only; none → no request).
async function refreshLocalLinks() {
  const found = await api('/api/local-links').catch(() => null);
  if (found === null) return;
  state.localLinks = found;
  if (state.route.page === 'characters') renderHeading();
}
async function loadActivity() {
  const key = state.nsfw + '|' + state.readEpoch; if (cast.activityKey === key) return;
  let act; try { act = await api('/api/character/activity?nsfw=' + state.nsfw); } catch (e) { error(readError(e)); return; }
  if (!act) return;
  cast.activity = new Map(Object.entries(act)); cast.activityKey = key;
  if (state.route.page === 'characters') drawCast();
}
async function searchIndex() {
  const route = state.route, token = state.token;
  const q = $('charSearch').value.trim();
  if (!q) { cast.list = null; drawCast(); return; }
  let list; try { list = await api('/api/characters?min=1&q=' + encodeURIComponent(q) + '&nsfw=' + state.nsfw); } catch (e) { if (route === state.route && token === state.token && q === $('charSearch').value.trim()) readFailure($('charIndex'), e); return; }
  if (route === state.route && token === state.token && q === $('charSearch').value.trim()) { cast.list = list; drawCast(); }
}
// Replace only the sections whose HTML changed, so covers that did not change are not reloaded.
function reconcileSections(container, sections, empty = '没有匹配的角色') {
  if (!sections.length) { container.innerHTML = `<p class="empty">${empty}</p>`; return; }
  const old = new Map([...container.children].map(el => [el._sectionKey, el]));
  let cursor = container.firstElementChild;
  const keep = new Set();
  for (const [key, html] of sections) {
    let el = old.get(key);
    if (!el || el._sectionHTML !== html) {
      const previous = el;
      const template = document.createElement('template'); template.innerHTML = html;
      el = template.content.firstElementChild; el._sectionKey = key; el._sectionHTML = html;
      if (previous) { previous.replaceWith(el); if (cursor === previous) cursor = el; }
    }
    if (el !== cursor) container.insertBefore(el, cursor);
    else cursor = cursor.nextElementSibling;
    keep.add(el);
  }
  for (const el of [...container.children]) if (!keep.has(el)) el.remove();
}
// Kept for callers that refresh the page after metadata changes.
function renderIndexBody() { if (state.route.page !== 'characters') return; if (state.route.section === 'works') drawShelf(); else { drawCast(); loadActivity(); } }
const recentOf = c => cast.activity.get(c.id)?.recent || 0;
const workKey = c => c.kind === 'oc' ? 'oc' : c.work_id || 'other';
function castWorks() {
  const names = new Map((state.workTree || []).map(w => [w.id, w.name])), groups = new Map();
  for (const c of state.chars) {
    const k = workKey(c);
    if (!groups.has(k)) groups.set(k, {key: k, name: k === 'oc' ? '原创角色' : k === 'other' ? '其他' : c.work_name || names.get(k) || '未命名作品', chars: [], images: 0, recent: 0});
    const g = groups.get(k); g.chars.push(c); g.images += c.count; g.recent += recentOf(c);
  }
  for (const g of groups.values()) g.chars.sort((a, b) => b.count - a.count);
  return [...groups.values()].sort((a, b) => b.images - a.images);
}
const cover = (c, size = 'thumb') => c?.cover ? `<img src="/image?id=${encodeURIComponent(c.cover)}&size=${size}" alt="" loading="lazy" decoding="async">` : '';
const avatar = (c, px) => `<i class="cast-av" style="width:${px}px;height:${px}px">${cover(c)}</i>`;
function poster(c, sub, rank = 0, big = false) {
  const r = recentOf(c), skins = (c.variants || []).filter(v => v.count).length;
  return `<a class="cast-poster" href="${charHash(c.id)}" title="${esc(nameTip(c))}"><span class="img">${cover(c, big ? 'medium' : 'thumb')}</span>${rank ? `<span class="rank">${rank}</span>` : ''}${r ? `<span class="pill ${rank ? 'r' : 'l'}"><i></i>+${number(r)}</span>` : ''}${!rank && skins ? `<span class="pill r">${skins} 个皮肤</span>` : ''}<span class="cap"><b>${c.kind === 'oc' ? '<em>原创</em>' : ''}${esc(c.name)}</b><small>${sub}</small></span></a>`;
}
const fan = chars => `<span class="cast-fan">${[1, 2, 0].map(i => `<i>${cover(chars[i])}</i>`).join('')}</span>`;
function tiers(list, showWork) {
  const big = list.filter(c => c.count >= 20), mid = list.filter(c => c.count >= 5 && c.count < 20), small = list.filter(c => c.count < 5);
  const sub = c => (showWork && c.work_name ? esc(c.work_name) + ' · ' : '') + number(c.count) + ' 张';
  return (big.length ? `<section class="cast-sec"><h3>主力<small>20 张以上 · ${number(big.length)} 位</small></h3><div class="cast-posters">${big.map(c => poster(c, sub(c))).join('')}</div></section>` : '') +
    (mid.length ? `<section class="cast-sec"><h3>常见<small>5–19 张 · ${number(mid.length)} 位</small></h3><div class="cast-roster">${mid.map(c => `<a class="cast-rs" href="${charHash(c.id)}" title="${esc(nameTip(c))}">${avatar(c, 72)}<b>${esc(c.name)}</b><small>${showWork && c.work_name ? esc(c.work_name) + ' · ' : ''}${c.count} 张</small></a>`).join('')}</div></section>` : '') +
    (small.length ? `<section class="cast-sec"><h3>偶尔<small>不到 5 张 · ${number(small.length)} 位</small></h3><div class="cast-chips">${small.map(c => `<a class="cast-chip" href="${charHash(c.id)}" title="${esc(nameTip(c))}">${avatar(c, 22)}${esc(c.name)}<small>${c.count}</small></a>`).join('')}</div></section>` : '');
}
function castTabs(works) {
  const recent = state.chars.filter(c => recentOf(c)).length, top = works.slice(0, 14);
  const current = !['all', 'recent'].includes(cast.tab) && !top.some(w => w.key === cast.tab) ? works.find(w => w.key === cast.tab) : null;
  const tab = (key, label, sub, lead = '') => `<button class="cast-tab${lead ? '' : ' plain'}${cast.tab === key ? ' on' : ''}" data-tab="${esc(key)}">${lead}${esc(label)}${sub ? `<small>${sub}</small>` : ''}</button>`;
  return `<div class="cast-tabs"><div class="cast-tabrow">${tab('all', '总览')}${tab('recent', '最近在收', recent ? number(recent) : '')}${[...(current ? [current] : []), ...top].map(w => tab(w.key, w.name, number(w.images), avatar(w.chars[0], 24))).join('')}</div>
    <button class="cast-allw${cast.pop ? ' on' : ''}" id="castAll" aria-expanded="${cast.pop}">全部作品 ${number(works.length)}<svg viewBox="0 0 20 20" aria-hidden="true"><path d="m6 8 4 4 4-4"/></svg></button>${cast.pop ? castPop(works) : ''}</div>`;
}
function castPop(works) {
  const q = cast.popQ.trim().toLowerCase(), list = works.filter(w => !q || w.name.toLowerCase().includes(q));
  return `<div class="cast-pop" id="castPop"><div class="search-wrap">${searchIcon()}<input type="search" id="castPopQ" placeholder="在 ${number(works.length)} 部作品里找" value="${esc(cast.popQ)}" autocomplete="off"></div>
    <div class="cast-wcols">${list.map(w => `<button class="cast-wrow" data-tab="${esc(w.key)}">${avatar(w.chars[0], 20)}<span>${esc(w.name)}</span><small>${number(w.images)}</small></button>`).join('') || '<p class="muted-note">没有匹配的作品</p>'}</div></div>`;
}
function castPane(works) {
  if (cast.list) return `<div class="cast-head"><div><h2>搜索结果</h2><div class="cast-tags"><span>${number(cast.list.length)} 位角色</span></div></div></div>${cast.list.length ? tiers(cast.list, true) : '<p class="empty">没有匹配的角色</p>'}`;
  if (cast.tab === 'all') {
    const top = [...state.chars].sort((a, b) => b.count - a.count).slice(0, 9);
    const rec = state.chars.filter(c => recentOf(c)).sort((a, b) => recentOf(b) - recentOf(a)), max = rec[0] ? recentOf(rec[0]) : 1;
    return `<section class="cast-sec"><h3>你最常看的<small>按收藏张数</small></h3><div class="cast-board">${top.map((c, i) => poster(c, (c.work_name ? esc(c.work_name) + ' · ' : '') + number(c.count) + ' 张', i + 1, i === 0)).join('')}</div></section>
      ${rec.length ? `<section class="cast-sec"><h3>最近 30 天在收<small>${number(rec.length)} 位角色有新图</small><button class="link-button strong" data-tab="recent">全部 ›</button></h3><div class="cast-recent">${rec.slice(0, 24).map(c => `<a class="cast-rc" href="${charHash(c.id)}" title="${esc(nameTip(c))}"><span class="ring" style="--p:${Math.max(8, Math.round(recentOf(c) / max * 100))}">${avatar(c, 66)}<em>+${number(recentOf(c))}</em></span><b>${esc(c.name)}</b><small>${esc(c.work_name || '')}</small></a>`).join('')}</div></section>` : ''}
      <section class="cast-sec"><h3>作品<small>${number(works.length)} 部 · 按张数</small></h3><div class="cast-works">${works.slice(0, 24).map(w => `<button class="cast-wt" data-tab="${esc(w.key)}">${fan(w.chars)}<b>${esc(w.name)}</b><small>${number(w.chars.length)} 位 · ${number(w.images)} 张${w.recent ? ` · <em>+${number(w.recent)}</em>` : ''}</small></button>`).join('')}</div></section>`;
  }
  if (cast.tab === 'recent') {
    const rec = state.chars.filter(c => recentOf(c)).sort((a, b) => recentOf(b) - recentOf(a));
    return `<div class="cast-head"><div><h2>最近 30 天在收</h2><div class="cast-tags"><span class="hot">${number(rec.length)} 位角色</span><span>按新增张数</span></div></div></div>${rec.length ? `<section class="cast-sec"><div class="cast-posters">${rec.slice(0, 120).map(c => poster(c, (c.work_name ? esc(c.work_name) + ' · ' : '') + '近 30 天 +' + number(recentOf(c)))).join('')}</div></section>` : '<p class="empty">最近 30 天没有新图。</p>'}`;
  }
  const w = works.find(x => x.key === cast.tab);
  if (!w) { cast.tab = 'all'; return castPane(works); }
  const real = !['oc', 'other'].includes(w.key), info = state.index?.works?.find(x => x.id === w.key);
  const manage = real && info ? `<div class="cast-manage">${watchSwitch(info)}<button class="quiet" data-shelf-edit="${esc(w.key)}">编辑作品…</button></div>` : '';
  return `<div class="cast-head">${fan(w.chars)}<div><h2>${esc(w.name)}</h2><div class="cast-tags"><span>${number(w.chars.length)} 位角色</span><span>${number(w.images)} 张</span>${w.recent ? `<span class="hot">近 30 天 +${number(w.recent)}</span>` : ''}</div>${manage}</div>${real ? `<button class="primary" data-work-lib="${esc(w.key)}">在图库里看全部</button>` : ''}</div>${tiers(w.chars, false)}`;
}
const BUCKETS = [['all', '全部', () => true], ['1', '1 张', c => c.count === 1], ['2', '2–4 张', c => c.count >= 2 && c.count < 5], ['5', '5–19 张', c => c.count >= 5 && c.count < 20], ['20', '20–99 张', c => c.count >= 20 && c.count < 100], ['100', '100 张以上', c => c.count >= 100]];
function castList() {
  const base = cast.list || state.chars, test = BUCKETS.find(b => b[0] === cast.bucket)?.[2] || (() => true);
  const key = {name: c => c.name, work: c => c.work_name || '', count: c => c.count, recent: recentOf, last: c => cast.activity.get(c.id)?.last || '', skins: c => (c.variants || []).filter(v => v.count).length}[cast.sort];
  const rows = base.filter(test).sort((a, b) => { const x = key(a), y = key(b); return (x > y ? 1 : x < y ? -1 : 0) * cast.dir; });
  const max = Math.max(1, ...base.map(c => c.count)), shown = rows.slice(0, cast.limit || 300);
  const th = (k, t) => `<th data-sort="${k}" class="${cast.sort === k ? 'on' : ''}" aria-sort="${cast.sort === k ? (cast.dir < 0 ? 'descending' : 'ascending') : 'none'}">${t}${cast.sort === k ? (cast.dir < 0 ? ' ↓' : ' ↑') : ''}</th>`;
  return `<div class="cast-buckets">${BUCKETS.map(([k, t, f]) => `<button data-bucket="${k}" class="${cast.bucket === k ? 'on' : ''}">${t}<small>${number(base.filter(f).length)}</small></button>`).join('')}</div>
    <table class="cast-table"><thead><tr>${th('name', '角色')}${th('work', '作品')}${th('count', '张数')}${th('recent', '近 30 天')}${th('last', '最后收藏')}${th('skins', '皮肤')}</tr></thead><tbody>
    ${shown.map(c => { const a = cast.activity.get(c.id), skins = (c.variants || []).filter(v => v.count).length, orig = c.tag && c.tag !== c.name ? c.tag.replaceAll('_', ' ') : '';
      return `<tr data-char="${esc(c.id)}"><td><div class="cast-who">${avatar(c, 36)}<span><b>${esc(c.name)}</b>${orig ? `<small>${esc(orig)}</small>` : ''}</span></div></td><td>${esc(c.kind === 'oc' ? '原创角色' : c.work_name || '—')}</td><td><div class="cast-bar"><i style="width:${Math.max(4, Math.log(c.count + 1) / Math.log(max + 1) * 110)}px"></i>${number(c.count)}</div></td><td>${a?.recent ? `<span class="cast-plus">+${number(a.recent)}</span>` : '<span class="dim">—</span>'}</td><td class="mono">${esc(a?.last || '')}</td><td>${skins || '<span class="dim">—</span>'}</td></tr>`; }).join('')}
    </tbody></table>${rows.length > shown.length ? `<button class="quiet cast-more" id="castMore">再显示 300 位（共 ${number(rows.length)} 位）</button>` : ''}`;
}
function drawCast() {
  if (state.route.page !== 'characters' || state.route.section) return;
  const works = castWorks(), root = $('charIndex');
  root.className = 'char-index cast ' + cast.mode;
  reconcileSections(root, cast.mode === 'list' ? [['list', `<div class="cast-listview">${castList()}</div>`]]
    : [...(cast.list ? [] : [['tabs', castTabs(works)]]), ['pane', `<div class="cast-pane">${castPane(works)}</div>`]]);
  const foot = (state.tagged || []).length ? `<p class="cast-tagged">不算角色、只当标签：${state.tagged.map(t => `<a href="${charHash(t.id)}">${esc(t.name)}</a><small>${number(t.count)}</small>`).join('')}</p>` : '';
  let el = root.querySelector('.cast-tagged'); if (el) el.remove();
  if (foot) root.insertAdjacentHTML('beforeend', foot);
  if (cast.pop) $('castPopQ')?.focus();
}
$('charIndex').addEventListener('click', e => {
  if (state.route.page !== 'characters') return;
  const t = e.target;
  const tab = t.closest('[data-tab]');
  if (tab) { cast.tab = tab.dataset.tab; cast.pop = false; cast.popQ = ''; drawCast(); $('viewport').scrollTop = 0; $('charIndex').querySelector('.cast-tab.on')?.scrollIntoView({inline: 'center', block: 'nearest'}); return; }
  if (t.closest('#castAll')) { cast.pop = !cast.pop; drawCast(); return; }
  if (cast.pop && !t.closest('#castPop')) { cast.pop = false; drawCast(); return; }
  const lib = t.closest('[data-work-lib]'); if (lib) { go({page: 'library', work: lib.dataset.workLib}); return; }
  const s = t.closest('[data-sort]');
  if (s) { const k = s.dataset.sort; cast.dir = cast.sort === k ? -cast.dir : (k === 'name' || k === 'work' ? 1 : -1); cast.sort = k; drawCast(); return; }
  const b = t.closest('[data-bucket]'); if (b) { cast.bucket = b.dataset.bucket; cast.limit = 300; drawCast(); return; }
  if (t.closest('#castMore')) { cast.limit = (cast.limit || 300) + 300; drawCast(); return; }
  const row = t.closest('tr[data-char]'); if (row) go(charRoute(row.dataset.char));
});
$('charIndex').addEventListener('input', e => { if (e.target.id === 'castPopQ') { cast.popQ = e.target.value; const pos = e.target.selectionStart; drawCast(); const i = $('castPopQ'); i?.setSelectionRange(pos, pos); } });
document.addEventListener('keydown', e => { if (e.key === 'Escape' && cast.pop && state.route.page === 'characters') { cast.pop = false; drawCast(); } });

// ---------- 资料库 · 作品: the work tree, its Danbooru links and the watch list (#198) ----------
// Watching a work means the index check asks Danbooru (work name only) for characters the local index lacks.
const shelf = {q: '', filter: 'all'};
async function renderShelf() {
  const route = state.route, token = state.token;
  $('hint').textContent = '点作品名看它的角色 · 检查关注作品的新角色前会确认联网';
  drawShelf();
  let x; try { x = await api('/api/index'); } catch (e) { if (route === state.route && token === state.token) readFailure($('charIndex'), e); return; }
  if (route !== state.route || token !== state.token) return;
  if (x) state.index = x;
  if (state.route.page === 'characters' && state.route.section === 'works') { renderHeading(); drawShelf(); }
}
// A watch toggle needs a Danbooru work tag the person has checked; the index check searches by that tag.
function watchSwitch(w) {
  const on = state.index?.watch.works.includes(w.id);
  return `<label class="shelf-watch${w.watchable ? '' : ' off'}" title="${w.watchable ? '定期检查 Danbooru 上这部作品还没收录的角色（只发送作品名）' : '先在「编辑作品」里核对 Danbooru 作品 tag，才能关注'}"><input type="checkbox" class="switch" data-watch="${esc(w.id)}" ${on ? 'checked' : ''} ${w.watchable || on ? '' : 'disabled'}><span>关注新角色</span></label>`;
}
function workLink(w) {
  if (w.tag) return `<a class="shelf-link ok" href="https://danbooru.donmai.us/posts?tags=${encodeURIComponent(w.tag)}" target="_blank" rel="noreferrer noopener" title="在 Danbooru 查看">${esc(w.tag)}</a>`;
  if (w.status === 'pending') return '<span class="shelf-link warn">链接待确认</span>';
  return '<span class="shelf-link">未关联 Danbooru</span>';
}
function drawShelf() {
  if (state.route.page !== 'characters' || state.route.section !== 'works') return;
  const root = $('charIndex'), x = state.index;
  root.className = 'char-index shelf';
  if (!x?.works) { root.innerHTML = '<p class="muted-note shelf-empty">正在读取作品…</p>'; return; }
  const watched = new Set(x.watch.works), missing = x.check?.missing || [], oldTags = x.watch.pending_tags || [];
  const fresh = w => w.tag ? missing.filter(r => r.copyright === w.tag).length : 0;
  const people = new Map();
  for (const c of state.chars) if (c.work_id) people.set(c.work_id, (people.get(c.work_id) || 0) + 1);
  // Depth-first under each parent, bigger works first; a child whose parent is gone shows at the top level.
  const ids = new Set(x.works.map(w => w.id)), kids = new Map();
  for (const w of x.works) { const p = ids.has(w.parent_id) ? w.parent_id : ''; if (!kids.has(p)) kids.set(p, []); kids.get(p).push(w); }
  for (const list of kids.values()) list.sort((a, b) => (b.count || 0) - (a.count || 0) || a.name.localeCompare(b.name, 'zh'));
  const rows = [], walk = (p, depth) => { for (const w of kids.get(p) || []) { rows.push([w, depth]); walk(w.id, depth + 1); } };
  walk('', 0);
  const q = shelf.q.trim().toLowerCase();
  const tests = {all: () => true, watched: w => watched.has(w.id), pending: w => w.status === 'pending'};
  const shown = rows.filter(([w]) => tests[shelf.filter](w) && (!q || w.name.toLowerCase().includes(q) || (w.tag || '').toLowerCase().includes(q)));
  const pendingN = x.works.filter(tests.pending).length;
  const chip = (k, t, n) => `<button class="chip-filter${shelf.filter === k ? ' on' : ''}" data-shelf-filter="${k}">${t}${n ? ` <span>${number(n)}</span>` : ''}</button>`;
  const todo = oldTags.length ? `<details class="shelf-todo" data-fold-key="shelfTodo"${prefs.get('settingsOpen', {}).shelfTodo ? ' open' : ''}><summary><b>待处理</b><span>${number(oldTags.length)} 个以前关注的 Danbooru 作品还没对应到本地作品，对应后才会检查它们的新角色</span></summary>
      ${oldTags.map(t => `<div class="shelf-todo-row"><span class="mono">${esc(t)}</span><span class="muted-note">${esc(WORK_NAMES[t] || '')}</span><div class="spacer"></div><button class="quiet small" data-shelf-adopt="${esc(t)}">对应到作品…</button></div>`).join('')}</details>` : '';
  const found = missing.length ? `<div class="shelf-found"><span class="local-dot busy"></span><span>上次检查发现 <b>${number(missing.length)}</b> 个还没收录的角色</span><div class="spacer"></div><button class="primary" data-index-add>查看并添加…</button></div>` : '';
  const row = ([w, depth]) => {
    const n = fresh(w), chars = people.get(w.id) || 0;
    return `<div class="shelf-row" style="--depth:${depth}">
      <button class="shelf-name" data-shelf-open="${esc(w.id)}" title="看这部作品的角色"><b>${esc(w.name)}</b>${w.qualifier ? `<small>${esc(w.qualifier)}</small>` : ''}</button>
      ${workLink(w)}
      <span class="shelf-num">${chars ? `${number(chars)} 位角色 · ` : ''}${number(w.count || 0)} 张</span>
      ${n ? `<button class="shelf-new" data-index-add="${esc(w.tag)}" title="Danbooru 上有、本地索引还没有的角色">+${number(n)} 个新角色</button>` : '<span></span>'}
      ${watchSwitch(w)}
      <button class="quiet small" data-shelf-edit="${esc(w.id)}">编辑…</button></div>`;
  };
  root.innerHTML = `${todo}${found}
    <div class="shelf-bar"><div class="chips-row">${chip('all', '全部', x.works.length)}${chip('watched', '已关注', watched.size)}${pendingN ? chip('pending', '链接待确认', pendingN) : ''}</div><div class="spacer"></div>
      <span class="watch-add"><input id="watchAdd" value="${esc(watchDraft)}" placeholder="关注 Danbooru 上的作品…" spellcheck="false" autocomplete="off" role="combobox" aria-expanded="false" aria-controls="watchMenu"><div id="watchMenu" class="tag-popover watch-menu" role="listbox" hidden></div></span></div>
    <div class="shelf-list">${shown.map(row).join('') || `<p class="muted-note shelf-empty">${x.works.length ? '没有符合条件的作品' : '还没有作品。给角色填写所属作品，或点右上角「新建作品」。'}</p>`}</div>`;
  wireWatchAdd(x, adoptWatch); wireFolds(root);
}
async function saveWatch(works) {
  try { state.index = await post('/api/index', {action: 'watch', works, min_posts: state.index.watch.min_posts}); }
  catch (e) { toast(e.message); }
  renderIndexBody(); renderHeading();
}
// From the Danbooru search: an existing local work with that tag, or one the person picks or creates for it.
async function adoptWatch(r) {
  const x = state.index;
  try {
    let id = r.id || x.works.find(w => w.tag === r.tag)?.id;
    if (!id) {
      const chosen = await chooseWork(`Danbooru 的 ${r.tag} 对应哪个本地作品？`, x.works, '', '新建作品');
      if (chosen === null) return;
      id = (await post('/api/works', chosen ? {action: 'link', id: chosen, tag: r.tag} : {action: 'create', name: r.name || r.tag, tag: r.tag})).id;
      state.index = await api('/api/index');
    }
    watchDraft = '';
    if (!state.index.watch.works.includes(id)) await saveWatch([...state.index.watch.works, id]); else renderIndexBody();
  } catch (e) { toast(e.message); }
}
$('charIndex').addEventListener('click', async e => {
  if (state.route.page !== 'characters') return;
  const t = e.target, works = state.index?.works || [];
  const f = t.closest('[data-shelf-filter]'); if (f) { shelf.filter = f.dataset.shelfFilter; drawShelf(); return; }
  const open = t.closest('[data-shelf-open]'); if (open) { cast.tab = open.dataset.shelfOpen; go({page: 'characters'}); return; }
  const edit = t.closest('[data-shelf-edit]'); if (edit) { const w = works.find(x => x.id === edit.dataset.shelfEdit); if (w) workDialog(w); return; }
  const adopt = t.closest('[data-shelf-adopt]'); if (adopt) { adoptWatch({tag: adopt.dataset.shelfAdopt, name: WORK_NAMES[adopt.dataset.shelfAdopt]}); return; }
  const add = t.closest('[data-index-add]'); if (add) openIndexAdd(add.dataset.indexAdd || '');
});
$('charIndex').addEventListener('change', e => {
  const b = e.target.closest('[data-watch]'); if (!b || !state.index) return;
  const id = b.dataset.watch, list = state.index.watch.works.filter(w => w !== id);
  saveWatch(b.checked ? [...list, id] : list);
});
async function createWork() {
  const w = await workDialog(null);
  if (w) toast(`已新建作品「${w.name}」`);
}
// One dialog for a new work and for editing one: name, parent, and the Danbooru tag the person has checked.
function workDialog(w) {
  const works = state.index?.works || [];
  return new Promise(resolve => {
    const d = document.createElement('dialog'); d.className = 'palette profile-dialog work-dialog';
    const parents = works.filter(x => !w || x.id !== w.id);
    d.innerHTML = `<form method="dialog"><div class="dialog-top"><h2>${w ? '编辑作品' : '新建作品'}</h2><button type="button" class="icon" data-close aria-label="关闭">×</button></div>
      <div class="form-grid">
        <label for="wdName">名称</label><input id="wdName" name="title" value="${esc(w?.name || '')}" maxlength="120" required autocomplete="off">
        <label for="wdParent">上级作品</label><select id="wdParent" name="parent"><option value="">无（顶层）</option>${parents.map(x => `<option value="${esc(x.id)}" ${w?.parent_id === x.id ? 'selected' : ''}>${esc(x.name)}</option>`).join('')}</select>
        <label for="wdTag">Danbooru tag</label><input id="wdTag" name="tag" value="${esc(w?.tag || '')}" placeholder="例如 blue_archive" spellcheck="false" autocomplete="off">
      </div>
      <p class="muted-note">${w?.status === 'pending' ? '链接还没确认：核对过就填上 tag；留空保存表示不关联。' : '只填核对过的 tag，不会按名字自动关联。关联后才能关注这部作品的新角色。'}</p>
      <div class="dialog-actions"><button value="cancel" class="quiet">取消</button><button value="ok" class="primary">${w ? '保存' : '新建'}</button></div></form>`;
    d.querySelector('[data-close]').onclick = () => d.close();
    d.onclose = async () => {
      const data = new FormData(d.querySelector('form')), ok = d.returnValue === 'ok'; d.remove();
      if (!ok) { resolve(null); return; }
      const name = String(data.get('title')).trim(), parent = data.get('parent'), tag = String(data.get('tag')).trim();
      try {
        let id = w?.id;
        if (!id) id = (await post('/api/works', tag ? {action: 'create', name, tag} : {action: 'create', name})).id;
        if (w || parent) await post('/api/works', {action: 'edit', id, name, parent, tag});
        state.index = await api('/api/index'); state.workTree = await api('/api/works').catch(e => { error(readError(e)); return state.workTree; });
        await refreshMeta().catch(e => error(readError(e)));
        renderIndexBody(); renderHeading();
        resolve({id, name});
      } catch (err) { toast(err.message); resolve(null); }
    };
    document.body.append(d); d.showModal(); d.querySelector('#wdName').focus();
  });
}
// Characters the last index check found on Danbooru for watched works; adding one downloads ~20 public reference pictures.
function openIndexAdd(tag = '') {
  const token = state.token, x = state.index, list = (x?.check?.missing || []).filter(r => !tag || r.copyright === tag);
  if (!list.length) return;
  const d = document.createElement('dialog'); d.className = 'palette profile-dialog index-add';
  d.innerHTML = `<form method="dialog"><div class="dialog-top"><h2>添加新角色</h2><button type="button" class="icon" data-close aria-label="关闭">×</button></div>
    <p class="muted-note">这些角色在 Danbooru 上有、本地索引还没有。添加时每个角色下载约 20 张公开参考图（约 ${x.mb_per_character} MB），在本机算好特征后重新扫描图库。点 tag 可以先去 Danbooru 看看。</p>
    <div class="index-add-list"><table class="settings-table index-table"><tr><th><input type="checkbox" id="iaAll" aria-label="全选" checked></th><th>角色 tag</th><th>作品</th><th>D 站图数</th></tr>
      ${list.map(r => `<tr><td><input type="checkbox" data-pick="${esc(r.short_tag)}" ${indexSkip.has(r.short_tag) ? '' : 'checked'}></td><td><a class="link" href="https://danbooru.donmai.us/posts?tags=${encodeURIComponent(r.tag)}" target="_blank" rel="noreferrer noopener">${esc(r.tag)}</a></td><td>${esc(x.works.find(w => w.tag === r.copyright)?.name || workName(r.copyright))}</td><td>${number(r.post_count)}</td></tr>`).join('')}</table></div>
    <div class="dialog-actions"><span class="muted-note" id="iaNote"></span><div class="spacer"></div><button value="cancel" class="quiet">取消</button><button value="ok" class="primary" id="iaApply"></button></div></form>`;
  const q = s => d.querySelector(s), picks = () => [...d.querySelectorAll('[data-pick]')];
  const note = () => {
    const n = picks().filter(b => b.checked).length, mb = n * x.mb_per_character;
    q('#iaApply').textContent = `添加 ${number(n)} 个（约 ${mb >= 10 ? Math.round(mb) : mb.toFixed(1)} MB）`; q('#iaApply').disabled = !n;
    q('#iaNote').textContent = mb > 500 ? '下载量超过 500 MB，网络不方便时可以分几次添加' : '';
    indexSkip = new Set([...indexSkip, ...picks().filter(b => !b.checked).map(b => b.dataset.pick)].filter(t => !picks().some(b => b.checked && b.dataset.pick === t)));
  };
  for (const b of picks()) b.onchange = note;
  q('#iaAll').onchange = e => { for (const b of picks()) b.checked = e.target.checked; note(); };
  q('[data-close]').onclick = () => d.close();
  d.onclose = async () => {
    const tags = picks().filter(b => b.checked).map(b => b.dataset.pick), ok = d.returnValue === 'ok'; d.remove();
    if (!ok || !tags.length || token !== state.token) return;
    try { state.index = await post('/api/index', {action: 'apply', tags, allow_network: true}); toast(`开始添加 ${number(tags.length)} 个角色，进度在任务面板里`); pollIndex(); renderIndexBody(); }
    catch (e) { toast(e.message); }
  };
  document.body.append(d); note(); d.showModal();
}

// ---------- keyboard ----------
document.addEventListener('keydown', e => {
  if (document.querySelector('dialog[open]')) return;
  const mod = e.metaKey || e.ctrlKey, key = e.key.toLowerCase();
  if (mod && key === 'k') { e.preventDefault(); jumpToCharacter(); return; }
  if (mod && (key === '=' || key === '+')) { e.preventDefault(); if (state.viewer >= 0) zoomView(1.4, innerWidth / 2, innerHeight / 2); else stepColumns(-1); return; }
  if (mod && key === '-') { e.preventDefault(); if (state.viewer >= 0) zoomView(1 / 1.4, innerWidth / 2, innerHeight / 2); else stepColumns(1); return; }
  if (e.target.closest('input,textarea,select,[contenteditable]')) { if (e.key === 'Escape') e.target.blur(); return; }
  if (mod && key === 'z') { e.preventDefault(); undo(); return; }
  if (mod && key === 'a' && !$('grid').hidden) { e.preventDefault(); selectAll(); return; }
  if (e.key === 'Escape') { if (state.viewer >= 0) closeViewer(); else if (state.focused) closeInspector(); else { state.selecting = false; clearSelection(); } return; }
  if (key === 'r' && !mod && !panelPage(state.route.page)) { e.preventDefault(); toggleFit(); return; }
  if (key === 'h' && !mod) { e.preventDefault(); setNsfw(state.nsfw === 'hide' ? '' : 'hide'); return; }
  if (e.key === '/' && $('search')) { e.preventDefault(); $('search').focus(); return; }
  if ((key === 'y' || key === 'n') && !mod && !state.busy && state.route.page === 'audit' && selCount() && state.viewer < 0) { e.preventDefault(); answerAudit(key === 'y'); return; }
  if ((key === 'y' || key === 'n') && !mod && !state.busy && state.question && [state.focused, state.viewer >= 0 && state.viewerItem?.uuid].includes(state.question.ident)) { e.preventDefault(); answerAsk(key === 'y' ? 'yes' : 'no'); return; }
  if (state.busy || $('grid').hidden) return;
  if (e.key === ' ') { e.preventDefault(); if (state.viewer >= 0) closeViewer(); else openViewer(state.cursor >= 0 ? state.cursor : state.focused ? indexOf(state.focused) : 0); return; }
  if (['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(e.key)) {
    e.preventDefault();
    const from = state.cursor < 0 ? 0 : state.cursor;
    const delta = e.key === 'ArrowLeft' ? -1 : e.key === 'ArrowRight' ? 1 : verticalStep(from, e.key === 'ArrowUp' ? -1 : 1) - from || (e.key === 'ArrowUp' ? -state.cols : state.cols);
    if (state.viewer >= 0 || state.focused) { if (Math.abs(delta) === 1) step(delta); else if (state.viewer < 0) step(delta); return; }
    state.cursor = Math.max(0, Math.min(state.total - 1, (state.cursor < 0 ? 0 : state.cursor + delta)));
    if (e.shiftKey && state.items[state.cursor]) { setSelected(state.items[state.cursor].uuid, true); renderSelection(); }
    if (!state.items[state.cursor]) loadPage(Math.floor(state.cursor / PAGE) * PAGE);
    ensureVisible(state.cursor); return;
  }
  if (e.key === 'Enter' && state.cursor >= 0 && state.viewer < 0) { e.preventDefault(); const item = state.items[state.cursor]; if (!item) return; if (cleanup() || state.selecting) { setSelected(item.uuid, !isSelected(item.uuid)); state.anchor = state.cursor; renderSelection(); } else inspect(item.uuid); return; }
  const n = selCount(), page = state.route.char ? 'character' : state.route.page;
  if (mod || e.altKey && !/^[1-9]$/.test(e.key)) return;
  if (['c', 'a', 't', 's', 'g'].includes(key) || /^[1-9]$/.test(e.key)) e.preventDefault(); // keep the letter out of the palette input
  if (e.key === 'Backspace' || e.key === 'Delete') { e.preventDefault(); if (n) runBarAction(page === 'character' ? 'remove' : browsing() ? 'notlib' : ''); else singleAction('remove'); return; }
  if (key === 'c') { if (n) runBarAction(page === 'character' ? 'swap' : 'assign'); else singleAction(page === 'character' ? 'swap' : 'also'); return; }
  if (key === 'a' && !mod) { if (n) runBarAction(page === 'character' ? 'also' : 'assign'); else singleAction('also'); return; }
  if (key === 't' && page === 'character' && n) { runBarAction('variant'); return; }
  if (key === 'g') { if (n) runBarAction('tag'); else singleAction('tag'); return; }
  if (key === 'x' && !mod) { const act = page === 'excluded' ? 'include' : 'exclude'; if (n) runBarAction(act); else singleAction(act); return; }
  if (key === 's' && (page === 'unmatched' || page === 'group')) { if (n) runBarAction('skip'); else singleAction('skip'); return; }
  if (/^[1-9]$/.test(e.key) && !n && state.detail && (state.focused || state.viewer >= 0)) { pickAlternative(state.detail, state.detail.uuid, Number(e.key) - 1, e.altKey); return; }
});

// ---------- view options (视图选项) ----------
// Everything here is optional detail: the defaults stay quiet, and each switch is saved per browser.
const VIEW_DEFAULTS = {captions: true, marks: true, hints: true, railCount: false, footer: false, chart: true, same: true, meta: true, homeSize: 1, hero: true, people: false, ask: true};
const viewOpts = {...VIEW_DEFAULTS, ...prefs.get('view', {})};
function applyViewClasses() {
  const b = document.body.classList;
  b.toggle('hide-marks', !viewOpts.marks); b.toggle('show-rail-count', viewOpts.railCount); b.toggle('hide-footer', !viewOpts.footer);
}
const viewSwitch = (key, label, hint = '') => `<button class="view-opt" data-view="${key}" role="switch" aria-checked="${viewOpts[key]}"><span>${label}${hint ? `<small>${hint}</small>` : ''}</span><i class="view-sw${viewOpts[key] ? ' on' : ''}"></i></button>`;
function renderViewPop() {
  const grid = !panelPage(state.route.page);
  $('viewPopover').innerHTML = (state.route.page === 'home' ? `<h6>首页</h6><div class="view-group">
      <div class="view-row"><span>缩略图</span><div class="segmented" id="homeSizeSeg">${[[0, '小'], [1, '中'], [2, '大']].map(([v, t]) => `<button data-hsize="${v}" class="${viewOpts.homeSize === v ? 'on' : ''}">${t}</button>`).join('')}</div></div>
      ${viewSwitch('hero', '放大每组主图')}
      ${viewSwitch('people', '分组显示角色数')}
      ${viewSwitch('marks', '图上显示副本数')}
    </div>` : '') + (grid ? `<h6>图片网格</h6><div class="view-group">
      <label class="view-row"><span>缩略图大小</span><input type="range" id="zoom" min="0" max="100" step="1" value="${sizeToSlider(state.size)}" aria-label="缩略图大小"></label>
      ${viewSwitch('captions', '图片下方显示角色名', '浏览页、格子够大时')}
      ${viewSwitch('marks', '图上显示副本数与皮肤')}
      ${viewSwitch('hints', '未识别的图上显示「可能是」', '点一下就设为该角色')}
    </div>` : '') + `<h6>侧栏与提示</h6><div class="view-group">
      ${viewSwitch('railCount', '侧栏显示数量')}
      ${viewSwitch('footer', '底部状态栏', '已加载张数与操作提示')}
    </div><h6>详情面板</h6><div class="view-group">
      ${viewSwitch('chart', '收藏曲线', '这个角色每月收了多少张')}
      ${viewSwitch('same', '同一角色的其它图')}
      ${viewSwitch('meta', '文件信息与画面标签')}
      ${viewSwitch('ask', '偶尔问一句「是 TA 吗？」', '详情和首页；回答会用来学习认人')}
    </div><div class="view-foot"><span>设置会自动保存</span><button class="link-button strong" id="viewReset">恢复默认</button></div>`;
  $('zoom')?.addEventListener('input', e => requestZoom(() => setSize(sliderToSize(Number(e.target.value)), undefined, undefined, true)));
  $('viewReset').onclick = () => { Object.assign(viewOpts, VIEW_DEFAULTS); prefs.set('view', viewOpts); afterViewChange(true); renderViewPop(); if (state.route.page === 'home') drawHome(); };
}
function afterViewChange(relayout) {
  applyViewClasses();
  if (relayout && !panelPage(state.route.page)) { layout(); renderGrid(true); }
}
function toggleViewPop(open = $('viewPopover').hidden) {
  $('viewPopover').hidden = !open; $('viewButton').classList.toggle('on', open); $('viewButton').setAttribute('aria-expanded', open);
  if (open) renderViewPop();
}
$('viewButton').onclick = e => { e.stopPropagation(); toggleViewPop(); };
$('viewPopover').addEventListener('click', e => {
  e.stopPropagation();
  const hs = e.target.closest('[data-hsize]'); if (hs) { viewOpts.homeSize = Number(hs.dataset.hsize); prefs.set('view', viewOpts); for (const x of $('homeSizeSeg').children) x.classList.toggle('on', x === hs); drawHome(); return; }
  const b = e.target.closest('[data-view]'); if (!b) return;
  const key = b.dataset.view; viewOpts[key] = !viewOpts[key]; prefs.set('view', viewOpts);
  b.setAttribute('aria-checked', viewOpts[key]); b.querySelector('.view-sw').classList.toggle('on', viewOpts[key]);
  afterViewChange(key === 'captions' || key === 'hints');
  if (['chart', 'same', 'meta'].includes(key) && state.focused && !$('inspector').hidden) inspect(state.focused, true);
  if (['hero', 'people', 'marks'].includes(key) && state.route.page === 'home') drawHome();
  if (key === 'ask') { if (state.focused && !$('inspector').hidden) inspect(state.focused, true); if (state.route.page === 'home') renderHome(); }
});
document.addEventListener('click', () => { if (!$('viewPopover').hidden) toggleViewPop(false); });
document.addEventListener('keydown', e => { if (e.key === 'Escape' && !$('viewPopover').hidden) { toggleViewPop(false); e.stopImmediatePropagation(); } }, true);
applyViewClasses();

// ---------- NSFW filter ----------
// One click cycles 显示 → 隐藏 → 仅 NSFW; H toggles hiding. NSFW = WD rating questionable/explicit.
// The R18 plate: hollow = everything shows, struck through = hidden, filled = R18 only.
const NSFW_TIP = {'': 'R18：显示中 · 点击隐藏（H）', hide: 'R18：已隐藏 · 点击只看 R18', only: 'R18：只看 R18 · 点击恢复全部'};
const R18_ICON = '<svg class="r18" viewBox="0 0 34 20" aria-hidden="true"><rect class="plate" x="1.2" y="1.2" width="31.6" height="17.6" rx="5.5"/><text x="17" y="14.1" text-anchor="middle">R18</text><path class="cut" d="M5 17.5 29 2.5"/><path class="slash" d="M5 17.5 29 2.5"/></svg>';
function renderNsfw() {
  const b = $('nsfwButton'); if (!b.firstChild) b.innerHTML = R18_ICON;
  b.className = 'nsfw-toggle s-' + (state.nsfw || 'all'); b.title = NSFW_TIP[state.nsfw]; b.setAttribute('aria-label', NSFW_TIP[state.nsfw]);
}
function setNsfw(mode) {
  state.nsfw = mode; prefs.set('nsfw', mode); renderNsfw(); state.selecting = false; clearSelection();
  toast({'': '已显示全部内容', hide: '已隐藏 R18', only: '只看 R18'}[mode]);
  if (!panelPage(state.route.page)) reload(true);
  else if (state.route.page === 'identity') renderIdentity();
  else if (state.route.page === 'home') renderHome();
  else if (state.route.page === 'groups') renderGroups();
  else if (state.route.page === 'similar') renderSimilar();
  else if (state.route.page === 'characters') { cast.activityKey = ''; loadActivity(); }
  refreshMeta().catch(e => error(readError(e)));
}
$('nsfwButton').onclick = () => setNsfw({'': 'hide', hide: 'only', only: ''}[state.nsfw]);
renderNsfw();

// ---------- 首页: newest images grouped by the day they were added ----------
const WEEK = '日一二三四五六';
const dayLabel = day => { const [y, m, d] = day.split('-').map(Number); const t = new Date(y, m - 1, d), now = new Date();
  const diff = Math.round((new Date(now.getFullYear(), now.getMonth(), now.getDate()) - t) / 864e5);
  return diff === 0 ? '今天' : diff === 1 ? '昨天' : `${y !== now.getFullYear() ? y + ' 年 ' : ''}${m} 月 ${d} 日`; };
const weekday = day => { const [y, m, d] = day.split('-').map(Number); return '周' + WEEK[new Date(y, m - 1, d).getDay()]; };
const home = {days: [], next: '', gen: 0, loading: false, ask: null, askSkip: [], askOff: new Set()};
let homeObserver = null, homeResize = null, homeWidth = 0;
async function renderHome(keep = false) {
  $('grid').hidden = true; $('empty').hidden = true; $('charIndex').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = true; $('homePage').hidden = false;
  $('selectionBar').hidden = true; $('resultCount').textContent = ''; $('hint').textContent = '点图片看详情 · 点角色名只看 TA';
  home.loading = false; homeObserver?.disconnect(); dayLoading.clear();
  const gen = ++home.gen, scroll = keep ? $('viewport').scrollTop : 0;
  if (!keep) { home.days = []; home.next = ''; $('viewport').scrollTop = 0; $('homePage').innerHTML = '<div class="loading-line">读取中…</div>'; }
  try {
    const r = await api('/api/home?' + new URLSearchParams({nsfw: state.nsfw, days: Math.min(20, Math.max(5, home.days.length))}));
    if (gen !== home.gen || state.route.page !== 'home') return;
    home.days = r.days; home.next = r.next;
    if (!keep) { home.askSkip = []; home.askOff = new Set(); home.ask = await nextAsk(); if (gen !== home.gen || state.route.page !== 'home') return; }
    drawHome();
    if (keep) $('viewport').scrollTop = scroll;
  } catch (e) { if (gen === home.gen && state.route.page === 'home') $('homePage').innerHTML = `<p class="empty">${esc(readError(e))}</p>`; }
}
async function moreHome() {
  if (home.loading || !home.next || state.route.page !== 'home') return;
  home.loading = true; const gen = home.gen; let appended = false;
  $('homeSentinel').textContent = '正在读取更早的图…';
  try {
    const r = await api('/api/home?' + new URLSearchParams({nsfw: state.nsfw, before: home.next}));
    if (gen !== home.gen || state.route.page !== 'home') return;
    const known = new Set(home.days.map(g => g.day)), days = r.days.filter(g => !known.has(g.day) && known.add(g.day));
    home.days.push(...days); home.next = r.next;
    const sentinel = $('homeSentinel');
    sentinel.insertAdjacentHTML('beforebegin', days.map(g => homeDay(g, homeWidth)).join(''));
    for (const day of days) for (const el of $('homePage').querySelectorAll(`.home-day[data-day="${day.day}"] .home-strip`)) syncStrip(el);
    state.items.push(...days.flatMap(g => g.items)); state.total = state.items.length;
    if (!home.next) { homeObserver?.disconnect(); sentinel.textContent = '已经到底了'; }
    appended = true;
  } catch (e) { if (gen === home.gen && state.route.page === 'home') {
    $('homeSentinel').innerHTML = '<button class="home-retry" data-home-retry>读取失败，重试</button>'; toast(e.message);
  } }
  finally {
    if (gen === home.gen) {
      home.loading = false;
      // Re-observe after every append so a sentinel still within the margin loads again.
      if (appended && home.next && state.route.page === 'home' && $('homeSentinel')) { homeObserver?.unobserve($('homeSentinel')); homeObserver?.observe($('homeSentinel')); }
    }
  }
}
const ratio = x => x.width && x.height ? Math.min(2.4, Math.max(.4, x.width / x.height)) : .75;
function homeTile(x, style, big = false) {
  const names = x.memberships.map(m => m.name), ids = x.memberships.map(m => m.id).join(' ');
  const badge = viewOpts.marks && x.dup ? `<span class="home-badge" title="共 ${x.dup.size} 份副本">×${x.dup.size}</span>` : '';
  return `<button class="home-j${state.focused === x.uuid ? ' on' : ''}" data-id="${esc(x.uuid)}" data-chars="${esc(ids)}" style="${style}"><img src="/image?id=${encodeURIComponent(x.uuid)}&size=${big ? 'medium' : 'thumb'}" alt="" loading="lazy" decoding="async" draggable="false">${badge}${big ? '' : `<span class="home-ov">${names.length ? names.map(esc).join(' · ') : x.unknown ? '已跳过' : '未识别'}</span>`}</button>`;
}
function homeRows(items, width, target) {
  const G = 6, rows = []; let row = [], sum = 0;
  for (const x of items) { row.push(x); sum += ratio(x); if (sum * target + G * (row.length - 1) >= width) { rows.push(row); row = []; sum = 0; } }
  if (row.length) {
    const fill = row.reduce((a, x) => a + ratio(x), 0) * target / width;
    if (fill < .7 && rows.length) { const both = rows.pop().concat(row), tot = both.reduce((a, x) => a + ratio(x), 0); let acc = 0, cut = 0;
      for (; cut < both.length - 1; cut++) { acc += ratio(both[cut]); if (acc >= tot / 2) break; }
      rows.push(both.slice(0, cut + 1), both.slice(cut + 1)); }
    else rows.push(row);
  }
  return rows.filter(r => r.length).map(r => { const s = r.reduce((a, x) => a + ratio(x), 0), fit = (width - G * (r.length - 1)) / s, h = r.length > 2 ? fit : Math.min(target * 1.8, fit);
    return `<div class="home-row" style="height:${Math.round(h)}px">${r.map(x => homeTile(x, `flex:${ratio(x)} 1 0`)).join('')}</div>`; }).join('');
}
function heroTile(hero, w, h) {
  const top = hero.memberships[0];
  return `<button class="home-j home-hero${state.focused === hero.uuid ? ' on' : ''}" data-id="${esc(hero.uuid)}" data-chars="${esc(hero.memberships.map(m => m.id).join(' '))}" style="width:${w}px;height:${h}px"><img src="/image?id=${encodeURIComponent(hero.uuid)}&size=medium" alt="" loading="lazy" decoding="async" draggable="false">${top ? `<span class="home-cap"><b>${esc(top.name)}</b>${top.tag && top.tag !== top.name ? `<small>${esc(top.tag.replaceAll('_', ' '))}</small>` : ''}</span>` : ''}</button>`;
}
// A day that does not fit the width scrolls sideways: the hero, then two rows filled shortest-first.
function stripRows(items, rowH) {
  const rows = [[], []], w = [0, 0];
  for (const x of items) { const i = w[0] <= w[1] ? 0 : 1; rows[i].push(x); w[i] += ratio(x) * rowH + 6; }
  return rows.map(r => `<div class="home-row strip-row" style="height:${rowH}px">${r.map(x => homeTile(x, `flex:none;width:${Math.round(ratio(x) * rowH)}px`)).join('')}</div>`).join('');
}
function homeSizes() {
  const i = viewOpts.homeSize in [0, 1, 2] ? viewOpts.homeSize : 1;
  return {target: [150, 190, 240][i], heroW: [200, 240, 280][i]};
}
function homeDay(g, width) {
  const {target, heroW} = homeSizes();
  const hero = viewOpts.hero && g.items.length >= 4 && width > heroW * 2.4 ? g.items.find(x => x.uuid === g.hero) || g.items[0] : null;
  const rest = hero ? g.items.filter(x => x !== hero) : g.items;
  const rowH = hero ? (Math.round(heroW / .72) - 6) / 2 : target;
  // Natural width of the two-row strip; when it overflows (or more pictures are waiting) the day scrolls sideways.
  const natural = (hero ? heroW + 6 : 0) + rest.reduce((a, x) => a + ratio(x) * rowH + 6, 0) / 2;
  const strip = g.count > g.items.length || natural > width * 1.5;
  let body;
  if (strip) {
    const heroH = Math.round(rowH * 2 + 6);
    body = `<div class="home-strip-wrap"><button class="strip-nav prev" data-strip="-1" aria-label="向左" hidden>${INSP_ICONS.prev}</button>
      <div class="home-strip" data-day="${g.day}">${hero ? heroTile(hero, heroW, heroH) : ''}<div class="strip-rows">${stripRows(rest, Math.round(rowH))}</div>
        ${g.count > g.items.length ? `<a class="strip-more" href="${hashFor({page: 'library', day: g.day})}" style="height:${heroH}px"><b>${number(g.count - g.items.length)}</b>张没显示<small>继续往右滑自动加载 · 或在图库里看全部</small></a>` : ''}</div>
      <button class="strip-nav next" data-strip="1" aria-label="向右">${INSP_ICONS.next}</button></div>`;
  } else if (hero) {
    const heroH = Math.round(heroW / .72), sideW = width - heroW - 6, side = [[], []];
    let r = 0, sum = 0, used = 0;
    for (const x of rest) { if (r > 1) break; side[r].push(x); sum += ratio(x); used++; if (sum * rowH + 6 * (side[r].length - 1) >= sideW) { r++; sum = 0; } }
    body = `<div class="home-top">${heroTile(hero, heroW, heroH)}<div class="home-side">${side.filter(x => x.length).map(rw => `<div class="home-row">${rw.map(x => homeTile(x, `flex:${ratio(x)} 1 0`)).join('')}</div>`).join('')}</div></div>${homeRows(rest.slice(used), width, target)}`;
  } else body = homeRows(g.items, width, target);
  const more = g.people - g.characters.length;
  const chips = g.characters.map(c => { const cover = state.charMap.get(c.id)?.cover;
    return `<a class="home-cc" href="${charHash(c.id)}" data-char="${esc(c.id)}" title="在图库里只看 TA">${cover ? `<img src="/image?id=${encodeURIComponent(cover)}&size=thumb" alt="" loading="lazy">` : '<i></i>'}${esc(c.name)}<small>${number(c.count)}</small></a>`; }).join('');
  return `<section class="home-day" data-day="${g.day}"><div class="home-dh"><b>${esc(dayLabel(g.day))}</b><span>${weekday(g.day)} · ${number(g.count)} 张${viewOpts.people && g.people ? ` · ${number(g.people)} 位角色` : ''}</span><a class="home-all" href="${hashFor({page: 'library', day: g.day})}">在图库里看 ›</a></div>
    ${chips ? `<div class="home-chars">${chips}${more > 0 ? `<span class="home-more">另外 ${number(more)} 位</span>` : ''}</div>` : ''}${body}</section>`;
}
// Sideways scrolling: arrows show only when there is more that way; nearing the end loads the next pictures of that day.
function syncStrip(el) {
  const wrap = el.parentElement, max = el.scrollWidth - el.clientWidth - 2;
  wrap.querySelector('.prev').hidden = el.scrollLeft <= 2; wrap.querySelector('.next').hidden = el.scrollLeft >= max;
  if (el.scrollLeft + el.clientWidth > el.scrollWidth - 600) moreOfDay(el.dataset.day);
}
const dayLoading = new Set();
async function moreOfDay(day) {
  const g = home.days.find(x => x.day === day); if (!g || g.items.length >= g.count || dayLoading.has(day)) return;
  dayLoading.add(day); const gen = home.gen;
  try {
    const r = await api('/api/assets?' + new URLSearchParams({day, nsfw: state.nsfw, order: 'newest', offset: g.items.length, limit: 30, count: 0}));
    if (gen !== home.gen || state.route.page !== 'home') return;
    const known = new Set(g.items.map(x => x.uuid)); g.items.push(...r.items.filter(x => !known.has(x.uuid)));
    if (!r.items.length) g.count = g.items.length;
    const el = $('homePage').querySelector(`.home-strip[data-day="${day}"]`), left = el?.scrollLeft || 0;
    const sec = el?.closest('.home-day'); if (!sec) return;
    const tmp = document.createElement('div'); tmp.innerHTML = homeDay(g, homeWidth); sec.replaceWith(tmp.firstElementChild);
    const fresh = $('homePage').querySelector(`.home-strip[data-day="${day}"]`); if (fresh) { fresh.scrollLeft = left; syncStrip(fresh); }
    state.items = home.days.flatMap(x => x.items); state.total = state.items.length;
  } catch (e) { if (gen === home.gen && state.route.page === 'home') toast(e.message); } finally { if (gen === home.gen) dayLoading.delete(day); }
}
function drawHome() {
  if (state.route.page !== 'home') return;
  const page = $('homePage'); homeWidth = page.clientWidth;
  if (!home.days.length) { page.innerHTML = '<p class="empty">没有符合当前 R18 设置的图片。</p>'; state.items = []; state.total = 0; return; }
  page.innerHTML = home.days.map((g, i) => homeDay(g, homeWidth) + (i === 0 ? askCardHTML() : '')).join('') + `<div class="home-more-line" id="homeSentinel">${home.next ? '正在读取更早的图…' : '已经到底了'}</div>`;
  state.items = home.days.flatMap(g => g.items); state.total = state.items.length;
  for (const el of page.querySelectorAll('.home-strip')) syncStrip(el);
  homeObserver?.disconnect();
  if (home.next) { homeObserver = new IntersectionObserver(es => { if (es.some(e => e.isIntersecting)) moreHome(); }, {root: $('viewport'), rootMargin: '600px'}); homeObserver.observe($('homeSentinel')); }
}
function markHomeFocus() { if (state.route.page === 'home') for (const b of $('homePage').querySelectorAll('.home-j')) b.classList.toggle('on', b.dataset.id === state.focused); }
// 首页「认一认」: one character, a few of its machine-recognised images; tap the wrong ones, then confirm the rest.
async function nextAsk() {
  if (!viewOpts.ask) return null;
  try { return (await api('/api/ask?' + new URLSearchParams({nsfw: state.nsfw, skip: home.askSkip.join(',')}))).batch; } catch { return null; }
}
function askCardHTML() {
  const b = home.ask; if (!b || !viewOpts.ask || !b.items.length) return '';
  const off = home.askOff.size, why = b.kind === 'hint' ? '和你确认过的 TA 很像 · 点对了就能认出更多' : b.kind === 'seed' ? `再确认 ${Math.max(1, 5 - b.confirmed)} 张就能学会认 TA` : '抽查一下认得准不准';
  return `<section class="ask-card" id="askCard"><div class="ask-head"><b>认一认</b><span>这些是 <a href="${charHash(b.entity)}">${esc(b.name)}</a> 吗？</span><small>${why}</small></div>
    <div class="ask-row">${b.items.map(x => `<button class="ask-img${home.askOff.has(x.uuid) ? ' off' : ''}" data-askimg="${esc(x.uuid)}" title="${home.askOff.has(x.uuid) ? '点一下：改回「是」' : '点一下：不是 TA'}" style="aspect-ratio:${ratio(x)}"><img src="/image?id=${encodeURIComponent(x.uuid)}&size=thumb" alt="" loading="lazy"><i aria-hidden="true">✕</i></button>`).join('')}</div>
    <div class="ask-foot"><span>${off ? `${off} 张不是 TA` : '点掉不是 TA 的'}</span><button class="btn" data-askcard="later">以后再说</button><button class="btn pri" data-askcard="ok">${off === b.items.length ? '都不是' : off ? `其余 ${b.items.length - off} 张是` : '都对'}</button></div></section>`;
}
function redrawAskCard() { const el = $('askCard'), html = askCardHTML(); if (!el) return drawHome(); if (!html) { el.remove(); return; } const t = document.createElement('div'); t.innerHTML = html; el.replaceWith(t.firstElementChild); }
async function submitAskCard(how) {
  const b = home.ask; if (!b || state.busy) return;
  const ids = b.items.map(x => x.uuid), no = ids.filter(u => home.askOff.has(u)), yes = ids.filter(u => !home.askOff.has(u));
  home.askSkip.push(b.entity); home.askOff = new Set();
  if (how === 'later') {
    home.ask = null; redrawAskCard();
    post('/api/ask/answer', {entity: b.entity, kind: b.kind, later: ids}).catch(e => toast(e.message));
    return;
  }
  home.ask = await nextAsk();   // the next character is ready before the page redraws
  await mutate('/api/ask/answer', {entity: b.entity, kind: b.kind, yes, no}, false);
}
$('homePage').addEventListener('click', e => {
  if (e.target.closest('[data-home-retry]')) { moreHome(); return; }
  const ai = e.target.closest('[data-askimg]');
  if (ai) { const u = ai.dataset.askimg; home.askOff.has(u) ? home.askOff.delete(u) : home.askOff.add(u); redrawAskCard(); return; }
  const ac = e.target.closest('[data-askcard]'); if (ac) { submitAskCard(ac.dataset.askcard); return; }
  const n = e.target.closest('[data-strip]');
  if (n) { const el = n.parentElement.querySelector('.home-strip'); el.scrollBy({left: Number(n.dataset.strip) * el.clientWidth * .8, behavior: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth'}); return; }
  const b = e.target.closest('.home-j'); if (b) inspect(b.dataset.id);
});
$('homePage').addEventListener('scroll', e => { if (e.target.classList?.contains('home-strip')) syncStrip(e.target); }, true);
$('homePage').addEventListener('dblclick', e => { const b = e.target.closest('.home-j'); if (b) openViewer(indexOf(b.dataset.id)); });
$('homePage').addEventListener('mouseover', e => {
  const c = e.target.closest('.home-cc'), day = c?.closest('.home-day');
  for (const d of $('homePage').querySelectorAll('.home-day.focus')) if (d !== day) d.classList.remove('focus');
  if (!c) return;
  day.classList.add('focus'); for (const b of day.querySelectorAll('.home-j')) b.classList.toggle('hit', b.dataset.chars.split(' ').includes(c.dataset.char));
});
$('homePage').addEventListener('mouseleave', () => { for (const d of $('homePage').querySelectorAll('.home-day.focus')) d.classList.remove('focus'); });
new ResizeObserver(() => { if (state.route.page !== 'home' || !home.days.length || Math.abs($('homePage').clientWidth - homeWidth) < 8) return; clearTimeout(homeResize); homeResize = setTimeout(drawHome, 120); }).observe($('homePage'));

// ---------- look-alike groups of unmatched images ----------
// 'look': close centres; 'trait': farther, but the same distinctive WD traits (weaker hint).
const nearText = g => g.near_kind === 'trait' ? `外观相似：第 ${g.near} 组` : `可能和第 ${g.near} 组是同一人`;
// 未识别, group by group: CCIP puts images of one person together reliably (99%) even when it cannot name them (45%).
// A group whose leads mostly agree is asked「是 TA 吗？」as a whole; the others get 设为… / 原创角色 / 跳过.
// Tap a picture to leave it out of the group's answer. Answered groups leave the page in place (no reload jump).
const groups = {list: [], shown: 40, off: new Map(), revision: 0};
async function renderGroups(keep = false) {
  if (state.route.page !== 'groups') return;
  $('grid').hidden = true; $('empty').hidden = true; $('charIndex').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = false;
  closeInspector(); $('selectionBar').hidden = true; $('resultCount').textContent = ''; $('hint').textContent = '点一下图片：不算在这组里 · 组头的按钮对整组生效，都可以撤销';
  const scroll = $('viewport').scrollTop, revision = ++groups.revision, nsfw = state.nsfw;
  let result; try { result = await api('/api/clusters?nsfw=' + state.nsfw); } catch (e) { if (state.route.page === 'groups' && revision === groups.revision && nsfw === state.nsfw) readFailure($('groupsPage'), e); return; }
  const {clusters, learner} = result;
  if (state.route.page !== 'groups' || revision !== groups.revision || nsfw !== state.nsfw) return;
  // Groups that can be confirmed in one click come first (biggest first), then the ones still to be named.
  groups.list = [...clusters].sort((a, b) => !!b.lead - !!a.lead || b.size - a.size || a.cid - b.cid); groups.learner = learner;
  if (!keep) { groups.shown = 40; groups.off = new Map(); }
  drawGroups();
  if (keep) $('viewport').scrollTop = scroll;
}
function drawGroups() {
  const list = groups.list, busy = ['learning', 'grouping', 'waiting'].includes(groups.learner?.status), images = list.reduce((s, g) => s + g.size, 0);
  const rest = Math.max(0, (state.pools.unmatched || 0) - images), named = list.filter(g => g.lead).length;
  if (!$('groupsPage').querySelector('.gsecs')) $('groupsPage').innerHTML = '<div class="groups-controls"></div><div class="gsecs"></div><div class="groups-more"></div>';
  $('groupsPage').querySelector('.groups-controls').innerHTML = `<div class="groups-bar"><span class="muted-note">${number(list.length)} 组 · 共 ${number(images)} 张${named ? ` · ${number(named)} 组能直接确认是谁` : ''}${list[0] ? ' · 分组于 ' + esc(stamp(list[0].built_at)) : ''}${busy ? ' · ' + esc(groups.learner.status === 'waiting' ? groups.learner.message || '' : '正在学习…') : ''}</span><div class="spacer"></div>${rest ? `<a class="quiet link" href="#/unmatched">没成组的 ${number(rest)} 张 →</a>` : ''}<button class="quiet" id="regroup" title="用当前的未识别图重新分组">重新分组</button></div>`;
  reconcileSections($('groupsPage').querySelector('.gsecs'), list.slice(0, groups.shown).map(g => [String(g.cid), groupSection(g)]), '没有成组的未识别图。可以点「重新分组」。');
  $('groupsPage').querySelector('.groups-more').innerHTML = `${list.length > groups.shown ? `<div class="gsec-more"><button class="quiet" id="moreGroups">再显示 ${number(Math.min(40, list.length - groups.shown))} 组</button></div>` : ''}`;
  $('regroup').onclick = async () => { try { await post('/api/regroup', {}); toast('正在重新分组…'); setTimeout(() => renderGroups(), 3000); } catch (e) { toast(e.message); } };
  $('moreGroups')?.addEventListener('click', () => { groups.shown += 40; drawGroups(); });
}
function groupSection(g) {
  const off = groups.off.get(g.cid) || new Set(), n = g.members.length - off.size, max = 16;
  const head = g.lead
    ? `<span class="gsec-q">是 <a href="${charHash(g.lead.id)}">${esc(g.lead.name)}</a> 吗？<small>${number(g.lead.count)} 张的线索都指向 TA</small></span>
       <button class="btn pri" data-gact="yes"${n ? '' : ' disabled'}>${off.size ? `其余 ${number(n)} 张是` : '都是'} ✓</button><button class="btn" data-gact="no">都不是 ✗</button><button class="quiet" data-gact="assign">是别人…</button>`
    : `<span class="gsec-q muted">还不知道是谁</span><button class="btn pri" data-gact="assign"${n ? '' : ' disabled'}>设为…</button><button class="quiet" data-gact="oc" title="画师自己的原创角色">原创角色</button><button class="quiet" data-gact="skip">跳过</button>`;
  const thumbs = g.members.slice(0, max).map(u => `<button class="gsec-img${off.has(u) ? ' off' : ''}" data-gimg="${esc(u)}" title="${off.has(u) ? '点一下：算回这组' : '点一下：不算在这组里'}"><img src="/image?id=${encodeURIComponent(u)}&size=thumb" loading="lazy" alt=""><i aria-hidden="true">✕</i></button>`).join('');
  return `<section class="gsec" data-cid="${g.cid}"><div class="gsec-head"><b>第 ${g.cid} 组</b><span class="muted-note">${number(g.size)} 张${g.traits?.length ? ' · ' + g.traits.slice(0, 2).map(esc).join(' · ') : ''}</span>${head}<a class="quiet link gsec-open" href="#/g/${g.cid}">逐张看 →</a></div>
    <div class="gsec-imgs">${thumbs}${g.members.length > max ? `<a class="gsec-rest" href="#/g/${g.cid}">+${number(g.members.length - max)}</a>` : ''}</div>${g.near ? `<div class="group-near">${nearText(g)}</div>` : ''}</section>`;
}
$('groupsPage').addEventListener('click', async e => {
  const sec = e.target.closest('.gsec'); if (!sec) return;
  const g = groups.list.find(x => x.cid === Number(sec.dataset.cid)); if (!g) return;
  const img = e.target.closest('[data-gimg]');
  if (img) {
    const off = groups.off.get(g.cid) || new Set(), u = img.dataset.gimg;
    off.has(u) ? off.delete(u) : off.add(u); groups.off.set(g.cid, off);
    drawGroups(); return;
  }
  const act = e.target.closest('[data-gact]')?.dataset.gact; if (!act) return;
  const off = groups.off.get(g.cid) || new Set(), ids = g.members.filter(u => !off.has(u)), n = ids.length;
  const done = async ok => { if (!ok) return; groups.list = groups.list.filter(x => x !== g); groups.off.delete(g.cid); drawGroups(); };
  if (act === 'yes') done(await mutate('/api/change', {ids, action: 'confirmed', entity: g.lead.id}, false));
  else if (act === 'no') { if (await mutate('/api/change', {ids, action: 'rejected', entity: g.lead.id}, false)) renderGroups(true); }
  else if (act === 'skip') done(await mutate('/api/change', {ids, action: 'unknown'}, false));
  else if (act === 'oc') { await markOc({ids}, false); renderGroups(true); }
  else if (act === 'assign') openPicker({title: `这 ${number(n)} 张是…`, sentence: x => `把这 ${number(n)} 张设为 ${x.name}`, suggest: {ids}, exclude: g.lead ? [g.lead.id] : [],
    pick: async x => done(await mutate('/api/change', {ids, action: 'confirmed', entity: x.id}, false))});
});

// ---------- look-alike images across the library (web/similar.py) ----------
const FOLD_OPTIONS = [['identical', '完全相同', '像素一样的不同文件'], ['duplicate', '重复', '同一张图缩放或重新压缩过'], ['subtle', '细微差分', '只差表情、签名、小物件（< 2%）'],
  ['edit', '局部编辑', '加字水印、调色、色块或马赛克'], ['crop', '局部重复', '一张是另一张的裁剪'], ['variant', '差分', '同一张画改了局部或重新上色']];
const RELATION_KINDS = [['', '全部'], ['duplicate', '重复'], ['subtle', '细微差分'], ['variant', '差分'], ['crop', '局部重复'], ['edit', '局部编辑']];
const RELATION_NAMES = {identical: '完全相同', duplicate: '重复', subtle: '细微差分', variant: '差分', crop: '局部重复', edit: '局部编辑'};
const SIMILAR_ACTIVE = ['reading', 'pairing', 'comparing', 'waiting'];
let similarTimer = 0;
function similarLine(s) {
  if (s.status === 'reading') return `正在读图算签名 ${number(s.done || 0)}/${number(s.total || 0)}`;
  if (s.status === 'pairing') return '正在找候选…';
  if (s.status === 'comparing') return `正在逐对比较 ${number(s.done || 0)}/${number(s.total || 0)} · 已找到 ${number(s.found || 0)} 对`;
  if (s.status === 'waiting') return s.message || '输入有变化，稍后自动重试';
  if (s.status === 'error') return '出错：' + (s.message || '');
  if (s.status === 'off') return '本次启动关闭了相似检测';
  return s.at ? `上次检查 ${stamp(s.at)}` : '';
}
async function renderSimilar() {
  if (state.route.page !== 'similar') return;
  clearTimeout(similarTimer);
  $('grid').hidden = true; $('empty').hidden = true; $('charIndex').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = false;
  closeInspector(); $('selectionBar').hidden = true; $('resultCount').textContent = ''; $('hint').textContent = '点一组进去看';
  const kind = state.route.kind || '';
  let result; try { result = await api('/api/similar?' + new URLSearchParams({kind, nsfw: state.nsfw})); } catch (e) { if (state.route.page === 'similar' && kind === (state.route.kind || '')) readFailure($('groupsPage'), e); return; }
  const {groups, state: s} = result;
  if (state.route.page !== 'similar') return;
  const busy = SIMILAR_ACTIVE.includes(s.status);
  $('groupsPage').innerHTML = `<div class="similar-status"><span class="local-dot${busy ? ' busy' : ''}"></span><span class="muted-note">${number(groups.length)} 组 · 共 ${number(groups.reduce((n, g) => n + g.size, 0))} 张${similarLine(s) ? ' · ' + esc(similarLine(s)) : ''}</span></div>
    <div class="groups-bar"><div class="segmented">${RELATION_KINDS.map(([k, t]) => `<button class="${k === kind ? 'on' : ''}" data-kind="${k}">${t}</button>`).join('')}</div>
      <div class="spacer"></div><button class="quiet" id="similarScan" ${busy || s.status === 'off' ? 'disabled' : ''} title="给新加入的图算签名，比较还没比较过的候选">重新检查</button></div>
    ${busy && s.total ? `<div class="progress"><span style="width:${Math.round((s.done || 0) / s.total * 100)}%"></span></div>` : ''}
    <div class="group-grid">${groups.map(g => `<a class="group-card" href="#/s/${g.gid}"><div class="group-covers">${g.covers.map(u => `<img src="/image?id=${encodeURIComponent(u)}&size=thumb" loading="lazy" alt="">`).join('')}</div>
      <div class="group-meta"><b>第 ${g.gid} 组</b><span>${number(g.size)} 张</span></div>
      <div class="group-traits">${Object.entries(g.kinds).sort((a, b) => b[1] - a[1]).map(([k, n]) => `<span>${RELATION_NAMES[k] || k}${n > 1 ? ' ×' + n : ''}</span>`).join('')}</div></a>`).join('')
      || `<p class="empty">${busy ? '还在检查，找到的组会陆续出现。' : '没有找到相似的图。'}</p>`}</div>`;
  for (const b of $('groupsPage').querySelectorAll('[data-kind]')) b.onclick = () => go({page: 'similar', kind: b.dataset.kind});
  $('similarScan').onclick = async () => { try { await post('/api/similar', {}); setTimeout(renderSimilar, 800); } catch (e) { toast(e.message); } };
  if (busy) similarTimer = setTimeout(renderSimilar, 3000);
}

// ---------- identity review: pairs of look-alike characters (web/identity.py, Store.identity_queue) ----------
// One click decides a pair; every decision is an undoable relation, raw records never move.
const IDENTITY_PAGE = 12;
const identity = {items: [], total: 0, offset: 0, busy: new Set()};
function identitySuggestion(x) {
  const h = x.hint; if (!h) return '';
  if (h.kind === 'duplicate_of') return 'same';
  if (h.kind === 'form_of') return h.subject && h.subject === x.b.tag ? 'b_of_a' : h.subject && h.subject === x.a.tag ? 'a_of_b' : '';
  return '';
}
function identityEvidence(x) {
  const h = x.hint, name = t => t === x.a.tag ? x.a.name : t === x.b.tag ? x.b.name : t.replaceAll('_', ' ');
  const parts = [];
  if (h?.kind === 'duplicate_of') parts.push(`${esc(h.source || '外部资料')}：同一个 tag 的别名`);
  else if (h?.kind === 'form_of') parts.push(`${esc(h.source || '外部资料')}：「${esc(name(h.subject))}」是「${esc(name(h.object))}」的一种形态`);
  if (x.distance !== null && x.distance !== undefined) parts.push(`参考图外观距离 ${x.distance.toFixed(3)}${x.distance <= 0.01 ? '（几乎分不开）' : ''}`);
  return parts.join(' · ') || '参考图外观很接近';
}
function identitySide(s, label) {
  return `<div class="pair-side"><div class="pair-name"><span class="pair-label">${label}</span><a href="${charHash(s.id)}" title="${esc(s.tag || s.name)}">${esc(s.name)}</a><small>${number(s.count)} 张</small></div>
    <div class="pair-thumbs">${s.samples.map(u => `<img src="/image?id=${encodeURIComponent(u)}&size=thumb" loading="lazy" alt="">`).join('') || '<span class="muted-note">没有可显示的图</span>'}</div></div>`;
}
function identityCard(x, i) {
  const s = identitySuggestion(x), btn = (act, text, title) => `<button class="${s === act ? 'primary' : 'quiet'}" data-act="${act}" data-i="${i}" title="${esc(title)}">${text}</button>`;
  return `<article class="pair-card" data-i="${i}">
    <div class="pair-body">${identitySide(x.a, 'A')}${identitySide(x.b, 'B')}</div>
    <p class="pair-evidence">${identityEvidence(x)}</p>
    <div class="pair-actions">
      ${btn('same', '同一角色', '其实是同一个人：合并显示，图和人工结论都不搬动，可撤销')}
      ${btn('b_of_a', 'B 是 A 的一种形态', 'B 挂到 A 下面作为子标签（服装、异格、另一形态）')}
      ${btn('a_of_b', 'A 是 B 的一种形态', 'A 挂到 B 下面作为子标签')}
      ${btn('different', '不同角色', '只是长得像、经常同框，或是 TA 的吉祥物、宠物：记下来，以后不再问')}
      <span class="spacer"></span>${btn('skip', '暂不判断', '先放一边，队列里不再出现（可撤销）')}
    </div></article>`;
}
async function renderIdentity(more = false) {
  if (state.route.page !== 'identity') return;
  $('grid').hidden = true; $('empty').hidden = true; $('charIndex').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = false;
  closeInspector(); $('selectionBar').hidden = true; $('hint').textContent = '点角色名看全部图 · 高亮的是外部资料建议的做法';
  if (!more) { identity.items = []; identity.offset = 0; }
  let page;
  try { page = await api('/api/identity/queue?' + new URLSearchParams({limit: IDENTITY_PAGE, offset: identity.items.length, nsfw: state.nsfw})); }
  catch (e) { $('groupsPage').innerHTML = `<p class="empty">${esc(readError(e))}</p>`; return; }
  if (state.route.page !== 'identity') return;
  identity.items.push(...page.items); identity.total = page.total; state.identityCount = page.total; navCount('countIdentity', page.total);
  $('resultCount').textContent = `${number(page.total)} 对待判断`;
  renderHeading();
  $('groupsPage').innerHTML = `<div class="pair-list">${identity.items.map(identityCard).join('') || '<p class="empty">没有需要判断的角色了。</p>'}</div>
    ${identity.items.length < identity.total ? '<div class="pair-more"><button class="quiet" id="identityMore">再显示一些</button></div>' : ''}`;
  $('identityMore')?.addEventListener('click', () => renderIdentity(true));
  for (const b of $('groupsPage').querySelectorAll('[data-act]')) b.onclick = () => decideIdentity(Number(b.dataset.i), b.dataset.act);
}
async function decideIdentity(i, act) {
  const x = identity.items[i]; if (!x || identity.busy.has(i) || state.busy) return;
  const card = $('groupsPage').querySelector(`.pair-card[data-i="${i}"]`);
  identity.busy.add(i); card?.classList.add('working');
  // A duplicate is shown under the side with more images.
  const [small, big] = x.a.count <= x.b.count ? [x.a, x.b] : [x.b, x.a];
  const body = act === 'skip' ? null : {status: 'accepted', reason: '身份整理',
    ...{same: {a: small.id, b: big.id, kind: 'duplicate_of'}, b_of_a: {a: x.b.id, b: x.a.id, kind: 'form_of'},
        a_of_b: {a: x.a.id, b: x.b.id, kind: 'form_of'}, different: {a: x.a.id, b: x.b.id, kind: 'different_from'}}[act]};
  try {
    const r = act === 'skip' ? await post('/api/identity/skip', {a: x.a.id, b: x.b.id}) : await post('/api/identity/decision', body);
    setUndo(r.undo); toast(act === 'skip' ? '已暂不判断' : {same: `已把 ${small.name} 并入 ${big.name}`, b_of_a: `${x.b.name} 已作为 ${x.a.name} 的一种形态`,
      a_of_b: `${x.a.name} 已作为 ${x.b.name} 的一种形态`, different: `已记下 ${x.a.name} 和 ${x.b.name} 是不同角色`}[act], true);
    card?.classList.add('leaving');
    await new Promise(done => setTimeout(done, 170));
    await renderIdentity();
    refreshMeta().catch(e => error(readError(e)));
  } catch (e) { toast(e.message); card?.classList.remove('working'); }
  finally { identity.busy.delete(i); }
}

// ---------- Photos sync & settings ----------
const SYNC_ACTIVE = ['scanning', 'recognising', 'importing'];
const bytes = n => n >= 1e9 ? (n / 1e9).toFixed(1) + ' GB' : n >= 1e6 ? (n / 1e6).toFixed(1) + ' MB' : Math.round(n / 1e3) + ' KB';
const stamp = t => (t || '').replace('T', ' ').slice(5, 16);
// Unix seconds → local "MM-DD hh:mm".
const localStamp = sec => { const d = new Date(sec * 1000), p = n => String(n).padStart(2, '0'); return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`; };
const syncBar = (s, bar) => s.status === 'recognising' && s.stage !== 'preparing' ? bar(s.done, s.total)
  : s.status === 'importing' && s.import_total ? bar(s.import_done, s.import_total) : '';
// Recognition and import are counted apart: done/total are images recognised or given up on,
// import_done/import_total the recognised ones written into the library.
function syncLine(s) {
  if (s.status === 'scanning') return s.scan_total == null ? '正在检查来源…' : `正在检查文件 ${number(s.scanned || 0)}/${number(s.scan_total)}`;
  if (s.status === 'importing') {
    if (!s.import_total) return '正在导入…';
    return s.stage === 'verifying' ? `正在校验识别结果（${number(s.import_total)} 张）…`
      : `正在导入识别结果 ${number(s.import_done || 0)}/${number(s.import_total)}`;
  }
  if (s.status === 'recognising') {
    if (s.stage === 'preparing') return `正在加载识别模型（共 ${number(s.total || 0)} 张）…`;
    const failed = s.failed ? ` · ${number(s.failed)} 张无法识别` : '';
    const left = s.ms_per_image && s.total > s.done ? ` · 约 ${Math.max(1, Math.round((s.total - s.done) * s.ms_per_image / 60000))} 分钟` : '';
    const imported = s.import_done ? ` · 已导入 ${number(s.import_done)}` : '';
    return `正在识别新图片 ${number(s.done || 0)}/${number(s.total || 0)}${failed}${imported}${left}`;
  }
  if (s.status === 'done') return `上次同步：${stamp(s.finished_at)}`;
  if (s.status === 'needs_windows') return '新增图片较多，需使用 Windows 批量识别';
  if (s.status === 'needs_recognition') return `有 ${number(s.total || 0)} 张图等待识别（识别服务未启动）`;
  if (s.status === 'interrupted') return s.message ? `同步已中断（${s.message}），可重新扫描` : '同步已中断，可重新扫描';
  if (s.status === 'error') return '同步失败';
  return '本地图库';
}
let syncTimer = null, lastImported = 0;
async function pollSync() {
  clearTimeout(syncTimer);
  try {
    const {state: s, history} = await api('/api/sync');
    const was = state.sync.status;
    state.sync = s; state.syncHistory = history;
    if ((s.imported || 0) > lastImported || (SYNC_ACTIVE.includes(was) && !SYNC_ACTIVE.includes(s.status))) {
      state.countDirty = true; state.readEpoch++;
      lastImported = s.imported || 0; await refreshMeta().catch(e => error(readError(e)));
      if (!panelPage(state.route.page) && !selCount()) reload();
    }
    if (SYNC_ACTIVE.includes(was) && s.status === 'done' && s.imported) toast(`同步完成：新加入 ${number(s.imported)} 张`);
    renderTasks();
    if (SYNC_ACTIVE.includes(s.status)) syncTimer = setTimeout(pollSync, 2000);
  } catch (e) { syncTimer = setTimeout(pollSync, 10000); }
}
// ---------- new characters for the external index (native/terms/src/app_index.rs) ----------
const WORK_NAMES = {blue_archive: '蔚蓝档案', 'arknights:_endfield': '明日方舟：终末地', wuthering_waves: '鸣潮', 'meitantei_precure!': '名侦探光之美少女',
  genshin_impact: '原神', 'honkai:_star_rail': '崩坏：星穹铁道', honkai_impact_3rd: '崩坏 3', zenless_zone_zero: '绝区零', arknights: '明日方舟'};
const workName = w => state.workNames?.get(w) || WORK_NAMES[w] || work(w);
const INDEX_ACTIVE = ['checking', 'fetching', 'cleaning', 'building', 'scanning', 'importing'];
let indexTimer = 0, indexArmed = null, indexSkip = new Set(), watchDraft = '';
function indexLine(s, check) {
  const n = s.total ? ` ${number(s.done || 0)}/${number(s.total)}` : '';
  return {checking: '正在检查 Danbooru…', fetching: '正在下载参考图' + n, cleaning: '正在处理参考图', building: '正在更新角色特征', scanning: '正在重新扫描图库', importing: '正在导入图库',
    error: '检查失败：' + (s.message || '')}[s.status] || (check ? `上次检查：${stamp(check.checked_at)} · 待添加 ${number(check.missing.length)} 个角色` : '尚未检查');
}
async function renderIndexCard(fresh = true) {
  const route = state.route, token = state.token;
  if (!$('indexCard') || state.route.page !== 'settings') return;
  if (fresh || !state.index) { try { const value = await settingsRequest('/api/index'); if (route !== state.route || token !== state.token) return; state.index = value; } catch (e) { if (route === state.route && token === state.token) readFailure($('indexCard'), e); return; } }
  const card = $('indexCard'), x = state.index; if (!x || !card) return;
  const s = x.state, active = INDEX_ACTIVE.includes(s.status), watched = x.watch.works.length;
  // Which works are watched is decided on each work in 资料库 · 作品; here only how the check runs.
  if (!setSettingsHTML(card, cardHead('检查新角色', `定期在 Danbooru 上查关注的作品有没有本地还不认识的角色，只发送作品名。<span class="hint-more" title="添加角色时每个角色下载约 20 张公开参考图（约 ${x.mb_per_character} MB），在本机算好特征后重新扫描图库。">了解详情</span>`, 'library') + `
    ${taskLink(indexLine(s, x.check), active ? 'busy' : s.status === 'error' ? 'warn' : s.status === 'off' ? 'off' : '')}
    <a class="setting-line set-link" href="#/characters/works"><span>关注的作品<small class="line-hint">在资料库的「作品」里逐部开关</small></span><span>${watched ? `${number(watched)} 部` : '还没有'} ›</span></a>
    <div class="setting-line"><span>只提示图片较多的角色<small class="line-hint">Danbooru 上图片少于这个数的角色不提示</small></span><span class="num-field"><input id="watchMin" type="number" min="20" max="5000" step="10" value="${x.watch.min_posts}"> 张</span></div>
    <p class="settings-note">每次检查前确认联网，只允许本次运行。</p>
    ${x.history.length ? fold('indexHistory', `最近添加的角色（${number(x.history.reduce((n, h) => n + h.characters.length, 0))} 个）`, `<table class="settings-table">${x.history.flatMap(h => h.characters.map(c => `<tr><td>${esc(stamp(h.at))}</td>
      <td>${c.entity && c.status === 'added' ? `<a class="link" href="${charHash(c.entity.id)}">${esc(c.entity.name)}</a>` : esc(c.tag || c.short_tag)}</td>
      <td>${c.status === 'added' ? `匹配 ${number(c.entity?.count ?? c.claims)} 张` : c.status === 'dropped' ? '已移除' : `未添加：${esc(c.reason || '')}`}</td>
      <td>${c.status === 'added' && c.entity ? `<button class="quiet small${indexArmed === c.short_tag ? ' armed' : ''}" data-drop="${esc(c.short_tag)}" ${active ? 'disabled' : ''} title="从索引中移除这个角色：重新扫描后它匹配的图恢复原状，以后检查不再提示">${indexArmed === c.short_tag ? '再点一次确认移除' : '移除'}</button>` : ''}</td></tr>`)).join('')}</table>`) : ''}`, x.works)) return;
  wireFolds(card);
  const act = async body => { try { state.index = await post('/api/index', body); renderIndexCard(false); pollIndex(); } catch (e) { toast(e.message); } };
  $('watchMin').onchange = () => act({action: 'watch', works: x.watch.works, min_posts: Number($('watchMin').value)});
  for (const b of card.querySelectorAll('[data-drop]')) b.onclick = () => {
    if (indexArmed !== b.dataset.drop) { indexArmed = b.dataset.drop; renderIndexCard(false); setTimeout(() => { if (indexArmed === b.dataset.drop) { indexArmed = null; renderIndexCard(false); } }, 4000); return; }
    indexArmed = null; act({action: 'drop', char: b.dataset.drop, reason: '在设置中手动移除'});
  };
}
// Work picker: works your library already has (instant, local names and aliases), then Danbooru
// copyright tags for the typed text (only the text goes out). Picking one yields an exact tag.
function wireWatchAdd(x, add) {
  const token = state.token, input = $('watchAdd'), menu = $('watchMenu'), watched = new Set(x.watch.works);
  let list = [], index = 0, gen = 0, localTimer = 0, remoteTimer = 0;
  const row = (r, i) => `<button class="picker-item watch-option${i === index ? ' on' : ''}" data-i="${i}" role="option" ${watched.has(r.id) || (r.id && !r.watchable) ? 'disabled' : ''}>
    <span class="watch-name">${esc(r.name || workName(r.tag))}</span><small>${esc(r.tag || 'D 站上还没有这个作品')}</small>
    <em>${watched.has(r.id) ? '已关注' : [r.count ? `你有 ${number(r.count)} 张` : '', r.posts ? `D 站 ${number(r.posts)}` : ''].filter(Boolean).join(' · ')}</em></button>`;
  const render = (sections, note = '') => {
    list = sections.flatMap(sec => sec.items); index = Math.max(0, list.findIndex(r => !watched.has(r.id)));
    let i = 0;
    menu.innerHTML = sections.filter(sec => sec.items.length).map(sec => `<div class="palette-label">${sec.label}</div>` + sec.items.map(r => row(r, i++)).join('')).join('') + (note ? `<p class="muted-note watch-note">${note}</p>` : '');
    menu.hidden = !menu.innerHTML; input.setAttribute('aria-expanded', String(!menu.hidden));
  };
  const highlight = () => { for (const b of menu.querySelectorAll('[data-i]')) b.classList.toggle('on', Number(b.dataset.i) === index); menu.querySelector('.on')?.scrollIntoView({block: 'nearest'}); };
  const search = async () => {
    const q = input.value.trim(), g = ++gen;
    let local; try { local = await api('/api/works?' + new URLSearchParams({q})); } catch (e) { if (g === gen && token === state.token && input === $('watchAdd')) render([], esc(readError(e))); return; }
    if (g !== gen || token !== state.token || input !== $('watchAdd')) return;
    if (!q) { render([{label: '你的图库里还没关注的作品', items: local.filter(r => !watched.has(r.id)).slice(0, 10)}]); return; }
    const mine = local.slice(0, 8);
    render([{label: '你的图库里有', items: mine}], '正在查 Danbooru…（只发送你输入的文字）');
    clearTimeout(remoteTimer);
    remoteTimer = setTimeout(async () => {
      const result = await remoteNames('/api/works?' + new URLSearchParams({q, remote: 1})), remote = result.found;
      if (g !== gen || token !== state.token || input !== $('watchAdd')) return;
      const posts = new Map((remote || []).map(r => [r.tag, r.posts]));
      for (const r of mine) if (posts.has(r.tag)) r.posts = posts.get(r.tag);
      const known = new Set(mine.map(r => r.tag));
      const others = (remote || []).filter(r => !known.has(r.tag));
      render([{label: '你的图库里有', items: mine}, {label: 'Danbooru', items: others}],
        remote === null ? esc(remoteNote(result)) + (result.unauthorized ? ' ' + namesAllowButton : '，可以稍后再试') : !mine.length && !others.length ? '没有找到这个作品。可以换个写法，比如英文或日文名' : '');
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
async function pollIndex() {
  clearTimeout(indexTimer);
  const was = state.index?.state.status;
  try {
    const progress = await api('/api/index?progress=1');
    // The full read (works, check results) only when something may have changed; the idle tick stays light.
    state.index = !state.index?.works || (!INDEX_ACTIVE.includes(progress.state?.status) && progress.state?.status !== was)
      ? await settingsRequest('/api/index') : {...state.index, ...progress};
  } catch (e) { indexTimer = setTimeout(pollIndex, 10000); return; }
  const s = state.index.state;
  if (INDEX_ACTIVE.includes(was) && !INDEX_ACTIVE.includes(s.status)) {
    if (s.status === 'done') { state.countDirty = true; state.readEpoch++; toast(s.kind === 'drop' ? `已移除，${number(s.changed)} 张图片已重新识别` : `已添加 ${number(s.added)} 个角色，${number(s.changed)} 张图片的识别结果已更新`); await refreshMeta().catch(e => error(readError(e))); }
    else if (s.status === 'checked') toast(s.found ? `发现 ${number(s.found)} 个可添加的角色，在任务面板里添加` : '未发现新角色');
    else if (s.status === 'error') toast('角色索引更新失败：' + (s.message || ''));
  }
  if (state.route.page === 'settings' && s.status !== was) renderIndexCard(false);
  if (state.route.section === 'works' && s.status !== was) drawShelf();
  renderTasks();
  if (INDEX_ACTIVE.includes(s.status)) indexTimer = setTimeout(pollIndex, 2000);
}

// ---------- display names ----------
// Chinese / Japanese names fetched from Bangumi (text queries only); a configured model API is an opt-in last resort.
let namesTimer = 0;
const NAMES_ACTIVE = ['fetching', 'matching', 'translating', 'running'];
const namesLine = s => s.status === 'off' ? '当前未启用' : NAMES_ACTIVE.includes(s.status) ? `正在获取 ${s.total ? `${number(s.done || 0)} / ${number(s.total)}` : '…'}` : s.status === 'error' ? '获取失败，将在下次启动时重试' : '已是最新';
async function renderNamesCard(fresh = true) {
  const route = state.route, token = state.token;
  if (!$('namesCard') || state.route.page !== 'settings') return;
  if (fresh || !state.names) { try { const value = await settingsRequest('/api/names'); if (route !== state.route || token !== state.token) return; state.names = value; } catch (e) { if (route === state.route && token === state.token) readFailure($('namesCard'), e); return; } }
  // The page may have been re-rendered while waiting: always work on the card that is on screen now.
  const card = $('namesCard'), x = state.names; if (!card) return; if (!x) { card.hidden = true; return; }
  card.hidden = false;
  const s = x.state || {}, active = NAMES_ACTIVE.includes(s.status);
  const row = (t, c) => c ? `<tr><td>${t}</td><td>${number(c.total)}</td><td>${number(c.zh || 0)}</td><td>${number(c.ja || 0)}</td><td>${number(c.readable || 0)}</td></tr>` : '';
  const cc = x.counts?.characters;
  card.innerHTML = cardHead('角色名称', '从 Bangumi 取角色的中文名，只发送 tag 和作品名。没有中文名时显示日文名，再没有就显示英文名；原始 tag 在悬停提示里。', 'library') + `
    ${taskLink(namesLine(s) + (s.error ? '：' + s.error : ''), active ? 'busy' : s.status === 'error' ? 'warn' : s.status === 'off' ? 'off' : '')}
    ${startupSwitch('names', '启动时获取', '只查新增的角色')}
    ${x.counts ? fold('namesStats', `覆盖率${active ? '（完成后更新）' : ''}${cc ? `：${Math.round(cc.zh / Math.max(1, cc.total) * 100)}% 的角色有中文名` : ''}`, `<table class="settings-table"><tr><th></th><th>共</th><th>中文名</th><th>日文名</th><th>英文名</th></tr>${row('角色', x.counts.characters)}${row('作品', x.counts.works)}${row('皮肤', x.counts.skins)}</table>`) : ''}`;
  wireFolds(card);
  renderModelCard();
  if (fresh && active) { clearTimeout(namesTimer); namesTimer = setTimeout(pollNames, 2000); }
}
// Optional OpenAI-compatible model used only to translate names that Bangumi does not have.
const modelHost = m => { try { return new URL(m?.base || 'https://api.deepseek.com').host; } catch (e) { return '—'; } };
function renderModelCard() {
  const card = $('modelCard'), m = state.names?.model_api; if (!card || !m) return;
  if (!setSettingsHTML(card, cardHead('名称翻译（可选）', 'Bangumi 没有中文名时，用兼容 OpenAI 接口的大模型翻译，只发送 tag、日文名和作品名。译名可能不准，悬停提示里会标「机器翻译」。API key 只存在这个图库的数据库里。', 'library') + `
    <div class="form-grid">
      <label for="modelBase">接口地址</label><input id="modelBase" value="${esc(m.base || '')}" placeholder="https://api.deepseek.com" spellcheck="false" autocomplete="off">
      <label for="modelName">模型</label><input id="modelName" value="${esc(m.model || '')}" placeholder="deepseek-chat" spellcheck="false" autocomplete="off">
      <label for="modelKey">API key</label><input id="modelKey" type="password" placeholder="${m.configured ? `已配置（尾号 ${esc(m.hint || '')}），留空则不修改` : '未配置'}" spellcheck="false" autocomplete="off">
    </div>
    <div class="sync-now form-actions"><span class="muted-note">${m.configured ? '已配置' : '未配置，不会使用'}</span><div class="spacer"></div>
      ${m.configured ? '<button class="quiet" id="modelClear">清除 key</button><button class="quiet" id="modelTest">测试连接</button>' : ''}<button class="primary" id="modelSave">保存</button></div>`)) return;
  const save = async (body, done) => { try { state.names = {...state.names, ...await post('/api/names', {action: 'model_api', ...body})}; toast(done); renderSettings(); } catch (e) { toast(e.message); } };
  $('modelSave').onclick = () => {
    const body = {base: $('modelBase').value.trim() || 'https://api.deepseek.com', model: $('modelName').value.trim() || 'deepseek-chat'};
    if ($('modelKey').value.trim()) body.key = $('modelKey').value.trim();
    save(body, '设置已保存');
  };
  for (const id of ['modelBase', 'modelName', 'modelKey']) $(id).onkeydown = e => { if (e.key === 'Enter' && !e.isComposing) $('modelSave').click(); };
  if ($('modelClear')) $('modelClear').onclick = () => save({key: null}, 'API key 已清除');
  if ($('modelTest')) $('modelTest').onclick = async () => {
    $('modelTest').disabled = true;
    try { const r = await post('/api/names', {action: 'model_api_test'}); toast(r.ok === false ? '连接失败：' + (r.error || '未知错误') : '连接成功'); } catch (e) { toast('连接失败：' + e.message); }
    if ($('modelTest')) $('modelTest').disabled = false;
  };
}
async function pollNames() {
  clearTimeout(namesTimer);
  const was = NAMES_ACTIVE.includes(state.names?.state?.status);
  try {
    const progress = await api('/api/names?progress=1');
    // Keep the existing coverage/settings while running; refresh them once on completion.
    state.names = !state.names?.counts || (was && !NAMES_ACTIVE.includes(progress.state?.status)) ? await api('/api/names') : {...state.names, ...progress};
  } catch (e) { namesTimer = setTimeout(pollNames, 10000); return; }
  const now = NAMES_ACTIVE.includes(state.names.state?.status);
  renderNamesCard(false);
  renderTasks();
  if (was && !now) refreshMeta();
  if (now) namesTimer = setTimeout(pollNames, 2000);
}
// Collapsible blocks whose open/closed state survives re-renders and reloads.
const fold = (key, summary, body) => `<details class="more" data-fold-key="${key}"${prefs.get('settingsOpen', {})[key] ? ' open' : ''}><summary>${summary}</summary><div class="more-body">${body}</div></details>`;
function wireFolds(root) {
  for (const d of root.querySelectorAll('details[data-fold-key]')) {
    if (d.dataset.foldWired) continue;
    d.dataset.foldWired = '1';
    d.ontoggle = () => { const o = prefs.get('settingsOpen', {}); if (d.open) o[d.dataset.foldKey] = true; else delete o[d.dataset.foldKey]; prefs.set('settingsOpen', o); };
  }
}
// Coalesce identical in-flight reads; a slow card never delays the local settings shell.
const settingsRequests = new Map(), settingsMarkup = new WeakMap();
let settingsLoading = null;
function settingsRequest(path) {
  if (!settingsRequests.has(path)) settingsRequests.set(path, api(path).finally(() => settingsRequests.delete(path)));
  return settingsRequests.get(path);
}
function loadSettingsData(refresh = false) {
  if (settingsLoading) return settingsLoading;
  const token = state.token;
  const missing = [['settings', '/api/settings'], ['fold', '/api/fold'], ['index', '/api/index'], ['names', '/api/names']]
    .filter(([key]) => refresh || !state[key]);
  if (!missing.length) return;
  settingsLoading = Promise.all(missing.map(async ([key, path]) => {
    try { const value = await settingsRequest(path); if (token !== state.token) return; state[key] = key === 'fold' ? value.kinds : value; }
    catch (e) { if (token !== state.token) return; error(readError(e)); }
    if (state.route.page === 'settings') renderSettings(false);
  })).finally(() => { settingsLoading = null; });
  return settingsLoading;
}
function setSettingsHTML(card, html, version = null) {
  const previous = settingsMarkup.get(card);
  if (previous?.html === html && previous.version === version) return false;
  // Background metadata updates must not erase an in-progress form edit.
  if (card.contains(document.activeElement) && document.activeElement?.matches('input,textarea,select')) return false;
  card.innerHTML = html;
  settingsMarkup.set(card, {html, version});
  return true;
}
function patchSettingsPage(html, section) {
  const page = $('settingsPage');
  // Another section is another page: replace it whole. The same section patches card by card, keeping forms in progress.
  if (page.querySelector('.settings-main')?.dataset.section !== section) {
    page.innerHTML = html;
    for (const card of page.querySelectorAll('.settings-card')) settingsMarkup.set(card, {html: card.innerHTML, version: null});
    $('viewport').scrollTop = 0;
    return;
  }
  const template = document.createElement('template'); template.innerHTML = html;
  for (const card of template.content.querySelectorAll('.settings-card')) {
    // These cards own their rendering, forms and async lifecycle.
    if (OWN_CARDS.includes(card.id)) continue;
    setSettingsHTML($(card.id), card.innerHTML);
  }
}
// One page per question the person brings: how it looks, where pictures come from, who is in them, who drew them,
// what leaves the machine, where data lives. Each page lists the cards it shows (#/settings/<id>).
const SETTINGS_SECTIONS = [
  ['general', '通用', '外观、图片网格与默认排序', ['setDisplay']],
  ['library', '图库', '当前图库、来源、同步与排除', ['setLibraries', 'setSync', 'setExclude', 'setFold']],
  ['characters', '角色资料', '新角色检查、角色名称与翻译', ['indexCard', 'namesCard', 'modelCard']],
  ['artists', '画师', '找回画师与绑定的账号', ['setProvenance', 'setAccounts']],
  ['network', '网络与隐私', '所有会访问网络的功能，以及它们发送什么', ['setNetwork']],
  ['data', '数据', '备份、缓存与存储位置', ['setData']],
  ['about', '关于', '版本、致谢与诊断信息', ['setAbout']]];
const OWN_CARDS = ['setLibraries', 'setProvenance', 'setAccounts', 'indexCard', 'namesCard', 'modelCard', 'setAbout'];
const settingsSection = () => SETTINGS_SECTIONS.find(s => s[0] === state.route.section) || SETTINGS_SECTIONS[0];
// Developer-only entries (test libraries, pasting a Pixiv cookie) show only after opening the page once with ?dev.
const devMode = (() => { try { if (new URLSearchParams(location.search).has('dev')) prefs.set('dev', true); } catch {} return !!prefs.get('dev', false); })();
// Where a setting is kept, so the person knows what follows them across libraries and browsers.
const SCOPE = {browser: ['仅本浏览器', '保存在这个浏览器里，换浏览器或清除网站数据会恢复默认'], library: ['本图库', '每个图库各自保存'], shared: ['所有图库共用', '保存在这台电脑上，所有图库共用']};
const scope = key => `<span class="set-scope" title="${SCOPE[key][1]}">${SCOPE[key][0]}</span>`;
const cardHead = (title, note = '', key = '') => `<h2>${title}${key ? scope(key) : ''}</h2>${note ? `<p class="settings-note">${note}</p>` : ''}`;
// A launch-time switch; --no-* flags still win for the current launch.
function startupSwitch(key, name, hint) {
  const info = state.settings; if (!info?.startup) return '';
  return `<label class="switch-line"><span><b>${name}</b><small>${hint} · 下次启动时生效</small>${(info.forced_off || []).includes(key) ? '<em>这次启动已用命令行参数关闭</em>' : ''}</span><input type="checkbox" class="switch" data-startup="${key}" ${info.startup[key] ? 'checked' : ''}></label>`;
}
const taskSimilar = s => s.status === 'off' ? '这次启动没有开相似检测' : similarLine(s) || '已是最新';
// A status line whose controls live in the task panel.
const taskLink = (line, dot = '') => `<div class="setting-line set-status"><span><span class="local-dot${dot ? ' ' + dot : ''}"></span>${esc(line)}</span><button class="quiet small" data-open-tasks>在任务面板查看</button></div>`;
const CREDITS = [
  ['WD SwinV2 Tagger v3', 'SmilingWolf', '图片标签与角色识别', 'https://huggingface.co/SmilingWolf/wd-swinv2-tagger-v3'],
  ['CCIP', 'DeepGHS', '角色特征比对', 'https://huggingface.co/deepghs/ccip'],
  ['Character Index', 'DeepGHS', '角色索引数据', 'https://huggingface.co/datasets/deepghs/character_index'],
  ['imgutils 检测与分级模型', 'DeepGHS', '人物检测、打码检测、内容分级', 'https://github.com/deepghs/imgutils'],
  ['Anime Censorship Tagger', 'Maltox', '打码类型识别', 'https://huggingface.co/Maltox/anime-censorship-tagger-mnv3-384'],
  ['Danbooru', '', '标签与角色资料', 'https://danbooru.donmai.us'],
  ['Bangumi 番组计划', '', '角色中文名称', 'https://bgm.tv'],
  ['Pillow · NumPy · ONNX Runtime', '', '图像处理与模型推理', 'https://onnxruntime.ai']];
async function renderAbout() {
  const route = state.route, token = state.token;
  if (!state.about) { try { const value = await settingsRequest('/api/about'); if (route !== state.route || token !== state.token) return; state.about = value; } catch (e) { if (route === state.route && token === state.token) readFailure($('setAbout'), e); return; } }
  const card = $('setAbout'), a = state.about; if (!card || !a) return;
  const version = `${a.version}${a.channel ? ' ' + a.channel : ''}`;
  const diag = [['应用', a.name], ['版本', version], ['构建', a.build || '—'], ['数据库架构', a.schema], [a.runtime ? '运行时' : 'Python', a.runtime || a.python], ['系统', a.platform],
    ...Object.entries(a.libraries || {}).map(([k, v]) => [k, v || '未安装'])];
  card.innerHTML = `<div class="about-head"><img src="/favicon.svg" alt="" width="44" height="44"><div><h2 class="about-name">${esc(a.name)}</h2><p class="muted-note">版本 ${esc(version)}${a.build ? ` · 构建 ${esc(a.build)}` : ''}</p></div></div>
    <p class="settings-note">在本机识别、整理收藏的插画和里面的角色。图片和识别数据都只在这台电脑上。</p>
    <div class="about-actions"><button class="quiet" id="aboutKeys">键盘快捷键</button><button class="quiet" id="aboutCopy">复制诊断信息</button></div>
    ${fold('credits', '致谢与第三方组件', `<table class="settings-table credits">${CREDITS.map(([n, by, use, url]) => `<tr><td><a class="link" href="${url}" target="_blank" rel="noreferrer noopener">${esc(n)}</a>${by ? `<small>${esc(by)}</small>` : ''}</td><td>${esc(use)}</td></tr>`).join('')}</table><p class="settings-note net-foot">各组件的许可条款以其项目主页为准。</p>`)}
    ${fold('diagnostics', '诊断信息', `<table class="settings-table">${diag.map(([k, v]) => `<tr><td>${esc(k)}</td><td class="mono">${esc(v)}</td></tr>`).join('')}</table>`)}`;
  wireFolds(card);
  $('aboutKeys').onclick = () => $('help').showModal();
  $('aboutCopy').onclick = async () => {
    try { await navigator.clipboard.writeText(diag.map(([k, v]) => `${k}: ${v}`).join('\n')); toast('诊断信息已复制'); } catch (e) { toast('无法访问剪贴板'); }
  };
}
// Every feature that talks to the network, what it sends, and whether it is on: the one place to check (#200).
function networkCard() {
  const up = state.settings?.startup || {}, p = state.provenance, acct = state.accounts, model = state.names?.model_api;
  const net = state.names?.network, names = !net ? '' : net.granted_at ? `已允许（${esc(net.granted_at)} 起）` : net.this_run ? '本次启动已允许（命令行）' : '未允许，不联网';
  const row = (section, name, to, what, now) => `<tr><td><a class="link-button strong" href="#/settings/${section}">${name}</a></td><td>${to}</td><td>${what}</td><td>${now}</td></tr>`;
  return cardHead('会访问网络的功能', '图片本身和由图片算出的特征，任何时候都不会发出去。下面是全部会联网的功能和它们发送的内容。') +
    `<table class="settings-table net-table"><tr><th>功能</th><th>服务</th><th>发送的内容</th><th>现在</th></tr>
      ${row('characters', '检查新角色', 'Danbooru', '关注作品的名称；添加角色时下载约 20 张公开参考图', '每次运行前确认')}
      ${row('characters', '角色名称', 'Bangumi、Danbooru', 'tag、作品名、日文名', net && !net.authorized ? names : up.names ? '启动时自动（只查新角色）' : '手动')}
      ${row('characters', '名称翻译', model?.configured ? esc(modelHost(model)) : '你配置的模型接口', 'tag、日文名、作品名', model?.configured ? '已配置' : '未配置，不使用')}
      ${row('artists', '找回画师', 'Danbooru（含 IQDB）', '像素哈希和 IQDB 签名（一串文本，不是图片）、画师名和账号', p?.auto_network ? '同步后自动，每次有流量上限' : '每次手动授权')}
      ${row('artists', '读取 Pixiv 关注', 'Pixiv', '用你在 app 里登录的状态读取关注列表（只读）', acct?.pixiv?.bound ? '已登录' : '未登录，不使用')}
      ${row('artists', '查找画师、找 TA 的其他图', 'Danbooru、画师主页', '所选图片的哈希文本；下载画师公开作品的小图，只在本机比对', '只在你点按钮时')}
      ${row('characters', '在 Danbooru 上搜角色和作品', 'Danbooru', '你输入的文字；自建角色的名字（核对能否对上 Danbooru）', net && !net.authorized ? names : '只在你搜索或打开资料库时')}
      ${row('artists', '以图搜图', 'SauceNAO', '—', acct?.saucenao?.bound ? '已保存 key，还没有功能使用它' : '未使用')}
    </table>
    ${net ? `<div class="setting-line"><span><b>名称联网查询</b><small>角色名称和 Danbooru 搜索共用这一项授权，只发送文字 · ${names}</small></span>
      ${net.granted_at ? '<button class="quiet" data-names-network="0">撤销</button>' : '<button class="quiet" data-names-network="1">允许…</button>'}</div>` : ''}
    <p class="settings-note net-foot">同步、图片识别、相似图片检测、学习与分组都只在本机进行。X 的关注通过导入官方数据归档获得，不联网。</p>`;
}
async function renderSettings(fetchMissing = true, refresh = false) {
  if (state.route.page !== 'settings') return;
  $('grid').hidden = true; $('empty').hidden = true; $('charIndex').hidden = true; $('groupsPage').hidden = true; $('settingsPage').hidden = false;
  closeInspector(); $('selectionBar').hidden = true; $('resultCount').textContent = ''; $('hint').textContent = '';
  if (fetchMissing) loadSettingsData(refresh);
  const [section, , , cards] = settingsSection();
  const runs = state.syncHistory, info = state.settings, up = info?.startup || {};
  const reasons = state.exclInfo.reasons || {};
  const sourcesOf = info?.package?.sources || state.libraries?.info?.sources || [];
  const photos = sourcesOf.some(x => x.kind === 'photos');
  const segment = (name, options, current) => `<div class="segmented" data-setting="${name}">${options.map(([v, t]) => `<button class="${current === v ? 'on' : ''}" data-value="${v}">${t}</button>`).join('')}</div>`;
  // Per-app counts exist only for the Photos source (pictures saved by X or Pixiv); a folder library has none.
  const apps = Object.entries(runs.find(r => r.sources)?.sources || {}).sort((a, b) => b[1].photos - a[1].photos);
  const appRow = ([, v]) => `<tr><td title="${esc(v.name)}">${esc(v.name)}</td><td>${number(v.photos)}</td><td>${v.collected ? number(v.collected) : '—'}</td></tr>`;
  const sort = prefs.get('sortLibrary', 'newest'), csort = prefs.get('sortCharacter', 'suspicious');
  const s = state.sync;
  const body = {
    setDisplay: () => cardHead('显示', '', 'browser') + `
      <div class="setting-line"><span>外观</span>${segment('theme', [['', '跟随系统'], ['light', '浅色'], ['dark', '深色']], prefs.get('theme', ''))}</div>
      <div class="setting-line"><span>图片网格</span>${segment('fit', [['0', '方形裁切'], ['1', '原比例']], state.fit ? '1' : '0')}</div>
      <div class="setting-line"><span>R18 内容</span>${segment('nsfw', [['', '显示'], ['hide', '隐藏'], ['only', '只看 R18']], state.nsfw)}</div>
      <div class="setting-line"><span>图库默认排序</span>${segment('sortLibrary', [['newest', '最新加入'], ['oldest', '最早加入']], sort)}</div>
      <div class="setting-line"><span>按角色筛选时的默认排序</span>${segment('sortCharacter', [['suspicious', '可疑优先'], ['newest', '最新加入'], ['oldest', '最早加入']], csort)}</div>
      <p class="settings-note net-foot">缩略图大小、图上显示什么、详情面板的内容，在顶栏的「视图选项」里调。</p>`,
    setSync: () => cardHead('同步', '只读读取来源里的图片，新增的导入并在本机识别；来源里删掉的图会从图库移除，人工整理记录保留。同步不联网。', 'library') + `
      ${taskLink(s.status === 'off' ? '这次启动没有开同步' : syncLine(s), SYNC_ACTIVE.includes(s.status) ? 'busy' : ['error', 'needs_windows', 'needs_recognition'].includes(s.status) ? 'warn' : s.status === 'off' ? 'off' : '')}
      ${startupSwitch('sync', '启动时同步', '检查来源里新增和删除的图')}
      <div class="setting-line"><span>本机识别上限<small class="line-hint">一次新增超过这么多张时，改为提示用 Windows 批量识别</small></span><span class="num-field"><input type="number" id="localLimit" ${!info ? 'disabled' : ''} min="100" max="100000" step="100" value="${up.local_limit || 3000}"> 张</span></div>
      ${photos && apps.length ? fold('sourcesApps', `iCloud 照片按存图的 App 统计（${number(apps.length)} 个 App）`, `<p class="muted-note">iCloud 照片里只导入 X 和 Pixiv 存下的图。</p><table class="settings-table"><tr><th>App</th><th>照片里</th><th>已导入</th></tr>${apps.map(appRow).join('')}</table>`) : ''}`,
    setExclude: () => cardHead('排除规则', `照片、截图、文字图这类不是插画的图不算进图库。只是做标记，不改动${photos ? '照片图库' : '来源文件'}；你的判断优先于自动规则。`, 'library') + `
      <div class="rule-row">${Object.entries(REASONS).map(([k, t]) => `<a class="rule" href="#/excluded?reason=${k}"><b>${number(reasons[k] || 0)}</b><span>${t}</span></a>`).join('')}<a class="rule" href="#/pending"><b>${number(state.pendingCount)}</b><span>待确认</span></a></div>`,
    setFold: () => cardHead('重复与相似', '勾选的关系会合并显示：图库里只留分辨率最高的一张，整组在「重复」页查看。<span class="hint-more" title="改动组里任一张会同步到整组。组里角色不一致时不合并，留在「重复」页等你处理。字节完全相同的副本总是合并。">了解详情</span>', 'library') + `
      ${startupSwitch('similar', '启动时检测相似图片', '只比较新导入的图')}
      <div class="fold-row">${FOLD_OPTIONS.map(([k, t, hint]) => `<label class="fold-option" title="${hint}"><input type="checkbox" data-fold="${k}" ${(state.fold || []).includes(k) ? 'checked' : ''} ${!state.fold ? 'disabled' : ''}><span>${t}</span><small>${hint}</small></label>`).join('')}</div>`,
    setNetwork: networkCard,
    setData: () => cardHead('备份与存储', '备份的是应用数据库里的人工整理记录；数据库升级前会自动备份一次。恢复备份前先退出 app。') + `
      <div class="setting-line"><span>${info ? `共 ${number(info.backup_count)} 份备份${info.backups?.[0] ? `<small class="line-hint">最近一次 ${esc(localStamp(info.backups[0].time))}</small>` : ''}` : '…'}</span><button class="quiet small" id="backupNow" ${!info ? 'disabled' : ''}>立即备份</button></div>
      <div class="setting-line"><span>缩略图缓存<small class="line-hint">${info ? `${number(info.thumbnails.files)} 个文件，${bytes(info.thumbnails.bytes)}；清除后浏览时会重新生成` : ''}</small></span><button class="quiet small" id="clearCache" ${info?.thumbnails.files ? '' : 'disabled'}>清除</button></div>
      ${info?.backups?.length ? fold('backupList', '最近的备份', `<table class="settings-table">${info.backups.slice(0, 5).map(b => `<tr><td class="mono">${esc(b.name)}</td><td>${bytes(b.bytes)}</td></tr>`).join('')}</table>`) : ''}
      ${info ? fold('dataPaths', '存储位置', `<table class="settings-table">${[['源数据库（识别结果，可以重建）', info.source], ['应用数据库（人工整理，不要删）', info.library], ['缩略图缓存（可以放心删）', info.thumbnails]].map(([t, d]) => `<tr><td>${t}</td><td class="mono">${esc(d.path)}</td><td>${bytes(d.bytes)}</td></tr>`).join('')}</table>`) : ''}`,
  };
  patchSettingsPage(`<nav class="settings-nav" aria-label="设置">${SETTINGS_SECTIONS.map(([id, t]) => `<a href="#/settings/${id}" class="${id === section ? 'on' : ''}"${id === section ? ' aria-current="page"' : ''}>${t}</a>`).join('')}</nav>
    <div class="settings-main" data-section="${section}">${cards.map(id => `<section class="settings-card" id="${id}">${body[id] ? body[id]() : ''}</section>`).join('')}</div>`, section);
  const page = $('settingsPage');
  renderAbout();
  if ($('setLibraries')) { if (!$('setLibraries').innerHTML) loadLibraries(); else renderLibrariesCard(); }
  if ($('setProvenance')) { if ($('setProvenance').innerHTML) renderProvenanceCard(); loadProvenance(); }   // the cached state at once, then fresh
  if ($('setAccounts') || section === 'network') { if (!state.accounts || !$('setAccounts')?.innerHTML) loadAccounts(); else renderAccountsCard(); }
  if (state.index?.works) renderIndexCard(false);
  if (state.names) renderNamesCard(false);
  wireFolds(page);
  for (const box of page.querySelectorAll('[data-fold]')) box.onchange = async () => {
    const kinds = [...page.querySelectorAll('[data-fold]:checked')].map(b => b.dataset.fold);
    try { state.fold = (await post('/api/fold', {kinds})).kinds; await refreshMeta(); toast(`已更新，「重复」页现在有 ${number(state.dupInfo.groups)} 组`); }
    catch (e) { toast(e.message); box.checked = !box.checked; }
  };
  const startup = async (body, done) => { try { state.settings = await post('/api/startup', body); toast(done); } catch (e) { toast(e.message); } renderSettings(); };
  page.onchange = e => { const box = e.target.closest('[data-startup]'); if (box) startup({[box.dataset.startup]: box.checked}, '已保存，下次启动时生效'); };
  if ($('localLimit')) $('localLimit').onchange = e => startup({local_limit: Math.round(Number(e.target.value))}, '已更新本机识别上限');
  if ($('backupNow')) $('backupNow').onclick = async () => { try { const r = await post('/api/backup', {}); toast('备份完成：' + r.path.split('/').pop()); state.settings = null; renderSettings(); } catch (e) { toast(e.message); } };
  if ($('clearCache')) $('clearCache').onclick = async () => { try { const r = await post('/api/cache', {action: 'clear'}); state.settings = r; toast(`已清除 ${number(r.removed)} 个缩略图`); renderSettings(); } catch (e) { toast(e.message); } };
  for (const g of page.querySelectorAll('[data-setting]')) for (const b of g.querySelectorAll('button')) b.onclick = () => {
    const key = g.dataset.setting, v = b.dataset.value;
    if (key === 'fit' && state.fit !== (v === '1')) { state.fit = !state.fit; prefs.set('fit', state.fit); }
    if (key === 'nsfw') setNsfw(v);
    if (key === 'theme') { prefs.set('theme', v); window.applyTheme?.(); }
    if (key === 'sortLibrary' || key === 'sortCharacter') prefs.set(key, v);
    renderSettings();
  };
}
// Status lines on the settings pages hand over to the task panel, where runs are started, stopped and confirmed.
$('settingsPage').addEventListener('click', e => {
  const b = e.target.closest('[data-open-tasks]'); if (!b) return;
  e.stopPropagation();
  if (b.dataset.openTasks === 'provenance') provenanceArmed = true;
  toggleTasks(true);
});

// ---------- misc wiring ----------
$('undoButton').onclick = undo; $('helpButton').onclick = () => $('help').showModal();
$('sidebarToggle').onclick = () => { if (innerWidth <= 760) $('sidebar').classList.toggle('mobile-open'); else $('sidebar').hidden = !$('sidebar').hidden; requestAnimationFrame(() => renderGrid()); };
for (const dialog of document.querySelectorAll('dialog')) dialog.addEventListener('click', e => { if (e.target === dialog) { const r = dialog.getBoundingClientRect(); if (e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom) dialog.close(); } });

// ---------- libraries (docs/LIBRARIES.md): managed on the settings page; opening one restarts the server on it ----------
async function loadLibraries() {
  try { state.libraries = await api('/api/libraries'); } catch { state.libraries = null; }
  const cur = state.libraries?.info;
  $('brandLib').textContent = cur?.test ? '测试·' + (cur.name.split(' · ').pop()) : '';
  $('brand').title = cur ? `Illustash · ${cur.name}` : 'Illustash';
  renderLibrariesCard();
}
// New libraries and the open library's sources are grouped by origin (#107): every folder names where its
// pictures came from; nothing is guessed from folder names. Folders include their subfolders and may not nest.
const ORIGINS = [['pixiv', 'Pixiv'], ['x', 'X'], ['other', '其他']];
const IS_MAC = /Mac/.test(navigator.platform);
const libForm = {open: false, name: '', folders: {}, photos: false};
// The backend lists the origins it knows (#111); the fallback covers the legacy layout without library info.
const origins = () => { const list = state.libraries?.info?.origins; return list?.length ? list.map(o => [o.id, o.label]) : ORIGINS; };
function renderLibrariesCard() {
  const card = $('setLibraries'); if (!card || state.route.page !== 'settings') return;
  const data = state.libraries;
  if (!data) { card.innerHTML = '<h2>图库</h2><p class="muted-note">正在读取…</p>'; return; }
  const home = p => p.replace(/^\/Users\/[^/]+/, '~'), cur = data.info, list = origins();
  const seg = (attr, value, options) => `<div class="segmented">${options.map(([v, t]) => `<button class="${v === value ? 'on' : ''}" ${attr}="${v}">${t}</button>`).join('')}</div>`;
  const editable = cur && !cur.test, last = cur?.sources.length <= 1;
  const remove = s => editable ? `<button class="quiet" data-lib-remove="${esc(s.id)}" ${last ? 'disabled title="图库至少要保留一个来源"' : ''}>移除</button>` : '';
  const folderRow = s => `<div class="setting-line lib-folder"><span class="lib-path" title="${esc(s.path)}"><bdi>${esc(home(s.path))}</bdi></span>
      <span class="lib-src">${editable ? seg(`data-lib-preset="${esc(s.id)}" data-preset`, s.preset, list) : ''}${remove(s)}</span></div>`;
  const groups = !cur ? '' : list.map(([id, label]) => {
    const rows = cur.sources.filter(s => s.kind === 'folder' && (list.some(o => o[0] === s.preset) ? s.preset : 'other') === id);
    if (!rows.length && !editable) return '';
    return `<div class="lib-group"><h3>${esc(label)}</h3>${rows.map(folderRow).join('') || '<p class="muted-note">还没有文件夹</p>'}
      ${editable ? `<button class="quiet" data-lib-add-source="${id}">添加文件夹…</button>` : ''}</div>`;
  }).join('') + (() => {
    const photos = cur.sources.find(s => s.kind === 'photos');
    if (photos) return `<div class="lib-group"><h3>iCloud 照片</h3><div class="setting-line lib-folder"><span class="lib-path">本机「照片」图库，按存图的 App 区分 X 和 Pixiv</span><span class="lib-src">${remove(photos)}</span></div></div>`;
    return editable && IS_MAC ? `<div class="lib-group"><h3>iCloud 照片</h3><button class="quiet" data-lib-add-photos>添加 iCloud 照片</button></div>` : '';
  })();
  // Test libraries were generated by a developer tool (tools/test_libraries.py, removed in #186), so their column shows only in dev mode.
  const dev = devMode && data.libraries.some(l => l.test);
  const row = l => `<tr><td>${esc(l.name)}<small>${esc(home(l.path))}</small></td>${dev ? `<td>${l.test ? esc(l.test) : '—'}</td>` : ''}<td>${l.path === data.current ? '<span class="tag-on">当前</span>'
    : `<button class="quiet" data-open-library="${esc(l.path)}" data-name="${esc(l.name)}">打开</button><button class="quiet" data-forget-library="${esc(l.path)}" title="只从列表里移除，不删除图库">移出列表</button>`}</td></tr>`;
  const ex = data.export || {};
  const exportLine = ex.status === 'running' ? `<p class="muted-note">正在导出到 ${esc(home(ex.path))}…</p>`
    : ex.status === 'done' ? `<p class="muted-note">已导出：${esc(home(ex.path))}（${(ex.bytes / 1048576).toFixed(0)} MB）</p>`
    : ex.status === 'error' ? `<p class="muted-note">导出失败：${esc(ex.error)}</p>` : '';
  const f = libForm;
  const form = !f.open ? '' : `<div class="lib-form">
      <p class="muted-note">按图片来自哪里分别添加文件夹，每类可以有多个。子文件夹会一起读取；图片保留在原位置，不复制。选 X 或 Pixiv 的文件夹才能用以图搜图、画师回查。</p>
      <div class="lib-columns">${list.map(([id, label]) => `<div class="lib-col"><h3>${esc(label)}</h3>
        ${(f.folders[id] || []).map((p, i) => `<div class="lib-picked"><span class="lib-path" title="${esc(p)}"><bdi>${esc(home(p))}</bdi></span><button class="quiet" data-lib-drop="${id}" data-index="${i}" title="不添加这个文件夹">×</button></div>`).join('')}
        <button class="quiet" data-lib-pick="${id}">添加文件夹…</button></div>`).join('')}</div>
      ${IS_MAC ? `<label class="lib-photos"><input type="checkbox" id="libPhotos" ${f.photos ? 'checked' : ''}> 同时读取 iCloud 照片（本机「照片」图库，按存图的 App 区分 X 和 Pixiv）</label>` : ''}
      <div class="setting-line"><span>名字<small class="line-hint">保存在 ${esc(home(data.home))}</small></span><input class="lib-input" id="libName" placeholder="新图库" value="${esc(f.name)}"></div>
      <p><button class="primary" data-lib-create>创建并打开</button> <button class="quiet" data-lib-cancel>取消</button></p></div>`;
  card.innerHTML = `${cardHead('图库', '每个图库各自保存识别结果和人工整理记录，图片本身留在原处、不复制。切换图库时后台会重启，几秒后自动刷新。')}
    ${cur ? `<div class="setting-line"><span><b>${esc(cur.name)}</b><small class="line-hint lib-path-hint">${esc(home(cur.path))}</small></span><span class="tag-on">当前图库</span></div>
    <div class="lib-groups">${groups}</div>` : '<p class="muted-note">当前是旧的数据布局（不是图库包）。</p>'}
    <p class="lib-actions"><button class="quiet" data-lib-new>新建图库</button><button class="quiet" data-lib-choose="library">打开其他图库…</button><button class="quiet" data-lib-choose="zip">导入…</button>${cur ? `<button class="quiet" data-lib-export ${ex.status === 'running' ? 'disabled' : ''}>导出当前图库</button>` : ''}</p>
    ${form}${exportLine}
    ${data.libraries.length > 1 || dev ? `<table class="settings-table lib-table"><tr><th>这台电脑上的图库</th>${dev ? '<th>测试画像</th>' : ''}<th></th></tr>${data.libraries.map(row).join('')}</table>` : ''}
    <p class="muted-note">导出的 zip 只有识别结果和人工整理记录，不含图片，可以换电脑或发给别人。</p>`;
}
// Gallery pages have no Tauri dialog permission. Keep confirmation inside the page;
// window.confirm may be replaced with an async plugin function by the desktop shell.
let confirmationOpen = false;
function confirmAction(message, title = '确认操作') {
  if (confirmationOpen) return Promise.resolve(false);
  confirmationOpen = true;
  const token = state.token, route = state.route;
  return new Promise(resolve => {
    const d = document.createElement('dialog'); d.className = 'palette profile-dialog';
    d.setAttribute('aria-label', title);
    d.innerHTML = `<form method="dialog"><div class="dialog-top"><h2></h2><button type="button" class="icon" data-close aria-label="关闭">×</button></div>
      <p class="muted-note" style="white-space:pre-line"></p>
      <div class="dialog-actions"><button value="cancel" class="quiet" autofocus>取消</button><button value="ok" class="primary">确认</button></div></form>`;
    d.querySelector('h2').textContent = title;
    d.querySelector('p').textContent = message;
    d.querySelector('[data-close]').onclick = () => d.close();
    d.onclick = e => {
      if (e.target !== d) return;
      const r = d.getBoundingClientRect();
      if (e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom) d.close();
    };
    d.onclose = () => {
      const ok = d.returnValue === 'ok' && token === state.token && route === state.route;
      d.remove(); confirmationOpen = false; resolve(ok);
    };
    document.body.append(d); d.showModal();
  });
}
// Native folder picker (several at once). Where none exists, a typed path stands in.
async function pickFolders() {
  try {
    const r = await api('/api/libraries/choose', {method: 'POST', body: JSON.stringify({kind: 'folder', multiple: true})});
    return r?.paths || [];
  } catch (err) {
    if (!/Unsupported/.test(err.message)) { toast(err.message); return []; }
    const typed = prompt('这台电脑上没有文件夹选择窗口，请输入文件夹路径：');
    return typed?.trim() ? [typed.trim()] : [];
  }
}
const removingSources = new Set();
async function libraryAction(action, body = {}, token = state.token) {
  try { return await api('/api/libraries/' + action, {method: 'POST', body: JSON.stringify(body), headers: {'X-CSRF-Token': token}}); } catch (err) { toast(err.message); return null; }
}
async function pollExport() {
  for (let i = 0; i < 2400 && state.libraries?.export?.status === 'running'; i++) { await new Promise(r => setTimeout(r, 1500)); await loadLibraries(); }
}
$('settingsPage').addEventListener('click', async e => {
  const t = e.target.closest('button'); if (!t || !$('setLibraries')?.contains(t)) return;
  const d = t.dataset;
  const keep = () => { libForm.name = $('libName')?.value ?? libForm.name; libForm.photos = $('libPhotos')?.checked ?? libForm.photos; };
  if ('libNew' in d) { Object.assign(libForm, {open: true, name: '', folders: {}, photos: false}); renderLibrariesCard(); }
  else if ('libCancel' in d) { libForm.open = false; renderLibrariesCard(); }
  else if (d.libPick) {
    keep();
    const picked = await pickFolders(), column = libForm.folders[d.libPick] ||= [];
    for (const p of picked) if (!Object.values(libForm.folders).some(c => c.includes(p))) column.push(p);
    renderLibrariesCard();
  }
  else if (d.libDrop) { keep(); libForm.folders[d.libDrop]?.splice(+d.index, 1); renderLibrariesCard(); }
  else if (d.libChoose) {
    const r = await libraryAction('choose', {kind: d.libChoose}); if (!r?.path) return;
    if (d.libChoose === 'library') openLibrary(r.path, r.path.split('/').pop().replace(/\.pilib$/, ''));
    else { toast('正在导入…'); const lib = await libraryAction('import', {path: r.path}); if (lib) { toast(`已导入「${lib.name}」`); loadLibraries(); } }
  }
  else if ('libCreate' in d) {
    keep();
    const sources = Object.entries(libForm.folders).flatMap(([preset, paths]) => paths.map(path => ({kind: 'folder', path, preset})));
    if (libForm.photos) sources.push({kind: 'photos'});
    if (!sources.length) { toast('请至少添加一个文件夹'); return; }
    const lib = await libraryAction('new', {name: libForm.name, sources});
    if (lib) { libForm.open = false; openLibrary(lib.path, lib.name); }
  }
  else if (d.libAddSource) {
    const paths = await pickFolders(); if (!paths.length) return;
    const r = await libraryAction('sources/add', {kind: 'folder', preset: d.libAddSource, paths});
    await loadLibraries();
    if (r) { toast(`已添加 ${r.added?.length || 1} 个文件夹${r.sync ? '，正在同步' : ''}`); if (r.sync) pollSync(); }
  }
  else if ('libAddPhotos' in d) {
    const r = await libraryAction('sources/add', {kind: 'photos'});
    await loadLibraries();
    if (r) { toast(`已添加 iCloud 照片${r.sync ? '，正在同步' : ''}`); if (r.sync) pollSync(); }
  }
  else if (d.libRemove) {
    if (removingSources.has(d.libRemove)) return;
    removingSources.add(d.libRemove);
    const token = state.token, route = state.route;
    try {
      const ask = await libraryAction('sources/remove', {source: d.libRemove}, token); if (!ask) return;
      if (token !== state.token || route !== state.route) return;
      if (!await confirmAction(`这个来源的 ${number(ask.count)} 张图会从图库移除。人工整理记录会保留，重新添加同一个文件夹可以恢复。确定移除吗？`, '移除来源')) return;
      const r = await libraryAction('sources/remove', {source: d.libRemove, confirm: 1}, token);
      await loadLibraries();
      if (r) { toast(`已移除来源（${number(r.removed)} 张图）${r.sync ? '，正在同步' : ''}`); if (r.sync) pollSync(); }
    } finally { removingSources.delete(d.libRemove); }
  }
  else if (d.libPreset !== undefined && d.preset) {
    if (await libraryAction('preset', {source: d.libPreset, preset: d.preset})) { toast('出处已更新'); loadLibraries(); }
  }
  else if (d.forgetLibrary) { if (await libraryAction('forget', {path: d.forgetLibrary})) loadLibraries(); }
  else if ('libExport' in d) { if (await libraryAction('export')) { await loadLibraries(); pollExport(); } }
});
async function openLibrary(path, name) {
  try { await api('/api/libraries/open', {method: 'POST', body: JSON.stringify({path})}); } catch (err) { toast(err.message); return; }
  toast(`正在打开「${name}」…`);
  for (let i = 0; i < 120; i++) {
    await new Promise(r => setTimeout(r, 700));
    try {
      const s = await fetch('/api/settings').then(r => r.json());
      if (s.package?.path === path) { location.hash = '#/home'; location.reload(); return; }
    } catch { /* the server is restarting */ }
  }
  error('切换图库超时：请检查启动终端。');
}
$('settingsPage').addEventListener('click', e => {
  const b = e.target.closest('[data-open-library]');
  if (b) openLibrary(b.dataset.openLibrary, b.dataset.name);
});
loadLibraries();

// ---------- bound accounts (docs/PROVENANCE.md section 8): shared by every library; secrets live in the Keychain ----------
async function loadAccounts() {
  try { state.accounts = await api('/api/accounts'); } catch { state.accounts = null; }
  renderAccountsCard();
}
function renderAccountsCard() {
  const card = $('setAccounts'); if (!card || state.route.page !== 'settings') return;
  const a = state.accounts;
  if (!a) { card.innerHTML = '<h2>账号</h2><p class="muted-note">正在读取…</p>'; return; }
  const p = a.pixiv, x = a.x, s = a.saucenao;
  card.innerHTML = `${cardHead('账号', '找回画师从你的关注出发：拉他们公开作品的小图，在本机逐张比对。只读，不点赞、不关注、不发任何内容。')}
    <h3 class="acct-h">Pixiv${scope('shared')}</h3>
    ${pixivAccount(a)}
    <h3 class="acct-h">X${scope('library')}</h3>
    <p class="muted-note">用 X 的官方数据归档导入关注列表：X 设置 → 你的账号 → 下载你的数据，收到邮件后下载 zip 导入。只读取其中的关注（数字账号 ID），私信、推文都不会打开。</p>
    <div class="setting-line"><span>${x.imported ? `已导入 ${number(x.imported)} 个关注<small class="line-hint">已对应到画师 ${number(x.resolved || 0)} 位 · 待查 ${number(x.pending || 0)} 个${x.imported_at ? ` · 导入于 ${esc(String(x.imported_at).slice(0, 16))}` : ''}</small>` : '尚未导入'}</span>
      <span class="lib-pick"><input class="lib-input" id="acctXPath" placeholder="或粘贴 zip / 解压后文件夹的路径"><button class="quiet" data-acct-x>导入归档…</button></span></div>
    <h3 class="acct-h">SauceNAO（以图搜图）${scope('shared')}</h3>
    ${s.bound ? `<div class="setting-line"><span>已保存 API key<small class="line-hint">以图搜图还没有上线，现在不会使用</small></span><span><button class="quiet" data-acct-unbind="saucenao">移除</button></span></div>`
      : `<div class="setting-line"><span>API key<small class="line-hint">在 saucenao.com 账号页的 search-api 里；以图搜图上线前不会使用</small></span><span class="lib-pick"><input class="lib-input" id="acctSauce" type="password" autocomplete="off" placeholder="40 位十六进制"><button class="quiet" data-acct-bind="saucenao">保存</button></span></div>`}`;
}
// Pixiv: in the desktop app the person logs in themselves in its Pixiv window (docs/PROVENANCE.md section 8);
// pasting PHPSESSID stays as the development entry of the plain web page.
function pixivAccount(a) {
  const p = a.pixiv, desk = a.desktop, inApp = p.via === 'app';
  const paste = label => `<div class="setting-line"><span>PHPSESSID<small class="line-hint">开发用入口</small></span><span class="lib-pick"><input class="lib-input" id="acctPixiv" type="password" autocomplete="off" placeholder="12345678_…"><button class="quiet" data-acct-bind="pixiv">${label}</button></span></div>`;
  // Outside dev mode the plain web page only points to the desktop app's login window.
  const pasteHint = devMode ? `<p class="muted-note">网页版（开发用）：在浏览器里登录 pixiv.net，打开开发者工具（${isMac ? '⌥⌘I' : 'Ctrl+Shift+I'}）→ Application（Safari 为「存储空间」）→ Cookie → pixiv.net，复制 <code>PHPSESSID</code> 的值粘贴到这里。</p>` : '<p class="muted-note">请从 Illustash 桌面 app 打开设置，在它的 Pixiv 窗口里登录。</p>';
  const login = label => state.pixivWaiting ? '<span class="muted-note">正在等你在 Pixiv 窗口里登录…</span>' : `<button class="quiet" data-acct-login>${label}</button>`;
  if (!p.bound) {
    return desk ? `<p class="muted-note">打开 Pixiv 官方登录页，由你亲手登录（出现人机验证请自己完成）；登录状态只留在 app 的 Pixiv 窗口里。登录后才能看到 R18 作品。Pixiv 没有给第三方开放接口，这里用的是网页自己的接口，低频只读。</p>
      <div class="setting-line"><span>尚未登录</span><span>${login('登录 Pixiv…')}</span></div>` : pasteHint + (devMode ? paste('绑定') : '');
  }
  const line = `<div class="setting-line"><span>${p.expired ? '<span class="acct-expired">登录已过期</span>' : '已绑定'}<small class="line-hint">${esc(p.name || '')}${p.user_id ? ` · ID ${esc(p.user_id)}` : ''}${p.following != null && !p.expired ? ` · 关注 ${number(p.following)} 位` : ''}${p.checked_at ? ` · 检查于 ${esc(p.checked_at)}` : ''}</small></span><span>${desk && !p.expired ? login(inApp ? '打开 Pixiv 窗口' : '改用窗口登录…') : ''}<button class="quiet" data-acct-check="pixiv">重新检查</button><button class="quiet" data-acct-unbind="pixiv">解绑</button></span></div>`;
  if (!p.expired) return line;
  const why = '找回画师会暂停查 Pixiv，免得在没登录的状态下漏掉 R18 作品。';
  if (desk) return line + `<p class="muted-note">Pixiv 不再接受这个登录（退出过，或者过期了）。${why}在 Pixiv 窗口里重新登录一次就能恢复。${p.window_logged_in ? '窗口里已经重新登录了，点「重新检查」确认。' : ''}</p>
      <div class="setting-line"><span>重新登录</span><span>${login('重新登录 Pixiv…')}</span></div>`;
  if (inApp) return line + `<p class="muted-note">Pixiv 登录在桌面 app 的 Pixiv 窗口里。${why}请从 Illustash 桌面 app 打开，再重新登录。</p>`;
  if (!devMode) return line + `<p class="muted-note">Pixiv 不再接受这个登录状态。${why}请从 Illustash 桌面 app 重新登录。</p>`;
  return line + `<p class="muted-note">Pixiv 不再接受这个登录状态（在浏览器里退出过，或者过期了）。${why}重新复制一次 <code>PHPSESSID</code> 粘贴到下面就能恢复。</p>` + paste('重新绑定');
}
let pixivPoll = null;
function stopPixivLogin() {
  if (pixivPoll) {
    clearTimeout(pixivPoll.timer);
    pixivPoll.controller.abort();
    pixivPoll = null;
  }
  state.pixivWaiting = false;
}
function waitPixivLogin() {   // one request at a time; superseded responses cannot change the UI
  stopPixivLogin();
  const poll = {controller: new AbortController(), timer: null, until: Date.now() + 10 * 60e3};
  pixivPoll = poll;
  state.pixivWaiting = true; renderAccountsCard();
  const tick = async () => {
    if (pixivPoll !== poll) return;
    if (Date.now() >= poll.until) { stopPixivLogin(); renderAccountsCard(); return; }
    let r;
    try {
      r = await api('/api/accounts/pixiv/connect', {method: 'POST', body: '{}', signal: poll.controller.signal});
    } catch (err) {
      if (pixivPoll !== poll) return;
      stopPixivLogin(); toast(err.message); loadAccounts(); return;
    }
    if (pixivPoll !== poll) return;
    if (r.bound) { stopPixivLogin(); toast(`已绑定 Pixiv：${r.name || r.user_id}`); loadAccounts(); }
    else if (!r.login_open || Date.now() >= poll.until) { stopPixivLogin(); renderAccountsCard(); }
    else poll.timer = setTimeout(tick, 2000);
  };
  poll.timer = setTimeout(tick, 2000);
}
async function accountAction(action, body = {}) {
  try { return await api('/api/accounts/' + action, {method: 'POST', body: JSON.stringify(body)}); } catch (err) { toast(err.message); return null; }
}
$('settingsPage').addEventListener('click', async e => {
  const t = e.target.closest('button'); if (!t || !$('setAccounts')?.contains(t)) return;
  const d = t.dataset;
  if (d.acctBind) {
    const input = $(d.acctBind === 'pixiv' ? 'acctPixiv' : 'acctSauce'); t.disabled = true;
    const r = await accountAction(d.acctBind, {value: input.value}); t.disabled = false;
    if (r) { toast(d.acctBind === 'pixiv' ? `已绑定 Pixiv：${r.name || r.user_id}` : '已保存 SauceNAO API key'); loadAccounts(); }
  } else if ('acctLogin' in d) {
    t.disabled = true;
    const r = await accountAction('pixiv/login'); t.disabled = false;
    if (r) waitPixivLogin();
  } else if (d.acctCheck) {
    t.disabled = true;
    const r = await accountAction('pixiv/check'); t.disabled = false;
    if (r) { toast(r.ok ? `Pixiv 登录有效：${r.name || r.user_id}` : 'Pixiv 登录已过期，请重新登录'); loadAccounts(); }
  } else if (d.acctUnbind) {
    if (d.acctUnbind === 'pixiv') stopPixivLogin();
    if (await accountAction(d.acctUnbind + '/unbind')) { toast('已解绑'); loadAccounts(); }
  } else if ('acctX' in d) {
    const path = $('acctXPath').value.trim();
    const r = await accountAction('x/import', path ? {path} : {});
    if (r && !r.cancelled) { toast(`新导入 ${number(r.new)} 个关注`); loadAccounts(); }
  }
});

// ---------- 找回画师 (docs/PROVENANCE.md section 8): the provenance worker runs as a child process of the server ----------
let provenanceTimer = 0, provenanceArmed = false;
const provenanceDrawn = new WeakMap();
async function loadProvenance() {
  clearTimeout(provenanceTimer);
  const was = state.provenance?.state?.status;
  try { state.provenance = await api('/api/provenance'); } catch {
    // One failed read must not end the polling of a run that is going on: keep what was shown and try again.
    if (['running', 'stopping'].includes(was)) provenanceTimer = setTimeout(loadProvenance, 4000);
    else { state.provenance = null; renderProvenanceCard(); renderTasks(); }
    return;
  }
  renderProvenanceCard(); renderTasks();
  const s = state.provenance?.state?.status;
  if (['running', 'stopping'].includes(s)) provenanceTimer = setTimeout(loadProvenance, 1500);
}
const RUN_STATUS = {done: '完成', stopped: '已停止', failed: '出错', running: '进行中'};
const pct = (n, total) => total ? `${(n / total * 100).toFixed(1)}%` : '—';
// provenance.sqlite keeps SQLite's UTC timestamps.
const localTime = utc => { const d = new Date(String(utc || '').replace(' ', 'T') + 'Z'); return isNaN(d) ? '' : `${d.getMonth() + 1}-${String(d.getDate()).padStart(2, '0')} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`; };
// Progress bars from the worker's numbers (docs/BACKEND_TASKS.md): this step's items, and traffic against the run's cap.
function provenanceBars(s) {
  const p = s.progress; if (!p || !p.total) return '';
  const bar = (label, done, total, detail) => `<div class="prov-bar-row"><span class="prov-bar-label">${label}</span><span class="prov-bar" role="progressbar" aria-valuemin="0" aria-valuemax="${total}" aria-valuenow="${done}"><span style="width:${Math.min(100, done / total * 100).toFixed(1)}%"></span></span><span class="prov-bar-num">${detail}</span></div>`;
  // Two levels when the step has them (本地比对: all artists, then the current artist's works), then traffic.
  const sub = p.sub_total ? bar(esc(p.sub_label || '当前'), p.sub_done || 0, p.sub_total, `${number(p.sub_done || 0)} / ${number(p.sub_total)}`) : '';
  return `<div class="prov-bars">${bar(esc(p.label || '进度'), p.done || 0, p.total, `${number(p.done || 0)} / ${number(p.total)}${p.found != null ? ` · 对上 ${number(p.found)}` : ''}`)}${sub}${p.mb != null && s.budget_mb ? bar('流量', p.mb, s.budget_mb, `${number(Math.round(p.mb))} / ${number(s.budget_mb)} MB`) : ''}</div>`;
}
function renderProvenanceCard() {
  const card = $('setProvenance'); if (!card || state.route.page !== 'settings') return;
  const p = state.provenance;
  if (!p) { card.innerHTML = '<h2>找回画师</h2><p class="muted-note">正在读取…</p>'; return; }
  if (!p.available) { card.innerHTML = '<h2>找回画师</h2><p class="muted-note">只有图库包能找回画师。</p>'; return; }
  const s = p.state || {}, busy = PROV_ACTIVE.includes(s.status), cov = p.coverage;
  const covLine = (label, key) => cov?.pictures?.[key] ? `<div class="prov-stat"><b>${pct(cov.with_artist?.[key] || 0, cov.pictures[key])}</b><span>${label}<small>${number(cov.with_artist?.[key] || 0)} / ${number(cov.pictures[key])} 张</small></span></div>` : '';
  const total = cov ? Object.values(cov.pictures || {}).reduce((a, b) => a + b, 0) : 0;
  const html = cardHead('找回画师', '逐张查图的画师：用像素哈希和 IQDB 签名（只发哈希文本，不发图片）在 Danbooru 查原帖，并比对你关注的画师。结果在详情面板的「画师」里，可以点「不是 TA」纠正。') + `
    ${cov ? `<div class="prov-stats">${covLine('X 的图有画师', 'x')}${covLine('Pixiv 的图有画师', 'pixiv')}${covLine('其他来源有画师', 'other')}<div class="prov-stat"><b>${pct(cov.with_original_post || 0, total)}</b><span>能对到原帖<small>${number(cov.with_original_post || 0)} 张</small></span></div></div>` : ''}
    <div class="setting-line set-status"><span><span class="local-dot${busy ? ' busy' : s.status === 'failed' ? ' warn' : ''}"></span>${busy ? '正在查找…' : s.status === 'failed' && s.reason ? `上次出错：${esc(s.reason)}` : p.runs?.[0] ? `上次 ${esc(localTime(p.runs[0].started_at))} · 新找到 ${number(p.runs[0].found || 0)}` : '还没有查过'}</span>${busy ? '<button class="quiet small" data-open-tasks>在任务面板查看</button>' : '<button class="quiet small" data-open-tasks="provenance">查找画师…</button>'}</div>
    <label class="switch-line"><span><b>同步后自动查新图${scope('library')}</b><small>${p.auto_network ? `新加入的图做签名识别、查像素哈希、比对新找到的画师；每次最多 ${number(p.budgets?.new || 50)} MB` : '新加入的图只在本机做签名识别；要联网查，在任务面板里每次授权'}${p.frozen ? ' · 测试图库不同步，这个开关不起作用' : ''}</small></span><input type="checkbox" class="switch" id="provAuto" ${p.auto ? 'checked' : ''}></label>
    ${cov ? '<a class="setting-line set-link" href="#/artist-audit"><span>画师抽查<small class="line-hint">随机抽 50 张，逐张判断找到的画师对不对</small></span><span>›</span></a>' : ''}
    ${state.accounts && !state.accounts.pixiv?.bound ? '<p class="settings-note net-foot">登录 Pixiv 后才能比对 R18 作品、查 Pixiv 上的相关画师（见下方「账号」）。</p>' : ''}`;
  // Polled while a run goes: redraw only on change, so a click is never lost to a replaced button.
  if (provenanceDrawn.get(card) === html) return;
  provenanceDrawn.set(card, html); card.innerHTML = html;
  wireFolds(card);
}
$('taskPanel').addEventListener('click', async e => {
  const b = e.target.closest('[data-prov]'); if (!b) return;
  // 开始 first shows what leaves the machine; the authorization covers this one run only.
  if (b.dataset.prov === 'arm' || b.dataset.prov === 'cancel') { provenanceArmed = b.dataset.prov === 'arm'; renderTasks(); return; }
  b.disabled = true;
  const body = {action: b.dataset.prov};
  if (b.dataset.prov === 'start') { body.allow_network = b.dataset.network === '1'; provenanceArmed = false; }
  try { state.provenance = await post('/api/provenance', body); } catch (err) { toast(err.message); }
  loadProvenance();
});
$('settingsPage').addEventListener('change', async e => {
  if (e.target.id !== 'provAuto') return;
  try { await post('/api/startup', {provenance: e.target.checked}); toast(e.target.checked ? '同步后会自动查新图的画师' : '同步后不再自动查画师'); }
  catch (err) { toast(err.message); e.target.checked = !e.target.checked; }
  loadProvenance();
});

// ---------- 任务面板 (#199): every background job's progress, confirmations and manual runs in one place ----------
// Each job keeps its own endpoint and poller; this only gathers them. While any job runs, its poller re-arms
// itself every 2 s; otherwise a slow tick (fast while the panel is open) catches jobs started elsewhere.
const taskPanel = {open: false, timer: 0, settingsAt: 0};
const PROV_ACTIVE = ['running', 'stopping'];
function taskList() {
  const s = state.sync, ix = state.index?.state || {}, nm = state.names?.state || {}, sim = state.settings?.tasks?.similar || {status: 'off'};
  const pv = state.provenance, ps = pv?.state || {}, ex = state.libraries?.export || {};
  const folder = !(state.settings?.package?.sources || state.libraries?.info?.sources || []).some(x => x.kind === 'photos');
  const bar = (done, total) => total ? `<div class="progress"><span style="width:${Math.min(100, Math.round((done || 0) / total * 100))}%"></span></div>` : '';
  const list = [
    {key: 'sync', name: folder ? '文件夹同步' : '照片同步', active: SYNC_ACTIVE.includes(s.status), line: s.status === 'off' ? '这次启动没有开同步' : syncLine(s),
      warn: ['error', 'needs_windows', 'needs_recognition', 'interrupted'].includes(s.status), body: syncBar(s, bar),
      action: `<button class="quiet small" data-task="sync" ${SYNC_ACTIVE.includes(s.status) || s.status === 'off' ? 'disabled' : ''}>立即同步</button>`,
      log: state.syncHistory?.length ? ['同步记录', `<table class="settings-table"><tr><th>时间</th><th>新增</th><th>移除</th><th>导入</th><th>失败</th></tr>${state.syncHistory.slice(0, 8).map(r => `<tr><td>${esc(stamp(r.started_at))}</td><td>${number(r.found)}</td><td>${number(r.removed)}</td><td>${number(r.imported)}</td><td>${number(r.failed)}</td></tr>`).join('')}</table>`] : null},
    {key: 'index', name: '检查新角色', active: INDEX_ACTIVE.includes(ix.status), line: state.index ? indexLine(ix, state.index.check) : '…', warn: ix.status === 'error',
      body: INDEX_ACTIVE.includes(ix.status) ? bar(ix.done, ix.total) : '',
      action: `<button class="quiet small" data-task="index" ${INDEX_ACTIVE.includes(ix.status) || ix.status === 'off' || !state.index?.watch?.works.length ? 'disabled' : ''} title="${state.index?.watch?.works.length ? '查关注作品在 Danbooru 上有没有新角色（只发送作品名）' : '先在资料库里关注作品'}">立即检查</button>`},
    {key: 'names', name: '角色名称', active: NAMES_ACTIVE.includes(nm.status), line: namesLine(nm), warn: nm.status === 'error', body: NAMES_ACTIVE.includes(nm.status) ? bar(nm.done, nm.total) : '',
      action: `<button class="quiet small" data-task="names" ${NAMES_ACTIVE.includes(nm.status) || nm.status === 'off' || !state.names ? 'disabled' : ''} title="从 Bangumi 重新获取全部角色的中文名（只发送 tag 与作品名）">重新获取</button>`},
    {key: 'similar', name: '相似图片', active: SIMILAR_ACTIVE.includes(sim.status), line: taskSimilar(sim), warn: sim.status === 'error', body: SIMILAR_ACTIVE.includes(sim.status) ? bar(sim.done, sim.total) : '',
      action: `<button class="quiet small" data-task="similar" ${SIMILAR_ACTIVE.includes(sim.status) || sim.status === 'off' ? 'disabled' : ''}>重新检查</button>`},
  ];
  if (pv?.available) list.push({key: 'provenance', name: '找回画师', active: PROV_ACTIVE.includes(ps.status), warn: ps.status === 'failed',
    line: ps.status === 'stopping' ? '正在停止…' : PROV_ACTIVE.includes(ps.status) ? `正在查找 · 第 ${(ps.index || 0) + 1}/${ps.total || 1} 步${ps.text ? ' · ' + ps.text : ''}`
      : ps.status === 'failed' ? `上次出错：${ps.reason || ''}` : pv.runs?.[0] ? `上次 ${localTime(pv.runs[0].started_at)} · 新找到 ${number(pv.runs[0].found || 0)}` : '还没有查过',
    body: PROV_ACTIVE.includes(ps.status) ? provenanceBars(ps) + (ps.side ? `<p class="task-side">同时进行 · ${esc(ps.side.name || ps.side.step)}${ps.side.text ? ' · ' + esc(ps.side.text) : ''}</p>${provenanceBars({progress: ps.side.progress})}` : '') : '',
    action: PROV_ACTIVE.includes(ps.status) ? `<button class="quiet small" data-prov="stop" ${ps.status === 'stopping' ? 'disabled' : ''}>停止</button>` : provenanceArmed ? '' : '<button class="quiet small" data-prov="arm">开始…</button>',
    log: pv.runs?.length ? ['运行记录', `<table class="settings-table"><tr><th>开始</th><th>方式</th><th>新找到</th><th>流量</th><th>结果</th></tr>${pv.runs.slice(0, 8).map(r => `<tr><td>${esc(localTime(r.started_at))}</td><td>${r.kind === 'new' ? '同步后' : r.kind === 'full' ? '手动' : '命令行'}</td><td>${number(r.found || 0)}</td><td>${(r.traffic_mb || 0).toFixed(1)} MB</td><td title="${esc(r.reason || '')}">${RUN_STATUS[r.status] || esc(r.status)}</td></tr>`).join('')}</table>`] : null});
  if (['running', 'done', 'error'].includes(ex.status)) list.push({key: 'export', name: '导出图库', active: ex.status === 'running', warn: ex.status === 'error',
    line: ex.status === 'running' ? '正在导出…' : ex.status === 'done' ? `已导出 ${(ex.bytes / 1048576).toFixed(0)} MB` : `导出失败：${ex.error || ''}`, body: '', action: ''});
  return list;
}
// Things that wait for the person: an authorization, characters to add, a sync that needs a hand.
function taskAsks() {
  const asks = [], p = state.provenance, missing = state.index?.check?.missing || [], s = state.sync;
  if (provenanceArmed && p?.available) asks.push(`<div class="task-ask prov-confirm"><b>找回画师这次要访问外网，确认后才会发出：</b>
    <ul><li>发给 Danbooru：图片的像素哈希和感知哈希（IQDB 签名，都是一串文本，不是图片）、画师名和账号</li>
    <li>${state.accounts?.pixiv?.bound ? '用你绑定的 Pixiv 登录状态读取你的关注列表（只读）' : '没有绑定 Pixiv：只看公开作品，不含 R18'}</li>
    <li>本次最多 ${number(p.budgets?.full || 1024)} MB；遇到拒绝、限流或验证码就停，不绕过</li></ul>
    <p class="muted-note">授权只对这一次有效，下次开始会再问。图片本身不会上传。查过的图和画师不会重查，可以随时停止。</p>
    <div class="prov-confirm-btns"><button class="primary" data-prov="start" data-network="1">授权并开始</button><button class="quiet" data-prov="start">只跑本地（签名识别）</button><button class="quiet" data-prov="cancel">取消</button></div></div>`);
  if (missing.length && !INDEX_ACTIVE.includes(state.index.state.status)) asks.push(`<div class="task-ask"><span>上次检查发现 <b>${number(missing.length)}</b> 个还没收录的角色</span><button class="primary small" data-task="index-add">查看并添加…</button></div>`);
  if (s.status === 'needs_windows') asks.push('<div class="task-ask"><span>这次新加的图超过本机识别上限：可以用 Windows 批量识别，或在设置的「图库」里调高上限后重新同步。</span></div>');
  return asks;
}
function renderTasks() {
  const list = taskList(), active = list.filter(t => t.active), warn = list.some(t => t.warn), asks = taskAsks();
  const button = $('taskButton'), badge = $('taskBadge');
  button.classList.toggle('busy', !!active.length);
  $('taskLabel').textContent = active.length > 1 ? `${active.length} 项进行中` : active.length ? active[0].name.replace(/^(文件夹|照片)/, '') + '中' : '任务';
  button.title = active.length ? active.map(t => `${t.name}：${t.line}`).join('\n') : asks.length ? '有事等你确认' : warn ? '有任务出错了' : '任务：同步、检查新角色、找回画师…';
  badge.hidden = !asks.length && !warn; badge.className = 'task-badge' + (warn && !asks.length ? ' warn' : '');
  if (!taskPanel.open) return;
  const order = [...active, ...list.filter(t => !t.active)];
  const html = `<div class="task-top"><h2>任务</h2><button class="icon" data-task="close" aria-label="关闭">×</button></div>
    ${asks.length ? `<h6>等你确认</h6><div class="task-asks">${asks.join('')}</div>` : ''}
    <h6>${active.length ? '进行中与其他任务' : '全部任务'}</h6><div class="view-group task-group">${order.map(t => `<div class="task-item${t.active ? ' on' : ''}">
      <div class="task-main"><span class="local-dot${t.active ? ' busy' : t.warn ? ' warn' : /未启用|没有开/.test(t.line) ? ' off' : ''}"></span><span class="task-text"><b>${t.name}</b><small>${esc(t.line)}</small></span>${t.action}</div>
      ${t.body ? `<div class="task-body">${t.body}</div>` : ''}${t.log ? fold('task-' + t.key, t.log[0], t.log[1]) : ''}</div>`).join('')}</div>
    <p class="task-foot">各任务是否自动运行，在<a class="link" href="#/settings">设置</a>里改。</p>`;
  const pop = $('taskPanel');
  // Polled while jobs run: redraw only on change, so a click is never lost to a replaced button.
  if (pop.dataset.html === html) return;
  pop.dataset.html = html; pop.innerHTML = html; wireFolds(pop);
}
async function taskTick() {
  clearTimeout(taskPanel.timer);
  const jobs = [pollSync(), pollIndex(), pollNames(), loadProvenance()];
  // The similar job's state comes with /api/settings, which also measures the caches: read it only when looked at.
  if (taskPanel.open && Date.now() - taskPanel.settingsAt > (SIMILAR_ACTIVE.includes(state.settings?.tasks?.similar?.status) ? 2500 : 15000)) { taskPanel.settingsAt = Date.now(); jobs.push(settingsRequest('/api/settings').then(s => { state.settings = s; }).catch(e => error(readError(e)))); }
  if (taskPanel.open && !state.libraries) jobs.push(loadLibraries());
  await Promise.allSettled(jobs);
  renderTasks();
  taskPanel.timer = setTimeout(taskTick, taskPanel.open ? 3000 : 30000);
}
function toggleTasks(open = !taskPanel.open) {
  taskPanel.open = open; $('taskPanel').hidden = !open;
  $('taskButton').setAttribute('aria-expanded', open); $('taskButton').classList.toggle('on', open);
  if (open) { delete $('taskPanel').dataset.html; renderTasks(); taskTick(); }
  else provenanceArmed = false;
}
$('taskButton').onclick = e => { e.stopPropagation(); toggleTasks(); };
$('taskPanel').addEventListener('click', async e => {
  e.stopPropagation();
  const b = e.target.closest('[data-task]'); if (!b) { if (e.target.closest('a[href]')) toggleTasks(false); return; }
  const act = b.dataset.task;
  if (act === 'close') { toggleTasks(false); return; }
  if (act === 'index-add') { openIndexAdd(); return; }
  b.disabled = true;
  try {
    if (act === 'sync') { await post('/api/sync', {}); state.sync = {status: 'scanning'}; }
    else if (act === 'index') { if (!await checkIndex()) return; }
    else if (act === 'names') { state.names = {...state.names, ...await post('/api/names', {action: 'refresh'})}; toast('已开始重新获取角色名称'); }
    else if (act === 'similar') { await post('/api/similar', {}); state.settings = await api('/api/settings'); }
  } catch (err) { toast(err.message); } finally { b.disabled = false; }
  taskTick();
});
document.addEventListener('click', e => { if (taskPanel.open && !e.target.closest('#taskPanel, dialog')) toggleTasks(false); });
document.addEventListener('keydown', e => { if (e.key === 'Escape' && taskPanel.open && !document.querySelector('dialog[open]')) { toggleTasks(false); e.stopImmediatePropagation(); } }, true);

// ---------- 画师抽查: acceptance of 找回画师 (docs/PROVENANCE.md section 8) ----------
// A fixed random sample (the server orders by a hash of the id), answered with the same 是 TA / 不是 as the detail
// panel, so every answer is in the undo history and fixes the library at the same time.
const AUDIT_METHODS = {pixel_hash: ['像素哈希', '和 Danbooru 上的帖子是同一个文件，画师取自帖子的画师标签'],
  gallery_match: ['本地比对', '和 TA 公开作品的小图逐张比对一致'], ocr: ['签名识别', '图上的签名或水印']};
const auditMethod = a => a.methods?.includes('pixel_hash') ? 'pixel_hash' : a.methods?.[0] || '';
async function renderArtistAudit() {
  if (state.route.page !== 'artist-audit') return;
  $('grid').hidden = true; $('empty').hidden = true; $('charIndex').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = false;
  closeInspector(); $('selectionBar').hidden = true; $('resultCount').textContent = '';
  $('hint').textContent = '点原帖或画师主页对照 · 看不出来就先留着 · 都可以撤销';
  const n = Number(prefs.get('artistAuditN', 50)) || 50, scroll = $('viewport').scrollTop;
  let d;
  try { d = await api('/api/artist-audit?n=' + n); } catch (e) { $('groupsPage').innerHTML = `<p class="muted-note">${esc(readError(e))}</p>`; return; }
  if (state.route.page !== 'artist-audit') return;
  const pairs = d.items.flatMap(it => it.artists.map(a => ({...a, method: auditMethod(a)})));
  const tally = list => { const yes = list.filter(a => a.confirmed).length, no = list.filter(a => a.rejected).length; return {yes, no, done: yes + no, all: list.length}; };
  const t = tally(pairs), acc = t.done ? t.yes / t.done : null;
  const byMethod = Object.keys(AUDIT_METHODS).map(m => [m, tally(pairs.filter(a => a.method === m))]).filter(([, x]) => x.all);
  const link = (url, label) => `<a class="link" href="${esc(url)}" target="_blank" rel="noreferrer noopener">${esc(label)}</a>`;
  const artistRow = (it, a) => {
    const m = auditMethod(a), [mName, mHint] = AUDIT_METHODS[m] || [m || '其他', ''];
    const data = `data-uuid="${esc(it.uuid)}" data-artist="${esc(a.id)}" data-name="${esc(a.name)}"`;
    const judged = a.confirmed ? `<span class="aa-verdict yes">✓ 对</span><button class="aa-b" data-aa="" ${data}>改</button>`
      : a.rejected ? `<span class="aa-verdict no">✗ 不对</span><button class="aa-b" data-aa="" ${data}>改</button>`
      : `<button class="aa-b yes" data-aa="yes" ${data}>对</button><button class="aa-b" data-aa="no" ${data}>不对</button>`;
    return `<div class="aa-artist"><div class="aa-top"><a class="artist-name" href="${hashFor({page: 'library', artist: a.id})}" title="在图库里只看 TA 的图">${esc(a.name)}</a><small>库里 ${number(a.count)} 张</small></div>
      ${a.homes.length ? `<div class="aa-links">主页 ${a.homes.map(h => link(h.url, h.label)).join(' · ')}</div>` : ''}
      <div class="aa-how" title="${esc(mHint)}">${esc(mName)}</div>
      <div class="aa-btns">${judged}</div></div>`;
  };
  const card = (it, i) => {
    const state_ = it.artists.some(a => a.rejected) ? ' no' : it.artists.length && it.artists.every(a => a.confirmed) ? ' yes' : '';
    return `<div class="aa-card${state_}"><a class="aa-img" href="/image?id=${encodeURIComponent(it.uuid)}&size=full" target="_blank" title="新标签页打开大图"><img src="/image?id=${encodeURIComponent(it.uuid)}&size=medium" alt="" loading="lazy"><span class="aa-no">${i + 1}</span></a>
      <div class="aa-body">${it.artists.map(a => artistRow(it, a)).join('')}${it.posts?.length ? `<div class="aa-links">原帖 ${it.posts.map(p => link(p.url, p.site)).join(' · ')}</div>` : ''}</div></div>`;
  };
  $('groupsPage').innerHTML = `<div class="aa-summary">
      <div class="prov-stat"><b>${acc === null ? '—' : (acc * 100).toFixed(1) + '%'}</b><span>准确率<small>目标 ≥ 95%</small></span></div>
      <div class="prov-stat"><b>${number(t.done)}<em> / ${number(t.all)}</em></b><span>已判断<small>对 ${number(t.yes)} · 不对 ${number(t.no)}</small></span></div>
      ${byMethod.map(([m, x]) => `<div class="prov-stat"><b>${x.done ? (x.yes / x.done * 100).toFixed(0) + '%' : '—'}</b><span>${esc(AUDIT_METHODS[m][0])}<small>${number(x.done)} / ${number(x.all)} 已判</small></span></div>`).join('')}
    </div>
    <p class="muted-note">从 ${number(d.pool)} 张有画师的图里按固定的随机顺序取前 ${number(d.items.length)} 张，每次打开都是同一批。判断会写进图库（「不对」的画师这张图就不再显示），和详情面板里的「是 TA / 不是」一样可以撤销。</p>
    <div class="aa-grid">${d.items.map(card).join('')}</div>
    ${d.items.length < d.pool ? '<p class="aa-more"><button class="quiet" id="aaMore">再加 10 张</button></p>' : ''}`;
  $('viewport').scrollTop = scroll;
  for (const b of $('groupsPage').querySelectorAll('[data-aa]')) b.onclick = async () => {
    if (state.busy) return; state.busy = true; b.disabled = true;
    try {
      const r = await post('/api/artist/verdict', {ids: [b.dataset.uuid], artist: b.dataset.artist, verdict: b.dataset.aa || null, name: b.dataset.name});
      setUndo(r.undo); toast(r.undo.label, true);
    } catch (e) { toast(e.message); }
    finally { state.busy = false; setUndo(state.undo); }
    renderArtistAudit();
  };
  $('aaMore')?.addEventListener('click', () => { prefs.set('artistAuditN', n + 10); renderArtistAudit(); });
}

// ---------- artists (provenance/, read-only here): shown only when found, never as an empty field ----------
const artistNames = {};
// ---------- 按需查找画师 (docs/PROVENANCE.md section 9): the click authorises this one lookup ----------
const LOOKUP_NOTE = '查找画师会把所选图片的像素哈希和 IQDB 签名（一串文本，不是图片）发给 Danbooru，按结果显示画师和原帖。\n「找 TA 的其他图」会下载这位画师公开作品的小缩略图，只在本机比对，比完不留图。\n\n以后点这两个按钮不再提示。继续吗？';
let lookupTimer = null;
async function lookupAllowed() {
  try { if (localStorage.getItem('artistLookupOk')) return true; } catch {}
  if (!await confirmAction(LOOKUP_NOTE, '查找画师')) return false;
  try { localStorage.setItem('artistLookupOk', '1'); } catch {}
  return true;
}
async function artistLookup(body, label, after) {
  const token = state.token;
  if (!await lookupAllowed() || token !== state.token) return;
  try { await api('/api/provenance', {method: 'POST', body: JSON.stringify(body), headers: {'X-CSRF-Token': token}}); } catch (err) { toast(err.message); return; }
  toast(label);
  clearTimeout(lookupTimer);
  const poll = async () => {
    let p;
    try { p = await api('/api/provenance'); } catch { lookupTimer = setTimeout(poll, 2000); return; }
    const s = p.state || {};
    if (['running', 'stopping'].includes(s.status)) { lookupTimer = setTimeout(poll, 1500); return; }
    const found = s.found || 0;
    toast(['done', 'stopped'].includes(s.status) ? (found ? `找到 ${number(found)} 张图的画师` : '没有找到新的画师')
      : `查找没有完成：${s.reason || s.status}`);
    after?.(found);
  };
  lookupTimer = setTimeout(poll, 1500);
}
const findArtists = (ids, after) => artistLookup({action: 'lookup', ids}, ids.length > 1 ? `正在查找 ${number(ids.length)} 张图的画师…` : '正在查找画师…', after);
const findArtistWorks = (artist, name, after) => artistLookup({action: 'artist', artist}, `正在找 ${name || '这位画师'} 的其他图…`, after);
function artistName(id) {
  if (!(id in artistNames)) {
    artistNames[id] = '…';
    api('/api/artist?' + new URLSearchParams({id})).then(r => { artistNames[id] = r.name || '未知画师'; if (state.route.artist === id) renderHeading(); }).catch(e => error(readError(e)));
  }
  return artistNames[id];
}
function artistSection(d, ident) {
  const find = `<div class="artist-empty"><span class="muted-note">${d.maybe?.length ? '还没确定画师' : '还不知道画师'}</span><button class="quiet" data-artist-find title="用像素哈希和 IQDB 查这张图的画师，几秒钟（只发哈希文本，不发图片）">查找画师</button></div>`;
  if (!d.artists?.length && !d.posts?.length && !d.maybe?.length) return `<section class="inspector-section"><h3 class="section-title">画师</h3>${find}</section>`;
  const link = (url, label, title = '') => `<a class="link" href="${esc(url)}" target="_blank" rel="noreferrer noopener"${title ? ` title="${esc(title)}"` : ''}>${esc(label)}</a>`;
  const judge = a => a.confirmed ? `<span class="artist-judge"><span class="artist-ok" title="你确认过">✓ 已确认</span><button data-artist-verdict="" data-artist="${esc(a.id)}" data-name="${esc(a.name)}" title="撤回确认">撤回</button></span>`
    : `<span class="artist-judge"><button data-artist-verdict="yes" data-artist="${esc(a.id)}" data-name="${esc(a.name)}" title="确认是 TA 画的">是 TA</button><button data-artist-verdict="no" data-artist="${esc(a.id)}" data-name="${esc(a.name)}" title="不是 TA 画的：这张不再显示这位画师">不是</button></span>`;
  const artist = a => `<div class="artist-row"><div class="artist-top"><a class="artist-name" href="${hashFor({page: 'library', artist: a.id})}" title="在图库里只看 TA 的图">${esc(a.name)}</a>${judge(a)}</div>
      ${a.count > 1 ? `<a class="artist-more" href="${hashFor({page: 'library', artist: a.id})}">库里还有 ${number(a.count - 1)} 张</a>` : ''}
      ${a.homes.length ? `<div class="artist-homes">${a.homes.map(h => link(h.url, h.label, `在 ${h.site} 打开 TA 的主页`)).join(' · ')}</div>` : ''}
      <button class="quiet artist-find-more" data-artist-more="${esc(a.id)}" data-name="${esc(a.name)}" title="下载 TA 公开作品的小缩略图，在本机比对出库里 TA 的其他图（一两分钟）">找 TA 的其他图</button></div>`;
  // 「也许是 TA」: a group recommendation or a signature read on the picture (native/terms/src/http_artists.rs); 是 TA settles it.
  const maybe = a => `<div class="artist-row artist-maybe"><div class="artist-top"><span class="artist-maybe-name">也许是 <a class="artist-name" href="${hashFor({page: 'library', artist: a.id})}" title="在图库里只看 TA 的图">${esc(a.name)}</a></span>
      <span class="artist-judge"><button data-artist-verdict="yes" data-artist="${esc(a.id)}" data-name="${esc(a.name)}" title="确认是 TA 画的">是 TA</button><button data-artist-verdict="no" data-artist="${esc(a.id)}" data-name="${esc(a.name)}" title="不是 TA 画的：不再推荐">不是</button></span></div>
      <div class="artist-reason">${esc(a.reason || '')}</div>
      ${a.homes.length ? `<div class="artist-homes">${a.homes.map(h => link(h.url, h.label, `在 ${h.site} 打开 TA 的主页`)).join(' · ')}</div>` : ''}
      <button class="quiet artist-find-more" data-artist-more="${esc(a.id)}" data-name="${esc(a.name)}" title="下载 TA 公开作品的小缩略图，在本机比对：是同一张图就能确定">找 TA 的其他图</button></div>`;
  return `<section class="inspector-section"><h3 class="section-title">画师</h3>${(d.artists || []).map(artist).join('')}${(d.maybe || []).map(maybe).join('')}${!d.artists?.length ? find : ''}
    ${d.posts?.length ? `<div class="artist-posts"><span>原帖</span>${d.posts.map(p => link(p.url, p.site)).join(' · ')}</div>` : ''}</section>`;
}
(async () => {
  try {
    const bootNsfw = state.nsfw;
    const boot = await api('/api/bootstrap?' + new URLSearchParams({nsfw: bootNsfw})); state.token = boot.token; setUndo(boot.undo);
    if (boot.read_only) toast(boot.read_only);   // the library could not be switched to the terms core: read-only
    navCount('countLibrary', boot.total);
    window.splash?.status(`正在整理 ${number(boot.total)} 张图…`);
    const meta = refreshMeta({boot: bootNsfw === state.nsfw ? boot : null});
    if (parseHash().char || parseHash().page === 'characters') await meta;
    await applyRoute();
    await meta;
    window.splash?.ready();
    taskTick();
  } catch (e) { window.splash?.ready(); error('启动失败：' + e.message + '。请检查启动终端，然后刷新页面。'); }
})();

// Pick a local work (or the empty choice); resolves null when cancelled.
function chooseWork(title, works, selected = '', emptyLabel = '未归类 / 无上级') {
  return new Promise(resolve => {
    const d = document.createElement('dialog'); d.className = 'palette profile-dialog work-dialog';
    d.innerHTML = `<form method="dialog"><div class="dialog-top"><h2></h2><button type="button" class="icon" data-close aria-label="关闭">×</button></div>
      <select name="work" class="work-choice"><option value="">${esc(emptyLabel)}</option>${works.map(w => `<option value="${esc(w.id)}" ${w.id === selected ? 'selected' : ''}>${esc(w.name)}${w.qualifier ? ' · ' + esc(w.qualifier) : ''}</option>`).join('')}</select>
      <div class="dialog-actions"><button value="cancel" class="quiet">取消</button><button value="ok" class="primary">确认</button></div></form>`;
    d.querySelector('h2').textContent = title;
    d.querySelector('[data-close]').onclick = () => d.close();
    document.body.append(d); d.onclose = () => { const value = d.returnValue === 'ok' ? d.querySelector('select').value : null; d.remove(); resolve(value); }; d.showModal();
  });
}
