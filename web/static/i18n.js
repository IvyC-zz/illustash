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
// whole text, or else the template whose wording matches it, each {name} standing for any text. Old records translate
// as long as their template is still in the table; anything else stays as sent.
// What a placeholder stood for (#198): a nested message of the backend's own — an error, a reason, a step, a duration
// it wrote — is translated the same way; anything else is data, which the user, the library or the outside world
// named (a tag, a character, a path, a key, a number), and is shown as sent, even when it reads like a table key
// (a tag named 未识别). A name in neither list counts as data. tests/i18n.test.cjs checks that each placeholder of the
// backend's templates is in exactly one of the two, so a new template has to say which its placeholders are.
const BACKEND_NESTED = new Set(['cleanup', 'duration', 'e', 'error', 'label', 'm', 'message', 'msg', 'note', 'reason', 'said', 'step', 'why']);
const BACKEND_DATA = new Set(['CONFLICT_RETRIES', 'FORMAT', 'MAX_ASSETS', 'MAX_DATA_BYTES', 'MAX_LINE_BYTES', 'SCHEMA_VERSION',
  'WORKER_KIND_ENV', 'action', 'anchor', 'arg', 'artist', 'artists', 'at', 'attempt', 'auto', 'available', 'backup', 'before', 'candidates',
  'choice', 'cid', 'code', 'context', 'differs', 'distance', 'done', 'entity', 'expected', 'fetched', 'field',
  'file', 'found', 'full', 'host', 'id', 'key', 'kind', 'left', 'library_id', 'limit', 'listed', 'loaded', 'matches',
  'max', 'mb', 'method', 'missing', 'moved', 'n', 'name', 'named', 'namespace', 'needed', 'no', 'object', 'origin', 'other',
  'owner', 'path', 'pending', 'percent', 'producer', 'projected', 'removed', 'report', 'requested', 'result', 'right', 'schema',
  'scoped', 'shown', 'size', 'source', 'status', 'system', 'table', 'tag', 'timeout', 'total', 'trigger', 'unknown', 'v',
  'version', 'work', 'works', 'yes']);
const backendPattern = (() => {
  const escape = s => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const patterns = new Map();
  return template => {
    let found = patterns.get(template);
    if (!found) {
      const parts = template.split(/\{(\w+)\}/), literal = parts.filter((_, i) => i % 2 === 0);
      const names = parts.filter((_, i) => i % 2);
      // {n} is a count (plural forms pick by it); any other placeholder stands for any text. For tb(), data never
      // holds a ；, which joins the backend's messages: "已将 1 张移出 雪；其他" is two messages, not a name 雪；其他.
      const pattern = data => new RegExp('^' + literal.map((text, i) => (i ? names[i - 1] === 'n' ? '(\\d[\\d,.]*)'
        : data && !BACKEND_NESTED.has(names[i - 1]) ? '([^；]*?)' : '([\\s\\S]*?)' : '') + escape(text)).join('') + '$');
      found = {template, names, literal, weight: literal.join('').length, re: pattern(false), shown: pattern(true)};
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
// The translation of a backend text, or undefined when no entry or template fits it.
function backendEntry(text, depth, split = true) {
  if (Object.hasOwn(I18N.table, text) && I18N.table[text]) return t(text);
  if (depth >= 3) return undefined;
  // The most specific template wins: the one with the most wording of its own.
  for (const p of backendTemplates()) {
    if (!text.startsWith(p.literal[0]) || !text.endsWith(p.literal.at(-1))) continue;
    const m = p.shown.exec(text);
    if (m) return t(p.template, Object.fromEntries(p.names.map((name, i) => [name, BACKEND_NESTED.has(name) ? tb(m[i + 1], depth + 1) : m[i + 1]])));
  }
  // Several messages the backend joined with ；, which may also be inside one: the longest runs that fit.
  const parts = split ? text.split('；') : [];
  if (parts.length < 2) return undefined;
  const out = [];
  for (let i = 0; i < parts.length;) {
    let j = parts.length - (i ? 0 : 1), found;
    for (; j > i; j--) if ((found = backendEntry(parts.slice(i, j).join('；'), depth + 1, false)) !== undefined) break;
    if (j === i) { out.push(parts[i]); i++; } else { out.push(found); i = j; }
  }
  return out.join(t('；', {context: 'join'}));
}
function tb(text, depth = 0) {
  if (I18N.language === 'zh-CN' || typeof text !== 'string' || !/\p{Script=Han}/u.test(text)) return text;
  if (!depth && backendShown.has(text)) return backendShown.get(text);
  const out = backendEntry(text, depth) ?? text;
  if (!depth) {
    if (backendShown.size > 5000) backendShown.clear();
    backendShown.set(text, out);
  }
  return out;
}
// Whether the backend sent the message of this template, whatever the page's language: the code to test. The
// message may come with the backend's prefixes ("不合法：…", "同步失败：…"), so the template may also match what
// follows a ：, as long as everything before it is such prefixes: words of Chinese alone. A ： inside data, as in
// "已给 1 张加上 甲：会话已过期，请刷新页面", follows a space, a digit or a placeholder's text, and never counts (#198).
function said(text, template) {
  if (typeof text !== 'string') return false;
  const re = backendPattern(template).re;
  for (let at = 0; at >= 0; at = text.indexOf('：', at) + 1 || -1) {
    if (at && !/^(?:\p{Script=Han}+：)+$/u.test(text.slice(0, at))) break;
    if (text.slice(at) === template || re.test(text.slice(at))) return true;
  }
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
