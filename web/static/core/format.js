// Formatting shared by every layout (#199): escaping, numbers, dates and shortcut names. No DOM; the page's language
// comes from I18N (web/static/i18n.js).
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
