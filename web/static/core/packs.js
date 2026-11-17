// 角色数据包（#550 WD 角色扩展、#559 CCIP 长尾包）：包的下载、导入和删除，以及本图库的启用、重新检查和撤回。不碰 DOM。
// 每个包一个路由，回复同一个形状（{package, download, library, task}，见 #550「接口与界面方案」第 3 节和 #559 的接口评论），
// 存在 state.packs[包]，只由这里写入；下载、导入或启用、撤回进行期间每秒读一次。onPack(fn) 在每次读到或改动后调用
// fn(包, before)，before 是这个包改动前的状态。
import {session, api} from './session.js';
import {state} from './state.js';
import {listeners} from './events.js';
const changed = listeners();
export const onPack = changed.on;

// 包 → 路由。界面上的名字和文字在 ui/packs.js。签名目录里的新版本（#602）另读 GET /api/packs，存在 state.packCatalog：
// {catalog, wd: {current, offer}, ccip: {current, offer}}；读到后对两个包各通知一次 onPack（before 是没变的那个包状态）。
export const PACKS = {wd: '/api/wd-extension', ccip: '/api/ccip-longtail'};
export const packDownloading = x => x?.download?.status === 'running';
export const packRunning = x => x?.task?.status === 'running';
// 几次读取会重叠（设置页渲染、任务面板、轮询）：每个包只采用最后发出的那次；换了会话的回应丢掉（#34）。
const timers = {}, polls = {};
async function adopt(key, value, mine, token) {
  if (mine !== polls[key] || token !== session.token) return state.packs[key];
  const before = state.packs[key];
  state.packs = {...state.packs, [key]: value};
  clearTimeout(timers[key]);
  if (packDownloading(value) || packRunning(value)) timers[key] = setTimeout(() => loadPack(key).catch(() => {}), 1000);
  await changed.emit(key, before);
  // 下载、导入或启用（换包）刚结束：卡片上的版本和「新版本」那一行可能变了，重读一次目录。
  if (packDownloadFinished(before, value) || packFinished(before, value)) loadCatalog().catch(() => {});
  return value;
}
let catalogPoll = 0;
export async function loadCatalog() {
  const mine = ++catalogPoll, token = session.token;
  const value = await api('/api/packs');
  if (mine !== catalogPoll || token !== session.token) return state.packCatalog;
  state.packCatalog = value;
  for (const key of Object.keys(PACKS)) await changed.emit(key, state.packs[key]);
  return value;
}
// 卡片上「有新版本」的那个版本（没有时为 null）。
export const packOffer = key => state.packCatalog?.[key]?.offer || null;
export function loadPack(key) {
  const mine = polls[key] = (polls[key] || 0) + 1, token = session.token;
  return api(PACKS[key]).then(value => adopt(key, value, mine, token));
}
// 没有副作用的 POST：不经 post()，免得启动一次任务就被当成图库写入。回复都是完整的状态；不带路径的导入先打开系统的选择框，
// 关掉时回复 {cancelled}。
async function send(key, path, body) {
  const mine = polls[key] = (polls[key] || 0) + 1, token = session.token;
  const value = await api(PACKS[key] + path, {method: 'POST', body: JSON.stringify(body)});
  return value.cancelled ? value : adopt(key, value, mine, token);
}
// download、import、cancel、discard、delete。下载带 version 时下载目录里的那个版本（#602）。
export const packageAction = (key, action, version) => send(key, '/package', version ? {action, version} : {action});
// 启用和重新检查是同一个路由；带 version 时换成那个版本。
export const enablePack = (key, version) => send(key, '/enable', version ? {version} : {});
export const withdrawPack = key => send(key, '/withdraw', {});
// 只在扫描阶段有效；写入阶段后端回 409。
export const cancelPack = (key, id = state.packs[key]?.task?.id) => send(key, '/cancel', {id});

// 刚结束的启用、重新检查、自动检查或撤回：同一个任务从运行变成别的状态时返回一次，否则为 null。
export function packFinished(before, after) {
  const was = before?.task, now = after?.task;
  return was?.status === 'running' && now && now.id === was.id && now.status !== 'running' ? now : null;
}
// 刚结束的下载或导入。
export const packDownloadFinished = (before, after) => packDownloading(before) && after?.download && !packDownloading(after) ? after.download : null;
