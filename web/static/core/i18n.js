// The page's language for modules (#173, #199): t(), tb(), said() and I18N as imports. Their one implementation is
// web/static/i18n.js, a classic script because the desktop shell's start page and splash.js load it too; it runs
// before any module (index.html), and in Node tests/web.cjs runs it first. Its I18N is a top-level const, a global
// binding but no property of globalThis, so it is read by name; the local names let the exports take theirs.
const translate = t, backend = tb, sent = said, language = I18N;
export {translate as t, backend as tb, sent as said, language as I18N};
