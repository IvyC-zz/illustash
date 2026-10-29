// 桌面布局里覆盖整个页面的部件（#199）：按 id 找元素、带撤销按钮的 toast、错误横幅和工具栏里的撤销按钮。它们的状态在
// core/session.js 和 core/edits.js。
import {t, tb} from '../core/i18n.js';
import {esc, isMac, shortcut} from '../core/format.js';
import {prefs} from '../core/prefs.js';
import {session, STALE, onStale, readError} from '../core/session.js';
import {onLatest} from '../core/edits.js';
import {workLabel} from '../core/labels.js';
export const $ = id => document.getElementById(id);
// 帮助文字和提示用 ⌘ 写；其他系统显示 Ctrl。
export function platformHints(root, mac = isMac) {
  if (mac) return;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  while (walker.nextNode()) walker.currentNode.nodeValue = walker.currentNode.nodeValue.replace(/⌘(?=\s|$)/g, 'Ctrl').replaceAll('⌘', 'Ctrl+');
  for (const el of root.querySelectorAll('[title]')) el.title = el.title.replace(/⌘(?=\s|$)/g, 'Ctrl').replaceAll('⌘', 'Ctrl+');
}
let toastTimer;
export function toast(message, withUndo = false) {
  $('toastText').textContent = message; $('toastUndo').hidden = !withUndo; $('toastAct').hidden = true; $('toast').hidden = false;
  clearTimeout(toastTimer); toastTimer = setTimeout(() => { $('toast').hidden = true; }, withUndo ? 6000 : 3500);
}
// 给正在显示的 toast 追加一点：再补几个字，可选再加一个按钮（如对刚设好的角色所属作品「关注」，#293）。toast 随后会多
// 停留一会儿。
export function toastMore(text, label = '', run = null) {
  if ($('toast').hidden) return;
  $('toastText').textContent += text;
  const b = $('toastAct'); b.hidden = !label; b.textContent = label; b.onclick = () => { $('toast').hidden = true; run(); };
  clearTimeout(toastTimer); toastTimer = setTimeout(() => { $('toast').hidden = true; }, 9000);
}
// 指针停在 toast 上时它不消失；它的撤销按钮运行 undo()。
export function wireToast(undo) {
  $('toast').addEventListener('mouseenter', () => clearTimeout(toastTimer));
  $('toast').addEventListener('mouseleave', () => { toastTimer = setTimeout(() => { $('toast').hidden = true; }, 2500); });
  $('toastUndo').onclick = () => undo();
}
// 角色名字后面用灰色小字写的作品（core/labels.js workLabel）。
export const workSmall = c => { const w = workLabel(c); return w ? `<small class="char-work">${esc(w)}</small>` : ''; };
export function error(message) { if (session.stale) return; $('errorBanner').textContent = message; $('errorBanner').hidden = !message; }
export function readFailure(node, e) { if (node) { node.hidden = false; node.innerHTML = `<p class="empty" role="alert">${esc(readError(e))}</p>`; } }
// 可折叠的块，展开或收起的状态在重新渲染和重新载入后保留。
export const fold = (key, summary, body) => `<details class="more" data-fold-key="${key}"${prefs.get('settingsOpen', {})[key] ? ' open' : ''}><summary>${summary}</summary><div class="more-body">${body}</div></details>`;
export function wireFolds(root) {
  for (const d of root.querySelectorAll('details[data-fold-key]')) {
    if (d.dataset.foldWired) continue;
    d.dataset.foldWired = '1';
    d.ontoggle = () => { const o = prefs.get('settingsOpen', {}); if (d.open) o[d.dataset.foldKey] = true; else delete o[d.dataset.foldKey]; prefs.set('settingsOpen', o); };
  }
}
// 合并相同的进行中读取；慢的卡片绝不拖慢本地的设置外壳。
// 只替换 HTML 变了的部分，没变的封面不会重新加载。
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
// 搜索框里的放大镜。
export const searchIcon = () => '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7"><circle cx="10" cy="10" r="6.5"/><path d="m15 15 5 5"/></svg>';
// 页面停下时的横幅，以及工具栏的撤销按钮。页面启动时调用一次。
export function wireDom() {
  // App 无法解码的图片（损坏的文件）：用一个朴素的占位，而不是浏览器的破图图标。
  // 查看器有它自己的提示（#viewerMissing）。
  document.addEventListener('error', e => {
    const img = e.target;
    if (!(img instanceof HTMLImageElement) || img.id || !img.getAttribute('src')?.startsWith('/image?')) return;
    const ph = document.createElement('span'); ph.className = 'img-broken'; ph.textContent = t('无法打开'); img.replaceWith(ph);
  }, true);
  // 停下的页面在错误横幅里说明，并带一个重新载入的按钮。
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
