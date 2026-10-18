// Database checks on the page (#212): the 「数据库检查」 line of the settings data card and the notice when a check found
// damage, with 「抢救重建」 when it is in source.sqlite (#228). The checks themselves are core/integrity.js.
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
// The settings data card has just been drawn: fill its line, asking first when nothing has been read yet.
export function renderIntegrityLine() {
  if (!$('integrityLine')) return;
  if (!state.integrity) { pollIntegrity(); return; }
  $('integrityLine').innerHTML = integrityLine();
  $('checkNow').onclick = async () => { try { await checkNow(); } catch (e) { toast(e.message); } };
}
// source.sqlite damaged: the rescue's button, then its progress, or why it did not finish (it can be started again).
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
// The new source.sqlite is put in place as the library opens again; the reopened page says what the rescue did.
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
