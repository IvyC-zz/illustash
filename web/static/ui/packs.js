// 角色数据包（#550 WD 角色扩展、#559 CCIP 长尾包）：「设置 › 角色资料」里每个包一张卡片的状态行、按钮、失败列表和包信息，
// 三个确认框，任务面板里每个包一项，以及结束时的提示。卡片本身由 ui/settings.js 画（cardHead 在那里），任务面板的项由
// ui/tasks.js 列出；状态、轮询和请求在 core/packs.js。两个包的回复形状相同，只有名字和几句文字不同（PACK_UI）。
// 用户点「下载」之前什么都不下载；导入只读用户选的文件；启用、重新检查和撤回都不联网。
import {t, tb} from '../core/i18n.js';
import {esc, bytes, number, localTime, home} from '../core/format.js';
import {state} from '../core/state.js';
import {packDownloading, packRunning, packFinished, packDownloadFinished, loadPack, onPack, packageAction, enablePack, withdrawPack,
  cancelPack} from '../core/packs.js';
import {$, toast, fold, busy} from './dom.js';
import {confirmAction} from './dialog.js';

// 每个包的名字和只属于它的几句文字。`shared` 是包文件是不是所有图库共用一份（WD 扩展包是；CCIP 长尾包存在每个图库里）。
export const PACK_UI = {
  wd: {
    name: () => t('WD 角色扩展'), card: 'wdExtCard', shared: true,
    note: n => t('多认 {n} 个 WD 词表里没有的较新角色。', {n}),
    file: () => t('扩展包'),
    scan: n => n != null ? t('用扩展包检查本图库 {n} 张图。', {n}) : t('用扩展包检查本图库的图。'),
    missing: size => t('扩展包还没下载（约 {size}）', {size}),
    partial: (have, size) => t('扩展包还没下载完（已有 {have}，共 {size}）', {have, size}),
    failed: reason => t('扩展包没能下载：{reason}', {reason}),
    download: size => t('下载扩展包（约 {size}）', {size}),
    absent: () => t('扩展包不在这台电脑上，新识别的图不检查'),
    unknown: () => t('不是扩展包'),
    remove: () => t('删除扩展包'), removed: () => t('扩展包已删除'), arrived: () => t('扩展包已就位'),
    removeNote: () => t('删除扩展包？已启用的图库保留现有的扩展候选，新识别的图不再检查；再用要重新下载。'),
    noVectors: () => t('这个图库还没有 WD 识别结果'),
    busy: {download: () => t('下载 WD 角色扩展包中'), check: () => t('检查 WD 角色扩展中'), withdraw: () => t('撤回 WD 角色扩展中')},
  },
  ccip: {
    name: () => t('CCIP 长尾包'), card: 'ccipCard', shared: false,
    note: n => t('多认 {n} 个图片较少、WD 和精选索引都没有的角色。', {n}),
    file: () => t('参考包'),
    scan: n => n != null ? t('用参考包检查本图库 {n} 张图。', {n}) : t('用参考包检查本图库的图。'),
    missing: () => t('参考包还没导入'),
    partial: () => t('参考包还没导入'),
    failed: reason => t('参考包没能下载：{reason}', {reason}),
    download: size => t('下载参考包（约 {size}）', {size}),
    absent: () => t('参考包不在这台电脑上，新识别的图不检查'),
    unknown: () => t('不是参考包'),
    remove: () => t('删除参考包'), removed: () => t('参考包已删除'), arrived: () => t('参考包已就位'),
    removeNote: () => t('删除参考包？已启用的图库保留现有的扩展候选，新识别的图不再检查；再用要重新导入。'),
    noVectors: () => t('这个图库还没有 CCIP 识别结果'),
    busy: {download: () => t('导入 CCIP 长尾包中'), check: () => t('检查 CCIP 长尾包中'), withdraw: () => t('撤回 CCIP 长尾包中')},
  },
};
export const PACK_KEYS = Object.keys(PACK_UI);
const ui = key => PACK_UI[key];
const current = key => state.packs[key];
const pkg = x => x?.package || {};
const lib = x => x?.library || {};
const size = x => bytes(pkg(x).file?.size || 0);
const left = x => Math.max(0, (pkg(x).file?.size || 0) - (pkg(x).have || 0));
const ready = x => pkg(x).status === 'ready';
// 能从网上下载（package.sources 不为空）；不能的只有从文件导入（0.3 的 CCIP 长尾包）。
const downloadable = x => !!pkg(x).sources?.length;
// 不能启用或重新检查的原因（library.blocked），写在按钮提示里。
const BLOCKED = {frozen: () => t('图库只读'), syncing: () => t('正在同步，等它结束'), busy: () => t('图库正在导入或导出')};
export const blockedReason = (key, x = current(key)) => {
  const b = lib(x).blocked;
  return !b ? '' : b === 'no_vectors' ? ui(key).noVectors() : BLOCKED[b]?.() || b;
};

// 已启用时的一行：扩展候选覆盖几张图，其中几张的归属由它补上。两个数字每次读取时现算。
function enabledLine(x) {
  const {images = 0, assigned = 0} = lib(x);
  if (!images) return t('已启用 · 没有扩展候选');
  return assigned ? t('已启用 · {n} 张图有扩展候选，其中 {assigned} 张由它补上归属', {n: number(images), assigned: number(assigned)})
    : t('已启用 · {n} 张图有扩展候选', {n: number(images)});
}
// 状态行：正在发生什么，或者现在是什么样。纯文本，调用方转义。
export function packLine(key, x = current(key)) {
  if (!x) return '…';
  const task = x.task, d = x.download, l = lib(x), u = ui(key);
  if (packRunning(x)) {
    if (task.kind === 'withdraw') return t('正在撤回…');
    if (task.phase === 'write') return t('正在写入…');
    // 刚开始、还没数出要检查几张时（total 为 0）不写「0 / 0」。
    if (!task.total) return t('正在检查…');
    return t('正在检查 {done} / {total} 张', {done: number(task.done || 0), total: number(task.total || 0)});
  }
  if (packDownloading(x)) {
    if (d.kind === 'import') return t('正在核对所选文件 {done} / {total}', {done: bytes(d.done), total: bytes(d.total)});
    return t('正在下载 {done} / {total}', {done: bytes(d.done), total: bytes(d.total)}) + (d.speed ? ' · ' + t('{speed}/秒', {speed: bytes(d.speed)}) : '')
      + (d.source ? ' · ' + t('来自 {source}', {source: d.source}) : '') + (d.proxy ? ' · ' + t('经代理 {proxy}', {proxy: d.proxy}) : '');
  }
  if (task?.status === 'failed') {
    const reason = tb(task.message || '');
    if (task.kind === 'auto') return t('检查新识别的图没有完成，图库没有改动：{reason}', {reason});
    return task.kind === 'withdraw' ? t('撤回没有完成，图库没有改动：{reason}', {reason}) : t('启用没有完成，图库没有改动：{reason}', {reason});
  }
  if (task?.status === 'cancelled') return t('已取消，图库没有改动');
  if (l.enabled) return enabledLine(x);
  if (!ready(x) && d?.kind === 'download' && d.status === 'failed') return u.failed(tb(d.message || '') || t('每个来源都试过了'));
  if (!ready(x) && d?.kind === 'download' && d.status === 'cancelled') return t('已取消');
  if (!ready(x)) return pkg(x).have ? u.partial(bytes(pkg(x).have), size(x)) : u.missing(size(x));
  return t('本图库未启用');
}
// 状态行左边的圆点：进行中、出错、没有启用。
export function packDot(key, x = current(key)) {
  if (!x) return 'off';
  if (packRunning(x) || packDownloading(x)) return 'busy';
  if (x.task?.status === 'failed' || (!ready(x) && x.download?.status === 'failed') || (lib(x).enabled && lib(x).error)) return 'warn';
  return lib(x).enabled ? '' : 'off';
}
export function packBar(key, x = current(key)) {
  const job = packRunning(x) ? x.task : packDownloading(x) ? x.download : null;
  if (!job?.total || (x.task === job && job.phase === 'write')) return '';
  const width = Math.min(100, (job.done || 0) / job.total * 100).toFixed(1);
  return `<div class="progress" role="progressbar" aria-valuemin="0" aria-valuemax="${job.total}" aria-valuenow="${job.done || 0}"><span style="width:${width}%"></span></div>`;
}
// 按钮带 data-pack="包:动作"；监听在 wirePacks。
const button = (key, act, label, cls = 'quiet', extra = '') => `<button class="${cls} small" data-pack="${key}:${act}"${extra}>${label}</button>`;
// 当前状态下的按钮；`compact` 只留主要的那个（任务面板）。
export function packActions(key, x = current(key), compact = false) {
  if (!x) return '';
  const l = lib(x), task = x.task, blocked = blockedReason(key, x), b = (...a) => button(key, ...a);
  if (packRunning(x)) {
    if (task.kind === 'withdraw') return '';
    return task.phase === 'write' ? b('cancel', t('取消'), 'quiet', ` disabled title="${t('正在写入，不能取消')}"`) : b('cancel', t('取消'));
  }
  if (packDownloading(x)) return b('package-cancel', t('取消'));
  const off = blocked ? ` disabled title="${esc(blocked)}"` : '';
  if (task?.status === 'failed') return b(task.kind === 'withdraw' ? 'withdraw-retry' : 'enable-retry', t('重试'), 'primary', task.kind === 'withdraw' ? '' : off);
  const withdraw = l.enabled ? b('withdraw', t('撤回…')) : '';
  if (!ready(x)) {
    const main = l.enabled ? 'quiet' : 'primary';
    const failed = x.download?.kind === 'download' && x.download.status === 'failed';
    const download = !downloadable(x) ? ''
      : b('download', failed ? t('重试') : pkg(x).have ? t('继续下载（还差 {left}）', {left: bytes(left(x))}) : ui(key).download(size(x)), main);
    const pick = b('import', t('从文件导入…'), download ? 'quiet' : main);
    if (compact) return download || pick;
    return download + pick + (pkg(x).partial && pkg(x).have ? b('discard', t('删除未完成的部分')) : '') + withdraw;
  }
  // 角色索引更新过（library.needs_scan）：重新检查后才用上，按钮升为主按钮。
  if (l.enabled) return compact ? '' : b('enable', t('重新检查'), l.needs_scan ? 'primary' : 'quiet', off) + withdraw;
  return b('enable-confirm', t('启用…'), 'primary', off);
}
// 状态行下面的小字：包不在本机、索引更新后要重新检查、上次自动检查的错误、上次检查时间，以及没就位时的手动下载说明。
export function packHint(key, x = current(key)) {
  if (!x || packRunning(x) || packDownloading(x)) return '';
  const l = lib(x), parts = [];
  if (l.enabled && !ready(x)) parts.push(esc(ui(key).absent()));
  if (l.enabled && l.needs_scan) parts.push(esc(t('角色索引更新过，重新检查后才用上')));
  if (l.enabled && l.error) parts.push(esc(t('上次自动检查没有完成：{reason}', {reason: tb(l.error)})));
  if (l.enabled && l.checked_at) parts.push(esc(t('上次检查 {time}', {time: localTime(l.checked_at)})));
  if (!ready(x) && pkg(x).manual) parts.push(`<a class="link" href="${esc(pkg(x).manual)}" target="_blank" rel="noreferrer noopener">${t('手动下载说明')}</a>`);
  return parts.join(' · ');
}
// 下载卡住时每个来源各自失败的原因。
export function packFailures(key, x = current(key)) {
  const d = x?.download, list = d?.status === 'failed' ? d.failures || [] : [];
  return list.length ? `<ul class="models-results">${list.map(f => `<li class="mismatch">${esc(t('{source}：{why}', {source: f.source, why: tb(f.why)}))}</li>`).join('')}</ul>` : '';
}
// 导入对所选的每个文件做了什么。只在这个页面发起的导入结束后列出：WD 扩展包整个 App 一份，导入结果也是，换了图库
// （页面重新载入）或别的窗口导入的不列。按包记。
const importedHere = new Set();
const RESULT = {accepted: () => t('已接收'), mismatch: () => t('SHA-256 不符，已拒绝'), already: () => t('已经有了')};
export function packImportSummary(key, x = current(key)) {
  const d = x?.download; if (!importedHere.has(key) || d?.kind !== 'import' || !d.results?.length || packDownloading(x)) return '';
  const what = r => r.result === 'unknown' ? ui(key).unknown() : RESULT[r.result]?.() || esc(r.result);
  return `<ul class="models-results">${d.results.map(r => `<li class="${esc(r.result)}"><bdi>${esc(r.name)}</bdi> · ${what(r)}</li>`).join('')}</ul>`;
}
// 折叠区：存放位置（所有图库共用，或只给本图库）、文件、版本和许可；已就位时可以删除。
export function packInfo(key, x = current(key)) {
  const p = pkg(x), f = p.file, u = ui(key); if (!f) return '';
  const working = packDownloading(x) || packRunning(x);
  const del = ready(x) ? `<div class="models-acts">${button(key, 'delete', u.remove(), 'quiet', working ? ` disabled title="${t('正在下载或检查，等它结束再删除')}"` : '')}</div>` : '';
  return fold(`pack-${key}`, u.file(), `<table class="settings-table models-files">
    <tr><td>${t('存放位置')}</td><td class="mono" title="${esc(p.location)}">${esc(home(p.location))} · ${u.shared ? t('所有图库共用') : t('本图库')}</td></tr>
    <tr><td class="mono">${esc(f.path)}</td><td class="mono" title="SHA-256 ${esc(f.sha256)}">${bytes(f.size)} · ${esc(String(f.sha256).slice(0, 12))}…</td></tr>
    <tr><td>${t('版本')}</td><td>${esc(p.version)} · ${t('{n} 个角色', {n: number(p.heads || 0)})}</td></tr>
    <tr><td>${t('许可')}</td><td>${esc(p.license)}</td></tr></table>${del}`);
}
// 卡片的状态行：和别的任务卡片一样带圆点；有过下载或任务时可以转到任务面板。
export function packStatus(key, x = current(key)) {
  const dot = packDot(key, x), hint = packHint(key, x), link = x?.download || x?.task;
  const text = `<span>${esc(packLine(key, x))}${hint ? `<small class="line-hint">${hint}</small>` : ''}</span>`;
  return `<div class="setting-line set-status"><span><span class="local-dot${dot ? ' ' + dot : ''}"></span>${text}</span>`
    + `${link ? `<button class="quiet small" data-open-tasks>${t('在任务面板查看')}</button>` : ''}</div>`;
}
// 卡片正文（标题之后）：状态行、进度、失败或导入结果、按钮和折叠区。
export const packCardBody = (key, x = current(key)) => !x ? '<p class="muted-note">…</p>'
  : `${packStatus(key, x)}${packBar(key, x)}${packFailures(key, x)}${packImportSummary(key, x)}<div class="models-acts">${packActions(key, x)}</div>${packInfo(key, x)}`;
export const packNote = (key, x = current(key)) => ui(key).note(number(pkg(x).heads || 0));

// 任务面板的一项：下载或导入、启用、撤回共用。运行中和结束后都列出来（同导入、导出）。
export function packTask(key, x = current(key)) {
  if (!x || !(x.download || x.task)) return null;
  const running = packRunning(x), downloading = packDownloading(x), u = ui(key);
  const busyText = running && x.task.kind === 'withdraw' ? u.busy.withdraw() : running ? u.busy.check() : u.busy.download();
  const failed = x.task?.status === 'failed' ? x.task : !ready(x) && x.download?.status === 'failed' ? x.download : null;
  return {key: `pack-${key}`, name: u.name(), busy: busyText, active: running || downloading, warn: !!failed, off: packDot(key, x) === 'off',
    line: packLine(key, x), body: packBar(key, x), action: packActions(key, x, true), diagnostic: failed?.diagnostic};
}

// 结束时的提示。`before` 是任务运行时最后一次读到的状态：那时已经启用的，这一次是重新检查。同步后对新识别的图自动检查
// （kind 为 auto）只在有改动或失败时提示。检查时没有兼容向量的图（result.missing、result.incompatible）一并说明。
export function finishedText(key, task, before, x = current(key)) {
  const name = ui(key).name(), images = lib(x).images || 0;
  const skipped = (task.result?.missing || 0) + (task.result?.incompatible || 0);
  const more = text => skipped ? text + t('；') + t('{n} 张图缺少兼容的向量，没有检查', {n: number(skipped)}) : text;
  const updated = () => more(t('{name}已更新：{n} 张图有扩展候选', {name, n: number(images)}));
  if (task.kind === 'auto' && task.status !== 'failed') return task.status === 'done' && task.result?.changed ? updated() : '';
  if (task.status === 'failed' || task.status === 'cancelled') return packLine(key, x);
  if (task.kind === 'withdraw') return t('已撤回 {name}', {name});
  if (lib(before).enabled) return !task.result?.changed ? more(t('没有变化')) : updated();
  return more(images ? t('{name}已启用：{n} 张图有扩展候选', {name, n: number(images)}) : t('{name}已启用，没有找到扩展候选', {name}));
}
export function downloadText(key, d, x = current(key)) {
  if (d.status === 'failed' && d.kind === 'download') return packLine(key, x);
  return ready(x) && d.status === 'done' ? ui(key).arrived() : '';
}

// 确认框只写做决定前要知道的事实。数字取自状态：启用前要检查的张数（library.vectors），撤回时由它补上的归属数。
export function enableMessage(key, x = current(key)) {
  const n = lib(x).vectors;
  return ui(key).scan(n != null ? number(n) : null) + t('还没有归属、也没有人工确认的图会补上一个机器归属，已有的归属和人工结论不变。以后可以撤回。');
}
export function withdrawMessage(key, x = current(key)) {
  const n = lib(x).assigned || 0;
  return (n ? t('去掉本图库的全部扩展候选，以及由它补上的 {n} 个机器归属。', {n: number(n)}) : t('去掉本图库的全部扩展候选。'))
    + t('你确认过、否定过的保留。撤回不进撤销历史，要恢复就重新启用。');
}

async function act(key, what) {
  const u = ui(key), name = u.name();
  try {
    if (what === 'download') { importedHere.delete(key); await packageAction(key, 'download'); }
    else if (what === 'import') { const r = await packageAction(key, 'import'); if (!r?.cancelled) importedHere.add(key); }
    else if (what === 'package-cancel') await packageAction(key, 'cancel');
    else if (what === 'discard') {
      if (!await confirmAction(t('删除已经下载但还没完成的部分？下次要从头下载。'), t('删除未完成的部分'), t('删除'))) return;
      await packageAction(key, 'discard');
    } else if (what === 'delete') {
      if (!await confirmAction(u.removeNote(), u.remove(), t('删除'))) return;
      await packageAction(key, 'delete');
      toast(u.removed());
    } else if (what === 'enable-confirm') {
      if (!await confirmAction(enableMessage(key), t('启用 {name}', {name}), t('启用'))) return;
      await enablePack(key);
    } else if (what === 'enable' || what === 'enable-retry') await enablePack(key);
    else if (what === 'withdraw') {
      if (!await confirmAction(withdrawMessage(key), t('撤回 {name}', {name}), t('撤回'))) return;
      await withdrawPack(key);
    } else if (what === 'withdraw-retry') await withdrawPack(key);
    else if (what === 'cancel') await cancelPack(key);
  } catch (e) { toast(e.message); }
}
// 处理函数。页面启动时调用一次。按钮在设置页的卡片和任务面板里，监听挂在这两处，不挂在 document 上。
export function wirePacks() {
  for (const root of [$('settingsPage'), $('taskPanel')]) root.addEventListener('click', e => {
    const b = e.target.closest('[data-pack]'); if (!b || b.disabled) return;
    const [key, what] = b.dataset.pack.split(':');
    if (PACK_UI[key]) busy(b, () => act(key, what));
  });
  onPack((key, before) => {
    const x = current(key), task = packFinished(before, x), d = packDownloadFinished(before, x);
    const note = task ? finishedText(key, task, before) : d ? downloadText(key, d) : '';
    if (note) toast(note);
  });
}
