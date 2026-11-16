// WD 角色扩展（#550）：「设置 › 角色资料」那张卡片里的状态行、按钮、失败列表和扩展包信息，三个确认框，任务面板的一项，
// 以及结束时的提示。卡片本身由 ui/settings.js 画（cardHead 在那里），任务面板的一项由 ui/tasks.js 列出；状态、轮询和请求
// 在 core/wd_extension.js。用户点「下载」之前什么都不下载；启用、重新检查和撤回都不联网。
import {t, tb} from '../core/i18n.js';
import {esc, bytes, number, localTime} from '../core/format.js';
import {state} from '../core/state.js';
import {home} from '../core/human.js';
import {extDownloading, extRunning, extFinished, extDownloadFinished, loadWdExtension, onWdExtension, packageAction, enableExtension,
  withdrawExtension, cancelExtension} from '../core/wd_extension.js';
import {$, toast, fold, busy} from './dom.js';
import {confirmAction} from './dialog.js';

const pkg = x => x?.package || {};
const lib = x => x?.library || {};
const size = x => bytes(pkg(x).file?.size || 0);
const left = x => Math.max(0, (pkg(x).file?.size || 0) - (pkg(x).have || 0));
const ready = x => pkg(x).status === 'ready';
// 不能启用或重新检查的原因（library.blocked），写在按钮提示里。
const BLOCKED = {
  frozen: () => t('图库只读'),
  no_vectors: () => t('这个图库还没有 WD 识别结果'),
  syncing: () => t('正在同步，等它结束'),
  busy: () => t('图库正在导入或导出'),
};
export const blockedReason = x => { const b = lib(x).blocked; return b ? BLOCKED[b]?.() || b : ''; };

// 已启用时的一行：扩展候选覆盖几张图，其中几张的归属由它补上。两个数字每次读取时现算。
function enabledLine(x) {
  const {images = 0, assigned = 0} = lib(x);
  if (!images) return t('已启用 · 没有扩展候选');
  return assigned ? t('已启用 · {n} 张图有扩展候选，其中 {assigned} 张由它补上归属', {n: number(images), assigned: number(assigned)})
    : t('已启用 · {n} 张图有扩展候选', {n: number(images)});
}
// 状态行：正在发生什么，或者现在是什么样。纯文本，调用方转义。
export function extLine(x = state.wdExtension) {
  if (!x) return '…';
  const task = x.task, d = x.download, l = lib(x);
  if (extRunning(x)) {
    if (task.kind === 'withdraw') return t('正在撤回…');
    if (task.phase === 'write') return t('正在写入…');
    return t('正在检查 {done} / {total} 张', {done: number(task.done || 0), total: number(task.total || 0)});
  }
  if (extDownloading(x)) {
    if (d.kind === 'import') return t('正在核对所选文件 {done} / {total}', {done: bytes(d.done), total: bytes(d.total)});
    return t('正在下载 {done} / {total}', {done: bytes(d.done), total: bytes(d.total)}) + (d.speed ? ' · ' + t('{speed}/秒', {speed: bytes(d.speed)}) : '')
      + (d.source ? ' · ' + t('来自 {source}', {source: d.source}) : '') + (d.proxy ? ' · ' + t('经代理 {proxy}', {proxy: d.proxy}) : '');
  }
  if (task?.status === 'failed') {
    const reason = tb(task.message || '');
    return task.kind === 'withdraw' ? t('撤回没有完成，图库没有改动：{reason}', {reason}) : t('启用没有完成，图库没有改动：{reason}', {reason});
  }
  if (task?.status === 'cancelled') return t('已取消，图库没有改动');
  if (l.enabled) return enabledLine(x);
  if (!ready(x) && d?.kind === 'download' && d.status === 'failed') return t('扩展包没能下载：{reason}', {reason: tb(d.message || '') || t('每个来源都试过了')});
  if (!ready(x) && d?.kind === 'download' && d.status === 'cancelled') return t('已取消');
  if (!ready(x)) return pkg(x).have ? t('扩展包还没下载完（已有 {have}，共 {size}）', {have: bytes(pkg(x).have), size: size(x)}) : t('扩展包还没下载（约 {size}）', {size: size(x)});
  return t('本图库未启用');
}
// 状态行左边的圆点：进行中、出错、没有启用。
export function extDot(x = state.wdExtension) {
  if (!x) return 'off';
  if (extRunning(x) || extDownloading(x)) return 'busy';
  if (x.task?.status === 'failed' || (!ready(x) && x.download?.status === 'failed')) return 'warn';
  return lib(x).enabled ? '' : 'off';
}
export function extBar(x = state.wdExtension) {
  const job = extRunning(x) ? x.task : extDownloading(x) ? x.download : null;
  if (!job?.total || (x.task === job && job.phase === 'write')) return '';
  const width = Math.min(100, (job.done || 0) / job.total * 100).toFixed(1);
  return `<div class="progress" role="progressbar" aria-valuemin="0" aria-valuemax="${job.total}" aria-valuenow="${job.done || 0}"><span style="width:${width}%"></span></div>`;
}
const button = (act, label, cls = 'quiet', extra = '') => `<button class="${cls} small" data-wdx="${act}"${extra}>${label}</button>`;
// 当前状态下的按钮；`compact` 只留主要的那个（任务面板）。
export function extActions(x = state.wdExtension, compact = false) {
  if (!x) return '';
  const l = lib(x), task = x.task, blocked = blockedReason(x);
  if (extRunning(x)) {
    if (task.kind === 'withdraw') return '';
    return task.phase === 'write' ? button('cancel', t('取消'), 'quiet', ` disabled title="${t('正在写入，不能取消')}"`) : button('cancel', t('取消'));
  }
  if (extDownloading(x)) return button('package-cancel', t('取消'));
  const off = blocked ? ` disabled title="${esc(blocked)}"` : '';
  if (task?.status === 'failed') return button(task.kind === 'withdraw' ? 'withdraw-retry' : 'enable-retry', t('重试'), 'primary', task.kind === 'withdraw' ? '' : off);
  const withdraw = l.enabled ? button('withdraw', t('撤回…')) : '';
  if (!ready(x)) {
    const failed = x.download?.kind === 'download' && x.download.status === 'failed';
    const download = button('download', failed ? t('重试') : pkg(x).have ? t('继续下载（还差 {left}）', {left: bytes(left(x))}) : t('下载扩展包（约 {size}）', {size: size(x)}),
      l.enabled ? 'quiet' : 'primary');
    if (compact) return download;
    return download + button('import', t('从文件导入…')) + (pkg(x).partial && pkg(x).have ? button('discard', t('删除未完成的部分')) : '') + withdraw;
  }
  if (l.enabled) return compact ? '' : button('enable', t('重新检查'), 'quiet', off) + withdraw;
  return button('enable-confirm', t('启用…'), 'primary', off);
}
// 没就位时的手动下载说明、已启用时的上次检查时间和扩展包不在本机的说明：写在状态行下面的小字里。
export function extHint(x = state.wdExtension) {
  if (!x || extRunning(x) || extDownloading(x)) return '';
  const l = lib(x), parts = [];
  if (l.enabled && !ready(x)) parts.push(esc(t('扩展包不在这台电脑上，新识别的图不检查')));
  if (l.enabled && l.checked_at) parts.push(esc(t('上次检查 {time}', {time: localTime(l.checked_at)})));
  if (!ready(x) && pkg(x).manual) parts.push(`<a class="link" href="${esc(pkg(x).manual)}" target="_blank" rel="noreferrer noopener">${t('手动下载说明')}</a>`);
  return parts.join(' · ');
}
// 下载卡住时每个来源各自失败的原因。
export function extFailures(x = state.wdExtension) {
  const d = x?.download, list = d?.status === 'failed' ? d.failures || [] : [];
  return list.length ? `<ul class="models-results">${list.map(f => `<li class="mismatch">${esc(t('{source}：{why}', {source: f.source, why: tb(f.why)}))}</li>`).join('')}</ul>` : '';
}
// 导入对所选的每个文件做了什么。
const RESULT = {accepted: () => t('已接收'), mismatch: () => t('SHA-256 不符，已拒绝'), unknown: () => t('不是扩展包'), already: () => t('已经有了')};
export function extImportSummary(x = state.wdExtension) {
  const d = x?.download; if (d?.kind !== 'import' || !d.results?.length || extDownloading(x)) return '';
  return `<ul class="models-results">${d.results.map(r => `<li class="${esc(r.result)}"><bdi>${esc(r.name)}</bdi> · ${RESULT[r.result]?.() || esc(r.result)}</li>`).join('')}</ul>`;
}
// 折叠区「扩展包」：存放位置（所有图库共用）、文件、版本和许可；已就位时可以删除。
export function extPackage(x = state.wdExtension) {
  const p = pkg(x), f = p.file; if (!f) return '';
  const working = extDownloading(x) || extRunning(x);
  const del = ready(x) ? `<div class="models-acts">${button('delete', t('删除扩展包'), 'quiet', working ? ` disabled title="${t('正在下载或检查，等它结束再删除')}"` : '')}</div>` : '';
  return fold('wdExtPackage', t('扩展包'), `<table class="settings-table models-files">
    <tr><td>${t('存放位置')}</td><td class="mono" title="${esc(p.location)}">${esc(home(p.location))} · ${t('所有图库共用')}</td></tr>
    <tr><td class="mono">${esc(f.path)}</td><td class="mono" title="SHA-256 ${esc(f.sha256)}">${bytes(f.size)} · ${esc(String(f.sha256).slice(0, 12))}…</td></tr>
    <tr><td>${t('版本')}</td><td>${esc(p.version)} · ${t('{n} 个角色', {n: number(p.heads || 0)})}</td></tr>
    <tr><td>${t('许可')}</td><td>${esc(p.license)}</td></tr></table>${del}`);
}
// 卡片的状态行：和别的任务卡片一样带圆点；有过下载或任务时可以转到任务面板。
export function extStatus(x = state.wdExtension) {
  const dot = extDot(x), hint = extHint(x), link = x?.download || x?.task;
  const text = `<span>${esc(extLine(x))}${hint ? `<small class="line-hint">${hint}</small>` : ''}</span>`;
  return `<div class="setting-line set-status"><span><span class="local-dot${dot ? ' ' + dot : ''}"></span>${text}</span>`
    + `${link ? `<button class="quiet small" data-open-tasks>${t('在任务面板查看')}</button>` : ''}</div>`;
}
// 卡片正文（标题之后）：状态行、进度、失败或导入结果、按钮和折叠区。
export const extCardBody = (x = state.wdExtension) => !x ? '<p class="muted-note">…</p>'
  : `${extStatus(x)}${extBar(x)}${extFailures(x)}${extImportSummary(x)}<div class="models-acts">${extActions(x)}</div>${extPackage(x)}`;
export const extNote = (x = state.wdExtension) => t('多认 {n} 个 WD 词表里没有的较新角色。', {n: number(pkg(x).heads || 228)});

// 任务面板的一项：下载、启用、撤回共用。运行中和结束后都列出来（同导入、导出）。
export function extTask(x = state.wdExtension) {
  if (!x || !(x.download || x.task)) return null;
  const running = extRunning(x), downloading = extDownloading(x);
  const busyText = running && x.task.kind === 'withdraw' ? t('撤回 WD 角色扩展中') : running ? t('检查 WD 角色扩展中') : t('下载 WD 角色扩展包中');
  const failed = x.task?.status === 'failed' ? x.task : !ready(x) && x.download?.status === 'failed' ? x.download : null;
  return {key: 'wdExtension', name: t('WD 角色扩展'), busy: busyText, active: running || downloading, warn: !!failed, off: extDot(x) === 'off',
    line: extLine(x), body: extBar(x), action: extActions(x, true), diagnostic: failed?.diagnostic};
}

// 结束时的提示。`before` 是任务运行时最后一次读到的状态：那时已经启用的，这一次是重新检查。
export function finishedText(task, before, x = state.wdExtension) {
  if (task.status === 'failed' || task.status === 'cancelled') return extLine(x);
  if (task.kind === 'withdraw') return t('已撤回 WD 角色扩展');
  const images = lib(x).images || 0;
  if (lib(before).enabled) return !task.result?.changed ? t('没有变化') : t('WD 角色扩展已更新：{n} 张图有扩展候选', {n: number(images)});
  return images ? t('WD 角色扩展已启用：{n} 张图有扩展候选', {n: number(images)}) : t('WD 角色扩展已启用，没有找到扩展候选');
}
export function downloadText(d, x = state.wdExtension) {
  if (d.status === 'failed' && d.kind === 'download') return extLine(x);
  return ready(x) && d.status === 'done' ? t('扩展包已就位') : '';
}

// 确认框只写做决定前要知道的事实。数字取自状态：启用前要检查的张数（library.vectors），撤回时由扩展补上的归属数。
export function enableMessage(x = state.wdExtension) {
  const n = lib(x).vectors;
  const rest = t('还没有归属、也没有人工确认的图会补上一个机器归属，已有的归属和人工结论不变。以后可以撤回。');
  return (n != null ? t('用扩展包检查本图库 {n} 张图。', {n: number(n)}) : t('用扩展包检查本图库的图。')) + rest;
}
export function withdrawMessage(x = state.wdExtension) {
  const n = lib(x).assigned || 0;
  return (n ? t('去掉本图库的全部扩展候选，以及由它补上的 {n} 个机器归属。', {n: number(n)}) : t('去掉本图库的全部扩展候选。'))
    + t('你确认过、否定过的保留。撤回不进撤销历史，要恢复就重新启用。');
}
export const DELETE_MESSAGE = () => t('删除扩展包？已启用的图库保留现有的扩展候选，新识别的图不再检查；再用要重新下载。');

async function act(what) {
  try {
    if (what === 'download') await packageAction('download');
    else if (what === 'import') await packageAction('import');
    else if (what === 'package-cancel') await packageAction('cancel');
    else if (what === 'discard') {
      if (!await confirmAction(t('删除已经下载但还没完成的部分？下次要从头下载。'), t('删除未完成的部分'), t('删除'))) return;
      await packageAction('discard');
    } else if (what === 'delete') {
      if (!await confirmAction(DELETE_MESSAGE(), t('删除扩展包'), t('删除'))) return;
      await packageAction('delete');
      toast(t('扩展包已删除'));
    } else if (what === 'enable-confirm') {
      if (!await confirmAction(enableMessage(), t('启用 WD 角色扩展'), t('启用'))) return;
      await enableExtension();
    } else if (what === 'enable' || what === 'enable-retry') await enableExtension();
    else if (what === 'withdraw') {
      if (!await confirmAction(withdrawMessage(), t('撤回 WD 角色扩展'), t('撤回'))) return;
      await withdrawExtension();
    } else if (what === 'withdraw-retry') await withdrawExtension();
    else if (what === 'cancel') await cancelExtension();
  } catch (e) { toast(e.message); }
}
// 处理函数。页面启动时调用一次。按钮在设置页的卡片和任务面板里，监听挂在这两处，不挂在 document 上。
export function wireWdExtension() {
  for (const root of [$('settingsPage'), $('taskPanel')]) root.addEventListener('click', e => {
    const b = e.target.closest('[data-wdx]'); if (!b || b.disabled) return;
    busy(b, () => act(b.dataset.wdx));
  });
  onWdExtension(before => {
    const task = extFinished(before), d = extDownloadFinished(before);
    const note = task ? finishedText(task, before) : d ? downloadText(d) : '';
    if (note) toast(note);
  });
}
// 第一次读取由任务面板的定时调用发起（ui/tasks.js 的 taskTick），设置页打开「角色资料」时再读一次。
export const refreshWdExtension = () => loadWdExtension().catch(() => {});
