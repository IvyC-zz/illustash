'use strict';
// 在样式表绘制之前运行，深色页面就不会闪一下白。'pi.theme' 为 ''（跟随系统）、'light' 或 'dark'。
(() => {
  const media = matchMedia('(prefers-color-scheme: dark)');
  const pref = () => { try { return JSON.parse(localStorage.getItem('pi.theme') || '""'); } catch { return ''; } };
  const apply = () => { const t = pref(); document.documentElement.dataset.theme = t === 'dark' || (t !== 'light' && media.matches) ? 'dark' : 'light'; };
  apply(); media.addEventListener('change', apply); window.applyTheme = apply;
})();
