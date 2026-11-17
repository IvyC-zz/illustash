// 用图动作（#450）：打开原文件、在访达或资源管理器中显示、拖到别的应用、导出到文件夹。不碰 DOM。
// 页面只传资产 ID，路径由宿主从图库里查（native/terms/src/app_asset_files.rs）；原图只读，不移动、不删除、不改。
// GET /api/files 的回应（能不能拖出、最近一次导出的状态）在 state.files，只由这里写入。onFiles(fn) 在每次读到新状态后
// 调用 fn({ended})，ended 表示一次导出刚结束。
import {t, tb} from './i18n.js';
import {isMac, number} from './format.js';
import {session, api} from './session.js';
import {state} from './state.js';
import {listeners} from './events.js';
import {poller} from './poll.js';
const changed = listeners();
export const onFiles = changed.on;

// 不经 post()：这些请求不写图库，不该让列表作废。
const send = (path, body) => api(path, {method: 'POST', body: JSON.stringify(body)});

export const revealLabel = () => isMac ? t('在访达中显示') : t('在资源管理器中显示');
// 照片来源的图不显示位置（在「照片」的图库包里）。图库只有照片来源时不画这个按钮；混合的图库由宿主逐张拒绝。
export const canReveal = () => {
  const sources = state.settings?.package?.sources || state.libraries?.info?.sources;
  return !sources?.length || sources.some(x => x.kind !== 'photos');
};
export const openFile = id => send('/api/files/open', {id});
export const revealFile = id => send('/api/files/reveal', {id});
// 一次最多拖出几张：与宿主的 DRAG_LIMIT 相同。
export const DRAG_LIMIT = 100;
export const canDrag = () => !!state.files?.drag;
export const dragFiles = ids => send('/api/files/drag', {ids});

// ---------- 导出到文件夹 ----------
export const EXPORT_ACTIVE = ['running'];
export const exportActive = () => EXPORT_ACTIVE.includes(state.files?.export?.status);
// 导出进行中每 0.7 s 读一次，读失败 5 s 后再试；重叠的读取和换了会话的回应丢掉（core/poll.js）。
export const loadFiles = poller(async fresh => {
  const token = session.token, current = () => fresh() && token === session.token;
  let value; try { value = await api('/api/files'); } catch { return current() ? 5000 : 0; }
  if (!current()) return 0;
  const was = state.files?.export?.status;
  state.files = value;
  await changed.emit({ended: EXPORT_ACTIVE.includes(was) && !exportActive()});
  return exportActive() ? 700 : 0;
});
// selection 是选择栏的 {ids} 或 {filter, exclude}；group 是 none、character 或 artist。宿主先弹选目录的窗口：
// 取消时回应 {cancelled: true}，返回 false。
export async function startExport(selection, group) {
  const reply = await send('/api/files/export', {...selection, group});
  if (reply.cancelled) return false;
  state.files = {...state.files, export: reply};
  await changed.emit({ended: false});
  loadFiles();
  return true;
}
export async function cancelExport() {
  try { await send('/api/files/export/cancel', {}); } finally { loadFiles(); }
}
export const revealExport = () => send('/api/files/export/reveal', {});
// 任务面板上的一行字。
export function exportLine(x) {
  if (x.status === 'running') return t('{done} / {total} 张', {done: number(x.done || 0), total: number(x.total || 0)});
  const extra = [
    x.existed ? t('{n} 个已存在', {n: number(x.existed)}) : '',
    x.failed ? t('{n} 张读不到原文件', {n: number(x.failed)}) : '',
    x.derived ? t('{n} 张是照片的派生图', {n: number(x.derived)}) : '',
  ].filter(Boolean).join(t('；'));
  const head = x.status === 'error' ? t('导出失败：{reason}', {reason: tb(x.error || '')})
    : x.status === 'cancelled' ? t('已取消，导出了 {n} 个文件', {n: number(x.copied || 0)})
    : t('已导出 {n} 个文件', {n: number(x.copied || 0)});
  return extra ? head + t('；') + extra : head;
}
