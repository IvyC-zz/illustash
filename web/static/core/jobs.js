// Background jobs (#199): the Photos or folder sync, new characters for the external index (native/terms/src/app_index.rs),
// display names, look-alike images, 找回画师 and imports. How each is polled, its status in words, and what starting
// or cancelling one sends; no DOM. Their state is still in core/state.js (sync, syncHistory, syncDamaged, index,
// names, provenance, importTask, settings.tasks.similar), written here and, until they move too, by the settings
// pages. While a job runs its poller re-arms itself; overlapping polls never wind the state back (#34).
import {t, tb} from './i18n.js';
import {number, stamp} from './format.js';
import {session, api, post, shared} from './session.js';
import {state} from './state.js';
import {listeners} from './events.js';
const changed = listeners();
// fn(job, change) after each answer that was applied; job is sync, index, names, provenance or import. change.was is
// the status before it, change.ended says a run has just ended; for sync, change.arrived says pictures came in or a
// run settled, so what the library shows may have changed.
export const onJob = changed.on;

// ---------- sync ----------
export const SYNC_ACTIVE = ['scanning', 'recognising', 'importing'];
export const syncBar = (s, bar) => s.status === 'recognising' && s.stage !== 'preparing' ? bar(s.done, s.total)
  : s.status === 'importing' && s.import_total ? bar(s.import_done, s.import_total) : '';
// Recognition and import are counted apart: done/total are images recognised or given up on,
// import_done/import_total the recognised ones written into the library.
export function syncLine(s) {
  if (s.status === 'scanning') return s.scan_total == null ? t('正在检查来源…') : t('正在检查文件 {done}/{total}', {done: number(s.scanned || 0), total: number(s.scan_total)});
  if (s.status === 'importing') {
    if (!s.import_total) return t('正在导入…');
    return s.stage === 'verifying' ? t('正在校验识别结果（{n} 张）…', {n: number(s.import_total)})
      : t('正在导入识别结果 {done}/{total}', {done: number(s.import_done || 0), total: number(s.import_total)});
  }
  if (s.status === 'recognising') {
    if (s.stage === 'preparing') return t('正在加载识别模型（共 {n} 张）…', {n: number(s.total || 0)});
    const failed = s.failed ? ' · ' + t('{n} 张无法识别', {n: number(s.failed)}) : '';
    const left = s.ms_per_image && s.total > s.done ? ' · ' + t('约 {n} 分钟', {n: Math.max(1, Math.round((s.total - s.done) * s.ms_per_image / 60000))}) : '';
    const imported = s.import_done ? ' · ' + t('已导入 {n}', {n: number(s.import_done)}) : '';
    return t('正在识别新图片 {done}/{total}', {done: number(s.done || 0), total: number(s.total || 0)}) + failed + imported + left;
  }
  // Paths the scan could not read keep their pictures; the next sync reads them again.
  const kept = s.unreadable ? ' · ' + t('{n} 处暂时读不到，已有图片保留不动', {n: number(s.unreadable)}) : '';
  // Pictures given up on, apart (#112): unreadable ones are tried again, damaged ones wait for their file to change.
  const gave = (s.failed ? ' · ' + t('{n} 张暂时读不到（可能还在 iCloud），下次同步重试', {n: number(s.failed)}) : '')
    + (s.damaged ? ' · ' + t('{n} 张已损坏或格式不支持，未识别', {n: number(s.damaged)}) : '');
  if (s.status === 'done') return t('上次同步：{time}', {time: stamp(s.finished_at)}) + kept + gave;
  if (s.status === 'needs_recognition') return t('有 {n} 张图等待识别（识别服务未启动）', {n: number(s.total || 0)}) + kept;
  if (s.status === 'interrupted') return s.message ? t('同步已中断（{reason}），可重新扫描', {reason: tb(s.message)}) : t('同步已中断，可重新扫描');
  if (s.status === 'error') return t('同步失败');
  return t('本地图库');
}
let syncTimer = null, lastImported = 0, syncPoll = 0;
export async function pollSync() {
  clearTimeout(syncTimer);
  // Polls overlap (task panel, timer, after a start): an older answer arriving last must not wind the state back.
  const poll = ++syncPoll;
  try {
    const {state: s, history, damaged} = await api('/api/sync');
    if (poll !== syncPoll) return;
    const was = state.sync.status, ended = SYNC_ACTIVE.includes(was) && !SYNC_ACTIVE.includes(s.status);
    state.sync = s; state.syncHistory = history; state.syncDamaged = damaged;
    // New pictures, or a run that settled: what the library shows may have changed.
    const arrived = (s.imported || 0) > lastImported || ended;
    if (arrived) lastImported = s.imported || 0;
    await changed.emit('sync', {was, ended, arrived});
    if (SYNC_ACTIVE.includes(s.status)) syncTimer = setTimeout(pollSync, 2000);
  } catch (e) { if (poll === syncPoll) syncTimer = setTimeout(pollSync, 10000); }
}
export const syncDoneText = s => t('同步完成：新加入 {n} 张', {n: number(s.imported)});
export async function startSync() { await post('/api/sync', {}); state.sync = {status: 'scanning'}; }

// ---------- new characters for the external index ----------
export const INDEX_ACTIVE = ['checking', 'fetching', 'cleaning', 'building', 'scanning', 'importing'];
export function indexLine(s, check) {
  const n = s.total ? ` ${number(s.done || 0)}/${number(s.total)}` : '';
  return {checking: t('正在检查 Danbooru…'), fetching: t('正在下载参考图') + n, cleaning: t('正在处理参考图'), building: t('正在更新角色特征'), scanning: t('正在重新扫描图库'), importing: t('正在导入图库'),
    error: t('检查失败：{reason}', {reason: tb(s.message || '')})}[s.status] || (check ? t('上次检查：{time} · 待添加 {n} 个角色', {time: stamp(check.checked_at), n: number(check.missing.length)}) : t('尚未检查'));
}
let indexTimer = 0;
export async function pollIndex() {
  clearTimeout(indexTimer);
  const was = state.index?.state.status;
  try {
    const progress = await api('/api/index?progress=1');
    // The full read (works, check results) only when something may have changed; the idle tick stays light.
    state.index = !state.index?.works || (!INDEX_ACTIVE.includes(progress.state?.status) && progress.state?.status !== was)
      ? await shared('/api/index') : {...state.index, ...progress};
  } catch (e) { indexTimer = setTimeout(pollIndex, 10000); return; }
  const s = state.index.state;
  await changed.emit('index', {was, ended: INDEX_ACTIVE.includes(was) && !INDEX_ACTIVE.includes(s.status)});
  if (INDEX_ACTIVE.includes(s.status)) indexTimer = setTimeout(pollIndex, 2000);
}
// What to tell when a run has ended, by how it ended.
export function indexEndText(s) {
  if (s.status === 'done') return s.kind === 'drop' ? t('已移除，{n} 张图片已重新识别', {n: number(s.changed)}) : t('已添加 {added} 个角色，{n} 张图片的识别结果已更新', {added: number(s.added), n: number(s.changed)});
  if (s.status === 'checked') return s.found ? t('发现 {n} 个可添加的角色，在任务面板里添加', {n: number(s.found)}) : t('未发现新角色');
  if (s.status === 'error') return t('角色索引更新失败：{reason}', {reason: tb(s.message || '')});
  return '';
}

// Watch list, minimum posts, dropping a character: the answer is the index's new state.
export async function indexAction(body) { state.index = await post('/api/index', body); }
// A check reaches Danbooru, so it is asked for each time (confirm(message, title) resolves to the answer); a library
// switched meanwhile is not checked.
export async function checkIndex(confirm) {
  const token = session.token;
  if (!await confirm(t('本次检查会联网查询 Danbooru，只发送关注作品的 tag，不发送图库图片。允许本次检查联网吗？'), t('检查新角色')) || token !== session.token) return false;
  const result = await post('/api/index', {action: 'check', allow_network: true});
  if (token !== session.token) return false;
  state.index = result; return true;
}


// ---------- display names: Bangumi first, an opt-in model API last ----------
export const NAMES_ACTIVE = ['fetching', 'matching', 'translating', 'running'];
export const namesLine = s => s.status === 'off' ? t('当前未启用') : NAMES_ACTIVE.includes(s.status) ? t('正在获取 {progress}', {progress: s.total ? `${number(s.done || 0)} / ${number(s.total)}` : '…'}) : s.status === 'error' ? t('获取失败，将在下次启动时重试') : s.status === 'unauthorized' ? t('未允许联网查询 · 可在设置 › 网络与隐私里允许') : t('已是最新');
let namesTimer = 0;
export async function pollNames() {
  clearTimeout(namesTimer);
  const was = state.names?.state?.status, running = NAMES_ACTIVE.includes(was);
  try {
    const progress = await api('/api/names?progress=1');
    // Keep the existing coverage/settings while running; refresh them once on completion.
    state.names = !state.names?.counts || (running && !NAMES_ACTIVE.includes(progress.state?.status)) ? await api('/api/names') : {...state.names, ...progress};
  } catch (e) { namesTimer = setTimeout(pollNames, 10000); return; }
  const now = NAMES_ACTIVE.includes(state.names.state?.status);
  await changed.emit('names', {was, ended: running && !now});
  if (now) namesTimer = setTimeout(pollNames, 2000);
}
// A run the settings card just showed is polled again in 2 s.
export function pollNamesSoon() { clearTimeout(namesTimer); namesTimer = setTimeout(pollNames, 2000); }
export async function refreshNames() { state.names = {...state.names, ...await post('/api/names', {action: 'refresh'})}; }

// ---------- look-alike images (its state comes with /api/settings) ----------
export const SIMILAR_ACTIVE = ['reading', 'pairing', 'comparing', 'waiting'];
export function similarLine(s) {
  if (s.status === 'reading') return t('正在读图算签名 {done}/{total}', {done: number(s.done || 0), total: number(s.total || 0)});
  if (s.status === 'pairing') return t('正在找候选…');
  if (s.status === 'comparing') return t('正在逐对比较 {done}/{total} · 已找到 {found} 对', {done: number(s.done || 0), total: number(s.total || 0), found: number(s.found || 0)});
  if (s.status === 'waiting') return tb(s.message) || t('输入有变化，稍后自动重试');
  if (s.status === 'error') return t('出错：{reason}', {reason: tb(s.message || '')});
  if (s.status === 'off') return t('本次启动关闭了相似检测');
  return s.at ? t('上次检查 {time}', {time: stamp(s.at)}) : '';
}
export const taskSimilar = s => s.status === 'off' ? t('这次启动没有开相似检测') : similarLine(s) || t('已是最新');

// ---------- 找回画师 (docs/PROVENANCE.md section 8) ----------
export const PROV_ACTIVE = ['running', 'stopping'];
export const RUN_STATUS = {done: t('完成'), stopped: t('已停止'), failed: t('出错'), running: t('进行中')};
let provenanceTimer = 0;
export async function loadProvenance() {
  clearTimeout(provenanceTimer);
  const was = state.provenance?.state?.status;
  try { state.provenance = await api('/api/provenance'); } catch {
    // One failed read must not end the polling of a run that is going on: keep what was shown and try again.
    if (PROV_ACTIVE.includes(was)) provenanceTimer = setTimeout(loadProvenance, 4000);
    else { state.provenance = null; await changed.emit('provenance', {was, ended: false}); }
    return;
  }
  const s = state.provenance?.state?.status;
  await changed.emit('provenance', {was, ended: PROV_ACTIVE.includes(was) && !PROV_ACTIVE.includes(s)});
  if (PROV_ACTIVE.includes(s)) provenanceTimer = setTimeout(loadProvenance, 1500);
}
// Start ({action: 'start', allow_network}) or stop a run; polled again either way.
export async function provenanceAction(body) {
  try { state.provenance = await post('/api/provenance', body); } finally { loadProvenance(); }
}

// ---------- imports (#88): an archive or recognition results, one at a time ----------
export const IMPORT_ACTIVE = ['running', 'stopping'];
export const importActive = () => IMPORT_ACTIVE.includes(state.importTask?.status);
export const IMPORT_STAGES = {
  archive: {reading: t('正在读取归档'), unpacking: t('正在解压'), cleaning: t('正在清除模型连接设置和联网授权'), checking: t('正在检查新图库能否打开'), publishing: t('正在登记新图库')},
  recognition: {models: t('正在核对模型指纹'), reading: t('正在读取结果包'), verifying: t('正在校验识别结果'), writing: t('正在写入识别结果'), committing: t('正在提交'), projecting: t('正在更新图库索引')},
};
export const mib = n => (n / 1048576).toFixed(n < 10485760 ? 1 : 0);
export function importCount(job) {
  if (job.done == null || !job.total) return '';
  if (job.stage === 'unpacking') return ` ${mib(job.done)}/${mib(job.total)} MB`;
  if (job.stage === 'reading') return t('（第 {n}/{total} 个文件）', {n: Math.min(job.done + 1, job.total), total: job.total});
  return ' ' + t('{done}/{total} 张', {done: number(job.done), total: number(job.total)});
}
export function importLine(job) {
  if (!job || job.status === 'idle') return '';
  const recognition = job.kind === 'recognition';
  const name = job.file ? t('「{file}」', {file: job.file}) : '';
  if (job.status === 'running') return (recognition ? t('导入识别结果{name}：{step}', {name, step: (IMPORT_STAGES[job.kind] || {})[job.stage] || t('正在准备')}) : t('导入归档{name}：{step}', {name, step: (IMPORT_STAGES[job.kind] || {})[job.stage] || t('正在准备')})) + importCount(job) + '…';
  if (job.status === 'stopping') return recognition ? t('正在取消导入识别结果{name}…', {name}) : t('正在取消导入归档{name}…', {name});
  if (job.status === 'cancelled') return t('{reason}；图库没有改动。', {reason: tb(job.reason) || t('导入已取消')});
  if (job.status === 'error') return recognition ? t('导入识别结果{name}失败：{reason}', {name, reason: tb(job.error)}) : t('导入归档{name}失败：{reason}', {name, reason: tb(job.error)});
  if (job.status !== 'done') return '';
  const r = job.result || {};
  // Published but not listed (#111): opening it lists it; importing again would make a second, numbered library.
  if (job.kind === 'archive' && r.listed === false) return t('已导入「{name}」，保存在 {path}，但没能加入最近图库列表：{reason}。打开它会再试一次加入列表；再导入会另建一个带编号的图库。模型接口与名称联网授权需重新设置。', {name: r.name, path: String(r.path).replace(/^\/Users\/[^/]+/, '~'), reason: tb(r.error)});
  if (job.kind === 'archive') return t('已导入「{name}」，模型接口与名称联网授权需重新设置。', {name: r.name});
  if (r.status === 'pending') return t('识别结果已导入，后续处理尚未完成（{reason}）。', {reason: tb(r.error) || t('可稍后恢复')});
  return r.duplicate ? t('这份识别结果之前已经导入过。') : t('识别结果已导入。');
}
// Polls overlap (the start answer, the timer, a cancel): an older answer arriving last must not wind the state back,
// and an answer for a page that has since changed session is dropped (#34).
let importTimer = null, importPoll = 0;
export async function pollImports() {
  clearTimeout(importTimer);
  const poll = ++importPoll, token = session.token;
  try {
    const job = await api('/api/imports');
    if (poll !== importPoll || token !== session.token) return;
    const was = state.importTask;
    state.importTask = job;
    await changed.emit('import', {was: was?.status, ended: IMPORT_ACTIVE.includes(was?.status) && was.id === job.id && !IMPORT_ACTIVE.includes(job.status)});
    if (IMPORT_ACTIVE.includes(job.status)) importTimer = setTimeout(pollImports, 700);
  } catch { if (poll === importPoll && token === session.token) importTimer = setTimeout(pollImports, 5000); }
}
// A started import (the answer of POST /api/libraries/import) shows at once and is polled until it ends.
export function importStarted(task) { state.importTask = task; changed.emit('import', {was: undefined, ended: false}); pollImports(); }
export async function cancelImport() {
  try { await api('/api/imports/cancel', {method: 'POST', body: '{}'}); } finally { pollImports(); }
}
