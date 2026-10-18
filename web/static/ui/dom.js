// The desktop layout's page-wide pieces (#199): element lookup, the toast with its undo button, the error banner and
// the undo button in the toolbar. Their state lives in core/session.js and core/edits.js.
import {t, tb} from '../core/i18n.js';
import {esc, isMac, shortcut} from '../core/format.js';
import {prefs} from '../core/prefs.js';
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
// Collapsible blocks whose open/closed state survives re-renders and reloads.
export const fold = (key, summary, body) => `<details class="more" data-fold-key="${key}"${prefs.get('settingsOpen', {})[key] ? ' open' : ''}><summary>${summary}</summary><div class="more-body">${body}</div></details>`;
export function wireFolds(root) {
  for (const d of root.querySelectorAll('details[data-fold-key]')) {
    if (d.dataset.foldWired) continue;
    d.dataset.foldWired = '1';
    d.ontoggle = () => { const o = prefs.get('settingsOpen', {}); if (d.open) o[d.dataset.foldKey] = true; else delete o[d.dataset.foldKey]; prefs.set('settingsOpen', o); };
  }
}
// Coalesce identical in-flight reads; a slow card never delays the local settings shell.
// Replace only the sections whose HTML changed, so covers that did not change are not reloaded.
export function reconcileSections(container, sections, empty = t('没有匹配的角色')) {
  if (!sections.length) { container.innerHTML = `<p class="empty">${empty}</p>`; return; }
  const old = new Map([...container.children].map(el => [el._sectionKey, el]));
  let cursor = container.firstElementChild;
  const keep = new Set();
  for (const [key, html] of sections) {
    let el = old.get(key);
    if (!el || el._sectionHTML !== html) {
      const previous = el;
      const template = document.createElement('template'); template.innerHTML = html;
      el = template.content.firstElementChild; el._sectionKey = key; el._sectionHTML = html;
      if (previous) { previous.replaceWith(el); if (cursor === previous) cursor = el; }
    }
    if (el !== cursor) container.insertBefore(el, cursor);
    else cursor = cursor.nextElementSibling;
    keep.add(el);
  }
  for (const el of [...container.children]) if (!keep.has(el)) el.remove();
}
// The magnifier in search boxes.
export const searchIcon = () => '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7"><circle cx="10" cy="10" r="6.5"/><path d="m15 15 5 5"/></svg>';
// The banner when the page stops, and the toolbar's undo button. Called once, when the page starts.
export function wireDom() {
  // A picture the app cannot decode (a damaged file): a plain placeholder instead of the browser's broken-image glyph.
  // The viewer has its own message (#viewerMissing).
  document.addEventListener('error', e => {
    const img = e.target;
    if (!(img instanceof HTMLImageElement) || img.id || !img.getAttribute('src')?.startsWith('/image?')) return;
    const ph = document.createElement('span'); ph.className = 'img-broken'; ph.textContent = t('无法打开'); img.replaceWith(ph);
  }, true);
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
}
