'use strict';
// 界面语言（#173、#182、docs/WEB_V2.md）。以 /i18n.js 提供，前面有服务端写的三行：
// self.ILLUSTASH_LANGUAGE——'system'、'zh-CN' 或 web/static/locales 里的一个代码，取自全应用共用的 preferences.json；
// self.ILLUSTASH_LANGUAGES——每个 locales/<code>.json，形如 {code: 这种语言用自己写的名字}；以及
// self.ILLUSTASH_TABLES——{code: table}，只有选定的那一个，'system' 时是全部，因为只有浏览器能决定选哪个。
// 表以中文原文本身为键（gettext 风格）。t('中文原文', {n: 3}) 显示页面语言的条目，没有就显示中文，所以还没翻译的
// 文字照常可用。
// 占位符写作 {name}。静态标记用 data-t（它的文字）和 data-t-attr="title aria-label" 表示要翻译。
// 同一个中文表示两种意思时带一个上下文，同 gettext 的 msgctxt：t('移除', {context: 'column'}) 在表里查
// "column\u0004移除"，在中文页面上照样显示「移除」。
// 一个条目可以有复数形式，按 {n} 用 Intl.PluralRules 挑选：{"one": "{n} picture", "other": "{n} pictures"}。
const I18N = (() => {
  const preference = self.ILLUSTASH_LANGUAGE || 'system';
  const languages = [['zh-CN', '简体中文'], ...Object.entries(self.ILLUSTASH_LANGUAGES || {})];
  const codes = languages.map(([code]) => code);
  // 跟随系统：取第一个有表的浏览器语言，先按完整代码，再按主语言（en-GB → en）；其他中文显示简体中文；都不匹配时，有
  // 英文表就显示英文。
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
  // 数字和日期通过 Intl 跟随页面语言（#182）。
  return {preference, languages, language, locale: plurals.resolvedOptions().locale, plurals, table: (self.ILLUSTASH_TABLES || {})[language] || {}};
})();
// {n} 是个数：一个数字，或已经由 number() 格式化过的整数（"1,234"、"1.234"）。
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
// 后端发来的文字（#173 第 3 阶段）在传输和数据库里保持中文：回复和存下的记录（同步记录、撤销标签）字节不变，后端
// 也从不需要知道页面语言。它的中文模板是同一批表的键，写法和 Rust 源码里一字不差（"已将 {n} 张移出 {name}"），所以
// 模板就是消息的代码。tb(text) 用页面语言显示一条后端消息：以整段文字为键的条目，否则用措辞与它匹配的模板，每个
// {name} 代表任意文字。旧记录只要模板还在表里就能翻译；其他的照发来时的样子显示。
// 占位符代表什么（#198）：后端自己的嵌套消息——它写的错误、原因、步骤、时长——按同样的方式翻译；其他都是数据，由用户、
// 图库或外部世界命名（tag、角色、路径、键、数字），照发来时的样子显示，即使读起来像表里的键（一个叫「未识别」的
// tag）。两个名单里都没有的名字算数据。tests/i18n.test.cjs 检查后端模板的每个占位符恰好在两者之一，所以新模板要说明
// 它的占位符属于哪一类。
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
      // {n} 是个数（复数形式按它挑选）；其他占位符代表任意文字。对 tb() 而言，数据里从不含「；」，它用来连接后端的
      // 多条消息："已将 1 张移出 雪；其他" 是两条消息，不是一个叫「雪；其他」的名字。
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
  // 只用自己带中文措辞的模板：单独一个 "{a}：{b}" 会匹配太多。
  return () => list ||= Object.keys(I18N.table).filter(key => !key.includes('\u0004') && /\{\w+\}/.test(key)
    && /\p{Script=Han}/u.test(key.replace(/\{\w+\}/g, ''))).map(backendPattern).sort((a, b) => b.weight - a.weight);
})();
const backendShown = new Map();
// 一段后端文字的翻译；没有条目或模板对得上时为 undefined。
function backendEntry(text, depth, split = true) {
  if (Object.hasOwn(I18N.table, text) && I18N.table[text]) return t(text);
  if (depth >= 3) return undefined;
  // 最具体的模板胜出：自己的措辞最多的那个。
  for (const p of backendTemplates()) {
    if (!text.startsWith(p.literal[0]) || !text.endsWith(p.literal.at(-1))) continue;
    const m = p.shown.exec(text);
    if (m) return t(p.template, Object.fromEntries(p.names.map((name, i) => [name, BACKEND_NESTED.has(name) ? tb(m[i + 1], depth + 1) : m[i + 1]])));
  }
  // 后端用「；」连起来的几条消息，单条消息里也可能有「；」：取能对上的最长片段。
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
// 不管页面是什么语言，后端发来的是否是这个模板的消息：用来判断的代码。消息可能带着后端的前缀（"不合法：…"、
// "同步失败：…"），所以模板也可以匹配某个「：」之后的部分，只要它之前全是这样的前缀：只由中文组成的词。数据里的「：」，
// 如 "已给 1 张加上 甲：会话已过期，请刷新页面"，跟在空格、数字或占位符的文字后面，从不算数（#198）。
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
