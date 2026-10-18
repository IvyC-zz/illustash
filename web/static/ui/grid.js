// The picture grid (desktop layout, #199): loading pages of pictures, the virtual grid and its zoom, selecting, the
// selection bar's actions, keyboard shortcuts, view options and the R18 filter.
import {forgetActivity, loadActivity} from '../core/catalog.js';
import {edits, setLatest, undoLatest, undoneText} from '../core/edits.js';
import {esc, isMac, number, shortcut} from '../core/format.js';
import {t, tb} from '../core/i18n.js';
import {REASONS} from '../core/labels.js';
import {metaSoon, refreshMeta} from '../core/meta.js';
import {prefs} from '../core/prefs.js';
import {LEAD_PAGES, browsing, cleanup, defaultSort, panelPage} from '../core/route.js';
import {api, post, readError} from '../core/session.js';
import {state} from '../core/state.js';
import {$, error, toast} from './dom.js';
import {renderChrome, renderHeading} from './shell.js';
import {findArtists} from './artists.js';
import {answerAsk, closeInspector, closeViewer, ensureVisible, indexOf, inspect, openViewer, pickAlternative, showViewer, singleAction, step, zoomView} from './detail.js';
import {drawHome, renderHome} from './home.js';
import {jumpToCharacter, markOc, newIds, openPicker, openRelate, targets} from './palette.js';
import {renderArtistAudit, renderGroups, renderIdentity, renderSimilar} from './review.js';

// ---------- asset loading ----------
// Zoom is a continuous target column width; the grid snaps to whole columns and animates between them.
const SIZE_MIN = 72, SIZE_MAX = 480, MAX_HEIGHT = 3e7;
const sliderToSize = v => Math.round(SIZE_MIN * Math.pow(SIZE_MAX / SIZE_MIN, v / 100));
const sizeToSlider = s => Math.round(100 * Math.log(s / SIZE_MIN) / Math.log(SIZE_MAX / SIZE_MIN));
export const PAGE = 100;
// The page reads in flight, and the debounce of reading the pages around the viewport.
let abortList, pageTimer;
export function filters() {
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
export async function reload(reset = false) {
  if (panelPage(state.route.page)) return;
  const epoch = state.readEpoch, route = state.route;
  const gen = ++state.generation; abortList?.abort(); abortList = new AbortController(); state.pending.clear();
  $('charIndex').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = true; $('grid').hidden = false; error('');
  if (reset) { $('viewport').scrollTop = 0; $('resultCount').textContent = t('正在读取…'); state.totalReady = false; }
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
    $('empty').innerHTML = state.route.char && !narrowed ? t('这个角色已经没有图片了。') : { unmatched: t('全部认出来了 🎉'), skipped: t('没有跳过的图片。'), duplicates: state.route.dup ? t('所有副本的角色都一致。') : t('没有重复的图片。'), excluded: t('没有被排除的图片。'), pending: t('没有待确认的图片。'), group: t('这一组都整理完了。'), recent: t('最近一次同步没有新图。'), audit: t('抽查做完了，谢谢！结果会用来衡量认人准不准。'), library: t('没有符合条件的图片。试试别的搜索或标签。')}[state.route.page] || '';
    renderHeading();
    if (state.route.char) refreshMaybe(gen);
  } catch (e) { if (e.name !== 'AbortError' && gen === state.generation && route === state.route) { error(e.message); $('resultCount').textContent = t('读取失败'); } }
}
export async function loadPage(offset) {
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
  $('resultCount').textContent = t('{n} 张', {n: number(state.total)}) + (loaded < state.total ? ' · ' + t('已加载 {n}', {n: number(loaded)}) : '');
  $('hint').textContent = state.route.char ? t('点击或拖动选择 · Space 看大图 · ⌫ 不是 TA · C 换成') : {character: t('点击或拖动选择 · Space 看大图 · ⌫ 不是 TA · C 换成'), unmatched: t('点击或拖动选择 · Space 看大图 · C 设为角色 · S 跳过'),
    skipped: t('点击或拖动选择 · Space 看大图'), excluded: t('点击或拖动选择 · Space 看大图 · X 恢复收录'), pending: t('点击或拖动选择 · Space 看大图 · X 排除'), audit: t('点一张或 Space 看大图 · Y 是 · N 不是 · 看不清可以跳过，换一张'), duplicates: t('同色角标为同一组 · 修改会同步到整组 · 角色不一致的组对齐后自动折叠'), library: t('点击查看 · 拖动或 {key} 点击多选 · Space 看大图', {key: isMac ? '⌘' : 'Ctrl'})}[state.route.page] || '';
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
export const imageRatio = item => { const r = rawRatio(item); return r ? Math.min(FIT_MAX, Math.max(FIT_MIN, r)) : fitEstimate; };
export const rowCount = () => state.fit ? fitRows : Math.ceil(state.total / state.cols);
export const rowStart = row => state.fit ? rowStarts[Math.max(0, Math.min(row, fitRows))] : Math.max(0, Math.min(row * state.cols, state.total));
export function rowOf(i) {
  if (!state.fit) return Math.floor(i / state.cols);
  let lo = 0, hi = Math.max(0, fitRows - 1);
  while (lo < hi) { const mid = (lo + hi + 1) >>> 1; if (rowStarts[mid] <= i) lo = mid; else hi = mid - 1; }
  return lo;
}
export function rowTop(row) { return state.fit ? rowTops[Math.max(0, Math.min(row, fitRows))] : row * state.rowH; }
export function rowHeight(row) { return state.fit ? rowHeights[row] || state.rowH : state.rowH; }
export function rowAt(y) {
  if (!state.fit) return Math.max(0, Math.floor(y / state.rowH));
  let lo = 0, hi = fitRows;
  while (lo < hi) { const mid = (lo + hi) >>> 1; if (rowTops[mid + 1] <= y) lo = mid + 1; else hi = mid; }
  return Math.min(lo, Math.max(0, fitRows - 1));
}
export function cardRect(i) {
  const row = rowOf(i);
  if (!state.fit) return {left: (i % state.cols) * (state.cellW + state.gap), top: rowTop(row), width: state.cellW, height: state.rowH - state.gap};
  const h = rowHeight(row) - state.gap - captionH();
  let left = 0; for (let j = rowStart(row); j < i; j++) left += imageRatio(state.items[j]) * h + state.gap;
  return {left, top: rowTop(row), width: imageRatio(state.items[i]) * h, height: h + captionH()};
}
// Arrow up/down: the image in the neighbouring row whose centre is closest horizontally.
export function verticalStep(i, dir) {
  if (!state.fit) return Math.max(0, Math.min(state.total - 1, i + dir * state.cols));
  const row = rowOf(i) + dir; if (row < 0 || row >= fitRows) return dir < 0 ? 0 : state.total - 1;
  const box = cardRect(i), x = box.left + box.width / 2;
  let best = rowStart(row), dist = Infinity;
  for (let j = rowStart(row); j < rowStart(row + 1); j++) { const b = cardRect(j), d = Math.abs(b.left + b.width / 2 - x); if (d < dist) { dist = d; best = j; } }
  return best;
}
const captionH = () => viewOpts.captions && browsing() && state.cellW >= 150 ? 28 : 0;
export function toggleFit() {
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
export function layout() {
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
export function indexAt(clientX, clientY, clamp = true) {
  const rect = $('grid').getBoundingClientRect(), x = clientX - rect.left, y = clientY - rect.top;
  const row = rowAt(y), start = rowStart(row), end = rowStart(row + 1);
  let i = start;
  while (i < end - 1) { const box = cardRect(i); if (x < box.left + box.width + state.gap) break; i++; }
  if (!clamp && (x < 0 || y < 0 || y >= rowTop(rowCount()))) return -1;
  return clamp ? Math.max(0, Math.min(i, state.total - 1)) : i;
}
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
  const tag = (id, name, cls, title) => `<span class="atag ${cls}" title="${esc(title)}"><b>${mark && cls.includes('checked') ? `<i>${mark}</i>` : ''}${esc(short(name))}</b><button data-tagact="yes" data-entity="${esc(id)}" title="${t('是 {name}', {name: esc(name)})}">✓</button><button data-tagact="no" data-entity="${esc(id)}" title="${t('不是 {name}', {name: esc(name)})}">✗</button></span>`;
  const others = item.memberships.filter(m => m.id !== a.id).map(m => tag(m.id, m.name, m.status === 'confirmed' ? 'confirmed' : '', m.status === 'confirmed' ? t('已确认') : t('机器识别')));
  return `<span class="chips atags">${tag(a.id, a.name, 'checked ' + (a.answer || 'open'), a.answer ? (a.answer === 'yes' ? t('抽查已答：是') : t('抽查已答：不是')) : t('抽查：是 TA 吗？'))}${others.join('')}</span>`;
}
function cardHTML(item, i) {
  // In justified rows a wide image is wider than the nominal cell: size its thumbnail and caption by that.
  const r = state.route, ratio = state.fit ? imageRatio(item) : 1, size = state.cellW * Math.max(1, ratio) > 260 ? 'medium' : 'thumb';
  let extra = '';
  if (r.char) {
    const m = item.memberships.find(m => m.id === r.char);
    const chips = (m?.variants || []).map(v => `<span>${esc(v.name)}</span>`).join('');
    extra = (chips && state.cellW >= 120 ? `<span class="chips">${chips}</span>` : '') + (m?.status === 'machine' ? `<span class="machine-dot" title="${t('机器识别，未检查')}"></span>` : '');
  }
  if (r.char && r.maybe && state.cellW >= 120) extra += `<span class="chips"><span>${item.memberships.length ? t('现为 {name}', {name: esc(short(item.memberships[0].name))}) : t('未识别')}</span></span>`;
  if (r.page === 'excluded' && !r.reason && item.excluded && state.cellW >= 120) extra += `<span class="chips"><span>${REASONS[item.excluded] || ''}</span></span>`;
  // 随机抽查 / 未识别 are skimmed at small sizes: their one-line badge stays down to 70 px cells.
  if (r.page === 'audit' && item.audit && state.cellW >= 70) extra += auditTags(item);
  if (viewOpts.hints && !item.memberships.length && item.hint && state.cellW >= (r.page === 'unmatched' ? 70 : 120)) extra += `<span class="chips"><button class="hint-add" data-hint title="${t('机器觉得可能是 {name}，点一下设为 TA（可撤销）', {name: esc(item.hint.name)})}">+ ${esc(short(item.hint.name))}</button></span>`;
  if (r.page === 'duplicates' && item.dup) extra += `<span class="dup-badge" style="--dup-hue:${dupHue(item.dup.grp)}" title="${t('同一组共 {n} 份副本', {n: item.dup.size})}">${item.dup.pos}/${item.dup.size}</span>`;
  else if (item.dup && state.cellW >= 70) extra += `<span class="dup-badge folded" title="${t('共 {n} 份相同副本，操作会同步到全部', {n: item.dup.size})}">×${item.dup.size}</span>`;
  // A narrow picture shows its first name and a count instead of cutting every name short; hover shows all.
  const names = item.memberships.map(m => m.name), narrow = state.fit && ratio * state.cellW * 4 / 3 < 170 && names.length > 1;
  const shown = narrow ? item.memberships.slice(0, 1) : item.memberships;
  const caption = captionH() ? `<div class="caption" title="${esc(names.join(t('、')))}">${shown.map(m => `<span class="${m.status}">${esc(m.name)}</span>`).join('<i>·</i>') || `<span class="none">${item.unknown ? t('已跳过') : t('未识别')}</span>`}${narrow ? `<small>+${names.length - 1}</small>` : ''}</div>` : '';
  const inner = `<img src="/image?id=${encodeURIComponent(item.uuid)}&size=${size}" loading="lazy" decoding="async" alt="" draggable="false">${extra}<span class="check" aria-hidden="true"></span>`;
  return `<div class="image-stage">${isSquare() ? inner : `<div class="fit-box${rawRatio(item) && rawRatio(item) !== ratio ? ' whole' : ''}">${inner}</div>`}</div>${caption}`;
}
export function renderGrid(relayout = false) {
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
      el.setAttribute('aria-label', t('{names}，第 {n} 张', {names: item.memberships.map(m => m.name).join(t('、')) || t('未识别图片'), n: i + 1}));
      el.innerHTML = cardHTML(item, i);
      el.querySelector('img').addEventListener('error', ev => { const msg = document.createElement('span'); msg.className = 'missing'; msg.textContent = t('图片暂不可读'); ev.target.replaceWith(msg); }, {once: true});
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
export function syncCards() {
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
export function scheduleGrid() {
  if (gridFrame !== null) return;
  gridFrame = requestAnimationFrame(() => { gridFrame = null; renderGrid(); });
}
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
// ---------- selection ----------
export const selCount = () => state.sel.all ? Math.max(0, state.total - state.sel.exclude.size) : state.sel.ids.size;
const isSelected = id => state.sel.all ? !state.sel.exclude.has(id) : state.sel.ids.has(id);
function setSelected(id, on) {
  if (state.sel.all) on ? state.sel.exclude.delete(id) : state.sel.exclude.add(id);
  else on ? state.sel.ids.add(id) : state.sel.ids.delete(id);
}
export function clearSelection() { state.sel = {all: false, ids: new Set(), exclude: new Set()}; state.anchor = -1; state.armed = null; renderSelection(); }
export function selectAll() { if (!state.total) return; state.sel = {all: true, ids: new Set(), exclude: new Set()}; state.selecting = true; renderSelection(); }
export function toggleSelecting() { state.selecting = !state.selecting; if (!state.selecting) clearSelection(); renderSelection(); }
function selectionPayload() { return state.sel.all ? {filter: filters(), exclude: [...state.sel.exclude]} : {ids: [...state.sel.ids]}; }
async function selectRange(a, b, on = true) {
  const lo = Math.min(a, b), hi = Math.max(a, b);
  if (hi - lo + 1 > 5000) { toast(t('一次最多连选 5,000 张；需要更多请用 {key}', {key: shortcut('A')})); return; }
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
// ---------- selection bar & actions ----------
function renderSelection() {
  syncCards();
  const n = selCount(), bar = $('selectionBar'), r = state.route;
  bar.hidden = !n; if (!n) { state.armed = null; return; }
  const big = n > 500, arm = key => state.armed === key ? ` armed` : '';
  const count = `<button class="icon" id="clearSel" title="${t('取消选择（Esc）')}">×</button><span class="sel-count">${state.sel.all ? t('已选全部 {n} 张', {n: number(n)}) : t('已选 {n} 张', {n: number(n)})}</span><div class="bar-divider"></div>`;
  let buttons = '';
  const ocButton = `<button class="quiet" data-act="oc" title="${t('画师自己的原创角色：统一标为「原创角色（未细分）」，以后需要时再按相似细分')}">${t('原创角色')}</button>`;
  if (r.char && r.maybe) {
    const name = short(state.char?.name || t('该角色'));
    buttons = `<button class="primary${arm('isme')}" data-act="isme">${state.armed === 'isme' ? t('再点一次：{n} 张设为 {name}', {n: number(n), name: esc(name)}) : t('是 {name}', {name: esc(name)})}</button><button class="quiet${arm('remove')}" data-act="remove">${state.armed === 'remove' ? t('再点一次：{n} 张不是', {n: number(n)}) : t('不是 TA')} <kbd>⌫</kbd></button>`;
  } else if (r.char) {
    const name = state.char?.name || t('该角色');
    buttons = `<button class="primary${arm('remove')}" data-act="remove">${state.armed === 'remove' ? t('再点一次：移出 {n} 张', {n: number(n)}) : t('不是 {name}', {name: esc(short(name))})} <kbd>⌫</kbd></button><button class="quiet" data-act="swap">${t('换成…')} <kbd>C</kbd></button><button class="quiet" data-act="also">${t('也是…')} <kbd>A</kbd></button>${state.char?.variants?.length ? `<button class="quiet" data-act="variant">${t('子标签')} <kbd>T</kbd></button>` : ''}`;
  } else if (r.page === 'audit') {
    // 随机抽查: each image asks about its own character (the dark badge); answer a whole selection at once.
    buttons = `<button class="primary" data-act="audit_yes" title="${t('选中的图都是角标上的角色')}">${t('都是')} <kbd>Y</kbd></button><button class="quiet" data-act="audit_no" title="${t('选中的图都不是角标上的角色')}">${t('都不是')} <kbd>N</kbd></button>`;
  } else if (r.page === 'unmatched' || r.page === 'group') {
    buttons = `<button class="primary" data-act="assign">${t('设为角色…')} <kbd>C</kbd></button>${ocButton}<button class="quiet${arm('skip')}" data-act="skip">${state.armed === 'skip' ? t('再点一次：跳过 {n} 张', {n: number(n)}) : t('跳过')} <kbd>S</kbd></button>`;
  } else if (r.page === 'excluded') {
    buttons = `<button class="primary${arm('include')}" data-act="include">${state.armed === 'include' ? t('再点一次：恢复 {n} 张', {n: number(n)}) : t('恢复收录')} <kbd>X</kbd></button>`;
  } else if (r.page === 'pending') {
    buttons = `<button class="primary${arm('exclude')}" data-act="exclude">${state.armed === 'exclude' ? t('再点一次：排除 {n} 张', {n: number(n)}) : t('排除')} <kbd>X</kbd></button><button class="quiet${arm('include')}" data-act="include">${state.armed === 'include' ? t('再点一次：保留 {n} 张', {n: number(n)}) : t('保留')}</button>`;
  } else if (r.page === 'skipped') {
    buttons = `<button class="primary${arm('resume')}" data-act="resume">${state.armed === 'resume' ? t('再点一次：恢复 {n} 张', {n: number(n)}) : t('恢复到未识别')}</button><button class="quiet" data-act="assign">${t('设为角色…')} <kbd>C</kbd></button>${ocButton}`;
  } else {
    buttons = `<button class="primary" data-act="assign">${t('加角色…')} <kbd>C</kbd></button><button class="quiet" data-act="notlib" ${state.sel.all ? `disabled title="${t('全选时请进入角色页移出')}"` : ''}>${t('不是…')} <kbd>⌫</kbd></button>`;
  }
  if (!['excluded', 'pending'].includes(r.page)) buttons += `<button class="quiet${arm('exclude')}" data-act="exclude" title="${t('不是插画（截图、照片等）：从图库移到「已排除」')}">${state.armed === 'exclude' ? t('再点一次：排除 {n} 张', {n: number(n)}) : t('排除')} <kbd>X</kbd></button>`;
  buttons += `<button class="quiet" data-act="tag">${t('标签')} <kbd>G</kbd></button>`;
  buttons += `<button class="quiet" data-act="findartist" title="${t('用像素哈希和 IQDB 查这些图的画师（只发哈希文本，不发图片；一次最多 500 张）')}">${t('查找画师')}</button>`;
  if (!state.sel.all && n >= 2 && n <= 12) buttons += `<button class="quiet" data-act="relate" title="${t('两两比较：同一张图、裁剪、差分、局部编辑，还是不同的图（只在本机计算）')}">${t('判断关系')}</button>`;
  bar.innerHTML = count + buttons + (big ? `<span class="bar-note">${t('超过 500 张需再点一次确认')}</span>` : '');
  $('clearSel').onclick = () => { state.selecting = false; clearSelection(); };
  for (const b of bar.querySelectorAll('[data-act]')) b.onclick = () => runBarAction(b.dataset.act);
}
export const short = name => name.length > 28 ? name.slice(0, 26) + '…' : name;
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
  const n = selCount(), r = state.route; if (!n || edits.busy) return;
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
  else if (act === 'swap') openPicker({title: t('把 {n} 张换成…', {n: number(n)}), sentence: e => t('把 {n} 张从 {from} 换成 {name}', {n: number(n), from: fromName, name: e.name}), exclude: [r.char], suggest: selectionPayload(), multi: true,
    pick: list => mutate('/api/reassign', {...payload(), old: r.char, new: newIds(list)}, true)});
  else if (act === 'also') openPicker({title: t('这 {n} 张也是…', {n: number(n)}), sentence: e => t('给 {n} 张加上 {name}', {n: number(n), name: e.name}), exclude: [r.char], suggest: selectionPayload(), multi: true,
    pick: list => mutate('/api/change', {...payload(), action: 'confirmed', ...targets(list)}, false)});
  else if (act === 'assign') openPicker({title: browsing() ? t('给 {n} 张加角色', {n: number(n)}) : t('把 {n} 张设为…', {n: number(n)}), sentence: e => t('把 {n} 张设为 {name}', {n: number(n), name: e.name}), suggest: selectionPayload(), multi: true,
    pick: list => mutate('/api/change', {...payload(), action: 'confirmed', ...targets(list)}, !browsing())});
  else if (act === 'oc') markOc(payload(), true);
  else if (act === 'relate') openRelate([...state.sel.ids]);
  else if (act === 'variant') openVariantMenu(payload);
  else if (act === 'tag') openTagPicker(payload, n, state.sel.all ? null : state.items.filter(x => x && isSelected(x.uuid)));
  else if (act === 'notlib') {
    const present = new Map(); for (const id of state.sel.ids) { const item = state.items.find(x => x?.uuid === id); for (const m of item?.memberships || []) present.set(m.id, m); }
    if (!present.size) { toast(t('所选图片还没有角色')); return; }
    openPicker({title: t('这些图不是…'), items: [...present.values()], sentence: e => t('把 {name} 从 {n} 张中移出', {name: e.name, n: number(n)}), pick: e => mutate('/api/change', {...payload(), action: 'rejected', entity: e.id}, false)});
  }
}
// Human tags reuse the palette: existing tags, adopt a WD tag, or create one. Items already on
// every selected image are offered for removal.
export function openTagPicker(payload, n, loaded, leaving = () => !!state.route.utag) {
  const present = new Map();
  for (const item of loaded || []) for (const tag of item.user_tags || []) present.set(tag, (present.get(tag) || 0) + 1);
  openPicker({title: t('标签'), tagMode: true, present, placeholder: t('输入标签，或搜索 WD 通用标签…'),
    sentence: e => e.remove ? t('从 {n} 张去掉标签 {tag}', {n: number(n), tag: e.tag}) : t('给 {n} 张加上标签 {tag}', {n: number(n), tag: e.tag}),
    pick: e => mutate('/api/tag', {...payload(), tag: e.tag, add: !e.remove}, e.remove && leaving())});
}
function openVariantMenu(payload) {
  const r = state.route, n = selCount(), items = [];
  if (r.variant) { const v = state.char.variants.find(v => v.id === r.variant); if (v) items.push({id: v.id, name: v.name, remove: true, hint: t('从所选图片去掉')}); }
  for (const v of state.char.variants) if (v.id !== r.variant) items.push({id: v.id, name: v.name, hint: t('{n} 张', {n: number(v.count)}), counted: true});
  openPicker({title: t('子标签'), items, placeholder: t('筛选子标签'), sentence: e => e.remove ? t('从 {n} 张去掉 {name}', {n: number(n), name: e.name}) : t('给 {n} 张加上 {name}', {n: number(n), name: e.name}),
    pick: e => mutate('/api/change', {...payload(), action: e.remove ? 'remove_variant' : 'add_variant', entity: e.id}, !!e.remove)});
}

// Writes: fade leaving cards at once, close the gaps as soon as the server agrees,
// then swap in fresh data without losing the scroll position.
// Editing the image open in the viewer or the detail panel never moves you to another one: the list may reorder
// (suspicious-first puts confirmed images last) or drop it, but it stays on screen so several edits can follow,
// and ←/→ then continue from the neighbours it had before the first edit. Only 随机抽查 answers advance.
// The picture kept in view while the list under it changes: {uuid, item, prev, next}.
export const kept = {hold: null};
function holdCurrent() {
  const uuid = state.viewer >= 0 ? state.viewerItem?.uuid : state.focused;
  if (!uuid) { kept.hold = null; return; }
  if (kept.hold?.uuid === uuid) return;
  const i = indexOf(uuid);
  kept.hold = {uuid, item: state.items[i] || state.viewerItem, prev: state.items[i - 1]?.uuid, next: state.items[i + 1]?.uuid};
}
export function currentItem() {
  const uuid = state.viewer >= 0 ? state.viewerItem?.uuid : state.focused;
  if (!uuid) return state.cursor >= 0 ? state.items[state.cursor] : null;
  const i = indexOf(uuid);
  return i >= 0 ? state.items[i] : uuid === kept.hold?.uuid ? kept.hold.item : state.viewerItem?.uuid === uuid ? state.viewerItem : null;
}
export async function mutate(path, body, leaving, {advance = false} = {}) {
  if (edits.busy) return; edits.busy = true; document.body.classList.add('busy');
  const started = performance.now();
  const advanceFrom = advance && state.viewer >= 0 ? state.viewerItem?.uuid : null;
  if (advance) kept.hold = null; else holdCurrent();
  const gone = leaving ? new Set(body.ids || state.items.filter(x => x && isSelected(x.uuid)).map(x => x.uuid)) : null;
  if (gone) for (const el of $('grid').children) if (gone.has(el.dataset.id)) el.classList.add('leaving');
  try {
    const r = await post(path, body); setLatest(r.undo);
    state.selecting = false; clearSelection();
    toast(tb(r.undo.label), true);
    if (gone) { await new Promise(done => setTimeout(done, Math.max(0, 170 - (performance.now() - started)))); collapse(gone); }
    await reload(); if (state.route.page === 'home') renderHome(true); metaSoon(path === '/api/tag');
    if (state.focused) inspect(state.focused, true);
    if (state.viewer >= 0) {
      if (advance && advanceFrom && indexOf(advanceFrom) >= 0) showViewer(Math.min(indexOf(advanceFrom) + 1, state.total - 1));
      else if (advance || !kept.hold) showViewer(Math.min(state.viewer, state.total - 1));
      else { const i = indexOf(kept.hold.uuid); showViewer(i >= 0 ? i : state.viewer, i >= 0 ? state.items[i] : state.viewerItem); }
    }
    return true;
  } catch (e) { toast(e.message); for (const el of $('grid').querySelectorAll('.leaving')) el.classList.remove('leaving'); }
  finally { edits.busy = false; document.body.classList.remove('busy'); setLatest(edits.latest); }
}
function collapse(gone) {
  const kept = []; let removed = 0;
  for (let i = 0; i < state.total; i++) { const x = state.items[i]; if (x && gone.has(x.uuid)) { removed++; continue; } kept[i - removed] = x; }
  kept.length = state.total - removed; state.items = kept; state.total = kept.length; state.dataVersion++; renderGrid(true); updateCount();
}
export async function undo() {
  if (!edits.latest || edits.busy) return; edits.busy = true;
  try {
    toast(undoneText(await undoLatest())); await reload(); refreshMeta().catch(e => error(readError(e)));
    if (state.route.page === 'home') renderHome(true);
    if (state.route.page === 'identity') renderIdentity();
    if (state.route.page === 'groups') renderGroups(true);
    if (state.route.page === 'artist-audit') renderArtistAudit();
    if (state.focused) inspect(state.focused, true);
    if (state.viewer >= 0) { const i = indexOf(state.viewerItem?.uuid); showViewer(i >= 0 ? i : state.viewer, i >= 0 ? state.items[i] : state.viewerItem); }
  } catch (e) {
    // The history may have moved on (another window, or a failure after the server committed): show its latest step.
    toast(e.message); refreshMeta().catch(err => error(readError(err)));
  } finally { edits.busy = false; setLatest(edits.latest); }
}
export async function sweep() {
  const c = state.char; if (!c?.unchecked) return;
  await mutate('/api/sweep', {entity: c.id}, false);
}

// ---------- view options (视图选项) ----------
// Everything here is optional detail: the defaults stay quiet, and each switch is saved per browser.
const VIEW_DEFAULTS = {captions: true, marks: true, hints: true, railCount: false, footer: false, chart: true, same: true, meta: true, homeSize: 1, hero: true, people: false, ask: true};
export const viewOpts = {...VIEW_DEFAULTS, ...prefs.get('view', {})};
function applyViewClasses() {
  const b = document.body.classList;
  b.toggle('hide-marks', !viewOpts.marks); b.toggle('show-rail-count', viewOpts.railCount); b.toggle('hide-footer', !viewOpts.footer);
}
const viewSwitch = (key, label, hint = '') => `<button class="view-opt" data-view="${key}" role="switch" aria-checked="${viewOpts[key]}"><span>${label}${hint ? `<small>${hint}</small>` : ''}</span><i class="view-sw${viewOpts[key] ? ' on' : ''}"></i></button>`;
function renderViewPop() {
  const grid = !panelPage(state.route.page);
  $('viewPopover').innerHTML = (state.route.page === 'home' ? `<h6>${t('首页')}</h6><div class="view-group">
      <div class="view-row"><span>${t('缩略图')}</span><div class="segmented" id="homeSizeSeg">${[[0, t('小')], [1, t('中')], [2, t('大')]].map(([v, label]) => `<button data-hsize="${v}" class="${viewOpts.homeSize === v ? 'on' : ''}">${label}</button>`).join('')}</div></div>
      ${viewSwitch('hero', t('放大每组主图'))}
      ${viewSwitch('people', t('分组显示角色数'))}
      ${viewSwitch('marks', t('图上显示副本数'))}
    </div>` : '') + (grid ? `<h6>${t('图片网格')}</h6><div class="view-group">
      <label class="view-row"><span>${t('缩略图大小')}</span><input type="range" id="zoom" min="0" max="100" step="1" value="${sizeToSlider(state.size)}" aria-label="${t('缩略图大小')}"></label>
      ${viewSwitch('captions', t('图片下方显示角色名'), t('浏览页、格子够大时'))}
      ${viewSwitch('marks', t('图上显示副本数与皮肤'))}
      ${viewSwitch('hints', t('未识别的图上显示「可能是」'), t('点一下就设为该角色'))}
    </div>` : '') + `<h6>${t('侧栏与提示')}</h6><div class="view-group">
      ${viewSwitch('railCount', t('侧栏显示数量'))}
      ${viewSwitch('footer', t('底部状态栏'), t('已加载张数与操作提示'))}
    </div><h6>${t('详情面板')}</h6><div class="view-group">
      ${viewSwitch('chart', t('收藏曲线'), t('这个角色每月收了多少张'))}
      ${viewSwitch('same', t('同一角色的其它图'))}
      ${viewSwitch('meta', t('文件信息与画面标签'))}
      ${viewSwitch('ask', t('偶尔问一句「是 TA 吗？」'), t('详情和首页；回答会用来学习认人'))}
    </div><div class="view-foot"><span>${t('设置会自动保存')}</span><button class="link-button strong" id="viewReset">${t('恢复默认')}</button></div>`;
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
// ---------- NSFW filter ----------
// One click cycles 显示 → 隐藏 → 仅 NSFW; H toggles hiding. NSFW = WD rating questionable/explicit.
// The R18 plate: hollow = everything shows, struck through = hidden, filled = R18 only.
const NSFW_TIP = {'': t('R18：显示中 · 点击隐藏（H）'), hide: t('R18：已隐藏 · 点击只看 R18'), only: t('R18：只看 R18 · 点击恢复全部')};
const R18_ICON = '<svg class="r18" viewBox="0 0 34 20" aria-hidden="true"><rect class="plate" x="1.2" y="1.2" width="31.6" height="17.6" rx="5.5"/><text x="17" y="14.1" text-anchor="middle">R18</text><path class="cut" d="M5 17.5 29 2.5"/><path class="slash" d="M5 17.5 29 2.5"/></svg>';
function renderNsfw() {
  const b = $('nsfwButton'); if (!b.firstChild) b.innerHTML = R18_ICON;
  b.className = 'nsfw-toggle s-' + (state.nsfw || 'all'); b.title = NSFW_TIP[state.nsfw]; b.setAttribute('aria-label', NSFW_TIP[state.nsfw]);
}
export function setNsfw(mode) {
  state.nsfw = mode; prefs.set('nsfw', mode); renderNsfw(); state.selecting = false; clearSelection();
  toast({'': t('已显示全部内容'), hide: t('已隐藏 R18'), only: t('只看 R18')}[mode]);
  if (!panelPage(state.route.page)) reload(true);
  else if (state.route.page === 'identity') renderIdentity();
  else if (state.route.page === 'home') renderHome();
  else if (state.route.page === 'groups') renderGroups();
  else if (state.route.page === 'similar') renderSimilar();
  else if (state.route.page === 'characters') { forgetActivity(); loadActivity(); }
  refreshMeta().catch(e => error(readError(e)));
}

// The handlers this part of the page registers. Called once, when the page starts.
export function wireGrid() {
  state.size = Math.min(SIZE_MAX, Math.max(SIZE_MIN, prefs.get('size', 185)));
  $('viewport').addEventListener('scroll', scheduleGrid, {passive: true});
  new ResizeObserver(scheduleGrid).observe($('viewport'));

  $('viewport').addEventListener('wheel', e => {
    if (!e.ctrlKey && !e.metaKey) return;
    e.preventDefault(); pinchFactor *= Math.exp(-e.deltaY * .01); pinchPoint = [e.clientX, e.clientY];
    requestZoom(() => { const f = pinchFactor; pinchFactor = 1; setSize(state.size * f, ...pinchPoint); });
  }, {passive: false});
  $('viewport').addEventListener('gesturestart', e => { e.preventDefault(); gestureStart = state.size; });
  $('viewport').addEventListener('gesturechange', e => { e.preventDefault(); const x = e.clientX, y = e.clientY, s = gestureStart * e.scale; requestZoom(() => setSize(s, x, y)); });

  // One click on a card's「+ 角色」lead: set the image to that character, same as choosing it in the picker.
  $('grid').addEventListener('click', e => {
    const hit = e.target.closest('[data-tagact]');
    if (hit) {
      const item = state.items[Number(hit.closest('.card').dataset.index)], yes = hit.dataset.tagact === 'yes', entity = hit.dataset.entity;
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
  $('grid').addEventListener('dblclick', e => { const card = e.target.closest('.card[data-id]'); if (card) openViewer(Number(card.dataset.index)); });

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
    if ((key === 'y' || key === 'n') && !mod && !edits.busy && state.route.page === 'audit' && selCount() && state.viewer < 0) { e.preventDefault(); answerAudit(key === 'y'); return; }
    if ((key === 'y' || key === 'n') && !mod && !edits.busy && state.question && [state.focused, state.viewer >= 0 && state.viewerItem?.uuid].includes(state.question.ident)) { e.preventDefault(); answerAsk(key === 'y' ? 'yes' : 'no'); return; }
    if (edits.busy || $('grid').hidden) return;
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

  $('nsfwButton').onclick = () => setNsfw({'': 'hide', hide: 'only', only: ''}[state.nsfw]);
  renderNsfw();
}
