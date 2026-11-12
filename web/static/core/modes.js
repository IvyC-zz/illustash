// 图片交互的模式（#344）：浏览、选择、详情、大图由 core/state.js 里的几个字段组成。这里把它们读成明确的模式，
// 并把「当前图取哪个来源」「一个快捷键在这一页、这个模式下做什么」各写成一处，键盘处理和单图动作都从这里取。
// 只读 state、不写，不碰 DOM，可以在 Node 里单独测试（tests/modes.test.cjs）。
//
// 四个模式和叠加规则（docs/WEB_UI_MAP.md 第 6 节）：
//   浏览  默认：没有选中、不在选择模式、没有详情栏、没有大图。
//   选择  选中了至少一张（sel 非空），或浏览页进入了选择模式（selecting）还没选。选择栏只在选中数 > 0 时出现。
//   详情  详情栏打开（focused 有值）。可以和「选择」同时在：都在时，单图快捷键先作用于选中的图（选择栏）。
//   大图  大图打开（viewer >= 0）。盖住网格、选择栏和详情栏：快捷键只作用于大图里那张，被盖住的选择栏不响应
//        （选中保留，关掉大图后照旧）。
// 当前图的来源优先级：大图 ＞ 详情栏 ＞ 键盘光标。
import {state} from './state.js';

// 选中了几张：全选用「过滤条件 + 排除集」表示，张数由总数减去排除的。
export const selectionSize = (s = state) => s.sel.all ? Math.max(0, s.total - s.sel.exclude.size) : s.sel.ids.size;

// 现在叠着哪几个模式。浏览是其余三个都不在。
export function modes(s = state) {
  const viewer = s.viewer >= 0, detail = !!s.focused, select = s.selecting || selectionSize(s) > 0;
  return {viewer, detail, select, browse: !viewer && !detail && !select};
}
// 最上面的一个模式：键盘先交给它。
export function topMode(s = state) {
  const m = modes(s);
  return m.viewer ? 'viewer' : m.detail ? 'detail' : m.select ? 'select' : 'browse';
}

// 快捷键作用于几张选中的图：大图打开时选择栏被盖住，算 0，快捷键改为作用于大图里那张。
export const keySelection = (s = state) => s.viewer >= 0 ? 0 : selectionSize(s);

// 大图或详情栏里那张图的 uuid（大图优先）；两者都没有时为空。
export const shownUuid = (s = state) => s.viewer >= 0 ? s.viewerItem?.uuid : s.focused;

// 当前图从哪来：大图或详情栏里那张（{from, uuid}），否则键盘光标（{from: 'cursor', index}），都没有为 null。
// 大图刚打开、图还没读到时（viewerItem 为空）退到光标。
export function currentSource(s = state) {
  const uuid = shownUuid(s);
  if (uuid) return {from: s.viewer >= 0 ? 'viewer' : 'focused', uuid};
  return s.cursor >= 0 ? {from: 'cursor', index: s.cursor} : null;
}

// Space 打开大图时打开哪一张（序号）：光标所在的，否则详情栏里那张，否则第一张。和「当前图」的优先级不同：
// 这时还没有大图，光标总是跟着最近一次点的或移到的那张。
export function viewerTarget(s = state) {
  if (s.cursor >= 0) return s.cursor;
  return s.focused ? s.items.findIndex(x => x?.uuid === s.focused) : 0;
}

// 单图／选择快捷键在这一页做什么。key：'remove'（⌫、Delete）、'c'、'a'、't'、'g'、'x'、's'。
// ctx：view 页面注册表里本页的条目（core/pages.js 的 viewOf），char 是否在角色页，n 作用于几张选中的图
// （keySelection），item 当前图（没有选中时用，决定 ⌫ 有没有可移出的角色）。
// 返回 {bar: 动作}（交给选择栏的同名按钮）、{single: 动作}（作用于当前图），或 null：这一页没有对应的动作。
export function keyAction(key, {view, char = false, n = 0, item = null}) {
  const browse = view.click === 'browse';
  switch (key) {
    case 'remove':
      if (n) return char ? {bar: 'remove'} : browse ? {bar: 'notlib'} : null;
      return char || item?.memberships?.length ? {single: 'remove'} : null;
    case 'c': return n ? {bar: char ? 'swap' : 'assign'} : {single: char ? 'swap' : 'also'};
    case 'a': return n ? {bar: char ? 'also' : 'assign'} : {single: 'also'};
    case 't': return char && n ? {bar: 'variant'} : null;
    case 'g': return n ? {bar: 'tag'} : {single: 'tag'};
    case 'x': { const act = view.bar === 'excluded' ? 'include' : 'exclude'; return n ? {bar: act} : {single: act}; }
    case 's':
      if (view.bar !== 'unmatched') return null;
      return n ? {bar: 'skip'} : view.pool === 'unmatched' ? {single: 'skip'} : null;
    default: return null;
  }
}
