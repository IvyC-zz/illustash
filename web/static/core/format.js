// 各布局共用的格式化（#199）：转义、数字、日期和快捷键名称。不碰 DOM；页面语言取自 I18N（web/static/i18n.js）。
import {I18N} from './i18n.js';
export const esc = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
// 让静态帮助和动态消息里显示的修饰键保持一致。
export const macLike = platform => /Mac|iPhone|iPad|iPod/.test(platform || '');
export const isMac = macLike(globalThis.navigator?.userAgentData?.platform || globalThis.navigator?.platform);
export const shortcut = (key, mac = isMac) => mac ? `⌘${key}` : `Ctrl+${key}`;
// 用 ⌘ 写的帮助文字：其他系统上换成 Ctrl（单独的 ⌘ 换成 Ctrl，⌘K 换成 Ctrl+K）。
export const platformText = (text, mac = isMac) => mac ? text : text.replace(/⌘(?=\s|$)/g, 'Ctrl').replaceAll('⌘', 'Ctrl+');
export const number = n => Number(n || 0).toLocaleString(I18N.locale);
// 日期（#182）：中文保留自己的空格（「5 月 3 日」）；其他语言照 Intl。
export const dateText = (y, m, d, withYear) => I18N.language === 'zh-CN' ? (withYear ? `${y} 年 ` : '') + `${m} 月 ${d} 日`
  : new Intl.DateTimeFormat(I18N.locale, withYear ? {year: 'numeric', month: 'numeric', day: 'numeric'} : {month: 'numeric', day: 'numeric'}).format(new Date(+y, m - 1, +d));
export const monthText = (y, m) => I18N.language === 'zh-CN' ? `${y} 年 ${m} 月` : new Intl.DateTimeFormat(I18N.locale, {year: 'numeric', month: 'short'}).format(new Date(+y, m - 1, 1));
export const work = w => (w || '').replaceAll('_', ' ');
// 大小和后端的时间戳，照设置和任务面板显示的样子。
export const bytes = n => n >= 1e9 ? (n / 1e9).toFixed(1) + ' GB' : n >= 1e6 ? (n / 1e6).toFixed(1) + ' MB' : Math.round(n / 1e3) + ' KB';
export const stamp = t => (t || '').replace('T', ' ').slice(5, 16);
// Unix 秒数 → 本地时间「MM-DD hh:mm」。
export const localStamp = sec => { const d = new Date(sec * 1000), p = n => String(n).padStart(2, '0'); return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`; };
export const pct = (n, total) => total ? `${(n / total * 100).toFixed(1)}%` : '—';
// provenance.sqlite 存的是 SQLite 的 UTC 时间戳。
export const localTime = utc => { const d = new Date(String(utc || '').replace(' ', 'T') + 'Z'); return isNaN(d) ? '' : `${d.getMonth() + 1}-${String(d.getDate()).padStart(2, '0')} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`; };
