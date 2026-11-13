// 不带识别模型的安装包在第一次使用时下载的模型（#168，native/terms/src/app_models.rs）。和更新检查一样全应用共用：
// 每个图库、每个窗口里状态相同，切换图库时下载照常继续。不碰 DOM。onModels(before, start) 在每次读取或改动后通知，带上改动前的
// 状态，以及正在运行或刚结束的任务开始时模型是否已就位（readyWhenStarted）。
import {api} from './session.js';
import {state} from './state.js';
import {listeners} from './events.js';
const changed = listeners();
export const onModels = changed.on;

let timer = 0, reading = null, started = null, settled = null;
const adopt = async value => {
  const before = state.models, start = started;
  state.models = value;
  started = readyWhenStarted(before, value, start);
  settled = settledWd(before, value, settled);
  // 下载或导入进行期间，每秒读一次进度；它自己会结束。
  clearTimeout(timer);
  if (value?.job?.status === 'running') timer = setTimeout(() => loadModels().catch(() => {}), 1000);
  await changed.emit(before, start);
  return value;
};
export function loadModels() {
  reading ||= api('/api/models').then(adopt).finally(() => { reading = null; });
  return reading;
}
// download、cancel、discard、mirror；不带路径的 import 先打开系统的选择框（关掉时返回 {cancelled}）。
export async function modelsPost(body) {
  const value = await api('/api/models', {method: 'POST', body: JSON.stringify(body)});
  return value.cancelled ? value : adopt(value);
}
// 「使用 GPU 加速识别」（#270），全应用共用，存在 preferences.json；模型状态里带着它，所以要重新读取。
export async function saveAcceleration(on) {
  await api('/api/preferences', {method: 'POST', body: JSON.stringify({acceleration: on})});
  return loadModels();
}
// 这个安装要自己下载模型，而模型还没就位：识别要等待。
export const modelsWaiting = (m = state.models) => !!m?.on_demand && m.status !== 'ready';
// 刚结束的任务及其结局：只在读到的状态说它不再运行时返回一次。
export const modelsFinished = (before, after = state.models) => before?.job?.status === 'running' && after?.job?.status !== 'running' ? after.job : null;
// 任务开始时模型是否已就位（#375）：第一次看到它在运行时取改动前的状态，之后沿用 `known`；没有任务在运行时为 null。
// 页面中途打开、没有改动前的状态时无从判断，记为没就位：宁可多识别一次，不漏掉。
export const readyWhenStarted = (before, after, known) =>
  after?.job?.status !== 'running' ? null : before?.job?.status === 'running' ? known : before?.status === 'ready';
// 刚结束的任务让模型从没就位变成就位：图库里等着的图现在可以识别了。首次下载在同一个任务里先激活原始模型、再下 WD
// 加速文件，所以看的是任务开始时（`start`），不是结束前一刻；加速文件成功、失败还是取消都算。模型本来就在、只补或修
// 加速文件或导入的，不算。
export const modelsArrived = (before, start, after = state.models) => !!modelsFinished(before, after) && start !== true && after?.status === 'ready';
// 装好 WD 加速文件之前留下的那条 WD 回退记录（#375）的 `at`：任务没失败、结束时不再缺可选文件，并且下载完成或导入接收了
// 可选文件。否则沿用 `known`。之后读到的 `last.wd.at` 还是它，就是旧记录；下一次识别写入新的 `at` 后恢复正常判断。
// 只记在这个页面里：装好之后、下一次识别之前重开页面或 App，旧记录会再显示一次。
export function settledWd(before, after, known) {
  const job = modelsFinished(before, after);
  if (!job || job.status !== 'done' || after.optional_missing) return known;
  const optional = new Set((after.files || []).filter(f => f.optional).map(f => f.path));
  const placed = job.kind === 'download' || (job.results || []).some(r => r.result === 'accepted' && optional.has(r.path));
  return placed ? after.acceleration?.last?.wd?.at ?? known : known;
}
// 上次识别里还算数的回退记录，按模型（#375）：开着加速、写了原因，WD 那条不是装好加速文件之前留下的（`stale`）。
export function liveFallbacks(m, stale) {
  const a = m?.acceleration; if (!a?.on) return {};
  return Object.fromEntries(Object.entries(a.last || {}).filter(([k, r]) => r?.reason && !(k === 'wd' && stale != null && r.at === stale)));
}
export const fallbacks = (m = state.models) => liveFallbacks(m, settled);
