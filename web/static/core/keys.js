// 键盘事件和文字输入的公共判断（#388、#645）。不碰 document，只在传进来的元素上挂监听，Node 里能直接导入。

// 输入法正在组字（拼音、假名等）：此时的回车是选词，方向键是挑候选，数字键是选第几个候选，Esc 是取消组字，都不该当成页面的操作。
// 只看 `isComposing` 不够：WebKit（macOS 桌面 App 的 WKWebView）里，结束组字的那一次按键先触发 compositionend，
// 随后的 keydown 的 isComposing 已经是 false，只有 keyCode 仍是 229（WebKit bug 165004）；Chromium（WebView2）两个都有。
// 所有输入框、所有 keydown 里处理 Enter、方向键、数字键、Esc 的地方，第一句都先问它；tools/check_frontend.py 的规则 9 检查这一点。
export const composing = e => !!e && (e.isComposing === true || e.keyCode === 229);

// 搜索框这类「输入就搜」的文字输入框（#645）：组字中的 input 不算数（不然打「张」会按 z、zh、zhang 各搜一次），
// 组完字（compositionend）按上屏的文字补一次。delay 毫秒内连续输入只算最后一次；不给 delay 就每次都立即调用。
// 按 Esc 取消组字时 compositionend 也会到，这时框里还是组字前的文字，fn 照常调用一次。
// el 可以是输入框本身，也可以是委托监听的外层容器（两种事件都冒泡），fn 收到的是触发的那个事件，从 e.target 取框。
// 所有「输入就搜」的 input 监听都经它挂，不在各处各写一份防抖。
export function onTyped(el, fn, delay = 0) {
  let timer = 0;
  const fire = e => {
    if (composing(e)) return;
    clearTimeout(timer);
    if (delay) timer = setTimeout(() => fn(e), delay); else fn(e);
  };
  el.addEventListener('input', fire);
  el.addEventListener('compositionend', fire);
}
