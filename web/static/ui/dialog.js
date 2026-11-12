// The page's own modal dialogs (desktop layout, #199, #319). openDialog is the one place that builds one: title row with
// a × button, body, action row, close on ×/[data-close]/Esc (native), a click on the backdrop and a removal when it closes.
// confirmAction and chooseWork are two ready-made dialogs on top of it; ui/catalog.js and ui/palette.js build their own.
import {t} from '../core/i18n.js';
import {esc} from '../core/format.js';
import {session} from '../core/session.js';
import {state} from '../core/state.js';
// A click that landed on the dialog's own backdrop (the <dialog> element itself, outside its box).
export function isBackdropClick(d, e) {
  if (e.target !== d) return false;
  const r = d.getBoundingClientRect();
  return e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom;
}
// Builds and shows a modal dialog and returns it. `title` is plain text; `body` and `actions` are HTML (already
// escaped/translated by the caller) for the middle and the bottom row (omitted when empty). `cls` adds classes after
// `modal`. `backdropClose: false` keeps a form that holds typed input from being dismissed by a stray click.
// `focus` is a selector for the control that gets the focus (default: the browser's first focusable one, or [autofocus]).
// `onClose(d)` runs when it closes (read d.returnValue and the form there); the dialog is removed right after.
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
// Gallery pages have no Tauri dialog permission. Keep confirmation inside the page;
// window.confirm may be replaced with an async plugin function by the desktop shell.
let confirmationOpen = false;
// `ok` names the confirming button; without `cancel` the dialog only informs.
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
// Pick a local work (or the empty choice); resolves null when cancelled.
export function chooseWork(title, works, selected = '', emptyLabel = t('未归类 / 无上级')) {
  return new Promise(resolve => {
    openDialog({title, cls: 'work-dialog',
      body: `<select name="work" class="work-choice"><option value="">${esc(emptyLabel)}</option>${works.map(w => workOption(w, selected)).join('')}</select>`,
      actions: `<button value="cancel" class="quiet">${t('取消')}</button><button value="ok" class="primary">${t('确认')}</button>`,
      onClose: d => resolve(d.returnValue === 'ok' ? d.querySelector('select').value : null)});
  });
}
