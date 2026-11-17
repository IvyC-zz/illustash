// 输入框加结果菜单（#432 ③-4）：命令面板、工具栏的标签菜单、角色资料和作品对话框里的 Danbooru 搜索、关注作品，都用这一套。
// 键盘一致：焦点一直留在输入框里，↑ ↓ 在结果之间循环移动高亮，Enter 选高亮的那项，Esc 关菜单并把焦点还回去（菜单是
// 浮层管理器 ui/overlay.js 里的 popover，由它处理 Esc；命令面板是对话框，Esc 关对话框）。结果项是菜单里带 data-i 的元素，
// 高亮用 .on。
import {esc} from '../core/format.js';
import {composing} from '../core/keys.js';
import {namesAllowButton} from './dom.js';
import {createLayer} from './overlay.js';

// 结果的键盘和鼠标。opts：
//   count()      现在有几项
//   pick(i, e)   选了第 i 项：Enter（e.type 'keydown'）或点击（'click'）
//   shown()      菜单开着、按键归它；默认看 list.hidden
//   moved(i, how)  改了高亮之后：how 是 'key'（↑ ↓）或 'hover'
//   more(e)      别的键（命令面板的数字键）
//   hover        鼠标移过时高亮跟着走（命令面板）
// 返回 {index, mark(i)}：结果重画后调 mark(i) 把高亮放到第 i 项。
export function listNav(input, list, opts) {
  const {count, pick, shown = () => !list.hidden, moved = () => {}, more = () => {}, hover = false} = opts;
  const nav = {
    index: 0,
    mark(i = nav.index) {
      nav.index = i;
      for (const b of list.querySelectorAll('[data-i]')) b.classList.toggle('on', Number(b.dataset.i) === i);
      list.querySelector('[data-i].on')?.scrollIntoView({block: 'nearest'});
    },
  };
  input.addEventListener('keydown', e => {
    if (composing(e) || !shown()) return;
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const n = count(); if (!n) return;
      nav.mark((nav.index + (e.key === 'ArrowDown' ? 1 : n - 1)) % n); moved(nav.index, 'key');
    } else if (e.key === 'Enter') { e.preventDefault(); if (count()) pick(nav.index, e); }
    else more(e);
  });
  // 点选项时焦点留在输入框里（不触发 blur 关菜单），松开后再选。
  list.addEventListener('mousedown', e => { if (e.target.closest('[data-i]')) e.preventDefault(); });
  list.addEventListener('click', e => { const b = e.target.closest('[data-i]'); if (b) pick(Number(b.dataset.i), e); });
  if (hover) {
    list.addEventListener('mousemove', e => {
      const b = e.target.closest('[data-i]');
      if (b && nav.index !== Number(b.dataset.i)) { nav.mark(Number(b.dataset.i)); moved(nav.index, 'hover'); }
    });
  }
  return nav;
}

// 一个结果项的默认样子：名称、副文字、右边的小字。
const optionRow = (r, i, on) => `<button type="button" class="menu-item watch-option${on ? ' on' : ''}" data-i="${i}">`
  + `<span class="watch-name">${esc(r.label)}</span><small>${esc(r.detail || '')}</small><em>${esc(r.extra || '')}</em></button>`;
// 默认的整份菜单：每个部分是 {label, items, note, allow}，note 是这一部分的说明（allow 时后面接「允许查询…」）。
function defaultDraw(sections, index, row) {
  let i = 0;
  return sections.filter(x => x.items.length || x.note).map(x => (x.label ? `<div class="palette-label">${esc(x.label)}</div>` : '')
    + (x.note ? `<p class="palette-note">${esc(x.note)}${x.allow ? ' ' + namesAllowButton : ''}</p>` : '')
    + x.items.map(r => { const at = i++; return row(r, at, at === index); }).join('')).join('');
}
// 带结果菜单的搜索框：`search(q, show)` 把结果按部分交给 show（可以先给本地的、再给联网的，旧的回答自动作废），
// `pick(item)` 取其中一项。菜单是非模态浮层，叠在输入框所在的对话框或页面上面：Esc 只关菜单，再按一次才轮到下面那层。
// opts：
//   id           浮层的名字（给文档和测试看）
//   delay        输入后多久开始搜（毫秒）
//   draw(sections, index)  整份菜单的 HTML，默认是 defaultDraw；结果项要带 data-i
//   row(r, i, on)          只换结果项的样子
//   first(list)  新结果到达时先高亮哪一项，默认第一项
//   allowed(r)   能不能选这一项（已经关注的作品不能再选）
//   typed(value) 每次输入之后（关注作品记下草稿）
export function combo(input, menu, search, pick, opts = {}) {
  const {id = 'combo', delay = 160, row = optionRow, draw = (sections, index) => defaultDraw(sections, index, row), first = () => 0, allowed = () => true, typed = () => {}} = opts;
  let list = [], gen = 0, timer = 0, fresh = true;
  const layer = createLayer({kind: 'popover', id, el: menu, anchor: input, onDismiss: reason => show(false, reason)});
  const show = (on, reason) => {
    menu.hidden = !on; input.setAttribute('aria-expanded', String(on));
    if (on) layer.open(); else layer.close(reason);
  };
  const choose = i => { const r = list[i]; if (!r || !allowed(r)) return; show(false); pick(r); };
  const nav = listNav(input, menu, {count: () => list.length, pick: choose});
  const render = sections => {
    // 一次新的搜索先高亮 first 那项；同一次搜索后到的结果（联网的那部分）不把已经移动过的高亮拉回去。
    list = sections.flatMap(x => x.items);
    nav.index = Math.max(0, Math.min(fresh ? first(list) : nav.index, list.length - 1)); fresh = false;
    menu.innerHTML = draw(sections, nav.index);
    show(!!menu.innerHTML);
  };
  const run = () => { const g = ++gen; fresh = true; clearTimeout(timer); timer = setTimeout(() => search(input.value.trim(), sections => { if (g === gen) render(sections); }), delay); };
  input.addEventListener('input', () => { typed(input.value); run(); });
  input.addEventListener('focus', run);
  input.addEventListener('blur', () => setTimeout(() => show(false, 'outside'), 150));
}
