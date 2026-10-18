// Requests and the session (#33, #199). No DOM: a layout learns that the page stopped through onStale() and that a
// write went through through onPost().
// The page belongs to the session it booted with. Another window or caller that switches (or reopens) the library
// starts a new session; adopting its token would send this page's selection, dialogs and queued work to that library.
// The page stops instead, and drops responses that arrive afterwards.
// Only this module writes session.token and session.stale; everything else reads them.
import {t, tb, said, I18N} from './i18n.js';
import {listeners} from './events.js';
export const session = {token: '', stale: false};
export const STALE = t('图库已在其他窗口切换或重新打开，请重新载入页面');
const stopped = listeners(), written = listeners();
// fn() runs once, when the page stops. Each returns a function that removes the listener again.
export const onStale = stopped.on;
// fn(path, body) runs after every write the backend accepted.
export const onPost = written.on;
export function staleSession() {
  if (session.stale) return;
  session.stale = true;
  stopped.emit();
}
export function adoptToken(token) {
  if (!session.token) session.token = token;
  else if (token !== session.token) staleSession();
  return !session.stale;
}
export async function api(path, options = {}) {
  if (session.stale) throw new Error(STALE);
  // A page in another language says so: the backend then shows English or romanized names (#173).
  const language = I18N.language === 'zh-CN' ? {} : {'X-Illustash-Language': I18N.language};
  const response = await fetch(path, {...options, headers: {'Content-Type': 'application/json', 'X-CSRF-Token': session.token, ...language, ...options.headers}});
  const result = await response.json();
  if (response.status === 403 && said(result.error, '会话已过期，请刷新页面')) staleSession();
  if (session.stale) throw new Error(STALE);
  // The backend's message in the page's language; e.said keeps it as sent, to test with said.
  if (!response.ok) throw Object.assign(new Error(tb(result.error) || t('请求失败')), {said: result.error});
  return result;
}
export async function post(path, body) {
  const result = await api(path, {method: 'POST', body: JSON.stringify(body)});
  if (path === '/api/suggest') return result;  // read-only; POST only carries the selection
  written.emit(path, body);
  return result;
}
// One read per path in flight: whoever asks for the same path meanwhile shares its answer (the settings page reads
// several cards' data at once, and the task panel some of the same).
const reads = new Map();
export function shared(path) {
  if (!reads.has(path)) reads.set(path, api(path).finally(() => reads.delete(path)));
  return reads.get(path);
}
export function readError(e) { return t('读取失败：{reason}', {reason: e?.message || String(e)}); }
