// What the settings pages read and change (#199): startup switches, folding look-alike pictures, backups and the
// thumbnail cache, the interface language, the names' network consent and model API, libraries and their sources,
// linked accounts and update checks. No DOM. The data is still in core/state.js (settings, fold, about, names,
// libraries, accounts, pixivWaiting, updates) until the page state is split; outside core/ nothing writes those
// fields. onSettings(what, detail) tells after each read or change; what is data, libraries, accounts, pixiv or
// updates.
import {t, said} from './i18n.js';
import {session, api, post, shared, leaveSession} from './session.js';
import {state} from './state.js';
import {listeners} from './events.js';
const changed = listeners();
export const onSettings = changed.on;

// ---------- the settings page's data ----------
let loading = null;
// Reads what the page has not got yet (everything with refresh), all at once. Each part is announced as it arrives
// (onSettings('data', {key, error})), so the useful ones show before the slow ones; a read for a library the page
// has since left is dropped.
export function loadSettingsData(refresh = false) {
  if (loading) return loading;
  const token = session.token;
  const missing = [['settings', '/api/settings'], ['fold', '/api/fold'], ['index', '/api/index'], ['names', '/api/names']]
    .filter(([key]) => refresh || !state[key]);
  if (!missing.length) return;
  loading = Promise.all(missing.map(async ([key, path]) => {
    let error = null;
    try { const value = await shared(path); if (token !== session.token) return; state[key] = key === 'fold' ? value.kinds : value; }
    catch (e) { if (token !== session.token) return; error = e; }
    await changed.emit('data', {key, error});
  })).finally(() => { loading = null; });
  return loading;
}
// Launch-time switches; --no-* flags still win for the current launch.
export async function saveStartup(body) { state.settings = await post('/api/startup', body); }
export async function saveFold(kinds) { state.fold = (await post('/api/fold', {kinds})).kinds; }
export async function backupNow() { const r = await post('/api/backup', {}); state.settings = null; return r; }
export async function clearCache() { const r = await post('/api/cache', {action: 'clear'}); state.settings = r; return r; }
// App-wide (preferences.json, #173): every window and library follows it; the page then reloads in the new language.
export const saveLanguage = language => api('/api/preferences', {method: 'POST', body: JSON.stringify({language})});
export async function loadAbout() {
  const token = session.token, value = await shared('/api/about');
  if (token === session.token) state.about = value;
}

// ---------- names: network consent (shared with Danbooru search) and the optional model API ----------
export async function allowNames(allow) {
  state.names = {...state.names, ...await api('/api/names', {method: 'POST', body: JSON.stringify({action: 'network', allow})})};
}
// A Danbooru name lookup (tag and work names only): its results, or why it failed. Without this library's consent
// the backend refuses with 显示名联网尚未授权; any other failure is a real network or service error.
export const remoteNames = path => api(path).then(found => ({found}), e => ({found: null, unauthorized: said(e.said, '显示名联网尚未授权'), reason: e.message}));
export const remoteNote = r => r.unauthorized ? t('查 Danbooru 需要先允许这个图库联网查询名称（只发送文字）') : t('Danbooru 暂时连不上：{reason}', {reason: r.reason});
export async function saveModelApi(body) { state.names = {...state.names, ...await post('/api/names', {action: 'model_api', ...body})}; }
export const testModelApi = () => post('/api/names', {action: 'model_api_test'});

// ---------- libraries (docs/LIBRARIES.md) ----------
export async function loadLibraries() {
  try { state.libraries = await api('/api/libraries'); } catch { state.libraries = null; }
  await changed.emit('libraries');
}
// POST /api/libraries/<action>, with the token of the page that asked: a confirmation may outlast its session.
export const libraryRequest = (action, body = {}, token = session.token) =>
  api('/api/libraries/' + action, {method: 'POST', body: JSON.stringify(body), headers: {'X-CSRF-Token': token}});
// Follows a running export, for up to an hour.
export async function pollExport() {
  for (let i = 0; i < 2400 && state.libraries?.export?.status === 'running'; i++) { await new Promise(r => setTimeout(r, 1500)); await loadLibraries(); }
}
// The native folder chooser, several at once; throws "Unsupported…" where there is none.
export async function chooseFolders() {
  const r = await api('/api/libraries/choose', {method: 'POST', body: JSON.stringify({kind: 'folder', multiple: true})});
  return r?.paths || [];
}
// Opening another library restarts the server on it. `agreed` is what the person agreed to first: 'upgrade' an older
// one (#193) or 'rescue' its damaged source.sqlite (#228). This page then belongs to the library it leaves: it stops
// reading and writing at once. Throws what the backend refused.
export async function openLibraryRequest(path, agreed = null) {
  await api('/api/libraries/open', {method: 'POST', body: JSON.stringify(agreed ? {path, [agreed]: true} : {path})});
  leaveSession();
}
// Whether the server came back on that library, within about 80 s.
export async function libraryOpened(path) {
  for (let i = 0; i < 120; i++) {
    await new Promise(r => setTimeout(r, 700));
    try {
      const s = await fetch('/api/settings').then(r => r.json());
      if (s.package?.path === path) return true;
    } catch { /* the server is restarting */ }
  }
  return false;
}

// ---------- linked accounts (docs/PROVENANCE.md section 8): shared by every library; secrets live in the Keychain ----------
export async function loadAccounts() {
  try { state.accounts = await api('/api/accounts'); } catch { state.accounts = null; }
  await changed.emit('accounts');
}
export const accountRequest = (action, body = {}) => api('/api/accounts/' + action, {method: 'POST', body: JSON.stringify(body)});
// Logging in to Pixiv in the desktop app's window: polled every 2 s for up to 10 minutes, one request at a time; an
// answer for a wait that was stopped or replaced changes nothing. The end is told as onSettings('pixiv', {bound}) with
// the account, or {error}.
let pixivPoll = null;
export function stopPixivLogin() {
  if (pixivPoll) {
    clearTimeout(pixivPoll.timer);
    pixivPoll.controller.abort();
    pixivPoll = null;
  }
  state.pixivWaiting = false;
}
export function waitPixivLogin() {
  stopPixivLogin();
  const poll = {controller: new AbortController(), timer: null, until: Date.now() + 10 * 60e3};
  pixivPoll = poll;
  state.pixivWaiting = true; changed.emit('accounts');
  const tick = async () => {
    if (pixivPoll !== poll) return;
    if (Date.now() >= poll.until) { stopPixivLogin(); changed.emit('accounts'); return; }
    let r;
    try {
      r = await api('/api/accounts/pixiv/connect', {method: 'POST', body: '{}', signal: poll.controller.signal});
    } catch (error) {
      if (pixivPoll !== poll) return;
      stopPixivLogin(); changed.emit('pixiv', {error}); loadAccounts(); return;
    }
    if (pixivPoll !== poll) return;
    if (r.bound) { stopPixivLogin(); changed.emit('pixiv', {bound: r}); loadAccounts(); }
    else if (!r.login_open || Date.now() >= poll.until) { stopPixivLogin(); changed.emit('accounts'); }
    else poll.timer = setTimeout(tick, 2000);
  };
  poll.timer = setTimeout(tick, 2000);
}

// ---------- update checks (#191, native/terms/src/app_updates.rs) ----------
// Off until the person allows it. Once allowed the backend checks at each launch and once a day while the app stays
// open; the page only asks whether a check is due. Nothing is downloaded or installed here.
let updatesAt = 0, updatesPending = null;
export async function updatesPost(body) {
  state.updates = await api('/api/updates', {method: 'POST', body: JSON.stringify(body)});
  return state.updates;
}
// A manual check: always goes out.
export async function checkUpdates() { const u = await updatesPost({action: 'check'}); updatesAt = Date.now(); return u; }
// From the task panel's tick: refresh the cached state now and then and run a due automatic check; onSettings('updates')
// tells when it is through, whatever came of it.
export function updatesTick() {
  if (updatesPending) return updatesPending;
  updatesPending = (async () => {
    if (!state.updates || Date.now() - updatesAt > 600000) { state.updates = await api('/api/updates'); updatesAt = Date.now(); }
    if (state.updates.due) await updatesPost({action: 'check', auto: true});
  })().catch(() => {}).finally(() => { updatesPending = null; changed.emit('updates'); });
  return updatesPending;
}
