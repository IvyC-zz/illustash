// Database checks on the page (#212): the 「数据库检查」 line of the settings data card and the notice when a check found
// damage. The checks themselves are core/integrity.js.
import {t, tb} from '../core/i18n.js';
import {esc, localStamp} from '../core/format.js';
import {state} from '../core/state.js';
import {onIntegrity, pollIntegrity, checkNow} from '../core/integrity.js';
import {$, toast} from './dom.js';
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
function showDamage() {
  const damaged = state.integrity?.damaged;
  $('damageBanner').textContent = damaged ? tb(damaged.message) : ''; $('damageBanner').hidden = !damaged;
}
export function wireIntegrity() {
  onIntegrity(() => { showDamage(); renderIntegrityLine(); });
}
