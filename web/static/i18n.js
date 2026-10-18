'use strict';
// Interface language (#173, docs/WEB_V2.md). Served at /i18n.js after two lines the server writes:
// self.ILLUSTASH_LANGUAGE — 'system', 'zh-CN' or 'en' from the app-wide preferences.json — and self.ILLUSTASH_EN,
// the English table from locales/en.json, keyed by the Chinese text itself (gettext style). t('中文原文', {n: 3})
// shows the English entry on an English page and the Chinese otherwise, so text not yet translated keeps working.
// Placeholders are {name}. Static markup opts in with data-t (its text) and data-t-attr="title aria-label".
// The same Chinese meaning two things takes a context, as gettext's msgctxt: t('移除', {context: 'column'}) looks up
// "column\u0004移除" in the table and still shows 移除 on a Chinese page.
const I18N = (() => {
  const preference = self.ILLUSTASH_LANGUAGE || 'system';
  const first = (navigator.languages && navigator.languages[0]) || navigator.language || 'zh';
  const language = preference === 'system' ? (/^zh\b/i.test(first) ? 'zh-CN' : 'en') : preference;
  return {preference, language, table: language === 'en' ? self.ILLUSTASH_EN || {} : {}};
})();
function t(text, params) {
  const key = params?.context ? params.context + '\u0004' + text : text;
  const out = Object.hasOwn(I18N.table, key) && I18N.table[key] ? I18N.table[key] : text;
  return params ? out.replace(/\{(\w+)\}/g, (all, key) => Object.hasOwn(params, key) ? String(params[key]) : all) : out;
}
function translateMarkup(root) {
  if (I18N.language === 'zh-CN') return;
  for (const el of root.querySelectorAll('[data-t]')) el.textContent = t(el.textContent);
  for (const el of root.querySelectorAll('[data-t-attr]')) {
    for (const name of el.dataset.tAttr.split(' ')) if (el.hasAttribute(name)) el.setAttribute(name, t(el.getAttribute(name)));
  }
}
document.documentElement.lang = I18N.language;
translateMarkup(document);
