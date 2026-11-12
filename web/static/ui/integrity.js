// 页面上的数据库检查（#212）：设置里数据卡片的「数据库检查」一行，以及检查发现损坏时的提示，损坏在 source.sqlite
// 时带「抢救重建」（#228）。检查本身在 core/integrity.js。
import {t, tb} from '../core/i18n.js';
import {esc, localStamp} from '../core/format.js';
import {state} from '../core/state.js';
import {onIntegrity, pollIntegrity, checkNow, rescueNow} from '../core/integrity.js';
import {loadLibraries} from '../core/settings.js';
import {$, toast} from './dom.js';
import {openLibrary} from './settings.js';
function integrityLine() {
  const g = state.integrity, run = g?.running;
  const when = file => g.checked?.[file] ? localStamp(g.checked[file]) : t('还没有检查过');
  const hint = !g ? '' : run ? t('正在检查 {file}，{percent}%', {file: run.database, percent: Math.floor(run.done * 100)})
    : t('上次检查：library.sqlite {library}，source.sqlite {source}', {library: when('library.sqlite'), source: when('source.sqlite')});
  const failed = g?.error && !run ? `<small class="line-hint">${esc(t('上次检查没有完成：{error}', {error: g.error}))}</small>` : '';
  return `<span>${t('数据库检查')}<small class="line-hint">${esc(hint)}</small>${failed}</span><button class="quiet small" id="checkNow" ${!g || run || g.damaged ? 'disabled' : ''}>${t('立即检查')}</button>`;
}
// 设置的数据卡片刚画好：填上它的那一行，还什么都没读过时先去问。
export function renderIntegrityLine() {
  if (!$('integrityLine')) return;
  if (!state.integrity) { pollIntegrity(); return; }
  $('integrityLine').innerHTML = integrityLine();
  $('checkNow').onclick = async () => { try { await checkNow(); } catch (e) { toast(e.message); } };
}
// source.sqlite 损坏：抢救的按钮，然后是它的进度，或它没有完成的原因（可以再次开始）。
function rescueLine(rescue) {
  if (rescue?.running) return `<p class="damage-line">${esc(t('正在抢救：{stage}，{percent}%', {stage: tb(rescue.running.stage), percent: Math.floor(rescue.running.done * 100)}))}</p>`;
  if (rescue?.ready) return `<p class="damage-line">${esc(t('抢救完成，正在重新打开图库…'))}</p>`;
  const failed = rescue?.error ? `<p class="damage-line">${esc(t('抢救没有完成：{error}', {error: tb(rescue.error)}))}</p>` : '';
  return `${failed}<p class="damage-line"><button class="small" id="rescueNow">${t('抢救重建')}</button></p>`;
}
function showDamage() {
  const g = state.integrity, damaged = g?.damaged;
  const rescuable = damaged?.database === 'source.sqlite';
  $('damageBanner').innerHTML = damaged ? esc(tb(damaged.message)) + (rescuable ? rescueLine(g.rescue) : '') : '';
  $('damageBanner').hidden = !damaged;
  if ($('rescueNow')) $('rescueNow').onclick = async () => {
    $('rescueNow').disabled = true;
    try { await rescueNow(); } catch (e) { toast(e.message); $('rescueNow') && ($('rescueNow').disabled = false); }
  };
}
// 新的 source.sqlite 在图库重新打开时换上；重新打开的页面会说明抢救做了什么。
let reopening = false;
async function reopenRescued() {
  if (reopening || !state.integrity?.rescue?.ready) return;
  reopening = true;
  await loadLibraries();
  const lib = state.libraries;
  if (lib?.current) await openLibrary(lib.current, lib.info?.name || '');
  reopening = false;
}
export function wireIntegrity() {
  onIntegrity(() => { showDamage(); renderIntegrityLine(); reopenRescued(); });
}
