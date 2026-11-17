// 各布局共用的格式化（#199）：转义、数字、日期和快捷键名称。不碰 DOM；页面语言取自 I18N（web/static/i18n.js）。
import {I18N} from './i18n.js';
export const esc = value => String(value ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
// 让静态帮助和动态消息里显示的修饰键保持一致。
// 不分大小写：Chromium 的 userAgentData.platform 在 macOS 上是 "macOS"（#389）。
export const macLike = platform => /mac|iphone|ipad|ipod/i.test(platform || '');
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
// 缩略图档位（#452）：原生端 thumb 长边 400、medium 长边 800。cssPx 是调用处估的显示尺寸（CSS 像素），乘屏幕倍率
// 换成物理像素，超过 260 用 medium。倍率按 1 封底，1 倍屏的结果和只看 CSS 像素时一样。倍率每次画的时候读：窗口
// 拖到倍率不同的屏幕后，下次重画换档。
export const screenScale = (dpr = globalThis.devicePixelRatio) => Math.max(1, Number(dpr) || 1);
export const thumbSize = (cssPx, dpr) => cssPx * screenScale(dpr) > 260 ? 'medium' : 'thumb';
// 大小和后端的时间戳，照设置和任务面板显示的样子。
export const bytes = n => n >= 1e9 ? (n / 1e9).toFixed(1) + ' GB' : n >= 1e6 ? (n / 1e6).toFixed(1) + ' MB' : Math.round(n / 1e3) + ' KB';
export const stamp = t => (t || '').replace('T', ' ').slice(5, 16);
// Unix 秒数 → 本地时间「MM-DD hh:mm」。
export const localStamp = sec => { const d = new Date(sec * 1000), p = n => String(n).padStart(2, '0'); return `${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`; };
export const pct = (n, total) => total ? `${(n / total * 100).toFixed(1)}%` : '—';
// 后端的 UTC 时间戳（SQLite 的「YYYY-MM-DD hh:mm:ss」，或带时区的 ISO 8601）按本地时区的日期显示；只有日期的
// （人工结论快照导入的）照原样显示；读不出时返回空。
export const localDate = utc => {
  const s = String(utc || ''), day = s.match(/^(\d{4})-(\d\d)-(\d\d)$/);
  if (day) return dateText(day[1], +day[2], +day[3]);
  const d = new Date(/(Z|[+-]\d\d:?\d\d)$/i.test(s) ? s : s.replace(' ', 'T') + 'Z');
  return isNaN(d) ? '' : dateText(d.getFullYear(), d.getMonth() + 1, d.getDate());
};
// provenance.sqlite 存的是 SQLite 的 UTC 时间戳。
export const localTime = utc => { const d = new Date(String(utc || '').replace(' ', 'T') + 'Z'); return isNaN(d) ? '' : `${d.getMonth() + 1}-${String(d.getDate()).padStart(2, '0')} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`; };
