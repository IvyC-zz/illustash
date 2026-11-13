// 浮层管理器（#343）：页面上所有"盖在别的东西上面"的层，开、关的先后顺序在这里记成一个栈。
// 各浮层只声明自己是什么类型、怎么关；Esc、点外关闭、焦点归还由这里统一做，别处不再监听 document 的 keydown / click。
// 规则写在 docs/WEB_UI_MAP.md 第 3 节。这个模块不碰具体页面，不 import 别的 ui/ 模块，可以单独在 Node 里测试。
//
// 三种类型（kind）：
//   modal    模态对话框（<dialog>.showModal）。点背景是否关闭由 backdropClose 声明；它之下的层不响应点外关闭。
//   popover  非模态浮层：视图选项、任务面板、标签菜单、更多菜单、"全部作品"、搜索候选菜单。点它和它的触发按钮之外
//            的任何地方关闭。
//   overlay  盖满页面的层：导览、大图。没有点外关闭。
// 栈按打开的先后排；Esc 只关栈顶一个，栈是空的就放行（页面自己的 Esc 兜底链接着走：输入框失焦 → 详情栏 → 取消选择）。
// 层级（z-index）仍由 app.css 的 --z-* 令牌决定，这里不写：原生 modal 在浏览器顶层，高于所有 z-index。

import {composing} from '../core/keys.js';

const stack = [];
let wired = null;
const get = x => (typeof x === 'function' ? x() : x);
const TYPING = 'input,textarea,select,[contenteditable]';

// 点在对话框自己的背景上（<dialog> 元素本身、在框外）。
export function isBackdropClick(d, e) {
  if (e.target !== d) return false;
  const r = d.getBoundingClientRect();
  return e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom;
}

// 点的位置是否算在这个层里：它自己的元素，或者打开它的按钮（点按钮由按钮自己的处理函数开关）。
const inside = (layer, target) => !!(get(layer.el)?.contains(target) || get(layer.anchor)?.contains(target));
// 元素被换掉、被藏起来（自己或祖先 hidden）、对话框已经关了：这一层其实已经不在了，悄悄清掉，免得白占一次 Esc。
const alive = layer => {
  const el = get(layer.el);
  return !!el && el.isConnected !== false && !el.hidden && !el.closest?.('[hidden]') && !(layer.kind === 'modal' && el.open === false);
};
const prune = () => { for (let i = stack.length - 1; i >= 0; i--) if (!alive(stack[i])) stack.splice(i, 1); };

function dismiss(layer, reason) {
  try {
    if (layer.onDismiss) layer.onDismiss(reason);
    else if (layer.kind === 'modal') get(layer.el).close();
    else if (get(layer.el)) get(layer.el).hidden = true;
  } finally { layer.close(reason); }
}

function onKeydown(e) {
  if (e.key !== 'Escape' || composing(e)) return;  // 组字时的 Esc 是取消组字，不关浮层
  prune();
  const top = stack[stack.length - 1];
  if (!top) return;
  // 大图这类层让位给输入框：焦点在层外的输入框里时，Esc 只让输入框失焦（页面的兜底链做）。
  if (top.yieldToInput && e.target?.closest?.(TYPING) && !inside(top, e.target)) return;
  e.preventDefault(); e.stopImmediatePropagation();
  dismiss(top, 'escape');
}

function onClick(e) {
  prune();
  for (let i = stack.length - 1; i >= 0; i--) {
    const layer = stack[i];
    if (layer.kind === 'modal') { if (layer.backdropClose && isBackdropClick(get(layer.el), e)) dismiss(layer, 'backdrop'); return; }
    if (layer.outside && !inside(layer, e.target)) dismiss(layer, 'outside');
  }
}

// 挂上 document 的 keydown（捕获阶段，先于页面其余处理）和 click（捕获阶段）。页面启动时调用一次，要排在其他 wire 之前。
export function wireOverlays() {
  if (wired === document) return;
  wired = document;
  document.addEventListener('keydown', onKeydown, true);
  document.addEventListener('click', onClick, true);
}

// 声明一个层。spec：
//   kind         'modal' | 'popover' | 'overlay'
//   id           名字，给文档和测试看
//   el           层的元素，或返回元素的函数（元素会被重画换掉的用函数）
//   anchor       popover 的触发按钮（元素或函数）：点它不算"点外"；没有打开者记录时 Esc 把焦点还给它
//   outside      点外关闭，popover 默认开，其余默认关
//   backdropClose  modal 点背景是否关闭。默认不关；点背景关的是只读或确认类，带输入的表单要保持不关
//   yieldToInput 焦点在层外的输入框时，Esc 不关这一层（只有大图用）
//   onDismiss(reason)  Esc / 点外 / 点背景时怎样关掉它（reason: 'escape' | 'outside' | 'backdrop'）；
//                modal 默认 close()，其余默认 hidden = true
// close(reason) 的 reason 决定焦点怎么办：'escape'、'api'（默认，选了一项之类）把焦点还给打开它的地方；
// 'outside'、'silent'（页面重画导致）不动焦点。
// 返回的层有 open()、close(reason)、isOpen。open() / close() 都可以重复调用；其他途径关掉它时也要调 close()，
// 栈才不会留着已经关了的层。
export function createLayer(spec) {
  const layer = {
    outside: spec.kind === 'popover', backdropClose: false, yieldToInput: false, ...spec, opener: null,
    get isOpen() { return stack.includes(layer); },
    open() {
      if (layer.isOpen) return layer;
      layer.opener = document.activeElement;
      stack.push(layer);
      return layer;
    },
    close(reason = 'api') {
      const i = stack.indexOf(layer);
      if (i < 0) return;
      stack.splice(i, 1);
      // 对话框关闭时浏览器自己归还焦点；点外关闭时用户已经点向别处；页面重画时（'silent'）不该抢焦点。
      // 其余情况（Esc、选了一项）把焦点还给打开它的地方。
      if (layer.kind === 'modal' || reason === 'outside' || reason === 'silent') return;
      const el = get(layer.el), now = document.activeElement;
      if (now && now !== document.body && !el?.contains(now)) return;
      const back = [layer.opener, get(layer.anchor)].find(x => x && x !== document.body && x.isConnected !== false && typeof x.focus === 'function' && !el?.contains(x));
      back?.focus({preventScroll: true});
    },
  };
  return layer;
}

// 打开一个 <dialog> 作为模态对话框，并登记进栈；对话框关闭时（任何途径）自动出栈。
export function openModal(dialog, {backdropClose = false, id = ''} = {}) {
  const known = stack.find(layer => layer.el === dialog);
  if (known) return known;
  const layer = createLayer({kind: 'modal', id, el: dialog, backdropClose});
  dialog.showModal();
  layer.open();
  dialog.addEventListener('close', () => layer.close('api'), {once: true});
  return layer;
}

// 清空栈、忘掉已挂的监听。只给测试用：每个测试换一个新的 document。
export function resetLayers() { stack.length = 0; wired = null; }
// 栈里现在有哪些层（由下到上的 id），给测试和调试看。
export const layerIds = () => { prune(); return stack.map(layer => layer.id); };
// 关栈顶一层，与按 Esc 相同；栈空返回 false。
export function dismissTop(reason = 'escape') {
  prune();
  const top = stack[stack.length - 1];
  if (!top) return false;
  dismiss(top, reason);
  return true;
}
