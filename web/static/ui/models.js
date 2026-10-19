// The recognition models an installer without them downloads on first use (#168): the words and buttons the guide's
// bar, the task panel and 设置 › 数据 share. Nothing is downloaded until the person clicks 下载; the button says how
// much. Refusing leaves 从文件导入 and the manual download page.
import {t, tb} from '../core/i18n.js';
import {esc, bytes, number} from '../core/format.js';
import {state} from '../core/state.js';
import {SYNC_ACTIVE, INDEX_ACTIVE, startSync, pollSync} from '../core/jobs.js';
import {loadModels, modelsPost, modelsWaiting, modelsFinished, onModels} from '../core/models.js';
import {$, toast} from './dom.js';
import {confirmAction} from './dialog.js';

const running = m => m?.job?.status === 'running';
const left = m => Math.max(0, (m?.total || 0) - (m?.have || 0));
const fileSize = n => n < 10000 ? t('{n} 字节', {n: number(n)}) : bytes(n);
// One line: what is happening or what is missing.
export function modelsLine(m = state.models) {
  if (!m) return '…';
  const j = m.job;
  if (running(m) && j.kind === 'import') return t('正在核对所选文件 {done} / {total}', {done: bytes(j.done), total: bytes(j.total)});
  if (running(m)) return t('正在下载 {done} / {total}', {done: bytes(j.done), total: bytes(j.total)})
    + (j.speed ? ' · ' + t('{speed}/秒', {speed: bytes(j.speed)}) : '') + (j.source ? ' · ' + t('来自 {source}', {source: j.source}) : '')
    + (j.proxy ? ' · ' + t('经代理 {proxy}', {proxy: j.proxy}) : '');
  if (m.status === 'ready') return t('识别模型已就位');
  if (j?.status === 'failed' && j.failed_file) return t('{path} 没能下载，每个来源都试过了', {path: j.failed_file});
  if (j?.status === 'failed') return t('下载没有完成：{reason}', {reason: tb(j.message || '')});
  if (j?.status === 'cancelled') return t('已取消，下好的部分会保留，下次接着下');
  return m.have ? t('还缺 {list}（已有 {have}）', {list: m.missing.join(t('、')), have: bytes(m.have)}) : t('还没下载（约 {size}）', {size: bytes(m.total)});
}
export function modelsBar(m = state.models) {
  const j = m?.job; if (!running(m) || !j.total) return '';
  return `<div class="progress" role="progressbar" aria-valuemin="0" aria-valuemax="${j.total}" aria-valuenow="${j.done}"><span style="width:${Math.min(100, j.done / j.total * 100).toFixed(1)}%"></span></div>`;
}
// The buttons for the current state; `compact` leaves out the less common ones (the task panel).
export function modelsActions(m = state.models, compact = false) {
  if (!m || m.status === 'ready') return '';
  if (running(m)) return `<button class="quiet small" data-models="cancel">${t('取消')}</button>`;
  const download = `<button class="primary small" data-models="download">${m.have ? t('继续下载（还差 {left}）', {left: fileSize(left(m))}) : t('下载模型（约 {size}）', {size: bytes(m.total)})}</button>`;
  if (compact) return download;
  return `${download}<button class="quiet small" data-models="import">${t('从文件导入…')}</button>`
    + (m.partial && m.have ? `<button class="quiet small" data-models="discard">${t('删除未完成的部分')}</button>` : '');
}
// Why each source failed for the file that stopped the download.
export function modelsFailures(m = state.models) {
  const list = m?.job?.status === 'failed' ? m.job.failures || [] : [];
  return list.length ? `<ul class="models-results">${list.map(f => `<li class="mismatch">${esc(t('{source}：{why}', {source: f.source, why: tb(f.why)}))}</li>`).join('')}</ul>` : '';
}
// In place in an install that downloads them (user 2026-10-19, #168): they can be deleted, not while in use.
export function deleteButton(m = state.models) {
  if (!m?.on_demand || m.status !== 'ready') return '';
  const busy = running(m) || SYNC_ACTIVE.includes(state.sync?.status) || INDEX_ACTIVE.includes(state.index?.state?.status);
  return `<div class="models-acts"><button class="quiet small" data-models="delete" ${busy ? `disabled title="${t('正在识别或下载，等它结束再删除')}"` : ''}>${t('删除已下载的模型')}</button></div>`;
}
export const manualLink = (m = state.models) => m?.manual ? `<a class="link" href="${esc(m.manual)}" target="_blank" rel="noreferrer noopener">${t('手动下载说明')}</a>` : '';
// What an import made of each chosen file.
const RESULT = {accepted: () => t('已接收'), mismatch: () => t('SHA-256 不符，已拒绝'), unknown: () => t('不是需要的模型文件'), already: () => t('已经有了')};
export function importSummary(job) {
  if (job?.kind !== 'import' || !job.results?.length) return '';
  return `<ul class="models-results">${job.results.map(r => `<li class="${r.result}"><bdi>${esc(r.name)}</bdi> · ${RESULT[r.result]?.() || esc(r.result)}</li>`).join('')}</ul>`;
}

// The guide's bar while recognition waits for the models, in every library of this install.
export function modelsNotice() {
  const m = state.models; if (!modelsWaiting(m)) return '';
  // The line says how it stands once something happened: a download running, stopped, or a part already here.
  const line = running(m) || m.job || m.have ? esc(modelsLine(m)) : '';
  return `<div class="models-notice"><span>${running(m) ? '' : t('识别要用的模型还没下载（约 {size}）。图片会先加入图库，可以浏览和手动整理；模型就位后会自动开始识别。', {size: bytes(m.total)}) + ' '}${line}</span>
    ${modelsBar(m)}${running(m) ? '' : modelsFailures(m)}<div class="models-acts">${modelsActions(m)}${running(m) ? '' : manualLink(m)}</div>${running(m) ? '' : importSummary(m.job)}</div>`;
}

// 设置 › 数据: status, where they are, the mirror, and the same buttons.
export function renderModelsCard() {
  const card = $('setModels'), m = state.models; if (!card) return;
  if (!m) { card.innerHTML = `<h2>${t('识别模型')}</h2><p class="muted-note">…</p>`; return; }
  const html = `<h2>${t('识别模型')}<span class="set-scope" title="${t('保存在这台电脑上，所有图库共用')}">${t('所有图库共用')}</span></h2>
    <p class="settings-note">${m.on_demand ? t('图片识别要用 WD 和 CCIP 两个模型，安装包里没有带，第一次使用时下载。每个文件都按应用里写好的大小和 SHA-256 核对，对不上就不用，从哪里下载都一样安全。') : t('这个安装自带识别模型。')}</p>
    <div class="setting-line"><span>${esc(modelsLine(m))}${m.status === 'ready' ? '' : `<small class="line-hint">${manualLink(m)}</small>`}</span></div>
    ${modelsBar(m)}${modelsFailures(m)}${importSummary(m.job)}${m.status === 'ready' ? deleteButton(m) : `<div class="models-acts">${modelsActions(m)}</div>`}
    ${m.on_demand ? `<div class="form-grid"><label for="modelMirror">${t('Hugging Face 镜像')}</label><input id="modelMirror" value="${esc(m.mirror)}" placeholder="${esc(m.mirror_default)}" spellcheck="false" autocomplete="off"></div>
    <p class="settings-note">${t('先从这个地址下载，不行再换 ModelScope。填 {hf} 就是直连 Hugging Face。', {hf: 'https://huggingface.co'})}</p>
    <div class="sync-now form-actions"><div class="spacer"></div>
      ${m.mirror !== m.mirror_default ? `<button class="quiet" data-models="mirror-reset">${t('恢复默认')}</button>` : ''}<button class="primary" data-models="mirror">${t('保存')}</button></div>` : ''}
    <table class="settings-table models-files"><tr><td>${t('存放位置')}</td><td class="mono">${esc(m.location)}</td></tr>
      ${m.files.map(f => `<tr><td class="mono">${esc(f.path)}</td><td class="mono" title="SHA-256 ${esc(f.sha256)}">${fileSize(f.size)} · ${esc(f.sha256.slice(0, 12))}…</td></tr>`).join('')}</table>`;
  // Polled while a job runs: redraw only on change, and never under a mirror being typed.
  if (card.dataset.html === html || card.contains(document.activeElement) && document.activeElement.matches('input')) return;
  card.dataset.html = html; card.innerHTML = html;
}

async function act(what) {
  try {
    if (what === 'download') await modelsPost({action: 'download'});
    else if (what === 'cancel') await modelsPost({action: 'cancel'});
    else if (what === 'import') { const r = await modelsPost({action: 'import'}); if (r.cancelled) return; }
    else if (what === 'delete') {
      if (!await confirmAction(t('删除已下载的识别模型（约 {size}）？删除后要重新下载或从文件导入，才能识别新加入的图片；已经识别的结果都保留。', {size: bytes(state.models.total)}), t('删除识别模型'), t('删除'))) return;
      await modelsPost({action: 'delete'});
      toast(t('识别模型已删除'));
    } else if (what === 'discard') {
      if (!await confirmAction(t('删除已经下载但还没完成的部分？下次要从头下载。'), t('删除未完成的部分'), t('删除'))) return;
      await modelsPost({action: 'discard'});
    } else if (what === 'mirror' || what === 'mirror-reset') {
      await modelsPost({action: 'mirror', mirror: what === 'mirror' ? $('modelMirror').value.trim() || null : null});
      toast(t('设置已保存'));
    }
  } catch (e) { toast(e.message); }
  renderModels();
}
// Redraws whatever shows the models; the guide's bar draws itself (ui/guide.js listens too).
export function renderModels() { renderModelsCard(); }

// The handlers. Called once, when the page starts.
export function wireModels() {
  document.addEventListener('click', e => {
    const button = e.target.closest('[data-models]'); if (!button || button.disabled) return;
    button.disabled = true;
    act(button.dataset.models);
  });
  onModels(before => {
    renderModels();
    const job = modelsFinished(before); if (!job) return;
    if (job.status === 'done' && state.models.status === 'ready') {
      // The pictures already in the library were waiting for exactly this: recognize them now.
      toast(t('识别模型已就位，开始识别'));
      if (!SYNC_ACTIVE.includes(state.sync?.status) && state.sync?.status !== 'off') startSync().then(pollSync).catch(e => toast(e.message));
    } else if (job.status === 'done' && job.kind === 'import') {
      toast(t('还缺 {n} 个文件', {n: state.models.missing.length}));
    } else if (job.status === 'failed') toast(job.failed_file ? t('{path} 没能下载，每个来源都试过了', {path: job.failed_file}) : t('下载没有完成：{reason}', {reason: tb(job.message || '')}));
  });
  loadModels().catch(() => {});
}
