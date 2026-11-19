// 不带识别模型的安装包在第一次使用时下载的模型（#168，native/terms/src/app_models.rs）。和更新检查一样全应用共用：
// 每个图库、每个窗口里状态相同，切换图库时下载照常继续。不碰 DOM。onModels(before, start) 在每次读取或改动后通知，带上改动前的
// 状态，以及正在运行或刚结束的任务开始时模型是否已就位（readyWhenStarted）。
import {api, session} from './session.js';
import {state} from './state.js';
import {listeners} from './events.js';
const changed = listeners();
export const onModels = changed.on;

let timer = 0, reading = null, started = null;
const adopt = async value => {
  const before = state.models, start = started;
  state.models = value;
  started = readyWhenStarted(before, value, start);
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
// 「检查新角色」的轮询可能只看到索引任务的终态。只要这张卡自己读到正在更新，就跟到它结束。
let baseTimer = 0, baseRequest = 0, baseWrite = null;
function watchCharacterBase() {
  clearTimeout(baseTimer);
  if (!state.characterBase?.running || session.stale) return;
  baseTimer = setTimeout(() => loadCharacterBase().catch(() => {}), 1000);
}
async function readCharacterBase(options) {
  clearTimeout(baseTimer);
  const request = ++baseRequest;
  try {
    const value = await api('/api/index/base', options);
    if (request === baseRequest) {
      state.characterBase = value;
      await changed.emit(state.models, started);
    }
    return state.characterBase;
  } finally {
    if (request === baseRequest) watchCharacterBase();
  }
}
export async function loadCharacterBase() {
  // 其他任务发起的刷新要等启用或撤回的回应，避免读到变更前的状态并丢掉轮询起点。
  while (baseWrite) await baseWrite.catch(() => {});
  return readCharacterBase();
}
export function characterBasePost(enable) {
  const write = readCharacterBase({method: 'POST', body: JSON.stringify({enable})}).finally(() => {
    if (baseWrite === write) baseWrite = null;
  });
  baseWrite = write;
  return write;
}
// 基础角色索引的文件在识别模型清单里的路径；这个版本不带时为空。
export const characterBasePath = (b = state.characterBase) => b?.included ? b.file?.path || '' : '';
// 模型就位后还缺的可选文件：WD 加速文件（Apple 芯片的 macOS）和基础角色索引。
export function optionalGaps(m = state.models) {
  const missing = (m?.files || []).filter(f => f.optional && m.missing?.includes(f.path)).map(f => f.path);
  const base = characterBasePath();
  return {base: !!base && missing.includes(base), wd: missing.some(p => p !== base)};
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
// 上次识别里各模型写了原因的回退记录（#375），只在开着加速时算。WD 加速文件装好之前留下的那条 WD 记录，宿主已经不报
// （native/terms/src/app_models.rs 的 drop_superseded_wd），下一次识别写入新记录后照常报告。
export function fallbacks(m = state.models) {
  const a = m?.acceleration; if (!a?.on) return {};
  return Object.fromEntries(Object.entries(a.last || {}).filter(([, r]) => r?.reason));
}
