'use strict';
// Interface language (#173, #182, docs/WEB_V2.md). Served at /i18n.js after three lines the server writes:
// self.ILLUSTASH_LANGUAGE — 'system', 'zh-CN' or a code from web/static/locales, from the app-wide preferences.json;
// self.ILLUSTASH_LANGUAGES — every locales/<code>.json as {code: the language's name in itself}; and
// self.ILLUSTASH_TABLES — {code: table}, only the chosen one, or all of them for 'system', which only the browser
// can resolve. Tables are keyed by the Chinese text itself (gettext style). t('中文原文', {n: 3}) shows the entry of
// the page's language and the Chinese otherwise, so text not yet translated keeps working.
// Placeholders are {name}. Static markup opts in with data-t (its text) and data-t-attr="title aria-label".
// The same Chinese meaning two things takes a context, as gettext's msgctxt: t('移除', {context: 'column'}) looks up
// "column\u0004移除" in the table and still shows 移除 on a Chinese page.
// An entry may hold plural forms, picked by {n} with Intl.PluralRules: {"one": "{n} picture", "other": "{n} pictures"}.
const I18N = (() => {
  const preference = self.ILLUSTASH_LANGUAGE || 'system';
  const languages = [['zh-CN', '简体中文'], ...Object.entries(self.ILLUSTASH_LANGUAGES || {})];
  const codes = languages.map(([code]) => code);
  // Following the system: the first browser language that has a table, by its exact code, then by its primary
  // language (en-GB → en); any other Chinese shows 简体中文; nothing matching shows English when there is a table.
  const system = () => {
    const wanted = navigator.languages?.length ? navigator.languages : [navigator.language || 'zh'];
    for (const tag of wanted.map(x => String(x).toLowerCase())) {
      const primary = tag.split('-')[0];
      const found = codes.find(c => c.toLowerCase() === tag) || (primary === 'zh' ? 'zh-CN' : codes.find(c => c.toLowerCase().split('-')[0] === primary));
      if (found) return found;
    }
    return codes.includes('en') ? 'en' : 'zh-CN';
  };
  const language = codes.includes(preference) ? preference : system();
  let plurals;
  try { plurals = new Intl.PluralRules(language); } catch { plurals = new Intl.PluralRules('en'); }
  // Numbers and dates follow the page's language through Intl (#182).
  return {preference, languages, language, locale: plurals.resolvedOptions().locale, plurals, table: (self.ILLUSTASH_TABLES || {})[language] || {}};
})();
// {n} is a count: a number, or an integer number() has already formatted ("1,234", "1.234").
const pluralOf = n => {
  const count = typeof n === 'number' ? n : /^\d[\d\s.,'’  ]*$/.test(n ?? '') ? Number(String(n).replace(/\D/g, '')) : NaN;
  return Number.isFinite(count) ? I18N.plurals.select(count) : 'other';
};
function t(text, params) {
  const key = params?.context ? params.context + '\u0004' + text : text;
  let out = Object.hasOwn(I18N.table, key) ? I18N.table[key] : '';
  if (out && typeof out === 'object') out = out[pluralOf(params?.n)] || out.other;
  if (!out || typeof out !== 'string') out = text;
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
