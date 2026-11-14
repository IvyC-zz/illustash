// 键盘事件的公共判断（#388）。不碰 DOM，Node 里能直接导入。

// 输入法正在组字（拼音、假名等）：此时的回车是选词，方向键是挑候选，数字键是选第几个候选，Esc 是取消组字，都不该当成页面的操作。
// 只看 `isComposing` 不够：WebKit（macOS 桌面 App 的 WKWebView）里，结束组字的那一次按键先触发 compositionend，
// 随后的 keydown 的 isComposing 已经是 false，只有 keyCode 仍是 229（WebKit bug 165004）；Chromium（WebView2）两个都有。
// 所有输入框、所有 keydown 里处理 Enter、方向键、数字键、Esc 的地方，第一句都先问它；tools/check_frontend.py 的规则 9 检查这一点。
export const composing = e => !!e && (e.isComposing === true || e.keyCode === 229);
