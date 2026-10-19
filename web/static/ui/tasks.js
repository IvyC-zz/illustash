// The task panel (desktop layout, #199): every background job's progress, confirmations and manual runs in one place.
// The jobs, their polling and their words are core/jobs.js.
import {t, tb} from '../core/i18n.js';
import {esc, number, stamp, localTime} from '../core/format.js';
import {state} from '../core/state.js';
import {api, post, shared, readError} from '../core/session.js';
import {SYNC_ACTIVE, INDEX_ACTIVE, NAMES_ACTIVE, SIMILAR_ACTIVE, PROV_ACTIVE, RUN_STATUS, onJob, syncLine, syncBar, syncDoneText, indexLine, indexEndText,
  namesLine, taskSimilar, pollSync, pollIndex, pollNames, loadProvenance, startSync, checkIndex, refreshNames, provenanceAction} from '../core/jobs.js';
import {loadLibraries, updatesTick} from '../core/settings.js';
import {$, toast, error, fold, wireFolds} from './dom.js';
import {confirmAction} from './dialog.js';
import {openIndexAdd} from './catalog.js';
import {modelsLine, modelsBar, modelsActions} from './models.js';
import {loadModels, onModels} from '../core/models.js';
// 开始 first shows what leaves the machine; the authorization covers this one run only.
let provenanceArmed = false;
// Open the panel; with armProvenance, at the authorization for a 找回画师 run.
export function openTasks({armProvenance = false} = {}) {
  if (armProvenance) provenanceArmed = true;
  toggleTasks(true);
}
// Pictures whose files open but are not images the app can read: where they are, as on the library page.
export function syncDamaged(d) {
  if (!d?.count) return '';
  const short = p => p.replace(/^\/Users\/[^/]+/, '~');
  const rows = d.paths.map(p => `<div class="setting-line lib-folder"><span class="lib-path" title="${esc(p)}"><bdi>${esc(short(p))}</bdi></span></div>`).join('');
  const more = d.count > d.paths.length ? `<p class="muted-note">${t('另有 {n} 张未列出。', {n: number(d.count - d.paths.length)})}</p>` : '';
  return fold('task-sync-damaged', t('{n} 张图片已损坏或格式不支持', {n: number(d.count)}),
    `<p class="muted-note">${t('这些文件读得到，但不是能打开的图片：已经加入图库，图上显示「无法打开」，不参与识别。修好或换成正常的图片后，下次同步会重新识别。')}</p>${rows}${more}`);
}
// Progress bars from the worker's numbers (docs/BACKEND_TASKS.md): this step's items, and traffic against the run's cap.
export function provenanceBars(s) {
  const p = s.progress; if (!p || !p.total) return '';
  const bar = (label, done, total, detail) => `<div class="prov-bar-row"><span class="prov-bar-label">${label}</span><span class="prov-bar" role="progressbar" aria-valuemin="0" aria-valuemax="${total}" aria-valuenow="${done}"><span style="width:${Math.min(100, done / total * 100).toFixed(1)}%"></span></span><span class="prov-bar-num">${detail}</span></div>`;
  // Two levels when the step has them (本地比对: all artists, then the current artist's works), then traffic.
  const sub = p.sub_total ? bar(esc(tb(p.sub_label) || t('当前')), p.sub_done || 0, p.sub_total, `${number(p.sub_done || 0)} / ${number(p.sub_total)}`) : '';
  return `<div class="prov-bars">${bar(esc(tb(p.label) || t('进度')), p.done || 0, p.total, `${number(p.done || 0)} / ${number(p.total)}${p.found != null ? ' · ' + t('对上 {n}', {n: number(p.found)}) : ''}`)}${sub}${p.mb != null && s.budget_mb ? bar(t('流量'), p.mb, s.budget_mb, `${number(Math.round(p.mb))} / ${number(s.budget_mb)} MB`) : ''}</div>`;
}

// ---------- 任务面板 (#199): every background job's progress, confirmations and manual runs in one place ----------
// Each job keeps its own endpoint and poller; this only gathers them. While any job runs, its poller re-arms
// itself every 2 s; otherwise a slow tick (fast while the panel is open) catches jobs started elsewhere.
export const taskPanel = {open: false, timer: 0, settingsAt: 0};
export function taskList() {
  const s = state.sync, ix = state.index?.state || {}, nm = state.names?.state || {}, sim = state.settings?.tasks?.similar || {status: 'off'};
  const pv = state.provenance, ps = pv?.state || {}, ex = state.libraries?.export || {};
  const folder = !(state.settings?.package?.sources || state.libraries?.info?.sources || []).some(x => x.kind === 'photos');
  const bar = (done, total) => total ? `<div class="progress"><span style="width:${Math.min(100, Math.round((done || 0) / total * 100))}%"></span></div>` : '';
  const list = [
    {key: 'sync', name: folder ? t('文件夹同步') : t('照片同步'), busy: t('同步中'), active: SYNC_ACTIVE.includes(s.status), off: s.status === 'off', line: s.status === 'off' ? t('这次启动没有开同步') : syncLine(s),
      warn: ['error', 'needs_recognition', 'interrupted'].includes(s.status), body: syncBar(s, bar) + (SYNC_ACTIVE.includes(s.status) ? '' : syncDamaged(state.syncDamaged)),
      action: `<button class="quiet small" data-task="sync" ${SYNC_ACTIVE.includes(s.status) || s.status === 'off' ? 'disabled' : ''}>${t('立即同步')}</button>`,
      log: state.syncHistory?.length ? [t('同步记录'), `<table class="settings-table"><tr><th>${t('时间')}</th><th>${t('新增')}</th><th>${t('移除', {context: 'column'})}</th><th>${t('导入')}</th><th>${t('失败')}</th></tr>${state.syncHistory.slice(0, 8).map(r => `<tr><td>${esc(stamp(r.started_at))}</td><td>${number(r.found)}</td><td>${number(r.removed)}</td><td>${number(r.imported)}</td><td>${number(r.failed)}</td></tr>`).join('')}</table>`] : null},
    {key: 'index', name: t('检查新角色'), busy: t('检查新角色中'), active: INDEX_ACTIVE.includes(ix.status), line: state.index ? indexLine(ix, state.index.check) : '…', warn: ix.status === 'error',
      body: INDEX_ACTIVE.includes(ix.status) ? bar(ix.done, ix.total) : '',
      action: `<button class="quiet small" data-task="index" ${INDEX_ACTIVE.includes(ix.status) || ix.status === 'off' || !state.index?.watch?.works.length ? 'disabled' : ''} title="${state.index?.watch?.works.length ? t('查关注作品在 Danbooru 上有没有新角色（只发送作品名）') : t('先在资料库里关注作品')}">${t('立即检查')}</button>`},
    {key: 'names', name: t('角色名称'), busy: t('角色名称中'), active: NAMES_ACTIVE.includes(nm.status), off: nm.status === 'off', line: namesLine(nm) + (nm.error ? t('：{reason}', {reason: tb(nm.error)}) : ''), warn: nm.status === 'error', body: NAMES_ACTIVE.includes(nm.status) ? bar(nm.done, nm.total) : '',
      action: `<button class="quiet small" data-task="names" ${NAMES_ACTIVE.includes(nm.status) || nm.status === 'off' || !state.names ? 'disabled' : ''} title="${t('从 Bangumi 重新获取全部角色的中文名（只发送 tag 与作品名）')}">${t('重新获取')}</button>`},
    {key: 'similar', name: t('相似图片'), busy: t('相似图片中'), active: SIMILAR_ACTIVE.includes(sim.status), off: sim.status === 'off', line: taskSimilar(sim), warn: sim.status === 'error', body: SIMILAR_ACTIVE.includes(sim.status) ? bar(sim.done, sim.total) : '',
      action: `<button class="quiet small" data-task="similar" ${SIMILAR_ACTIVE.includes(sim.status) || sim.status === 'off' ? 'disabled' : ''}>${t('重新检查')}</button>`},
  ];
  if (pv?.available) list.push({key: 'provenance', name: t('找回画师'), busy: t('找回画师中'), active: PROV_ACTIVE.includes(ps.status), warn: ps.status === 'failed',
    line: ps.status === 'stopping' ? t('正在停止…') : PROV_ACTIVE.includes(ps.status) ? t('正在查找 · 第 {step}/{steps} 步', {step: (ps.index || 0) + 1, steps: ps.total || 1}) + (ps.text ? ' · ' + tb(ps.text) : '')
      : ps.status === 'failed' ? t('上次出错：{reason}', {reason: tb(ps.reason || '')}) : pv.runs?.[0] ? t('上次 {time} · 新找到 {n}', {time: localTime(pv.runs[0].started_at), n: number(pv.runs[0].found || 0)}) : t('还没有查过'),
    body: PROV_ACTIVE.includes(ps.status) ? provenanceBars(ps) + (ps.side ? `<p class="task-side">${t('同时进行')} · ${esc(tb(ps.side.name || ps.side.step))}${ps.side.text ? ' · ' + esc(tb(ps.side.text)) : ''}</p>${provenanceBars({progress: ps.side.progress})}` : '') : '',
    action: PROV_ACTIVE.includes(ps.status) ? `<button class="quiet small" data-prov="stop" ${ps.status === 'stopping' ? 'disabled' : ''}>${t('停止')}</button>` : provenanceArmed ? '' : `<button class="quiet small" data-prov="arm">${t('开始…')}</button>`,
    log: pv.runs?.length ? [t('运行记录'), `<table class="settings-table"><tr><th>${t('开始')}</th><th>${t('方式')}</th><th>${t('新找到')}</th><th>${t('流量')}</th><th>${t('结果')}</th></tr>${pv.runs.slice(0, 8).map(r => `<tr><td>${esc(localTime(r.started_at))}</td><td>${r.kind === 'new' ? t('同步后') : r.kind === 'full' ? t('手动') : t('命令行')}</td><td>${number(r.found || 0)}</td><td>${(r.traffic_mb || 0).toFixed(1)} MB</td><td title="${esc(tb(r.reason || ''))}">${RUN_STATUS[r.status] || esc(r.status)}</td></tr>`).join('')}</table>`] : null});
  // An install without its models (#168): until they are in place, and while a download or import runs.
  const m = state.models;
  if (m?.on_demand && (m.status !== 'ready' || m.job?.status === 'running')) list.push({key: 'models', name: t('识别模型'), busy: m.job?.kind === 'import' ? t('导入识别模型中') : t('下载识别模型中'),
    active: m.job?.status === 'running', warn: m.job?.status === 'failed', line: modelsLine(m), body: modelsBar(m), action: modelsActions(m, true)});
  if (['running', 'done', 'error'].includes(ex.status)) list.push({key: 'export', name: t('导出图库'), busy: t('导出图库中'), active: ex.status === 'running', warn: ex.status === 'error',
    line: ex.status === 'running' ? t('正在导出…') : ex.status === 'done' ? t('已导出 {mb} MB', {mb: (ex.bytes / 1048576).toFixed(0)}) : t('导出失败：{error}', {error: tb(ex.error || '')}), body: '', action: ''});
  return list;
}
// Things that wait for the person: an authorization, characters to add, a sync that needs a hand.
function taskAsks() {
  const asks = [], p = state.provenance, missing = state.index?.check?.missing || [], s = state.sync;
  if (provenanceArmed && p?.available) asks.push(`<div class="task-ask prov-confirm"><b>${t('找回画师这次要访问外网，确认后才会发出：')}</b>
    <ul><li>${t('发给 Danbooru：图片的像素哈希和感知哈希（IQDB 签名，都是一串文本，不是图片）、画师名和账号')}</li>
    <li>${state.accounts?.pixiv?.bound ? t('用你绑定的 Pixiv 登录状态读取你的关注列表（只读）') : t('没有绑定 Pixiv：只看公开作品，不含 R18')}</li>
    <li>${t('本次最多 {mb} MB；遇到拒绝、限流或验证码就停，不绕过', {mb: number(p.budgets?.full || 1024)})}</li></ul>
    <p class="muted-note">${t('授权只对这一次有效，下次开始会再问。图片本身不会上传。查过的图和画师不会重查，可以随时停止。')}</p>
    <div class="prov-confirm-btns"><button class="primary" data-prov="start" data-network="1">${t('授权并开始')}</button><button class="quiet" data-prov="start">${t('只跑本地（签名识别）')}</button><button class="quiet" data-prov="cancel">${t('取消')}</button></div></div>`);
  if (missing.length && !INDEX_ACTIVE.includes(state.index.state.status)) asks.push(`<div class="task-ask"><span>${t('上次检查发现 {n} 个还没收录的角色', {n: `<b>${number(missing.length)}</b>`})}</span><button class="primary small" data-task="index-add">${t('查看并添加…')}</button></div>`);
  return asks;
}
export function renderTasks() {
  const list = taskList(), active = list.filter(t => t.active), warn = list.some(t => t.warn), asks = taskAsks();
  const button = $('taskButton'), badge = $('taskBadge');
  button.classList.toggle('busy', !!active.length);
  $('taskLabel').textContent = active.length > 1 ? t('{n} 项进行中', {n: active.length}) : active.length ? active[0].busy : t('任务');
  button.title = active.length ? active.map(task => t('{name}：{line}', {name: task.name, line: task.line})).join('\n') : asks.length ? t('有事等你确认') : warn ? t('有任务出错了') : t('任务：同步、检查新角色、找回画师…');
  badge.hidden = !asks.length && !warn; badge.className = 'task-badge' + (warn && !asks.length ? ' warn' : '');
  if (!taskPanel.open) return;
  const order = [...active, ...list.filter(t => !t.active)];
  const html = `<div class="task-top"><h2>${t('任务')}</h2><button class="icon" data-task="close" aria-label="${t('关闭')}">×</button></div>
    ${asks.length ? `<h6>${t('等你确认')}</h6><div class="task-asks">${asks.join('')}</div>` : ''}
    <h6>${active.length ? t('进行中与其他任务') : t('全部任务')}</h6><div class="view-group task-group">${order.map(task => `<div class="task-item${task.active ? ' on' : ''}">
      <div class="task-main"><span class="local-dot${task.active ? ' busy' : task.warn ? ' warn' : task.off ? ' off' : ''}"></span><span class="task-text"><b>${task.name}</b><small>${esc(task.line)}</small></span>${task.action}</div>
      ${task.body ? `<div class="task-body">${task.body}</div>` : ''}${task.log ? fold('task-' + task.key, task.log[0], task.log[1]) : ''}</div>`).join('')}</div>
    <p class="task-foot">${t('各任务是否自动运行，在{link}里改。', {link: `<a class="link" href="#/settings">${t('设置')}</a>`})}</p>`;
  const pop = $('taskPanel');
  // Polled while jobs run: redraw only on change, so a click is never lost to a replaced button.
  if (pop.dataset.html === html) return;
  pop.dataset.html = html; pop.innerHTML = html; wireFolds(pop);
}
export async function taskTick() {
  clearTimeout(taskPanel.timer);
  const jobs = [pollSync(), pollIndex(), pollNames(), loadProvenance()];
  // The models' own poller runs while they download; this catches a download started in another window.
  if (taskPanel.open || !state.models) jobs.push(loadModels());
  // The similar job's state comes with /api/settings, which also measures the caches: read it only when looked at.
  if (taskPanel.open && Date.now() - taskPanel.settingsAt > (SIMILAR_ACTIVE.includes(state.settings?.tasks?.similar?.status) ? 2500 : 15000)) { taskPanel.settingsAt = Date.now(); jobs.push(shared('/api/settings').then(s => { state.settings = s; }).catch(e => error(readError(e)))); }
  // The library list (the sync task's name and the export's progress) is read once the panel is open.
  if (taskPanel.open && !state.libraries) jobs.push(loadLibraries());
  await Promise.allSettled(jobs);
  renderTasks();
  updatesTick();
  taskPanel.timer = setTimeout(taskTick, taskPanel.open ? 3000 : 30000);
}
export function toggleTasks(open = !taskPanel.open) {
  taskPanel.open = open; $('taskPanel').hidden = !open;
  $('taskButton').setAttribute('aria-expanded', open); $('taskButton').classList.toggle('on', open);
  if (open) { delete $('taskPanel').dataset.html; renderTasks(); taskTick(); }
  else provenanceArmed = false;
}
// The panel's handlers. Called once, when the page starts.
export function wireTasks() {
  // The models' download (#168) keeps its own poller: the button and panel follow each read.
  onModels(() => renderTasks());
  onJob(async (job, change) => {
    if (job === 'sync' && change.ended && state.sync.status === 'done' && state.sync.imported) toast(syncDoneText(state.sync));
    if (job === 'index' && change.ended && indexEndText(state.index.state)) toast(indexEndText(state.index.state));
    renderTasks();
  });
  $('taskPanel').addEventListener('click', async e => {
    const b = e.target.closest('[data-prov]'); if (!b) return;
    // 开始 first shows what leaves the machine; the authorization covers this one run only.
    if (b.dataset.prov === 'arm' || b.dataset.prov === 'cancel') { provenanceArmed = b.dataset.prov === 'arm'; renderTasks(); return; }
    b.disabled = true;
    const body = {action: b.dataset.prov};
    if (b.dataset.prov === 'start') { body.allow_network = b.dataset.network === '1'; provenanceArmed = false; }
    try { await provenanceAction(body); } catch (err) { toast(err.message); }
  });
  $('taskButton').onclick = e => { e.stopPropagation(); toggleTasks(); };
  $('taskPanel').addEventListener('click', async e => {
    e.stopPropagation();
    const b = e.target.closest('[data-task]'); if (!b) { if (e.target.closest('a[href]')) toggleTasks(false); return; }
    const act = b.dataset.task;
    if (act === 'close') { toggleTasks(false); return; }
    if (act === 'index-add') { openIndexAdd(); return; }
    b.disabled = true;
    try {
      if (act === 'sync') await startSync();
      else if (act === 'index') { if (!await checkIndex(confirmAction)) return; }
      else if (act === 'names') { await refreshNames(); toast(t('已开始重新获取角色名称')); }
      else if (act === 'similar') { await post('/api/similar', {}); state.settings = await api('/api/settings'); }
    } catch (err) { toast(err.message); } finally { b.disabled = false; }
    taskTick();
  });
  document.addEventListener('click', e => { if (taskPanel.open && !e.target.closest('#taskPanel, dialog')) toggleTasks(false); });
  document.addEventListener('keydown', e => { if (e.key === 'Escape' && taskPanel.open && !document.querySelector('dialog[open]')) { toggleTasks(false); e.stopImmediatePropagation(); } }, true);
}
