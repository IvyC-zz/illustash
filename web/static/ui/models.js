// 不带识别模型的安装包在第一次使用时下载的模型（#168）：向导横条、任务面板和「设置 › 数据」共用的文字和按钮。用户点
// 「下载」之前什么都不下载；按钮上写明有多大。拒绝的话还有「从文件导入」和手动下载页面。
import {t, tb} from '../core/i18n.js';
import {esc, bytes, number} from '../core/format.js';
import {state} from '../core/state.js';
import {SYNC_ACTIVE, INDEX_ACTIVE, startSync, pollSync, onJob} from '../core/jobs.js';
import {loadModels, modelsPost, modelsWaiting, modelsFinished, modelsArrived, onModels, saveAcceleration, fallbacks} from '../core/models.js';
import {$, toast} from './dom.js';
import {confirmAction} from './dialog.js';
import {home} from '../core/human.js';

const running = m => m?.job?.status === 'running';
const left = m => Math.max(0, (m?.total || 0) - (m?.have || 0));
const fileSize = n => n < 10000 ? t('{n} 字节', {n: number(n)}) : bytes(n);
// 一行：正在发生什么，或缺了什么。
export function modelsLine(m = state.models) {
  if (!m) return '…';
  const j = m.job;
  if (running(m) && j.kind === 'import') return t('正在核对所选文件 {done} / {total}', {done: bytes(j.done), total: bytes(j.total)});
  if (running(m)) return t('正在下载 {done} / {total}', {done: bytes(j.done), total: bytes(j.total)})
    + (j.speed ? ' · ' + t('{speed}/秒', {speed: bytes(j.speed)}) : '') + (j.source ? ' · ' + t('来自 {source}', {source: j.source}) : '')
    + (j.proxy ? ' · ' + t('经代理 {proxy}', {proxy: j.proxy}) : '');
  if (m.status === 'ready') return m.optional_missing ? t('识别模型已就位；WD 加速文件尚未就位，WD 暂用 CPU。') : t('识别模型已就位');
  if (j?.status === 'failed' && j.failed_file) return t('{path} 没能下载，每个来源都试过了', {path: j.failed_file});
  if (j?.status === 'failed') return t('下载没有完成：{reason}', {reason: tb(j.message || '')});
  if (j?.status === 'cancelled') return t('已取消，下好的部分会保留，下次接着下');
  return m.have ? t('还缺 {list}（已有 {have}）', {list: m.missing.join(t('、')), have: bytes(m.have)}) : t('还没下载（约 {size}）', {size: bytes(m.total)});
}
export function modelsBar(m = state.models) {
  const j = m?.job; if (!running(m) || !j.total) return '';
  return `<div class="progress" role="progressbar" aria-valuemin="0" aria-valuemax="${j.total}" aria-valuenow="${j.done}"><span style="width:${Math.min(100, j.done / j.total * 100).toFixed(1)}%"></span></div>`;
}
// 模型已就位后还能补的（#375）：缺 WD 加速文件时下载，开着加速而上次 WD 没用上时修复。只在会自己下载模型的安装里；
// 自带模型的安装，模型在 App 包里，不往里写。
const extra = m => {
  if (m?.status !== 'ready' || !m.on_demand) return '';
  return m.optional_missing ? 'download' : m.acceleration?.backend === 'coreml' && fallbacks(m).wd ? 'repair' : '';
};
// 当前状态下的按钮；`compact` 省掉不常用的那些（任务面板）。
export function modelsActions(m = state.models, compact = false) {
  if (!m) return '';
  if (running(m)) return `<button class="quiet small" data-models="cancel">${t('取消')}</button>`;
  if (m.status === 'ready') {
    const want = extra(m); if (!want) return '';
    const label = want === 'download' ? t('下载 WD 加速文件（约 {size}）', {size: bytes(left(m))}) : t('修复 WD 加速文件');
    return `<button class="quiet small" data-models="download">${label}</button>`
      + `<button class="quiet small" data-models="import">${t('从文件导入…')}</button>`;
  }
  const download = `<button class="primary small" data-models="download">${m.have ? t('继续下载（还差 {left}）', {left: fileSize(left(m))}) : t('下载模型（约 {size}）', {size: bytes(m.total)})}</button>`;
  if (compact) return download;
  return `${download}<button class="quiet small" data-models="import">${t('从文件导入…')}</button>`
    + (m.partial && m.have ? `<button class="quiet small" data-models="discard">${t('删除未完成的部分')}</button>` : '');
}
// 下载卡在某个文件上时，每个来源各自失败的原因。
export function modelsFailures(m = state.models) {
  const list = m?.job?.status === 'failed' ? m.job.failures || [] : [];
  return list.length ? `<ul class="models-results">${list.map(f => `<li class="mismatch">${esc(t('{source}：{why}', {source: f.source, why: tb(f.why)}))}</li>`).join('')}</ul>` : '';
}
// 在会自己下载模型的安装里已经就位（用户 2026-10-19 定，#168）：可以删除，正在使用时不行。
export function deleteButton(m = state.models) {
  if (!m?.on_demand || m.status !== 'ready') return '';
  const busy = running(m) || SYNC_ACTIVE.includes(state.sync?.status) || INDEX_ACTIVE.includes(state.index?.state?.status);
  return `<div class="models-acts"><button class="quiet small" data-models="delete" ${busy ? `disabled title="${t('正在识别或下载，等它结束再删除')}"` : ''}>${t('删除已下载的模型')}</button></div>`;
}
export const manualLink = (m = state.models) => m?.manual ? `<a class="link" href="${esc(m.manual)}" target="_blank" rel="noreferrer noopener">${t('手动下载说明')}</a>` : '';
// 导入对每个选中的文件做了什么。
const RESULT = {accepted: () => t('已接收'), mismatch: () => t('SHA-256 不符，已拒绝'), unknown: () => t('不是需要的模型文件'), already: () => t('已经有了')};
export function importSummary(job) {
  if (job?.kind !== 'import' || !job.results?.length) return '';
  return `<ul class="models-results">${job.results.map(r => `<li class="${r.result}"><bdi>${esc(r.name)}</bdi> · ${RESULT[r.result]?.() || esc(r.result)}</li>`).join('')}</ul>`;
}

// 识别等待模型期间向导的横条，出现在这个安装的每个图库里。
export function modelsNotice() {
  const m = state.models; if (!modelsWaiting(m)) return '';
  // 有事发生之后，这一行才说明现状：下载在进行、已停止，或者已经有一部分在这里。
  const line = running(m) || m.job || m.have ? esc(modelsLine(m)) : '';
  return `<div class="models-notice"><span>${running(m) ? '' : t('识别要用的模型还没下载（约 {size}）。图片会先加入图库，可以浏览和手动整理；模型就位后会自动开始识别。', {size: bytes(m.total)}) + ' '}${line}</span>
    ${modelsBar(m)}${running(m) ? '' : modelsFailures(m)}<div class="models-acts">${modelsActions(m)}${running(m) ? '' : manualLink(m)}</div>${running(m) ? '' : importSummary(m.job)}</div>`;
}

// 「使用 GPU 加速识别」（#270）：只在这个构建带加速器的地方出现。全应用共用（preferences.json）；worker 启动时读取。
// 上一个 worker 没能用上加速器时，说明原因，以及它改用 CPU 继续了。
export function accelerationLine(m = state.models) {
  const a = m?.acceleration; if (!a?.available) return '';
  const hint = a.backend === 'coreml'
    ? t('用 Core ML 加速标签与角色特征计算。首次需要准备编译缓存；加速不可用时自动用 CPU。')
    : t('用显卡计算，识别更快，角色判定和只用 CPU 时一致。');
  const fell = Object.values(fallbacks(m))[0];
  return `<label class="switch-line"><span><b>${t('使用 GPU 加速识别')}</b><small>${hint} ${t('关掉就全部用 CPU。')} · ${t('下次识别时生效')}</small>${fell ? `<em>${esc(t('上次没能用上加速，已改用 CPU：{reason}', {reason: tb(fell.reason)}))}</em>` : ''}</span><input type="checkbox" class="switch" data-acceleration ${a.on ? 'checked' : ''}></label>`;
}

// 下载或导入刚结束时提示的一句（#375），没什么要说时为空。模型刚到（`arrived`）时说开始识别；首次下载在同一个任务里
// 接着下 WD 加速文件，它没下成时并进同一句（提示条只有一个，后一条会盖掉前一条）。没下成的是可选文件时写「WD 加速文件」。
export function finishedNote(job, arrived, m = state.models) {
  const optional = !!job.failed_file && !!m?.files?.find(f => f.path === job.failed_file)?.optional;
  const failure = job.status !== 'failed' ? ''
    : optional ? t('WD 加速文件没能下载，WD 暂用 CPU。')
    : job.failed_file ? t('{path} 没能下载，每个来源都试过了', {path: job.failed_file})
    : t('下载没有完成：{reason}', {reason: tb(job.message || '')});
  if (arrived) return optional ? t('识别模型已就位，开始识别；WD 加速文件没能下载，WD 暂用 CPU。')
    : failure ? t('识别模型已就位，开始识别。{problem}', {problem: failure}) : t('识别模型已就位，开始识别');
  if (failure) return failure;
  return job.status === 'done' && job.kind === 'import' && m?.status !== 'ready' ? t('还缺 {n} 个文件', {n: m.missing.length}) : '';
}
// 镜像说明：本平台清单里有可选的 WD 加速文件时（Apple 芯片的 macOS），说明它从哪里下载。
const mirrorNote = m => {
  const hf = {hf: 'https://huggingface.co'};
  return m.files.some(f => f.optional)
    ? t('原始模型先从这个地址下载，不行再换 ModelScope。填 {hf} 就是直连 Hugging Face。WD 加速文件从项目的 GitHub Release 下载。', hf)
    : t('先从这个地址下载，不行再换 ModelScope。填 {hf} 就是直连 Hugging Face。', hf);
};
// 设置 › 数据：状态、存放位置、镜像，以及同样的按钮。
export function renderModelsCard() {
  const card = $('setModels'), m = state.models; if (!card) return;
  if (!m) { card.innerHTML = `<h2>${t('识别模型')}</h2><p class="muted-note">…</p>`; return; }
  const html = `<h2>${t('识别模型')}<span class="set-scope" title="${t('保存在这台电脑上，所有图库共用')}">${t('所有图库共用')}</span></h2>
    <p class="settings-note">${m.on_demand ? t('图片识别要用 WD 和 CCIP 两个模型，安装包里没有带，第一次使用时下载。每个文件都按应用里写好的大小和 SHA-256 核对，对不上就不用，从哪里下载都一样安全。') : t('这个安装自带识别模型。')}</p>
    <div class="setting-line"><span>${esc(modelsLine(m))}${m.status === 'ready' && extra(m) !== 'download' ? '' : `<small class="line-hint">${manualLink(m)}</small>`}</span></div>
    ${modelsBar(m)}${modelsFailures(m)}${importSummary(m.job)}<div class="models-acts">${modelsActions(m)}</div>${deleteButton(m)}
    ${accelerationLine(m)}
    ${m.on_demand ? `<div class="form-grid"><label for="modelMirror">${t('Hugging Face 镜像')}</label><input id="modelMirror" value="${esc(m.mirror)}" placeholder="${esc(m.mirror_default)}" spellcheck="false" autocomplete="off"></div>
    <p class="settings-note">${mirrorNote(m)}</p>
    <div class="sync-now form-actions"><div class="spacer"></div>
      ${m.mirror !== m.mirror_default ? `<button class="quiet" data-models="mirror-reset">${t('恢复默认')}</button>` : ''}<button class="primary" data-models="mirror">${t('保存')}</button></div>` : ''}
    <table class="settings-table models-files"><tr><td>${t('存放位置')}</td><td class="mono" title="${esc(m.location)}">${esc(home(m.location))}</td></tr>
      ${m.files.map(f => `<tr><td class="mono">${esc(f.path)}</td><td class="mono" title="SHA-256 ${esc(f.sha256)}">${fileSize(f.size)} · ${esc(f.sha256.slice(0, 12))}…</td></tr>`).join('')}</table>`;
  // 任务运行期间会轮询：只在有变化时重画，正在输入镜像时绝不重画。
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
// 重画所有显示模型的地方；向导横条自己画自己（ui/guide.js 也在监听）。
export function renderModels() { renderModelsCard(); }

// 处理函数。页面启动时调用一次。
export function wireModels() {
  document.addEventListener('click', e => {
    const button = e.target.closest('[data-models]'); if (!button || button.disabled) return;
    button.disabled = true;
    act(button.dataset.models);
  });
  document.addEventListener('change', async e => {
    const box = e.target.closest('[data-acceleration]'); if (!box) return;
    box.disabled = true;
    try { await saveAcceleration(box.checked); toast(t('设置已保存')); } catch (err) { toast(err.message); box.checked = !box.checked; }
    box.disabled = false;
  });
  onModels((before, start) => {
    renderModels();
    const job = modelsFinished(before); if (!job) return;
    const arrived = modelsArrived(before, start), note = finishedNote(job, arrived);
    if (note) toast(note);
    // 图库里已有的图片等的就是这个：现在就识别它们。
    if (arrived && !SYNC_ACTIVE.includes(state.sync?.status) && state.sync?.status !== 'off') startSync().then(pollSync).catch(e => toast(e.message));
  });
  // 一次同步结束：重新读取模型状态，卡片上的加速回退原因跟着最近一次识别变，不用等打开任务面板。
  onJob((job, change) => { if (job === 'sync' && change.ended) loadModels().catch(() => {}); });
  loadModels().catch(() => {});
}
