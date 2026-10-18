// Where the page is (#199): the address (#/page/section?query) read into a route and written back, and what kind of
// page a route is. No DOM: it reads and sets location.hash and history.
import {prefs} from './prefs.js';
import {state} from './state.js';

// ---------- routing ----------
export function parseHash() {
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
export function hashFor(r) {
  let path = r.page === 'group' ? `#/g/${encodeURIComponent(r.id)}` : r.page === 'sgroup' ? `#/s/${encodeURIComponent(r.id)}` : '#/' + r.page + (r.section ? '/' + r.section : '');
  const q = new URLSearchParams();
  if (r.work) q.set('work', r.work); if (r.day) q.set('day', r.day); if (r.artist) q.set('artist', r.artist);
  if (r.char) q.set('char', r.char); if (r.variant) q.set('variant', r.variant); if (r.check) q.set('check', r.check); if (r.doubt) q.set('doubt', r.doubt);
  if (r.q) q.set('q', r.q); if (r.dup) q.set('dup', r.dup); if (r.reason) q.set('reason', r.reason); if (r.kind) q.set('kind', r.kind); if (r.maybe) q.set('maybe', r.maybe); if (r.tag) q.set('tag', r.tag); if (r.utag) q.set('utag', r.utag); if (r.sort && r.sort !== defaultSort(r)) q.set('sort', r.sort);
  return path + (q.toString() ? '?' + q : '');
}
export const LEAD_PAGES = ['unmatched', 'skipped', 'group'];
export const defaultSort = r => r.char ? prefs.get('sortCharacter', 'suspicious') : LEAD_PAGES.includes(r.page) ? prefs.get('sortUnmatched', prefs.get('sortLibrary', 'newest')) : prefs.get('sortLibrary', 'newest');
export function go(route) { location.hash = hashFor(route); }
// A character is a library filter, not a page of its own.
export const charRoute = (id, variant = '') => ({page: 'library', char: id, variant});
export const charHash = (id, variant = '') => hashFor(charRoute(id, variant));
export function replaceRoute(patch) { Object.assign(state.route, patch); history.replaceState(null, '', hashFor(state.route)); }
// Cleanup views (a character filter, or a review pool): click selects.
export const cleanup = () => !!state.route.char || ['unmatched', 'skipped', 'excluded', 'pending', 'group', 'audit'].includes(state.route.page);
// Browsing pages: click opens details, selection is opt-in.
export const browsing = () => !state.route.char && ['library', 'duplicates', 'recent'].includes(state.route.page);
// Pages that replace the image grid with their own panel.
export const panelPage = page => page === 'home' || page === 'characters' || page === 'settings' || page === 'groups' || page === 'similar' || page === 'identity';
