// 角色资料包（#602；原来分开的 #550 WD 角色扩展和 #559 CCIP 长尾包合成一个包）：包的下载、导入和删除，本图库的启用、
// 换包、撤回，以及签名目录里的新版本。不碰 DOM。状态是 GET /api/packs 的回复（形状见 native/terms/src/http_host_get.rs
// 的 `packs`），存在 state.pack，只由这里写入；下载、导入或启用、撤回进行期间每秒读一次。onPack(fn) 在每次读到或改动后
// 调用 fn(before)，before 是改动前的状态。两个组成部分各自的旧路由（/api/wd-extension、/api/ccip-longtail）页面不再读。
import {session, api} from './session.js';
import {state} from './state.js';
import {listeners} from './events.js';
const changed = listeners();
export const onPack = changed.on;

export const PACK_ROUTE = '/api/packs';
export const packDownloading = x => x?.download?.status === 'running';
export const packRunning = x => x?.task?.status === 'running';
// 几次读取会重叠（设置页渲染、任务面板、轮询）：只采用最后发出的那次；换了会话的回应丢掉（#34）。
let timer = 0, poll = 0;
async function adopt(value, mine, token) {
  if (mine !== poll || token !== session.token) return state.pack;
  const before = state.pack;
  state.pack = value;
  clearTimeout(timer);
  if (packDownloading(value) || packRunning(value)) timer = setTimeout(() => loadPack().catch(() => {}), 1000);
  await changed.emit(before);
  return value;
}
export function loadPack() {
  const mine = ++poll, token = session.token;
  return api(PACK_ROUTE).then(value => adopt(value, mine, token));
}
// 没有副作用的 POST：不经 post()，免得启动一次任务就被当成图库写入。回复都是完整的状态；不带路径的导入先打开系统的选择框，
// 关掉时回复 {cancelled}。
async function send(body) {
  const mine = ++poll, token = session.token;
  const value = await api(PACK_ROUTE, {method: 'POST', body: JSON.stringify(body)});
  return value.cancelled ? value : adopt(value, mine, token);
}
// download（带 version 时下载目录里的那个版本）、import、cancel（下载或导入）、discard、delete。
export const packAction = (action, version) => send(version ? {action, version} : {action});
// 启用和重新检查是同一个动作；带 version 时换成那个版本。
export const enablePack = version => send(version ? {action: 'enable', version} : {action: 'enable'});
export const withdrawPack = () => send({action: 'withdraw'});
// 已有图库在默认启用的提示里选了不用。
export const declinePack = () => send({action: 'decline'});
// 只在扫描阶段有效；写入阶段后端回 409。
export const cancelPack = (id = state.pack?.task?.id) => send({action: 'cancel', id});
// 卡片上「有新版本」的那个版本（没有时为 null）。
export const packOffer = (x = state.pack) => x?.offer || null;

// 刚结束的启用、重新检查、自动检查或撤回：同一个任务从运行变成别的状态时返回一次，否则为 null。
export function packFinished(before, after) {
  const was = before?.task, now = after?.task;
  return was?.status === 'running' && now && now.id === was.id && now.status !== 'running' ? now : null;
}
// 刚结束的下载或导入。
export const packDownloadFinished = (before, after) => packDownloading(before) && after?.download && !packDownloading(after) ? after.download : null;
