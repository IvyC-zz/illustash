// The desktop layout's page-wide pieces (#199): element lookup, the toast with its undo button, the error banner and
// the undo button in the toolbar. Their state lives in core/session.js and core/edits.js.
import {esc, isMac, shortcut} from '../core/format.js';
import {session, STALE, onStale, readError} from '../core/session.js';
import {onLatest} from '../core/edits.js';
export const $ = id => document.getElementById(id);
// Help text and titles are written with ⌘; other systems show Ctrl.
export function platformHints(root, mac = isMac) {
  if (mac) return;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  while (walker.nextNode()) walker.currentNode.nodeValue = walker.currentNode.nodeValue.replace(/⌘(?=\s|$)/g, 'Ctrl').replaceAll('⌘', 'Ctrl+');
  for (const el of root.querySelectorAll('[title]')) el.title = el.title.replace(/⌘(?=\s|$)/g, 'Ctrl').replaceAll('⌘', 'Ctrl+');
}
let toastTimer;
export function toast(message, withUndo = false) {
  $('toastText').textContent = message; $('toastUndo').hidden = !withUndo; $('toast').hidden = false;
  clearTimeout(toastTimer); toastTimer = setTimeout(() => { $('toast').hidden = true; }, withUndo ? 6000 : 3500);
}
// The toast stays while the pointer is on it; its undo button runs undo().
export function wireToast(undo) {
  $('toast').addEventListener('mouseenter', () => clearTimeout(toastTimer));
  $('toast').addEventListener('mouseleave', () => { toastTimer = setTimeout(() => { $('toast').hidden = true; }, 2500); });
  $('toastUndo').onclick = () => undo();
}
export function error(message) { if (session.stale) return; $('errorBanner').textContent = message; $('errorBanner').hidden = !message; }
export function readFailure(node, e) { if (node) { node.hidden = false; node.innerHTML = `<p class="empty" role="alert">${esc(readError(e))}</p>`; } }
// A stopped page says so in the error banner, with a button to reload.
onStale(() => {
  const banner = $('errorBanner'), reload = document.createElement('button');
  banner.textContent = t('{reason}。这个页面已停止读写，未保存的输入不会提交。', {reason: STALE});
  reload.type = 'button'; reload.textContent = t('重新载入'); reload.onclick = () => location.reload();
  banner.append(reload); banner.hidden = false;
});
onLatest(({latest, busy}) => {
  $('undoButton').disabled = !latest || busy;
  $('undoButton').title = latest ? t('撤销：{label}（{key}）', {label: tb(latest.label), key: shortcut('Z')}) : t('暂无可撤销操作');
});
