// The page's own confirmation dialog (desktop layout, #199).
import {t} from '../core/i18n.js';
import {session} from '../core/session.js';
import {state} from '../core/state.js';
// Gallery pages have no Tauri dialog permission. Keep confirmation inside the page;
// window.confirm may be replaced with an async plugin function by the desktop shell.
let confirmationOpen = false;
// `ok` names the confirming button; without `cancel` the dialog only informs.
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
