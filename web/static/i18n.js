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
// Text the backend sends (#173 stage 3) stays Chinese on the wire and in the database: replies and stored records
// (sync records, undo labels) keep their bytes, and the backend never needs the page's language. Its Chinese
// templates are keys of the same tables, written exactly as in the Rust source ("已将 {n} 张移出 {name}"), so the
// template is the message's code. tb(text) shows a backend message in the page's language: an entry keyed by the
// whole text, or else the template whose wording matches it, each {name} standing for any text; what a placeholder
// stood for is translated the same way (a reason may itself be a backend message). Old records translate as long as
// their template is still in the table; anything else stays as sent.
const backendPattern = (() => {
  const escape = s => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const patterns = new Map();
  return template => {
    let found = patterns.get(template);
    if (!found) {
      const parts = template.split(/\{(\w+)\}/), literal = parts.filter((_, i) => i % 2 === 0);
      found = {template, names: parts.filter((_, i) => i % 2), literal, weight: literal.join('').length,
        // {n} is a count (plural forms pick by it); any other placeholder stands for any text.
        re: new RegExp('^' + literal.map((text, i) => (i ? parts[2 * i - 1] === 'n' ? '(\\d[\\d,.]*)' : '([\\s\\S]*?)' : '') + escape(text)).join('') + '$')};
      patterns.set(template, found);
    }
    return found;
  };
})();
const backendTemplates = (() => {
  let list;
  // Only templates with Chinese wording of their own: "{a}：{b}" alone would match too much.
  return () => list ||= Object.keys(I18N.table).filter(key => !key.includes('\u0004') && /\{\w+\}/.test(key)
    && /\p{Script=Han}/u.test(key.replace(/\{\w+\}/g, ''))).map(backendPattern).sort((a, b) => b.weight - a.weight);
})();
const backendShown = new Map();
function tb(text, depth = 0) {
  if (I18N.language === 'zh-CN' || typeof text !== 'string' || !/\p{Script=Han}/u.test(text)) return text;
  if (Object.hasOwn(I18N.table, text)) return t(text);
  if (!depth && backendShown.has(text)) return backendShown.get(text);
  let out = text;
  if (depth < 3) {
    // The most specific template wins: the one with the most wording of its own.
    for (const p of backendTemplates()) {
      if (!text.startsWith(p.literal[0]) || !text.endsWith(p.literal.at(-1))) continue;
      const m = p.re.exec(text);
      if (!m) continue;
      const params = Object.fromEntries(p.names.map((name, i) => [name, tb(m[i + 1], depth + 1)]));
      out = t(p.template, params);
      break;
    }
    // Several messages the backend joined with ；, when no template covers the whole.
    if (out === text && text.includes('；')) out = text.split('；').map(part => tb(part, depth + 1)).join(t('；', {context: 'join'}));
  }
  if (!depth) {
    if (backendShown.size > 5000) backendShown.clear();
    backendShown.set(text, out);
  }
  return out;
}
// Whether the backend sent the message of this template, whatever the page's language: the code to test. The
// message may come with the backend's prefixes ("不合法：…"), so the template may also match what follows a ：.
function said(text, template) {
  if (typeof text !== 'string') return false;
  const re = backendPattern(template).re;
  for (let at = 0; at >= 0; at = text.indexOf('：', at) + 1 || -1) if (text.slice(at) === template || re.test(text.slice(at))) return true;
  return false;
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
