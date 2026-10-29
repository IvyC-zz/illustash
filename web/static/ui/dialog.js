// 页面自己的确认对话框（桌面布局，#199）。
import {t} from '../core/i18n.js';
import {esc} from '../core/format.js';
import {session} from '../core/session.js';
import {state} from '../core/state.js';
// 图库页面没有 Tauri 对话框权限。确认框留在页面内；window.confirm 可能被桌面外壳换成一个异步的插件函数。
let confirmationOpen = false;
// `ok` 是确认按钮的名字；没有 `cancel` 时对话框只作告知。
export function confirmAction(message, title = t('确认操作'), ok = t('确认'), cancel = true) {
  if (confirmationOpen) return Promise.resolve(false);
  confirmationOpen = true;
  const token = session.token, route = state.route;
  return new Promise(resolve => {
    const d = document.createElement('dialog'); d.className = 'palette profile-dialog';
    d.setAttribute('aria-label', title);
    d.innerHTML = `<form method="dialog"><div class="dialog-top"><h2></h2><button type="button" class="icon" data-close aria-label="${t('关闭')}">×</button></div>
      <p class="muted-note" style="white-space:pre-line"></p>
      <div class="dialog-actions">${cancel ? `<button value="cancel" class="quiet" autofocus>${t('取消')}</button>` : ''}<button value="ok" class="primary"></button></div></form>`;
    d.querySelector('h2').textContent = title;
    d.querySelector('[value="ok"]').textContent = ok;
    d.querySelector('p').textContent = message;
    d.querySelector('[data-close]').onclick = () => d.close();
    d.onclick = e => {
      if (e.target !== d) return;
      const r = d.getBoundingClientRect();
      if (e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom) d.close();
    };
    d.onclose = () => {
      const ok = d.returnValue === 'ok' && token === session.token && route === state.route;
      d.remove(); confirmationOpen = false; resolve(ok);
    };
    document.body.append(d); d.showModal();
  });
}
// 挑一个本地作品（或空选项）；取消时解析为 null。
export function chooseWork(title, works, selected = '', emptyLabel = t('未归类 / 无上级')) {
  return new Promise(resolve => {
    const d = document.createElement('dialog'); d.className = 'palette profile-dialog work-dialog';
    d.innerHTML = `<form method="dialog"><div class="dialog-top"><h2></h2><button type="button" class="icon" data-close aria-label="${t('关闭')}">×</button></div>
      <select name="work" class="work-choice"><option value="">${esc(emptyLabel)}</option>${works.map(w => `<option value="${esc(w.id)}" ${w.id === selected ? 'selected' : ''}>${esc(w.name)}${w.qualifier ? ' · ' + esc(w.qualifier) : ''}</option>`).join('')}</select>
      <div class="dialog-actions"><button value="cancel" class="quiet">${t('取消')}</button><button value="ok" class="primary">${t('确认')}</button></div></form>`;
    d.querySelector('h2').textContent = title;
    d.querySelector('[data-close]').onclick = () => d.close();
    document.body.append(d); d.onclose = () => { const value = d.returnValue === 'ok' ? d.querySelector('select').value : null; d.remove(); resolve(value); }; d.showModal();
  });
}
