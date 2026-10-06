'use strict';
// Runs before the stylesheet paints so a dark page never flashes white. 'pi.theme' is '' (follow system), 'light' or 'dark'.
(() => {
  const media = matchMedia('(prefers-color-scheme: dark)');
  const pref = () => { try { return JSON.parse(localStorage.getItem('pi.theme') || '""'); } catch { return ''; } };
  const apply = () => { const t = pref(); document.documentElement.dataset.theme = t === 'dark' || (t !== 'light' && media.matches) ? 'dark' : 'light'; };
  apply(); media.addEventListener('change', apply); window.applyTheme = apply;
})();
