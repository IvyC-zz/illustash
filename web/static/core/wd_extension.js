// WD 角色扩展（#550；后端 native/terms/src/app_wd_extension.rs）：扩展包的下载、导入和删除（整个 App 一份），以及本图库的
// 启用、重新检查和撤回。不碰 DOM。GET /api/wd-extension 的回应在 state.wdExtension，只由这里写入；下载、导入或启用、撤回
// 进行期间每秒读一次。onWdExtension(fn) 在每次读到或改动后调用 fn(before)，before 是改动前的状态。
// 接口的格式见 #550 的「接口与界面方案」第 3 节。
import {session, api} from './session.js';
import {state} from './state.js';
import {listeners} from './events.js';
const changed = listeners();
export const onWdExtension = changed.on;

const PATH = '/api/wd-extension';
export const extDownloading = (x = state.wdExtension) => x?.download?.status === 'running';
export const extRunning = (x = state.wdExtension) => x?.task?.status === 'running';
// 几次读取会重叠（设置页渲染、任务面板、轮询）：只采用最后发出的那次；换了会话的回应丢掉（#34）。
let timer = 0, poll = 0;
async function adopt(value, mine, token) {
  if (mine !== poll || token !== session.token) return state.wdExtension;
  const before = state.wdExtension;
  state.wdExtension = value;
  clearTimeout(timer);
  if (extDownloading(value) || extRunning(value)) timer = setTimeout(() => loadWdExtension().catch(() => {}), 1000);
  await changed.emit(before);
  return value;
}
export function loadWdExtension() {
  const mine = ++poll, token = session.token;
  return api(PATH).then(value => adopt(value, mine, token));
}
// 没有副作用的 POST：不经 post()，免得启动一次任务就被当成图库写入。回复都是完整的状态；不带路径的导入先打开系统的选择框，
// 关掉时回复 {cancelled}。
async function send(path, body) {
  const mine = ++poll, token = session.token;
  const value = await api(path, {method: 'POST', body: JSON.stringify(body)});
  return value.cancelled ? value : adopt(value, mine, token);
}
// download、import、cancel、discard、delete。
export const packageAction = action => send(PATH + '/package', {action});
// 启用和重新检查是同一个路由。
export const enableExtension = () => send(PATH + '/enable', {});
export const withdrawExtension = () => send(PATH + '/withdraw', {});
// 只在扫描阶段有效；写入阶段后端回 409。
export const cancelExtension = (id = state.wdExtension?.task?.id) => send(PATH + '/cancel', {id});

// 刚结束的启用或撤回：同一个任务从运行变成别的状态时返回一次，否则为 null。
export function extFinished(before, after = state.wdExtension) {
  const was = before?.task, now = after?.task;
  return was?.status === 'running' && now && now.id === was.id && now.status !== 'running' ? now : null;
}
// 刚结束的下载或导入。
export const extDownloadFinished = (before, after = state.wdExtension) =>
  extDownloading(before) && after?.download && !extDownloading(after) ? after.download : null;
