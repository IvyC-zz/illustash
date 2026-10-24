// The 资料库 (#199): characters grouped by work and how many came in lately, search, the work tree, and the watch list
// of the external index. No DOM; the shelves, lists and dialogs are ui/catalog.js. state.chars, state.tagged,
// state.workTree, state.workNames, state.localLinks and state.index are still in core/state.js.
import {t} from './i18n.js';
import {work} from './format.js';
import {api, post} from './session.js';
import {state} from './state.js';
// Recent activity per character ({recent, last}), read once per filter and data version.
export const catalog = {activity: new Map(), activityKey: ''};
export const recentOf = c => catalog.activity.get(c.id)?.recent || 0;
// Whether a new read was needed and made.
export async function loadActivity() {
  const key = state.nsfw + '|' + state.readEpoch; if (catalog.activityKey === key) return false;
  const act = await api('/api/character/activity?nsfw=' + state.nsfw);
  if (!act) return false;
  catalog.activity = new Map(Object.entries(act)); catalog.activityKey = key;
  return true;
}
// The next loadActivity() reads again (another filter shows other pictures).
export function forgetActivity() { catalog.activityKey = ''; }
export const searchCharacters = q => api('/api/characters?min=1&q=' + encodeURIComponent(q) + '&nsfw=' + state.nsfw);
export async function loadTagged() { state.tagged = await api('/api/tagged?' + new URLSearchParams({nsfw: state.nsfw})); }
export async function loadWorkTree() { state.workTree = await api('/api/works'); }
// Own characters whose name is a Danbooru tag (asks Danbooru with those names only; none → no request). Whether the
// answer came.
export async function loadLocalLinks() {
  const found = await api('/api/local-links').catch(() => null);
  if (found === null) return false;
  state.localLinks = found;
  return true;
}

// ---------- characters by work ----------
export const workKey = c => c.kind === 'oc' ? 'oc' : c.work_id || 'other';
// The groups that are not works (原创角色, 未归作品) carry pseudo and come after the works (#293).
export function castWorks() {
  const names = new Map((state.workTree || []).map(w => [w.id, w.name])), groups = new Map();
  for (const c of state.chars) {
    const k = workKey(c);
    if (!groups.has(k)) groups.set(k, {key: k, name: k === 'oc' ? t('原创角色') : k === 'other' ? t('未归作品') : c.work_name || names.get(k) || t('未命名作品'), pseudo: k === 'oc' || k === 'other', chars: [], images: 0, recent: 0});
    const g = groups.get(k); g.chars.push(c); g.images += c.count; g.recent += recentOf(c);
  }
  for (const g of groups.values()) g.chars.sort((a, b) => b.count - a.count);
  return [...groups.values()].sort((a, b) => a.pseudo - b.pseudo || b.images - a.images);
}
export const PSEUDO_NOTES = {oc: t('画师的原创角色，不属于任何作品'), other: t('还没填所属作品的角色。在角色的「编辑资料」里填上作品，就会归到那部作品下')};
// The list's size buckets: [key, label, test].
export const BUCKETS = [['all', t('全部'), () => true], ['1', t('1 张'), c => c.count === 1], ['2', t('2–4 张'), c => c.count >= 2 && c.count < 5], ['5', t('5–19 张'), c => c.count >= 5 && c.count < 20], ['20', t('20–99 张'), c => c.count >= 20 && c.count < 100], ['100', t('100 张以上'), c => c.count >= 100]];
// The list's rows: base (all characters, or a search) in one bucket, sorted by a column (dir 1 or -1).
export function castRows(base, {bucket, sort, dir}) {
  const test = BUCKETS.find(b => b[0] === bucket)?.[2] || (() => true);
  const key = {name: c => c.name, work: c => c.work_name || '', count: c => c.count, recent: recentOf, last: c => catalog.activity.get(c.id)?.last || '', skins: c => (c.variants || []).filter(v => v.count).length}[sort];
  return base.filter(test).sort((a, b) => { const x = key(a), y = key(b); return (x > y ? 1 : x < y ? -1 : 0) * dir; });
}

// ---------- works and the watch list (#198) ----------
// Works some Danbooru tags name, for watch lists from before local works existed.
export const WORK_NAMES = {blue_archive: t('蔚蓝档案'), 'arknights:_endfield': t('明日方舟：终末地'), wuthering_waves: t('鸣潮'), 'meitantei_precure!': t('名侦探光之美少女'),
  genshin_impact: t('原神'), 'honkai:_star_rail': t('崩坏：星穹铁道'), honkai_impact_3rd: t('崩坏 3'), zenless_zone_zero: t('绝区零'), arknights: t('明日方舟')};
export const workName = w => state.workNames?.get(w) || WORK_NAMES[w] || work(w);
// Local works whose name, alias or tag matches q (all when empty).
export const searchWorks = q => api('/api/works?' + new URLSearchParams({q}));
export async function loadIndex() { const x = await api('/api/index'); if (x) state.index = x; return x; }
// The work tree as shown: depth-first under each parent, bigger works first; a child whose parent is gone shows at the
// top level. filter is all, watched or pending; q matches a name or tag.
export function shelfRows(x, {q = '', filter = 'all'}) {
  const watched = new Set(x.watch.works), ids = new Set(x.works.map(w => w.id)), kids = new Map();
  for (const w of x.works) { const p = ids.has(w.parent_id) ? w.parent_id : ''; if (!kids.has(p)) kids.set(p, []); kids.get(p).push(w); }
  for (const list of kids.values()) list.sort((a, b) => (b.count || 0) - (a.count || 0) || a.name.localeCompare(b.name, 'zh'));
  const rows = [], walk = (p, depth) => { for (const w of kids.get(p) || []) { rows.push([w, depth]); walk(w.id, depth + 1); } };
  walk('', 0);
  q = q.trim().toLowerCase();
  const tests = {all: () => true, watched: w => watched.has(w.id), pending: w => w.status === 'pending'};
  return {rows: rows.filter(([w]) => tests[filter](w) && (!q || w.name.toLowerCase().includes(q) || (w.tag || '').toLowerCase().includes(q))),
    watched, pending: x.works.filter(tests.pending).length};
}
export async function saveWatch(works) { state.index = await post('/api/index', {action: 'watch', works, min_posts: state.index.watch.min_posts}); }
// The local work to watch for a Danbooru work from the search: the one with its tag, else the one the person picks
// (choose(title, works) resolves to an id, '' for a new work, null to cancel), linked or created with that tag.
// Resolves to its id, or null when cancelled.
export async function watchTarget(r, choose) {
  const x = state.index;
  let id = r.id || x.works.find(w => w.tag === r.tag)?.id;
  if (!id) {
    const chosen = await choose(t('Danbooru 的 {tag} 对应哪个本地作品？', {tag: r.tag}), x.works);
    if (chosen === null) return null;
    id = (await post('/api/works', chosen ? {action: 'link', id: chosen, tag: r.tag} : {action: 'create', name: r.name || r.tag, tag: r.tag})).id;
    state.index = await api('/api/index');
  }
  return id;
}
// A new work (id null) with its parent in one request, or an edit of one; the index is read again. Resolves to its id.
export async function saveWork(id, {name, parent, tag}) {
  if (!id) id = (await post('/api/works', {action: 'create', name, ...(tag ? {tag} : {}), ...(parent ? {parent} : {})})).id;
  else await post('/api/works', {action: 'edit', id, name, parent, tag});
  state.index = await api('/api/index');
  return id;
}
