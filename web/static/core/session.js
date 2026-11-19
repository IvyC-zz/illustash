// 请求与会话（#33、#199）。不碰 DOM：布局通过 onStale() 得知页面已停下，通过 onPost() 得知一次写入已生效。
// 页面属于它启动时的那个会话。另一个窗口或调用方切换（或重新打开）图库会开始新的会话；采用它的 token 会把这个页面的
// 选择、对话框和排队的工作发到那个图库去。所以页面改为停下，并丢掉之后才到的回应。
// 只有本模块写 session.token 和 session.stale；其他地方只读。
import {t, tb, said, I18N, sentences} from './i18n.js';
import {listeners} from './events.js';
export const session = {token: '', stale: false};
export let lastDiagnostic = null;
export const STALE = t('图库已在其他窗口切换或重新打开，请重新载入页面');
const stopped = listeners(), written = listeners();
// fn() 在页面停下时运行一次。每个都返回一个再把这个监听去掉的函数。
export const onStale = stopped.on;
// fn(path, body) 在后端接受的每次写入之后运行。
export const onPost = written.on;
export function staleSession() {
  if (session.stale) return;
  session.stale = true;
  stopped.emit();
}
// 页面正在离开它的图库（打开另一个会重启服务）：重新载入前不再读写，也不显示横幅，因为马上就会重新载入。
export function leaveSession() { session.stale = true; }
export function adoptToken(token) {
  if (!session.token) session.token = token;
  else if (token !== session.token) staleSession();
  return !session.stale;
}
export async function api(path, options = {}) {
  if (session.stale) throw new Error(STALE);
  // 别的语言的页面会说明自己的语言：后端于是显示英文或罗马字名字（#173）。
  const language = I18N.language === 'zh-CN' ? {} : {'X-Illustash-Language': I18N.language};
  const response = await fetch(path, {...options, headers: {'Content-Type': 'application/json', 'X-CSRF-Token': session.token, ...language, ...options.headers}});
  const result = await response.json();
  if (response.status === 403 && said(result.error, '会话已过期，请刷新页面')) staleSession();
  if (session.stale) throw new Error(STALE);
  // 后端的消息，用页面的语言显示；e.said 保留发来时的原文，供 said 比对；e.status 是 HTTP 状态码。
  if (!response.ok) {
    let d = null;
    try { d = JSON.parse(response.headers?.get('X-Illustash-Diagnostic') || 'null'); } catch {}
    if (d?.id) lastDiagnostic = d;
    const message = sentences(tb(result.error) || t('请求失败'), d?.id && t('错误编号：{id}', {id: d.id}), d?.id && !d.stored && t('诊断记录未保存'));
    throw Object.assign(new Error(message), {said: result.error, diagnostic: d, status: response.status});
  }
  return result;
}
export async function post(path, body) {
  const result = await api(path, {method: 'POST', body: JSON.stringify(body)});
  if (path === '/api/suggest') return result;  // 只读；用 POST 只是为了带上选择集
  written.emit(path, body);
  return result;
}
// 同一路径同一时间只读一次：期间请求同一路径的都共用它的回应（设置页会同时读好几张卡片的数据，任务面板也读其中一些）。
const reads = new Map();
export function shared(path) {
  if (!reads.has(path)) reads.set(path, api(path).finally(() => reads.delete(path)));
  return reads.get(path);
}
export function readError(e) { return t('读取失败：{reason}', {reason: e?.message || String(e)}); }
