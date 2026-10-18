// Formatting shared by every layout (#199): escaping, numbers, dates and shortcut names. No DOM; the page's language
// comes from I18N (web/static/i18n.js).
import {I18N} from './i18n.js';
export const esc = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
// Keep displayed modifiers consistent across static help and dynamic messages.
export const macLike = platform => /Mac|iPhone|iPad|iPod/.test(platform || '');
export const isMac = macLike(globalThis.navigator?.userAgentData?.platform || globalThis.navigator?.platform);
export const shortcut = (key, mac = isMac) => mac ? `⌘${key}` : `Ctrl+${key}`;
export const number = n => Number(n || 0).toLocaleString(I18N.locale);
// Dates (#182): Chinese keeps its own spacing (「5 月 3 日」); other languages follow Intl.
export const dateText = (y, m, d, withYear) => I18N.language === 'zh-CN' ? (withYear ? `${y} 年 ` : '') + `${m} 月 ${d} 日`
  : new Intl.DateTimeFormat(I18N.locale, withYear ? {year: 'numeric', month: 'numeric', day: 'numeric'} : {month: 'numeric', day: 'numeric'}).format(new Date(+y, m - 1, +d));
export const monthText = (y, m) => I18N.language === 'zh-CN' ? `${y} 年 ${m} 月` : new Intl.DateTimeFormat(I18N.locale, {year: 'numeric', month: 'short'}).format(new Date(+y, m - 1, 1));
export const work = w => (w || '').replaceAll('_', ' ');
// Sizes and the backend's timestamps, as the settings and the task panel show them.
export const bytes = n => n >= 1e9 ? (n / 1e9).toFixed(1) + ' GB' : n >= 1e6 ? (n / 1e6).toFixed(1) + ' MB' : Math.round(n / 1e3) + ' KB';
export const stamp = t => (t || '').replace('T', ' ').slice(5, 16);
// Unix seconds → local "MM-DD hh:mm".
export const localStamp = sec => { const d = new Date(sec * 1000), p = n => String(n).padStart(2, '0'); return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`; };
export const pct = (n, total) => total ? `${(n / total * 100).toFixed(1)}%` : '—';
// provenance.sqlite keeps SQLite's UTC timestamps.
export const localTime = utc => { const d = new Date(String(utc || '').replace(' ', 'T') + 'Z'); return isNaN(d) ? '' : `${d.getMonth() + 1}-${String(d.getDate()).padStart(2, '0')} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`; };
