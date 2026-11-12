// 页面自己的模态对话框（桌面布局，#199、#319）。openDialog 是唯一的生成处：标题行带 ×、正文、操作区，
// 点 ×、[data-close]、Esc（原生）或背景都能关，关闭后从页面移除。confirmAction 和 chooseWork 是建在它上面的两个现成对话框；
// ui/catalog.js 和 ui/palette.js 自己的对话框也走这里。
import {t} from '../core/i18n.js';
import {esc} from '../core/format.js';
import {session} from '../core/session.js';
import {state} from '../core/state.js';
// 点在对话框自己的背景上（<dialog> 元素本身，盒子之外）。
export function isBackdropClick(d, e) {
  if (e.target !== d) return false;
  const r = d.getBoundingClientRect();
  return e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom;
}
// 生成并显示一个模态对话框，返回它。`title` 是纯文本；`body` 和 `actions` 是 HTML（调用方已转义、已翻译），
// 分别是中间和底部一行（为空就不画）。`cls` 接在 `modal` 后面。点背景关闭的规则（#343）：只读和确认类对话框关，
// 带输入的表单类（文字、选择、勾选）用 `backdropClose: false`，免得误点丢掉输入。
// `focus` 是要获得焦点的控件的选择器（默认是浏览器的第一个可聚焦控件，或带 [autofocus] 的）。
// `onClose(d)` 在关闭时运行（在里面读 d.returnValue 和表单）；运行完立刻把对话框从页面移除。
export function openDialog({title, body = '', actions = '', cls = '', backdropClose = true, focus = '', onClose} = {}) {
  const d = document.createElement('dialog'); d.className = ('modal ' + cls).trim();
  d.setAttribute('aria-label', title);
  d.innerHTML = `<form method="dialog">
    <div class="dialog-top"><h2></h2><button type="button" class="icon" data-close aria-label="${t('关闭')}">×</button></div>
    ${body}${actions ? `<div class="dialog-actions">${actions}</div>` : ''}</form>`;
  d.querySelector('h2').textContent = title;
  d.onclick = e => {
    if (e.target === d) { if (backdropClose && isBackdropClick(d, e)) d.close(); }
    else if (e.target.closest('[data-close]')) d.close();
  };
  d.onclose = () => { try { return onClose?.(d); } finally { d.remove(); } };
  document.body.append(d); d.showModal();
  if (focus) d.querySelector(focus)?.focus?.();
  return d;
}
// 图库页面没有 Tauri 对话框权限。确认框留在页面内；window.confirm 可能被桌面外壳换成一个异步的插件函数。
let confirmationOpen = false;
// `ok` 是确认按钮的名字；没有 `cancel` 时对话框只作告知。
export function confirmAction(message, title = t('确认操作'), ok = t('确认'), cancel = true) {
  if (confirmationOpen) return Promise.resolve(false);
  confirmationOpen = true;
  const token = session.token, route = state.route;
  return new Promise(resolve => {
    const d = openDialog({title, body: '<p class="muted-note dialog-message"></p>',
      actions: `${cancel ? `<button value="cancel" class="quiet" autofocus>${t('取消')}</button>` : ''}<button value="ok" class="primary"></button>`,
      onClose: d => { confirmationOpen = false; resolve(d.returnValue === 'ok' && token === session.token && route === state.route); }});
    d.querySelector('[value="ok"]').textContent = ok;
    d.querySelector('p').textContent = message;
  });
}
const workOption = (w, selected) => `<option value="${esc(w.id)}" ${w.id === selected ? 'selected' : ''}>${esc(w.name)}${w.qualifier ? ' · ' + esc(w.qualifier) : ''}</option>`;
// 挑一个本地作品（或空选项）；取消时解析为 null。选择类表单，点背景不关。
export function chooseWork(title, works, selected = '', emptyLabel = t('未归类 / 无上级')) {
  return new Promise(resolve => {
    openDialog({title, cls: 'work-dialog', backdropClose: false,
      body: `<select name="work" class="work-choice"><option value="">${esc(emptyLabel)}</option>${works.map(w => workOption(w, selected)).join('')}</select>`,
      actions: `<button value="cancel" class="quiet">${t('取消')}</button><button value="ok" class="primary">${t('确认')}</button>`,
      onClose: d => resolve(d.returnValue === 'ok' ? d.querySelector('select').value : null)});
  });
}
