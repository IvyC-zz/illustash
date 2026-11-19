// 供模块使用的页面语言（#173、#199）：以导入的形式提供 t()、tb()、said() 和 I18N。它们唯一的实现是 web/static/i18n.js，
// 那是一个经典脚本，因为桌面外壳的启动页和 splash.js 也加载它；它在所有模块之前运行（index.html），在 Node 里由
// tests/web.cjs 先运行它。它的 I18N 是顶层 const，是全局绑定但不是 globalThis 的属性，所以按名字读；本地名字让导出
// 可以用上它们的名字。
const translate = t, backend = tb, sent = said, language = I18N;
export {translate as t, backend as tb, sent as said, language as I18N};

// 纯文本提示中的多段消息，用界面语言的句号和留白连接，已有句末标点则保留。
export function sentences(...parts) {
  const text = parts.filter(Boolean);
  if (text.length < 2) return text[0] || '';
  const chinese = language.language === 'zh-CN';
  const end = chinese ? '。' : '.';
  return text.map(line => /[.!?。！？…][”’"'）)]*$/u.test(line) ? line : line + end).join(chinese ? '' : ' ');
}
