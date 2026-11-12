// 页面在哪（#199）：把地址（#/page/section?query）读成路由再写回去，以及一个路由是哪类页面。不碰 DOM：它读写
// location.hash 和 history。
import {prefs} from './prefs.js';
import {matchPath, pageOf, pathFor, viewOf} from './pages.js';
import {state} from './state.js';

// ---------- routing ----------
export function parseHash() {
  const [path, query = ''] = location.hash.replace(/^#/, '').split('?');
  const parts = path.split('/').filter(Boolean), q = new URLSearchParams(query);
  const route = {page: 'library', q: q.get('q') || '', tag: q.get('tag') || '', utag: q.get('utag') || '', dup: q.get('dup') || '', reason: q.get('reason') || '', kind: q.get('kind') || '', maybe: q.get('maybe') || '', sort: q.get('sort') || '',
    work: q.get('work') || '', day: q.get('day') || '', artist: q.get('artist') || '', char: q.get('char') || '', variant: q.get('variant') || '', check: q.get('check') || '', doubt: q.get('doubt') || ''};
  // 打开 App 落在首页；只有查询的地址（#/?artist=… 和其他旧链接）仍表示图库；#/c/id/skin 是旧的角色页，现在是按这个角色
  // 筛选的图库；资料库和设置有各自地址的子视图。哪个路径对应哪个页面由 core/pages.js 登记。
  Object.assign(route, matchPath(parts, !!query));
  // 角色、画师和检查筛选只属于图库（有图库工具栏的页面）。
  if (pageOf(route).toolbar !== 'library') Object.assign(route, {char: '', variant: '', check: '', doubt: '', artist: ''});
  return route;
}
export function hashFor(r) {
  const path = pathFor(r);
  const q = new URLSearchParams();
  if (r.work) q.set('work', r.work); if (r.day) q.set('day', r.day); if (r.artist) q.set('artist', r.artist);
  if (r.char) q.set('char', r.char); if (r.variant) q.set('variant', r.variant); if (r.check) q.set('check', r.check); if (r.doubt) q.set('doubt', r.doubt);
  if (r.q) q.set('q', r.q); if (r.dup) q.set('dup', r.dup); if (r.reason) q.set('reason', r.reason); if (r.kind) q.set('kind', r.kind); if (r.maybe) q.set('maybe', r.maybe); if (r.tag) q.set('tag', r.tag); if (r.utag) q.set('utag', r.utag); if (r.sort && r.sort !== defaultSort(r)) q.set('sort', r.sort);
  return path + (q.toString() ? '?' + q : '');
}
export const defaultSort = r => r.char ? prefs.get('sortCharacter', 'suspicious') : viewOf(r).lead ? prefs.get('sortUnmatched', prefs.get('sortLibrary', 'newest')) : prefs.get('sortLibrary', 'newest');
export function go(route) { location.hash = hashFor(route); }
// 角色是图库的一个筛选，不是单独的页面。
export const charRoute = (id, variant = '') => ({page: 'library', char: id, variant});
export const charHash = (id, variant = '') => hashFor(charRoute(id, variant));
export function replaceRoute(patch) { Object.assign(state.route, patch); history.replaceState(null, '', hashFor(state.route)); }
// 整理视图（按角色筛选，或复核池）：点击即选择。
export const cleanup = () => viewOf(state.route).click === 'cleanup';
// 浏览页：点击打开详情，选择要主动开启。
export const browsing = () => viewOf(state.route).click === 'browse';
